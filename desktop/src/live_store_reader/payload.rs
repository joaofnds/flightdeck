//! The desktop's decoders consume bounded presentations of immutable Store facts, except the
//! CPU history, which the page builds from the Store's values as they are.
use std::sync::Arc;

use serde_json::Value;
use viewer::live_app_response::{Observation, Page};
use viewer::store::{ChartPoints, QueryContext, QueryReadiness, RankedSeconds, Response};

use super::Reused;
use super::ownership::{PartsWindow, RanksWindow};
use super::presentation::Presentation;
use crate::live_source::{AppChartSource, Command, Context, Shared, Snapshot};

/// The reading of `context` the page shows. Where it brings the top Apps of each second, a
/// second `reused` ranked for the reading before is not ranked again, and where it draws a
/// chart, a point `reused` holds from the reading before is not built again.
pub(super) fn snapshot(
    context: &Arc<QueryContext>,
    command: &mut Command,
    shared: &Shared,
    inspected: Option<&Observation>,
    reused: &mut Reused,
) -> Result<Box<Snapshot>, String> {
    let observation = inspected.copied().unwrap_or(context.selected().observation);
    let route = &command.request.route;
    let draws_no_chart = super::super::route_draws_no_chart(route);
    let shows_no_reading = super::super::route_shows_no_reading(route);
    // A page that draws no chart lets go of the points drawn for the page before it, and so
    // does a page that shows no reading, which reads no selection and so draws no chart either.
    // A hover of a page that draws a chart builds no chart, and keeps every point the reader
    // holds for the reading after it.
    if draws_no_chart || shows_no_reading {
        reused.chart_points = ChartPoints::default();
    }
    let selection = if shows_no_reading {
        Presentation::Legacy(viewer::Selection::default())
    } else {
        Presentation::Store(
            context
                .selection_for(
                    &observation,
                    inspected.is_some() || draws_no_chart,
                    &mut reused.chart_points,
                    &shared.cancellation,
                    command.epoch,
                )
                .map_err(|error| error.to_string())?,
        )
    };
    let mut snapshot =
        crate::live_source::reader_snapshot_owned(command.generation, context.catalog(), selection);
    snapshot.histories_pending = context.readiness() == QueryReadiness::Primary;
    // What the page shared as it asked decides what the reading leaves to the Store's View, of
    // its App histories and of its older tail, at every readiness, so the two never disagree.
    let source = shared.app_chart_source();
    if crate::live_source::earlier_left_to_view(&command.request, source) {
        snapshot.points_keeping = crate::live_source::PointsKeeping::TailLeftToView;
    }
    if crate::live_source::route_ranks_apps(&command.request.route) {
        rankings(context, &observation, &mut snapshot)?;
    }
    if command.request.route == "cpu" {
        cpu(context, &observation, command, shared, &mut snapshot)?;
    }
    if command.request.route == "apps" {
        apps(context, &observation, command, shared, &mut snapshot)?;
    }
    if command.context != Context::Keep {
        if context.readiness() == QueryReadiness::Complete {
            histories(
                context,
                command,
                shared,
                source,
                &mut snapshot,
                &mut reused.ranked,
            )?;
        }
    }

    Ok(snapshot)
}

/// Gives `snapshot` the top Apps of `observation`, or why it has none.
fn rankings(
    context: &Arc<QueryContext>,
    observation: &Observation,
    snapshot: &mut Snapshot,
) -> Result<(), String> {
    let ranks = context
        .selected_rank_status_for(observation)
        .map_err(|error| error.to_string())?;
    let ranks = ranks.try_map(|ranks| match ranks {
        Ok(ranks) => Ok(crate::live_rankings::Rankings::Ready(Box::new(
            crate::live_rankings::from_ranks(&ranks),
        ))),
        Err(viewer::store::Missing::Refused(reason)) => Ok(
            crate::live_rankings::Rankings::Unavailable(crate::bounded::bounded(&reason, 512)),
        ),
        Err(viewer::store::Missing::TooMany) => Ok(crate::live_rankings::Rankings::Unavailable(
            String::from("This observation has too many Apps"),
        )),
        Err(error) => Err(error.to_string()),
    })?;
    snapshot.rankings = Some(Arc::new(Presentation::Canonical(ranks)));

    Ok(())
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
            .try_map(|history| {
                crate::live_cpu_history::CPUHistory::from_store(history, &snapshot.selection)
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
                Page {
                    offset,
                    limit: viewer::live_app_response::APPS_PAGE_ROWS_MAX,
                },
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
                    Page {
                        offset,
                        limit: viewer::live_app_response::PROCESSES_PAGE_ROWS_MAX,
                    },
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
    source: AppChartSource,
    snapshot: &mut Snapshot,
    ranked: &mut RankedSeconds,
) -> Result<(), String> {
    let request = &command.request;
    // A chart the window draws from the Store's View would replace this reading's history of
    // it, so the reading builds none, and says so, since the window may have stopped drawing
    // from the View by the time it takes the reading. The top Apps of each second stay, since a
    // hover reads them.
    snapshot.app_chart_source = source;
    if request.load_resident_history {
        if source == AppChartSource::Reading {
            histories_resident(context, command, shared, snapshot)?;
        }
    }
    if request.load_app_attribution_history {
        if source == AppChartSource::Reading {
            attribution(context, command, shared, snapshot)?;
        }
    }
    if request.load_app_attribution_history && crate::live_source::route_ranks_apps(&request.route)
    {
        snapshot.app_ranks = Some(Arc::new(RanksWindow::Store(
            context
                .app_ranks(ranked)
                .map_err(|error| error.to_string())?,
        )));
    } else {
        // A page that shows no top Apps lets go of the ones ranked for the page before it.
        *ranked = RankedSeconds::default();
    }
    if request.route == "apps" {
        if request.load_process_history {
            process(context, command, shared, snapshot)?;
        }
    }

    Ok(())
}

/// Reads the history of resident memory by App on the pages that draw it.
fn histories_resident(
    context: &QueryContext,
    command: &Command,
    shared: &Shared,
    snapshot: &mut Snapshot,
) -> Result<(), String> {
    if matches!(command.request.route.as_str(), "memory" | "overview") {
        let history = context
            .app_resident_history(&shared.cancellation, command.epoch)
            .map_err(|error| error.to_string())?
            .map(|value| {
                crate::live_resident_history::ResidentHistory::prepare(&value, &snapshot.selection)
            });
        snapshot.resident_history = Some(Arc::new(Presentation::Store(history)));
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
