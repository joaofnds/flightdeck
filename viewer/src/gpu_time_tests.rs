use serde_json::Value;

use crate::live::identity_fixture::Fixture;
use crate::live_app_response::KeyKind;
use crate::{AppAttributionMetric, LiveSession};

use super::named;

#[test]
fn an_apps_gpu_time_reads_the_same_live_and_from_the_replay_of_its_saved_minute() {
    // Goal: the GPU time the collector reads for each process becomes its App's GPU time, its
    // GPU history and the GPU line of its own history, live and once the minute is saved, a
    // process without GPU time reads unknown, and the GPU history leads with the App whose GPU
    // time leads. Method: a worker that gains a quarter second of GPU time over one second
    // beside a helper with none, and a second App that leads in CPU time with a tenth of a
    // second of GPU time, read live, then sealed without App parts so the saved read replays
    // the minute.
    let (fixture, live) = gpu_live();

    named::seal_selected(&fixture, &[1, 2]);
    let saved = gpu_saved(&fixture);

    assert_eq!(live.detail["app"]["gpu_percent"], 25.0);
    assert_eq!(gpu_of(&live.detail, 7)["known"], "250000000");
    assert_eq!(gpu_of(&live.detail, 8)["unavailable"], "unknown");
    assert_eq!(live.apps["metric"], "gpu");
    assert_eq!(live.apps["points"][1]["apps"][0]["gpu_percent"], 25.0);
    assert_eq!(live.apps["points"][1]["apps"][1]["gpu_percent"], 10.0);
    assert_eq!(live.app["points"][1]["gpu_percent"], 25.0);
    assert_same(&saved, &live);
}

#[test]
fn an_apps_gpu_time_reads_the_same_from_the_parts_its_saved_window_keeps() {
    // Goal: the GPU time by App of a sealed window whose App parts the persister keeps reads
    // what the live read shows from the GPU columns of those parts, which the Overview's read
    // of the App histories caches. Method: the same processes, sealed with their App parts
    // derived, read through the Overview's histories and then the GPU history.
    let (fixture, live) = gpu_live();
    named::seal_selected_derived(&fixture, &[1, 2], &crate::SealedApps::default());
    let session = gpu_saved_session(&fixture);
    session.app_attribution_histories().unwrap();

    let apps = session
        .app_attribution_history(AppAttributionMetric::Gpu)
        .unwrap();

    assert_eq!(apps["points"][1]["apps"], live.apps["points"][1]["apps"]);
}

/// What a page reads of GPU time at one second.
struct GpuReads {
    /// The App's detail.
    detail: Value,
    /// The GPU history of every App.
    apps: Value,
    /// The App's own history.
    app: Value,
}

/// Publishes the worker and the helper at two seconds and returns what the live read shows.
fn gpu_live() -> (Fixture, GpuReads) {
    let mut fixture = named::dense_with_system();
    let usage = super::usage();
    let mut busy = usage;
    busy.user_ns += 500_000_000;
    let process = |pid, name: &'static [u8], app: &'static [u8], usage, gpu_ns| {
        let mut row = named::member(pid, name, app, usage);
        row.gpu_ns = gpu_ns;
        row
    };
    named::publish(
        &mut fixture,
        1,
        &[
            process(7, b"worker", b"/A.app", usage, Some(1_000_000_000)),
            process(8, b"helper", b"/A.app", usage, None),
            process(9, b"other", b"/B.app", usage, Some(1_000_000_000)),
        ],
    );
    named::publish(
        &mut fixture,
        2,
        &[
            process(7, b"worker", b"/A.app", usage, Some(1_250_000_000)),
            process(8, b"helper", b"/A.app", usage, None),
            process(9, b"other", b"/B.app", busy, Some(1_100_000_000)),
        ],
    );
    let mut session = LiveSession::open(&fixture.root).unwrap();

    let live = gpu_reads(&mut session);
    (fixture, live)
}

/// What the saved second shows, read with no collector running.
fn gpu_saved(fixture: &Fixture) -> GpuReads {
    let mut session = gpu_saved_session(fixture);

    gpu_reads(&mut session)
}

/// A session of the saved seconds, opened with no collector running, at the later second.
fn gpu_saved_session(fixture: &Fixture) -> LiveSession {
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1002, 0).unwrap();

    session
}

fn gpu_reads(session: &mut LiveSession) -> GpuReads {
    session.select(1002, 0).unwrap();

    GpuReads {
        detail: session.app_detail(KeyKind::Bundle, b"/A.app", 0).unwrap(),
        apps: session
            .app_attribution_history(AppAttributionMetric::Gpu)
            .unwrap(),
        app: session.app_history(KeyKind::Bundle, b"/A.app").unwrap(),
    }
}

fn assert_same(saved: &GpuReads, live: &GpuReads) {
    assert_eq!(saved.detail["app"]["gpu_ns"], live.detail["app"]["gpu_ns"]);
    assert_eq!(gpu_of(&saved.detail, 7), gpu_of(&live.detail, 7));
    assert_eq!(gpu_of(&saved.detail, 8), gpu_of(&live.detail, 8));
    assert_eq!(
        saved.apps["points"][1]["apps"],
        live.apps["points"][1]["apps"]
    );
    assert_eq!(
        saved.app["points"][1]["gpu_percent"],
        live.app["points"][1]["gpu_percent"]
    );
}

/// The GPU time of the process `pid` in an App's detail.
fn gpu_of(detail: &Value, pid: i32) -> Value {
    let processes = detail["processes"].as_array().unwrap();
    let process = processes
        .iter()
        .find(|process| process["pid"] == pid)
        .unwrap();

    process["gpu_ns"].clone()
}
