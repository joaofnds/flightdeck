//! The App rule: what each App's processes spent between two ticks of one collector run.
use crate::programs::{App, Programs};

/// One tick of a collector run: when the collector took it, and its processes.
pub struct Tick<'columns> {
    pub stamp: Stamp,
    pub processes: Processes<'columns>,
}

/// Where and when a tick was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub recording: [u64; 2],
    pub boot: [u64; 2],
    pub run: [u64; 2],
    /// The tick's number in its run, one more than the tick before it.
    pub publication: u64,
    /// The Mac's continuous clock at the tick, which counts sleep, if it was read.
    pub continuous_ns: Option<u64>,
}

/// The processes of one tick as columns of one length, in ascending pid order with each pid
/// once.
pub struct Processes<'columns> {
    pub pid: &'columns [i32],
    /// The process's creation, if it was read, which tells two processes of one pid apart.
    pub creation: &'columns [Option<Creation>],
    /// The process's program, as its entry in the run's dictionary, if it was read. Two entries
    /// of one index hold two programs, as the ring's dictionary holds each program once.
    pub program: &'columns [Option<u32>],
    /// Whether the process's usage, the columns below, was read.
    pub usage: &'columns [Measurement],
    pub awake_ns_at_start: &'columns [u64],
    pub user_ns: &'columns [u64],
    pub system_ns: &'columns [u64],
}

/// A process's creation as the kernel numbers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Creation {
    pub unique_id: u64,
    /// Grows when the process runs another program.
    pub pid_version: i32,
}

/// Whether a process's usage was read at a tick, or why not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Measurement {
    Measured,
    /// macOS refused the read.
    Refused,
    /// The process exited before the read.
    Gone,
}

impl Processes<'_> {
    fn count(&self) -> usize {
        let count = self.pid.len();
        assert_eq!(self.creation.len(), count);
        assert_eq!(self.program.len(), count);
        assert_eq!(self.usage.len(), count);
        assert_eq!(self.awake_ns_at_start.len(), count);
        assert_eq!(self.user_ns.len(), count);
        assert_eq!(self.system_ns.len(), count);
        for pair in self.pid.windows(2) {
            assert!(pair[0] < pair[1]);
        }
        count
    }

    /// The row of the process `pid` of `creation`, if the tick holds it.
    fn find(&self, pid: i32, creation: Option<Creation>) -> Option<usize> {
        let creation = creation?;
        let row = self.pid.binary_search(&pid).ok()?;
        let found = self.creation[row]?;
        if found.unique_id == creation.unique_id {
            Some(row)
        } else {
            None
        }
    }
}

/// What one process of the selected tick spent since the tick before.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessTotals {
    pub cpu_ns: Amount,
    /// Why no App takes what the process spent, where one does not.
    pub app_attribution: Option<Unavailable>,
}

/// What a process spent, or why it is not known.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Amount {
    Known(u64),
    Unavailable(Unavailable),
}

/// Why what a process spent is not known, or why no App takes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Unavailable {
    Refused,
    Gone,
    /// The two ticks do not span one interval of one collector run.
    Interval,
    /// The process is not the one the previous tick observed under its pid.
    Creation,
    /// The previous tick holds no observation of the process.
    Baseline,
    /// A counter went back.
    CounterReset,
    Overflow,
    /// The process ran another program, so its usage cannot be split between the two.
    Exec,
    /// The process's program could not be read.
    Program,
}

/// What one App's processes spent between the two ticks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Totals {
    pub app: App,
    pub members: u32,
    pub cpu_ns: Subtotal,
}

/// The sum of what the App's members spent, over the members it could be measured for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Subtotal {
    /// Exact sum of measured members. Zero with no measured members is not measured zero.
    pub known: u64,
    pub measured_members: u32,
    pub unavailable_members: u32,
    pub overflowed: bool,
}

/// The Apps of one tick, in buffers taken at startup.
pub struct Apps {
    /// What each process of the tick spent, in the tick's order.
    processes: Vec<ProcessTotals>,
    /// The totals of the Apps the tick's processes belong to.
    totals: Vec<Totals>,
    /// Each App's row in `totals`, or `SLOT_EMPTY` for an App no process of the tick runs.
    slots: Vec<u32>,
    /// The tick's processes whose program could not be read.
    ungrouped_members: u32,
}

const SLOT_EMPTY: u32 = u32::MAX;

impl Apps {
    #[must_use]
    pub fn new(processes_max: u32, programs: &Programs) -> Self {
        Self {
            processes: Vec::with_capacity(crate::usize_from_u32(processes_max)),
            totals: Vec::with_capacity(crate::usize_from_u32(processes_max)),
            slots: vec![SLOT_EMPTY; programs.apps_max()],
            ungrouped_members: 0,
        }
    }

    /// Derives the Apps of `selected`, the tick after `previous`.
    pub fn derive(&mut self, programs: &Programs, previous: &Tick, selected: &Tick) {
        let count = selected.processes.count();
        assert!(count <= self.processes.capacity());
        assert!(previous.processes.count() <= self.processes.capacity());
        self.clear();

        let compatible = derive_compatible(&previous.stamp, &selected.stamp);
        for row in 0..count {
            let process = derive_process(compatible, &previous.processes, &selected.processes, row);
            self.processes.push(process);
            if let Some(entry) = selected.processes.program[row] {
                let totals = self.totals_of(programs.app(entry));
                totals.members += 1;
                totals
                    .cpu_ns
                    .add(attributed(process.cpu_ns, process.app_attribution));
            } else {
                self.ungrouped_members += 1;
            }
        }
    }

    /// What each process of the tick last derived spent, in the tick's order.
    #[must_use]
    pub fn processes(&self) -> &[ProcessTotals] {
        &self.processes
    }

    /// The Apps of the tick last derived.
    #[must_use]
    pub fn totals(&self) -> &[Totals] {
        &self.totals
    }

    /// The processes of the tick last derived that belong to no App.
    #[must_use]
    pub fn ungrouped_members(&self) -> u32 {
        self.ungrouped_members
    }

    fn clear(&mut self) {
        for totals in &self.totals {
            self.slots[totals.app.index()] = SLOT_EMPTY;
        }
        self.totals.clear();
        self.processes.clear();
        self.ungrouped_members = 0;
    }

    fn totals_of(&mut self, app: App) -> &mut Totals {
        if self.slots[app.index()] == SLOT_EMPTY {
            assert!(self.totals.len() < self.totals.capacity());

            self.slots[app.index()] = crate::u32_from_length(self.totals.len());
            self.totals.push(Totals {
                app,
                members: 0,
                cpu_ns: Subtotal::default(),
            });
        }
        let slot = crate::usize_from_u32(self.slots[app.index()]);
        &mut self.totals[slot]
    }
}

/// Whether `previous` and `selected` span one interval of one collector run, in which the
/// continuous clock moved on.
fn derive_compatible(previous: &Stamp, selected: &Stamp) -> bool {
    if previous.recording != selected.recording {
        return false;
    }
    if previous.boot != selected.boot {
        return false;
    }
    if previous.run != selected.run {
        return false;
    }
    if previous.publication.checked_add(1) != Some(selected.publication) {
        return false;
    }
    let Some(previous) = previous.continuous_ns else {
        return false;
    };
    let Some(selected) = selected.continuous_ns else {
        return false;
    };
    previous < selected
}

/// What the process of `selected`'s `row` spent since `previous`.
fn derive_process(
    compatible: bool,
    previous: &Processes,
    selected: &Processes,
    row: usize,
) -> ProcessTotals {
    let mut process = ProcessTotals {
        cpu_ns: Amount::Unavailable(Unavailable::Baseline),
        app_attribution: None,
    };
    if !compatible {
        process.cpu_ns = Amount::Unavailable(Unavailable::Interval);
        return process;
    }
    if let Some(reason) = unmeasured(selected.usage[row]) {
        process.cpu_ns = Amount::Unavailable(reason);
        return process;
    }

    if let Some(before) = previous.find(selected.pid[row], selected.creation[row]) {
        if !derive_same_program(previous, before, selected, row) {
            process.app_attribution = Some(Unavailable::Exec);
        }
        process.cpu_ns = derive_cpu_ns(previous, before, selected, row);
    }
    if selected.program[row].is_none() {
        process.app_attribution = Some(Unavailable::Program);
    }
    process
}

/// Whether the process ran one program at both ticks, which its pid version and its program
/// tell.
fn derive_same_program(
    previous: &Processes,
    before: usize,
    selected: &Processes,
    row: usize,
) -> bool {
    if previous.creation[before] != selected.creation[row] {
        return false;
    }
    let Some(program) = selected.program[row] else {
        return false;
    };
    previous.program[before] == Some(program)
}

/// The CPU time the process spent between its observations at `before` and `row`.
fn derive_cpu_ns(previous: &Processes, before: usize, selected: &Processes, row: usize) -> Amount {
    if let Some(reason) = unmeasured(previous.usage[before]) {
        return Amount::Unavailable(reason);
    }
    if previous.awake_ns_at_start[before] != selected.awake_ns_at_start[row] {
        return Amount::Unavailable(Unavailable::Creation);
    }

    let user_ns = selected.user_ns[row].checked_sub(previous.user_ns[before]);
    let system_ns = selected.system_ns[row].checked_sub(previous.system_ns[before]);
    let (Some(user_ns), Some(system_ns)) = (user_ns, system_ns) else {
        return Amount::Unavailable(Unavailable::CounterReset);
    };
    match user_ns.checked_add(system_ns) {
        Some(cpu_ns) => Amount::Known(cpu_ns),
        None => Amount::Unavailable(Unavailable::Overflow),
    }
}

/// Why a process's usage was not read, if it was not.
fn unmeasured(measurement: Measurement) -> Option<Unavailable> {
    match measurement {
        Measurement::Measured => None,
        Measurement::Refused => Some(Unavailable::Refused),
        Measurement::Gone => Some(Unavailable::Gone),
    }
}

/// What an App takes of `value`: nothing where `reason` says why no App takes it.
fn attributed(value: Amount, reason: Option<Unavailable>) -> Amount {
    match reason {
        Some(reason) => Amount::Unavailable(reason),
        None => value,
    }
}

impl Subtotal {
    fn add(&mut self, value: Amount) {
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

#[cfg(test)]
mod tests;
