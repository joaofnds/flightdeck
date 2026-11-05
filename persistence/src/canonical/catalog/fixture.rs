//! A long recording in the time it takes to link one: windows that exist only in the namespaces, as
//! the writer's grammar lays them out. Their proofs are empty, so a reader that admits one fails,
//! which is how a test shows a read left them alone.

use super::{CanonicalKey, KnownReading, Namespace};
use collector::recording::private;
use std::io;
use std::path::{Path, PathBuf};

/// The links one empty file takes, well under what the file system allows.
const LINKS_PER_TARGET_MAX: usize = 1 << 14;
/// The links a leaf of the writer's grammar holds.
const LEAF_LINKS_MAX: usize = 128;

/// Adds to the recording at `root` a window of one reading for each of `readings`, keeping every
/// window it holds. Nothing may be publishing, since this rebuilds the namespaces in place.
pub fn link_unread_windows(root: &Path, readings: &[KnownReading]) -> io::Result<()> {
    let known = readings
        .iter()
        .map(KnownReading::key)
        .collect::<io::Result<Vec<_>>>()?;
    let ordinary = readings
        .iter()
        .map(|reading| CanonicalKey::source(Namespace::Ordinary, &reading.run, reading.publication))
        .collect::<io::Result<Vec<_>>>()?;
    link_unread_windows_rebuild(root, Namespace::Known, &known)?;
    link_unread_windows_rebuild(root, Namespace::Ordinary, &ordinary)
}

fn link_unread_windows_rebuild(
    root: &Path,
    namespace: Namespace,
    added: &[CanonicalKey],
) -> io::Result<()> {
    let base = root.join(namespace.name());
    let held = root.join("fixture-held");
    private::create_private_dir(&held)?;
    link_unread_windows_set_aside(&base, &held)?;
    let mut sources = Vec::with_capacity(added.len());
    for (index, chunk) in added.chunks(LINKS_PER_TARGET_MAX).enumerate() {
        let target = held.join(format!("target-{index}"));
        let file = std::fs::File::create_new(&target)?;
        drop(file);
        for key in chunk {
            sources.push((key.file_name(), target.clone()));
        }
    }
    for entry in std::fs::read_dir(&held)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| super::invalid())?;
        if !name.starts_with("target-") {
            sources.push((name, entry.path()));
        }
    }
    sources.sort_unstable();
    link_unread_windows_tree(&base, &sources)?;

    std::fs::remove_dir_all(held)
}

/// Moves every link of the namespace at `base` into `held`, and leaves `base` empty.
fn link_unread_windows_set_aside(base: &Path, held: &Path) -> io::Result<()> {
    let mut pending = vec![base.to_owned()];
    let mut directories = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                pending.push(entry.path());
            } else {
                std::fs::rename(entry.path(), held.join(entry.file_name()))?;
            }
        }
        if directory != base {
            directories.push(directory);
        }
    }
    for directory in directories.iter().rev() {
        std::fs::remove_dir(directory)?;
    }

    Ok(())
}

/// Links `sources`, sorted by name, under `base` as nodes of the writer's grammar: a leaf of at
/// most 128 links, or a branch of at most 16 children that differ in the nibble after their shared
/// prefix.
fn link_unread_windows_tree(base: &Path, sources: &[(String, PathBuf)]) -> io::Result<()> {
    let mut branches = vec![(base.to_owned(), 0, sources)];
    while let Some((directory, nibbles, names)) = branches.pop() {
        for group in
            names.chunk_by(|left, right| left.0.as_bytes()[nibbles] == right.0.as_bytes()[nibbles])
        {
            let first = &group[0].0;
            let last = &group[group.len() - 1].0;
            let shared = first
                .bytes()
                .zip(last.bytes())
                .take_while(|(left, right)| left == right && *left != b'.')
                .count();
            let child = directory.join(&first[nibbles..shared]);
            private::create_private_dir(&child)?;
            if group.len() <= LEAF_LINKS_MAX {
                for (name, source) in group {
                    std::fs::hard_link(source, child.join(name))?;
                }
            } else {
                branches.push((child, shared, group));
            }
        }
    }

    Ok(())
}
