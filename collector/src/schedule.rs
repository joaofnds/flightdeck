use std::io;

use crate::clock;

/// When each tick of a run falls due, `period` clock ticks apart from `start`.
#[derive(Debug)]
pub struct Schedule {
    start: u64,
    period: u64,
}

impl Schedule {
    /// Starts a schedule now whose ticks fall due `period` clock ticks apart, or an error for
    /// a period of 0, which would put every tick at the start.
    pub fn starting_now(period: u64) -> io::Result<Self> {
        if period == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a schedule's period is 0 clock ticks",
            ));
        }

        let schedule = Self {
            start: clock::now(),
            period,
        };
        assert!(schedule.period > 0);
        Ok(schedule)
    }

    /// Gives the clock reading at which `tick` falls due.
    #[must_use]
    pub fn deadline(&self, tick: u64) -> u64 {
        assert!(self.period > 0);

        let deadline = self.start + tick * self.period;
        assert!(deadline >= self.start);
        deadline
    }

    /// Gives the first tick whose deadline is past `now`.
    #[must_use]
    pub fn first_ahead_of(&self, now: u64) -> u64 {
        assert!(self.period > 0);

        let tick = if now < self.start {
            0
        } else {
            (now - self.start) / self.period + 1 // The division floors.
        };

        assert!(now < self.deadline(tick));
        if tick > 0 {
            assert!(self.deadline(tick - 1) <= now);
        }
        tick
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_the_clock_reading_of_its_start() {
        // Goal: a schedule's tick 0 falls due when the schedule starts.
        // Method: read the clock before and after starting a schedule.
        let before = clock::now();

        let schedule = Schedule::starting_now(24).unwrap();

        let after = clock::now();
        assert!(before <= schedule.deadline(0));
        assert!(schedule.deadline(0) <= after);
    }

    #[test]
    fn puts_each_tick_a_whole_number_of_periods_after_the_start() {
        // Goal: tick n falls due n periods after the start.
        // Method: read the deadlines of ticks 0, 1 and 10 of a schedule of 24-tick periods.
        let schedule = Schedule::starting_now(24).unwrap();
        let start = schedule.deadline(0);

        assert_eq!(
            [start, start + 24, start + 240],
            [0, 1, 10].map(|tick| schedule.deadline(tick))
        );
    }

    #[test]
    fn gives_the_first_tick_whose_deadline_is_still_ahead() {
        // Goal: the tick ahead of a time is the first whose deadline is past it, so a time on
        // a deadline belongs to the tick before.
        // Method: ask at the start, inside the first period, on its end, and three periods on.
        let schedule = Schedule::starting_now(24).unwrap();
        let start = schedule.deadline(0);

        assert_eq!(
            [1, 1, 2, 4],
            [start, start + 23, start + 24, start + 90].map(|now| schedule.first_ahead_of(now))
        );
    }

    #[test]
    fn gives_tick_0_before_the_start() {
        // Goal: before the start, tick 0 is the first tick still ahead, never a later one.
        // Method: ask one clock tick before the start.
        let schedule = Schedule::starting_now(24).unwrap();

        assert_eq!(0, schedule.first_ahead_of(schedule.deadline(0) - 1));
    }

    #[test]
    fn refuses_a_period_of_0() {
        // Goal: a period of 0 clock ticks is an error, since every tick would fall due at the
        // start.
        // Method: start a schedule of 0-tick periods.
        let schedule = Schedule::starting_now(0);

        assert_eq!(io::ErrorKind::InvalidInput, schedule.unwrap_err().kind());
    }
}
