#[repr(C)]
struct MachTimebaseInfo {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_continuous_time() -> u64;
    fn mach_timebase_info(info: *mut MachTimebaseInfo) -> libc::c_int;
}

#[must_use]
pub fn now() -> u64 {
    unsafe { mach_continuous_time() }
}

#[derive(Clone, Copy)]
pub struct Timebase {
    numer: u64,
    denom: u64,
}

impl Timebase {
    #[must_use]
    pub fn read() -> Self {
        let mut info = MachTimebaseInfo { numer: 0, denom: 0 };
        unsafe { mach_timebase_info(&raw mut info) };

        Self {
            numer: info.numer.into(),
            denom: info.denom.into(),
        }
    }

    #[must_use]
    pub fn nanos(self, ticks: u64) -> u64 {
        u64::try_from(u128::from(ticks) * u128::from(self.numer) / u128::from(self.denom))
            .unwrap_or(u64::MAX)
    }

    #[must_use]
    pub fn ticks(self, nanos: u64) -> u64 {
        u64::try_from(u128::from(nanos) * u128::from(self.denom) / u128::from(self.numer))
            .unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::*;

    #[test]
    fn measures_an_interval_within_what_the_standard_clock_measures() {
        let timebase = Timebase::read();
        let outer = Instant::now();
        let start = now();

        std::thread::sleep(Duration::from_millis(20));

        let elapsed = Duration::from_nanos(timebase.nanos(now() - start));
        let outer_elapsed = outer.elapsed();
        assert!(
            Duration::from_millis(20) <= elapsed && elapsed <= outer_elapsed,
            "{elapsed:?} inside {outer_elapsed:?}"
        );
    }

    #[test]
    fn turns_a_second_of_ticks_into_a_second_of_nanoseconds() {
        let timebase = Timebase::read();

        assert_eq!(1_000_000_000, timebase.nanos(timebase.ticks(1_000_000_000)));
    }
}
