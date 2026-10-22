use std::fs::{self, File};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use collector::recording::private::{self, FileLock};
use collector::ring::{Identity, RunIdentity};

use crate::layout::{self, Kind, WindowKey};
use crate::metadata::invalid;
use crate::{Manifest, WindowIdentity};

#[cfg(test)]
mod tests;

pub struct Store {
    root: PathBuf,
    recording: Identity,
    owner: FileLock,
    staging: Arc<AtomicBool>,
}

pub enum Begin {
    Committed(Manifest),
    Pending(PendingWindow),
}

pub struct PendingWindow {
    path: PathBuf,
    identity: WindowIdentity,
    permit: StagingPermit,
}

struct StagingPermit(Arc<AtomicBool>);

#[derive(Debug)]
struct OwnershipBusy;

impl std::fmt::Display for OwnershipBusy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("recording is owned by a surviving persister")
    }
}

impl std::error::Error for OwnershipBusy {}

#[must_use]
pub fn ownership_contended(error: &io::Error) -> bool {
    if let Some(source) = error.get_ref() {
        source.is::<OwnershipBusy>()
    } else {
        false
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommitBoundary {
    BeforeFilesSync,
    FilesSynced,
    BeforeMetadataWrite,
    MetadataSynced,
    BeforePendingSync,
    PendingSynced,
    BeforeReadyRename,
    ReadyRenamed,
    BeforeReadySync,
    ReadySynced,
    BeforeMarkerLink,
    MarkerLinked,
    BeforeMarkerSync,
    MarkerSynced,
}

const PARTITION_ENTRIES_MAX: usize = 1442;
const WINDOW_FILES_MAX: usize = 17;
const _: () = {
    // An enumeration reads every table, held or derived, the metadata, and then its end.
    assert!(
        crate::schema::TABLES.len() + crate::schema::DERIVED_TABLES.len() + 1 < WINDOW_FILES_MAX
    );
};

impl Store {
    pub fn open(root: &Path) -> io::Result<Self> {
        private::create_private_dir(root)?;
        let registration = FileLock::wait_short(&root.join("registration.lock"))?;
        let recording = collector::recording::recording_identity(root)?;
        drop(registration);
        let owner = match FileLock::try_lock(&root.join("persister.lock")) {
            Ok(owner) => owner,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, OwnershipBusy));
            }
            Err(error) => return Err(error),
        };
        private::create_private_dir(&root.join("data"))?;
        private::create_private_dir(&root.join("commits"))?;
        Ok(Self {
            root: root.to_owned(),
            recording,
            owner,
            staging: Arc::new(AtomicBool::new(false)),
        })
    }

    #[must_use]
    pub fn recording(&self) -> Identity {
        self.recording
    }

    pub fn registration_lock(&self) -> io::Result<FileLock> {
        FileLock::try_lock(&self.root.join("registration.lock"))
    }

    pub fn progress(&self, run: &RunIdentity) -> io::Result<Option<Manifest>> {
        let identity = WindowIdentity {
            run: *run,
            window: 0,
        };
        self.validate_identity(&identity)?;
        private::verify_directory(&self.root.join("commits"))?;
        let path = layout::run(&self.root, Kind::Commits, run.run);
        match private::verify_directory(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
        let mut latest = None;
        visit_names(&path, |name| {
            let day = layout::validate_day(name)?;
            let partition = path.join(name);
            private::verify_directory(&partition)?;
            let day_identity = WindowIdentity {
                run: *run,
                window: day
                    .checked_mul(layout::WINDOWS_PER_DAY)
                    .ok_or_else(|| invalid("elapsed day"))?,
            };
            Self::verify_partition(&self.root, Kind::Data, &day_identity)?;
            visit_names(&partition, |name| {
                let window = name
                    .strip_suffix(".seal")
                    .ok_or_else(|| invalid("marker name"))?;
                layout::validate_day(window)?;
                let (manifest, marker) = read_marker(&self.root, &partition.join(name))?;
                if manifest.identity.run != *run {
                    return Err(invalid("progress run identity mismatch"));
                }
                Self::validate_window_files(&self.root, &manifest)?;
                Self::validate_link(&self.root, &manifest, &marker)?;
                if latest.as_ref().is_none_or(|previous: &Manifest| {
                    previous.publication_end < manifest.publication_end
                }) {
                    latest = Some(manifest);
                }
                Ok(())
            })?;
            private::sync_directory(&partition)
        })?;
        Ok(latest)
    }

    /// The directory holding a sealed window's tables.
    #[must_use]
    pub fn data_path(&self, manifest: &Manifest) -> PathBuf {
        self.root.join(&manifest.data_directory)
    }

    #[must_use]
    pub fn marker_path(&self, identity: &WindowIdentity) -> PathBuf {
        WindowKey::from(identity).marker(&self.root)
    }

    pub fn existing(&self, identity: &WindowIdentity) -> io::Result<Option<Manifest>> {
        self.validate_identity(identity)?;
        match Self::verify_partition(&self.root, Kind::Commits, identity) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        }
        let marker = self.marker_path(identity);
        let mut file = match private::open_private_file(&marker, false) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let manifest = Manifest::read(&mut file)?;
        if manifest.identity != *identity {
            return Err(invalid("canonical identity mismatch"));
        }
        Self::validate_committed(&self.root, &manifest, &file)?;
        private::sync_directory(&self.partition(Kind::Commits, identity))?;
        Ok(Some(manifest))
    }

    pub fn begin(&self, identity: &WindowIdentity) -> io::Result<Begin> {
        if self
            .staging
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        let permit = StagingPermit(Arc::clone(&self.staging));
        self.begin_pending(identity, permit)
    }

    fn begin_pending(&self, identity: &WindowIdentity, permit: StagingPermit) -> io::Result<Begin> {
        self.validate_identity(identity)?;
        if let Some(manifest) = self.existing(identity)? {
            return Ok(Begin::Committed(manifest));
        }
        self.create_partition(Kind::Data, identity)?;
        self.create_partition(Kind::Commits, identity)?;
        if let Some(manifest) = self.reconcile_ready(identity)? {
            return Ok(Begin::Committed(manifest));
        }
        let path = self
            .partition(Kind::Data, identity)
            .join(format!(".{:020}.pending", identity.window));
        Self::remove_pending(&path)?;
        private::create_private_dir(&path)?;
        Ok(Begin::Pending(PendingWindow {
            path,
            identity: *identity,
            permit,
        }))
    }

    pub fn commit(&self, pending: PendingWindow, manifest: Manifest) -> io::Result<Manifest> {
        self.commit_observed(pending, manifest, &mut |_| Ok(()))
    }

    pub fn commit_observed(
        &self,
        pending: PendingWindow,
        mut manifest: Manifest,
        observe: &mut impl FnMut(CommitBoundary) -> io::Result<()>,
    ) -> io::Result<Manifest> {
        self.validate_identity(&pending.identity)?;
        if !Arc::ptr_eq(&pending.permit.0, &self.staging) {
            return Err(invalid("foreign pending owner"));
        }
        if manifest.identity != pending.identity {
            return Err(invalid("pending identity mismatch"));
        }
        if let Some(committed) = self.existing(&manifest.identity)? {
            return Ok(committed);
        }
        manifest.validate()?;
        let relative = ready_relative(&manifest);
        manifest.data_directory = relative.to_string_lossy().into_owned();
        observe(CommitBoundary::BeforeFilesSync)?;
        Self::sync_tables(&pending.path, &manifest)?;
        observe(CommitBoundary::FilesSynced)?;
        let metadata_path = pending.path.join("metadata");
        if metadata_path.try_exists()? {
            return Err(invalid("pending metadata exists"));
        }
        observe(CommitBoundary::BeforeMetadataWrite)?;
        let mut metadata = private::open_private_file(&metadata_path, true)?;
        manifest.write(&mut metadata)?;
        private::full_sync(&metadata)?;
        drop(metadata);
        observe(CommitBoundary::MetadataSynced)?;
        observe(CommitBoundary::BeforePendingSync)?;
        private::sync_directory(&pending.path)?;
        observe(CommitBoundary::PendingSynced)?;
        let ready = self.root.join(relative);
        if ready.try_exists()? {
            return Err(invalid("ready directory exists"));
        }
        observe(CommitBoundary::BeforeReadyRename)?;
        fs::rename(&pending.path, &ready)?;
        observe(CommitBoundary::ReadyRenamed)?;
        observe(CommitBoundary::BeforeReadySync)?;
        private::sync_directory(&self.partition(Kind::Data, &manifest.identity))?;
        observe(CommitBoundary::ReadySynced)?;
        self.publish_marker(&manifest, observe)?;
        drop(pending);
        Ok(manifest)
    }

    fn publish_marker(
        &self,
        manifest: &Manifest,
        observe: &mut impl FnMut(CommitBoundary) -> io::Result<()>,
    ) -> io::Result<()> {
        let metadata = self.root.join(&manifest.data_directory).join("metadata");
        let marker = self.marker_path(&manifest.identity);
        observe(CommitBoundary::BeforeMarkerLink)?;
        match fs::hard_link(&metadata, &marker) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let committed = self
                    .existing(&manifest.identity)?
                    .ok_or_else(|| invalid("canonical marker disappeared"))?;
                if committed != *manifest {
                    return Err(invalid("canonical marker conflicts"));
                }
            }
            Err(error) => return Err(error),
        }
        observe(CommitBoundary::MarkerLinked)?;
        observe(CommitBoundary::BeforeMarkerSync)?;
        private::sync_directory(&self.partition(Kind::Commits, &manifest.identity))?;
        observe(CommitBoundary::MarkerSynced)
    }

    fn reconcile_ready(&self, identity: &WindowIdentity) -> io::Result<Option<Manifest>> {
        let partition = self.partition(Kind::Data, identity);
        let mut entries = fs::read_dir(&partition)?;
        let prefix = format!("{:020}_", identity.window);
        let mut orphan = None;
        for _index in 0..PARTITION_ENTRIES_MAX {
            let Some(entry) = entries.next() else {
                return self.reconcile_orphan(identity, orphan);
            };
            let entry = entry?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| invalid("directory name encoding"))?;
            if name.starts_with(&prefix) {
                if orphan.is_some() {
                    return Err(invalid("multiple ready orphans"));
                }
                orphan = Some(entry.path());
            }
        }
        if let Some(entry) = entries.next() {
            entry?;
            return Err(invalid("incomplete partition enumeration"));
        }
        self.reconcile_orphan(identity, orphan)
    }

    fn reconcile_orphan(
        &self,
        identity: &WindowIdentity,
        orphan: Option<PathBuf>,
    ) -> io::Result<Option<Manifest>> {
        let Some(path) = orphan else {
            return Ok(None);
        };
        private::verify_directory(&path)?;
        let mut metadata = private::open_private_file(&path.join("metadata"), false)?;
        let manifest = Manifest::read(&mut metadata)?;
        if manifest.identity != *identity {
            return Err(invalid("orphan identity mismatch"));
        }
        if self.root.join(&manifest.data_directory) != path {
            return Err(invalid("orphan data identity mismatch"));
        }
        Self::validate_data(&self.root, &manifest)?;
        private::sync_directory(&self.partition(Kind::Data, identity))?;
        self.publish_marker(&manifest, &mut |_| Ok(()))?;
        Ok(Some(manifest))
    }

    fn validate_committed(root: &Path, manifest: &Manifest, marker: &File) -> io::Result<()> {
        Self::validate_data(root, manifest)?;
        Self::validate_link(root, manifest, marker)
    }

    /// The marker's own open already checked the inode both names share, so the metadata name
    /// is checked without following it and without opening the inode a second time.
    fn validate_link(root: &Path, manifest: &Manifest, marker: &File) -> io::Result<()> {
        let source = fs::symlink_metadata(root.join(&manifest.data_directory).join("metadata"))?;
        let target = marker.metadata()?;
        if source.dev() != target.dev() {
            return Err(invalid("marker device mismatch"));
        }
        if source.ino() != target.ino() {
            return Err(invalid("marker inode mismatch"));
        }
        Ok(())
    }

    fn validate_data(root: &Path, manifest: &Manifest) -> io::Result<()> {
        Self::verify_partition(root, Kind::Data, &manifest.identity)?;
        Self::validate_window_files(root, manifest)
    }

    fn validate_window_files(root: &Path, manifest: &Manifest) -> io::Result<()> {
        validate_data_directory(manifest)?;
        let path = root.join(&manifest.data_directory);
        private::verify_directory(&path)?;
        Self::validate_files(&path, manifest, true)?;
        for table in &manifest.tables {
            private::verify_file(&path.join(format!("{}.parquet", table.name)))?;
        }
        Ok(())
    }

    fn sync_tables(pending: &Path, manifest: &Manifest) -> io::Result<()> {
        Self::validate_files(pending, manifest, false)?;
        for table in &manifest.tables {
            let path = pending.join(format!("{}.parquet", table.name));
            private::make_file_private(&path)?;
            private::full_sync(&private::open_private_file(&path, false)?)?;
        }
        Ok(())
    }

    fn validate_files(path: &Path, manifest: &Manifest, metadata: bool) -> io::Result<()> {
        let mut entries = fs::read_dir(path)?;
        let mut count = 0;
        for _index in 0..WINDOW_FILES_MAX {
            let Some(entry) = entries.next() else {
                let expected = manifest.tables.len() + usize::from(metadata);
                if count != expected {
                    return Err(invalid("missing window file"));
                }
                return Ok(());
            };
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| invalid("file name encoding"))?;
            if metadata {
                if name == "metadata" {
                    count += 1;
                    continue;
                }
            }
            if !manifest
                .tables
                .iter()
                .any(|table| name == format!("{}.parquet", table.name))
            {
                return Err(invalid("unexpected window file"));
            }
            count += 1;
        }
        Err(invalid("incomplete file enumeration"))
    }

    fn remove_pending(path: &Path) -> io::Result<()> {
        match fs::symlink_metadata(path) {
            Ok(_) => private::verify_directory(path)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        }
        let mut entries = fs::read_dir(path)?;
        let mut files = Vec::with_capacity(WINDOW_FILES_MAX);
        for _index in 0..WINDOW_FILES_MAX {
            let Some(entry) = entries.next() else {
                for file in files {
                    fs::remove_file(file)?;
                }
                fs::remove_dir(path)?;
                return Ok(());
            };
            let path = entry?.path();
            private::verify_file(&path)?;
            files.push(path);
        }
        Err(invalid("incomplete pending enumeration"))
    }

    fn create_partition(&self, kind: Kind, identity: &WindowIdentity) -> io::Result<()> {
        let run = layout::run(&self.root, kind, identity.run.run);
        private::create_private_dir(&run)?;
        private::create_private_dir(&self.partition(kind, identity))
    }

    fn verify_partition(root: &Path, kind: Kind, identity: &WindowIdentity) -> io::Result<()> {
        private::verify_directory(&layout::base(root, kind))?;
        let run = layout::run(root, kind, identity.run.run);
        private::verify_directory(&run)?;
        private::verify_directory(&partition_root(root, kind, identity))
    }

    fn partition(&self, kind: Kind, identity: &WindowIdentity) -> PathBuf {
        partition_root(&self.root, kind, identity)
    }

    fn validate_identity(&self, identity: &WindowIdentity) -> io::Result<()> {
        if identity.run.recording != self.recording {
            return Err(invalid("recording identity mismatch"));
        }
        if self.owner.file().metadata()?.nlink() != 1 {
            return Err(invalid("ownership lock changed"));
        }
        private::verify_directory(&self.root)
    }
}

impl PendingWindow {
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for StagingPermit {
    fn drop(&mut self) {
        let held = self.0.swap(false, Ordering::Release);
        assert!(held);
    }
}

fn ready_relative(manifest: &Manifest) -> PathBuf {
    WindowKey::from(&manifest.identity).ready_relative(manifest.wall_ns)
}

/// Refuses a data directory other than the one the window's identity names.
fn validate_data_directory(manifest: &Manifest) -> io::Result<()> {
    if Path::new(&manifest.data_directory) != ready_relative(manifest) {
        return Err(invalid("data directory identity"));
    }
    Ok(())
}

pub fn read_committed(root: &Path, marker: &Path) -> io::Result<Manifest> {
    private::verify_directory(root)?;
    let (manifest, file) = read_marker(root, marker)?;
    Store::verify_partition(root, Kind::Commits, &manifest.identity)?;
    Store::validate_committed(root, &manifest, &file)?;
    if collector::recording::read_identity(root)? != manifest.identity.run.recording {
        return Err(invalid("recording identity mismatch"));
    }
    Ok(manifest)
}

/// Rereads a marker that `read_committed` accepted, for a reader holding the window open. A
/// sealed window's files never change, so the marker still linking the window's metadata
/// shows its commitment stands, without verifying every file of the window again.
pub fn reread_committed(root: &Path, marker: &Path) -> io::Result<Manifest> {
    read_linked_marker(root, marker)
}

/// Lists a sealed window of `recording` from its marker alone, for a reader that verifies the
/// window's files with `read_committed` before its first read of them.
pub fn list_committed(root: &Path, marker: &Path, recording: Identity) -> io::Result<Manifest> {
    let manifest = read_linked_marker(root, marker)?;
    if manifest.identity.run.recording != recording {
        return Err(invalid("recording identity mismatch"));
    }
    Ok(manifest)
}

/// Reads a marker that sits where its identity puts it and links the window's metadata where
/// its identity puts that, without verifying the window's files.
fn read_linked_marker(root: &Path, marker: &Path) -> io::Result<Manifest> {
    let (manifest, file) = read_marker(root, marker)?;
    validate_data_directory(&manifest)?;
    Store::validate_link(root, &manifest, &file)?;
    Ok(manifest)
}

fn read_marker(root: &Path, marker: &Path) -> io::Result<(Manifest, File)> {
    let mut file = private::open_private_file(marker, false)?;
    let manifest = Manifest::read(&mut file)?;
    let expected = WindowKey::from(&manifest.identity).marker(root);
    if expected != marker {
        return Err(invalid("noncanonical marker path"));
    }
    Ok((manifest, file))
}

fn partition_root(root: &Path, kind: Kind, identity: &WindowIdentity) -> PathBuf {
    WindowKey::from(identity).partition(root, kind)
}

fn visit_names(path: &Path, visit: impl FnMut(&str) -> io::Result<()>) -> io::Result<()> {
    layout::visit_names(path, visit)
}
