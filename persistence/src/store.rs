use std::fs::{self, File};
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use collector::recording::private::{self, FileLock};
use collector::ring::Identity;

use crate::Manifest;
use crate::metadata::invalid;

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
pub(crate) use restage::{Interrupted, Kept};
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
    #[cfg(test)]
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
            #[cfg(test)]
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

    fn validate_window_files(root: &Path, manifest: &Manifest) -> io::Result<()> {
        validate_data_directory(manifest)?;
        let path = root.join(&manifest.data_directory);
        private::verify_directory(&path)?;
        Self::validate_files(&path, manifest)?;
        for table in &manifest.tables {
            if table.name == "events" {
                event_table::validate(&path.join("events"), manifest)?;
                continue;
            }
            private::verify_file(&path.join(format!("{}.parquet", table.name)))?;
        }
        Ok(())
    }

    fn validate_files(path: &Path, manifest: &Manifest) -> io::Result<()> {
        let mut entries = fs::read_dir(path)?;
        let mut count = 0;
        for _index in 0..WINDOW_FILES_MAX {
            let Some(entry) = entries.next() else {
                if count != manifest.tables.len() + 1 {
                    return Err(invalid("missing window file"));
                }
                return Ok(());
            };
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_str().ok_or_else(|| invalid("file name encoding"))?;
            if name == PROOF_NAME || name == "events" {
                count += 1;
                continue;
            }
            if !manifest
                .tables
                .iter()
                .any(|table| table.name != "events" && name == format!("{}.parquet", table.name))
            {
                return Err(invalid("unexpected window file"));
            }
            count += 1;
        }
        Err(invalid("incomplete file enumeration"))
    }
}

impl Drop for StagingPermit {
    fn drop(&mut self) {
        let held = self.0.swap(false, Ordering::Release);
        assert!(held);
    }
}

/// Refuses a data directory other than an attempt's, `data/<attempt>/ready`.
pub(crate) fn validate_data_directory(manifest: &Manifest) -> io::Result<()> {
    let mut parts = Path::new(&manifest.data_directory).components();
    let attempt = parts.next().is_some_and(|base| base.as_os_str() == "data")
        && parts.next().is_some()
        && parts
            .next()
            .is_some_and(|child| child.as_os_str() == "ready")
        && parts.next().is_none();
    if !attempt {
        return Err(invalid("data directory identity"));
    }

    Ok(())
}

/// The file that stands for `manifest`'s commitment, which the read functions below take: the proof
/// in the window's directory, which its gates link.
#[must_use]
pub fn commitment_path(root: &Path, manifest: &Manifest) -> PathBuf {
    root.join(&manifest.data_directory).join(PROOF_NAME)
}

pub fn read_committed(root: &Path, marker: &Path) -> io::Result<Manifest> {
    let mut file = private::open_private_file(marker, false)?;
    read_committed_file(root, marker, &mut file)
}

/// Reads the caller's opened proof so its identity can be bound across parsing.
pub(crate) fn read_committed_file(
    root: &Path,
    marker: &Path,
    file: &mut File,
) -> io::Result<Manifest> {
    private::verify_directory(root)?;
    let manifest = read_proof(root, marker, file)?;
    Store::validate_window_files(root, &manifest)?;
    validate_link(file)?;
    let directory = private::PrivateDirectory::open(root)?;
    match collector::recording::bootstrap::classify(&directory, &mut [0; 4097])? {
        collector::recording::bootstrap::Classification::Initialized(recording)
            if recording == manifest.identity.run.recording =>
        {
            Ok(manifest)
        }
        _ => Err(invalid("recording identity mismatch")),
    }
}

/// Rereads a proof that `read_committed` accepted, for a reader holding the window open. A sealed
/// window's files never change, so the proof still being linked shows its commitment stands,
/// without verifying every file of the window again.
pub fn reread_committed(root: &Path, marker: &Path) -> io::Result<Manifest> {
    let mut file = private::open_private_file(marker, false)?;
    let manifest = read_proof(root, marker, &mut file)?;
    validate_data_directory(&manifest)?;
    validate_link(&file)?;
    Ok(manifest)
}

/// A window's proof is the one file its name in the catalog links to, so a proof no name links to
/// is a copy, which the path a reader opened cannot tell apart. The catalog compares that link
/// itself when it lists the window.
fn validate_link(proof: &File) -> io::Result<()> {
    if proof.metadata()?.nlink() < 2 {
        return Err(invalid("proof without its catalog link"));
    }

    Ok(())
}

/// Reads the manifest out of a window's proof, which names the directory it must lie in.
fn read_proof(root: &Path, path: &Path, file: &mut File) -> io::Result<Manifest> {
    let metadata = crate::canonical::CanonicalMetadata::read(file)?;
    let crate::canonical::metadata::Body::Ordinary(manifest) = metadata.body else {
        return Err(invalid("proof of no ordinary window"));
    };
    if root.join(&manifest.data_directory).join(PROOF_NAME) != path {
        return Err(invalid("noncanonical proof path"));
    }

    Ok(manifest)
}

#[cfg(test)]
mod registration_tests;
