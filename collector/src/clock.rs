use std::io;
use std::mem::offset_of;

use crate::kernel::{KernReturn, kern_return_check};

#[must_use]
pub fn now() -> u64 {
    unsafe { mach_continuous_time() }
}

/// Converts between ticks and nanoseconds, at `numerator` nanoseconds to `denominator`
/// ticks.
#[derive(Debug, Clone, Copy)]
pub struct Timebase {
    numerator: u64,
    denominator: u64,
}

impl Timebase {
    pub fn read() -> io::Result<Self> {
        let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
        let returned = unsafe { mach_timebase_info(&raw mut info) };
        kern_return_check(returned, "mach_timebase_info")?;

        read_check(info)
    }

    /// Converts `ticks` to nanoseconds, rounded down.
    pub fn nanos(self, ticks: u64) -> io::Result<u64> {
        assert!(self.numerator > 0);
        assert!(self.denominator > 0);

        let nanos = u128::from(ticks) * u128::from(self.numerator) / u128::from(self.denominator);
        u64::try_from(nanos).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{ticks} ticks run past the nanoseconds a u64 holds"),
            )
        })
    }

    /// Converts `nanos` to ticks, rounded down.
    pub fn ticks(self, nanos: u64) -> io::Result<u64> {
        assert!(self.numerator > 0);
        assert!(self.denominator > 0);

        let ticks = u128::from(nanos) * u128::from(self.denominator) / u128::from(self.numerator);
        u64::try_from(ticks).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("{nanos} nanoseconds run past the ticks a u64 holds"),
            )
        })
    }
}

/// Checks that `info` gives a numerator and a denominator above 0, and returns the
/// timebase it gives.
fn read_check(info: MachTimebaseInfo) -> io::Result<Timebase> {
    if info.numer == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "mach_timebase_info gave a numerator of 0",
        ));
    }
    if info.denom == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "mach_timebase_info gave a denominator of 0",
        ));
    }

    Ok(Timebase {
        numerator: info.numer.into(),
        denominator: info.denom.into(),
    })
}

/// The ratio of nanoseconds to ticks, as `struct mach_timebase_info` declares it.
#[repr(C)]
#[derive(Clone, Copy)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

const _: () = assert!(size_of::<MachTimebaseInfo>() == 8);
const _: () = assert!(offset_of!(MachTimebaseInfo, numer) == 0);
const _: () = assert!(offset_of!(MachTimebaseInfo, denom) == 4);

unsafe extern "C" {
    fn mach_continuous_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> KernReturn;
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn measures_an_interval_of_at_least_the_time_slept() {
        // Goal: an interval measured in ticks is at least the time slept inside it.
        // Method: sleep 20 ms between two readings of the clock.
        let timebase = Timebase::read().unwrap();
        let start = now();

        std::thread::sleep(Duration::from_millis(20));

        let elapsed = Duration::from_nanos(timebase.nanos(now() - start).unwrap());
        assert!(Duration::from_millis(20) <= elapsed, "{elapsed:?}");
    }

    #[test]
    fn measures_an_interval_within_what_the_standard_clock_measures() {
        // Goal: an interval measured in ticks is no longer than the standard clock measures
        // around it, so ticks convert to nanoseconds at no more than their real rate.
        // Method: read the standard clock outside two readings of this clock, 20 ms apart.
        let timebase = Timebase::read().unwrap();
        let outer = Instant::now();
        let start = now();

        std::thread::sleep(Duration::from_millis(20));

        let elapsed = Duration::from_nanos(timebase.nanos(now() - start).unwrap());
        let outer_elapsed = outer.elapsed();
        assert!(
            elapsed <= outer_elapsed,
            "{elapsed:?} inside {outer_elapsed:?}"
        );
    }

    #[test]
    fn turns_a_second_of_ticks_into_a_second_of_nanoseconds() {
        // Goal: a second converted to ticks and back is a second, with nothing lost to
        // rounding.
        // Method: convert 10^9 nanoseconds with this Mac's timebase.
        let timebase = Timebase::read().unwrap();

        let ticks = timebase.ticks(1_000_000_000).unwrap();

        assert_eq!(1_000_000_000, timebase.nanos(ticks).unwrap());
    }

    #[test]
    fn takes_a_timebase_of_125_nanoseconds_to_3_ticks() {
        // Goal: a timebase with a numerator and a denominator above 0 converts ticks.
        // Method: check the 125/3 this Mac's mach_timebase_info gave, and convert 3 ticks.
        let info = MachTimebaseInfo {
            numer: 125,
            denom: 3,
        };

        let timebase = read_check(info).unwrap();

        assert_eq!(125, timebase.nanos(3).unwrap());
    }

    #[test]
    fn refuses_a_timebase_with_a_numerator_of_0() {
        // Goal: a numerator of 0 is an error, since every tick would read as no time.
        // Method: check a timebase of 0/3.
        let info = MachTimebaseInfo { numer: 0, denom: 3 };

        let checked = read_check(info);

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_timebase_with_a_denominator_of_0() {
        // Goal: a denominator of 0 is an error, since no conversion divides by it.
        // Method: check a timebase of 125/0.
        let info = MachTimebaseInfo {
            numer: 125,
            denom: 0,
        };

        let checked = read_check(info);

        assert_eq!(io::ErrorKind::InvalidData, checked.unwrap_err().kind());
    }

    #[test]
    fn refuses_ticks_past_the_nanoseconds_a_u64_holds() {
        // Goal: ticks whose nanoseconds a u64 cannot hold are an error, never a made-up time.
        // Method: convert the most ticks a u64 holds at 125/3, about 42 nanoseconds a tick.
        let timebase = read_check(MachTimebaseInfo {
            numer: 125,
            denom: 3,
        })
        .unwrap();

        let nanos = timebase.nanos(u64::MAX);

        assert_eq!(io::ErrorKind::InvalidData, nanos.unwrap_err().kind());
    }

    #[test]
    fn refuses_nanoseconds_past_the_ticks_a_u64_holds() {
        // Goal: nanoseconds whose ticks a u64 cannot hold are an error, never a made-up time.
        // Method: convert the most nanoseconds a u64 holds at 3/125, about 42 ticks a
        // nanosecond.
        let timebase = read_check(MachTimebaseInfo {
            numer: 3,
            denom: 125,
        })
        .unwrap();

        let ticks = timebase.ticks(u64::MAX);

        assert_eq!(io::ErrorKind::InvalidData, ticks.unwrap_err().kind());
    }
}
