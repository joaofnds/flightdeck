use serde_json::{Value, json};

use crate::LiveSession;
use crate::live::identity_fixture::Fixture;
use crate::live_app_response::KeyKind;

macro_rules! disk_history_interval {
    ($name:ident, $start:expr, $continuous:expr, $awake:expr,
        $read:expr, $written:expr, $combined:expr) => {
        #[test]
        fn $name() {
            // Goal: saved/live disk detail and history agree using continuous, not awake, time.
            // Method: publish exact high counters, seal them and compare both public paths.
            let mut fixture = Fixture::new();
            fixture.set_clock_intervals($continuous, $awake);
            fixture.start_at_run_tick($start);
            let mut usage = super::tests::fixture::usage(100, 200, 300);
            usage.awake_ns_at_start = 0;
            usage.disk_read_bytes = 9_007_199_254_740_993;
            usage.disk_written_bytes = 9_007_199_254_741_993;
            let mut session = LiveSession::open(&fixture.root).unwrap();
            fixture.publish_with_usage(1001, &usage);
            session.poll().unwrap();
            usage.disk_read_bytes += 4096;
            usage.disk_written_bytes += 1024;
            fixture.publish_with_usage(1002, &usage);
            session.poll().unwrap();

            let live = selected_disk(&mut session, "live");
            drop(session);
            seal_fixture(&fixture, $start);
            let mut reopened = LiveSession::open(&fixture.root).unwrap();
            assert_eq!(reopened.catalog()["tail_ticks"], 0);
            let saved = selected_disk(&mut reopened, "saved_cold");
            let warm = selected_disk(&mut reopened, "saved_warm");

            assert_eq!(live, saved);
            assert_eq!(saved, warm);
            assert_eq!(live["app"]["disk_read_bytes"]["known"], "4096");
            assert_eq!(live["app"]["disk_written_bytes"]["known"], "1024");
            assert_eq!(live["app"]["disk_io_bytes"]["known"], "5120");
            assert_eq!(live["app"]["disk_read_bps"], $read);
            assert_eq!(live["app"]["disk_written_bps"], $written);
            assert_eq!(live["app"]["disk_io_bps"], $combined);
            assert_eq!(live["process"]["disk_read_bps"], $read);
            assert_eq!(live["process"]["disk_written_bps"], $written);
            assert_eq!(live["process"]["disk_io_bps"], $combined);
            assert_eq!(live["history"], live["app"]);
            assert_eq!(live["first"]["disk_io_bps"], Value::Null);
            assert_eq!(live["first"]["disk_io_bytes"]["measured_members"], 0);
        }
    };
}

disk_history_interval!(
    keeps_two_second_disk_rates_across_sealing,
    0,
    2_000_000_000,
    100_000_000,
    2048.0,
    512.0,
    2560.0
);
disk_history_interval!(
    keeps_subsecond_disk_rates_across_sealing,
    0,
    250_000_000,
    50_000_000,
    16384.0,
    4096.0,
    20480.0
);
disk_history_interval!(
    keeps_disk_rates_across_adjacent_committed_minutes,
    59,
    1_000_000_000,
    1_000_000_000,
    4096.0,
    1024.0,
    5120.0
);

fn seal_fixture(fixture: &Fixture, start: u64) {
    if start == 59 {
        fixture.seal_minute(0);
        fixture.seal_minute(1);
    } else {
        assert_eq!(start, 0);
        fixture.seal();
    }

    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
}

fn selected_disk(session: &mut LiveSession, phase: &str) -> Value {
    let timebase = collector::clock::Timebase::read().unwrap();
    let pid = collector::processes::own_pid().unwrap();
    let before = collector::processes::read_usage(timebase, pid)
        .unwrap()
        .unwrap();
    let started = std::time::Instant::now();
    let selection = session.select(1002, 0).unwrap();
    let detail = session
        .app_detail(KeyKind::Bundle, b"/Apps/Current.app", 0)
        .unwrap();
    let history = session
        .app_history(KeyKind::Bundle, b"/Apps/Current.app")
        .unwrap();
    let endpoint = history["points"]
        .as_array()
        .unwrap()
        .iter()
        .find(|point| point["observation"]["stamp"] == detail["selected"]["stamp"])
        .unwrap();
    let wall_ns = started.elapsed().as_nanos();
    let after = collector::processes::read_usage(timebase, pid)
        .unwrap()
        .unwrap();
    println!(
        "{}",
        json!({"fixture_only":true, "phase":phase,
        "wall_ns":wall_ns, "cpu_ns":after.user_ns - before.user_ns
            + after.system_ns - before.system_ns,
        "footprint_before_bytes":before.footprint_bytes,
        "footprint_after_bytes":after.footprint_bytes,
        "resident_before_bytes":before.resident_bytes, "resident_after_bytes":after.resident_bytes,
        "detail_json_bytes":serde_json::to_vec(&detail).unwrap().len(),
        "history_json_bytes":serde_json::to_vec(&history).unwrap().len(),
        "history_coverage":history["coverage"], "elapsed_ns":detail["elapsed_ns"]})
    );
    assert_eq!(history["selected"], detail["selected"]);
    assert_eq!(selection["selected"]["publication"].as_u64(), Some(2));

    json!({"app":disk_fields(&detail["app"]),
        "process":disk_fields(&detail["processes"][0]),
        "history":disk_fields(endpoint), "first":disk_fields(&history["points"][0])})
}

fn disk_fields(source: &Value) -> Value {
    json!({"disk_read_bytes":source["disk_read_bytes"],
        "disk_written_bytes":source["disk_written_bytes"], "disk_io_bytes":source["disk_io_bytes"],
        "disk_read_bps":source["disk_read_bps"], "disk_written_bps":source["disk_written_bps"],
        "disk_io_bps":source["disk_io_bps"]})
}
