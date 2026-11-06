use std::fs::{self, File};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use collector::recording::private::{self, FileLock};
use collector::ring::Identity;

use crate::layout::{self, Kind, WindowKey};
use crate::metadata::invalid;
use crate::{Manifest, WindowIdentity};

mod attempt;
mod finite;
mod finite_scratch;
pub use finite::{FiniteEventRows, FiniteEvents};
mod census_digest;
mod inventory;
mod normal_proof;
mod ordinary_recovery;
mod publication;
pub use ordinary_recovery::OrdinaryRecovery;
mod publication_control;
mod restage;
mod row_candidates;
mod row_merge;
pub use attempt::{InheritedAttempt, RegisteredAttempt, RegisteredFile};
pub use inventory::{
    BatchFact, FileIdentity, IndexFact, InventoryStep, InventoryWork, OriginalFile,
    OriginalInventory, OriginalPlacement,
};
pub(crate) use restage::Interrupted;
mod publication_ledger;
pub use publication::{
    LossPublication, NormalProof, NormalPublication, ProofStep, PublicationStep, PublicationWork,
    RowPublication, RowPublicationStep,
};
pub use publication_ledger::PublicationLedger;
mod event_table;
#[cfg(test)]
mod tests;

pub struct Store {
    root: PathBuf,
    directory: private::PrivateDirectory,
    recording: Identity,
    owner: FileLock,
    staging: Arc<AtomicBool>,
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

const WINDOW_FILES_MAX: usize = 17;
/// The file in a registered window's directory that its gates link.
const PROOF_NAME: &str = "metadata";
const _: () = {
    // An enumeration reads every table, held or derived, the metadata, and then its end.
    assert!(
        crate::schema::TABLES.len() + crate::schema::DERIVED_TABLES.len() + 1 < WINDOW_FILES_MAX
    );
};

impl Store {
    pub fn open(root: &Path) -> io::Result<Self> {
        collector::recording::check_writer(root)?;
        private::create_private_dir(root)?;
        let directory = private::PrivateDirectory::open(root)?;
        let registration = FileLock::wait_short_in(&directory, "registration.lock")?;
        let recording = collector::recording::recording_identity(root, &registration)?;
        drop(registration);
        let owner = match FileLock::try_lock_in(&directory, "persister.lock") {
            Ok(owner) => owner,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, OwnershipBusy));
            }
            Err(error) => return Err(error),
        };
        directory.full_sync()?;
        Ok(Self {
            root: root.to_owned(),
            directory,
            recording,
            owner,
            staging: Arc::new(AtomicBool::new(false)),
        })
    }

    #[cfg(test)]
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub fn recording(&self) -> Identity {
        self.recording
    }

    pub fn registration_lock(&self) -> io::Result<FileLock> {
        FileLock::try_lock_in(&self.directory, "registration.lock")
    }

    /// The directory holding a sealed window's tables.
    #[must_use]
    pub fn data_path(&self, manifest: &Manifest) -> PathBuf {
        self.root.join(&manifest.data_directory)
    }

    fn validate_committed(root: &Path, manifest: &Manifest, marker: &File) -> io::Result<()> {
        Self::validate_data(root, manifest)?;
        Self::validate_link(root, manifest, marker)
    }

    /// The marker's own open already checked the inode both names share, so the metadata name
    /// is checked without following it and without opening the inode a second time.
    fn validate_link(root: &Path, manifest: &Manifest, marker: &File) -> io::Result<()> {
        let target = marker.metadata()?;
        // A window's proof is the one file its name in the catalog links to, so a proof no name
        // links to is a copy, which the path a reader opened cannot tell apart. The catalog
        // compares that link itself when it lists the window.
        if registered_directory(&manifest.data_directory) {
            if target.nlink() < 2 {
                return Err(invalid("proof without its catalog link"));
            }
            return Ok(());
        }

        let source = fs::symlink_metadata(root.join(&manifest.data_directory).join("metadata"))?;
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
            if table.name == "events" && manifest.event_batches.is_some() {
                event_table::validate(&path.join("events"), manifest)?;
                continue;
            }
            private::verify_file(&path.join(format!("{}.parquet", table.name)))?;
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
            if name == "events" && manifest.event_batches.is_some() {
                count += 1;
                continue;
            }
            if !manifest.tables.iter().any(|table| {
                !(table.name == "events" && manifest.event_batches.is_some())
                    && name == format!("{}.parquet", table.name)
            }) {
                return Err(invalid("unexpected window file"));
            }
            count += 1;
        }
        Err(invalid("incomplete file enumeration"))
    }

    fn verify_partition(root: &Path, kind: Kind, identity: &WindowIdentity) -> io::Result<()> {
        private::verify_directory(&layout::base(root, kind))?;
        let run = layout::run(root, kind, identity.run.run);
        private::verify_directory(&run)?;
        private::verify_directory(&partition_root(root, kind, identity))
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
pub(crate) fn validate_data_directory(manifest: &Manifest) -> io::Result<()> {
    if registered_directory(&manifest.data_directory) {
        // The proof that holds this manifest was decoded against its attempt's directory.
        return Ok(());
    }
    if Path::new(&manifest.data_directory) != ready_relative(manifest) {
        return Err(invalid("data directory identity"));
    }
    Ok(())
}

/// The file that stands for `manifest`'s commitment, which the read functions below take. A
/// window of a registered attempt has no marker: its gates link the proof in its own directory.
#[must_use]
pub fn commitment_path(root: &Path, manifest: &Manifest) -> PathBuf {
    if registered_directory(&manifest.data_directory) {
        root.join(&manifest.data_directory).join(PROOF_NAME)
    } else {
        WindowKey::from(&manifest.identity).marker(root)
    }
}

/// Whether a window's directory is an attempt's, `data/<attempt>/ready`, rather than one its
/// identity names, which lies deeper.
fn registered_directory(data_directory: &str) -> bool {
    let mut parts = Path::new(data_directory).components();
    parts.next().is_some_and(|base| base.as_os_str() == "data")
        && parts.next().is_some()
        && parts
            .next()
            .is_some_and(|child| child.as_os_str() == "ready")
        && parts.next().is_none()
}

pub fn read_committed(root: &Path, marker: &Path) -> io::Result<Manifest> {
    let mut file = private::open_private_file(marker, false)?;
    read_committed_file(root, marker, &mut file)
}

/// Reads the caller's opened marker so its identity can be bound across parsing.
pub(crate) fn read_committed_file(
    root: &Path,
    marker: &Path,
    file: &mut File,
) -> io::Result<Manifest> {
    private::verify_directory(root)?;
    let manifest = read_marker_file(root, marker, file)?;
    if registered_directory(&manifest.data_directory) {
        Store::validate_window_files(root, &manifest)?;
        Store::validate_link(root, &manifest, file)?;
        if registered_recording(root)? != manifest.identity.run.recording {
            return Err(invalid("recording identity mismatch"));
        }
        return Ok(manifest);
    }
    Store::verify_partition(root, Kind::Commits, &manifest.identity)?;
    Store::validate_committed(root, &manifest, file)?;
    if collector::recording::read_identity(root)? != manifest.identity.run.recording {
        return Err(invalid("recording identity mismatch"));
    }
    Ok(manifest)
}

fn registered_recording(root: &Path) -> io::Result<Identity> {
    let directory = private::PrivateDirectory::open(root)?;
    match collector::recording::bootstrap::classify(&directory, &mut [0; 4097])? {
        collector::recording::bootstrap::Classification::Initialized(recording) => Ok(recording),
        _ => Err(invalid("recording identity mismatch")),
    }
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
    let manifest = read_marker_file(root, marker, &mut file)?;
    Ok((manifest, file))
}

fn read_marker_file(root: &Path, marker: &Path, file: &mut File) -> io::Result<Manifest> {
    if marker.file_name().is_some_and(|name| name == PROOF_NAME) {
        return read_marker_file_proof(root, marker, file);
    }
    let manifest = Manifest::read(file)?;
    let expected = WindowKey::from(&manifest.identity).marker(root);
    if expected != marker {
        return Err(invalid("noncanonical marker path"));
    }
    Ok(manifest)
}

/// Reads the manifest out of a registered window's proof, which names the directory it must lie in.
fn read_marker_file_proof(root: &Path, marker: &Path, file: &mut File) -> io::Result<Manifest> {
    let metadata = crate::canonical::CanonicalMetadata::read(file)?;
    let crate::canonical::metadata::Body::Ordinary(manifest) = metadata.body else {
        return Err(invalid("proof of no ordinary window"));
    };
    if root.join(&manifest.data_directory).join(PROOF_NAME) != marker {
        return Err(invalid("noncanonical marker path"));
    }
    Ok(manifest)
}

fn partition_root(root: &Path, kind: Kind, identity: &WindowIdentity) -> PathBuf {
    WindowKey::from(identity).partition(root, kind)
}

#[cfg(test)]
mod registration_tests;
