//! One logical event table, stored as bounded parts in one window commit.
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use polars::prelude::DataFrame;

use crate::engine::{AdmissionRefusal, AdmittedReadRefusal, EventSourcePin};
use crate::resources::{ByteBudget, ByteReservation};
use crate::{Error, Manifest, Result};

pub(crate) mod index;
pub(crate) mod query;
mod read;
mod workspace;
pub(crate) mod writer;

pub struct EventTable {
    directory: PathBuf,
    rows: u64,
    index: index::Index,
    budget: Arc<ByteBudget>,
    pin: EventSourcePin,
    _reservation: ByteReservation,
}

pub struct EventPart {
    path: PathBuf,
    rows: u64,
    events: Range<u64>,
    identities: bool,
    pin: EventSourcePin,
    index: usize,
    _reservation: ByteReservation,
}

pub struct EventFrame {
    rows: DataFrame,
    reservation: ByteReservation,
}

/// One typed operation's immutable admitted bytes, retained across bounded physical slices.
pub struct CapturedEventPart {
    input: crate::engine::AdmittedTable,
    part: EventPart,
}

impl EventTable {
    /// Convenience for one independent caller; shared loaders use `open_budgeted`.
    pub fn open(root: &Path, manifest: &Manifest) -> Result<Self> {
        Self::open_budgeted(
            root,
            manifest,
            &Arc::new(ByteBudget::new(
                index::WORKING_BYTES + 4096 + EventSourcePin::CAPTURE_BYTES_MAX,
            )),
        )
        .map_err(read::ordinary_error)
    }

    pub fn open_budgeted(
        root: &Path,
        manifest: &Manifest,
        budget: &Arc<ByteBudget>,
    ) -> std::result::Result<Self, AdmittedReadRefusal> {
        let pin = EventSourcePin::capture(root, manifest, budget)?;
        Self::open_budgeted_pinned(root, manifest, budget, &pin)
    }

    pub(crate) fn open_budgeted_pinned(
        root: &Path,
        manifest: &Manifest,
        budget: &Arc<ByteBudget>,
        pin: &EventSourcePin,
    ) -> std::result::Result<Self, AdmittedReadRefusal> {
        let reservation = budget
            .reserve(index::WORKING_BYTES)
            .ok_or(AdmittedReadRefusal::Admission(AdmissionRefusal::Budget))?;
        let marker = crate::store::commitment_path(root, manifest);
        let mut file = collector::recording::private::open_private_file(&marker, false)?;
        pin.validate_marker_file(&file)?;
        let committed = crate::store::read_committed_file(root, &marker, &mut file)?;
        pin.validate_marker_file(&file)?;
        if committed != *manifest {
            return Err(Error::Bound("changed event table commitment").into());
        }
        let directory = root.join(&manifest.data_directory);
        let rows = manifest
            .tables
            .iter()
            .find(|table| table.name == "events")
            .ok_or(Error::Bound("missing event table"))?
            .rows;
        let mut file = collector::recording::private::open_private_file(
            &directory.join("events/index"),
            false,
        )?;
        pin.validate_index_file(&file)?;
        let index = index::Index::read_file(&mut file, manifest)?;
        pin.validate_index_file(&file)?;
        if !pin.validate()? {
            return Err(Error::Bound("changed event source identity").into());
        }
        Ok(Self {
            directory,
            rows,
            index,
            budget: Arc::clone(budget),
            pin: pin.clone(),
            _reservation: reservation,
        })
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.index.batches.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn part(&self, index: usize) -> std::result::Result<EventPart, AdmittedReadRefusal> {
        if index >= self.len() {
            return Err(Error::Bound("event part index").into());
        }
        let reservation = self
            .budget
            .reserve(4096)
            .ok_or(AdmittedReadRefusal::Admission(AdmissionRefusal::Budget))?;
        let batch = &self.index.batches[index];

        Ok(EventPart {
            path: self
                .directory
                .join("events")
                .join(crate::event_recovery::batch_name(index)),
            rows: batch.rows,
            events: batch.first..batch.end,
            identities: self.index.identities,
            pin: self.pin.clone(),
            index,
            _reservation: reservation,
        })
    }
}

impl EventPart {
    /// Calls the existing concrete-file capture boundary once and verifies its returned owner.
    pub fn capture_with(
        self,
        budget: &Arc<ByteBudget>,
        capture: impl FnOnce(
            std::fs::File,
            &Arc<ByteBudget>,
        )
            -> std::result::Result<crate::engine::AdmittedTable, AdmissionRefusal>,
    ) -> std::result::Result<CapturedEventPart, AdmittedReadRefusal> {
        read::capture(self, budget, capture)
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
    #[must_use]
    pub fn rows(&self) -> u64 {
        self.rows
    }
    #[must_use]
    pub fn events(&self) -> Range<u64> {
        self.events.clone()
    }

    /// Decodes/validates at most one physical batch, preserving every detached row.
    pub fn read_slice(
        &self,
        offset: u64,
        limit: u32,
        budget: &Arc<ByteBudget>,
    ) -> std::result::Result<EventFrame, AdmittedReadRefusal> {
        read::slice(self, offset, limit, budget)
    }

    pub(crate) fn read_slice_pinned(
        &self,
        offset: u64,
        limit: u32,
        budget: &Arc<ByteBudget>,
        pin: &EventSourcePin,
        index: usize,
    ) -> std::result::Result<EventFrame, AdmittedReadRefusal> {
        if self.pin != *pin || self.index != index {
            return Err(Error::Bound("event part source identity").into());
        }
        self.read_slice(offset, limit, budget)
    }
}

impl CapturedEventPart {
    #[must_use]
    pub fn rows(&self) -> u64 {
        self.part.rows
    }

    /// Reads only the captured bytes; later path changes cannot substitute another input.
    pub fn read_slice(
        &self,
        offset: u64,
        count: u32,
        budget: &Arc<ByteBudget>,
    ) -> std::result::Result<EventFrame, AdmittedReadRefusal> {
        read::captured_slice(&self.part, &self.input, offset, count, budget)
    }
}

impl EventFrame {
    /// Consumes one validated part, retaining only compact associated rows with buffer leases.
    pub fn associated(
        self,
        budget: &Arc<ByteBudget>,
    ) -> std::result::Result<DataFrame, AdmittedReadRefusal> {
        assert!(self.reservation.bytes() >= workspace::association(self.rows.height())?);
        let retained = crate::event_observation::associated_rows(&self.rows)?;
        crate::engine::frame_lease::copy_event_frame(&retained, budget)
    }

    #[must_use]
    pub fn rows(&self) -> &DataFrame {
        &self.rows
    }
    pub fn into_parts(self) -> (DataFrame, ByteReservation) {
        (self.rows, self.reservation)
    }
}
