//! One tick's columns in buffers taken at startup, which a source's adapter fills a row at a time
//! and the App rule reads as a `Tick`.
use crate::apps::{
    Creation, Execs, Exits, Measurement, Processes, ProgramUnavailable, Stamp, Starts, Tick, Usage,
    Usages,
};

/// The columns of one tick, with room for the processes and events of the largest tick its
/// source holds.
pub struct TickColumns {
    stamp: Stamp,
    processes_max: usize,
    events_max: usize,
    processes: ProcessColumns,
    exits: ExitColumns,
    starts: EventColumns,
    execs: EventColumns,
    /// The tick's events before `finish` puts them in the rule's orders.
    exit_rows: Vec<ExitRow>,
    start_rows: Vec<EventRow>,
    exec_rows: Vec<EventRow>,
}

/// A live process of a tick.
#[derive(Clone, Copy)]
pub struct ProcessRow {
    pub pid: i32,
    pub creation: Option<Creation>,
    pub program: Result<u32, ProgramUnavailable>,
    pub usage: Result<Usage, Measurement>,
    pub gpu_ns: Option<u64>,
    pub threads: Option<u64>,
}

/// A process that exited since the tick before, with its usage as its exit read it last.
#[derive(Clone, Copy)]
pub struct ExitRow {
    pub number: u64,
    pub pid: i32,
    pub creation: Option<Creation>,
    pub usage: Result<Usage, Measurement>,
}

/// A process that started, or ran another program, since the tick before.
#[derive(Clone, Copy)]
pub struct EventRow {
    pub number: u64,
    pub pid: i32,
    pub creation: Option<Creation>,
    pub program: Result<u32, ProgramUnavailable>,
}

/// Why a process was not added to the tick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// The tick holds as many processes as the columns have room for.
    Full,
    /// The process's pid is not above the pid of the process added before it.
    Order,
}

/// The refusal of an event: the tick holds as many events of its kind as the columns have
/// room for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Full;

struct ProcessColumns {
    pid: Vec<i32>,
    creation: Vec<Option<Creation>>,
    program: Vec<Result<u32, ProgramUnavailable>>,
    usage: UsageColumns,
    gpu_ns: Vec<Option<u64>>,
    threads: Vec<Option<u64>>,
}

struct ExitColumns {
    number: Vec<u64>,
    pid: Vec<i32>,
    creation: Vec<Option<Creation>>,
    usage: UsageColumns,
}

struct EventColumns {
    number: Vec<u64>,
    pid: Vec<i32>,
    creation: Vec<Option<Creation>>,
    program: Vec<Result<u32, ProgramUnavailable>>,
}

struct UsageColumns {
    measurement: Vec<Measurement>,
    awake_ns_at_start: Vec<u64>,
    user_ns: Vec<u64>,
    system_ns: Vec<u64>,
    energy_nj: Vec<u64>,
    disk_read_bytes: Vec<u64>,
    disk_written_bytes: Vec<u64>,
    idle_wakeups: Vec<u64>,
    pageins: Vec<u64>,
    resident_bytes: Vec<u64>,
    footprint_bytes: Vec<u64>,
}

impl TickColumns {
    /// Columns with room for `processes_max` live processes and `events_max` events of each
    /// kind a tick.
    #[must_use]
    pub fn new(processes_max: u32, events_max: u32) -> Self {
        let processes = crate::usize_from_u32(processes_max);
        let events = crate::usize_from_u32(events_max);
        Self {
            stamp: EMPTY,
            processes_max: processes,
            events_max: events,
            processes: ProcessColumns {
                pid: Vec::with_capacity(processes),
                creation: Vec::with_capacity(processes),
                program: Vec::with_capacity(processes),
                usage: UsageColumns::new(processes),
                gpu_ns: Vec::with_capacity(processes),
                threads: Vec::with_capacity(processes),
            },
            exits: ExitColumns {
                number: Vec::with_capacity(events),
                pid: Vec::with_capacity(events),
                creation: Vec::with_capacity(events),
                usage: UsageColumns::new(events),
            },
            starts: EventColumns::new(events),
            execs: EventColumns::new(events),
            exit_rows: Vec::with_capacity(events),
            start_rows: Vec::with_capacity(events),
            exec_rows: Vec::with_capacity(events),
        }
    }

    /// Empties the columns for the tick of `stamp`.
    pub fn start(&mut self, stamp: Stamp) {
        self.clear();
        self.stamp = stamp;
    }

    /// Empties the columns, which then hold a tick of no run that no tick follows.
    pub fn clear(&mut self) {
        self.stamp = EMPTY;
        self.processes.clear();
        self.exits.clear();
        self.starts.clear();
        self.execs.clear();
        self.exit_rows.clear();
        self.start_rows.clear();
        self.exec_rows.clear();
    }

    /// Adds the live process of `row`, whose pid is above the pid of the one added before it.
    pub fn push_process(&mut self, row: &ProcessRow) -> Result<(), Refusal> {
        if self.processes.pid.len() == self.processes_max {
            return Err(Refusal::Full);
        }
        if let Some(&previous) = self.processes.pid.last() {
            if previous >= row.pid {
                return Err(Refusal::Order);
            }
        }

        self.processes.pid.push(row.pid);
        self.processes.creation.push(row.creation);
        self.processes.program.push(row.program);
        self.processes.usage.push(row.usage);
        self.processes.gpu_ns.push(row.gpu_ns);
        self.processes.threads.push(row.threads);
        Ok(())
    }

    /// Adds the exit of `row`, in any order among the tick's exits.
    pub fn push_exit(&mut self, row: &ExitRow) -> Result<(), Full> {
        push_row(&mut self.exit_rows, self.events_max, *row)
    }

    /// Adds the start of `row`, in any order among the tick's starts.
    pub fn push_start(&mut self, row: &EventRow) -> Result<(), Full> {
        push_row(&mut self.start_rows, self.events_max, *row)
    }

    /// Adds the exec of `row`, in any order among the tick's execs.
    pub fn push_exec(&mut self, row: &EventRow) -> Result<(), Full> {
        push_row(&mut self.exec_rows, self.events_max, *row)
    }

    /// Puts the tick's events in the orders the App rule reads them in, once every row is added.
    pub fn finish(&mut self) {
        assert!(self.exits.pid.is_empty());
        assert!(self.starts.pid.is_empty());
        assert!(self.execs.pid.is_empty());

        self.exit_rows
            .sort_unstable_by_key(|row| (row.pid, unique_id(row.creation), row.number));
        for row in &self.exit_rows {
            self.exits.number.push(row.number);
            self.exits.pid.push(row.pid);
            self.exits.creation.push(row.creation);
            self.exits.usage.push(row.usage);
        }

        self.start_rows
            .sort_unstable_by_key(|row| (row.pid, unique_id(row.creation), row.number));
        self.starts.extend(&self.start_rows);

        self.exec_rows
            .sort_unstable_by_key(|row| (row.pid, row.number));
        self.execs.extend(&self.exec_rows);
    }

    /// The tick the columns hold, as the App rule reads it.
    #[must_use]
    pub fn tick(&self) -> Tick<'_> {
        assert_eq!(self.exits.pid.len(), self.exit_rows.len());
        assert_eq!(self.starts.pid.len(), self.start_rows.len());
        assert_eq!(self.execs.pid.len(), self.exec_rows.len());

        Tick {
            stamp: self.stamp,
            processes: Processes {
                pid: &self.processes.pid,
                creation: &self.processes.creation,
                program: &self.processes.program,
                usage: self.processes.usage.usages(),
                gpu_ns: &self.processes.gpu_ns,
                threads: &self.processes.threads,
            },
            exits: Exits {
                number: &self.exits.number,
                pid: &self.exits.pid,
                creation: &self.exits.creation,
                usage: self.exits.usage.usages(),
            },
            starts: self.starts.events(),
            execs: Execs {
                number: &self.execs.number,
                pid: &self.execs.pid,
                creation: &self.execs.creation,
                program: &self.execs.program,
            },
        }
    }

    /// The live processes the columns have room for.
    #[must_use]
    pub fn processes_max(&self) -> usize {
        self.processes_max
    }
}

/// The stamp of columns that hold no tick: of no run, and published by no tick.
const EMPTY: Stamp = Stamp {
    recording: [0; 2],
    boot: [0; 2],
    run: [0; 2],
    publication: 0,
    continuous_ns: None,
};

/// The values a row of usage the collector did not read holds, which the rule never reads.
const UNREAD: Usage = Usage {
    awake_ns_at_start: 0,
    user_ns: 0,
    system_ns: 0,
    energy_nj: 0,
    disk_read_bytes: 0,
    disk_written_bytes: 0,
    idle_wakeups: 0,
    pageins: 0,
    resident_bytes: 0,
    footprint_bytes: 0,
};

fn push_row<Row>(rows: &mut Vec<Row>, rows_max: usize, row: Row) -> Result<(), Full> {
    if rows.len() == rows_max {
        return Err(Full);
    }
    rows.push(row);
    Ok(())
}

/// The unique id the rule orders a tick's exits and starts by after their pid, an event
/// without its creation first.
fn unique_id(creation: Option<Creation>) -> Option<u64> {
    creation.map(|creation| creation.unique_id)
}

impl ProcessColumns {
    fn clear(&mut self) {
        self.pid.clear();
        self.creation.clear();
        self.program.clear();
        self.usage.clear();
        self.gpu_ns.clear();
        self.threads.clear();
    }
}

impl ExitColumns {
    fn clear(&mut self) {
        self.number.clear();
        self.pid.clear();
        self.creation.clear();
        self.usage.clear();
    }
}

impl EventColumns {
    fn new(capacity: usize) -> Self {
        Self {
            number: Vec::with_capacity(capacity),
            pid: Vec::with_capacity(capacity),
            creation: Vec::with_capacity(capacity),
            program: Vec::with_capacity(capacity),
        }
    }

    fn extend(&mut self, rows: &[EventRow]) {
        for row in rows {
            self.number.push(row.number);
            self.pid.push(row.pid);
            self.creation.push(row.creation);
            self.program.push(row.program);
        }
    }

    fn clear(&mut self) {
        self.number.clear();
        self.pid.clear();
        self.creation.clear();
        self.program.clear();
    }

    fn events(&self) -> Starts<'_> {
        Starts {
            number: &self.number,
            pid: &self.pid,
            creation: &self.creation,
            program: &self.program,
        }
    }
}

impl UsageColumns {
    fn new(capacity: usize) -> Self {
        Self {
            measurement: Vec::with_capacity(capacity),
            awake_ns_at_start: Vec::with_capacity(capacity),
            user_ns: Vec::with_capacity(capacity),
            system_ns: Vec::with_capacity(capacity),
            energy_nj: Vec::with_capacity(capacity),
            disk_read_bytes: Vec::with_capacity(capacity),
            disk_written_bytes: Vec::with_capacity(capacity),
            idle_wakeups: Vec::with_capacity(capacity),
            pageins: Vec::with_capacity(capacity),
            resident_bytes: Vec::with_capacity(capacity),
            footprint_bytes: Vec::with_capacity(capacity),
        }
    }

    /// Adds a row of `usage`, or of zeros with the reason it was not read.
    fn push(&mut self, usage: Result<Usage, Measurement>) {
        let (measurement, usage) = match usage {
            Ok(usage) => (Measurement::Measured, usage),
            Err(reason) => {
                assert_ne!(reason, Measurement::Measured);
                (reason, UNREAD)
            }
        };
        self.measurement.push(measurement);
        self.awake_ns_at_start.push(usage.awake_ns_at_start);
        self.user_ns.push(usage.user_ns);
        self.system_ns.push(usage.system_ns);
        self.energy_nj.push(usage.energy_nj);
        self.disk_read_bytes.push(usage.disk_read_bytes);
        self.disk_written_bytes.push(usage.disk_written_bytes);
        self.idle_wakeups.push(usage.idle_wakeups);
        self.pageins.push(usage.pageins);
        self.resident_bytes.push(usage.resident_bytes);
        self.footprint_bytes.push(usage.footprint_bytes);
    }

    fn clear(&mut self) {
        self.measurement.clear();
        self.awake_ns_at_start.clear();
        self.user_ns.clear();
        self.system_ns.clear();
        self.energy_nj.clear();
        self.disk_read_bytes.clear();
        self.disk_written_bytes.clear();
        self.idle_wakeups.clear();
        self.pageins.clear();
        self.resident_bytes.clear();
        self.footprint_bytes.clear();
    }

    fn usages(&self) -> Usages<'_> {
        Usages {
            measurement: &self.measurement,
            awake_ns_at_start: &self.awake_ns_at_start,
            user_ns: &self.user_ns,
            system_ns: &self.system_ns,
            energy_nj: &self.energy_nj,
            disk_read_bytes: &self.disk_read_bytes,
            disk_written_bytes: &self.disk_written_bytes,
            idle_wakeups: &self.idle_wakeups,
            pageins: &self.pageins,
            resident_bytes: &self.resident_bytes,
            footprint_bytes: &self.footprint_bytes,
        }
    }
}

#[cfg(test)]
mod tests;
