use serde_json::{Value, json};

use crate::live_app_attribution_history::AppAttributionHistory;
use crate::live_resident_history::ResidentHistory;
use crate::live_source::Snapshot;

const OBSERVATIONS_MAX: usize = 4096;
const IDENTITY_FIELDS: [&str; 5] = ["run", "boot", "publication", "run_tick", "wall_ns"];

pub(crate) fn validate(
    snapshot: &Snapshot,
    route: &str,
    expected: &Value,
) -> Result<Value, String> {
    validate_input(snapshot, expected)?;
    let payloads = report(snapshot)?;
    if payloads != expected["payloads"][route] {
        return Err(format!(
            "Measured {route} payload availability differs: {payloads}"
        ));
    }

    Ok(payloads)
}

pub(crate) fn validate_input(snapshot: &Snapshot, expected: &Value) -> Result<(), String> {
    for field in IDENTITY_FIELDS {
        if expected["selected"][field].is_null() {
            return Err(format!("Expected selected identity has no {field}"));
        }
        if snapshot.selection["selected"][field] != expected["selected"][field] {
            return Err(format!(
                "Measured selected {field} differs from the frozen input"
            ));
        }
    }
    if expected["context_mode"] == "kept" {
        if snapshot.selection.get("trends").is_some() {
            return Err("Hover unexpectedly returned a new context".into());
        }
        if expected.get("observations") != Some(&Value::Null) {
            return Err("Expected hover must declare absent returned context".into());
        }

        context_shape(&expected["kept_observations"])?;
        return Ok(());
    }
    validate_context(&snapshot.selection["trends"], &expected["observations"])
}

pub(crate) fn validate_context(points: &Value, observations: &Value) -> Result<(), String> {
    let observations = context_shape(observations)?;
    let points = points.as_array().ok_or("Missing measured context")?;
    if points.len() != observations.len() {
        return Err("Measured context length differs from the frozen input".into());
    }

    for (point, observation) in points.iter().zip(observations) {
        for field in IDENTITY_FIELDS {
            if point[field] != observation[field] {
                return Err(format!(
                    "Measured context {field} differs from the frozen input"
                ));
            }
        }
    }
    Ok(())
}

fn context_shape(observations: &Value) -> Result<&[Value], String> {
    let observations = observations
        .as_array()
        .ok_or("Missing expected observations")?;
    if observations.len() > OBSERVATIONS_MAX {
        return Err("Expected context exceeds the observation bound".into());
    }

    for observation in observations {
        for field in IDENTITY_FIELDS {
            if observation[field].is_null() {
                return Err(format!("Expected context identity has no {field}"));
            }
        }
    }
    Ok(observations)
}

pub(crate) fn report(snapshot: &Snapshot) -> Result<Value, String> {
    let resident = match snapshot.resident_history.as_deref() {
        Some(ResidentHistory::Ready(_)) => json!({"state": "ready"}),
        Some(ResidentHistory::Unavailable(error)) => {
            json!({"state": "unavailable", "error": error})
        }
        None => Value::Null,
    };
    let overview = snapshot.overview_app_histories.as_ref().map(|histories| {
        json!({"cpu": attribution(Some(&histories.cpu)),
            "power": attribution(Some(&histories.power)),
            "disk": attribution(Some(&histories.disk))})
    });
    let process = match snapshot.process_history.as_deref() {
        Some(
            crate::live_process_history::ProcessHistory::Ready { .. }
            | crate::live_process_history::ProcessHistory::All { .. },
        ) => json!({"state": "ready"}),
        Some(crate::live_process_history::ProcessHistory::Unavailable(error)) => {
            json!({"state": "unavailable", "error": error})
        }
        None => Value::Null,
    };
    Ok(
        json!({"rankings": rankings(snapshot)?, "rank_window": rank_window(snapshot)?,
        "resident_history": resident, "overview_app_histories": overview,
        "app_attribution_history": attribution(snapshot.app_attribution_history.as_deref()),
        "process_history": process, "apps_present": snapshot.apps.is_some(),
        "detail_present": snapshot.detail.is_some(), "history_present": snapshot.history.is_some(),
        "app_parts_present": snapshot.app_parts.is_some(),
        "cpu_detail_present": snapshot.cpu_detail.is_some(),
        "cpu_history": snapshot.cpu_history.as_ref().map(|history| json!({
            "observations": history.observation_count(), "offset": history.offset(),
            "total": history.total(), "restoration_error": history.restoration_error()}))}),
    )
}

fn attribution(history: Option<&AppAttributionHistory>) -> Value {
    match history {
        Some(AppAttributionHistory::Ready(_)) => json!({"state": "ready"}),
        Some(AppAttributionHistory::Unavailable(error)) => {
            json!({"state": "unavailable", "error": error})
        }
        None => Value::Null,
    }
}

fn rankings(snapshot: &Snapshot) -> Result<Value, String> {
    use crate::live_rankings::Rankings;
    let Some(rankings) = snapshot.rankings.as_deref() else {
        return Ok(Value::Null);
    };

    match rankings {
        Rankings::Ready(rankings) => {
            crate::live_source::identity_validate(
                &rankings.selected,
                &snapshot.selection["selected"],
            )?;
            Ok(json!({"state": "ready", "app_count": rankings.app_count,
                "membership_complete": rankings.membership_complete,
                "ungrouped_members": rankings.ungrouped_members,
                "metrics": rankings.metrics.iter().map(|metric| json!({
                    "metric": metric.metric.title(), "entries": metric.entries.len(),
                    "unavailable": metric.unavailable, "overflowed": metric.overflowed,
                    "partial": metric.partial})).collect::<Vec<_>>()}))
        }
        Rankings::Unavailable(error) => Ok(json!({"state": "unavailable", "error": error})),
    }
}

fn rank_window(snapshot: &Snapshot) -> Result<Value, String> {
    let Some(ranks) = &snapshot.app_ranks else {
        return Ok(Value::Null);
    };

    let points = snapshot.selection["trends"]
        .as_array()
        .ok_or("Missing rank context")?;
    if points.len() > OBSERVATIONS_MAX {
        return Err("Measured rank context exceeds the observation bound".into());
    }

    let last = crate::live_projection::context_last_second(&snapshot.selection)
        .ok_or("Missing rank context end")?;
    let first_ns = last.saturating_sub(899) * 1_000_000_000;
    let end_ns = last
        .checked_add(1)
        .and_then(|end| end.checked_mul(1_000_000_000))
        .ok_or("Invalid rank context end")?;
    let mut available = 0_u32;
    let mut observations = 0_u32;
    for point in points {
        let wall = point["wall_ns"]
            .as_u64()
            .ok_or("Missing rank observation clock")?;
        if (first_ns..end_ns).contains(&wall) {
            observations += 1;
            let key = viewer::ObservationKey {
                run: collector::recording::parse_identity(
                    point["run"].as_str().ok_or("Missing rank run")?,
                )
                .map_err(|error| error.to_string())?
                .0,
                boot: collector::recording::parse_identity(
                    point["boot"].as_str().ok_or("Missing rank boot")?,
                )
                .map_err(|error| error.to_string())?
                .0,
                publication: point["publication"]
                    .as_u64()
                    .ok_or("Missing rank publication")?,
            };
            if ranks.get(&key).is_some() {
                available += 1;
            }
        }
    }
    Ok(json!({"available": available, "observations": observations}))
}
