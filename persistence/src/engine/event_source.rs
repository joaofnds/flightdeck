//! Cached logical event relations over independently admitted physical parts.
use std::fs::File;
use std::io::Write;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use collector::recording::private;

use super::sources::Fingerprint;
use super::{AdmissionRefusal, AdmittedReadRefusal, Engine};
use crate::event_table::EventTable;
use crate::resources::{ByteBudget, ByteReservation};
use crate::{Error, Manifest, Result};

/// Exact immutable source identity, retained separately from the bounded SQL alias.
#[derive(Clone, Debug)]
pub struct EventSourcePin(Arc<EventSourceSnapshot>);

#[derive(Debug)]
struct EventSourceSnapshot {
    marker: PathBuf,
    directory: PathBuf,
    marker_directory: (u64, u64),
    partitioned: bool,
    files: Vec<Fingerprint>,
    reservation: ByteReservation,
}

const FILES_MAX: usize = crate::event_recovery::metadata::BATCHES_MAX + 2;
const SNAPSHOT_OVERHEAD: usize = size_of::<EventSourceSnapshot>() + 2 * size_of::<usize>();

impl Engine {
    /// Registers the current source; saved SQL also needs the identity from `event_source_pinned`.
    pub fn event_source(
        &self,
        root: &Path,
        manifest: &Manifest,
        budget: &Arc<ByteBudget>,
    ) -> Result<String> {
        self.event_source_budgeted(root, manifest, budget)
            .map_err(crate::event_table::query::ordinary_error)
    }

    /// The same immutable table under different caller ledgers gets distinct registrations.
    pub fn event_source_budgeted(
        &self,
        root: &Path,
        manifest: &Manifest,
        budget: &Arc<ByteBudget>,
    ) -> std::result::Result<String, AdmittedReadRefusal> {
        self.event_source_pinned_budgeted(root, manifest, budget)
            .map(|(alias, _pin)| alias)
    }

    /// Keep the pin with saved SQL and compare it when refreshing an evicted registration.
    pub fn event_source_pinned(
        &self,
        root: &Path,
        manifest: &Manifest,
        budget: &Arc<ByteBudget>,
    ) -> Result<(String, EventSourcePin)> {
        self.event_source_pinned_budgeted(root, manifest, budget)
            .map_err(crate::event_table::query::ordinary_error)
    }

    pub fn event_source_pinned_budgeted(
        &self,
        root: &Path,
        manifest: &Manifest,
        budget: &Arc<ByteBudget>,
    ) -> std::result::Result<(String, EventSourcePin), AdmittedReadRefusal> {
        let pin = EventSourcePin::capture(root, manifest, budget)?;
        pin.validate_commitment(root, manifest)?;
        let mut sources = self
            .sources
            .lock()
            .map_err(|_| Error::Bound("query sources poisoned"))?;
        let mut context = self
            .context
            .lock()
            .map_err(|_| Error::Bound("query context poisoned"))?;
        sources.event_source(&mut context, &pin, budget, || {
            EventTable::open_budgeted_pinned(root, manifest, budget, &pin)?.into_lazy(budget)
        })
    }
}

impl EventSourcePin {
    pub(crate) const CAPTURE_BYTES_MAX: usize =
        SNAPSHOT_OVERHEAD + 8 * private::PATH_MAX + FILES_MAX * size_of::<Fingerprint>();

    pub(crate) fn capture(
        root: &Path,
        manifest: &Manifest,
        budget: &Arc<ByteBudget>,
    ) -> std::result::Result<Self, AdmittedReadRefusal> {
        crate::query::validate_root(root)?;
        if root.as_os_str().len() >= private::PATH_MAX
            || manifest.data_directory.len() >= private::PATH_MAX
        {
            return Err(Error::Bound("event source path size").into());
        }
        let count = manifest
            .event_batches
            .map_or(Ok(1), usize::try_from)
            .map_err(|_| Error::Bound("event source part count"))?;
        if count > crate::event_recovery::metadata::BATCHES_MAX {
            return Err(Error::Bound("event source part count").into());
        }
        let count = count + 1 + usize::from(manifest.event_batches.is_some());
        let reservation = budget
            .reserve(SNAPSHOT_OVERHEAD + 8 * private::PATH_MAX + count * size_of::<Fingerprint>())
            .ok_or(AdmittedReadRefusal::Admission(AdmissionRefusal::Budget))?;
        if Path::new(&manifest.data_directory)
            != crate::layout::WindowKey::from(&manifest.identity).ready_relative(manifest.wall_ns)
        {
            return Err(Error::Bound("event source data directory").into());
        }
        let marker = crate::layout::WindowKey::from(&manifest.identity).marker(root);
        let directory = root.join(&manifest.data_directory);
        if marker.as_os_str().len() >= private::PATH_MAX
            || directory.as_os_str().len() >= private::PATH_MAX
        {
            return Err(Error::Bound("event source path size").into());
        }
        let marker_directory = marker
            .parent()
            .ok_or(Error::Bound("event marker directory"))?
            .metadata();
        crate::work::record(|counts| counts.source_metadata_checks += 1);
        let marker_directory = marker_directory?;
        let mut snapshot = EventSourceSnapshot {
            marker,
            directory,
            marker_directory: (marker_directory.dev(), marker_directory.ino()),
            partitioned: manifest.event_batches.is_some(),
            files: Vec::with_capacity(count),
            reservation,
        };
        for index in 0..count {
            let fingerprint = snapshot.with_path(index, Fingerprint::read)?;
            snapshot.files.push(fingerprint);
        }
        snapshot.reservation.shrink_to(
            SNAPSHOT_OVERHEAD
                + snapshot.marker.capacity()
                + snapshot.directory.capacity()
                + snapshot.files.capacity() * size_of::<Fingerprint>(),
        );
        let pin = Self(Arc::new(snapshot));
        if !pin.validate()? {
            return Err(Error::Bound("changed event source identity").into());
        }
        Ok(pin)
    }

    /// A changed identity is false; filesystem failures retain their original error.
    pub fn validate(&self) -> Result<bool> {
        crate::work::record(|counts| counts.source_metadata_checks += 1);
        let directory = self
            .0
            .marker
            .parent()
            .ok_or(Error::Bound("event marker directory"))?
            .metadata()?;
        if (directory.dev(), directory.ino()) != self.0.marker_directory {
            return Ok(false);
        }
        assert!(self.0.files.len() <= FILES_MAX);
        for (index, expected) in self.0.files.iter().enumerate() {
            if self.0.with_path(index, Fingerprint::read)? != *expected {
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(crate) fn validate_part_file(&self, index: usize, file: &File) -> Result<()> {
        let offset = 1 + usize::from(self.0.partitioned);
        self.validate_file(index.checked_add(offset), file)
    }

    pub(crate) fn validate_part_input(
        &self,
        index: usize,
        input: &super::AdmittedTable,
    ) -> Result<()> {
        let offset = 1 + usize::from(self.0.partitioned);
        let expected = index
            .checked_add(offset)
            .and_then(|index| self.0.files.get(index));
        if expected.is_none() || input.captured_file() != expected {
            return Err(Error::Bound("changed event source input"));
        }

        Ok(())
    }

    pub(crate) fn validate_index_file(&self, file: &File) -> Result<()> {
        if !self.0.partitioned {
            return Err(Error::Bound("event source has no index"));
        }
        self.validate_file(Some(1), file)
    }

    pub(crate) fn validate_marker_file(&self, file: &File) -> Result<()> {
        self.validate_file(Some(0), file)
    }

    fn validate_file(&self, index: Option<usize>, file: &File) -> Result<()> {
        let expected = index
            .and_then(|index| self.0.files.get(index))
            .ok_or(Error::Bound("event source file index"))?;
        crate::work::record(|counts| counts.source_metadata_checks += 1);
        if Fingerprint::from(&file.metadata()?) != *expected {
            return Err(Error::Bound("changed event source file"));
        }
        Ok(())
    }

    fn validate_commitment(&self, root: &Path, manifest: &Manifest) -> Result<()> {
        let mut file = private::open_private_file(self.marker(), false)?;
        self.validate_marker_file(&file)?;
        let actual = crate::store::read_committed_file(root, self.marker(), &mut file)?;
        self.validate_marker_file(&file)?;
        if actual != *manifest {
            return Err(Error::Bound("changed event query commitment"));
        }
        Ok(())
    }

    pub(super) fn marker(&self) -> &Path {
        &self.0.marker
    }

    pub(super) fn marker_fingerprint(&self) -> Fingerprint {
        self.0.files[0]
    }

    pub(super) fn marker_directory(&self) -> (u64, u64) {
        self.0.marker_directory
    }
}

impl EventSourceSnapshot {
    fn with_path<T>(&self, index: usize, read: impl FnOnce(&Path) -> Result<T>) -> Result<T> {
        if index == 0 {
            return read(&self.marker);
        }
        // Validation uses bounded stack paths, so cloned pins need no mutable scratch or lease.
        let mut bytes = [0; private::PATH_MAX];
        let mut remaining = &mut bytes[..];
        remaining.write_all(self.directory.as_os_str().as_bytes())?;
        if !self.partitioned {
            remaining.write_all(b"/events.parquet")?;
        } else if index == 1 {
            remaining.write_all(b"/events/index")?;
        } else {
            write!(remaining, "/events/{:04}.parquet", index - 2)?;
        }
        let count = private::PATH_MAX - remaining.len();
        read(Path::new(std::ffi::OsStr::from_bytes(&bytes[..count])))
    }
}

impl PartialEq for EventSourcePin {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
            || (self.0.marker == other.0.marker
                && self.0.directory == other.0.directory
                && self.0.marker_directory == other.0.marker_directory
                && self.0.partitioned == other.0.partitioned
                && self.0.files == other.0.files)
    }
}

impl Eq for EventSourcePin {}
