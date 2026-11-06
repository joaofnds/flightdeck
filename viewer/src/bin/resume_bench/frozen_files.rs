use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::unix::fs::{DirBuilderExt, MetadataExt};
use std::path::{Component, Path, PathBuf};

use collector::recording::private;
use collector::ring::RunIdentity;
use persistence::Manifest;

use super::Directories;

pub(super) struct FrozenFiles {
    markers: Vec<FrozenMarker>,
    hidden: RefCell<Vec<usize>>,
    pub(super) windows: u64,
    pub(super) captured: Vec<CapturedWindow>,
}

/// The names that make a sealed window visible, one for its source and one for each reading.
struct FrozenMarker {
    names: Vec<FrozenName>,
    publication_end: u64,
    window: u64,
}

struct FrozenName {
    visible: PathBuf,
    hidden: PathBuf,
}

pub(super) struct Seal {
    pub(super) window: u64,
    pub(super) publication: u64,
}

pub(super) struct CapturedWindow {
    pub(super) manifest: Manifest,
    pub(super) marker: PathBuf,
}

struct Destination<'a> {
    root: &'a Path,
    path: &'a Path,
}

const WINDOWS_MAX: usize = 100_000;
const COMPONENTS_MAX: usize = 8;
/// More files and directories than the tree of 100,000 windows holds.
const ENTRIES_MAX: usize = 1 << 28;
/// The directories that hold what a recording saved. Their files never change once linked.
const SAVED_DIRECTORIES: [&str; 8] = [
    "data",
    "recovery-data",
    "ordinary",
    "observations-known",
    "observations-unknown",
    "terminal",
    "event-fragments",
    "retention",
];
/// The directories that name a window of the recording, by hard links to its proof.
const WINDOW_NAMES: [&str; 3] = ["ordinary", "observations-known", "observations-unknown"];
/// The files that describe the recording, which change by replacement.
const LINKED_FILES: [&str; 4] = ["identity", "foreground", "maintenance", "namespace-state"];
/// The locks that exist in a set up recording. A link would share the running recording's lock.
const LOCKS: [&str; 3] = ["registration.lock", "collector.lock", "recorder.lock"];

impl FrozenFiles {
    pub(super) fn capture(directories: &Directories<'_>, active: &RunIdentity) -> io::Result<Self> {
        for name in LINKED_FILES {
            fs::hard_link(directories.source.join(name), directories.target.join(name))?;
        }
        for name in LOCKS {
            drop(private::open_private_file(
                &directories.target.join(name),
                true,
            )?);
        }
        create_directory(&directories.target.join("frozen_hidden"))?;
        for name in SAVED_DIRECTORIES {
            capture_tree(directories, Path::new(name))?;
        }
        let windows = capture_windows(directories.target)?;
        let mut names = capture_names(directories.target)?;

        let mut markers = Vec::new();
        for window in &windows {
            if window.manifest.identity.run == *active {
                let proof = fs::metadata(&window.marker)?.ino();
                let links = names
                    .remove(&proof)
                    .ok_or_else(|| io::Error::other("frozen recording window without a name"))?;
                markers.push(FrozenMarker {
                    names: frozen_names(directories.target, links)?,
                    publication_end: window.manifest.publication_end,
                    window: window.manifest.identity.window,
                });
            }
        }
        let count = u64::try_from(windows.len()).map_err(io::Error::other)?;
        let hidden = RefCell::new(Vec::with_capacity(markers.len()));
        Ok(Self {
            markers,
            hidden,
            windows: count,
            captured: windows,
        })
    }

    pub(super) fn checkpoint(&self, newest: u64) -> io::Result<()> {
        self.advance()?;
        let mut hidden = self.hidden.borrow_mut();
        assert!(self.markers.len() <= WINDOWS_MAX);
        for (index, marker) in self.markers.iter().enumerate() {
            if marker.publication_end > newest + 1 {
                for name in &marker.names {
                    fs::rename(&name.visible, &name.hidden)?;
                }
                hidden.push(index);
            }
        }
        Ok(())
    }

    pub(super) fn advance(&self) -> io::Result<()> {
        let mut hidden = self.hidden.borrow_mut();
        assert!(hidden.len() <= self.markers.len());
        for _marker in 0..self.markers.len() {
            let Some(index) = hidden.last().copied() else {
                return Ok(());
            };
            reveal(&self.markers[index])?;
            assert_eq!(hidden.pop(), Some(index));
        }
        assert!(hidden.is_empty());
        Ok(())
    }

    pub(super) fn replay_seal(&self, seal: &Seal) -> io::Result<()> {
        let mut hidden = self.hidden.borrow_mut();
        let position = hidden
            .iter()
            .position(|index| self.markers[*index].window == seal.window)
            .ok_or_else(|| io::Error::other("seal is absent or already visible"))?;
        if self.markers[hidden[position]].publication_end > seal.publication + 1 {
            return Err(io::Error::other("seal extends past replay endpoint"));
        }

        self.replay_reveal(&mut hidden, position)
    }

    pub(super) fn replay_seal_eligible(&self, newest: u64) -> io::Result<()> {
        let mut hidden = self.hidden.borrow_mut();
        let mut position = 0;
        for _marker in 0..self.markers.len() {
            if position == hidden.len() {
                return Ok(());
            }

            if self.markers[hidden[position]].publication_end <= newest + 1 {
                self.replay_reveal(&mut hidden, position)?;
            } else {
                position += 1;
            }
        }
        assert_eq!(position, hidden.len());
        Ok(())
    }

    pub(super) fn replay_complete(&self, newest: u64) -> io::Result<()> {
        self.replay_seal_eligible(newest)?;
        if !self.hidden.borrow().is_empty() {
            return Err(io::Error::other(
                "captured seals extend past final replay input",
            ));
        }

        Ok(())
    }

    fn replay_reveal(&self, hidden: &mut Vec<usize>, position: usize) -> io::Result<()> {
        assert!(position < hidden.len());
        reveal(&self.markers[hidden[position]])?;
        hidden.remove(position);
        Ok(())
    }
}

/// Makes a hidden window visible again. A reveal that failed part way resumes where it stopped.
fn reveal(marker: &FrozenMarker) -> io::Result<()> {
    for name in &marker.names {
        match fs::rename(&name.hidden, &name.visible) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }

    Ok(())
}

/// Links the files under `relative` of the source into the target, with their directories.
fn capture_tree(directories: &Directories<'_>, relative: &Path) -> io::Result<()> {
    create_directory(&directories.target.join(relative))?;
    let mut unread = vec![relative.to_path_buf()];
    let mut entries = 0;
    while let Some(directory) = unread.pop() {
        for entry in fs::read_dir(directories.source.join(&directory))? {
            entries += 1;
            if entries == ENTRIES_MAX {
                return Err(io::Error::other("frozen recording entry bound"));
            }
            let entry = entry?;
            let kind = entry.file_type()?;
            let child = directory.join(entry.file_name());
            if kind.is_symlink() {
                return Err(io::Error::other("fixture source symlink"));
            }
            if kind.is_dir() {
                create_directory(&directories.target.join(&child))?;
                unread.push(child);
            } else {
                fs::hard_link(entry.path(), directories.target.join(&child))?;
            }
        }
    }

    Ok(())
}

/// The windows a captured recording holds, each with the proof that commits it.
fn capture_windows(target: &Path) -> io::Result<Vec<CapturedWindow>> {
    let mut windows = Vec::new();
    for attempt in fs::read_dir(target.join("data"))? {
        let marker = attempt?.path().join("ready").join("metadata");
        let proof = match fs::metadata(&marker) {
            Ok(proof) => proof,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        // A proof no name links to was never published, so no reader ever listed its window.
        if proof.nlink() < 2 {
            continue;
        }

        if windows.len() == WINDOWS_MAX {
            return Err(io::Error::other("frozen recording window bound"));
        }
        let manifest = persistence::read_committed(target, &marker)?;
        windows.push(CapturedWindow { manifest, marker });
    }

    Ok(windows)
}

/// The names of each proof in the directories that name windows, by the proof's inode.
fn capture_names(target: &Path) -> io::Result<HashMap<u64, Vec<PathBuf>>> {
    let mut names: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    let mut unread: Vec<PathBuf> = WINDOW_NAMES.iter().map(|name| target.join(name)).collect();
    let mut entries = 0;
    while let Some(directory) = unread.pop() {
        for entry in fs::read_dir(&directory)? {
            entries += 1;
            if entries == ENTRIES_MAX {
                return Err(io::Error::other("frozen recording entry bound"));
            }
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                unread.push(entry.path());
            } else {
                let inode = entry.metadata()?.ino();
                names.entry(inode).or_default().push(entry.path());
            }
        }
    }

    Ok(names)
}

fn frozen_names(target: &Path, links: Vec<PathBuf>) -> io::Result<Vec<FrozenName>> {
    let mut names = Vec::with_capacity(links.len());
    for visible in links {
        let relative = visible.strip_prefix(target).map_err(io::Error::other)?;
        let hidden = target.join("frozen_hidden").join(relative);
        create_parent(&Destination {
            root: target,
            path: &hidden,
        })?;
        names.push(FrozenName { visible, hidden });
    }

    Ok(names)
}

fn create_parent(destination: &Destination<'_>) -> io::Result<()> {
    let parent = destination
        .path
        .parent()
        .ok_or_else(|| io::Error::other("fixture path parent"))?;
    let relative = parent
        .strip_prefix(destination.root)
        .map_err(io::Error::other)?;
    let mut directory = destination.root.to_path_buf();
    for (index, component) in relative.components().take(COMPONENTS_MAX + 1).enumerate() {
        if index == COMPONENTS_MAX {
            return Err(io::Error::other("fixture path depth bound"));
        }
        let Component::Normal(name) = component else {
            return Err(io::Error::other("fixture path component"));
        };
        directory.push(name);
        create_directory(&directory)?;
    }
    Ok(())
}

fn create_directory(path: &Path) -> io::Result<()> {
    // The fixture needs private immutable links, but no per-directory durability barrier.
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Ok(()) => private::verify_directory(path),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            private::verify_directory(path)
        }
        Err(error) => Err(error),
    }
}
