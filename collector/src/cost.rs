use std::ffi::{c_int, c_uint};
use std::io;
use std::time::Duration;

use crate::clock::{self, Timebase};
use crate::kernel::Timespec;
use crate::processes::{self, Usage};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Machine,
    Interfaces,
    Processes,
    Coalitions,
    Events,
    Scans,
    StartReads,
    ExecReads,
    Watches,
    ExitReads,
    IOReport,
    Smc,
    /// Publishing each tick to the ring.
    Ring,
    Battery,
    Disks,
    /// The load average, open files, thermal pressure, GPU memory and boot time.
    System,
    /// Each process's GPU time, from the GPU driver's clients.
    GpuClients,
}

impl Source {
    pub const ALL: [Source; 17] = [
        Source::Processes,
        Source::Coalitions,
        Source::Events,
        Source::Scans,
        Source::StartReads,
        Source::ExecReads,
        Source::Watches,
        Source::ExitReads,
        Source::IOReport,
        Source::Smc,
        Source::Machine,
        Source::Interfaces,
        Source::Ring,
        Source::Battery,
        Source::Disks,
        Source::System,
        Source::GpuClients,
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
            Source::ExecReads => "exec reads",
            Source::Watches => "watches",
            Source::ExitReads => "exit reads",
            Source::IOReport => "ioreport",
            Source::Smc => "smc",
            Source::Ring => "ring",
            Source::Battery => "battery",
            Source::Disks => "disks",
            Source::System => "system",
            Source::GpuClients => "gpu clients",
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
            Source::Ring => 11,
            Source::Battery => 12,
            Source::Disks => 13,
            Source::ExecReads => 14,
            Source::System => 15,
            Source::GpuClients => 16,
        }
    }
}

// Each source takes a slot of its own, so no read is charged to another source's slot.
const _: () = {
    let mut taken = [false; Source::ALL.len()];
    let mut position = 0;
    while position < Source::ALL.len() {
        let index = Source::ALL[position].index();
        assert!(!taken[index]);
        taken[index] = true;
        position += 1;
    }
};

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
        let index = source.index();
        let Some(charged) = self.spent_ns[index].checked_add(spent) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "{spent} ns more charged to {} pass what a u64 holds",
                    source.name()
                ),
            ));
        };
        self.spent_ns[index] = charged;
        Ok(result)
    }
}

const CLOCK_THREAD_CPUTIME_ID: c_uint = 16;

unsafe extern "C" {
    fn clock_gettime(clock: c_uint, time: *mut Timespec) -> c_int;
}

fn thread_cpu_ns() -> io::Result<u64> {
    thread_cpu_ns_of(CLOCK_THREAD_CPUTIME_ID)
}

/// Reads `clock` in nanoseconds.
fn thread_cpu_ns_of(clock: c_uint) -> io::Result<u64> {
    let mut now = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let returned = unsafe { clock_gettime(clock, &raw mut now) };
    if returned != 0 {
        return Err(io::Error::last_os_error());
    }

    thread_cpu_ns_from(now)
}

/// Returns `time` in nanoseconds, or an error where it is negative, its nanoseconds hold a
/// second or more, or a u64 cannot hold it.
fn thread_cpu_ns_from(time: Timespec) -> io::Result<u64> {
    let invalid = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a clock gave {} s and {} ns", time.tv_sec, time.tv_nsec),
        )
    };
    let seconds = u64::try_from(time.tv_sec).map_err(|_| invalid())?;
    let nanoseconds = u64::try_from(time.tv_nsec).map_err(|_| invalid())?;
    if nanoseconds >= NANOSECONDS_PER_SECOND {
        return Err(invalid());
    }
    let Some(time_ns) = seconds
        .checked_mul(NANOSECONDS_PER_SECOND)
        .and_then(|seconds_ns| seconds_ns.checked_add(nanoseconds))
    else {
        return Err(invalid());
    };

    Ok(time_ns)
}

const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;

#[cfg(test)]
pub(crate) fn spin_on_this_thread_for(cpu_time: Duration) {
    let start = thread_cpu_ns().unwrap();
    while Duration::from_nanos(thread_cpu_ns().unwrap() - start) < cpu_time {
        std::hint::black_box(0);
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Snapshot {
    at_ns: u64,
    cpu_ns: u64,
    energy_nj: u64,
    instructions: u64,
    super_instructions: u64,
    spent_ns: [u64; Source::ALL.len()],
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
            at_ns: timebase.nanoseconds(clock::now())?,
            cpu_ns: take_cpu_ns(&own)?,
            energy_nj: own.energy_nj,
            instructions: own.instructions,
            super_instructions: own.super_instructions,
            spent_ns: meter.spent_ns,
        })
    }

    /// The CPU time this process had spent when the snapshot was taken.
    #[must_use]
    pub fn cpu_ns(&self) -> u64 {
        self.cpu_ns
    }

    /// Returns what this process spent from `earlier` to this snapshot, or an error where a
    /// counter macOS gives went back or no time passed, since a cost is a rate over the time.
    /// Both snapshots come from one meter, whose charges only grow.
    pub fn cost_since(&self, earlier: &Snapshot) -> io::Result<Cost> {
        let elapsed_ns = self.cost_since_counter(earlier, "time", |snapshot| snapshot.at_ns)?;
        if elapsed_ns == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("no time passed between two snapshots at {} ns", self.at_ns),
            ));
        }
        let cpu_ns = self.cost_since_counter(earlier, "CPU time", |snapshot| snapshot.cpu_ns)?;
        let energy_nj =
            self.cost_since_counter(earlier, "energy", |snapshot| snapshot.energy_nj)?;
        let instructions =
            self.cost_since_counter(earlier, "instructions", |snapshot| snapshot.instructions)?;
        let super_instructions =
            self.cost_since_counter(earlier, "super core instructions", |snapshot| {
                snapshot.super_instructions
            })?;
        if super_instructions > instructions {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "this process ran {super_instructions} instructions on super cores of \
                     {instructions} in all"
                ),
            ));
        }
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
        assert!(!name.is_empty());

        let before = read(earlier);
        let after = read(self);
        let Some(grown) = after.checked_sub(before) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("this process's {name} went back from {before} to {after}"),
            ));
        };

        Ok(grown)
    }
}

/// Returns the CPU time `own` gives, user and system together, or an error where a u64 cannot
/// hold it.
fn take_cpu_ns(own: &Usage) -> io::Result<u64> {
    let Some(cpu_ns) = own.user_ns.checked_add(own.system_ns) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "a user time of {} ns and a system time of {} ns pass what a u64 holds",
                own.user_ns, own.system_ns
            ),
        ));
    };

    Ok(cpu_ns)
}

#[derive(Debug, Clone, Copy)]
pub struct Cost {
    /// More than zero, which `Snapshot::cost_since` checks, since every rate divides by it.
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
        assert!(self.elapsed > Duration::ZERO);

        self.cpu.div_duration_f64(self.elapsed)
    }

    /// The power spent, as nanojoules a second divided by the million of them in a milliwatt.
    #[must_use]
    pub fn milliwatts(&self) -> f64 {
        assert!(self.elapsed > Duration::ZERO);

        f64_from_u64(self.energy_nj) / self.elapsed.as_secs_f64() / 1_000_000.0
    }

    /// The share of the instructions run that ran on super cores, or `None` where none ran.
    #[must_use]
    pub fn share_on_super_cores(&self) -> Option<f64> {
        assert!(self.super_instructions <= self.instructions);

        if self.instructions == 0 {
            return None;
        }
        // Rounding to the nearest f64 keeps the order of two counts, so the share stays at
        // or under 1.
        Some(f64_from_u64(self.super_instructions) / f64_from_u64(self.instructions))
    }

    #[must_use]
    pub fn ms_per_second(&self, source: Source) -> f64 {
        assert!(self.elapsed > Duration::ZERO);

        self.spent[source.index()].div_duration_f64(self.elapsed) * 1_000.0
    }

    /// The CPU time no source took, in milliseconds a second. It is negative where the sources
    /// took more than the process, since this thread's clock times each source and the
    /// process's usage times the process.
    #[must_use]
    pub fn unattributed_ms_per_second(&self) -> f64 {
        assert!(self.elapsed > Duration::ZERO);
        let attributed: Duration = self.spent.iter().sum();

        (self.cpu.as_secs_f64() - attributed.as_secs_f64()) / self.elapsed.as_secs_f64() * 1_000.0
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
    assert_eq!(value, u64::from(high) << 32 | u64::from(low));

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
    fn refuses_more_super_core_instructions_than_instructions_in_all() {
        // Goal: a span whose super core instructions outnumber every instruction it ran is an
        // error, never a share past 100%.
        // Method: take the cost to a snapshot 2 instructions later, 3 of them on super cores.
        let later = Snapshot {
            at_ns: EARLIER.at_ns + 1,
            instructions: EARLIER.instructions + 2,
            super_instructions: EARLIER.super_instructions + 3,
            ..EARLIER
        };

        let cost = later.cost_since(&EARLIER);

        assert_eq!(io::ErrorKind::InvalidData, cost.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_cost_over_no_time() {
        // Goal: a cost over two snapshots no time apart is an error, never a division by zero
        // time.
        // Method: take the cost from a snapshot to itself.
        let cost = EARLIER.cost_since(&EARLIER);

        assert_eq!(io::ErrorKind::InvalidData, cost.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_cpu_time_past_the_nanoseconds_a_u64_holds() {
        // Goal: a user and system time that add up past what a u64 holds is an error, never an
        // addition that traps.
        // Method: add a user time of u64::MAX nanoseconds to a system time of 1.
        let timebase = Timebase::read().unwrap();
        let own = Usage {
            user_ns: u64::MAX,
            system_ns: 1,
            ..processes::read_usage(timebase, processes::own_pid().unwrap())
                .unwrap()
                .unwrap()
        };

        let cpu_ns = take_cpu_ns(&own);

        assert_eq!(io::ErrorKind::InvalidData, cpu_ns.unwrap_err().kind());
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
        // Method: take the cost to a snapshot a nanosecond later that ran no instructions.
        let later = Snapshot {
            at_ns: EARLIER.at_ns + 1,
            ..EARLIER
        };

        let cost = later.cost_since(&EARLIER).unwrap();

        assert_eq!(None, cost.share_on_super_cores());
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
    fn gives_a_negative_rest_where_the_sources_took_more_than_the_process() {
        // Goal: sources that took more CPU time than the process read as a negative rest,
        // never a rest of 0, since the thread's clock and the process's usage then disagree.
        // Method: spend 0.1 s of CPU in ten seconds, 0.2 s of it on processes.
        let cost = ten_seconds_later(100_000_000, 0, 200_000_000);

        assert!((cost.unattributed_ms_per_second() + 10.0).abs() < 1e-12);
    }

    #[test]
    fn gives_the_share_of_instructions_run_on_super_cores() {
        // Goal: super core instructions read as their share of every instruction run.
        // Method: run 3,000 of 4,000 instructions on super cores.
        let cost = ten_seconds_later(0, 0, 0);

        assert!((cost.share_on_super_cores().unwrap() - 0.75).abs() < 1e-12);
    }

    #[test]
    fn gives_a_share_of_1_where_every_instruction_ran_on_super_cores() {
        // Goal: a span whose every instruction ran on super cores gives a share of 1, never an
        // error or a share refused as past 1.
        // Method: take the cost to a snapshot 4,000 instructions later, all on super cores.
        let later = Snapshot {
            at_ns: EARLIER.at_ns + 1,
            instructions: EARLIER.instructions + 4_000,
            super_instructions: EARLIER.super_instructions + 4_000,
            ..EARLIER
        };

        let cost = later.cost_since(&EARLIER).unwrap();

        assert_eq!(Some(1.0), cost.share_on_super_cores());
    }

    #[test]
    fn refuses_a_clock_macos_does_not_have() {
        // Goal: a clock that clock_gettime refuses is an error, never a time of 0.
        // Method: read clock 99, which clock_gettime refused with -1 on macOS 27.0.1.
        let read = thread_cpu_ns_of(99);

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

        assert_eq!(2_000_000_005, thread_cpu_ns_from(time).unwrap());
    }

    #[test]
    fn reads_the_last_nanosecond_of_a_second() {
        // Goal: the most nanoseconds a time holds past its seconds read as a time, never as
        // a whole second refused.
        // Method: read 1 s and 999,999,999 ns.
        let time = Timespec {
            tv_sec: 1,
            tv_nsec: 999_999_999,
        };

        assert_eq!(1_999_999_999, thread_cpu_ns_from(time).unwrap());
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

        let read = thread_cpu_ns_from(time);

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_negative_count_of_nanoseconds() {
        // Goal: a clock that gives a negative count of nanoseconds is an error, never a time
        // read as 0 nanoseconds past its second.
        // Method: read 0 s and -1 ns.
        let time = Timespec {
            tv_sec: 0,
            tv_nsec: -1,
        };

        let read = thread_cpu_ns_from(time);

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }

    #[test]
    fn refuses_nanoseconds_of_a_second_or_more() {
        // Goal: a clock whose nanoseconds hold a whole second is an error, since a time keeps
        // its whole seconds apart from the nanoseconds past them.
        // Method: read 0 s and 1,000,000,000 ns.
        let time = Timespec {
            tv_sec: 0,
            tv_nsec: 1_000_000_000,
        };

        let read = thread_cpu_ns_from(time);

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }

    #[test]
    fn refuses_nanoseconds_that_carry_a_time_past_what_a_u64_holds() {
        // Goal: a time whose seconds a u64 holds in nanoseconds, but not with its nanoseconds
        // added, is an error, never a time that wrapped.
        // Method: read the seconds of u64::MAX nanoseconds, with 1 ns more than the rest.
        let time = Timespec {
            tv_sec: 18_446_744_073,
            tv_nsec: 709_551_616,
        };

        let read = thread_cpu_ns_from(time);

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

        let read = thread_cpu_ns_from(time);

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

    #[test]
    fn refuses_to_charge_a_source_past_the_nanoseconds_a_u64_holds() {
        // Goal: a charge that takes a source's CPU time past what a u64 holds is an error, never
        // an addition that traps.
        // Method: measure a read that spins 1 ms against a meter that has charged each source
        // u64::MAX nanoseconds.
        let mut meter = Meter {
            spent_ns: [u64::MAX; Source::ALL.len()],
        };

        let measured = meter.measure(Source::Machine, || {
            spin_on_this_thread_for(Duration::from_millis(1));
            Ok(())
        });

        assert_eq!(io::ErrorKind::InvalidData, measured.unwrap_err().kind());
    }
}
