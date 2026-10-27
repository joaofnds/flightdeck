//! A real recording endpoint held still while separate builds measure the same resume.
use std::cell::Cell;
use std::fs::DirBuilder;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};

use collector::recording::{Registry, private, random_identity, read_identity};

#[path = "frozen_files.rs"]
mod files;
#[path = "frozen_input.rs"]
mod input;
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
    replay: Cell<Option<u64>>,
    boundaries: Vec<input::Boundary>,
    observations: Vec<serde_json::Value>,
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
        let boundaries = input::boundaries(&rings[rings.len() - 1])?;
        let observations = input::observations(&rings)?;
        Ok(Self {
            root: directories.target.to_path_buf(),
            files,
            rings,
            replay: Cell::new(None),
            boundaries,
            observations,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Hides whole sealed windows extending past the cutoff. Their immutable data stays linked.
    /// A gap exceeding this generation's run returns an error instead of fabricating history.
    /// Consumers must be stopped or paused while checkpoint changes their visible endpoint.
    pub fn checkpoint(&self, gap_publications: u64) -> io::Result<()> {
        if self.replay.get().is_some() {
            return Err(io::Error::other(
                "reset replay only between stopped consumers",
            ));
        }
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
        self.replay.set(None);
        self.files.advance()?;
        let ring = self.active_ring();
        ring.restore();
        Ok(())
    }

    /// Consumers must be stopped. Subsequent steps only increase visibility.
    pub fn replay_reset(&self, first: u64) -> io::Result<()> {
        let ring = self.active_ring();
        if !(ring.retained_first..=ring.newest).contains(&first) {
            return Err(io::Error::other(
                "replay start is outside validated retained input",
            ));
        }

        let boundary = self
            .boundaries
            .iter()
            .find(|entry| entry.publication == first)
            .ok_or_else(|| io::Error::other("missing replay boundary"))?;
        let retained = self
            .boundaries
            .first()
            .ok_or_else(|| io::Error::other("missing retained replay boundary"))?;
        ring.replay_start(retained.events_first)?;
        ring.complete(false);
        ring.replay_publish(&mapping::ReplayPosition {
            publication: first - 1,
            event: boundary.events_first - 1,
        });
        self.files.checkpoint(first - 1)?;
        self.replay.set(Some(first - 1));
        Ok(())
    }

    pub fn replay_step(&self, publication: u64) -> io::Result<()> {
        let previous = self
            .replay
            .get()
            .ok_or_else(|| io::Error::other("reset replay first"))?;
        if publication <= previous {
            return Err(io::Error::other("replay publication must increase"));
        }

        let boundary = self
            .boundaries
            .iter()
            .find(|entry| entry.publication == publication)
            .ok_or_else(|| io::Error::other("publication outside validated retained input"))?;
        self.active_ring().replay_publish(&mapping::ReplayPosition {
            publication,
            event: boundary.events_end - 1,
        });
        self.replay.set(Some(publication));
        Ok(())
    }

    pub fn replay_seal(&self, window: u64) -> io::Result<()> {
        let newest = self
            .replay
            .get()
            .ok_or_else(|| io::Error::other("reset replay first"))?;
        self.files.replay_seal(&files::Seal {
            window,
            publication: newest,
        })
    }

    pub fn replay_complete(&self) -> io::Result<()> {
        if self.replay.get() != Some(self.active_ring().newest) {
            return Err(io::Error::other(
                "replay has not reached its final publication",
            ));
        }

        self.files.replay_complete(self.active_ring().newest)?;
        self.active_ring().complete(true);
        Ok(())
    }

    /// A finite replay, bounded to five minutes, with seals at their captured end publication.
    pub fn replay_run(&self, interval_ms: u64) -> io::Result<()> {
        let previous = self
            .replay
            .get()
            .ok_or_else(|| io::Error::other("reset replay first"))?;
        if !(1..=10_000).contains(&interval_ms) {
            return Err(io::Error::other(
                "replay interval must be 1..10000 milliseconds",
            ));
        }

        let count = self.active_ring().newest - previous;
        if count * interval_ms > 300_000 {
            return Err(io::Error::other("paced replay exceeds five minutes"));
        }

        self.files.replay_seal_eligible(previous)?;
        let started = std::time::Instant::now();
        for publication in previous + 1..=self.active_ring().newest {
            let elapsed = std::time::Duration::from_millis((publication - previous) * interval_ms);
            std::thread::sleep(
                (started + elapsed).saturating_duration_since(std::time::Instant::now()),
            );
            self.replay_step(publication)?;
            self.files.replay_seal_eligible(publication)?;
            println!(
                "{}",
                serde_json::json!({"event": "replay_publication",
                "publication": publication, "elapsed_ns": started.elapsed().as_nanos()})
            );
        }
        self.replay_complete()
    }

    /// Export before resetting. Backing files are evidence, not an import format.
    pub fn export(&self, directory: &Path) -> io::Result<()> {
        if self.replay.get().is_some() {
            return Err(io::Error::other(
                "export requires the captured full endpoint",
            ));
        }

        input::export(self, directory)
    }

    /// Input identities at the current endpoint; invoke outside measured intervals.
    pub fn expectation(&self, path: &Path) -> io::Result<()> {
        input::expectation(self, path)
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
