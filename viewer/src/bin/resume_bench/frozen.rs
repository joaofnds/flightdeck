//! A real recording endpoint held still while separate builds measure the same resume.
use std::fs::DirBuilder;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use collector::recording::{Registry, private, random_identity, read_identity};

#[path = "frozen_files.rs"]
mod files;
#[path = "frozen_mapping.rs"]
mod mapping;

use files::FrozenFiles;
use mapping::FrozenRing;

#[cfg(test)]
#[path = "frozen_tests.rs"]
mod tests;

/// Owns only fixture links and copied rings. Dropping it leaves the disk capture intact.
pub struct FrozenRecording {
    root: PathBuf,
    files: FrozenFiles,
    rings: Vec<FrozenRing>,
}

pub struct Directories<'a> {
    pub source: &'a Path,
    pub target: &'a Path,
}

impl FrozenRecording {
    /// `target` must be absent and on the source filesystem, since sealed files are linked.
    /// A source seal or retention racing capture can fail capture; originals stay untouched.
    pub fn capture(directories: &Directories<'_>) -> io::Result<Self> {
        private::verify_directory(directories.source)?;
        let registry = Registry::read(directories.source)?;
        let active = registry
            .runs
            .last()
            .ok_or_else(|| io::Error::other("recording has no registered ring"))?;
        let recording = read_identity(directories.source)?;

        DirBuilder::new().mode(0o700).create(directories.target)?;
        private::verify_directory(directories.target)?;
        let files = FrozenFiles::capture(directories, recording, &active.identity)?;
        let mut rings = Vec::with_capacity(registry.runs.len());
        let nonce = random_identity()?.0[0];
        assert!(registry.runs.len() <= 2);
        for (index, run) in registry.runs.iter().enumerate() {
            rings.push(FrozenRing::capture(run, nonce, index)?);
        }
        capture_registry_stable(directories.source, &registry)?;
        capture_registry_write(directories.target, registry, &rings)?;
        Ok(Self {
            root: directories.target.to_path_buf(),
            files,
            rings,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Hides whole sealed windows extending past the cutoff. Their immutable data stays linked.
    /// A gap exceeding this generation's run returns an error instead of fabricating history.
    /// Consumers must be stopped or paused while checkpoint changes their visible endpoint.
    pub fn checkpoint(&self, gap_publications: u64) -> io::Result<()> {
        let ring = self.active_ring();
        let newest = ring
            .newest
            .checked_sub(gap_publications)
            .ok_or_else(|| io::Error::other("gap exceeds captured publications"))?;
        if newest < ring.first - 1 {
            return Err(io::Error::other(
                "gap precedes the captured ring generation",
            ));
        }
        self.files.checkpoint(newest)?;
        ring.publish(newest);
        Ok(())
    }

    /// Call while consumers are paused, immediately before timing their live restore.
    pub fn advance(&self) -> io::Result<()> {
        self.files.advance()?;
        let ring = self.active_ring();
        ring.publish(ring.newest);
        Ok(())
    }

    pub fn publication_newest(&self) -> u64 {
        self.active_ring().newest
    }

    /// Physical slot capacity; a recently started generation may hold fewer publications.
    pub fn retained_publications(&self) -> u64 {
        self.active_ring().slots
    }

    pub fn committed_windows(&self) -> u64 {
        self.files.windows
    }

    fn active_ring(&self) -> &FrozenRing {
        assert!(!self.rings.is_empty());
        &self.rings[self.rings.len() - 1]
    }
}

impl Drop for FrozenRecording {
    fn drop(&mut self) {
        if let Err(error) = self.advance() {
            eprintln!("resume fixture: restoring captured endpoint failed: {error}");
        }
    }
}

fn capture_registry_stable(source: &Path, captured: &Registry) -> io::Result<()> {
    let current = Registry::read(source)?;
    if current.evictions != captured.evictions {
        return Err(io::Error::other("registry changed during capture"));
    }
    if current.runs.len() != captured.runs.len() {
        return Err(io::Error::other("registry changed during capture"));
    }
    assert!(captured.runs.len() <= 2);
    for (current, captured) in current.runs.iter().zip(&captured.runs) {
        if current.identity != captured.identity {
            return Err(io::Error::other("registry changed during capture"));
        }
        if current.ring_name != captured.ring_name {
            return Err(io::Error::other("registry changed during capture"));
        }
        if current.collector_pid != captured.collector_pid {
            return Err(io::Error::other("registry changed during capture"));
        }
    }
    Ok(())
}

fn capture_registry_write(
    target: &Path,
    mut registry: Registry,
    rings: &[FrozenRing],
) -> io::Result<()> {
    assert_eq!(registry.runs.len(), rings.len());
    for (run, ring) in registry.runs.iter_mut().zip(rings) {
        assert_eq!(run.identity, ring.identity);
        ring.name()?.clone_into(&mut run.ring_name);
        // The fixture owner keeps this deliberately frozen live source available to readers.
        run.collector_pid = std::process::id();
    }
    registry.write(target)?;
    private::verify_file(&target.join("identity"))?;
    Ok(())
}
