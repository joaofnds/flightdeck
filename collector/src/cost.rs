use std::time::Duration;

use crate::clock::{self, Timebase};
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
}

#[derive(Default)]
pub struct Meter {
    spent_ns: [u64; Source::ALL.len()],
}

impl Meter {
    pub fn measure<T>(&mut self, source: Source, read: impl FnOnce() -> T) -> T {
        let before = thread_cpu_ns();
        let result = read();
        self.spent_ns[source as usize] += thread_cpu_ns() - before;
        result
    }
}

fn thread_cpu_ns() -> u64 {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &raw mut now) };
    u64::try_from(now.tv_sec).unwrap_or(0) * 1_000_000_000 + u64::try_from(now.tv_nsec).unwrap_or(0)
}

#[cfg(test)]
pub(crate) fn spin_on_this_thread_for(cpu_time: Duration) {
    let start = thread_cpu_ns();
    while Duration::from_nanos(thread_cpu_ns() - start) < cpu_time {
        std::hint::black_box(0);
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Snapshot {
    pub at_ns: u64,
    pub cpu_ns: u64,
    pub energy_nj: u64,
    pub instructions: u64,
    pub super_instructions: u64,
    pub spent_ns: [u64; Source::ALL.len()],
}

impl Snapshot {
    #[must_use]
    pub fn take(meter: &Meter, timebase: Timebase) -> Self {
        let own = processes::read_usage(std::process::id().cast_signed(), timebase)
            .expect("a process can read its own usage");

        Self {
            at_ns: timebase.nanos(clock::now()),
            cpu_ns: own.user_ns + own.system_ns,
            energy_nj: own.energy_nj,
            instructions: own.instructions,
            super_instructions: own.super_instructions,
            spent_ns: meter.spent_ns,
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
    pub fn between(earlier: &Snapshot, later: &Snapshot) -> Self {
        Self {
            elapsed: Duration::from_nanos(later.at_ns - earlier.at_ns),
            cpu: Duration::from_nanos(later.cpu_ns - earlier.cpu_ns),
            energy_nj: later.energy_nj - earlier.energy_nj,
            instructions: later.instructions - earlier.instructions,
            super_instructions: later.super_instructions - earlier.super_instructions,
            spent: std::array::from_fn(|source| {
                Duration::from_nanos(later.spent_ns[source] - earlier.spent_ns[source])
            }),
        }
    }

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
        self.spent[source as usize].div_duration_f64(self.elapsed) * 1_000.0
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
        spent_ns[Source::Processes as usize] += processes_ns;
        let later = Snapshot {
            at_ns: 15_000_000_000,
            cpu_ns: 1_000 + cpu_ns,
            energy_nj: 2_000 + energy_nj,
            spent_ns,
            instructions: 500 + 4_000,
            super_instructions: 100 + 3_000,
        };

        Cost::between(&earlier, &later)
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
    fn charges_a_measured_read_to_its_source() {
        let mut meter = Meter::default();

        meter.measure(Source::Coalitions, || {
            spin_on_this_thread_for(Duration::from_millis(20));
        });

        let spent = Snapshot::take(&meter, Timebase::read()).spent_ns;
        assert!(spent[Source::Coalitions as usize] >= 20_000_000);
        assert_eq!(0, spent[Source::Processes as usize]);
    }
}
