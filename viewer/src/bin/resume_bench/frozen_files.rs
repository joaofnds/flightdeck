use std::cell::RefCell;
use std::fs;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Component, Path, PathBuf};

use collector::recording::private;
use collector::ring::{Identity, RunIdentity};
use persistence::Manifest;

use super::Directories;

pub(super) struct FrozenFiles {
    markers: Vec<FrozenMarker>,
    hidden: RefCell<Vec<usize>>,
    pub(super) windows: u64,
    pub(super) captured: Vec<CapturedWindow>,
}

struct FrozenMarker {
    visible: PathBuf,
    hidden: PathBuf,
    publication_end: u64,
    window: u64,
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
const RUNS_MAX: usize = 4096;
const DAYS_MAX: usize = 4096;
const COMPONENTS_MAX: usize = 8;

impl FrozenFiles {
    pub(super) fn capture(
        directories: &Directories<'_>,
        recording: Identity,
        active: &RunIdentity,
    ) -> io::Result<Self> {
        fs::hard_link(
            directories.source.join("identity"),
            directories.target.join("identity"),
        )?;
        create_directory(&directories.target.join("commits"))?;
        create_directory(&directories.target.join("data"))?;
        create_directory(&directories.target.join("frozen_hidden"))?;
        let mut windows = Vec::new();
        capture_metadata(directories, recording, &mut windows)?;

        let mut markers = Vec::new();
        for window in &windows {
            capture_data(directories, window)?;
            if window.manifest.identity.run == *active {
                let relative = window
                    .marker
                    .strip_prefix(directories.target)
                    .map_err(io::Error::other)?;
                let hidden = directories.target.join("frozen_hidden").join(relative);
                create_parent(&Destination {
                    root: directories.target,
                    path: &hidden,
                })?;
                markers.push(FrozenMarker {
                    visible: window.marker.clone(),
                    hidden,
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
                fs::rename(&marker.visible, &marker.hidden)?;
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
            let marker = &self.markers[index];
            fs::rename(&marker.hidden, &marker.visible)?;
            assert_eq!(hidden.pop(), Some(index));
        }
        assert!(hidden.is_empty());
        Ok(())
    }

    pub(super) fn replay_seal(&self, window: u64, newest: u64) -> io::Result<()> {
        let mut hidden = self.hidden.borrow_mut();
        let position = hidden
            .iter()
            .position(|index| self.markers[*index].window == window)
            .ok_or_else(|| io::Error::other("seal is absent or already visible"))?;
        let marker = &self.markers[hidden[position]];
        if marker.publication_end > newest + 1 {
            return Err(io::Error::other("seal extends past replay endpoint"));
        }

        fs::rename(&marker.hidden, &marker.visible)?;
        hidden.remove(position);
        Ok(())
    }

    pub(super) fn replay_seal_eligible(&self, newest: u64) -> io::Result<()> {
        assert!(self.markers.len() <= WINDOWS_MAX);
        for marker in &self.markers {
            if marker.publication_end == newest + 1 {
                if marker.hidden.try_exists()? {
                    self.replay_seal(marker.window, newest)?;
                }
            }
        }
        Ok(())
    }
}

fn capture_metadata(
    directories: &Directories<'_>,
    recording: Identity,
    windows: &mut Vec<CapturedWindow>,
) -> io::Result<()> {
    visit_entries(&directories.source.join("commits"), RUNS_MAX, |run| {
        visit_entries(run, DAYS_MAX, |day| {
            visit_entries(day, WINDOWS_MAX, |marker| {
                if windows.len() == WINDOWS_MAX {
                    return Err(io::Error::other("frozen recording window bound"));
                }
                let relative = marker
                    .strip_prefix(directories.source)
                    .map_err(io::Error::other)?;
                let captured = directories.target.join(relative);
                create_parent(&Destination {
                    root: directories.target,
                    path: &captured,
                })?;
                fs::hard_link(marker, &captured)?;
                let manifest = persistence::list_committed(directories.source, marker, recording)?;
                windows.push(CapturedWindow {
                    manifest,
                    marker: captured,
                });
                Ok(())
            })
        })
    })
}

fn capture_data(directories: &Directories<'_>, window: &CapturedWindow) -> io::Result<()> {
    let relative = Path::new(&window.manifest.data_directory);
    let directory = directories.target.join(relative);
    create_parent(&Destination {
        root: directories.target,
        path: &directory.join("metadata"),
    })?;
    fs::hard_link(&window.marker, directory.join("metadata"))?;
    assert!(window.manifest.tables.len() <= 16);
    for table in &window.manifest.tables {
        let name = format!("{}.parquet", table.name);
        let original = directories.source.join(relative).join(&name);
        private::verify_file(&original)?;
        fs::hard_link(original, directory.join(name))?;
    }
    persistence::read_committed(directories.target, &window.marker)?;
    Ok(())
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

fn visit_entries(
    directory: &Path,
    maximum: usize,
    mut visit: impl FnMut(&Path) -> io::Result<()>,
) -> io::Result<()> {
    private::verify_directory(directory)?;
    let entries = fs::read_dir(directory)?;
    for (index, entry) in entries.take(maximum + 1).enumerate() {
        if index == maximum {
            return Err(io::Error::other("fixture directory entry bound"));
        }
        let entry = entry?;
        if entry.file_type()?.is_symlink() {
            return Err(io::Error::other("fixture source symlink"));
        }
        visit(&entry.path())?;
    }
    Ok(())
}
