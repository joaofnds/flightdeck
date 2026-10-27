//! Typed process payloads retained under one shared byte reservation, including View owners.
use std::ops::Deref;
use std::sync::Arc;

use super::{AppsSecond, Missing};
use crate::apps::{Amount, ProcessState, Stamp, Unavailable};
use crate::byte_budget::{ByteBudget, ByteReservation};
use crate::live_app_response::{Measure, Observation, Process, Sample};
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
#[derive(Debug, PartialEq, Eq)]
pub struct ProcessName {
    pub bytes: Vec<u8>,
    pub display: String,
}

/// Every process of the sample, including unavailable identity and usage.
///
/// Executable storage cannot escape the payload's byte reservation.
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

    pub(super) fn capture(
        sample: &Sample,
        previous: Option<&AppsSecond>,
        budget: &Arc<ByteBudget>,
    ) -> Result<Self, Missing> {
        let reservation = budget
            .reserve(Self::capture_bytes(sample))
            .ok_or(Missing::Budget)?;
        let mut rows = Vec::with_capacity(sample.processes().len());
        let previous = previous
            .and_then(|apps| apps.processes.as_ref().ok())
            .map(Arc::as_ref);
        for process in sample.processes() {
            rows.push(ProcessRow::capture(process, previous));
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
        Ok(Self {
            selected: sample.selected,
            previous: sample.previous,
            rows,
            apps,
            reservation,
        })
    }

    /// A conservative charge counts shared names once for each retaining payload.
    /// The reservation is acquired before copying any rows or name bytes.
    fn capture_bytes(sample: &Sample) -> usize {
        let mut bytes = 2 * size_of::<usize>()
            + size_of::<Self>()
            + sample.processes().len() * size_of::<ProcessRow>()
            + sample.apps().len() * size_of::<AppDetails>();
        for process in sample.processes() {
            if let Some(name) = &process.executable_name {
                bytes += 2 * size_of::<usize>()
                    + size_of::<ProcessName>()
                    + name.bytes.len()
                    + name.display.len();
            }
        }
        bytes
    }

    /// Reserved bytes, including conservative charges for shared names.
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

    /// The name remains borrowed from the payload that owns its byte reservation.
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

    fn capture(process: &Process, previous: Option<&Payload>) -> Self {
        let executable_name = process.executable_name.as_ref().map(|name| {
            let held = previous.and_then(|previous| {
                previous.rows.iter().find_map(|row| {
                    let held = row.executable_name.as_ref()?;
                    (held.bytes == name.bytes).then(|| Arc::clone(held))
                })
            });
            held.unwrap_or_else(|| {
                Arc::new(ProcessName {
                    bytes: name.bytes.clone(),
                    display: name.display.clone(),
                })
            })
        });
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
