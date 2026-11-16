use std::sync::Arc;

use persistence::{Error, Result};
use serde_json::{Value, json};

use super::bounds::{ATTRIBUTION_POINT_BYTES, RESIDENT_POINT_BYTES, history_result_bytes};
use super::{QueryContext, Response};
use crate::Cancellation;
use crate::app_attribution_history::cache::Total;
use crate::app_attribution_history::{self, AppAttributionMetric};
use crate::cancellation::Operation;
use crate::live::resident_history::{self, Source};
use crate::live_app_response::{Metric, Observation, Rankable, Selected};
use crate::store::{AppsCursor, BlockSeconds, Second};

impl QueryContext {
    /// The existing resident cohort/point reduction, fed solely by retained compact App rows.
    pub fn app_resident_history(
        &self,
        cancellation: &Arc<Cancellation>,
        epoch: u64,
    ) -> crate::store::HistoryResult<Response<Value>> {
        self.history_ready()?;
        let operation = Arc::clone(cancellation).operation(epoch)?;
        let reservation =
            self.reserve_response(history_result_bytes(self, RESIDENT_POINT_BYTES))?;
        let scratch = self.reserve_response(
            resident_history::WORKING_BYTES_MAX
                + FOUND_MAX * size_of::<ResidentFound>()
                + AppsCursor::WORKING_BYTES_MAX,
        )?;

        Ok(operation.run(|| {
            let response = Response::json(reservation, self.response_resident(&operation)?)?;
            drop(scratch);

            Ok(response)
        })?)
    }

    fn response_resident(&self, operation: &Operation) -> Result<Value> {
        let selected = Selected::new(self.selected().observation, self.context_last_second())?;

        self.response_history_finish(response_resident_reduce(
            self.history_seconds(),
            &selected,
            operation,
        )?)
    }

    /// The existing mean-rate cohort/point reduction over the retained context's App measures.
    pub fn app_attribution_history(
        &self,
        metric: AppAttributionMetric,
        cancellation: &Arc<Cancellation>,
        epoch: u64,
    ) -> crate::store::HistoryResult<Response<Value>> {
        self.history_ready()?;
        let operation = Arc::clone(cancellation).operation(epoch)?;
        let reservation =
            self.reserve_response(history_result_bytes(self, ATTRIBUTION_POINT_BYTES))?;
        let scratch = self.reserve_response(
            app_attribution_history::WORKING_BYTES_MAX
                + FOUND_MAX * size_of::<AttributionFound>()
                + AppsCursor::WORKING_BYTES_MAX,
        )?;

        Ok(operation.run(|| {
            let response =
                Response::json(reservation, self.response_attribution(metric, &operation)?)?;
            drop(scratch);

            Ok(response)
        })?)
    }

    fn response_attribution(
        &self,
        metric: AppAttributionMetric,
        operation: &Operation,
    ) -> Result<Value> {
        let selected = Selected::new(self.selected().observation, self.context_last_second())?;

        self.response_history_finish(response_attribution_reduce(
            self.history_seconds(),
            &selected,
            metric,
            operation,
        )?)
    }

    /// The context's seconds as a history reads them.
    fn history_seconds(&self) -> impl Iterator<Item = HistorySecond<'_>> + Clone {
        self.seconds().iter().map(|second| HistorySecond {
            observation: second.observation,
            apps: second.apps(),
        })
    }

    fn response_history_finish(&self, mut response: Value) -> Result<Value> {
        let count = response["points"]
            .as_array()
            .ok_or(Error::Bound("App history merged observations"))?
            .len();
        response["coverage"]["observations_retained"] = json!(count);
        response["coverage"]["context_metadata_bytes"] = json!(self.bytes());

        crate::live_app_response::bounded(response)
    }
}

/// The resident history's response over `seconds` for `selected`, before the context adds its
/// coverage: every App's mean over the seconds, the cohort they choose and each second's point.
pub(in crate::store) fn response_resident_reduce<'context>(
    seconds: impl Iterator<Item = HistorySecond<'context>> + Clone,
    selected: &Selected,
    operation: &Operation,
) -> Result<Value> {
    let mut reduction = resident_history::Reduction::new(selected, operation);
    let mut cursor = AppsCursor::default();
    let mut block = None;
    let mut found = Vec::with_capacity(FOUND_MAX);
    for second in seconds.clone() {
        if !BlockSeconds::holds(block.as_ref(), second.apps) {
            response_resident_merge(&mut reduction, block.take(), &mut found)?;
        }
        let observed = response_resident_observe(&mut reduction, &mut cursor, second, &mut block);
        if let Err(error) = observed {
            // The Apps of the block's seconds before this one reach the means first, as they
            // would one second at a time, so a bound they meet is the error.
            response_resident_merge(&mut reduction, block.take(), &mut found)?;
            return Err(error);
        }
    }
    response_resident_merge(&mut reduction, block.take(), &mut found)?;
    reduction.choose()?;
    for second in seconds {
        if response_apps_available(second)? {
            reduction.project(
                &response_capture(second),
                response_residents(&mut cursor, second.apps),
                Source::Live,
            )?;
        }
        reduction.retain_metadata(&second.observation)?;
    }

    reduction.response()
}

/// The attribution history's response of `metric` over `seconds` for `selected`, before the
/// context adds its coverage: every App's mean over the seconds, the cohort they choose and each
/// second's point.
pub(in crate::store) fn response_attribution_reduce<'context>(
    seconds: impl Iterator<Item = HistorySecond<'context>> + Clone,
    selected: &Selected,
    metric: AppAttributionMetric,
    operation: &Operation,
) -> Result<Value> {
    let mut reduction = app_attribution_history::Reduction::new(selected, metric, operation);
    let mut cursor = AppsCursor::default();
    let mut block = None;
    let mut found = Vec::with_capacity(FOUND_MAX);
    for second in seconds.clone() {
        if !BlockSeconds::holds(block.as_ref(), second.apps) {
            response_attribution_merge(&mut reduction, block.take(), metric, &mut found)?;
        }
        let observed =
            response_attribution_observe(&mut reduction, &mut cursor, second, metric, &mut block);
        if let Err(error) = observed {
            // The Apps of the block's seconds before this one reach the means first, as they
            // would one second at a time, so a bound they meet is the error.
            response_attribution_merge(&mut reduction, block.take(), metric, &mut found)?;
            return Err(error);
        }
    }
    response_attribution_merge(&mut reduction, block.take(), metric, &mut found)?;
    reduction.choose()?;
    for second in seconds {
        if response_apps_available(second)? {
            reduction.project(
                &response_attribution_capture(second),
                response_attribution_readings(&mut cursor, second.apps, metric),
                Source::Live,
            )?;
        }
        reduction.retain_metadata(&second.observation)?;
    }

    reduction.response()
}

/// One second of a context as a history reads it: its observation and its Apps.
#[derive(Clone, Copy)]
pub(in crate::store) struct HistorySecond<'context> {
    pub(in crate::store) observation: Observation,
    pub(in crate::store) apps: Second<'context>,
}

/// The most Apps a history's first pass merges from one packed block: every App the block holds,
/// which is at most a second's bound.
const FOUND_MAX: usize = crate::store::APPS_PER_SECOND_MAX;

/// An App of a packed block a history's first pass merges, with what its readings over the
/// block's seconds add to its mean.
#[derive(Clone, Copy)]
struct Found<Sum> {
    /// The index of the first second of the block that holds the App.
    first: u32,
    /// The App's place in the block.
    place: u32,
    sum: Sum,
}

impl<Sum> Found<Sum> {
    /// The order the Apps reach the means in: by the first second that holds each, then in key
    /// order, which is the order of their places.
    fn order(&self) -> (u32, u32) {
        (self.first, self.place)
    }
}

type ResidentFound = Found<resident_history::Samples>;

type AttributionFound = Found<Total>;

fn response_apps_available(second: HistorySecond<'_>) -> Result<bool> {
    let apps = second.apps;
    if apps.observation() != Some(second.observation) {
        return Err(Error::Bound("live App stale detail"));
    }

    Ok(apps.missing().is_none())
}

/// Adds `second` to the first pass of `reduction`, its Apps at once where it holds its own and,
/// where its block's table holds them, to `block` for `response_resident_merge`.
fn response_resident_observe<'context>(
    reduction: &mut resident_history::Reduction<'_>,
    cursor: &mut AppsCursor,
    second: HistorySecond<'context>,
    block: &mut Option<BlockSeconds<'context>>,
) -> Result<()> {
    if !response_apps_available(second)? {
        return Ok(());
    }
    let capture = response_capture(second);
    let Some((apps, index)) = second.apps.block_apps() else {
        return reduction.observe(
            &capture,
            response_residents(cursor, second.apps),
            Source::Live,
        );
    };
    if reduction.observe_point(&capture, Source::Live)? {
        BlockSeconds::add(block, apps, index, second.apps.elapsed_ns());
    }

    Ok(())
}

/// Adds to the means of `reduction` each App of `block`, where there is one, summed over the
/// seconds whose points it added, as `response_residents` and `observe` add them one second at a
/// time: an App whose reading at a second has no resident memory is not read there. The Apps
/// reach the means in the order those seconds first show them, by the first second that holds
/// each and then in key order. `found` is room for them.
fn response_resident_merge(
    reduction: &mut resident_history::Reduction<'_>,
    block: Option<BlockSeconds<'_>>,
    found: &mut Vec<ResidentFound>,
) -> Result<()> {
    let Some(block) = block else {
        return Ok(());
    };
    assert!(collector::usize_from_u32(block.apps.count()) <= found.capacity());
    found.clear();
    for place in 0..block.apps.count() {
        let mut first = None;
        let mut samples = resident_history::Samples::default();
        block.apps.rows(place, block.seconds, |second_index, row| {
            let resident = row.measure(Metric::Resident);
            if !resident_history::has_resident(&resident) {
                return;
            }
            first.get_or_insert(second_index);
            samples.add(&resident);
        });
        if let Some(first) = first {
            found.push(Found {
                first,
                place,
                sum: samples,
            });
        }
    }
    found.sort_unstable_by_key(Found::order);
    for app in &*found {
        let name = block.apps.name(app.place);
        reduction.merge(&name.kind.key(&name.bytes), app.sum)?;
    }

    Ok(())
}

fn response_capture(second: HistorySecond<'_>) -> resident_history::Capture {
    let completeness = second.apps.completeness();

    resident_history::Capture {
        observation: second.observation,
        membership_complete: completeness.membership_complete,
        ungrouped_members: completeness.live_ungrouped_members,
        usage_refused: completeness.usage_refused,
        usage_gone: completeness.usage_gone,
    }
}

fn response_residents<'cursor>(
    cursor: &'cursor mut AppsCursor,
    apps: Second<'cursor>,
) -> impl Iterator<Item = resident_history::AppReading<'cursor>> {
    cursor
        .apps(apps)
        .iter()
        .map(|app| resident_history::AppReading {
            key: app.name.kind.key(&app.name.bytes),
            resident: app.measure(Metric::Resident),
        })
        .filter(|app| resident_history::has_resident(&app.resident))
}

fn response_attribution_capture(second: HistorySecond<'_>) -> app_attribution_history::Capture {
    let mut coverage = response_capture(second);
    coverage.ungrouped_members = second.apps.completeness().ungrouped_members;

    app_attribution_history::Capture {
        coverage,
        elapsed_ns: second.apps.elapsed_ns(),
    }
}

/// Adds `second` to the first pass of `reduction`, its Apps at once where it holds its own and,
/// where its block's table holds them, to `block` for `response_attribution_merge`.
fn response_attribution_observe<'context>(
    reduction: &mut app_attribution_history::Reduction<'_>,
    cursor: &mut AppsCursor,
    second: HistorySecond<'context>,
    metric: AppAttributionMetric,
    block: &mut Option<BlockSeconds<'context>>,
) -> Result<()> {
    if !response_apps_available(second)? {
        return Ok(());
    }
    let capture = response_attribution_capture(second);
    let Some((apps, index)) = second.apps.block_apps() else {
        return reduction.observe(
            &capture,
            response_attribution_readings(cursor, second.apps, metric),
            Source::Live,
        );
    };
    if reduction.observe_point(&capture, Source::Live)? {
        BlockSeconds::add(block, apps, index, capture.elapsed_ns);
    }

    Ok(())
}

/// Adds to the means of `reduction` each App of `block`, where there is one, summed over the
/// seconds whose points it added, as `observe` adds an App one second at a time: the App's
/// amount and the second's interval where the second has an interval and the App's measure is
/// measured. The Apps reach the means in the order those seconds first show them, by the first
/// second that holds each and then in key order. `found` is room for them.
fn response_attribution_merge(
    reduction: &mut app_attribution_history::Reduction<'_>,
    block: Option<BlockSeconds<'_>>,
    metric: AppAttributionMetric,
    found: &mut Vec<AttributionFound>,
) -> Result<()> {
    let Some(block) = block else {
        return Ok(());
    };
    let metric = crate::store::attribution::ranked_metric(metric);
    assert!(collector::usize_from_u32(block.apps.count()) <= found.capacity());
    found.clear();
    for place in 0..block.apps.count() {
        let mut first = None;
        let mut total = Total::default();
        block.apps.rows(place, block.seconds, |second_index, row| {
            first.get_or_insert(second_index);
            let elapsed_ns = block.elapsed_ns[collector::usize_from_u32(second_index)];
            total.add(Total::of_reading(&row.measure(metric), elapsed_ns));
        });
        if let Some(first) = first {
            found.push(Found {
                first,
                place,
                sum: total,
            });
        }
    }
    found.sort_unstable_by_key(Found::order);
    reduction.merge(found.iter().map(|app| {
        let name = block.apps.name(app.place);
        (name.kind.key(&name.bytes), app.sum)
    }))
}

fn response_attribution_readings<'cursor>(
    cursor: &'cursor mut AppsCursor,
    apps: Second<'cursor>,
    metric: AppAttributionMetric,
) -> impl Iterator<Item = app_attribution_history::Reading<'cursor>> {
    let metric = crate::store::attribution::ranked_metric(metric);

    cursor
        .apps(apps)
        .iter()
        .map(move |app| app_attribution_history::Reading {
            key: app.name.kind.key(&app.name.bytes),
            measure: app.measure(metric),
            members: app.members,
            membership_complete: app.membership_complete,
        })
}
