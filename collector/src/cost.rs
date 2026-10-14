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
    IOReport,
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
        Source::IOReport,
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
            Source::IOReport => "ioreport",
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
            Source::IOReport => 9,
            Source::Smc => 10,
        }
    }
}

#[derive(Default)]
pub struct Meter {
    spent_ns: [u64; Source::ALL.len()],
}

impl Meter {
    /// Runs `read`, charges the CPU time this thread spent on it to `source`, and returns
    /// what it returned.
    pub fn measure<T>(
        &mut self,
        source: Source,
        read: impl FnOnce() -> io::Result<T>,
    ) -> io::Result<T> {
        let before = thread_cpu_ns()?;
        let result = read()?;
        let after = thread_cpu_ns()?;

        let Some(spent) = after.checked_sub(before) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("this thread's CPU time went back from {before} ns to {after} ns"),
            ));
        };
        self.spent_ns[source.index()] += spent;
        Ok(result)
    }
}

const CLOCK_THREAD_CPUTIME_ID: c_uint = 16;

unsafe extern "C" {
    fn clock_gettime(clock: c_uint, time: *mut Timespec) -> c_int;
}

fn thread_cpu_ns() -> io::Result<u64> {
    clock_ns(CLOCK_THREAD_CPUTIME_ID)
}

/// Reads `clock` in nanoseconds.
fn clock_ns(clock: c_uint) -> io::Result<u64> {
    let mut now = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let returned = unsafe { clock_gettime(clock, &raw mut now) };
    if returned != 0 {
        return Err(io::Error::last_os_error());
    }

    clock_ns_from(now)
}

/// Returns `time` in nanoseconds, or an error where it is negative or a u64 cannot hold it.
fn clock_ns_from(time: Timespec) -> io::Result<u64> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a clock gave {} s and {} ns", time.tv_sec, time.tv_nsec),
        )
    };
    let seconds = u64::try_from(time.tv_sec).map_err(|_| invalid())?;
    let nanos = u64::try_from(time.tv_nsec).map_err(|_| invalid())?;

    seconds
        .checked_mul(1_000_000_000)
        .and_then(|seconds_ns| seconds_ns.checked_add(nanos))
        .ok_or_else(invalid)
}

#[cfg(test)]
pub(crate) fn spin_on_this_thread_for(cpu_time: Duration) {
    let start = thread_cpu_ns().unwrap();
    while Duration::from_nanos(thread_cpu_ns().unwrap() - start) < cpu_time {
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
        let own = match processes::read_usage(timebase, processes::own_pid()?)? {
            Ok(own) => own,
            Err(unreadable) => {
                return Err(io::Error::other(format!(
                    "cannot read this process's own usage: {unreadable:?}"
                )));
            }
        };

        Ok(Self {
            at_ns: timebase.nanos(clock::now())?,
            cpu_ns: own.user_ns + own.system_ns,
            energy_nj: own.energy_nj,
            instructions: own.instructions,
            super_instructions: own.super_instructions,
            spent_ns: meter.spent_ns,
        })
    }

    /// Returns what this process spent from `earlier` to this snapshot, or an error where a
    /// counter macOS gives went back.
    pub fn cost_since(&self, earlier: &Snapshot) -> io::Result<Cost> {
        let elapsed_ns = self.cost_since_counter(earlier, "time", |snapshot| snapshot.at_ns)?;
        let cpu_ns = self.cost_since_counter(earlier, "CPU time", |snapshot| snapshot.cpu_ns)?;
        let energy_nj =
            self.cost_since_counter(earlier, "energy", |snapshot| snapshot.energy_nj)?;
        let instructions =
            self.cost_since_counter(earlier, "instructions", |snapshot| snapshot.instructions)?;
        let super_instructions =
            self.cost_since_counter(earlier, "super core instructions", |snapshot| {
                snapshot.super_instructions
            })?;
        let spent = std::array::from_fn(|source| {
            assert!(self.spent_ns[source] >= earlier.spent_ns[source]);
            Duration::from_nanos(self.spent_ns[source] - earlier.spent_ns[source])
        });

        Ok(Cost {
            elapsed: Duration::from_nanos(elapsed_ns),
            cpu: Duration::from_nanos(cpu_ns),
            energy_nj,
            instructions,
            super_instructions,
            spent,
        })
    }

    /// Returns how far the counter that `read` reads, which `name` names, grew from `earlier`
    /// to this snapshot.
    fn cost_since_counter(
        &self,
        earlier: &Snapshot,
        name: &str,
        read: fn(&Snapshot) -> u64,
    ) -> io::Result<u64> {
        let before = read(earlier);
        let after = read(self);
        after.checked_sub(before).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("this process's {name} went back from {before} to {after}"),
            )
        })
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

    /// The power spent, as nanojoules a second divided by the million of them in a milliwatt.
    #[must_use]
    pub fn milliwatts(&self) -> f64 {
        f64_from_u64(self.energy_nj) / self.elapsed.as_secs_f64() / 1_000_000.0
    }

    #[must_use]
    pub fn share_on_super_cores(&self) -> f64 {
        f64_from_u64(self.super_instructions) / f64_from_u64(self.instructions)
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

/// Converts `value` to the nearest f64, which `f64::from` does not do for a u64, from its two
/// halves, each of which an f64 holds exactly.
fn f64_from_u64(value: u64) -> f64 {
    let high = match u32::try_from(value >> 32) {
        Ok(high) => high,
        Err(error) => unreachable!("{error}"),
    };
    let low = match u32::try_from(value & u64::from(u32::MAX)) {
        Ok(low) => low,
        Err(error) => unreachable!("{error}"),
    };

    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EARLIER: Snapshot = Snapshot {
        at_ns: 5_000_000_000,
        cpu_ns: 1_000,
        energy_nj: 2_000,
        spent_ns: [7; Source::ALL.len()],
        instructions: 500,
        super_instructions: 100,
    };

    fn ten_seconds_later(cpu_ns: u64, energy_nj: u64, processes_ns: u64) -> Cost {
        let mut spent_ns = EARLIER.spent_ns;
        spent_ns[Source::Processes.index()] += processes_ns;
        let later = Snapshot {
            at_ns: EARLIER.at_ns + 10_000_000_000,
            cpu_ns: EARLIER.cpu_ns + cpu_ns,
            energy_nj: EARLIER.energy_nj + energy_nj,
            spent_ns,
            instructions: EARLIER.instructions + 4_000,
            super_instructions: EARLIER.super_instructions + 3_000,
        };

        later.cost_since(&EARLIER).unwrap()
    }

    #[test]
    fn refuses_a_cost_over_a_counter_that_went_back() {
        // Goal: a counter of this process that macOS gives lower than before is an error,
        // never a subtraction that traps.
        // Method: take the cost to a snapshot whose energy is a nanojoule below the earlier
        // one's.
        let later = Snapshot {
            at_ns: EARLIER.at_ns + 1,
            energy_nj: EARLIER.energy_nj - 1,
            ..EARLIER
        };

        let cost = later.cost_since(&EARLIER);

        assert_eq!(io::ErrorKind::InvalidData, cost.unwrap_err().kind());
    }

    #[test]
    fn gives_the_cpu_spent_as_a_share_of_one_core() {
        // Goal: CPU time reads as the share of one core it took over the span.
        // Method: spend 0.1 s of CPU in ten seconds, a hundredth of a core.
        let cost = ten_seconds_later(100_000_000, 0, 0);

        assert!((cost.share_of_a_core() - 0.01).abs() < 1e-12);
    }

    #[test]
    fn gives_the_energy_spent_as_average_power() {
        // Goal: energy reads as the power it averaged over the span.
        // Method: spend 25 mJ in ten seconds, 2.5 mW.
        let cost = ten_seconds_later(0, 25_000_000, 0);

        assert!((cost.milliwatts() - 2.5).abs() < 1e-12);
    }

    #[test]
    fn gives_power_past_what_a_millionth_in_a_u32_holds() {
        // Goal: power reads as what was spent, however large, never as a cap.
        // Method: spend 50 kJ in ten seconds, 5 kW, past the 4,295 W that parts per million
        // in a u32 hold.
        let cost = ten_seconds_later(0, 50_000_000_000_000, 0);

        assert!(
            (cost.milliwatts() - 5_000_000.0).abs() < 1e-6,
            "{}",
            cost.milliwatts()
        );
    }

    #[test]
    fn gives_no_share_where_no_instructions_ran() {
        // Goal: a span that ran no instructions gives no share on super cores, never a made-up
        // share of 0.
        // Method: share no instructions on super cores among none.
        let cost = Cost {
            instructions: 0,
            super_instructions: 0,
            ..ten_seconds_later(0, 0, 0)
        };

        assert!(cost.share_on_super_cores().is_nan());
    }

    #[test]
    fn converts_a_u64_to_the_nearest_f64() {
        // Goal: a u64 past the 2^53 an f64 holds exactly converts to the nearest f64.
        // Method: convert the largest u64, 2^64 - 1, whose nearest f64 is 2^64.
        let converted = f64_from_u64(u64::MAX);

        assert_eq!(2_f64.powi(64).to_bits(), converted.to_bits());
    }

    #[test]
    fn gives_each_source_and_the_rest_in_milliseconds_a_second() {
        // Goal: each source's time reads as milliseconds a second, and so does the CPU time
        // no source took.
        // Method: spend 0.1 s of CPU in ten seconds, 73 ms of it on processes and none on
        // coalitions.
        let cost = ten_seconds_later(100_000_000, 0, 73_000_000);

        assert!((cost.ms_per_second(Source::Processes) - 7.3).abs() < 1e-12);
        assert!((cost.ms_per_second(Source::Coalitions)).abs() < 1e-12);
        assert!((cost.unattributed_ms_per_second() - 2.7).abs() < 1e-12);
    }

    #[test]
    fn gives_the_share_of_instructions_run_on_super_cores() {
        // Goal: super core instructions read as their share of every instruction run.
        // Method: run 3,000 of 4,000 instructions on super cores.
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
    fn refuses_a_clock_macos_does_not_have() {
        // Goal: a clock that clock_gettime refuses is an error, never a time of 0.
        // Method: read clock 99, which clock_gettime refused with -1 on macOS 27.0.1.
        let read = clock_ns(99);

        assert_eq!(io::ErrorKind::InvalidInput, read.unwrap_err().kind());
    }

    #[test]
    fn reads_a_time_in_nanoseconds() {
        // Goal: a time of seconds and nanoseconds reads as nanoseconds.
        // Method: read 2 s and 5 ns.
        let time = Timespec {
            tv_sec: 2,
            tv_nsec: 5,
        };

        assert_eq!(2_000_000_005, clock_ns_from(time).unwrap());
    }

    #[test]
    fn refuses_a_negative_time() {
        // Goal: a clock that gives a negative time is an error, since no time a thread spent
        // is negative.
        // Method: read -1 s.
        let time = Timespec {
            tv_sec: -1,
            tv_nsec: 0,
        };

        let read = clock_ns_from(time);

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_time_past_the_nanoseconds_a_u64_holds() {
        // Goal: a time whose nanoseconds a u64 cannot hold is an error, never a made-up time.
        // Method: read the most seconds an i64 holds.
        let time = Timespec {
            tv_sec: i64::MAX,
            tv_nsec: 0,
        };

        let read = clock_ns_from(time);

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }

    #[test]
    fn charges_a_measured_read_to_its_source() {
        // Goal: the CPU time a measured read spends is charged to its source and no other.
        // Method: spin 20 ms of CPU inside a read measured as coalitions.
        let mut meter = Meter::default();

        meter
            .measure(Source::Coalitions, || {
                spin_on_this_thread_for(Duration::from_millis(20));
                Ok(())
            })
            .unwrap();

        let spent = Snapshot::take(&meter, Timebase::read().unwrap())
            .unwrap()
            .spent_ns;
        assert!(spent[Source::Coalitions.index()] >= 20_000_000);
        assert_eq!(0, spent[Source::Processes.index()]);
    }
}
