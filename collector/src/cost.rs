use std::ffi::{c_int, c_uint};
use std::io;
use std::time::Duration;

use crate::clock::{self, Timebase};
use crate::kernel::Timespec;
use crate::processes;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Machine,
    Interfaces,
    Processes,
    Coalitions,
    Events,
    Scans,
    StartReads,
    Watches,
    ExitReads,
    IoReport,
    Smc,
}

impl Source {
    pub const ALL: [Source; 11] = [
        Source::Processes,
        Source::Coalitions,
        Source::Events,
        Source::Scans,
        Source::StartReads,
        Source::Watches,
        Source::ExitReads,
        Source::IoReport,
        Source::Smc,
        Source::Machine,
        Source::Interfaces,
    ];

    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Source::Machine => "machine",
            Source::Interfaces => "interfaces",
            Source::Processes => "processes",
            Source::Coalitions => "coalitions",
            Source::Events => "events",
            Source::Scans => "scans",
            Source::StartReads => "start reads",
            Source::Watches => "watches",
            Source::ExitReads => "exit reads",
            Source::IoReport => "ioreport",
            Source::Smc => "smc",
        }
    }

    /// The source's slot in an array that holds a value for each source.
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Source::Machine => 0,
            Source::Interfaces => 1,
            Source::Processes => 2,
            Source::Coalitions => 3,
            Source::Events => 4,
            Source::Scans => 5,
            Source::StartReads => 6,
            Source::Watches => 7,
            Source::ExitReads => 8,
            Source::IoReport => 9,
            Source::Smc => 10,
        }
    }
}

#[derive(Default)]
pub struct Meter {
    spent_ns: [u64; Source::ALL.len()],
}

impl Meter {
    pub fn measure<T>(&mut self, source: Source, read: impl FnOnce() -> T) -> T {
        let before = thread_cpu_ns();
        let result = read();
        self.spent_ns[source.index()] += thread_cpu_ns() - before;
        result
    }
}

const CLOCK_THREAD_CPUTIME_ID: c_uint = 16;

unsafe extern "C" {
    fn clock_gettime(clock: c_uint, time: *mut Timespec) -> c_int;
}

fn thread_cpu_ns() -> u64 {
    let mut now = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { clock_gettime(CLOCK_THREAD_CPUTIME_ID, &raw mut now) };
    u64::try_from(now.tv_sec).unwrap_or(0) * 1_000_000_000 + u64::try_from(now.tv_nsec).unwrap_or(0)
}

#[cfg(test)]
pub(crate) fn spin_on_this_thread_for(cpu_time: Duration) {
    let start = thread_cpu_ns();
    while Duration::from_nanos(thread_cpu_ns() - start) < cpu_time {
        std::hint::black_box(0);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    pub at_ns: u64,
    pub cpu_ns: u64,
    pub energy_nj: u64,
    pub instructions: u64,
    pub super_instructions: u64,
    pub spent_ns: [u64; Source::ALL.len()],
}

impl Snapshot {
    pub fn take(meter: &Meter, timebase: Timebase) -> io::Result<Self> {
        let own = match processes::read_usage(timebase, std::process::id().cast_signed())? {
            Ok(own) => own,
            Err(unreadable) => {
                return Err(io::Error::other(format!(
                    "cannot read this process's own usage: {unreadable:?}"
                )));
            }
        };

        Ok(Self {
            at_ns: timebase.nanos(clock::now()),
            cpu_ns: own.user_ns + own.system_ns,
            energy_nj: own.energy_nj,
            instructions: own.instructions,
            super_instructions: own.super_instructions,
            spent_ns: meter.spent_ns,
        })
    }

    #[must_use]
    pub fn cost_since(&self, earlier: &Snapshot) -> Cost {
        Cost {
            elapsed: Duration::from_nanos(self.at_ns - earlier.at_ns),
            cpu: Duration::from_nanos(self.cpu_ns - earlier.cpu_ns),
            energy_nj: self.energy_nj - earlier.energy_nj,
            instructions: self.instructions - earlier.instructions,
            super_instructions: self.super_instructions - earlier.super_instructions,
            spent: std::array::from_fn(|source| {
                Duration::from_nanos(self.spent_ns[source] - earlier.spent_ns[source])
            }),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Cost {
    elapsed: Duration,
    cpu: Duration,
    energy_nj: u64,
    instructions: u64,
    super_instructions: u64,
    spent: [Duration; Source::ALL.len()],
}

impl Cost {
    #[must_use]
    pub fn share_of_a_core(&self) -> f64 {
        self.cpu.div_duration_f64(self.elapsed)
    }

    #[must_use]
    pub fn milliwatts(&self) -> f64 {
        ratio(u128::from(self.energy_nj), self.elapsed.as_nanos()) * 1_000.0
    }

    #[must_use]
    pub fn share_on_super_cores(&self) -> f64 {
        ratio(
            u128::from(self.super_instructions),
            u128::from(self.instructions),
        )
    }

    #[must_use]
    pub fn ms_per_second(&self, source: Source) -> f64 {
        self.spent[source.index()].div_duration_f64(self.elapsed) * 1_000.0
    }

    #[must_use]
    pub fn unattributed_ms_per_second(&self) -> f64 {
        let attributed: Duration = self.spent.iter().sum();
        self.cpu
            .saturating_sub(attributed)
            .div_duration_f64(self.elapsed)
            * 1_000.0
    }
}

fn ratio(part: u128, whole: u128) -> f64 {
    let parts_per_million = part * 1_000_000 / whole.max(1);
    f64::from(u32::try_from(parts_per_million).unwrap_or(u32::MAX)) / 1_000_000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ten_seconds_later(cpu_ns: u64, energy_nj: u64, processes_ns: u64) -> Cost {
        let earlier = Snapshot {
            at_ns: 5_000_000_000,
            cpu_ns: 1_000,
            energy_nj: 2_000,
            spent_ns: [7; Source::ALL.len()],
            instructions: 500,
            super_instructions: 100,
        };
        let mut spent_ns = [7; Source::ALL.len()];
        spent_ns[Source::Processes.index()] += processes_ns;
        let later = Snapshot {
            at_ns: 15_000_000_000,
            cpu_ns: 1_000 + cpu_ns,
            energy_nj: 2_000 + energy_nj,
            spent_ns,
            instructions: 500 + 4_000,
            super_instructions: 100 + 3_000,
        };

        later.cost_since(&earlier)
    }

    #[test]
    fn gives_the_cpu_spent_as_a_share_of_one_core() {
        let cost = ten_seconds_later(100_000_000, 0, 0);

        assert!((cost.share_of_a_core() - 0.01).abs() < 1e-12);
    }

    #[test]
    fn gives_the_energy_spent_as_average_power() {
        let cost = ten_seconds_later(0, 25_000_000, 0);

        assert!((cost.milliwatts() - 2.5).abs() < 1e-12);
    }

    #[test]
    fn gives_each_source_and_the_rest_in_milliseconds_a_second() {
        let cost = ten_seconds_later(100_000_000, 0, 73_000_000);

        assert!((cost.ms_per_second(Source::Processes) - 7.3).abs() < 1e-12);
        assert!((cost.ms_per_second(Source::Coalitions)).abs() < 1e-12);
        assert!((cost.unattributed_ms_per_second() - 2.7).abs() < 1e-12);
    }

    #[test]
    fn gives_the_share_of_instructions_run_on_super_cores() {
        let cost = ten_seconds_later(0, 0, 0);

        assert!((cost.share_on_super_cores() - 0.75).abs() < 1e-12);
    }

    #[test]
    fn index_gives_each_source_its_own_slot() {
        // Goal: every source indexes a slot of a per-source array, and no two share one.
        // Method: mark the slot of each source in turn, failing on a slot marked twice.
        let mut taken = [false; Source::ALL.len()];

        for source in Source::ALL {
            let index = source.index();
            assert!(index < taken.len(), "{source:?}");
            assert!(!taken[index], "{source:?}");
            taken[index] = true;
        }
    }

    #[test]
    fn charges_a_measured_read_to_its_source() {
        let mut meter = Meter::default();

        meter.measure(Source::Coalitions, || {
            spin_on_this_thread_for(Duration::from_millis(20));
        });

        let spent = Snapshot::take(&meter, Timebase::read()).unwrap().spent_ns;
        assert!(spent[Source::Coalitions.index()] >= 20_000_000);
        assert_eq!(0, spent[Source::Processes.index()]);
    }
}
