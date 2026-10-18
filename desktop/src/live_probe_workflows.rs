use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::{Probe, probe_read, probe_request, probe_validate, probe_wait};
use crate::live_source::{AppKey, Request, Snapshot, identity_signature, identity_validate};

const OBSERVATIONS_MAX: usize = 4096;
const APP_PAGES_MAX: u32 = 64;
const RAPID_REQUESTS: u32 = 32;
const CONTEXT_SECONDS: u64 = 900;
const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;
const _: () = {
    assert!(APP_PAGES_MAX * 512 == 32768);
    assert!(RAPID_REQUESTS > 1);
    assert!(OBSERVATIONS_MAX >= 900);
};

pub(super) fn run(probe: &mut Probe) -> Result<Value, String> {
    let latest = probe_read(probe, &probe_request("apps"))?;
    let scalar = history_coverage(&latest.selection, "trends", false)?;
    let selected_apps = run_apps(probe, &latest)?;
    let cpu = run_cpu(probe)?;

    Ok(
        json!({"scalar_history": scalar, "selected_apps": selected_apps,
        "selected_cpu": cpu}),
    )
}

fn run_apps(probe: &mut Probe, initial: &Snapshot) -> Result<Value, String> {
    let mut report = serde_json::Map::new();
    for kind in ["bundle", "executable"] {
        let Some(key) = run_apps_find(probe, initial, kind)? else {
            report.insert(kind.to_owned(), json!({"status": "no_captured_key"}));
            continue;
        };
        let mut request = pinned_request("apps", initial)?;
        request.app = Some(key);
        let detail = probe_read(probe, &request)?;
        let before = run_apps_validate(&detail, &request)?;
        let rapid = run_apps_rapid(probe, &request)?;
        let paused = probe_read(probe, &request)?;
        run_apps_validate(&paused, &request)?;
        if detail.detail != paused.detail {
            return Err("Paused App detail changed at the same observation".into());
        }
        request.second = None;
        request.identity = None;
        let resumed = probe_read(probe, &request)?;
        let resumed = run_apps_validate(&resumed, &request)?;
        report.insert(
            kind.to_owned(),
            json!({"status": "observed",
            "paused": before, "resumed": resumed, "rapid_latest_generation": rapid}),
        );
    }

    Ok(Value::Object(report))
}

fn run_apps_find(
    probe: &mut Probe,
    initial: &Snapshot,
    kind: &str,
) -> Result<Option<AppKey>, String> {
    let mut request = pinned_request("apps", initial)?;
    for _page in 0..APP_PAGES_MAX {
        let snapshot = probe_read(probe, &request)?;
        let apps = snapshot.apps.as_ref().ok_or("Missing live App list")?;
        identity_validate(&apps["selected"], &initial.selection["selected"])?;
        let rows = apps["apps"].as_array().ok_or("Missing captured App keys")?;
        if rows.len() > 512 {
            return Err("Captured App key page exceeds 512 rows".into());
        }
        for row in rows {
            if row["key"]["kind"].as_str() == Some(kind) {
                return Ok(Some(AppKey {
                    kind: kind.to_owned(),
                    bytes_hex: row["key"]["bytes_hex"]
                        .as_str()
                        .ok_or("Missing captured App bytes")?
                        .to_owned(),
                }));
            }
        }
        let Some(next) = apps["page"]["next_offset"].as_u64() else {
            return Ok(None);
        };
        if next <= u64::from(request.apps_offset) {
            return Err("App pagination did not advance".into());
        }
        request.apps_offset = u32::try_from(next).map_err(|error| error.to_string())?;
    }

    Err("App key lookup exceeded 64 bounded pages".into())
}

fn run_apps_validate(snapshot: &Snapshot, request: &Request) -> Result<Value, String> {
    if let Some(identity) = &request.identity {
        identity_validate(&snapshot.selection["selected"], identity)?;
    }
    let key = request.app.as_ref().ok_or("Missing requested App key")?;
    let detail = snapshot
        .detail
        .as_ref()
        .ok_or("Missing selected App detail")?;
    let history = snapshot
        .history
        .as_ref()
        .ok_or("Missing selected App history")?;
    identity_validate(&history["selected"], &snapshot.selection["selected"])?;
    if detail.is_null() {
        if request.identity.is_some() {
            return Err("Captured App disappeared from its pinned observation".into());
        }
        return Ok(json!({"selected": snapshot.selection["selected"],
            "detail_available": false, "history": history_coverage(history, "points", true)?}));
    }
    identity_validate(&detail["selected"], &snapshot.selection["selected"])?;
    if detail["app"]["key"]["kind"].as_str() != Some(key.kind.as_str()) {
        return Err("App detail changed the captured key kind".into());
    }
    if detail["app"]["key"]["bytes_hex"].as_str() != Some(key.bytes_hex.as_str()) {
        return Err("App detail changed the exact captured key bytes".into());
    }
    let members = detail["processes"]
        .as_array()
        .ok_or("Missing selected App members")?;
    if members.len() > 512 {
        return Err("Selected App member page exceeds 512 rows".into());
    }
    for member in members {
        if member["app_index"] != detail["app"]["app_index"] {
            return Err("Selected App detail contains a member from another App".into());
        }
    }
    let coverage = history_coverage(history, "points", true)?;
    run_apps_endpoint(history, detail)?;

    // Captured paths remain private; equality was checked against the original bytes above.
    Ok(
        json!({"selected": snapshot.selection["selected"], "detail_available": true,
        "key_kind": key.kind, "key_bytes_count": key.bytes_hex.len() / 2,
        "exact_key_match": true, "member_count": detail["page"]["total"],
        "member_rows_checked": members.len(), "app_index": detail["app"]["app_index"],
        "history": coverage, "history_endpoint_matches_detail": true}),
    )
}

fn run_apps_endpoint(history: &Value, detail: &Value) -> Result<(), String> {
    let points = history["points"]
        .as_array()
        .ok_or("Missing App history points")?;
    if points.len() > OBSERVATIONS_MAX {
        return Err("App history exceeds the observation bound".into());
    }
    let selected = identity_signature(&history["selected"])?;
    for point in points {
        if identity_signature(&point["observation"])? == selected {
            for field in [
                "members",
                "cpu_ns",
                "energy_nj",
                "resident_bytes",
                "footprint_bytes",
                "membership_complete",
                "cpu_percent_one_core",
                "energy_joules",
                "energy_watts",
                "disk_read_bytes",
                "disk_written_bytes",
                "disk_io_bytes",
                "disk_read_bps",
                "disk_written_bps",
                "disk_io_bps",
            ] {
                let observed = point
                    .get(field)
                    .ok_or_else(|| format!("Selected App history endpoint lacks {field}"))?;
                let expected = detail["app"]
                    .get(field)
                    .ok_or_else(|| format!("Selected App detail lacks {field}"))?;
                if observed != expected {
                    return Err(format!("Selected App history endpoint differs in {field}"));
                }
            }
            return Ok(());
        }
    }

    Err("Selected App history has no exact selected observation".into())
}

fn run_apps_rapid(probe: &mut Probe, selected: &Request) -> Result<u64, String> {
    let mut generation = 0;
    for index in 0..RAPID_REQUESTS {
        let request = if index % 2 == 0 {
            probe_request("cpu")
        } else {
            selected.clone()
        };
        generation = probe.worker.request(request)?;
    }
    let latest = probe_wait(probe, generation)?;
    run_apps_validate(&latest, selected)?;

    Ok(generation)
}

fn run_cpu(probe: &mut Probe) -> Result<Value, String> {
    let latest = probe_read(probe, &probe_request("cpu"))?;
    probe_validate(&latest, "cpu")?;
    let trends = latest.selection["trends"]
        .as_array()
        .ok_or("Missing CPU context")?;
    if trends.len() > OBSERVATIONS_MAX {
        return Err("CPU context exceeds the observation bound".into());
    }
    let first = trends.first().ok_or("CPU context has no observation")?;
    let mut request = probe_request("cpu");
    request.second = Some(observation_second(first)?);
    request.identity = Some(first.clone());
    let earliest = probe_read(probe, &request)?;
    identity_validate(&earliest.selection["selected"], first)?;
    probe_validate(&earliest, "cpu")?;
    let paused = probe_read(probe, &request)?;
    let earliest = run_cpu_report(&earliest)?;
    if earliest != run_cpu_report(&paused)? {
        return Err("Paused logical CPU interval changed".into());
    }

    Ok(
        json!({"earliest_context": earliest, "latest": run_cpu_report(&latest)?,
        "paused_interval_stable": true}),
    )
}

fn run_cpu_report(snapshot: &Snapshot) -> Result<Value, String> {
    let activity = snapshot
        .cpu_detail
        .as_ref()
        .ok_or("Missing selected CPU interval")?;
    identity_validate(&activity.observation, &snapshot.selection["selected"])?;
    Ok(json!({"selected": activity.observation,
        "previous": activity.previous_observation,
        "interval_available": activity.interval_available,
        "unavailable_reason": activity.unavailable_reason,
        "busy_percent": activity.cpus.iter().map(|cpu| cpu.busy_percent).collect::<Vec<_>>()}))
}

fn pinned_request(route: &str, snapshot: &Snapshot) -> Result<Request, String> {
    let mut request = probe_request(route);
    request.second = Some(observation_second(&snapshot.selection["selected"])?);
    request.identity = Some(snapshot.selection["selected"].clone());
    Ok(request)
}

fn observation_second(observation: &Value) -> Result<u64, String> {
    Ok(observation_wall(observation)? / NANOSECONDS_PER_SECOND)
}

fn observation_wall(observation: &Value) -> Result<u64, String> {
    Ok(match &observation["wall_ns"] {
        Value::String(text) => text.parse::<u64>().map_err(|error| error.to_string())?,
        value => value.as_u64().ok_or("Missing observation time")?,
    })
}

pub(super) fn history_coverage(source: &Value, field: &str, app: bool) -> Result<Value, String> {
    let selected_second = observation_second(&source["selected"])?;
    let first_second = selected_second.saturating_sub(CONTEXT_SECONDS - 1);
    let points = source[field].as_array().ok_or("Missing history points")?;
    if points.len() > OBSERVATIONS_MAX {
        return Err("History exceeds the observation bound".into());
    }
    let mut seconds = BTreeSet::new();
    let mut identities = BTreeSet::new();
    let mut walls = BTreeSet::new();
    let mut present = 0_u32;
    for point in points {
        let observation = if app { &point["observation"] } else { point };
        let second = observation_second(observation)?;
        if !(first_second..=selected_second).contains(&second) {
            return Err("History observation is outside its 900-second context".into());
        }
        if !identities.insert(identity_signature(observation)?) {
            return Err("History repeats the same exact observation".into());
        }
        seconds.insert(second);
        walls.insert(observation_wall(observation)?);
        if app {
            if !point["members"].is_null() {
                present += 1;
            }
        }
    }
    let span = match (seconds.first(), seconds.last()) {
        (Some(first), Some(last)) => last - first + 1,
        _ => 0,
    };
    let unique = u64::try_from(seconds.len()).map_err(|error| error.to_string())?;
    let elapsed = match (walls.first(), walls.last()) {
        (Some(first), Some(last)) => std::time::Duration::from_nanos(last - first).as_secs_f64(),
        _ => 0.0,
    };
    Ok(
        json!({"sample_count": points.len(), "unique_seconds": unique,
        "first_second": seconds.first(), "last_second": seconds.last(),
        "observed_span_seconds": elapsed, "observed_second_slots": span,
        "missing_seconds_within_span": span - unique,
        "context_seconds": CONTEXT_SECONDS, "context_missing_seconds": CONTEXT_SECONDS - unique,
        "full_context_covered": unique == CONTEXT_SECONDS,
        "app_present_samples": if app { Some(present) } else { None },
        "source_coverage": source.get("coverage")}),
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{history_coverage, run_apps_endpoint};

    #[test]
    fn accepts_the_exact_app_history_endpoint_with_matching_measurements() {
        // Matching wire values at the selected publication form one consistent App reading.
        let app = disk_app();
        let mut point = app.clone();
        point["observation"] = observation(903, 4);
        let history = json!({"selected": observation(903, 4), "points": [point]});

        let result = run_apps_endpoint(&history, &json!({"app": app}));

        assert!(result.is_ok());
    }

    macro_rules! disk_endpoint_cases {
        ($mismatch:ident, $missing:ident, $field:literal) => {
            #[test]
            fn $mismatch() {
                // A directional or jointly measured field must agree at the selected observation.
                let app = disk_app();
                let mut point = app.clone();
                point["observation"] = observation(903, 4);
                point[$field] = serde_json::Value::Null;
                let history = json!({"selected":observation(903, 4), "points":[point]});

                let result = run_apps_endpoint(&history, &json!({"app":app}));

                assert_eq!(
                    result.unwrap_err(),
                    format!("Selected App history endpoint differs in {}", $field)
                );
            }

            #[test]
            fn $missing() {
                // Jointly missing fields cannot prove that the disk contract was exercised.
                let mut app = disk_app();
                assert!(app.as_object_mut().unwrap().remove($field).is_some());
                let mut point = app.clone();
                point["observation"] = observation(903, 4);
                let history = json!({"selected":observation(903, 4), "points":[point]});

                assert!(run_apps_endpoint(&history, &json!({"app":app})).is_err());
            }
        };
    }

    disk_endpoint_cases!(
        rejects_read_amount_mismatch,
        rejects_missing_read_amount,
        "disk_read_bytes"
    );
    disk_endpoint_cases!(
        rejects_written_amount_mismatch,
        rejects_missing_written_amount,
        "disk_written_bytes"
    );
    disk_endpoint_cases!(
        rejects_joint_amount_mismatch,
        rejects_missing_joint_amount,
        "disk_io_bytes"
    );
    disk_endpoint_cases!(
        rejects_read_rate_mismatch,
        rejects_missing_read_rate,
        "disk_read_bps"
    );
    disk_endpoint_cases!(
        rejects_written_rate_mismatch,
        rejects_missing_written_rate,
        "disk_written_bps"
    );
    disk_endpoint_cases!(
        rejects_joint_rate_mismatch,
        rejects_missing_joint_rate,
        "disk_io_bps"
    );

    fn disk_app() -> serde_json::Value {
        let measure = |known: u64| {
            json!({"known":known.to_string(), "measured_members":2,
                "unavailable_members":0, "overflowed":false})
        };
        json!({"members":2, "membership_complete":true, "cpu_ns":measure(100),
            "cpu_percent_one_core":0.00001, "energy_nj":null, "energy_joules":null,
            "energy_watts":null, "resident_bytes":null, "footprint_bytes":null,
            "disk_read_bytes":measure(4096), "disk_written_bytes":measure(1024),
            "disk_io_bytes":measure(5120), "disk_read_bps":2048.0,
            "disk_written_bps":512.0, "disk_io_bps":2560.0})
    }

    #[test]
    fn rejects_app_history_from_a_different_selected_observation() {
        // A nearby point with equal App values cannot stand in for the selected publication.
        let app = disk_app();
        let mut point = app.clone();
        point["observation"] = observation(902, 3);
        let history = json!({"selected": observation(903, 4), "points": [point]});

        assert_eq!(
            run_apps_endpoint(&history, &json!({"app":app})).unwrap_err(),
            "Selected App history has no exact selected observation"
        );
    }

    #[test]
    fn rejects_app_history_with_different_selected_measurements() {
        // Matching observation stamps cannot conceal a mismatched App aggregate.
        let app = disk_app();
        let mut point = app.clone();
        point["observation"] = observation(903, 4);
        point["members"] = json!(1);
        let history = json!({"selected": observation(903, 4), "points": [point]});

        assert_eq!(
            run_apps_endpoint(&history, &json!({"app":app})).unwrap_err(),
            "Selected App history endpoint differs in members"
        );
    }

    #[test]
    fn rejects_app_history_with_a_different_cpu_aggregate() {
        // Equal identity and membership cannot conceal a different captured CPU subtotal.
        let app = disk_app();
        let mut point = app.clone();
        point["observation"] = observation(903, 4);
        point["cpu_ns"]["known"] = json!("101");
        let history = json!({"selected": observation(903, 4), "points": [point]});

        assert_eq!(
            run_apps_endpoint(&history, &json!({"app":app})).unwrap_err(),
            "Selected App history endpoint differs in cpu_ns"
        );
    }

    #[test]
    fn rejects_app_history_with_a_different_energy_aggregate() {
        // The energy subtotal must agree even when the captured identity and members agree.
        let app = disk_app();
        let mut point = app.clone();
        point["observation"] = observation(903, 4);
        point["energy_nj"] = json!({"known":"201","measured_members":2,
            "unavailable_members":0,"overflowed":false});
        let history = json!({"selected": observation(903, 4), "points": [point]});

        assert_eq!(
            run_apps_endpoint(&history, &json!({"app":app})).unwrap_err(),
            "Selected App history endpoint differs in energy_nj"
        );
    }

    #[test]
    fn rejects_app_history_with_a_different_resident_memory_aggregate() {
        // A different resident-memory subtotal must fail without relying on member mismatch.
        let app = disk_app();
        let mut point = app.clone();
        point["observation"] = observation(903, 4);
        point["resident_bytes"] = json!({"known":"2048","measured_members":2,
            "unavailable_members":0,"overflowed":false});
        let history = json!({"selected": observation(903, 4), "points": [point]});

        assert_eq!(
            run_apps_endpoint(&history, &json!({"app":app})).unwrap_err(),
            "Selected App history endpoint differs in resident_bytes"
        );
    }

    #[test]
    fn rejects_app_history_with_a_different_footprint_aggregate() {
        // Footprint must agree independently of resident memory and membership.
        let app = disk_app();
        let mut point = app.clone();
        point["observation"] = observation(903, 4);
        point["footprint_bytes"] = json!({"known":"8192","measured_members":2,
            "unavailable_members":0,"overflowed":false});
        let history = json!({"selected": observation(903, 4), "points": [point]});

        assert_eq!(
            run_apps_endpoint(&history, &json!({"app":app})).unwrap_err(),
            "Selected App history endpoint differs in footprint_bytes"
        );
    }

    #[test]
    fn reports_a_gap_without_counting_siblings_as_covered_seconds() {
        // Two observations in one second cannot fill a missed second in the history.
        let source = json!({"selected": observation(903, 4), "trends": [
            observation(900, 1), observation(900, 2), observation(903, 4)]});

        let report = history_coverage(&source, "trends", false).unwrap();

        assert_eq!(report["sample_count"], 3);
        assert_eq!(report["unique_seconds"], 2);
        assert_eq!(report["observed_span_seconds"], 3.0);
        assert_eq!(report["observed_second_slots"], 4);
        assert_eq!(report["missing_seconds_within_span"], 2);
        assert_eq!(report["full_context_covered"], false);
    }

    #[test]
    fn rejects_history_that_borrows_an_observation_outside_the_context() {
        // A preceding point outside the requested 900 seconds must not inflate coverage.
        let source = json!({"selected": observation(903, 4), "trends": [observation(3, 1)]});

        assert!(history_coverage(&source, "trends", false).is_err());
    }

    #[test]
    fn reports_absent_app_samples_without_turning_them_into_readings() {
        // A retained observation with no captured App contributes time, not an App reading.
        let source = json!({"selected": observation(903, 4), "points": [
            {"observation": observation(902, 3), "members": null},
            {"observation": observation(903, 4), "members": 2}]});

        let report = history_coverage(&source, "points", true).unwrap();

        assert_eq!(report["app_present_samples"], 1);
        assert_eq!(report["observed_span_seconds"], 1.0);
        assert_eq!(report["context_missing_seconds"], 898);
    }

    fn observation(second: u64, publication: u64) -> serde_json::Value {
        json!({"run": "00000000000000010000000000000002",
            "boot": "00000000000000030000000000000004", "publication": publication,
            "wall_ns": second * 1_000_000_000})
    }
}
