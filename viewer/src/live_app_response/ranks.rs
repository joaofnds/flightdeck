//! The Apps that used the most of each resource in one observation. A sample ranks its Apps
//! once, so the viewer can keep every observation of a window ranked and show any of them at
//! once.
use std::cmp::Ordering;
use std::sync::Arc;

use super::{App, Error, KeyKind, Measure, Observation, Result, Sample};
use crate::apps;

pub const RANKS_MAX: usize = 10;
pub(crate) const METRICS: [Metric; 4] =
    [Metric::Cpu, Metric::Resident, Metric::Power, Metric::Disk];

#[derive(Debug, PartialEq)]
pub struct Ranks {
    pub selected: Observation,
    pub app_count: u32,
    pub membership_complete: bool,
    pub ungrouped_members: u32,
    /// CPU, resident memory, power and disk, in that order.
    pub metrics: [Ranking; 4],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Cpu,
    Resident,
    Power,
    Disk,
}

#[derive(Debug, PartialEq)]
pub struct Ranking {
    pub metric: Metric,
    /// At most `RANKS_MAX`, the largest total first.
    pub entries: Vec<Rank>,
    /// Apps no member of which measured this metric, or whose rate needs an interval the
    /// sample lacks.
    pub unavailable: u32,
    pub overflowed: u32,
    /// Ranked Apps with some members unmeasured.
    pub partial: u32,
}

#[derive(Debug, PartialEq)]
pub struct Rank {
    pub app: Arc<RankedApp>,
    pub amount: u64,
    pub rate: Option<f64>,
    pub measured_members: u32,
    pub unavailable_members: u32,
}

#[derive(Debug, PartialEq)]
pub struct RankedApp {
    pub kind: KeyKind,
    pub bytes: Vec<u8>,
    pub display_name: String,
}

const _: () = {
    assert!(RANKS_MAX == 10);
    assert!(super::APPS_MAX <= 4_294_967_295);
};

impl Sample {
    pub fn ranks(&self) -> Result<Ranks> {
        let mut ranks = Ranks {
            selected: self.selected,
            app_count: u32::try_from(self.apps.len()).map_err(|_| Error::Bound("ranked Apps"))?,
            membership_complete: self.membership_complete,
            ungrouped_members: self.ungrouped_members,
            metrics: METRICS.map(Ranking::new),
        };
        for app in &self.apps {
            ranks_app(app, self.elapsed_ns, &mut ranks.metrics);
        }

        Ok(ranks)
    }
}

fn ranks_app(app: &App, elapsed_ns: Option<u64>, metrics: &mut [Ranking; 4]) {
    let mut ranked = None;
    for ranking in metrics {
        let Some(reading) = ranking.read(app, elapsed_ns) else {
            continue;
        };

        if ranking.entries.len() == RANKS_MAX {
            if reading.amount < ranking.entries[RANKS_MAX - 1].amount {
                continue;
            }
        }

        let ranked = ranked.get_or_insert_with(|| {
            Arc::new(RankedApp {
                kind: app.key.kind,
                bytes: app.key.bytes.clone(),
                display_name: app.display_name.clone(),
            })
        });
        ranking.insert(Rank {
            app: Arc::clone(ranked),
            amount: reading.amount,
            rate: reading.rate,
            measured_members: reading.measure.measured_members,
            unavailable_members: reading.measure.unavailable_members,
        });
    }
}

struct Reading<'app> {
    measure: &'app Measure,
    amount: u64,
    rate: Option<f64>,
}

impl RankedApp {
    /// The App whose key is `bytes`, named as its sample names it.
    pub(crate) fn new(kind: KeyKind, bytes: Vec<u8>) -> Result<Self> {
        let display_name = super::capture::display_name(kind, &bytes)?;

        Ok(Self {
            kind,
            bytes,
            display_name,
        })
    }
}

impl Ranking {
    pub(crate) fn new(metric: Metric) -> Self {
        Self {
            metric,
            entries: Vec::with_capacity(RANKS_MAX + 1),
            unavailable: 0,
            overflowed: 0,
            partial: 0,
        }
    }

    fn read<'app>(&mut self, app: &'app App, elapsed_ns: Option<u64>) -> Option<Reading<'app>> {
        let measure = self.metric.measure(app);
        if measure.overflowed {
            self.overflowed += 1;
            return None;
        }

        if measure.measured_members == 0 {
            self.unavailable += 1;
            return None;
        }

        let rate = elapsed_ns.and_then(|elapsed_ns| self.metric.rate(measure.known, elapsed_ns));
        if self.metric != Metric::Resident {
            if rate.is_none() {
                self.unavailable += 1;
                return None;
            }
        }

        if measure.unavailable_members > 0 {
            self.partial += 1;
        }

        Some(Reading {
            measure,
            amount: measure.known,
            rate,
        })
    }

    fn insert(&mut self, candidate: Rank) {
        let index = self
            .entries
            .partition_point(|entry| entry.compare(&candidate).is_lt());
        if index < RANKS_MAX {
            self.entries.insert(index, candidate);
            if self.entries.len() > RANKS_MAX {
                assert!(self.entries.pop().is_some());
            }
        }
        assert!(self.entries.len() <= RANKS_MAX);
    }
}

impl Rank {
    /// The larger total first, then a bundle before an executable, then the key's bytes.
    fn compare(&self, other: &Self) -> Ordering {
        other
            .amount
            .cmp(&self.amount)
            .then(kind_order(self.app.kind).cmp(&kind_order(other.app.kind)))
            .then(self.app.bytes.cmp(&other.app.bytes))
    }
}

fn kind_order(kind: KeyKind) -> u8 {
    match kind {
        KeyKind::Bundle => 0,
        KeyKind::Executable => 1,
    }
}

impl Metric {
    fn measure(self, app: &App) -> &Measure {
        match self {
            Self::Cpu => &app.cpu_ns,
            Self::Resident => &app.resident_bytes,
            Self::Power => &app.energy_nj,
            Self::Disk => &app.disk_io_bytes,
        }
    }

    fn rate(self, amount: u64, elapsed_ns: u64) -> Option<f64> {
        match self {
            Self::Cpu => apps::cpu_percent_one_core(apps::CpuRate {
                cpu_ns: amount,
                elapsed_ns,
            }),
            Self::Resident => None,
            Self::Power => apps::energy_watts(apps::EnergyRate {
                energy_nj: amount,
                elapsed_ns,
            }),
            Self::Disk => apps::bytes_per_second(apps::BytesRate {
                bytes: amount,
                elapsed_ns,
            }),
        }
    }
}
