use std::sync::Arc;

use persistence::{Error, Result};
use serde_json::{Value, json};

use super::bounds::{TRACE_POINT_BYTES, history_result_bytes};
use super::{QueryContext, QuerySecond, Response};
use crate::Cancellation;
use crate::cancellation::Operation;
use crate::live::resident_history::{Interval, consecutive};
use crate::live_app_history::{GroupTarget, Selection, Target, TracePoint, add_process_view};
use crate::live_app_response::{KeyKind, Selected};

impl QueryContext {
    /// One App's exact retained measures, with the shared legacy trace JSON/rate formatter.
    pub fn app_history(
        &self,
        kind: KeyKind,
        bytes: &[u8],
        cancellation: &Arc<Cancellation>,
        epoch: u64,
    ) -> crate::store::HistoryResult<Response<Value>> {
        self.history_ready()?;
        kind.checked_key(bytes)?;
        let operation = Arc::clone(cancellation).operation(epoch)?;
        let reservation = self.reserve_response(history_result_bytes(self, TRACE_POINT_BYTES))?;
        let scratch = self.reserve_response(self.seconds().len() * size_of::<&QuerySecond>())?;

        Ok(operation.run(|| {
            let value = self.response_trace(&Target { kind, bytes }, &operation)?;
            let response = Response::json(reservation, value)?;
            drop(scratch);

            Ok(response)
        })?)
    }

    /// One captured executable-name group within one App, using retained process amounts.
    pub fn app_process_history(
        &self,
        kind: KeyKind,
        app_bytes: &[u8],
        executable: &[u8],
        cancellation: &Arc<Cancellation>,
        epoch: u64,
    ) -> crate::store::HistoryResult<Response<Value>> {
        self.history_ready()?;
        let target = GroupTarget {
            app: Target {
                kind,
                bytes: app_bytes,
            },
            executable,
        };
        target.validate()?;
        let operation = Arc::clone(cancellation).operation(epoch)?;
        let reservation = self.reserve_response(
            history_result_bytes(self, TRACE_POINT_BYTES)
                + 8 * (app_bytes.len() + executable.len()),
        )?;
        let scratch = self.reserve_response(self.seconds().len() * size_of::<&QuerySecond>())?;

        Ok(operation.run(|| {
            let mut response = self.response_trace(&target, &operation)?;
            target.identify(&mut response)?;
            let response = Response::json(reservation, response)?;
            drop(scratch);

            Ok(response)
        })?)
    }

    fn response_trace(&self, target: &dyn Selection, operation: &Operation) -> Result<Value> {
        let selected = Selected::new(self.selected().observation, self.context_last_second())?;
        let mut seconds: Vec<_> = self
            .seconds()
            .iter()
            .filter(|second| {
                second
                    .observation
                    .wall_ns
                    .is_some_and(|wall| selected.holds(wall / 1_000_000_000))
            })
            .collect();
        seconds.sort_by_key(|second| {
            (
                second.observation.wall_ns,
                second.observation.stamp.boot,
                second.observation.stamp.run,
                second.observation.stamp.publication,
            )
        });
        let mut previous = None;
        let mut segment = 0_u64;
        let merged = if self.has_committed_history() {
            true
        } else {
            target.executable().is_some()
        };
        let mut missing = false;
        let mut points = Vec::with_capacity(seconds.len());
        for second in seconds {
            operation.check()?;
            if merged {
                if let Some(before) = previous {
                    if !consecutive(&Interval {
                        before,
                        after: &second.observation,
                    }) {
                        segment += 1;
                    }
                }
            } else {
                segment = second
                    .machine()
                    .segment
                    .checked_add(1)
                    .ok_or(Error::Bound("segment"))?;
            }
            let point = response_trace_point(second, target)?;
            missing |= !point.source_available();
            points.push(point.json(segment, target.executable().is_some()));
            previous = Some(&second.observation);
        }

        let mut response = json!({"selected": self.selected().observation.json(),
            "context_seconds": 900, "points": null, "coverage": {
                "retention_limited": missing, "observations_retained": points.len(),
                "context_metadata_bytes": self.bytes()}});
        response["points"] = Value::Array(points);

        crate::live_app_response::bounded(response)
    }
}

fn response_trace_point(second: &QuerySecond, target: &dyn Selection) -> Result<TracePoint> {
    let apps = second.apps();
    if apps.missing.is_some() {
        return Ok(TracePoint::unavailable(&second.observation));
    }
    if apps.observation != Some(second.observation) {
        return Err(Error::Bound("live App stale detail"));
    }
    let index = apps.apps.iter().position(|app| {
        app.name.kind.key(&app.name.bytes) == target.app().kind.key(target.app().bytes)
    });
    let Some(index) = index else {
        return Ok(TracePoint::from_app_view(
            &second.observation,
            apps.elapsed_ns,
            None,
        ));
    };
    let Ok(payload) = &apps.processes else {
        return Ok(TracePoint::unavailable(&second.observation));
    };
    apps.presentation()?;
    if let Some(executable) = target.executable() {
        let app_index = u32::try_from(index).map_err(|_| Error::Bound("live App index"))?;
        let mut totals = crate::apps::AppTotals::default();
        for process in payload
            .iter()
            .filter(|process| process.app_index == Some(app_index))
        {
            if let Some(name) = process.executable_name() {
                if name.bytes == executable {
                    add_process_view(&mut totals, &process.presentation());
                }
            }
        }
        totals.membership_complete = apps.membership_complete;
        let present = (totals.members > 0).then_some(&totals);

        return Ok(TracePoint::from_totals(
            &second.observation,
            apps.elapsed_ns,
            present,
        ));
    }

    let app = apps.apps[index].presentation(&payload.apps()[index]);

    Ok(TracePoint::from_app_view(
        &second.observation,
        apps.elapsed_ns,
        Some(&app),
    ))
}
