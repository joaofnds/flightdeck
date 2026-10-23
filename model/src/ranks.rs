//! The Apps that used the most of each resource in one tick, ranked once so a page shows any
//! saved second's top Apps without ranking them again.
use crate::apps::{Subtotal, Totals};
use crate::programs::{App, Programs};
use crate::rates;

pub const RANKS_MAX: usize = 10;
/// The resources a tick's Apps rank by, in the order `Ranks` holds their rankings.
pub const METRICS: [Metric; 5] = [
    Metric::Cpu,
    Metric::Resident,
    Metric::Power,
    Metric::Disk,
    Metric::Gpu,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Cpu,
    Resident,
    Power,
    Disk,
    Gpu,
}

/// The ranks of one tick's Apps, in buffers taken at startup.
#[derive(Debug, PartialEq)]
pub struct Ranks {
    pub app_count: u32,
    /// CPU, resident memory, power, disk and GPU, in that order.
    pub metrics: [Ranking; 5],
}

#[derive(Debug, PartialEq)]
pub struct Ranking {
    pub metric: Metric,
    /// At most `RANKS_MAX`, the largest total first.
    entries: Vec<Rank>,
    /// Apps no member of which measured this metric, or whose rate needs an interval the
    /// tick lacks.
    pub unavailable: u32,
    pub overflowed: u32,
    /// Ranked Apps with some members unmeasured.
    pub partial: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rank {
    pub app: App,
    pub amount: u64,
    pub rate: Option<f64>,
    pub measured_members: u32,
    pub unavailable_members: u32,
}

impl Default for Ranks {
    fn default() -> Self {
        Self {
            app_count: 0,
            metrics: METRICS.map(Ranking::new),
        }
    }
}

impl Ranks {
    /// Ranks `totals`, the Apps of one tick, whose rates divide by `elapsed_ns`. Equal totals
    /// rank by their Apps' keys, a bundle before an executable and then by the key's bytes.
    pub fn rank(&mut self, programs: &Programs, totals: &[Totals], elapsed_ns: Option<u64>) {
        self.app_count = crate::u32_from_length(totals.len());
        for ranking in &mut self.metrics {
            ranking.clear();
        }
        for app in totals {
            for ranking in &mut self.metrics {
                ranking.add(programs, app, elapsed_ns);
            }
        }
    }
}

impl Ranking {
    fn new(metric: Metric) -> Self {
        Self {
            metric,
            entries: Vec::with_capacity(RANKS_MAX),
            unavailable: 0,
            overflowed: 0,
            partial: 0,
        }
    }

    /// The ranked Apps, the largest total first.
    #[must_use]
    pub fn entries(&self) -> &[Rank] {
        &self.entries
    }

    fn clear(&mut self) {
        self.entries.clear();
        self.unavailable = 0;
        self.overflowed = 0;
        self.partial = 0;
    }

    fn add(&mut self, programs: &Programs, app: &Totals, elapsed_ns: Option<u64>) {
        let measure = self.metric.measure(app);
        if measure.overflowed {
            self.overflowed += 1;
            return;
        }

        if measure.measured_members == 0 {
            self.unavailable += 1;
            return;
        }

        let rate = elapsed_ns.and_then(|elapsed_ns| self.metric.rate(measure.known, elapsed_ns));
        if self.metric != Metric::Resident {
            if rate.is_none() {
                self.unavailable += 1;
                return;
            }
        }

        if measure.unavailable_members > 0 {
            self.partial += 1;
        }

        self.insert(
            programs,
            Rank {
                app: app.app,
                amount: measure.known,
                rate,
                measured_members: measure.measured_members,
                unavailable_members: measure.unavailable_members,
            },
        );
    }

    fn insert(&mut self, programs: &Programs, candidate: Rank) {
        let index = self.entries.partition_point(|entry| {
            if entry.amount == candidate.amount {
                programs.rank(entry.app) < programs.rank(candidate.app)
            } else {
                entry.amount > candidate.amount
            }
        });
        if index < RANKS_MAX {
            // The ranks never outgrow the buffer taken at startup.
            self.entries.truncate(RANKS_MAX - 1);
            self.entries.insert(index, candidate);
        }
        assert!(self.entries.len() <= RANKS_MAX);
    }
}

impl Metric {
    fn measure(self, app: &Totals) -> &Subtotal {
        match self {
            Self::Cpu => &app.cpu_ns,
            Self::Resident => &app.resident_bytes,
            Self::Power => &app.energy_nj,
            Self::Disk => &app.disk_io_bytes,
            Self::Gpu => &app.gpu_ns,
        }
    }

    fn rate(self, amount: u64, elapsed_ns: u64) -> Option<f64> {
        match self {
            Self::Cpu => rates::cpu_percent_one_core(rates::CpuRate {
                cpu_ns: amount,
                elapsed_ns,
            }),
            Self::Resident => None,
            Self::Power => rates::energy_watts(rates::EnergyRate {
                energy_nj: amount,
                elapsed_ns,
            }),
            Self::Disk => rates::bytes_per_second(rates::BytesRate {
                bytes: amount,
                elapsed_ns,
            }),
            Self::Gpu => rates::gpu_percent(rates::GpuRate {
                gpu_ns: amount,
                elapsed_ns,
            }),
        }
    }
}

#[cfg(test)]
mod tests;
