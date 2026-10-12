pub struct Schedule {
    start: u64,
    period: u64,
}

impl Schedule {
    #[must_use]
    pub fn new(start: u64, period: u64) -> Self {
        Self { start, period }
    }

    #[must_use]
    pub fn deadline(&self, tick: u64) -> u64 {
        self.start + tick * self.period
    }

    #[must_use]
    pub fn first_ahead_of(&self, now: u64) -> u64 {
        now.saturating_sub(self.start) / self.period + 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn puts_each_tick_a_whole_number_of_periods_after_the_start() {
        let schedule = Schedule::new(1_000, 24);

        assert_eq!(
            [1_000, 1_024, 1_240],
            [0, 1, 10].map(|tick| schedule.deadline(tick))
        );
    }

    #[test]
    fn gives_the_first_tick_whose_deadline_is_still_ahead() {
        let schedule = Schedule::new(1_000, 24);

        assert_eq!(
            [1, 1, 2, 4],
            [1_000, 1_023, 1_024, 1_090].map(|now| schedule.first_ahead_of(now))
        );
    }
}
