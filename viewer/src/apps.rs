//! Bounded, byte-preserving App totals shared by saved and live presentation adapters.
//! Callers join recorded observations and supply coverage; no current-process lookup occurs.
use std::collections::{BTreeMap, BTreeSet};

use collector::processes::Usage;
pub use collector::ring::{IdentityUnavailable, ProcessCreation, ProgramBytes};

/// Numerator and continuous-time denominator for one-core CPU utilization.
#[derive(Clone, Copy)]
pub struct CpuRate {
    pub cpu_ns: u64,
    pub elapsed_ns: u64,
}

/// Energy delta and continuous-time denominator for average power.
#[derive(Clone, Copy)]
pub struct EnergyRate {
    pub energy_nj: u64,
    pub elapsed_ns: u64,
}

/// GPU time delta and continuous-time denominator for the share of the interval the GPU spent.
#[derive(Clone, Copy)]
pub struct GpuRate {
    pub gpu_ns: u64,
    pub elapsed_ns: u64,
}

/// Exact interval bytes and its continuous-time denominator for disk throughput.
#[derive(Clone, Copy)]
pub struct BytesRate {
    pub bytes: u64,
    pub elapsed_ns: u64,
}

#[derive(Clone, Copy)]
pub struct EventRate {
    pub events: u64,
    pub elapsed_ns: u64,
}

struct Endpoints<T> {
    previous: T,
    selected: T,
}

pub type CapturedProgram<'bytes> = Result<&'bytes ProgramBytes<'bytes>, IdentityUnavailable>;

/// Viewer operation bounds, not collector event-retention capacities.
pub const PROCESS_INTERVALS_MAX: usize = 24_000;
pub const PROGRAM_BYTES_MAX: usize = 64 * 1024 * 1024;

const _: () = {
    assert!(PROCESS_INTERVALS_MAX < 65_536);
    assert!(PROGRAM_BYTES_MAX >= collector::ring::PROGRAM_PATH_BYTES_MAX);
    assert!(size_of::<Amount>() <= 16);
    assert!(size_of::<CpuRate>() <= 16);
    assert!(size_of::<EnergyRate>() <= 16);
    assert!(size_of::<BytesRate>() <= 16);
    assert!(size_of::<EventRate>() <= 16);
    assert!(size_of::<Subtotal>() <= 24);
    assert!(size_of::<AppTotals>() <= 280);
    assert!(size_of::<ProcessTotals<'_>>() <= 248);
    assert!(size_of::<ProcessCreation>() <= 16);
};

pub struct Aggregate<'bytes> {
    pub apps: BTreeMap<AppKey<'bytes>, AppTotals>,
    pub processes: Vec<ProcessTotals<'bytes>>,
    pub ungrouped_members: u32,
    /// The ungrouped members live at the selected tick, which an exited process does not join.
    pub live_ungrouped_members: u32,
    /// Completeness of membership only; metric availability is carried by each subtotal.
    pub membership_complete: bool,
    pub elapsed_ns: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum AppKey<'bytes> {
    Bundle(&'bytes [u8]),
    Executable(&'bytes [u8]),
}

/// Searches `ordered`, indexes whose keys ascend, as `binary_search_by` does. The Apps of one
/// reading arrive in key order, so the slot after the last match usually holds the next App,
/// and trying `next` first spares the search, whose key comparisons dominate a history.
pub(crate) fn search_ordered<'key>(
    ordered: &[u16],
    next: usize,
    key: &AppKey<'_>,
    key_at: impl Fn(u16) -> AppKey<'key>,
) -> Result<usize, usize> {
    if let Some(index) = ordered.get(next) {
        if key_at(*index) == *key {
            return Ok(next);
        }
    }

    ordered.binary_search_by(|index| key_at(*index).cmp(key))
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct AppTotals {
    pub members: u32,
    pub live_members: u32,
    pub cpu_ns: Subtotal,
    pub energy_nj: Subtotal,
    pub gpu_ns: Subtotal,
    pub disk_read_bytes: Subtotal,
    pub disk_written_bytes: Subtotal,
    /// Sum of members with both directional deltas measured, not two partial App subtotals.
    pub disk_io_bytes: Subtotal,
    pub resident_bytes: Subtotal,
    pub footprint_bytes: Subtotal,
    pub idle_wakeups: Subtotal,
    pub pageins: Subtotal,
    pub threads: Subtotal,
    /// Completeness of membership only; metric availability is carried by each subtotal.
    pub membership_complete: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Subtotal {
    /// Exact sum of measured members. Zero with no measured members is not measured zero.
    pub known: u64,
    pub measured_members: u32,
    pub unavailable_members: u32,
    pub overflowed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Amount {
    Known(u64),
    Unavailable(Unavailable),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unavailable {
    Refused,
    Gone,
    Unknown,
    Interval,
    Creation,
    Baseline,
    CounterReset,
    Overflow,
    Exec,
    Program,
    ExitUnverified,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ProcessTotals<'bytes> {
    pub pid: i32,
    pub creation: Option<ProcessCreation>,
    pub app: Option<AppKey<'bytes>>,
    pub executable_name: Option<&'bytes [u8]>,
    pub program_unavailable: Option<IdentityUnavailable>,
    pub cpu_ns: Amount,
    pub energy_nj: Amount,
    pub gpu_ns: Amount,
    pub disk_read_bytes: Amount,
    pub disk_written_bytes: Amount,
    pub disk_io_bytes: Amount,
    pub idle_wakeups: Amount,
    pub pageins: Amount,
    pub threads: Option<u64>,
    pub app_attribution: Option<Unavailable>,
    pub resident_bytes_last: Option<u64>,
    pub footprint_bytes_last: Option<u64>,
    pub state: ProcessState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessState {
    Live,
    /// Caller verified final usage and its creation identity against the recorded exit.
    ExitedVerified,
    ExitedUnverified,
}

/// Every endpoint carries its source identity, including identity-only observations.
pub struct ProcessObservation<'bytes> {
    pub stamp: Stamp,
    pub pid: i32,
    pub creation: Option<ProcessCreation>,
    pub program: CapturedProgram<'bytes>,
    pub usage: Result<&'bytes Usage, Unavailable>,
    /// The GPU time the process's GPU clients have used, none where it has no GPU client or
    /// the GPU read failed, which the recording does not tell apart.
    pub gpu_ns: Option<u64>,
    pub threads: Option<u64>,
    pub state: ProcessState,
}

pub struct ProcessInterval<'bytes> {
    pub previous: Option<&'bytes ProcessObservation<'bytes>>,
    pub selected: &'bytes ProcessObservation<'bytes>,
    pub start: Option<&'bytes VerifiedStart<'bytes>>,
}

/// An observed Start is evidence only after its kernel creation was verified.
/// Its observation time does not claim an exact kernel occurrence time.
pub struct VerifiedStart<'bytes> {
    pub stamp: Stamp,
    pub pid: i32,
    pub creation: ProcessCreation,
    pub program: CapturedProgram<'bytes>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub boot: [u64; 2],
    pub run: [u64; 2],
    pub publication: u64,
    pub continuous_ns: Option<u64>,
    pub awake_ns: Option<u64>,
}

pub struct Interval {
    pub previous: Stamp,
    pub selected: Stamp,
    /// False after missing observations, absent sources or an unavailable interval.
    pub observations_complete: bool,
    /// Event loss prevents a complete membership claim and any new-creation zero baseline.
    pub events_complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    ProcessBound,
    ProgramBound,
    InvalidProgram,
    DuplicateProcess,
    InvalidEndpoint,
}

impl<'bytes> Aggregate<'bytes> {
    /// Initializes the large result in caller-owned storage.
    pub fn initialize(output: &mut Option<Box<Self>>) {
        assert!(output.is_none());
        *output = Some(Box::new(Self {
            apps: BTreeMap::new(),
            processes: Vec::new(),
            ungrouped_members: 0,
            live_ungrouped_members: 0,
            membership_complete: false,
            elapsed_ns: None,
        }));
    }

    /// Replaces prior output. Any refusal clears output, preventing partial-complete claims.
    /// Working storage is O(rows + captured bytes), with both bounded above.
    pub fn compute(
        &mut self,
        interval: &Interval,
        processes: &[ProcessInterval<'bytes>],
    ) -> Result<(), Error> {
        self.apps.clear();
        self.processes.clear();
        self.ungrouped_members = 0;
        self.live_ungrouped_members = 0;
        self.membership_complete = false;
        self.elapsed_ns = None;
        validate(interval, processes)?;
        self.processes.reserve_exact(processes.len());
        self.elapsed_ns = elapsed(interval);
        self.membership_complete = interval.observations_complete;
        if !interval.events_complete {
            self.membership_complete = false;
        }
        for process in processes {
            self.compute_process(interval, process);
        }
        for totals in self.apps.values_mut() {
            totals.membership_complete = self.membership_complete;
        }
        assert!(self.apps.len() <= processes.len());
        assert_eq!(self.processes.len(), processes.len());
        Ok(())
    }

    fn compute_process(&mut self, interval: &Interval, process: &ProcessInterval<'bytes>) {
        let selected = process.selected;
        let (app, executable_name, program_unavailable) = match selected.program {
            Ok(program) => {
                let app = match program.app_path {
                    Some(path) => AppKey::Bundle(path),
                    None => AppKey::Executable(program.executable_name),
                };
                (Some(app), Some(program.executable_name), None)
            }
            Err(reason) => (None, None, Some(reason)),
        };
        self.processes.push(ProcessTotals {
            pid: selected.pid,
            creation: selected.creation,
            app,
            executable_name,
            program_unavailable,
            cpu_ns: Amount::Unavailable(Unavailable::Baseline),
            energy_nj: Amount::Unavailable(Unavailable::Baseline),
            gpu_ns: Amount::Unavailable(Unavailable::Baseline),
            disk_read_bytes: Amount::Unavailable(Unavailable::Baseline),
            disk_written_bytes: Amount::Unavailable(Unavailable::Baseline),
            disk_io_bytes: Amount::Unavailable(Unavailable::Baseline),
            idle_wakeups: Amount::Unavailable(Unavailable::Baseline),
            pageins: Amount::Unavailable(Unavailable::Baseline),
            threads: if selected.state == ProcessState::Live {
                selected.threads
            } else {
                None
            },
            app_attribution: None,
            resident_bytes_last: last_gauge(process, |usage| usage.resident_bytes),
            footprint_bytes_last: last_gauge(process, |usage| usage.footprint_bytes),
            state: selected.state,
        });
        let output = self.processes.last_mut().unwrap_or_else(|| unreachable!());
        deltas(interval, process, output);
        if let Some(app) = app {
            let totals = self.apps.entry(app).or_default();
            totals.members += 1;
            totals.membership_complete = self.membership_complete;
            totals
                .cpu_ns
                .add(attributed(output.cpu_ns, output.app_attribution));
            totals
                .energy_nj
                .add(attributed(output.energy_nj, output.app_attribution));
            totals
                .gpu_ns
                .add(attributed(output.gpu_ns, output.app_attribution));
            compute_process_counters(totals, output);
            if selected.state == ProcessState::Live {
                compute_process_gauges(totals, selected);
            }
        } else {
            self.ungrouped_members += 1;
            if selected.state == ProcessState::Live {
                self.live_ungrouped_members += 1;
            }
            self.membership_complete = false;
        }
    }
}

fn compute_process_gauges(totals: &mut AppTotals, selected: &ProcessObservation<'_>) {
    totals.live_members += 1;
    totals
        .resident_bytes
        .add(gauge(selected, |usage| usage.resident_bytes));
    totals
        .footprint_bytes
        .add(gauge(selected, |usage| usage.footprint_bytes));
    totals.threads.add(
        selected
            .threads
            .map_or(Amount::Unavailable(Unavailable::Unknown), Amount::Known),
    );
}

fn compute_process_counters(totals: &mut AppTotals, process: &ProcessTotals<'_>) {
    totals
        .idle_wakeups
        .add(attributed(process.idle_wakeups, process.app_attribution));
    totals
        .pageins
        .add(attributed(process.pageins, process.app_attribution));
    totals
        .disk_read_bytes
        .add(attributed(process.disk_read_bytes, process.app_attribution));
    totals.disk_written_bytes.add(attributed(
        process.disk_written_bytes,
        process.app_attribution,
    ));
    totals
        .disk_io_bytes
        .add(attributed(process.disk_io_bytes, process.app_attribution));
}

impl Subtotal {
    pub(crate) fn add(&mut self, value: Amount) {
        match value {
            Amount::Known(value) => {
                if let Some(sum) = self.known.checked_add(value) {
                    self.known = sum;
                    self.measured_members += 1;
                } else {
                    self.unavailable_members += 1;
                    self.overflowed = true;
                }
            }
            Amount::Unavailable(_) => self.unavailable_members += 1,
        }
    }
}

fn gauge(observation: &ProcessObservation, field: fn(&Usage) -> u64) -> Amount {
    match observation.usage {
        Ok(usage) => Amount::Known(field(usage)),
        Err(reason) => Amount::Unavailable(reason),
    }
}

fn last_gauge(process: &ProcessInterval, field: fn(&Usage) -> u64) -> Option<u64> {
    if let Ok(usage) = process.selected.usage {
        return Some(field(usage));
    }
    if process.selected.state == ProcessState::Live {
        return None;
    }
    let previous = process.previous?;
    if !same_source(&Endpoints {
        previous: &previous.stamp,
        selected: &process.selected.stamp,
    }) {
        return None;
    }
    if !same_creation(&Endpoints {
        previous,
        selected: process.selected,
    }) {
        return None;
    }
    match previous.usage {
        Ok(usage) => Some(field(usage)),
        Err(_) => None,
    }
}

fn attributed(value: Amount, reason: Option<Unavailable>) -> Amount {
    reason.map_or(value, Amount::Unavailable)
}

fn elapsed(interval: &Interval) -> Option<u64> {
    if !same_source(&Endpoints {
        previous: &interval.previous,
        selected: &interval.selected,
    }) {
        return None;
    }
    if interval.previous.publication.checked_add(1) != Some(interval.selected.publication) {
        return None;
    }
    if !interval.observations_complete {
        return None;
    }
    interval
        .selected
        .continuous_ns?
        .checked_sub(interval.previous.continuous_ns?)
        .filter(|value| *value > 0)
}

fn same_source(endpoints: &Endpoints<&Stamp>) -> bool {
    let Endpoints { previous, selected } = endpoints;
    if previous.boot != selected.boot {
        return false;
    }
    previous.run == selected.run
}

fn same_creation(endpoints: &Endpoints<&ProcessObservation>) -> bool {
    let Endpoints { previous, selected } = endpoints;
    if previous.pid != selected.pid {
        return false;
    }
    match (previous.creation, selected.creation) {
        (Some(previous), Some(selected)) => previous.unique_id == selected.unique_id,
        _ => false,
    }
}

fn same_program(endpoints: &Endpoints<&ProcessObservation>) -> bool {
    let Endpoints { previous, selected } = endpoints;
    if previous.creation != selected.creation {
        return false;
    }
    programs_match(&Endpoints {
        previous: previous.program,
        selected: selected.program,
    })
}

fn programs_match(endpoints: &Endpoints<CapturedProgram>) -> bool {
    match (endpoints.previous, endpoints.selected) {
        (Ok(previous), Ok(selected)) => {
            if previous.executable_name != selected.executable_name {
                return false;
            }
            previous.app_path == selected.app_path
        }
        _ => false,
    }
}

fn deltas(interval: &Interval, process: &ProcessInterval, output: &mut ProcessTotals) {
    let selected = process.selected;
    if elapsed(interval).is_none() {
        deltas_unavailable(output, Unavailable::Interval);
        return;
    }
    if selected.state == ProcessState::ExitedUnverified {
        deltas_unavailable(output, Unavailable::ExitUnverified);
        return;
    }
    let now = match selected.usage {
        Ok(usage) => usage,
        Err(reason) => {
            deltas_unavailable(output, reason);
            return;
        }
    };
    if let Some(previous) = process.previous {
        if !same_creation(&Endpoints { previous, selected }) {
            deltas_unavailable(output, Unavailable::Creation);
            return;
        }
        if !same_program(&Endpoints { previous, selected }) {
            output.app_attribution = Some(Unavailable::Exec);
        }
        match previous.usage {
            Ok(before) => deltas_usage(
                &Endpoints {
                    previous: before,
                    selected: now,
                },
                &Endpoints {
                    previous: previous.gpu_ns,
                    selected: selected.gpu_ns,
                },
                output,
            ),
            Err(reason) => deltas_unavailable(output, reason),
        }
    } else {
        if start_matches(interval, process) {
            output.cpu_ns = cpu_delta(now, None);
            output.energy_nj = Amount::Known(now.energy_nj);
            output.gpu_ns = selected
                .gpu_ns
                .map_or(Amount::Unavailable(Unavailable::Unknown), Amount::Known);
            deltas_disk(now, None, output);
            deltas_activity(now, None, output);
            deltas_start(process, output);
        } else {
            deltas_unavailable(output, Unavailable::Baseline);
        }
    }
    if selected.program.is_err() {
        output.app_attribution = Some(Unavailable::Program);
    }
}

fn deltas_start(process: &ProcessInterval, output: &mut ProcessTotals) {
    if let Some(start) = process.start {
        if process.selected.creation == Some(start.creation) {
            if !programs_match(&Endpoints {
                previous: start.program,
                selected: process.selected.program,
            }) {
                output.app_attribution = Some(Unavailable::Exec);
            }
        } else {
            output.app_attribution = Some(Unavailable::Exec);
        }
    }
}

fn deltas_unavailable(output: &mut ProcessTotals, reason: Unavailable) {
    output.cpu_ns = Amount::Unavailable(reason);
    output.energy_nj = Amount::Unavailable(reason);
    output.gpu_ns = Amount::Unavailable(reason);
    output.disk_read_bytes = Amount::Unavailable(reason);
    output.disk_written_bytes = Amount::Unavailable(reason);
    output.disk_io_bytes = Amount::Unavailable(reason);
    output.idle_wakeups = Amount::Unavailable(reason);
    output.pageins = Amount::Unavailable(reason);
}

fn deltas_usage(
    endpoints: &Endpoints<&Usage>,
    gpu_ns: &Endpoints<Option<u64>>,
    output: &mut ProcessTotals,
) {
    let Endpoints {
        previous: before,
        selected: now,
    } = endpoints;
    if before.awake_ns_at_start != now.awake_ns_at_start {
        deltas_unavailable(output, Unavailable::Creation);
        return;
    }
    output.cpu_ns = cpu_delta(now, Some(before));
    output.energy_nj = now.energy_nj.checked_sub(before.energy_nj).map_or(
        Amount::Unavailable(Unavailable::CounterReset),
        Amount::Known,
    );
    output.gpu_ns = gpu_delta(gpu_ns);
    deltas_disk(now, Some(before), output);
    deltas_activity(now, Some(before), output);
}

/// The GPU time gained between the endpoints, unknown where either lacks one, since a
/// missing GPU time may be a failed read rather than no GPU use.
fn gpu_delta(gpu_ns: &Endpoints<Option<u64>>) -> Amount {
    let Some(previous) = gpu_ns.previous else {
        return Amount::Unavailable(Unavailable::Unknown);
    };
    let Some(selected) = gpu_ns.selected else {
        return Amount::Unavailable(Unavailable::Unknown);
    };

    selected.checked_sub(previous).map_or(
        Amount::Unavailable(Unavailable::CounterReset),
        Amount::Known,
    )
}

fn deltas_disk(now: &Usage, before: Option<&Usage>, output: &mut ProcessTotals<'_>) {
    output.disk_read_bytes = now
        .disk_read_bytes
        .checked_sub(before.map_or(0, |usage| usage.disk_read_bytes))
        .map_or(
            Amount::Unavailable(Unavailable::CounterReset),
            Amount::Known,
        );
    output.disk_written_bytes = now
        .disk_written_bytes
        .checked_sub(before.map_or(0, |usage| usage.disk_written_bytes))
        .map_or(
            Amount::Unavailable(Unavailable::CounterReset),
            Amount::Known,
        );
    output.disk_io_bytes = match (output.disk_read_bytes, output.disk_written_bytes) {
        (Amount::Known(read), Amount::Known(written)) => read
            .checked_add(written)
            .map_or(Amount::Unavailable(Unavailable::Overflow), Amount::Known),
        (Amount::Unavailable(reason), _) | (_, Amount::Unavailable(reason)) => {
            Amount::Unavailable(reason)
        }
    };
}

fn deltas_activity(now: &Usage, before: Option<&Usage>, output: &mut ProcessTotals<'_>) {
    output.idle_wakeups = now
        .idle_wakeups
        .checked_sub(before.map_or(0, |usage| usage.idle_wakeups))
        .map_or(
            Amount::Unavailable(Unavailable::CounterReset),
            Amount::Known,
        );
    output.pageins = now
        .pageins
        .checked_sub(before.map_or(0, |usage| usage.pageins))
        .map_or(
            Amount::Unavailable(Unavailable::CounterReset),
            Amount::Known,
        );
}

/// Exact event counts divided only after aggregation at the presentation boundary.
#[must_use]
pub fn events_per_second(rate: EventRate) -> Option<f64> {
    if rate.elapsed_ns == 0 {
        return None;
    }

    Some(number(rate.events) / number(rate.elapsed_ns) * 1_000_000_000.0)
}

fn cpu_delta(now: &Usage, before: Option<&Usage>) -> Amount {
    let user = now
        .user_ns
        .checked_sub(before.map_or(0, |usage| usage.user_ns));
    let system = now
        .system_ns
        .checked_sub(before.map_or(0, |usage| usage.system_ns));
    match (user, system) {
        (Some(user), Some(system)) => user
            .checked_add(system)
            .map_or(Amount::Unavailable(Unavailable::Overflow), Amount::Known),
        _ => Amount::Unavailable(Unavailable::CounterReset),
    }
}

fn start_matches(interval: &Interval, process: &ProcessInterval) -> bool {
    if !interval.events_complete {
        return false;
    }
    let Some(start) = process.start else {
        return false;
    };
    if process.selected.creation.map(|creation| creation.unique_id)
        != Some(start.creation.unique_id)
    {
        return false;
    }
    if start.pid != process.selected.pid {
        return false;
    }
    // Notice time does not prove birth. The verified rusage creation clock must fall
    // inside the interval's source awake-clock bounds, independently of continuous time.
    let Ok(usage) = process.selected.usage else {
        return false;
    };
    let (Some(before), Some(now)) = (interval.previous.awake_ns, interval.selected.awake_ns) else {
        return false;
    };
    if usage.awake_ns_at_start <= before {
        return false;
    }
    usage.awake_ns_at_start <= now
}

fn validate(interval: &Interval, processes: &[ProcessInterval]) -> Result<(), Error> {
    if processes.len() > PROCESS_INTERVALS_MAX {
        return Err(Error::ProcessBound);
    }
    let mut bytes = 0;
    let mut creations = BTreeSet::new();
    for process in processes {
        validate_endpoint(interval, process)?;
        let creation = process.selected.creation.map(|creation| creation.unique_id);
        if !creations.insert((process.selected.pid, creation)) {
            return Err(Error::DuplicateProcess);
        }
        validate_program(process.selected.program, &mut bytes)?;
        if let Some(previous) = process.previous {
            validate_program(previous.program, &mut bytes)?;
        }
        if let Some(start) = process.start {
            validate_program(start.program, &mut bytes)?;
        }
    }
    Ok(())
}

fn validate_endpoint(interval: &Interval, process: &ProcessInterval) -> Result<(), Error> {
    if process.selected.stamp != interval.selected {
        return Err(Error::InvalidEndpoint);
    }
    if let Some(previous) = process.previous {
        if previous.stamp != interval.previous {
            return Err(Error::InvalidEndpoint);
        }
        if previous.state != ProcessState::Live {
            return Err(Error::InvalidEndpoint);
        }
    }
    if let Some(start) = process.start {
        if !same_source(&Endpoints {
            previous: &start.stamp,
            selected: &interval.selected,
        }) {
            return Err(Error::InvalidEndpoint);
        }
        if start.stamp.publication != interval.selected.publication {
            return Err(Error::InvalidEndpoint);
        }
    }
    Ok(())
}

fn validate_program(program: CapturedProgram, bytes: &mut usize) -> Result<(), Error> {
    if let Ok(program) = program {
        program.validate().map_err(|_| Error::InvalidProgram)?;
        *bytes = bytes
            .checked_add(program.executable_name.len())
            .and_then(|bytes| bytes.checked_add(program.app_path.map_or(0, <[u8]>::len)))
            .ok_or(Error::ProgramBound)?;
        if *bytes > PROGRAM_BYTES_MAX {
            return Err(Error::ProgramBound);
        }
    }
    Ok(())
}

/// Floating conversion happens only at the display boundary, after exact delta summation.
#[must_use]
pub fn cpu_percent_one_core(rate: CpuRate) -> Option<f64> {
    if rate.elapsed_ns == 0 {
        return None;
    }
    Some(number(rate.cpu_ns) / number(rate.elapsed_ns) * 100.0)
}

#[must_use]
pub fn energy_joules(energy_nj: u64) -> f64 {
    number(energy_nj) / 1_000_000_000.0
}

#[must_use]
pub fn energy_watts(rate: EnergyRate) -> Option<f64> {
    if rate.elapsed_ns == 0 {
        return None;
    }
    Some(number(rate.energy_nj) / number(rate.elapsed_ns))
}

/// The GPU time as a percent of the interval, which passes 100 where an App's clients overlap.
#[must_use]
pub fn gpu_percent(rate: GpuRate) -> Option<f64> {
    if rate.elapsed_ns == 0 {
        return None;
    }

    Some(number(rate.gpu_ns) / number(rate.elapsed_ns) * 100.0)
}

#[must_use]
pub fn bytes_per_second(rate: BytesRate) -> Option<f64> {
    if rate.elapsed_ns == 0 {
        return None;
    }

    Some(number(rate.bytes) / number(rate.elapsed_ns) * 1_000_000_000.0)
}

fn number(value: u64) -> f64 {
    let upper = u32::try_from(value >> 32).unwrap_or_else(|_| unreachable!());
    let lower = u32::try_from(value & u64::from(u32::MAX)).unwrap_or_else(|_| unreachable!());
    f64::from(upper) * 4_294_967_296.0 + f64::from(lower)
}

#[cfg(test)]
mod boundary_tests;
#[cfg(test)]
mod disk_tests;
#[cfg(test)]
mod fixture_tests;
#[cfg(test)]
mod gpu_tests;
#[cfg(test)]
mod limits_tests;
#[cfg(test)]
mod order_tests;
#[cfg(test)]
mod performance_tests;
#[cfg(test)]
mod tests;

#[cfg(test)]
mod timing_tests;
