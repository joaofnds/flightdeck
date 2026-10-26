//! Exact counter arithmetic shared by saved, live and Store readings. Adapters own pairing
//! and row order; a missing value or reset stays absent. Machine presentation multiplies
//! before division, while per-core shares and model App rates retain division first.
use persistence::{Error, Result};

pub(crate) struct CounterValue {
    pub current: Option<u64>,
    pub previous: Option<u64>,
}

#[derive(Clone, Copy)]
pub(crate) struct CounterPair<'a, const COUNT: usize> {
    pub current: &'a [Option<u64>; COUNT],
    pub previous: &'a [Option<u64>; COUNT],
}

pub(crate) struct CounterTotals<const COUNT: usize> {
    sums: [Option<u64>; COUNT],
}

#[derive(Clone, Copy, Default)]
pub(crate) struct CpuTotals {
    busy: u64,
    total: u64,
}

pub(crate) struct Fraction {
    pub numerator: u64,
    pub denominator: u64,
}

pub(crate) struct Rate {
    pub delta: u64,
    pub elapsed_ns: u64,
}

pub(crate) fn delta(value: &CounterValue) -> Option<u64> {
    value
        .current
        .zip(value.previous)
        .and_then(|(now, before)| now.checked_sub(before))
}

/// A flattened sum retains the saved adapter's row/column visitation order, including
/// whether an overflow precedes a later absent or reset counter.
pub(crate) struct CounterTotal {
    sum: Option<u64>,
}

impl CounterTotal {
    pub fn new() -> Self {
        Self { sum: Some(0) }
    }

    pub fn add(&mut self, value: &CounterValue) -> Result<()> {
        self.sum = match self.sum.zip(delta(value)) {
            Some((sum, delta)) => Some(sum.checked_add(delta).ok_or(Error::Bound("counter sum"))?),
            None => None,
        };
        Ok(())
    }

    pub fn finish(&self) -> Option<u64> {
        self.sum
    }
}

impl<const COUNT: usize> CounterTotals<COUNT> {
    pub fn new() -> Self {
        assert!(COUNT > 0);
        assert!(COUNT <= 6);
        Self {
            sums: [Some(0); COUNT],
        }
    }

    pub fn add(&mut self, pair: CounterPair<'_, COUNT>) -> Result<()> {
        for (index, sum) in self.sums.iter_mut().enumerate() {
            let gained = delta(&CounterValue {
                current: pair.current[index],
                previous: pair.previous[index],
            });
            *sum = match sum.zip(gained) {
                Some((sum, gained)) => {
                    Some(sum.checked_add(gained).ok_or(Error::Bound("counter sum"))?)
                }
                None => None,
            };
        }
        Ok(())
    }

    pub fn finish(self) -> [Option<u64>; COUNT] {
        self.sums
    }
}

impl CpuTotals {
    pub fn between(pair: CounterPair<'_, 4>) -> Result<Option<Self>> {
        let mut totals = Self::default();
        for index in 0..4 {
            let Some(gained) = delta(&CounterValue {
                current: pair.current[index],
                previous: pair.previous[index],
            }) else {
                return Ok(None);
            };
            totals.total = totals
                .total
                .checked_add(gained)
                .ok_or(Error::Bound("CPU counter total"))?;
            if index < 3 {
                totals.busy = totals
                    .busy
                    .checked_add(gained)
                    .ok_or(Error::Bound("CPU busy total"))?;
            }
        }
        Ok(Some(totals))
    }

    pub fn add(&mut self, other: &Self) -> Result<()> {
        self.busy = self
            .busy
            .checked_add(other.busy)
            .ok_or(Error::Bound("CPU sum"))?;
        self.total = self
            .total
            .checked_add(other.total)
            .ok_or(Error::Bound("CPU sum"))?;
        Ok(())
    }

    pub fn machine_percent(self) -> Option<f64> {
        machine_percent(&Fraction {
            numerator: self.busy,
            denominator: self.total,
        })
    }

    pub fn core_percent(self) -> Option<f64> {
        if self.total == 0 {
            return None;
        }

        let percent = super::number(self.busy) / super::number(self.total) * 100.0;
        assert!(percent.is_finite());
        assert!((0.0..=100.0).contains(&percent));
        Some(percent)
    }
}

pub(crate) fn busy_sum(
    pairs: impl ExactSizeIterator<Item = Result<Option<CpuTotals>>>,
) -> Result<Option<f64>> {
    assert!(pairs.len() <= crate::cpu_history_sample::CPUS_MAX);
    if pairs.len() == 0 {
        return Ok(None);
    }

    let mut sum = 0.0;
    for pair in pairs {
        let Some(percent) = pair?.and_then(CpuTotals::core_percent) else {
            return Ok(None);
        };
        sum += percent;
    }
    assert!(sum.is_finite());
    Ok(Some(sum))
}

pub(crate) fn machine_percent(fraction: &Fraction) -> Option<f64> {
    if fraction.denominator == 0 {
        return None;
    }

    Some(super::number(fraction.numerator) * 100.0 / super::number(fraction.denominator))
}

pub(crate) fn per_second(rate: &Rate) -> f64 {
    assert!(rate.elapsed_ns > 0);
    super::number(rate.delta) * 1e9 / super::number(rate.elapsed_ns)
}
