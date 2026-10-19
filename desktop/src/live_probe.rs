use std::collections::BTreeMap;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::live_source::{Request, Snapshot, Update, Worker, identity_signature};

#[path = "live_probe_workflows.rs"]
mod workflows;

#[path = "live_probe_soak.rs"]
mod soak;

pub fn soak(root: PathBuf, seconds: u32) -> Result<(), String> {
    soak::run(root, seconds)
}

pub fn run(root: PathBuf, seconds: u32) -> Result<(), String> {
    if !(5..=300).contains(&seconds) {
        return Err("Live probe duration must be between 5 and 300 seconds".to_owned());
    }
    let started = Instant::now();
    let mut probe = Probe {
        worker: Worker::start(root.clone()).map_err(|error| error.to_string())?,
        deadline: started + Duration::from_secs(u64::from(seconds)),
        snapshots: 0,
    };
    let mut report = probe_routes(&mut probe)?;
    probe.worker.stop();
    report["native_workflows"] = crate::native_workflows::verify_live(root, probe.deadline)?;
    report["elapsed_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    println!("{report}");

    Ok(())
}

struct Probe {
    worker: Worker,
    deadline: Instant,
    snapshots: u32,
}

const WAIT_STEPS_MAX: u32 = 1200;
const WAIT_INTERVAL: Duration = Duration::from_millis(250);
const _: () = {
    assert!(WAIT_STEPS_MAX == 300 * 4);
    assert!(crate::live_dashboard::ROUTES.len() == 12);
};

fn probe_routes(probe: &mut Probe) -> Result<Value, String> {
    let mut routes = BTreeMap::new();
    let mut initial = None;
    for (route, _) in crate::live_dashboard::ROUTES {
        let request = probe_request(route);
        let snapshot = probe_read(probe, &request)?;
        probe_validate(&snapshot, route)?;
        if route == "overview" {
            initial = Some(snapshot);
        } else {
            routes.insert(route.to_owned(), probe_route_report(&snapshot)?);
        }
    }
    let initial = initial.ok_or_else(|| "Probe has no initial observation".to_owned())?;
    routes.insert("overview".to_owned(), probe_route_report(&initial)?);
    let workflows = workflows::run(probe)?;
    let pagination = probe_pagination(probe)?;
    let latest = probe_overlapping(probe)?;
    let paused = probe_pause(probe, &latest)?;
    let resumed = probe_resume(probe, &paused)?;
    let missing_second = probe_missing(probe, &resumed)?;
    let recovered = probe_read(probe, &probe_request("overview"))?;
    probe_validate(&recovered, "overview")?;

    Ok(json!({
        "mode": "live", "routes": routes, "snapshots": probe.snapshots,
        "first_observation": initial.selection["selected"],
        "paused_observation": paused.selection["selected"],
        "resumed_observation": resumed.selection["selected"],
        "advancing_observations": true, "paused_selection_stable": true,
        "missing_second": missing_second, "missing_selection_empty": true,
        "missing_routes_recovered": ["overview", "apps", "cpu"],
        "recovered_observation": recovered.selection["selected"],
        "workflows": workflows,
        "pagination_offsets_reset": pagination,
        "overlapping_requests_latest_generation": latest.generation
    }))
}

fn probe_request(route: &str) -> Request {
    Request {
        route: route.to_owned(),
        second: None,
        identity: None,
        table: "processes".to_owned(),
        offset: 0,
        cpu_offset: 0,
        load_resident_history: true,
        load_app_attribution_history: true,
        app: None,
        apps_offset: 0,
        members_offset: 0,
    }
}

fn probe_read(probe: &mut Probe, request: &Request) -> Result<Box<Snapshot>, String> {
    let generation = probe.worker.request(request.clone())?;
    probe_wait(probe, generation).map_err(|error| {
        format!(
            "{} generation {generation}, second {:?}: {error}",
            request.route, request.second
        )
    })
}

fn probe_wait(probe: &mut Probe, generation: u64) -> Result<Box<Snapshot>, String> {
    for _attempt in 0..WAIT_STEPS_MAX {
        if Instant::now() >= probe.deadline {
            return Err("Live probe exceeded its duration waiting for a snapshot".to_owned());
        }
        match probe.worker.take()? {
            Some(Update::Ready(snapshot)) => {
                if snapshot.generation != generation {
                    return Err("Live probe received a superseded snapshot".to_owned());
                }
                probe.snapshots += 1;
                return Ok(snapshot);
            }
            Some(Update::Failed {
                generation: failed,
                message,
            }) => {
                return Err(format!("Live probe request {failed} failed: {message}"));
            }
            None => thread::sleep(WAIT_INTERVAL),
        }
    }

    Err("Live probe exceeded its bounded wait attempts".to_owned())
}

fn probe_validate(snapshot: &Snapshot, route: &str) -> Result<(), String> {
    let panels =
        crate::live_projection::panels(route, &snapshot.selection, snapshot.history.as_ref())?;
    let limit = if route == "overview" {
        crate::live_projection::OVERVIEW_PANELS_MAX
    } else {
        crate::live_projection::ROUTE_PANELS_MAX
    };
    if panels.len() > limit {
        return Err("Live projection exceeds its panel bound".to_owned());
    }
    if matches!(route, "computer" | "self") {
        if !snapshot.selection.is_null() {
            return Err("Metadata route unexpectedly queried a selection".to_owned());
        }
    } else {
        if snapshot.selection["selected"].is_null() {
            return Err(format!("Live route {route} has no selected observation"));
        }
    }
    let tables: &[&str] = match route {
        "overview" => &["fans"],
        "raw" => &["processes"],
        _ => &[],
    };
    if snapshot.pages.len() != tables.len() {
        return Err(format!("Live route {route} returned the wrong table count"));
    }
    for table in tables {
        let page = snapshot
            .pages
            .get(*table)
            .ok_or_else(|| format!("Missing {table} page"))?;
        probe_flat_identity(&page["observation"], &snapshot.selection["selected"])?;
        let rows = page["rows"]
            .as_array()
            .ok_or_else(|| "Missing page rows".to_owned())?;
        if rows.len() > 512 {
            return Err("Live page exceeds 512 rows".to_owned());
        }
    }
    if route == "apps" {
        probe_apps(snapshot)?;
    }
    if route == "overview" {
        rankings_report(snapshot)?;
    }
    probe_app_attribution(snapshot, route)?;

    Ok(())
}

fn probe_app_attribution(snapshot: &Snapshot, route: &str) -> Result<(), String> {
    use crate::live_app_attribution_history::{AppAttributionHistory, Metric};
    let Some(metric) = Metric::from_route(route) else {
        return Ok(());
    };
    let history = snapshot
        .app_attribution_history
        .as_deref()
        .ok_or("Missing App attribution history")?;
    if let AppAttributionHistory::Unavailable(error) = history {
        return Err(format!("App attribution unavailable: {error}"));
    }
    let index = history
        .index(None)
        .ok_or("Missing selected App attribution")?;
    let ranking = match metric {
        Metric::Cpu => 0,
        Metric::Power => 2,
        Metric::Disk => 3,
    };
    if let Some(crate::live_rankings::Rankings::Ready(rankings)) = snapshot.rankings.as_deref() {
        if !rankings.metrics[ranking].entries.is_empty() {
            if !(0..9).any(|line| history.amount(line, Some(index)).is_some()) {
                return Err("Measured App rankings have no selected attribution rate".into());
            }
        }
    }

    Ok(())
}

fn rankings_report(snapshot: &Snapshot) -> Result<Value, String> {
    let rankings = snapshot
        .rankings
        .as_deref()
        .ok_or("Overview App rankings payload is missing")?;
    let rankings = match rankings {
        crate::live_rankings::Rankings::Ready(rankings) => rankings,
        crate::live_rankings::Rankings::Unavailable(error) => {
            return Err(format!("Overview App rankings unavailable: {error}"));
        }
    };
    crate::live_source::identity_validate(&rankings.selected, &snapshot.selection["selected"])?;
    for metric in &rankings.metrics {
        if metric.entries.len() > crate::live_rankings::RANKS_MAX {
            return Err("Overview App ranking exceeds ten entries".into());
        }
    }

    Ok(json!({"app_count": rankings.app_count,
        "membership_complete": rankings.membership_complete,
        "ungrouped_members": rankings.ungrouped_members,
        "metrics": rankings.metrics.iter().map(|metric| json!({
            "metric": metric.metric.title(), "entries": metric.entries.len(),
            "unavailable": metric.unavailable, "overflowed": metric.overflowed,
            "partial": metric.partial})).collect::<Vec<_>>()}))
}

fn probe_flat_identity(observed: &Value, selected: &Value) -> Result<(), String> {
    for field in ["run", "boot", "publication"] {
        if selected[field].is_null() {
            return Err(format!("Selected observation is missing {field}"));
        }
        if observed[field] != selected[field] {
            return Err(format!("Live page has a different {field}"));
        }
    }

    Ok(())
}

fn probe_apps(snapshot: &Snapshot) -> Result<(), String> {
    let apps = snapshot
        .apps
        .as_ref()
        .ok_or_else(|| "Missing live App page".to_owned())?;
    probe_app_identity(&apps["selected"], &snapshot.selection["selected"])?;
    let rows = apps["apps"]
        .as_array()
        .ok_or_else(|| "Missing live App rows".to_owned())?;
    if rows.len() > 512 {
        return Err("Live App page exceeds 512 rows".to_owned());
    }
    if !rows.is_empty() {
        let detail = snapshot
            .detail
            .as_ref()
            .ok_or_else(|| "Missing App detail".to_owned())?;
        let history = snapshot
            .history
            .as_ref()
            .ok_or_else(|| "Missing App history".to_owned())?;
        probe_app_identity(&detail["selected"], &snapshot.selection["selected"])?;
        probe_app_identity(&history["selected"], &snapshot.selection["selected"])?;
        if detail["app"]["key"] != rows[0]["key"] {
            return Err("Live App detail chose a different captured key".to_owned());
        }
    }

    Ok(())
}

fn probe_app_identity(observed: &Value, selected: &Value) -> Result<(), String> {
    for field in ["run", "boot"] {
        let text = selected[field]
            .as_str()
            .ok_or_else(|| format!("Missing {field}"))?;
        let identity = u128::from_str_radix(text, 16).map_err(|error| error.to_string())?;
        let expected = json!([
            (identity >> 64).to_string(),
            (identity & u128::from(u64::MAX)).to_string()
        ]);
        if observed["stamp"][field] != expected {
            return Err(format!("Live App payload has a different {field}"));
        }
    }
    let publication = selected["publication"]
        .as_u64()
        .ok_or_else(|| "Missing selected publication".to_owned())?;
    if observed["stamp"]["publication"].as_str() != Some(publication.to_string().as_str()) {
        return Err("Live App payload has a different publication".to_owned());
    }

    Ok(())
}

fn probe_pause(probe: &mut Probe, initial: &Snapshot) -> Result<Box<Snapshot>, String> {
    let mut request = probe_request("overview");
    request.second = Some(
        initial.selection["second"]
            .as_u64()
            .ok_or_else(|| "Missing selected second".to_owned())?,
    );
    request.identity = Some(initial.selection["selected"].clone());
    let paused = probe_read(probe, &request)?;
    probe_flat_identity(
        &paused.selection["selected"],
        &initial.selection["selected"],
    )?;
    for _attempt in 0..4 {
        thread::sleep(WAIT_INTERVAL);
        match probe.worker.take()? {
            Some(Update::Ready(snapshot)) => {
                probe_flat_identity(
                    &snapshot.selection["selected"],
                    &paused.selection["selected"],
                )?;
            }
            Some(Update::Failed { message, .. }) => return Err(message),
            None => {}
        }
    }

    Ok(paused)
}

fn probe_resume(probe: &mut Probe, paused: &Snapshot) -> Result<Box<Snapshot>, String> {
    let paused_identity = identity_signature(&paused.selection["selected"])?;
    let mut resumed = probe_read(probe, &probe_request("overview"))?;
    for _attempt in 0..WAIT_STEPS_MAX {
        if identity_signature(&resumed.selection["selected"])? != paused_identity {
            probe_validate(&resumed, "overview")?;
            return Ok(resumed);
        }
        resumed = probe_wait(probe, resumed.generation)?;
    }

    Err("Live probe did not observe an advancing publication".to_owned())
}

fn probe_pagination(probe: &mut Probe) -> Result<Value, String> {
    let mut offsets = BTreeMap::new();
    let mut request = probe_request("raw");
    request.offset = u64::MAX - u64::MAX % 512;
    let snapshot = probe_read(probe, &request)?;
    probe_validate(&snapshot, "raw")?;
    let offset = &snapshot.pages["processes"]["offset"];
    if offset.as_u64() != Some(0) {
        return Err("Stale raw process offset did not reset to zero".into());
    }
    offsets.insert("raw".to_owned(), offset.clone());
    offsets.insert("cpu".to_owned(), json!(probe_pagination_cpu(probe)?));
    let mut request = probe_request("apps");
    request.apps_offset = u32::MAX - u32::MAX % 512;
    request.members_offset = request.apps_offset;
    let snapshot = probe_read(probe, &request)?;
    probe_validate(&snapshot, "apps")?;
    let apps = snapshot
        .apps
        .as_ref()
        .ok_or("Missing paginated App response")?;
    let app_offset = &apps["page"]["offset"];
    if app_offset.as_u64() != Some(0) {
        return Err("Stale App offset did not reset to zero".to_owned());
    }
    let detail = snapshot
        .detail
        .as_ref()
        .ok_or("Missing paginated member response")?;
    let member_offset = &detail["page"]["offset"];
    if member_offset.as_u64() != Some(0) {
        return Err("Stale member offset did not reset to zero".to_owned());
    }
    offsets.insert("apps".to_owned(), app_offset.clone());
    offsets.insert("members".to_owned(), member_offset.clone());

    Ok(json!(offsets))
}

fn probe_pagination_cpu(probe: &mut Probe) -> Result<u32, String> {
    let mut request = probe_request("cpu");
    request.cpu_offset = 992;
    let snapshot = probe_read(probe, &request)?;
    probe_validate(&snapshot, "cpu")?;
    let history = snapshot
        .cpu_history
        .as_deref()
        .ok_or("CPU pagination cannot be checked because CPU history is unavailable")?;
    if history.total() == 0 {
        return Err("CPU pagination cannot be checked because logical CPU history is empty".into());
    }

    let expected = if request.cpu_offset < history.total() {
        request.cpu_offset
    } else {
        0
    };
    if history.offset() != expected {
        return Err("Stale CPU history offset did not match the logical CPU domain".into());
    }

    Ok(history.offset())
}

fn probe_overlapping(probe: &mut Probe) -> Result<Box<Snapshot>, String> {
    let mut latest_generation = None;
    for route in ["apps", "cpu", "raw", "overview"] {
        latest_generation = Some(probe.worker.request(probe_request(route))?);
    }
    let latest_generation = latest_generation.ok_or("No overlapping requests were submitted")?;
    let snapshot = probe_wait(probe, latest_generation)?;
    probe_validate(&snapshot, "overview")?;

    Ok(snapshot)
}

fn probe_missing(probe: &mut Probe, resumed: &Snapshot) -> Result<u64, String> {
    let second = resumed.catalog["last_second"]
        .as_u64()
        .and_then(|second| second.checked_add(86_400))
        .ok_or_else(|| "Missing or invalid recording time bound".to_owned())?;
    for route in ["overview", "apps", "cpu"] {
        let mut request = probe_request(route);
        request.second = Some(second);
        let missing = probe_read(probe, &request)?;
        probe_missing_validate(&missing)?;
        let recovered = probe_read(probe, &probe_request(route))?;
        probe_validate(&recovered, route)?;
    }

    Ok(second)
}

fn probe_missing_validate(missing: &Snapshot) -> Result<(), String> {
    if !missing.selection["selected"].is_null() {
        return Err("Missing second substituted another observation".to_owned());
    }
    if !missing.selection["summary"].is_null() {
        return Err("Missing second substituted another summary".to_owned());
    }
    if !missing.pages.is_empty() {
        return Err("Missing second returned raw pages".to_owned());
    }
    for payload in [&missing.apps, &missing.detail, &missing.history] {
        if payload.is_some() {
            return Err("Missing second returned App payloads".to_owned());
        }
    }
    if missing.cpu_detail.is_some() {
        return Err("Missing second returned CPU payload".to_owned());
    }

    Ok(())
}

fn probe_route_report(snapshot: &Snapshot) -> Result<Value, String> {
    let rankings = match snapshot.rankings {
        Some(_) => Some(rankings_report(snapshot)?),
        None => None,
    };
    Ok(json!({
        "generation": snapshot.generation,
        "selected": snapshot.selection["selected"],
        "pages": snapshot.pages.keys().collect::<Vec<_>>(),
        "app_count": snapshot.apps.as_ref().map(|apps| &apps["app_count"]),
        "overview_rankings": rankings,
        "app_attribution": attribution_report(snapshot)?,
        "logical_cpu_count": snapshot.cpu_detail.as_ref().map(|activity| activity.cpus.len()),
        "cpu_interval_available": snapshot.cpu_detail.as_ref()
            .map(|activity| activity.interval_available),
        "read_ms": snapshot.read_ms
    }))
}

fn attribution_report(snapshot: &Snapshot) -> Result<Value, String> {
    let Some(history) = snapshot.app_attribution_history.as_ref() else {
        return Ok(Value::Null);
    };
    let panel = history.panel().ok_or_else(|| history.coverage(None))?;
    let available: usize = panel
        .lines
        .iter()
        .map(|line| line.values.iter().filter(|value| value.is_some()).count())
        .sum();

    Ok(json!({"panel":panel.id,"series":panel.lines.len(),
        "points":panel.positions.as_ref().map(Vec::len),"available_values":available,
        "selected_index":panel.selected_index,"selected":history.headline(history.index(None))}))
}
