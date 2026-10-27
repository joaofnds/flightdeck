use std::sync::Arc;

use persistence::{Error, Result};
use serde_json::{Value, json};

use super::bounds::{ATTRIBUTION_POINT_BYTES, RESIDENT_POINT_BYTES, history_result_bytes};
use super::{QueryContext, QuerySecond, Response};
use crate::Cancellation;
use crate::app_attribution_history::{self, AppAttributionMetric};
use crate::cancellation::Operation;
use crate::live::resident_history::{self, Source};
use crate::live_app_response::{Metric, Rankable, Selected};
use crate::store::AppsSecond;

impl QueryContext {
    /// The existing resident cohort/point reduction, fed solely by retained compact App rows.
    pub fn app_resident_history(
        &self,
        cancellation: &Arc<Cancellation>,
        epoch: u64,
    ) -> Result<Response<Value>> {
        let operation = Arc::clone(cancellation).operation(epoch)?;
        let reservation =
            self.reserve_response(history_result_bytes(self, RESIDENT_POINT_BYTES))?;
        let scratch = self.reserve_response(resident_history::WORKING_BYTES_MAX)?;

        operation.run(|| {
            let response = Response::json(reservation, self.response_resident(&operation)?)?;
            drop(scratch);

            Ok(response)
        })
    }

    fn response_resident(&self, operation: &Operation) -> Result<Value> {
        let selected = Selected::new(self.selected().observation, self.context_last_second())?;
        let mut reduction = resident_history::Reduction::new(&selected, operation);
        for second in self.seconds() {
            if response_apps_available(second)? {
                reduction.observe(
                    &response_capture(second),
                    response_residents(second.apps()),
                    Source::Live,
                )?;
            }
        }
        reduction.choose()?;
        for second in self.seconds() {
            if response_apps_available(second)? {
                reduction.project(
                    &response_capture(second),
                    response_residents(second.apps()),
                    Source::Live,
                )?;
            }
            reduction.retain_metadata(&second.observation)?;
        }

        self.response_history_finish(reduction.response()?)
    }

    /// The existing mean-rate cohort/point reduction over the retained context's App measures.
    pub fn app_attribution_history(
        &self,
        metric: AppAttributionMetric,
        cancellation: &Arc<Cancellation>,
        epoch: u64,
    ) -> Result<Response<Value>> {
        let operation = Arc::clone(cancellation).operation(epoch)?;
        let reservation =
            self.reserve_response(history_result_bytes(self, ATTRIBUTION_POINT_BYTES))?;
        let scratch = self.reserve_response(app_attribution_history::WORKING_BYTES_MAX)?;

        operation.run(|| {
            let response =
                Response::json(reservation, self.response_attribution(metric, &operation)?)?;
            drop(scratch);

            Ok(response)
        })
    }

    fn response_attribution(
        &self,
        metric: AppAttributionMetric,
        operation: &Operation,
    ) -> Result<Value> {
        let selected = Selected::new(self.selected().observation, self.context_last_second())?;
        let mut reduction = app_attribution_history::Reduction::new(&selected, metric, operation);
        for second in self.seconds() {
            if response_apps_available(second)? {
                reduction.observe(
                    &response_attribution_capture(second),
                    response_attribution_readings(second.apps(), metric),
                    Source::Live,
                )?;
            }
        }
        reduction.choose()?;
        for second in self.seconds() {
            if response_apps_available(second)? {
                reduction.project(
                    &response_attribution_capture(second),
                    response_attribution_readings(second.apps(), metric),
                    Source::Live,
                )?;
            }
            reduction.retain_metadata(&second.observation)?;
        }

        self.response_history_finish(reduction.response()?)
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

fn response_apps_available(second: &QuerySecond) -> Result<bool> {
    let apps = second.apps();
    if apps.observation != Some(second.observation) {
        return Err(Error::Bound("live App stale detail"));
    }

    Ok(apps.missing.is_none())
}

fn response_capture(second: &QuerySecond) -> resident_history::Capture {
    let apps = second.apps();

    resident_history::Capture {
        observation: second.observation,
        membership_complete: apps.membership_complete,
        ungrouped_members: apps.live_ungrouped_members,
        usage_refused: apps.usage_refused,
        usage_gone: apps.usage_gone,
    }
}

fn response_residents(apps: &AppsSecond) -> impl Iterator<Item = resident_history::AppReading<'_>> {
    apps.apps
        .iter()
        .map(|app| resident_history::AppReading {
            key: app.name.kind.key(&app.name.bytes),
            resident: app.measure(Metric::Resident),
        })
        .filter(|app| resident_history::has_resident(&app.resident))
}

fn response_attribution_capture(second: &QuerySecond) -> app_attribution_history::Capture {
    let mut coverage = response_capture(second);
    coverage.ungrouped_members = second.apps().ungrouped_members;

    app_attribution_history::Capture {
        coverage,
        elapsed_ns: second.apps().elapsed_ns,
    }
}

fn response_attribution_readings(
    apps: &AppsSecond,
    metric: AppAttributionMetric,
) -> impl Iterator<Item = app_attribution_history::Reading<'_>> {
    let metric = match metric {
        AppAttributionMetric::Cpu => Metric::Cpu,
        AppAttributionMetric::Power => Metric::Power,
        AppAttributionMetric::Disk => Metric::Disk,
        AppAttributionMetric::Gpu => Metric::Gpu,
    };

    apps.apps
        .iter()
        .map(move |app| app_attribution_history::Reading {
            key: app.name.kind.key(&app.name.bytes),
            measure: app.measure(metric),
            members: app.members,
            membership_complete: app.membership_complete,
        })
}
