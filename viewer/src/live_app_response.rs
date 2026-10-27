//! Owned and bounded live App presentation, without current-process enrichment.
use collector::ring::IdentityUnavailable;
use persistence::{Error, Result};
use serde_json::Value;

const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;

use crate::apps::{self, Amount, AppKey, ProcessCreation, ProcessState, Stamp, Unavailable};
use crate::live_apps::Frame;

pub(crate) use json::{Header, bounded};
pub use part::AppPart;
pub(crate) use part::Present;
pub(crate) use ranks::{METRICS, Rankable, Ranked, rank_apps};
pub use ranks::{Metric, RANKS_MAX, Rank, RankedApp, Ranking, Ranks};

pub const SAMPLE_BYTES_MAX: usize = 8 * 1024 * 1024;
pub const RESPONSE_BYTES_MAX: usize = 8 * 1024 * 1024;
pub const PAGE_ROWS_MAX: u32 = 512;
pub const APPS_MAX: usize = apps::PROCESS_INTERVALS_MAX;
pub const MEMBERS_MAX: usize = apps::PROCESS_INTERVALS_MAX;

pub struct Sample {
    pub selected: Observation,
    pub previous: Option<Stamp>,
    pub elapsed_ns: Option<u64>,
    pub membership_complete: bool,
    pub ungrouped_members: u32,
    /// The ungrouped members live at the selected tick, which an exited process does not join.
    pub live_ungrouped_members: u32,
    pub usage_refused: u32,
    pub usage_gone: u32,
    apps: Vec<App>,
    processes: Vec<Process>,
}

pub use crate::live_apps::Observation;

/// The selected observation and the last second of the context read with it, which every
/// history of the selection spans: the 900 seconds that end at that second, and the lead-in
/// before them that its charts draw from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Selected {
    observation: Observation,
    context_last_second: u64,
}

impl Selected {
    /// The context of `observation` that ends at `context_last_second`, which lies no earlier
    /// than the observation's second and no more than `CONTEXT_AFTER_SECONDS` after it.
    pub(crate) fn new(observation: Observation, context_last_second: u64) -> Result<Self> {
        let wall_ns = observation
            .wall_ns
            .ok_or(Error::Bound("selected context clock"))?;
        let second = wall_ns / NANOSECONDS_PER_SECOND;
        if context_last_second < second {
            return Err(Error::Bound("selected context end"));
        }
        if context_last_second - second > crate::session::CONTEXT_AFTER_SECONDS {
            return Err(Error::Bound("selected context end"));
        }

        Ok(Self {
            observation,
            context_last_second,
        })
    }

    /// The context that ends at the selected second, as a following page's does.
    #[cfg(test)]
    pub(crate) fn ending_at(observation: Observation) -> Result<Self> {
        let wall_ns = observation
            .wall_ns
            .ok_or(Error::Bound("selected context clock"))?;

        Self::new(observation, wall_ns / NANOSECONDS_PER_SECOND)
    }

    /// The selected observation.
    pub(crate) fn observation(&self) -> &Observation {
        &self.observation
    }

    /// The last second the context holds.
    pub(crate) fn context_last_second(&self) -> u64 {
        self.context_last_second
    }

    /// The first second the context holds, `LEAD_IN_SECONDS` before the 900 seconds, so the
    /// first points of an App chart average whole windows as the scalar charts' do.
    pub(crate) fn first_second(&self) -> u64 {
        self.context_last_second
            .saturating_sub(crate::session::CHARTED_SECONDS - 1)
    }

    /// Whether the context holds `second`.
    pub(crate) fn holds(&self, second: u64) -> bool {
        (self.first_second()..=self.context_last_second).contains(&second)
    }
}

#[derive(Clone)]
pub struct App {
    pub key: Key,
    pub display_name: String,
    pub members: u32,
    pub live_members: u32,
    pub membership_complete: bool,
    pub cpu_ns: Measure,
    pub energy_nj: Measure,
    pub gpu_ns: Measure,
    /// Exact attributed bytes during the selected observation interval.
    pub disk_read_bytes: Measure,
    pub disk_written_bytes: Measure,
    pub disk_io_bytes: Measure,
    pub resident_bytes: Measure,
    pub footprint_bytes: Measure,
}

#[derive(Clone)]
pub struct Key {
    pub kind: KeyKind,
    pub bytes: Vec<u8>,
    pub display: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    Bundle,
    Executable,
}

impl KeyKind {
    /// The key of this kind whose captured bytes are `bytes`, refused as `Key::new` refuses
    /// them, without copying the bytes.
    pub(crate) fn checked_key(self, bytes: &[u8]) -> Result<AppKey<'_>> {
        capture::validate_bytes(bytes)?;

        Ok(self.key(bytes))
    }

    pub(crate) fn key(self, bytes: &[u8]) -> AppKey<'_> {
        match self {
            KeyKind::Bundle => AppKey::Bundle(bytes),
            KeyKind::Executable => AppKey::Executable(bytes),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measure {
    pub known: u64,
    pub measured_members: u32,
    pub unavailable_members: u32,
    pub overflowed: bool,
}

#[derive(Clone)]
pub struct Process {
    pub pid: i32,
    pub creation: Option<ProcessCreation>,
    /// Scoped to this sample's exact selected observation, never stable across samples.
    pub app_index: Option<u32>,
    pub executable_name: Option<Bytes>,
    pub program_unavailable: Option<IdentityUnavailable>,
    pub cpu_ns: Amount,
    pub energy_nj: Amount,
    pub gpu_ns: Amount,
    /// Exact process interval bytes, distinct from the raw cumulative counters.
    pub disk_read_bytes: Amount,
    pub disk_written_bytes: Amount,
    pub disk_io_bytes: Amount,
    pub app_attribution: Option<Unavailable>,
    pub resident_bytes: Amount,
    pub footprint_bytes: Amount,
    pub idle_wakeups: Amount,
    pub pageins: Amount,
    pub threads: Amount,
    pub state: ProcessState,
}

#[derive(Clone)]
pub struct Bytes {
    pub bytes: Vec<u8>,
    pub display: String,
}

#[derive(Clone, Copy)]
pub struct Page {
    pub offset: u32,
    pub limit: u32,
}

pub struct Detail<'sample> {
    pub selected: &'sample Observation,
    pub app_index: u32,
    pub page: Page,
}

impl Observation {
    #[must_use]
    pub fn json(&self) -> Value {
        json::observation(self)
    }
}

impl App {
    /// The capacity of the strings and vectors the App row holds.
    fn heap_bytes(&self) -> usize {
        self.key.bytes.capacity() + self.key.display.capacity() + self.display_name.capacity()
    }
}

impl Key {
    /// The key of the App with these bytes, displayed as its sample displays it.
    pub(crate) fn new(kind: KeyKind, bytes: Vec<u8>) -> Result<Self> {
        let display = capture::display(&bytes)?;

        Ok(Self {
            kind,
            bytes,
            display,
        })
    }

    /// The key as an aggregate of the App's processes holds it.
    pub(crate) fn view(&self) -> AppKey<'_> {
        self.kind.key(&self.bytes)
    }

    /// The name the App of this key shows.
    pub(crate) fn display_name(&self) -> Result<String> {
        capture::display_name(self.kind, &self.bytes)
    }
}

impl Bytes {
    /// These bytes, displayed as a sample displays them.
    pub(crate) fn new(bytes: Vec<u8>) -> Result<Self> {
        let display = capture::display(&bytes)?;

        Ok(Self { bytes, display })
    }
}

impl Process {
    /// The capacity of the strings and vectors the process row holds.
    fn heap_bytes(&self) -> usize {
        let Some(name) = &self.executable_name else {
            return 0;
        };

        name.bytes.capacity() + name.display.capacity()
    }
}

impl Measure {
    #[must_use]
    pub fn json(&self) -> Value {
        json::measure(self)
    }

    #[must_use]
    pub fn measured(&self) -> Option<u64> {
        if self.measured_members == 0 {
            return None;
        }
        if self.overflowed {
            return None;
        }

        Some(self.known)
    }
}

const _: () = {
    assert!(PAGE_ROWS_MAX > 0);
    assert!(SAMPLE_BYTES_MAX < apps::PROGRAM_BYTES_MAX);
    assert!(APPS_MAX == MEMBERS_MAX);
    assert!(size_of::<Measure>() <= 24);
    assert!(size_of::<App>() <= 288);
    assert!(size_of::<Process>() <= 264);
};

impl Sample {
    /// Applies capture's aggregate and presentation admission without retaining a presentation.
    /// A rate predecessor needs its accepted raw frame, rather than copies of the App rows.
    pub(crate) fn validate_capture(selected: &Frame, previous: Option<&Frame>) -> Result<()> {
        selected.with_aggregate(previous, capture::validate)
    }

    /// Copies every borrowed fact before the aggregate callback returns.
    /// Refusal clears output rather than retaining an older successful observation.
    pub fn capture(
        selected: &Frame,
        previous: Option<&Frame>,
        output: &mut Option<Box<Self>>,
    ) -> Result<()> {
        *output = None;

        let observation = selected.observation();

        selected.with_aggregate(previous, |aggregate| {
            Self::from_aggregate(
                &observation,
                previous.map(|frame| *frame.stamp()),
                aggregate,
                output,
            )
        })?;

        let sample = output
            .as_mut()
            .ok_or(Error::Bound("live App empty sample"))?;

        sample.usage_refused = selected.usage_refused;
        sample.usage_gone = selected.usage_gone;

        Ok(())
    }

    fn from_aggregate(
        selected: &Observation,
        previous: Option<Stamp>,
        aggregate: &apps::Aggregate<'_>,
        output: &mut Option<Box<Self>>,
    ) -> Result<()> {
        *output = None;
        capture::validate(aggregate)?;

        let mut sample = Box::new(Self {
            selected: *selected,
            previous,
            elapsed_ns: aggregate.elapsed_ns,
            membership_complete: aggregate.membership_complete,
            ungrouped_members: aggregate.ungrouped_members,
            live_ungrouped_members: aggregate.live_ungrouped_members,
            usage_refused: 0,
            usage_gone: 0,
            apps: Vec::with_capacity(aggregate.apps.len()),
            processes: Vec::with_capacity(aggregate.processes.len()),
        });

        capture::apps(aggregate, &mut sample.apps)?;
        capture::processes(aggregate, &sample.apps, &mut sample.processes)?;

        if sample.bytes() > SAMPLE_BYTES_MAX {
            return Err(Error::Bound("live App sample bytes"));
        }

        assert!(sample.apps.len() <= APPS_MAX);
        assert!(sample.processes.len() <= MEMBERS_MAX);
        *output = Some(sample);

        Ok(())
    }

    /// Includes every retained vector/string capacity, plus the boxed sample itself.
    #[must_use]
    pub fn bytes(&self) -> usize {
        let mut bytes = size_of::<Self>()
            + self.apps.capacity() * size_of::<App>()
            + self.processes.capacity() * size_of::<Process>();
        for app in &self.apps {
            bytes += app.heap_bytes();
        }
        for process in &self.processes {
            bytes += process.heap_bytes();
        }

        bytes
    }

    #[must_use]
    pub fn apps(&self) -> &[App] {
        &self.apps
    }

    #[must_use]
    pub fn processes(&self) -> &[Process] {
        &self.processes
    }

    pub fn apps_page(&self, page: Page) -> Result<Value> {
        json::apps_page(self, page)
    }

    pub fn app_detail(&self, detail: &Detail<'_>) -> Result<Value> {
        if detail.selected != &self.selected {
            return Err(Error::Bound("live App stale detail"));
        }

        json::app_detail(self, detail)
    }

    pub fn ungrouped_page(&self, page: Page) -> Result<Value> {
        json::members_page(self, None, page)
    }
}

#[cfg(test)]
mod boundary_tests;
mod capture;
#[cfg(test)]
mod capture_tests;
#[cfg(test)]
mod disk_history_tests;
mod json;
mod part;
#[cfg(test)]
mod part_tests;
mod ranks;
#[cfg(test)]
mod ranks_tests;
#[cfg(test)]
mod tests;
