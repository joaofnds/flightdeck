//! CPU queries format the intervals captured once by the canonical producer.
use std::sync::Arc;

use collector::recording::identity_text;
use collector::ring::Identity as SourceIdentity;
use persistence::{Error, Result};
use serde_json::Value;

use super::context_response::identity;
use super::{ByteReservation, CpuPrevious, QueryContext, QuerySecond, Response};
use crate::Cancellation;
use crate::cpu_history::{CpuHistory, CpuHistoryObservation, CpuHistoryRow, PAGE_CPUS};
use crate::cpu_history_sample::{IDENTITY_TEXT_BYTES_MAX, Identity};
use crate::live_app_response::Observation;
use crate::session::{ContextRange, OBSERVATIONS_MAX};

const DETAIL_WORK_BYTES_MAX: usize = 512 * 1024;
/// What a CPU history response may be charged: the largest history the Store builds and what the
/// page builds beside it, which the block below sizes.
const HISTORY_CHARGE_BYTES_MAX: usize = 8 * 1024 * 1024;
const _: () = {
    // The count below holds each observation at this size, so a field added to it must be
    // counted there before this passes.
    assert!(size_of::<CpuHistoryObservation>() == 152);
    // Each observation holds an identity and its predecessor, each a run and a boot text of at
    // most the length `Identity::read` admits.
    let observation_bytes = size_of::<CpuHistoryObservation>() + 4 * IDENTITY_TEXT_BYTES_MAX;
    let row_bytes = size_of::<CpuHistoryRow>() + OBSERVATIONS_MAX * size_of::<Option<f64>>();
    let held_bytes_max =
        size_of::<CpuHistory>() + OBSERVATIONS_MAX * observation_bytes + PAGE_CPUS * row_bytes;
    assert!(size_of::<Response<CpuHistory>>() + 2 * held_bytes_max <= HISTORY_CHARGE_BYTES_MAX);
};

impl QueryContext {
    pub fn cpu_detail(
        &self,
        cancellation: &Arc<Cancellation>,
        epoch: u64,
    ) -> Result<Response<Value>> {
        self.cpu_detail_for(&self.selected().observation, cancellation, epoch)
    }

    pub fn cpu_detail_for(
        &self,
        observation: &Observation,
        cancellation: &Arc<Cancellation>,
        epoch: u64,
    ) -> Result<Response<Value>> {
        let operation = Arc::clone(cancellation).operation(epoch)?;
        operation.check()?;
        let reservation = self.reserve_response(DETAIL_WORK_BYTES_MAX)?;
        let selected = self
            .inspect(observation)
            .map_err(|_| Error::Bound("CPU selection"))?;
        let interval = selected
            .machine()
            .cpu_interval()?
            .ok_or(Error::Bound("CPU reading unavailable"))?;
        let current = identity(observation, selected.apps().origin());
        let mut previous = None;
        detail_previous(self, selected, interval.previous, &mut previous)?;
        let unavailable = if previous.is_some() {
            interval.unavailable_reason
        } else {
            Some("Previous CPU reading unavailable")
        };
        let value = crate::cpu_detail::project_values(&crate::cpu_detail::Presentation {
            identity: &current,
            previous: previous.as_ref(),
            unavailable,
            values: interval.values,
        })?;
        operation.check()?;

        Response::json(reservation, value)
    }

    pub fn cpu_history(
        &self,
        offset: u32,
        cancellation: &Arc<Cancellation>,
        epoch: u64,
    ) -> Result<Response<CpuHistory>> {
        let operation = Arc::clone(cancellation).operation(epoch)?;
        operation.check()?;
        let work = self.reserve_response(history_work_bytes(self)?)?;
        let reservation = self.reserve_response(HISTORY_CHARGE_BYTES_MAX)?;
        let range = ContextRange::ending_at(self.context_last_second())?;
        let mut context = Vec::with_capacity(self.seconds().len());
        for second in self.seconds() {
            operation.check()?;
            if history_in_range(second, range) {
                history_identity(second, &mut context);
            }
        }
        let mut retained = Vec::with_capacity(context.len());
        for (second, current) in self
            .seconds()
            .iter()
            .filter(|second| history_in_range(second, range))
            .zip(&context)
        {
            operation.check()?;
            history_retained(second, current, &mut retained)?;
        }
        let selected = identity(
            &self.selected().observation,
            self.selected().apps().origin(),
        );
        let history = crate::cpu_history::project_history(&crate::cpu_history::Request {
            selected: &selected,
            context_last_second: self.context_last_second(),
            context: &context,
            saved: None,
            retained: Some(&retained),
            offset,
        })?;
        operation.check()?;
        drop(work);

        cpu_history_response(reservation, history)
    }
}

/// Charges `history` to `reservation`, which holds at least the charge bound. The page takes over
/// the history's texts and rows of readings, and adds its own vector of observations, its
/// intervals and one copy of the selected identity. Twice what the history holds covers the
/// moment both vectors of observations exist, with what the page adds.
fn cpu_history_response(
    mut reservation: ByteReservation,
    history: CpuHistory,
) -> Result<Response<CpuHistory>> {
    let bytes = size_of::<Response<CpuHistory>>() + 2 * history.held_bytes();
    if bytes > HISTORY_CHARGE_BYTES_MAX {
        return Err(Error::Bound("CPU history response bytes"));
    }

    assert!(bytes <= reservation.bytes());
    reservation.shrink_to(bytes);

    Ok(Response::reserved(reservation, history))
}

fn detail_previous(
    context: &QueryContext,
    current: &QuerySecond,
    previous: Option<&CpuPrevious>,
    output: &mut Option<Value>,
) -> Result<()> {
    assert!(output.is_none());
    let Some(previous) = previous else {
        return Ok(());
    };
    if previous.publication.checked_add(1) != Some(current.observation.stamp.publication) {
        return Ok(());
    }
    for before in context.seconds() {
        if detail_previous_matches(&before.observation, &current.observation, previous) {
            *output = Some(identity(&before.observation, before.apps().origin()));

            return Ok(());
        }
    }
    if let Some(before) = current.outside_cpu_predecessor() {
        if detail_previous_matches(&before.observation, &current.observation, previous) {
            *output = Some(identity(&before.observation, before.origin));

            return Ok(());
        }
    }

    Err(Error::Bound("CPU predecessor metadata not retained"))
}

fn detail_previous_matches(
    before: &Observation,
    current: &Observation,
    previous: &CpuPrevious,
) -> bool {
    before.recording == current.recording
        && before.stamp.run == current.stamp.run
        && before.stamp.boot == current.stamp.boot
        && before.stamp.publication == previous.publication
        && before.run_tick == previous.run_tick
        && before.wall_ns == Some(previous.wall_ns)
        && before.stamp.continuous_ns == previous.continuous_ns
}

fn history_in_range(second: &QuerySecond, range: ContextRange) -> bool {
    second
        .observation
        .wall_ns
        .is_some_and(|wall| (range.first_ns..range.end_ns).contains(&wall))
}

fn history_identity(second: &QuerySecond, output: &mut Vec<Identity>) {
    let observation = &second.observation;
    output.push(Identity {
        run: identity_text(SourceIdentity(observation.stamp.run)).to_string(),
        boot: identity_text(SourceIdentity(observation.stamp.boot)).to_string(),
        observation: crate::session::Observation {
            window: 0,
            publication: observation.stamp.publication,
            run_tick: observation.run_tick,
            wall: second.machine().wall_ns,
            continuous: observation.stamp.continuous_ns,
        },
    });
}

fn history_retained<'a>(
    second: &'a QuerySecond,
    current: &'a Identity,
    output: &mut Vec<crate::cpu_history::RetainedEntry<'a>>,
) -> Result<()> {
    let Some(interval) = second.machine().cpu_interval()? else {
        return Ok(());
    };
    let mut previous = None;
    if let Some(before) = interval.previous {
        previous = Some(Identity {
            run: current.run.clone(),
            boot: current.boot.clone(),
            observation: crate::session::Observation {
                window: 0,
                publication: before.publication,
                run_tick: before.run_tick,
                wall: before.wall_ns,
                continuous: before.continuous_ns,
            },
        });
    }
    output.push(crate::cpu_history::RetainedEntry {
        identity: current,
        previous,
        interval_available: interval.unavailable_reason.is_none(),
        values: interval.values,
    });

    Ok(())
}

fn history_work_bytes(context: &QueryContext) -> Result<usize> {
    assert!(context.seconds().len() <= super::SECONDS_MAX);

    crate::cpu_history::retained_work_bytes(context.seconds().len())
}

#[cfg(test)]
#[path = "context_cpu_tests.rs"]
mod tests;
