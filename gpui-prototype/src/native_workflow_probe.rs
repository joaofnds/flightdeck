use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::live_dashboard::LiveDashboard;
use crate::live_source::{Update, Worker};

pub fn run(root: PathBuf, deadline: Instant) -> Result<Value, String> {
    let worker = Worker::start(root.clone()).map_err(|error| error.to_string())?;
    let mut dashboard = LiveDashboard::new(worker, "overview".into(), None);

    read_initial(&mut dashboard, deadline)?;

    let initial = dashboard
        .snapshot
        .as_ref()
        .ok_or("Native probe has no initial snapshot")?;
    let selected = initial.selection["selected"].clone();
    if selected.is_null() {
        return Err("Native probe needs an actual recorded observation".into());
    }

    let second = initial.selection["second"].as_u64();

    reject_invalid_from_computer(&mut dashboard, &root, deadline)?;
    dashboard.request.route = "computer".into();
    dashboard.request.second = second;
    dashboard.request.identity = Some(selected.clone());
    dashboard.request.offset = 512;
    dashboard.chart_hover =
        dashboard
            .request
            .second
            .map(|second| crate::live_dashboard::ChartHover {
                source: "recording-switch-probe".into(),
                second,
            });
    dashboard.expanded = Some(("ticks".into(), 1));
    dashboard.begin_recording(root);
    wait_switch(&mut dashboard, deadline)?;

    report(&mut dashboard, &selected)
}

fn wait_switch(dashboard: &mut LiveDashboard, deadline: Instant) -> Result<(), String> {
    for _attempt in 0..1200 {
        if Instant::now() >= deadline {
            return Err("Native recording switch probe exceeded its deadline".into());
        }
        if dashboard.recording_step() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err("Native recording switch probe exceeded its bounded steps".into())
}

fn reject_invalid_from_computer(
    dashboard: &mut LiveDashboard,
    root: &std::path::Path,
    deadline: Instant,
) -> Result<(), String> {
    let selected = dashboard
        .snapshot
        .as_ref()
        .ok_or("Missing original recording")?
        .selection["selected"]
        .clone();
    let second = dashboard
        .snapshot
        .as_ref()
        .ok_or("Missing original recording")?
        .selection["second"]
        .as_u64();
    dashboard.request.route = "computer".into();
    let previous = dashboard.request.clone();
    let missing = root.join("gpui-native-invalid-recording-probe");
    if missing.exists() {
        return Err("Native probe requires its invalid recording child to be absent".into());
    }
    dashboard.begin_recording(missing);
    wait_switch(dashboard, deadline)?;
    if !dashboard
        .native
        .notice
        .as_deref()
        .is_some_and(|notice| notice.starts_with("Recording unchanged:"))
    {
        return Err("Computer recording validation accepted an invalid folder".into());
    }
    if dashboard.request != previous {
        return Err("Failed recording switch changed the previous request".into());
    }
    wait_retirement(dashboard, deadline)?;
    dashboard.request = super::fresh_request("overview");
    dashboard.request.second = second;
    dashboard.request.identity = Some(selected.clone());
    dashboard.generation = dashboard.worker.request(dashboard.request.clone())?;
    read_initial(dashboard, deadline)?;
    if dashboard
        .snapshot
        .as_ref()
        .ok_or("Original recording became unavailable")?
        .selection["selected"]
        != selected
    {
        return Err("Failed switch lost the original reader's exact observation".into());
    }
    Ok(())
}

fn wait_retirement(dashboard: &mut LiveDashboard, deadline: Instant) -> Result<(), String> {
    for _attempt in 0..1200 {
        if Instant::now() >= deadline {
            return Err("Native retirement probe exceeded its deadline".into());
        }
        dashboard.native.poll_retirement()?;
        if !dashboard.native.busy() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err("Native retirement probe exceeded its bounded steps".into())
}

fn read_initial(dashboard: &mut LiveDashboard, deadline: Instant) -> Result<(), String> {
    for _attempt in 0..1200 {
        if Instant::now() >= deadline {
            return Err("Native initial reading probe exceeded its deadline".into());
        }

        match dashboard.worker.take()? {
            Some(Update::Ready(snapshot)) if snapshot.generation == dashboard.generation => {
                dashboard.accept(snapshot);
                if let Some(error) = &dashboard.error {
                    return Err(error.clone());
                }

                return Ok(());
            }

            Some(Update::Failed { message, .. }) => return Err(message),
            _ => {}
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    Err("Native initial reading probe exceeded its bounded steps".into())
}

fn report(dashboard: &mut LiveDashboard, initial: &Value) -> Result<Value, String> {
    if dashboard.native.notice.as_deref() != Some("Recording opened") {
        return Err(format!(
            "Native switch did not open: {:?}",
            dashboard.native.notice
        ));
    }

    if dashboard.request.second.is_some() {
        return Err("Native recording switch retained the previous inspection".into());
    }

    if dashboard.request.identity.is_some() {
        return Err("Native recording switch retained the previous inspection".into());
    }

    if dashboard.expanded.is_some() {
        return Err("Native recording switch retained the previous row".into());
    }

    if dashboard.chart_hover.is_some() {
        return Err("Native recording switch retained previous row or hover state".into());
    }

    if dashboard.request.offset != 0 {
        return Err("Native recording switch retained the previous page offset".into());
    }

    if dashboard.request.route != "overview" {
        return Err("Successful recording open from Computer did not land on Overview".into());
    }

    let snapshot = dashboard
        .snapshot
        .as_ref()
        .ok_or("Native switch has no snapshot")?;
    if snapshot.selection["selected"].is_null() {
        return Err("Native switch did not read an actual observation".into());
    }

    let copied = super::selected_copy(Some(snapshot), &dashboard.request, None)?;
    let copied: Value = serde_json::from_str(&copied).map_err(|error| error.to_string())?;
    if copied["observation"] != snapshot.selection["selected"] {
        return Err("Clipboard serialization changed the observation identity".into());
    }

    let report = json!({"same_recording_reopen_succeeded":true,
        "invalid_open_from_computer_preserved_readable_original":true,
        "successful_open_from_computer_landed_overview":true,
        "previous_inspection_cleared":true,"previous_row_hover_page_cleared":true,
        "clipboard_payload_roundtrip":true,"initial_observation":initial,
        "reopened_observation":snapshot.selection["selected"],
        "tail_cache_bytes":snapshot.catalog["tail_cache_bytes"],
        "tail_ticks":snapshot.catalog["tail_ticks"],
        "native_picker_clipboard_ime_voiceover_exercised":false});

    dashboard.worker.stop();

    Ok(report)
}
