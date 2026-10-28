//! Existing desktop decoders consume bounded presentations of immutable Store facts.
use std::sync::Arc;

use serde_json::Value;
use viewer::live_app_response::{Observation, Page};
use viewer::store::{QueryContext, QueryReadiness, Response};

use super::ownership::{PartsWindow, RanksWindow};
use super::presentation::Presentation;
use crate::live_source::{Command, Context, Shared, Snapshot};

pub(super) fn snapshot(
    context: &Arc<QueryContext>,
    command: &mut Command,
    shared: &Shared,
    inspected: Option<&Observation>,
) -> Result<Box<Snapshot>, String> {
    snapshot_with_raw(context, context, command, shared, inspected)
}

pub(super) fn snapshot_with_raw(
    context: &Arc<QueryContext>,
    raw: &Arc<QueryContext>,
    command: &mut Command,
    shared: &Shared,
    inspected: Option<&Observation>,
) -> Result<Box<Snapshot>, String> {
    let observation = inspected.copied().unwrap_or(context.selected().observation);
    let selection = context
        .selection_for(
            &observation,
            inspected.is_some(),
            &shared.cancellation,
            command.epoch,
        )
        .map_err(|error| error.to_string())?;
    let mut snapshot = crate::live_source::reader_snapshot_owned(
        command.generation,
        context.catalog(),
        Presentation::Store(selection),
    );
    snapshot.histories_pending = context.readiness() == QueryReadiness::Primary;
    if command.request.route == "overview" {
        let page = raw
            .raw_page_for(&observation, "fans", 0, &shared.cancellation, command.epoch)
            .map_err(|error| error.to_string())?;
        snapshot
            .pages
            .insert("fans".into(), Presentation::Store(page));
    }
    if crate::live_source::route_ranks_apps(&command.request.route) {
        let ranks = context
            .selected_rank_status_for(&observation)
            .map_err(|error| error.to_string())?;
        let ranks = ranks.try_map(|ranks| match ranks {
            Ok(ranks) => Ok(crate::live_rankings::Rankings::Ready(Box::new(
                crate::live_rankings::from_ranks(&ranks),
            ))),
            Err(viewer::store::Missing::Refused(reason)) => Ok(
                crate::live_rankings::Rankings::Unavailable(crate::bounded::bounded(&reason, 512)),
            ),
            Err(viewer::store::Missing::TooMany) => {
                Ok(crate::live_rankings::Rankings::Unavailable(String::from(
                    "This observation has too many Apps",
                )))
            }
            Err(error) => Err(error.to_string()),
        })?;
        snapshot.rankings = Some(Arc::new(Presentation::Canonical(ranks)));
    }
    if command.request.route == "cpu" {
        cpu(context, &observation, command, shared, &mut snapshot)?;
    }
    if command.request.route == "apps" {
        apps(context, &observation, command, shared, &mut snapshot)?;
    }
    if command.context != Context::Keep {
        if context.readiness() == QueryReadiness::Complete {
            histories(context, command, shared, &mut snapshot)?;
        }
    }

    Ok(snapshot)
}

fn cpu(
    context: &QueryContext,
    observation: &Observation,
    command: &mut Command,
    shared: &Shared,
    snapshot: &mut Snapshot,
) -> Result<(), String> {
    let detail = context
        .cpu_detail_for(observation, &shared.cancellation, command.epoch)
        .map_err(|error| error.to_string())?
        .try_map(|value| {
            crate::live_cpu::CPUActivity::decode(value, &snapshot.selection["selected"])
        })?;
    let total = detail.cpus.len();
    snapshot.cpu_detail = Some(Presentation::Store(detail));
    if command.context != Context::Keep {
        if usize::try_from(command.request.cpu_offset).map_err(|error| error.to_string())? >= total
        {
            command.request.cpu_offset = 0;
        }
        let history = context
            .cpu_history(
                command.request.cpu_offset,
                &shared.cancellation,
                command.epoch,
            )
            .map_err(|error| error.to_string())?
            .try_map(|value| {
                crate::live_cpu_history::CPUHistory::decode(value, &snapshot.selection)
            })?;
        snapshot.cpu_history = Some(Arc::new(Presentation::Store(history)));
    }

    Ok(())
}

fn apps(
    context: &Arc<QueryContext>,
    observation: &Observation,
    command: &mut Command,
    shared: &Shared,
    snapshot: &mut Snapshot,
) -> Result<(), String> {
    let apps = page(command.request.apps_offset, |offset| {
        context
            .apps_page_for(
                observation,
                paging(offset),
                &shared.cancellation,
                command.epoch,
            )
            .map_err(|error| error.to_string())
    })?;
    crate::live_source::reader_apps_remember(&mut command.request, &apps)?;
    if let Some(key) = &command.request.app {
        let kind = crate::live_source::app_key_kind(&key.kind)?;
        let bytes = crate::live_source::app_key_bytes(&key.bytes_hex)?;
        let detail = page(command.request.members_offset, |offset| {
            context
                .app_detail_for(
                    observation,
                    kind,
                    &bytes,
                    paging(offset),
                    &shared.cancellation,
                    command.epoch,
                )
                .map_err(|error| error.to_string())
        })?;
        if !detail.is_null() {
            crate::live_source::identity_validate(
                &detail["selected"],
                &snapshot.selection["selected"],
            )?;
        }
        snapshot.detail = Some(Presentation::Store(detail));
        if command.context != Context::Keep {
            if context.readiness() == QueryReadiness::Complete {
                let history = context
                    .app_history(kind, &bytes, &shared.cancellation, command.epoch)
                    .map_err(|error| error.to_string())?;
                snapshot.history = Some(Presentation::Store(history));
                if command.request.load_process_history {
                    snapshot.app_parts = Some(Arc::new(PartsWindow::Store(
                        context
                            .app_parts(kind, &bytes)
                            .map_err(|error| error.to_string())?,
                    )));
                }
            }
        }
    }
    snapshot.apps = Some(Presentation::Store(apps));

    Ok(())
}

fn histories(
    context: &Arc<QueryContext>,
    command: &Command,
    shared: &Shared,
    snapshot: &mut Snapshot,
) -> Result<(), String> {
    let request = &command.request;
    if request.load_resident_history {
        if matches!(request.route.as_str(), "memory" | "overview") {
            let history = context
                .app_resident_history(&shared.cancellation, command.epoch)
                .map_err(|error| error.to_string())?
                .map(|value| {
                    crate::live_resident_history::ResidentHistory::prepare(
                        &value,
                        &snapshot.selection,
                    )
                });
            snapshot.resident_history = Some(Arc::new(Presentation::Store(history)));
        }
    }
    if request.load_app_attribution_history {
        attribution(context, command, shared, snapshot)?;
        if crate::live_source::route_ranks_apps(&request.route) {
            snapshot.app_ranks = Some(Arc::new(RanksWindow::Store(
                context.app_ranks().map_err(|error| error.to_string())?,
            )));
        }
    }
    if request.route == "apps" {
        if request.load_process_history {
            process(context, command, shared, snapshot)?;
        }
    }

    Ok(())
}

fn attribution(
    context: &QueryContext,
    command: &Command,
    shared: &Shared,
    snapshot: &mut Snapshot,
) -> Result<(), String> {
    use crate::live_app_attribution_history::{AppAttributionHistory, Metric};
    let read = |metric: Metric| -> Result<_, String> {
        let history = context
            .app_attribution_history(metric.source(), &shared.cancellation, command.epoch)
            .map_err(|error| error.to_string())?
            .map(|value| AppAttributionHistory::prepare(metric, &value, &snapshot.selection));

        Ok(Arc::new(Presentation::Store(history)))
    };
    if command.request.route == "overview" {
        snapshot.overview_app_histories = Some(Arc::new(
            crate::live_overview_app_histories::OverviewAppHistories {
                cpu: read(Metric::Cpu)?,
                power: read(Metric::Power)?,
                disk: read(Metric::Disk)?,
            },
        ));
    } else {
        if let Some(metric) = Metric::from_route(&command.request.route) {
            snapshot.app_attribution_history = Some(read(metric)?);
        }
    }

    Ok(())
}

fn process(
    context: &QueryContext,
    command: &Command,
    shared: &Shared,
    snapshot: &mut Snapshot,
) -> Result<(), String> {
    use crate::live_process_history::ProcessHistory;
    let request = &command.request;
    let key = request.app.as_ref().ok_or("Selected App unavailable")?;
    let kind = crate::live_source::app_key_kind(&key.kind)?;
    let bytes = crate::live_source::app_key_bytes(&key.bytes_hex)?;
    let history = match request.executable.as_deref() {
        None => context
            .app_process_groups_history(kind, &bytes, &shared.cancellation, command.epoch)
            .map_err(|error| error.to_string())?
            .map(|value| ProcessHistory::prepare_all(value, request, &snapshot.selection)),
        Some(filter) => {
            let name = crate::live_process_history::name_bytes(filter)?
                .ok_or("Named process history unavailable")?;
            context
                .app_process_history(kind, &bytes, &name, &shared.cancellation, command.epoch)
                .map_err(|error| error.to_string())?
                .map(|value| ProcessHistory::prepare(value, request, &snapshot.selection))
        }
    };
    snapshot.process_history = Some(Arc::new(Presentation::Store(history)));

    Ok(())
}

fn page(
    requested: u32,
    read: impl Fn(u32) -> Result<Response<Value>, String>,
) -> Result<Response<Value>, String> {
    let first = read(0)?;
    if first.is_null() {
        return Ok(first);
    }
    let offset = crate::live_source::page_offset(u64::from(requested), &first["page"]["total"])?;
    if offset == 0 {
        return Ok(first);
    }
    drop(first);

    read(u32::try_from(offset).map_err(|error| error.to_string())?)
}

fn paging(offset: u32) -> Page {
    Page {
        offset,
        limit: viewer::live_app_response::PAGE_ROWS_MAX,
    }
}
