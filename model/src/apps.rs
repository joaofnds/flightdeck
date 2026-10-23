//! The App rule: what each App's processes spent between two ticks of one collector run.
use crate::programs::{App, Programs};

/// One tick of a collector run: when the collector took it, its processes, and the processes
/// that exited since the tick before.
pub struct Tick<'columns> {
    pub stamp: Stamp,
    pub processes: Processes<'columns>,
    pub exits: Exits<'columns>,
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
    /// The process's program, as its entry in the run's dictionary, or why it was not read. Two
    /// entries of one index hold two programs, as the ring's dictionary holds each program once.
    pub program: &'columns [Result<u32, ProgramUnavailable>],
    pub usage: Usages<'columns>,
    /// The GPU time of the process's GPU clients, none where it has no GPU client or the read
    /// failed, which the recording does not tell apart.
    pub gpu_ns: &'columns [Option<u64>],
    /// The process's threads, if they were read.
    pub threads: &'columns [Option<u64>],
}

/// The processes that exited since the tick before, each with its usage as its exit read it
/// last, as columns of one length in ascending order of pid and then of unique id.
pub struct Exits<'columns> {
    pub pid: &'columns [i32],
    pub creation: &'columns [Option<Creation>],
    pub usage: Usages<'columns>,
}

/// What processes had used and held when the collector read their usage, as columns of one
/// length.
pub struct Usages<'columns> {
    /// Whether the usage, the columns below, was read.
    pub measurement: &'columns [Measurement],
    pub awake_ns_at_start: &'columns [u64],
    pub user_ns: &'columns [u64],
    pub system_ns: &'columns [u64],
    pub energy_nj: &'columns [u64],
    pub disk_read_bytes: &'columns [u64],
    pub disk_written_bytes: &'columns [u64],
    pub idle_wakeups: &'columns [u64],
    pub pageins: &'columns [u64],
    pub resident_bytes: &'columns [u64],
    pub footprint_bytes: &'columns [u64],
}

/// A process's creation as the kernel numbers it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Creation {
    pub unique_id: u64,
    /// Grows when the process runs another program.
    pub pid_version: i32,
}

/// Why a process's program was not read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProgramUnavailable {
    /// macOS refused the read.
    Refused,
    /// The process exited before the read.
    Gone,
    /// macOS gave no answer the collector could use, or the call failed.
    Unsupported,
    /// The process ran another program between the readings that would name it.
    Changed,
    /// The kernel's answer was not of the size the collector reads.
    Invalid,
    /// The run's program dictionary was full.
    Exhausted,
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
        assert_eq!(self.usage.count(), count);
        assert_eq!(self.gpu_ns.len(), count);
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

    /// The order the rule gives the process of `row` among the tick's processes and exits.
    fn key(&self, row: usize) -> Key {
        key(self.pid[row], self.creation[row])
    }

    /// The live process of `row` as the rule reads it.
    fn observation(&self, row: usize) -> Observation {
        Observation {
            pid: self.pid[row],
            creation: self.creation[row],
            program: self.program[row],
            usage: self.usage.row(row),
            gpu_ns: self.gpu_ns[row],
            threads: self.threads[row],
            state: ProcessState::Live,
        }
    }
}

impl Exits<'_> {
    fn count(&self) -> usize {
        let count = self.pid.len();
        assert_eq!(self.creation.len(), count);
        assert_eq!(self.usage.count(), count);
        for row in 1..count {
            assert!(self.key(row - 1) <= self.key(row));
        }
        count
    }

    fn key(&self, row: usize) -> Key {
        key(self.pid[row], self.creation[row])
    }

    /// Whether the tick holds another exit of the process of `row`, which leaves which was its
    /// last unknown.
    fn ambiguous(&self, row: usize) -> bool {
        if row > 0 {
            if self.key(row - 1) == self.key(row) {
                return true;
            }
        }
        if row + 1 < self.pid.len() {
            if self.key(row + 1) == self.key(row) {
                return true;
            }
        }
        false
    }
}

impl Usages<'_> {
    fn count(&self) -> usize {
        let count = self.measurement.len();
        assert_eq!(self.awake_ns_at_start.len(), count);
        assert_eq!(self.user_ns.len(), count);
        assert_eq!(self.system_ns.len(), count);
        assert_eq!(self.energy_nj.len(), count);
        assert_eq!(self.disk_read_bytes.len(), count);
        assert_eq!(self.disk_written_bytes.len(), count);
        assert_eq!(self.idle_wakeups.len(), count);
        assert_eq!(self.pageins.len(), count);
        assert_eq!(self.resident_bytes.len(), count);
        assert_eq!(self.footprint_bytes.len(), count);
        count
    }

    /// The usage of `row`, or why it was not read.
    fn row(&self, row: usize) -> Result<Usage, Unavailable> {
        if let Some(reason) = unmeasured(self.measurement[row]) {
            return Err(reason);
        }
        Ok(Usage {
            awake_ns_at_start: self.awake_ns_at_start[row],
            user_ns: self.user_ns[row],
            system_ns: self.system_ns[row],
            energy_nj: self.energy_nj[row],
            disk_read_bytes: self.disk_read_bytes[row],
            disk_written_bytes: self.disk_written_bytes[row],
            idle_wakeups: self.idle_wakeups[row],
            pageins: self.pageins[row],
            resident_bytes: self.resident_bytes[row],
            footprint_bytes: self.footprint_bytes[row],
        })
    }
}

/// The order of a tick's processes and exits: by pid, and then by unique id, a process
/// without its creation first.
type Key = (i32, Option<u64>);

fn key(pid: i32, creation: Option<Creation>) -> Key {
    (pid, creation.map(|creation| creation.unique_id))
}

/// One process of a tick as the rule reads it, live or at its exit.
struct Observation {
    pid: i32,
    creation: Option<Creation>,
    program: Result<u32, ProgramUnavailable>,
    usage: Result<Usage, Unavailable>,
    gpu_ns: Option<u64>,
    threads: Option<u64>,
    state: ProcessState,
}

/// What a process had used and held when the collector read its usage.
#[derive(Clone, Copy)]
struct Usage {
    awake_ns_at_start: u64,
    user_ns: u64,
    system_ns: u64,
    energy_nj: u64,
    disk_read_bytes: u64,
    disk_written_bytes: u64,
    idle_wakeups: u64,
    pageins: u64,
    resident_bytes: u64,
    footprint_bytes: u64,
}

/// What one process of the selected tick spent since the tick before, and what it held at
/// the tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProcessTotals {
    pub pid: i32,
    pub creation: Option<Creation>,
    pub program: Result<u32, ProgramUnavailable>,
    pub deltas: Deltas,
    /// Why no App takes what the process spent, where one does not.
    pub app_attribution: Option<Unavailable>,
    /// The process's memory when its usage was last read, at its exit or, where its exit was
    /// not read, at the tick before.
    pub resident_bytes_last: Option<u64>,
    pub footprint_bytes_last: Option<u64>,
    pub threads: Option<u64>,
    pub state: ProcessState,
}

/// Whether a process of the selected tick was live, or exited during the tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessState {
    Live,
    /// The process's exit read its final usage, which agrees with its live row where it has
    /// one.
    ExitedVerified,
    /// The process exited, and its final usage is not known.
    ExitedUnverified,
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
    /// The process exited, and its final usage is not known.
    ExitUnverified,
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

    /// Adds what the live member `selected` held at the tick.
    fn add_gauges(&mut self, selected: &Observation) {
        self.live_members += 1;
        self.resident_bytes
            .add(gauge(&selected.usage, |usage| usage.resident_bytes));
        self.footprint_bytes
            .add(gauge(&selected.usage, |usage| usage.footprint_bytes));
        self.threads.add(match selected.threads {
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
    /// What each process of the tick spent, live or exited, in order of pid and unique id.
    processes: Vec<ProcessTotals>,
    /// The totals of the Apps the tick's processes belong to.
    totals: Vec<Totals>,
    /// Each App's row in `totals`, or `SLOT_EMPTY` for an App no process of the tick runs.
    slots: Vec<u32>,
    /// The tick's processes whose program could not be read.
    ungrouped_members: u32,
    /// The ungrouped members live at the tick, which an exited process does not join.
    live_ungrouped_members: u32,
}

const SLOT_EMPTY: u32 = u32::MAX;

/// Which of the tick's processes and exits the rule reads next.
enum Next {
    Live,
    Exit,
    /// The exit of the live process the rule reads next.
    Both,
    Done,
}

impl Apps {
    #[must_use]
    pub fn new(processes_max: u32, programs: &Programs) -> Self {
        Self {
            processes: Vec::with_capacity(crate::usize_from_u32(processes_max)),
            totals: Vec::with_capacity(crate::usize_from_u32(processes_max)),
            slots: vec![SLOT_EMPTY; programs.apps_max()],
            ungrouped_members: 0,
            live_ungrouped_members: 0,
        }
    }

    /// Derives the Apps of `selected`, the tick after `previous`. The selected tick's processes
    /// and exits together fit the processes the Apps were made for.
    pub fn derive(&mut self, programs: &Programs, previous: &Tick, selected: &Tick) {
        let count = selected.processes.count();
        let exits = selected.exits.count();
        assert!(count + exits <= self.processes.capacity());
        assert!(previous.processes.count() <= self.processes.capacity());
        self.clear();

        let compatible = derive_compatible(&previous.stamp, &selected.stamp);
        let predecessor = if compatible {
            Some(&previous.processes)
        } else {
            None
        };
        let mut row = 0;
        let mut exit = 0;
        for _ in 0..count + exits {
            match derive_next(&selected.processes, row, &selected.exits, exit) {
                Next::Live => {
                    let live = selected.processes.observation(row);
                    self.derive_observation(programs, predecessor, &live);
                    row += 1;
                }
                Next::Exit => {
                    if let Some(ended) = derive_exit(predecessor, &selected.exits, exit, None) {
                        self.derive_observation(programs, predecessor, &ended);
                    }
                    exit += 1;
                }
                Next::Both => {
                    let live = selected.processes.observation(row);
                    let ended = derive_exit(predecessor, &selected.exits, exit, Some(&live));
                    self.derive_observation(programs, predecessor, ended.as_ref().unwrap_or(&live));
                    row += 1;
                    exit += 1;
                }
                Next::Done => break,
            }
        }
        assert_eq!(row, count);
        assert_eq!(exit, exits);
    }

    /// What each process of the tick last derived spent, live or exited, in order of pid and
    /// unique id.
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

    /// The processes of the tick last derived that belong to no App and are live at it.
    #[must_use]
    pub fn live_ungrouped_members(&self) -> u32 {
        self.live_ungrouped_members
    }

    fn clear(&mut self) {
        for totals in &self.totals {
            self.slots[totals.app.index()] = SLOT_EMPTY;
        }
        self.totals.clear();
        self.processes.clear();
        self.ungrouped_members = 0;
        self.live_ungrouped_members = 0;
    }

    /// Adds the process of `selected` to the tick's processes and to its App. `previous` is the
    /// tick before where the two span one interval.
    fn derive_observation(
        &mut self,
        programs: &Programs,
        previous: Option<&Processes>,
        selected: &Observation,
    ) {
        let before = previous.and_then(|previous| {
            let row = previous.find(selected.pid, selected.creation)?;
            Some(previous.observation(row))
        });
        let process = derive_process(previous.is_some(), before.as_ref(), selected);
        self.processes.push(process);

        let live = selected.state == ProcessState::Live;
        if let Ok(entry) = selected.program {
            let totals = self.totals_of(programs.app(entry));
            totals.add_deltas(&process);
            if live {
                totals.add_gauges(selected);
            }
        } else {
            self.ungrouped_members += 1;
            if live {
                self.live_ungrouped_members += 1;
            }
        }
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

/// Which of the live process of `row` and the exit of `exit` comes first in the tick's order.
fn derive_next(processes: &Processes, row: usize, exits: &Exits, exit: usize) -> Next {
    let live = row < processes.pid.len();
    let ended = exit < exits.pid.len();
    if !live {
        return if ended { Next::Exit } else { Next::Done };
    }
    if !ended {
        return Next::Live;
    }
    match processes.key(row).cmp(&exits.key(exit)) {
        std::cmp::Ordering::Less => Next::Live,
        std::cmp::Ordering::Greater => Next::Exit,
        std::cmp::Ordering::Equal => Next::Both,
    }
}

/// The exit of `exit` as the rule reads it, if it is taken: not where the tick holds another
/// exit of its process, nor where no reading names its program. `live` is the process's live
/// row, whose usage stands where the exit disagrees with it.
fn derive_exit(
    previous: Option<&Processes>,
    exits: &Exits,
    exit: usize,
    live: Option<&Observation>,
) -> Option<Observation> {
    if exits.ambiguous(exit) {
        return None;
    }
    let program = derive_exit_program(previous, exits.pid[exit], exits.creation[exit])?;

    let mut observation = Observation {
        pid: exits.pid[exit],
        creation: exits.creation[exit],
        program,
        usage: exits.usage.row(exit),
        gpu_ns: None,
        threads: None,
        state: ProcessState::ExitedVerified,
    };
    if observation.usage.is_err() {
        observation.state = ProcessState::ExitedUnverified;
    }
    if let Some(live) = live {
        if let Ok(scheduled) = live.usage {
            if !derive_exit_final(&observation.usage, &scheduled) {
                observation.usage = live.usage;
                observation.state = ProcessState::ExitedUnverified;
            }
        }
    }
    Some(observation)
}

/// The program the process of `pid` and `creation` ran when it exited, as the previous tick
/// names it, or why it is unknown.
fn derive_exit_program(
    previous: Option<&Processes>,
    pid: i32,
    creation: Option<Creation>,
) -> Option<Result<u32, ProgramUnavailable>> {
    let previous = previous?;
    let before = previous.find(pid, creation)?;
    if previous.creation[before] == creation {
        Some(previous.program[before])
    } else {
        Some(Err(ProgramUnavailable::Changed))
    }
}

/// Whether an exit's usage is the final usage of the process whose live row read `live`: of
/// the same start, with no CPU time or energy gone back.
fn derive_exit_final(exit: &Result<Usage, Unavailable>, live: &Usage) -> bool {
    let Ok(exit) = exit else {
        return false;
    };
    if exit.awake_ns_at_start != live.awake_ns_at_start {
        return false;
    }
    if exit.user_ns < live.user_ns {
        return false;
    }
    if exit.system_ns < live.system_ns {
        return false;
    }
    exit.energy_nj >= live.energy_nj
}

/// What the process of `selected` spent since `previous`, its observation at the tick before,
/// if that tick holds one and the two span one interval.
fn derive_process(
    compatible: bool,
    previous: Option<&Observation>,
    selected: &Observation,
) -> ProcessTotals {
    let mut process = ProcessTotals {
        pid: selected.pid,
        creation: selected.creation,
        program: selected.program,
        deltas: Deltas::unavailable(Unavailable::Baseline),
        app_attribution: None,
        resident_bytes_last: derive_last(previous, selected, |usage| usage.resident_bytes),
        footprint_bytes_last: derive_last(previous, selected, |usage| usage.footprint_bytes),
        threads: selected.threads,
        state: selected.state,
    };
    if !compatible {
        process.deltas = Deltas::unavailable(Unavailable::Interval);
        return process;
    }
    if selected.state == ProcessState::ExitedUnverified {
        process.deltas = Deltas::unavailable(Unavailable::ExitUnverified);
        return process;
    }
    let now = match selected.usage {
        Ok(now) => now,
        Err(reason) => {
            process.deltas = Deltas::unavailable(reason);
            return process;
        }
    };

    if let Some(previous) = previous {
        if !derive_same_program(previous, selected) {
            process.app_attribution = Some(Unavailable::Exec);
        }
        process.deltas = derive_deltas(previous, &now, selected.gpu_ns);
    }
    if selected.program.is_err() {
        process.app_attribution = Some(Unavailable::Program);
    }
    process
}

/// A gauge of the process when its usage was last read: at the selected tick, or, for a
/// process whose exit was not read, at the tick before.
fn derive_last(
    previous: Option<&Observation>,
    selected: &Observation,
    gauge: fn(&Usage) -> u64,
) -> Option<u64> {
    if let Ok(usage) = &selected.usage {
        return Some(gauge(usage));
    }
    if selected.state == ProcessState::Live {
        return None;
    }
    let previous = previous?;
    match &previous.usage {
        Ok(usage) => Some(gauge(usage)),
        Err(_) => None,
    }
}

/// Whether the process ran one program at both ticks, which its pid version and its program
/// tell.
fn derive_same_program(previous: &Observation, selected: &Observation) -> bool {
    if previous.creation != selected.creation {
        return false;
    }
    let Ok(program) = selected.program else {
        return false;
    };
    previous.program == Ok(program)
}

/// How much the process's counters grew from `previous` to `now`, its usage at the selected
/// tick beside `gpu_ns`, its GPU time then.
fn derive_deltas(previous: &Observation, now: &Usage, gpu_ns: Option<u64>) -> Deltas {
    let before = match &previous.usage {
        Ok(before) => before,
        Err(reason) => return Deltas::unavailable(*reason),
    };
    if before.awake_ns_at_start != now.awake_ns_at_start {
        return Deltas::unavailable(Unavailable::Creation);
    }

    let disk_read_bytes = change(before.disk_read_bytes, now.disk_read_bytes);
    let disk_written_bytes = change(before.disk_written_bytes, now.disk_written_bytes);
    Deltas {
        cpu_ns: derive_cpu_ns(before, now),
        energy_nj: change(before.energy_nj, now.energy_nj),
        gpu_ns: derive_gpu_ns(previous.gpu_ns, gpu_ns),
        disk_read_bytes,
        disk_written_bytes,
        disk_io_bytes: derive_disk_io_bytes(disk_read_bytes, disk_written_bytes),
        idle_wakeups: change(before.idle_wakeups, now.idle_wakeups),
        pageins: change(before.pageins, now.pageins),
    }
}

/// The CPU time the process spent from `before` to `now`.
fn derive_cpu_ns(before: &Usage, now: &Usage) -> Amount {
    let user_ns = now.user_ns.checked_sub(before.user_ns);
    let system_ns = now.system_ns.checked_sub(before.system_ns);
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
fn gauge(usage: &Result<Usage, Unavailable>, gauge: fn(&Usage) -> u64) -> Amount {
    match usage {
        Ok(usage) => Amount::Known(gauge(usage)),
        Err(reason) => Amount::Unavailable(*reason),
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
