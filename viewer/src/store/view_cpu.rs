//! The CPU page's history of its logical CPUs and the logical CPUs of the newest second, built
//! from a View as a context that follows the newest second builds them, so a page that follows
//! the live Mac draws them with no read.
use collector::recording::identity_text;
use collector::ring::Identity as SourceIdentity;
use persistence::{Error, Result};

use super::context_cpu::{self, CpuSecond, HeldObservation, HistoryRequest};
use super::{CpuDetail, CpuHistory, CpuHistoryIdentity, View, selection};
use crate::live_app_response::Observation;
use crate::session::ContextRange;

const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;

/// The logical CPUs the CPU page draws of a View: the history of the 900 seconds that end at its
/// newest second, with the logical CPUs of the first page, and the logical CPUs of that second,
/// with the reading their interval runs from where the View holds it.
pub struct LogicalCpus {
    pub history: CpuHistory,
    pub detail: CpuDetail,
}

impl View {
    /// The logical CPUs of the View's newest second and their history, from one pass over the
    /// identities the View's Apps hold.
    pub fn logical_cpus(&self) -> Result<LogicalCpus> {
        let newest = self.logical_cpus_newest()?;
        let observations = self.logical_cpus_observations();

        Ok(LogicalCpus {
            history: self.logical_cpus_history(&newest, &observations)?,
            detail: logical_cpus_detail(&newest, &observations)?,
        })
    }

    /// The CPU history of the 900 seconds that end at `newest`, read from the seconds whose
    /// identities `observations` hold in the same order.
    fn logical_cpus_history(
        &self,
        newest: &NewestCpu<'_>,
        observations: &[HeldObservation],
    ) -> Result<CpuHistory> {
        let context_last_second = newest.machine.wall_ns / NANOSECONDS_PER_SECOND;
        let range = ContextRange::ending_at(context_last_second)?;
        let mut seconds = Vec::with_capacity(self.seconds.len());
        for (machine, held) in self.seconds.iter().zip(observations) {
            if context_cpu::history_in_range(&held.observation, range) {
                seconds.push(CpuSecond {
                    observation: &held.observation,
                    machine,
                });
            }
        }
        seconds.sort_by(|before, after| selection::compare(before.observation, after.observation));
        let selected = super::context_response::identity(&newest.observation, newest.origin);

        context_cpu::history_of(
            &seconds,
            &HistoryRequest {
                selected: &selected,
                context_last_second,
                offset: 0,
            },
            || Ok(()),
        )
    }

    /// The identity the Apps of each of the View's seconds hold, in the same order.
    fn logical_cpus_observations(&self) -> Vec<HeldObservation> {
        assert_eq!(self.apps.len(), self.seconds.len());

        self.apps
            .seconds()
            .map(|apps| HeldObservation {
                observation: apps
                    .observation()
                    .unwrap_or_else(|| unreachable!("every accepted second has an identity")),
                origin: apps.origin(),
            })
            .collect()
    }

    fn logical_cpus_newest(&self) -> Result<NewestCpu<'_>> {
        let machine = self
            .seconds
            .last()
            .ok_or(Error::Bound("CPU reading of an empty View"))?;
        let apps = self
            .apps
            .newest()
            .ok_or(Error::Bound("CPU reading of an empty View"))?;
        let observation = apps
            .observation()
            .unwrap_or_else(|| unreachable!("every accepted second has an identity"));
        assert_eq!(observation.stamp.publication, machine.publication);

        Ok(NewestCpu {
            observation,
            origin: apps.origin(),
            machine,
        })
    }
}

/// The logical CPUs of `newest`, with the reading their interval runs from found among
/// `observations`.
fn logical_cpus_detail(
    newest: &NewestCpu<'_>,
    observations: &[HeldObservation],
) -> Result<CpuDetail> {
    let detail = context_cpu::detail_of(
        &newest.observation,
        newest.machine,
        observations.iter().copied(),
    )?;
    let previous = match detail.previous {
        Some(before) => Some(logical_cpus_detail_identity(&before.observation)?),
        None => None,
    };

    Ok(CpuDetail {
        identity: logical_cpus_detail_identity(&newest.observation)?,
        previous,
        unavailable_reason: detail.unavailable,
        busy_percent: detail.values.to_vec(),
    })
}

/// The View's newest second, with the identity its Apps hold.
struct NewestCpu<'a> {
    observation: Observation,
    origin: super::native::Origin,
    machine: &'a super::MachineSecond,
}

/// The identity the page names `observation` by. The persister writes the wall clock of the
/// Apps' reading, so a second without one is refused rather than asserted.
fn logical_cpus_detail_identity(observation: &Observation) -> Result<CpuHistoryIdentity> {
    Ok(CpuHistoryIdentity {
        run: identity_text(SourceIdentity(observation.stamp.run)).to_string(),
        boot: identity_text(SourceIdentity(observation.stamp.boot)).to_string(),
        publication: observation.stamp.publication,
        run_tick: observation.run_tick,
        wall_ns: observation
            .wall_ns
            .ok_or(Error::Bound("CPU reading clock"))?,
    })
}
