//! Typed process payloads and shared names retain their own admissions, including View owners.
use std::fmt;
use std::ops::Deref;
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
    rows: Vec<ProcessRow>,
    apps: Vec<AppDetails>,
    reservation: ByteReservation,
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
    /// Additional App measures, borrowed from their reservation owner.
    #[must_use]
    pub fn apps(&self) -> &[AppDetails] {
        &self.apps
    }

    pub(super) fn capture(sample: &Sample, budget: &Arc<ByteBudget>) -> Result<Self, Missing> {
        let names = ProcessNames::new(sample.processes().len(), budget)?;
        let (payload, additions) = Self::capture_with_names(sample, &names)?;
        drop(additions);

        Ok(payload)
    }

    pub(super) fn capture_with_names(
        sample: &Sample,
        names: &ProcessNames,
    ) -> Result<(Self, NameAdditions), Missing> {
        let reservation = names
            .budget()
            .reserve(Self::capture_bytes(sample))
            .ok_or(Missing::Budget)?;
        #[cfg(test)]
        let before = names.work_snapshot();
        let additions = match names.additions(sample) {
            Ok(additions) => additions,
            Err(missing) => {
                #[cfg(test)]
                names.record_receipt(sample.selected, before, false);
                return Err(missing);
            }
        };
        let mut rows = Vec::with_capacity(sample.processes().len());
        for process in sample.processes() {
            let name = process
                .executable_name
                .as_ref()
                .map(|name| names.name(name, &additions));
            rows.push(ProcessRow::capture(process, name));
        }

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

    /// Names own separate reservations; this admission precedes either container allocation.
    fn capture_bytes(sample: &Sample) -> usize {
        2 * size_of::<usize>()
            + size_of::<Self>()
            + sample.processes().len() * size_of::<ProcessRow>()
            + sample.apps().len() * size_of::<AppDetails>()
    }

    /// This payload's header and containers, excluding separately admitted shared names.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.reservation.bytes()
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

impl Deref for Payload {
    type Target = [ProcessRow];

    fn deref(&self) -> &Self::Target {
        &self.rows
    }
}

impl PartialEq for Payload {
    fn eq(&self, other: &Self) -> bool {
        self.selected == other.selected
            && self.previous == other.previous
            && self.rows == other.rows
            && self.apps == other.apps
    }
}

const _: () = {
    assert!(size_of::<ProcessRow>() <= 152);
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
