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
    pub energy_nj: &'columns [u64],
    /// The GPU time of the process's GPU clients, none where it has no GPU client or the read
    /// failed, which the recording does not tell apart.
    pub gpu_ns: &'columns [Option<u64>],
    pub disk_read_bytes: &'columns [u64],
    pub disk_written_bytes: &'columns [u64],
    pub idle_wakeups: &'columns [u64],
    pub pageins: &'columns [u64],
    pub resident_bytes: &'columns [u64],
    pub footprint_bytes: &'columns [u64],
    /// The process's threads, if they were read.
    pub threads: &'columns [Option<u64>],
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
        assert_eq!(self.energy_nj.len(), count);
        assert_eq!(self.gpu_ns.len(), count);
        assert_eq!(self.disk_read_bytes.len(), count);
        assert_eq!(self.disk_written_bytes.len(), count);
        assert_eq!(self.idle_wakeups.len(), count);
        assert_eq!(self.pageins.len(), count);
        assert_eq!(self.resident_bytes.len(), count);
        assert_eq!(self.footprint_bytes.len(), count);
        assert_eq!(self.threads.len(), count);
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

/// What one process of the selected tick spent since the tick before, and what it held at
/// the tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessTotals {
    pub deltas: Deltas,
    /// Why no App takes what the process spent, where one does not.
    pub app_attribution: Option<Unavailable>,
    pub resident_bytes_last: Option<u64>,
    pub footprint_bytes_last: Option<u64>,
    pub threads: Option<u64>,
}

/// How much each of a process's counters grew between the two ticks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Deltas {
    /// User and system time together.
    pub cpu_ns: Amount,
    pub energy_nj: Amount,
    pub gpu_ns: Amount,
    pub disk_read_bytes: Amount,
    pub disk_written_bytes: Amount,
    /// Disk reads and writes together.
    pub disk_io_bytes: Amount,
    pub idle_wakeups: Amount,
    pub pageins: Amount,
}

impl Deltas {
    /// The deltas of a process none of whose counters' changes are known, for `reason`.
    fn unavailable(reason: Unavailable) -> Self {
        let amount = Amount::Unavailable(reason);
        Self {
            cpu_ns: amount,
            energy_nj: amount,
            gpu_ns: amount,
            disk_read_bytes: amount,
            disk_written_bytes: amount,
            disk_io_bytes: amount,
            idle_wakeups: amount,
            pageins: amount,
        }
    }
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
    /// The value was not read, which the recording does not tell from no value.
    Unknown,
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

/// What one App's processes spent between the two ticks, and what its live processes held at
/// the selected tick.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Totals {
    pub app: App,
    pub members: u32,
    pub live_members: u32,
    pub cpu_ns: Subtotal,
    pub energy_nj: Subtotal,
    pub gpu_ns: Subtotal,
    pub disk_read_bytes: Subtotal,
    pub disk_written_bytes: Subtotal,
    /// Sum of members with both directional deltas measured, not two partial App subtotals.
    pub disk_io_bytes: Subtotal,
    pub idle_wakeups: Subtotal,
    pub pageins: Subtotal,
    pub resident_bytes: Subtotal,
    pub footprint_bytes: Subtotal,
    pub threads: Subtotal,
}

impl Totals {
    fn new(app: App) -> Self {
        Self {
            app,
            members: 0,
            live_members: 0,
            cpu_ns: Subtotal::default(),
            energy_nj: Subtotal::default(),
            gpu_ns: Subtotal::default(),
            disk_read_bytes: Subtotal::default(),
            disk_written_bytes: Subtotal::default(),
            disk_io_bytes: Subtotal::default(),
            idle_wakeups: Subtotal::default(),
            pageins: Subtotal::default(),
            resident_bytes: Subtotal::default(),
            footprint_bytes: Subtotal::default(),
            threads: Subtotal::default(),
        }
    }

    /// Adds what a member spent, unless no App takes it.
    fn add_deltas(&mut self, process: &ProcessTotals) {
        let reason = process.app_attribution;
        let deltas = &process.deltas;
        self.members += 1;
        self.cpu_ns.add(attributed(deltas.cpu_ns, reason));
        self.energy_nj.add(attributed(deltas.energy_nj, reason));
        self.gpu_ns.add(attributed(deltas.gpu_ns, reason));
        self.disk_read_bytes
            .add(attributed(deltas.disk_read_bytes, reason));
        self.disk_written_bytes
            .add(attributed(deltas.disk_written_bytes, reason));
        self.disk_io_bytes
            .add(attributed(deltas.disk_io_bytes, reason));
        self.idle_wakeups
            .add(attributed(deltas.idle_wakeups, reason));
        self.pageins.add(attributed(deltas.pageins, reason));
    }

    /// Adds what the live member of `selected`'s `row` held at the tick.
    fn add_gauges(&mut self, selected: &Processes, row: usize) {
        let unmeasured = unmeasured(selected.usage[row]);
        self.live_members += 1;
        self.resident_bytes
            .add(gauge(unmeasured, selected.resident_bytes[row]));
        self.footprint_bytes
            .add(gauge(unmeasured, selected.footprint_bytes[row]));
        self.threads.add(match selected.threads[row] {
            Some(threads) => Amount::Known(threads),
            None => Amount::Unavailable(Unavailable::Unknown),
        });
    }
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
                totals.add_deltas(&process);
                totals.add_gauges(&selected.processes, row);
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
            self.totals.push(Totals::new(app));
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
    let unmeasured = unmeasured(selected.usage[row]);
    let mut process = ProcessTotals {
        deltas: Deltas::unavailable(Unavailable::Baseline),
        app_attribution: None,
        resident_bytes_last: last(unmeasured, selected.resident_bytes[row]),
        footprint_bytes_last: last(unmeasured, selected.footprint_bytes[row]),
        threads: selected.threads[row],
    };
    if !compatible {
        process.deltas = Deltas::unavailable(Unavailable::Interval);
        return process;
    }
    if let Some(reason) = unmeasured {
        process.deltas = Deltas::unavailable(reason);
        return process;
    }

    if let Some(before) = previous.find(selected.pid[row], selected.creation[row]) {
        if !derive_same_program(previous, before, selected, row) {
            process.app_attribution = Some(Unavailable::Exec);
        }
        process.deltas = derive_deltas(previous, before, selected, row);
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

/// How much the process's counters grew between its observations at `before` and `row`.
fn derive_deltas(previous: &Processes, before: usize, selected: &Processes, row: usize) -> Deltas {
    if let Some(reason) = unmeasured(previous.usage[before]) {
        return Deltas::unavailable(reason);
    }
    if previous.awake_ns_at_start[before] != selected.awake_ns_at_start[row] {
        return Deltas::unavailable(Unavailable::Creation);
    }

    let disk_read_bytes = change(
        previous.disk_read_bytes[before],
        selected.disk_read_bytes[row],
    );
    let disk_written_bytes = change(
        previous.disk_written_bytes[before],
        selected.disk_written_bytes[row],
    );
    Deltas {
        cpu_ns: derive_cpu_ns(previous, before, selected, row),
        energy_nj: change(previous.energy_nj[before], selected.energy_nj[row]),
        gpu_ns: derive_gpu_ns(previous.gpu_ns[before], selected.gpu_ns[row]),
        disk_read_bytes,
        disk_written_bytes,
        disk_io_bytes: derive_disk_io_bytes(disk_read_bytes, disk_written_bytes),
        idle_wakeups: change(previous.idle_wakeups[before], selected.idle_wakeups[row]),
        pageins: change(previous.pageins[before], selected.pageins[row]),
    }
}

/// The CPU time the process spent between its observations at `before` and `row`.
fn derive_cpu_ns(previous: &Processes, before: usize, selected: &Processes, row: usize) -> Amount {
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

/// The GPU time gained between the ticks, unknown where either lacks one, since a missing GPU
/// time may be a failed read rather than no GPU use.
fn derive_gpu_ns(before: Option<u64>, now: Option<u64>) -> Amount {
    let Some(before) = before else {
        return Amount::Unavailable(Unavailable::Unknown);
    };
    let Some(now) = now else {
        return Amount::Unavailable(Unavailable::Unknown);
    };
    change(before, now)
}

/// The disk bytes read and written together, unknown for the reads' reason and then the
/// writes' where either is.
fn derive_disk_io_bytes(read: Amount, written: Amount) -> Amount {
    let Amount::Known(read) = read else {
        return read;
    };
    let Amount::Known(written) = written else {
        return written;
    };
    match read.checked_add(written) {
        Some(bytes) => Amount::Known(bytes),
        None => Amount::Unavailable(Unavailable::Overflow),
    }
}

/// How much a counter grew from `before` to `now`, unknown where it went back.
fn change(before: u64, now: u64) -> Amount {
    match now.checked_sub(before) {
        Some(change) => Amount::Known(change),
        None => Amount::Unavailable(Unavailable::CounterReset),
    }
}

/// A gauge of a live process, unknown for the reason its usage was not read.
fn gauge(unmeasured: Option<Unavailable>, value: u64) -> Amount {
    match unmeasured {
        Some(reason) => Amount::Unavailable(reason),
        None => Amount::Known(value),
    }
}

/// A gauge of a live process, if its usage was read.
fn last(unmeasured: Option<Unavailable>, value: u64) -> Option<u64> {
    match unmeasured {
        Some(_) => None,
        None => Some(value),
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
