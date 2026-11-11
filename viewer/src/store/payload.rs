//! Typed process payloads and shared names retain their own admissions, including View owners.
use std::fmt;
use std::ops::{Deref, Index};
use std::sync::Arc;

#[path = "payload_names.rs"]
mod names;
#[cfg(test)]
pub(super) use names::NameTrace;
pub(super) use names::{NameAdditions, ProcessNames};

use super::Missing;
use crate::apps::{Amount, ProcessState, Stamp, Unavailable};
use crate::byte_budget::{ByteBudget, ByteReservation};
use crate::live_app_response::{Bytes, Measure, Observation, Process, Sample};
use collector::ring::{IdentityUnavailable, ProcessCreation};

/// A complete observation's processes and additional App measures.
///
/// App detail allocations cannot be detached from their byte reservation.
/// ```compile_fail
/// use viewer::store::{AppDetails, Payload};
/// fn escape(payload: &mut Payload) -> Vec<AppDetails> {
///     std::mem::take(&mut payload.apps)
/// }
/// ```
#[derive(Debug)]
pub struct Payload {
    pub(crate) selected: Observation,
    pub previous: Option<Stamp>,
    rows: Vec<Arc<SharedRow>>,
    apps: Vec<AppDetails>,
    reservation: ByteReservation,
}

/// One process row and its admission, held by every second whose reading of the process
/// equals it.
#[derive(Debug)]
struct SharedRow {
    row: ProcessRow,
    _reservation: ByteReservation,
}

/// The process rows of one payload in its sample's order, borrowed from their owners.
#[derive(Clone, Debug)]
pub struct Rows<'payload> {
    rows: std::slice::Iter<'payload, Arc<SharedRow>>,
}

/// Captured executable bytes and their display spelling, shared across observations.
///
/// Buffers cannot be detached from their reservation, even with an exclusive owner.
/// ```compile_fail
/// use viewer::store::ProcessName;
/// fn escape(name: &mut ProcessName) -> Vec<u8> {
///     std::mem::take(&mut name.bytes)
/// }
/// ```
pub struct ProcessName {
    name: Bytes,
    reservation: ByteReservation,
}

/// Every process of the sample, including unavailable identity and usage.
///
/// Executable ownership is shared privately; public access remains borrowed.
/// ```compile_fail
/// use std::sync::Arc;
/// use viewer::store::{ProcessName, ProcessRow};
/// fn escape(row: &ProcessRow) -> Option<Arc<ProcessName>> {
///     row.executable_name.clone()
/// }
/// ```
#[derive(Debug, PartialEq, Eq)]
pub struct ProcessRow {
    pub pid: i32,
    pub creation: Option<ProcessCreation>,
    pub app_index: Option<u32>,
    executable_name: Option<Arc<ProcessName>>,
    pub program_unavailable: Option<IdentityUnavailable>,
    cpu_ns: CompactAmount,
    energy_nj: CompactAmount,
    gpu_ns: CompactAmount,
    disk_read_bytes: CompactAmount,
    disk_written_bytes: CompactAmount,
    disk_io_bytes: CompactAmount,
    resident_bytes: CompactAmount,
    footprint_bytes: CompactAmount,
    idle_wakeups: CompactAmount,
    pageins: CompactAmount,
    threads: CompactAmount,
    pub app_attribution: Option<Unavailable>,
    pub state: ProcessState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CompactAmount {
    known: [u8; 8],
    unavailable: Option<Unavailable>,
}

/// Measures displayed in App detail, aligned with the observation's App rows.
#[derive(Debug, PartialEq, Eq)]
pub struct AppDetails {
    pub live_members: u32,
    pub disk_read_bytes: Measure,
    pub disk_written_bytes: Measure,
    pub footprint_bytes: Measure,
}

impl Payload {
    /// The charge of each distinct process row, however many seconds hold it.
    pub const ROW_BYTES: usize = SharedRow::BYTES;

    /// The charge of each process a second lists, whether its row is shared or the second's own.
    pub const ROW_REFERENCE_BYTES: usize = size_of::<Arc<SharedRow>>();

    /// Additional App measures, borrowed from their reservation owner.
    #[must_use]
    pub fn apps(&self) -> &[AppDetails] {
        &self.apps
    }

    /// The process rows in the sample's order.
    #[must_use]
    pub fn iter(&self) -> Rows<'_> {
        Rows {
            rows: self.rows.iter(),
        }
    }

    /// The count of process rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Captures `sample`, sharing each row that equals the row `previous`, the payload of the
    /// second before, holds for the same process.
    pub(super) fn capture_with_names(
        sample: &Sample,
        names: &ProcessNames,
        previous: Option<&Self>,
    ) -> Result<(Self, NameAdditions), Missing> {
        if let Some(previous) = previous {
            assert!(previous.reservation.charged_to(names.budget()));
        }

        let reservation = names
            .budget()
            .reserve(Self::capture_bytes(sample))
            .ok_or(Missing::Budget)?;
        #[cfg(test)]
        let before = names.work_snapshot();
        let previous_rows = previous.map_or(&[][..], |payload| &payload.rows);
        let (additions, rows) = match Self::capture_with_names_parts(sample, names, previous_rows) {
            Ok(parts) => parts,
            Err(missing) => {
                #[cfg(test)]
                names.record_receipt(sample.selected, before, false);
                return Err(missing);
            }
        };

        let apps = sample
            .apps()
            .iter()
            .map(|app| AppDetails {
                live_members: app.live_members,
                disk_read_bytes: app.disk_read_bytes,
                disk_written_bytes: app.disk_written_bytes,
                footprint_bytes: app.footprint_bytes,
            })
            .collect();

        Ok((
            Self {
                selected: sample.selected,
                previous: sample.previous,
                rows,
                apps,
                reservation,
            },
            additions,
        ))
    }

    fn capture_with_names_parts(
        sample: &Sample,
        names: &ProcessNames,
        previous_rows: &[Arc<SharedRow>],
    ) -> Result<(NameAdditions, Vec<Arc<SharedRow>>), Missing> {
        let additions = names.additions(sample)?;
        let rows = Self::capture_with_names_rows(sample, names, &additions, previous_rows)?;

        Ok((additions, rows))
    }

    /// The row of each process of `sample`, which is the row of `previous_rows` where that
    /// second read the same. One pass over `previous_rows` finds each process while both lists
    /// ascend by pid, and a list in another order shares fewer rows and reads the same.
    fn capture_with_names_rows(
        sample: &Sample,
        names: &ProcessNames,
        additions: &NameAdditions,
        previous_rows: &[Arc<SharedRow>],
    ) -> Result<Vec<Arc<SharedRow>>, Missing> {
        let mut rows = Vec::with_capacity(sample.processes().len());
        let mut previous_index = 0;
        for process in sample.processes() {
            let name = process
                .executable_name
                .as_ref()
                .map(|name| names.name(name, additions));
            let row = ProcessRow::capture(process, name);
            previous_index += previous_rows[previous_index..]
                .iter()
                .take_while(|shared| shared.row.pid < row.pid)
                .count();
            let shared = match previous_rows.get(previous_index) {
                Some(shared) if shared.row == row => Arc::clone(shared),
                _ => SharedRow::admit(row, names.budget())?,
            };
            rows.push(shared);
        }

        Ok(rows)
    }

    /// Names and rows own separate reservations; this admission precedes either container
    /// allocation.
    fn capture_bytes(sample: &Sample) -> usize {
        2 * size_of::<usize>()
            + size_of::<Self>()
            + sample.processes().len() * Self::ROW_REFERENCE_BYTES
            + sample.apps().len() * size_of::<AppDetails>()
    }

    /// This payload's header and containers, excluding separately admitted shared names and
    /// rows.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.reservation.bytes()
    }
}

impl SharedRow {
    /// One allocation's charge: its two reference counts, the row and this reservation.
    const BYTES: usize = 2 * size_of::<usize>() + size_of::<Self>();

    fn admit(row: ProcessRow, budget: &Arc<ByteBudget>) -> Result<Arc<Self>, Missing> {
        let reservation = budget.reserve(Self::BYTES).ok_or(Missing::Budget)?;

        Ok(Arc::new(Self {
            row,
            _reservation: reservation,
        }))
    }
}

impl<'payload> Iterator for Rows<'payload> {
    type Item = &'payload ProcessRow;

    fn next(&mut self) -> Option<Self::Item> {
        self.rows.next().map(|shared| &shared.row)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.rows.size_hint()
    }
}

impl<'payload> IntoIterator for &'payload Payload {
    type Item = &'payload ProcessRow;
    type IntoIter = Rows<'payload>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl Index<usize> for Payload {
    type Output = ProcessRow;

    fn index(&self, index: usize) -> &Self::Output {
        &self.rows[index].row
    }
}

impl ProcessRow {
    pub(crate) fn presentation(&self) -> crate::live_app_response::json::ProcessView<'_> {
        crate::live_app_response::json::ProcessView {
            pid: self.pid,
            creation: self.creation,
            app_index: self.app_index,
            executable_name: self
                .executable_name()
                .map(|name| (name.bytes.as_slice(), name.display.as_str())),
            program_unavailable: self.program_unavailable,
            cpu_ns: self.cpu_ns(),
            energy_nj: self.energy_nj(),
            gpu_ns: self.gpu_ns(),
            disk_read_bytes: self.disk_read_bytes(),
            disk_written_bytes: self.disk_written_bytes(),
            disk_io_bytes: self.disk_io_bytes(),
            resident_bytes: self.resident_bytes(),
            footprint_bytes: self.footprint_bytes(),
            idle_wakeups: self.idle_wakeups(),
            pageins: self.pageins(),
            threads: self.threads(),
            app_attribution: self.app_attribution,
            state: self.state,
        }
    }

    #[must_use]
    pub fn cpu_ns(&self) -> Amount {
        self.cpu_ns.amount()
    }

    #[must_use]
    pub fn energy_nj(&self) -> Amount {
        self.energy_nj.amount()
    }

    #[must_use]
    pub fn gpu_ns(&self) -> Amount {
        self.gpu_ns.amount()
    }

    #[must_use]
    pub fn disk_read_bytes(&self) -> Amount {
        self.disk_read_bytes.amount()
    }

    #[must_use]
    pub fn disk_written_bytes(&self) -> Amount {
        self.disk_written_bytes.amount()
    }

    #[must_use]
    pub fn disk_io_bytes(&self) -> Amount {
        self.disk_io_bytes.amount()
    }

    #[must_use]
    pub fn resident_bytes(&self) -> Amount {
        self.resident_bytes.amount()
    }

    #[must_use]
    pub fn footprint_bytes(&self) -> Amount {
        self.footprint_bytes.amount()
    }

    #[must_use]
    pub fn idle_wakeups(&self) -> Amount {
        self.idle_wakeups.amount()
    }

    #[must_use]
    pub fn pageins(&self) -> Amount {
        self.pageins.amount()
    }

    #[must_use]
    pub fn threads(&self) -> Amount {
        self.threads.amount()
    }

    /// The name remains borrowed from its shared reservation owner.
    /// ```compile_fail
    /// use viewer::store::{ProcessName, ProcessRow};
    /// fn escape(row: &ProcessRow) -> Option<&'static ProcessName> {
    ///     row.executable_name()
    /// }
    /// ```
    #[must_use]
    pub fn executable_name(&self) -> Option<&ProcessName> {
        self.executable_name.as_deref()
    }

    fn capture(process: &Process, executable_name: Option<Arc<ProcessName>>) -> Self {
        Self {
            pid: process.pid,
            creation: process.creation,
            app_index: process.app_index,
            executable_name,
            program_unavailable: process.program_unavailable,
            cpu_ns: CompactAmount::capture(process.cpu_ns),
            energy_nj: CompactAmount::capture(process.energy_nj),
            gpu_ns: CompactAmount::capture(process.gpu_ns),
            disk_read_bytes: CompactAmount::capture(process.disk_read_bytes),
            disk_written_bytes: CompactAmount::capture(process.disk_written_bytes),
            disk_io_bytes: CompactAmount::capture(process.disk_io_bytes),
            resident_bytes: CompactAmount::capture(process.resident_bytes),
            footprint_bytes: CompactAmount::capture(process.footprint_bytes),
            idle_wakeups: CompactAmount::capture(process.idle_wakeups),
            pageins: CompactAmount::capture(process.pageins),
            threads: CompactAmount::capture(process.threads),
            app_attribution: process.app_attribution,
            state: process.state,
        }
    }
}

impl ProcessName {
    const OWNER_BYTES: usize = 2 * size_of::<usize>() + size_of::<Self>();

    /// Adopts buffers allocated under this reservation, including their spare capacity.
    /// The caller reserves before allocating the buffers and the enclosing Arc.
    #[must_use]
    pub fn from_reserved(name: Bytes, mut reservation: ByteReservation) -> Self {
        let Some(bytes) = Self::OWNER_BYTES
            .checked_add(name.bytes.capacity())
            .and_then(|bytes| bytes.checked_add(name.display.capacity()))
        else {
            panic!("process name capacity exceeds address space");
        };

        assert!(bytes <= reservation.bytes());
        reservation.shrink_to(bytes);

        Self { name, reservation }
    }

    fn capture(name: &Bytes, budget: &Arc<ByteBudget>) -> Result<Arc<Self>, Missing> {
        let bytes = Self::OWNER_BYTES
            .checked_add(name.bytes.len())
            .and_then(|bytes| bytes.checked_add(name.display.len()))
            .ok_or(Missing::Budget)?;
        let reservation = budget.reserve(bytes).ok_or(Missing::Budget)?;

        Ok(Arc::new(Self::from_reserved(name.clone(), reservation)))
    }

    /// This shared allocation's charge, counted once regardless of its retaining payloads.
    #[must_use]
    pub fn reserved_bytes(&self) -> usize {
        self.reservation.bytes()
    }
}

impl Deref for ProcessName {
    type Target = Bytes;

    fn deref(&self) -> &Self::Target {
        &self.name
    }
}

impl fmt::Debug for ProcessName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessName")
            .field("bytes", &self.bytes)
            .field("display", &self.display)
            .finish()
    }
}

impl PartialEq for ProcessName {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes && self.display == other.display
    }
}

impl Eq for ProcessName {}

impl CompactAmount {
    fn capture(amount: Amount) -> Self {
        match amount {
            Amount::Known(value) => Self {
                known: value.to_le_bytes(),
                unavailable: None,
            },
            Amount::Unavailable(reason) => Self {
                known: [0; 8],
                unavailable: Some(reason),
            },
        }
    }

    fn amount(self) -> Amount {
        match self.unavailable {
            Some(reason) => Amount::Unavailable(reason),
            None => Amount::Known(u64::from_le_bytes(self.known)),
        }
    }
}

impl PartialEq for Payload {
    fn eq(&self, other: &Self) -> bool {
        self.selected == other.selected
            && self.previous == other.previous
            && self.iter().eq(other.iter())
            && self.apps == other.apps
    }
}

const _: () = {
    assert!(size_of::<ProcessRow>() <= 152);
    assert!(size_of::<SharedRow>() <= 168);
    assert!(size_of::<CompactAmount>() <= 9);
    assert!(align_of::<CompactAmount>() == 1);
    assert!(size_of::<AppDetails>() <= 80);
};

#[cfg(test)]
#[path = "payload_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "payload_name_tests.rs"]
mod name_tests;

#[cfg(test)]
#[path = "payload_namespace_tests.rs"]
mod namespace_tests;

#[cfg(test)]
#[path = "payload_row_tests.rs"]
mod row_tests;
