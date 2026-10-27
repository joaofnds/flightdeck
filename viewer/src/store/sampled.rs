//! Older machine samples own their exact identity and bounded hover rankings together.
use std::sync::Arc;

use super::{AppsSecond, MachineSecond, Missing};
use crate::apps::Stamp;
use crate::byte_budget::{ByteBudget, ByteReservation};
use crate::live_app_response::{
    METRICS, Metric, Observation, RANKS_MAX, Rank, RankedApp, Ranking, Ranks,
};

/// Ranked names and vectors remain owned by this sample's byte reservation.
/// ```compile_fail
/// use std::sync::Arc;
/// use viewer::live_app_response::RankedApp;
/// use viewer::store::{Missing, SampledSecond};
/// fn escape(sample: &SampledSecond) -> Result<Arc<RankedApp>, &Missing> {
///     sample.ranks.as_ref().map(|ranks| Arc::clone(&ranks.metrics[0].entries[0].app))
/// }
/// ```
/// ```compile_fail
/// use viewer::live_app_response::Ranks;
/// use viewer::store::{Missing, SampledSecond};
/// fn escape(sample: &mut SampledSecond) -> Result<Ranks, Missing> {
///     std::mem::replace(&mut sample.ranks, Err(Missing::Unread))
/// }
/// ```
#[derive(Debug)]
pub struct SampledSecond {
    pub machine: MachineSecond,
    pub observation: Observation,
    ranks: Result<Ranks, Missing>,
    reservation: Option<ByteReservation>,
}

/// Rankings borrowed from their reservation owner, without independently cloneable names.
#[derive(Debug, PartialEq)]
pub struct SampledRanks<'sample> {
    pub selected: Observation,
    pub app_count: u32,
    pub membership_complete: bool,
    pub ungrouped_members: u32,
    pub metrics: [SampledRanking<'sample>; 5],
}

#[derive(Debug, PartialEq)]
pub struct SampledRanking<'sample> {
    pub metric: Metric,
    entries: &'sample [Rank],
    pub unavailable: u32,
    pub overflowed: u32,
    pub partial: u32,
}

/// An entry borrows its name for no longer than its sampled owner is borrowed.
/// ```compile_fail
/// use viewer::live_app_response::RankedApp;
/// use viewer::store::SampledRank;
/// fn escape(rank: SampledRank<'_>) -> &'static RankedApp {
///     rank.app
/// }
/// ```
#[derive(Debug, PartialEq)]
pub struct SampledRank<'sample> {
    pub app: &'sample RankedApp,
    pub amount: u64,
    pub rate: Option<f64>,
    pub measured_members: u32,
    pub unavailable_members: u32,
}

impl SampledSecond {
    /// Retains one observation's machine reading and bounded hover rankings on `budget`.
    pub fn from_second(
        machine: &MachineSecond,
        apps: &AppsSecond,
        budget: &Arc<ByteBudget>,
    ) -> Self {
        let mut sample = Self::empty();
        sample.take(machine, apps, budget);
        sample
    }

    pub fn ranks(&self) -> Result<SampledRanks<'_>, &Missing> {
        self.ranks.as_ref().map(SampledRanks::from)
    }

    pub(super) fn empty() -> Self {
        Self {
            machine: MachineSecond::default(),
            observation: Observation {
                recording: [0; 2],
                stamp: Stamp {
                    boot: [0; 2],
                    run: [0; 2],
                    publication: 0,
                    continuous_ns: None,
                    awake_ns: None,
                },
                run_tick: 0,
                wall_ns: None,
            },
            ranks: Err(Missing::Unread),
            reservation: None,
        }
    }

    pub(super) fn take(
        &mut self,
        machine: &MachineSecond,
        apps: &AppsSecond,
        budget: &Arc<ByteBudget>,
    ) {
        self.ranks = Err(Missing::Unread);
        self.reservation = None;
        self.machine = machine.clone();
        self.observation = apps.observation.unwrap_or_else(|| unreachable!());
        assert_eq!(self.observation.stamp.publication, machine.publication);
        assert_eq!(self.observation.wall_ns, Some(machine.wall_ns));
        let reservation = budget.reserve(Self::take_bytes(apps));
        match reservation {
            Some(reservation) => {
                self.ranks = apps.ranks();
                self.reservation = Some(reservation);
            }
            None => self.ranks = Err(Missing::Budget),
        }
    }

    fn take_bytes(apps: &AppsSecond) -> usize {
        let name_bytes = apps
            .apps
            .iter()
            .map(|app| {
                size_of::<RankedApp>()
                    + 2 * size_of::<usize>()
                    + app.name.bytes.capacity()
                    + app.name.display_name.capacity()
            })
            .max()
            .unwrap_or(0);
        let refusal_bytes = match &apps.missing {
            Some(Missing::Refused(reason)) => reason.len(),
            _ => 0,
        };
        size_of::<Self>()
            + METRICS.len() * ((RANKS_MAX + 1) * size_of::<Rank>() + RANKS_MAX * name_bytes)
            + refusal_bytes
    }
}

impl<'sample> From<&'sample Ranks> for SampledRanks<'sample> {
    fn from(ranks: &'sample Ranks) -> Self {
        Self {
            selected: ranks.selected,
            app_count: ranks.app_count,
            membership_complete: ranks.membership_complete,
            ungrouped_members: ranks.ungrouped_members,
            metrics: ranks.metrics.each_ref().map(SampledRanking::from),
        }
    }
}

impl<'sample> From<&'sample Ranking> for SampledRanking<'sample> {
    fn from(ranking: &'sample Ranking) -> Self {
        Self {
            metric: ranking.metric,
            entries: &ranking.entries,
            unavailable: ranking.unavailable,
            overflowed: ranking.overflowed,
            partial: ranking.partial,
        }
    }
}

impl<'sample> SampledRanking<'sample> {
    pub fn entries(&self) -> impl ExactSizeIterator<Item = SampledRank<'sample>> {
        self.entries.iter().map(SampledRank::from)
    }
}

impl<'sample> From<&'sample Rank> for SampledRank<'sample> {
    fn from(rank: &'sample Rank) -> Self {
        Self {
            app: &rank.app,
            amount: rank.amount,
            rate: rank.rate,
            measured_members: rank.measured_members,
            unavailable_members: rank.unavailable_members,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_ranking_reserves_the_rankers_spare_capacity() {
        // Goal: the hover reservation includes allocated spare ranking slots, not only entries.
        // Method: rank an empty observation and count the actual owned vector capacities.
        let machine = MachineSecond::default();
        let mut sample = SampledSecond::empty();
        sample.observation.wall_ns = Some(machine.wall_ns);
        let mut apps = AppsSecond::with_room();
        apps.observation = Some(sample.observation);
        apps.missing = None;
        let budget = Arc::new(ByteBudget::new(1024 * 1024));

        sample.take(&machine, &apps, &budget);

        let ranks = sample.ranks.as_ref().unwrap();
        let owned = size_of::<SampledSecond>()
            + ranks
                .metrics
                .iter()
                .map(|ranking| ranking.entries.capacity() * size_of::<Rank>())
                .sum::<usize>();
        assert!(
            budget.used() >= owned,
            "reserved {}, owned {owned}",
            budget.used()
        );
    }
}
