//! The Apps that used the most of each resource in one observation. A sample ranks its Apps
//! once, so the viewer can keep every observation of a window ranked and show any of them at
//! once.
use std::cmp::Ordering;
use std::sync::Arc;

use super::{App, Error, KeyKind, Measure, Observation, Result, Sample};
use crate::apps;

pub const RANKS_MAX: usize = 10;
pub(crate) const METRICS: [Metric; 5] = [
    Metric::Cpu,
    Metric::Resident,
    Metric::Power,
    Metric::Disk,
    Metric::Gpu,
];

#[derive(Debug, PartialEq)]
pub struct Ranks {
    pub selected: Observation,
    pub app_count: u32,
    pub membership_complete: bool,
    pub ungrouped_members: u32,
    /// CPU, resident memory, power, disk and GPU, in that order.
    pub metrics: [Ranking; 5],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Cpu,
    Resident,
    Power,
    Disk,
    Gpu,
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

/// One App as the ranking rule reads it, so a sample's Apps and the Apps the store holds rank
/// by one rule.
pub(crate) trait Rankable {
    /// What the App's members used of `metric`.
    fn measure(&self, metric: Metric) -> Measure;

    /// The App as a ranking names it.
    fn ranked(&self) -> Arc<RankedApp>;
}

/// What the ranks of one observation carry beside its Apps.
pub(crate) struct Ranked {
    pub(crate) selected: Observation,
    pub(crate) elapsed_ns: Option<u64>,
    pub(crate) membership_complete: bool,
    pub(crate) ungrouped_members: u32,
}

impl Sample {
    pub fn ranks(&self) -> Result<Ranks> {
        rank_apps(
            &Ranked {
                selected: self.selected,
                elapsed_ns: self.elapsed_ns,
                membership_complete: self.membership_complete,
                ungrouped_members: self.ungrouped_members,
            },
            &self.apps,
        )
    }
}

/// The Apps of `apps` that used the most of each resource in the observation `ranked` names.
pub(crate) fn rank_apps(ranked: &Ranked, apps: &[impl Rankable]) -> Result<Ranks> {
    let mut ranks = Ranks {
        selected: ranked.selected,
        app_count: u32::try_from(apps.len()).map_err(|_| Error::Bound("ranked Apps"))?,
        membership_complete: ranked.membership_complete,
        ungrouped_members: ranked.ungrouped_members,
        metrics: METRICS.map(Ranking::new),
    };
    for app in apps {
        ranks_app(app, ranked.elapsed_ns, &mut ranks.metrics);
    }

    Ok(ranks)
}

impl Rankable for App {
    fn measure(&self, metric: Metric) -> Measure {
        match metric {
            Metric::Cpu => self.cpu_ns,
            Metric::Resident => self.resident_bytes,
            Metric::Power => self.energy_nj,
            Metric::Disk => self.disk_io_bytes,
            Metric::Gpu => self.gpu_ns,
        }
    }

    fn ranked(&self) -> Arc<RankedApp> {
        Arc::new(RankedApp {
            kind: self.key.kind,
            bytes: self.key.bytes.clone(),
            display_name: self.display_name.clone(),
        })
    }
}

fn ranks_app(app: &impl Rankable, elapsed_ns: Option<u64>, metrics: &mut [Ranking; 5]) {
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

        let ranked = ranked.get_or_insert_with(|| app.ranked());
        ranking.insert(Rank {
            app: Arc::clone(ranked),
            amount: reading.amount,
            rate: reading.rate,
            measured_members: reading.measure.measured_members,
            unavailable_members: reading.measure.unavailable_members,
        });
    }
}

struct Reading {
    measure: Measure,
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

    fn read(&mut self, app: &impl Rankable, elapsed_ns: Option<u64>) -> Option<Reading> {
        let measure = app.measure(self.metric);
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
            Self::Gpu => apps::gpu_percent(apps::GpuRate {
                gpu_ns: amount,
                elapsed_ns,
            }),
        }
    }
}
