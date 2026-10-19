use collector::processes::Usage;
use serde_json::{Value, json};

use crate::LiveSession;
use crate::live_app_response::KeyKind;

use super::named;

#[test]
fn captured_two_second_rates_and_current_threads_survive_saved_reopen() {
    // Sum exact member deltas before dividing, then reopen the real sealed capture.
    let mut fixture = named::dense();
    let initial = usage(9_007_199_254_740_993, 900);
    named::publish_with_interval(
        &mut fixture,
        1,
        &[
            named::member(7, b"worker", b"/A.app", initial),
            named::member(8, b"worker", b"/A.app", initial),
        ],
        2_000_000_000,
    );
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let mut first = named::member(
        7,
        b"worker",
        b"/A.app",
        usage(initial.idle_wakeups + 6, 904),
    );
    first.threads = 3;
    let mut second = named::member(
        8,
        b"worker",
        b"/A.app",
        usage(initial.idle_wakeups + 4, 902),
    );
    second.threads = 5;
    named::publish_with_interval(&mut fixture, 2, &[first, second], 2_000_000_000);
    session.poll().unwrap();
    session.select(1002, 0).unwrap();

    let live = selected(&mut session);
    let detail = session.app_detail(KeyKind::Bundle, b"/A.app", 0).unwrap();
    let all = session
        .app_process_groups_history(KeyKind::Bundle, b"/A.app")
        .unwrap();
    named::seal_selected(&fixture, &[1, 2]);
    drop(session);
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut reopened = LiveSession::open(&fixture.root).unwrap();
    reopened.select(1002, 0).unwrap();
    let saved = selected(&mut reopened);
    let saved_all = reopened
        .app_process_groups_history(KeyKind::Bundle, b"/A.app")
        .unwrap();

    let point = &live["points"][1];
    assert_eq!(point["idle_wakeups_per_second"], 5.0);
    assert_eq!(point["pageins_per_second"], 3.0);
    assert_eq!(point["idle_wakeups"]["known"], "10");
    assert_eq!(point["pageins"]["known"], "6");
    assert_eq!(point["threads"]["known"], "8");
    assert_eq!(detail["processes"][0]["threads"]["known"], "3");
    assert_eq!(detail["processes"][1]["threads"]["known"], "5");
    assert_eq!(detail["processes"][0]["idle_wakeups_per_second"], 3.0);
    assert_eq!(detail["processes"][1]["pageins_per_second"], 1.0);
    assert_eq!(live["points"], saved["points"]);
    assert_eq!(all.points[1].readings[0].measures[7].known, 10);
    assert_eq!(all.points[1].readings[0].measures[8].known, 6);
    assert_eq!(all.points[1].readings[0].measures[9].known, 8);
    assert_eq!(saved_all.points[1].readings[0].measures[9].known, 8);
}

#[test]
fn reset_wakeups_keep_the_measured_peer_and_independent_pageins() {
    // A reset must not erase the other counter or manufacture a complete group total.
    let mut fixture = named::dense();
    named::publish(
        &mut fixture,
        1,
        &[
            named::member(7, b"worker", b"/A.app", usage(100, 10)),
            named::member(8, b"worker", b"/A.app", usage(100, 10)),
        ],
    );
    let mut session = LiveSession::open(&fixture.root).unwrap();
    named::publish(
        &mut fixture,
        2,
        &[
            named::member(7, b"worker", b"/A.app", usage(1, 14)),
            named::member(8, b"worker", b"/A.app", usage(106, 12)),
        ],
    );
    session.poll().unwrap();
    session.select(1002, 0).unwrap();

    let actual = selected(&mut session);
    let detail = session.app_detail(KeyKind::Bundle, b"/A.app", 0).unwrap();

    assert_eq!(
        actual["points"][1]["idle_wakeups"],
        json!({
            "known":"6", "measured_members":1, "unavailable_members":1, "overflowed":false
        })
    );
    assert_eq!(actual["points"][1]["pageins"]["known"], "6");
    assert_eq!(
        detail["processes"][0]["idle_wakeups"]["unavailable"],
        "counter_reset"
    );
}

macro_rules! current_threads {
    ($name:ident, $input:expr, $known:expr, $measured:expr) => {
        #[test]
        fn $name() {
            // A current captured thread gauge survives a saved round trip without a baseline.
            let mut fixture = named::dense();
            let mut row = named::member(7, b"worker", b"/A.app", usage(100, 10));
            row.threads = $input;
            named::publish(&mut fixture, 1, &[row]);
            let mut session = LiveSession::open(&fixture.root).unwrap();
            session.select(1001, 0).unwrap();

            let actual = session
                .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
                .unwrap();
            named::seal_selected(&fixture, &[1]);
            drop(session);
            collector::recording::Registry {
                runs: Vec::new(),
                evictions: 0,
            }
            .write(&fixture.root)
            .unwrap();
            let mut reopened = LiveSession::open(&fixture.root).unwrap();
            reopened.select(1001, 0).unwrap();
            let saved = reopened
                .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
                .unwrap();

            assert_eq!(actual["points"][0]["threads"]["known"], $known);
            assert_eq!(
                actual["points"][0]["threads"]["measured_members"],
                $measured
            );
            assert_eq!(actual["points"][0]["idle_wakeups_per_second"], Value::Null);
            assert_eq!(actual["points"], saved["points"]);
        }
    };
}

current_threads!(negative_captured_threads_are_unavailable, -1, "0", 0);
current_threads!(zero_captured_threads_are_measured, 0, "0", 1);
current_threads!(
    positive_captured_threads_need_no_elapsed_interval,
    8,
    "8",
    1
);

#[test]
fn verified_exits_keep_final_counter_deltas_without_a_present_thread_gauge() {
    // Exit rusage has counters but no Task reading, so a previous gauge cannot be reused.
    let mut fixture = named::dense();
    named::publish(
        &mut fixture,
        1,
        &[named::member(7, b"worker", b"/A.app", usage(100, 10))],
    );
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let mut exit = named::member(7, b"worker", b"/A.app", usage(106, 14));
    exit.exited = true;
    named::publish(&mut fixture, 2, &[exit]);
    session.poll().unwrap();
    session.select(1002, 0).unwrap();

    let actual = selected(&mut session);
    let detail = session.app_detail(KeyKind::Bundle, b"/A.app", 0).unwrap();

    assert_eq!(actual["points"][1]["idle_wakeups"]["known"], "6");
    assert_eq!(actual["points"][1]["threads"]["measured_members"], 0);
    assert_eq!(detail["processes"][0]["threads"]["known"], Value::Null);
}

fn selected(session: &mut LiveSession) -> Value {
    session
        .app_process_history(KeyKind::Bundle, b"/A.app", b"worker")
        .unwrap()
}

fn usage(wakeups: u64, pageins: u64) -> Usage {
    Usage {
        awake_ns_at_start: 0,
        user_ns: 100,
        system_ns: 200,
        energy_nj: 300,
        instructions: 0,
        super_instructions: 0,
        cycles: 0,
        idle_wakeups: wakeups,
        interrupt_wakeups: 0,
        pageins,
        resident_bytes: 1024,
        footprint_bytes: 2048,
        disk_read_bytes: 0,
        disk_written_bytes: 0,
    }
}

macro_rules! captured_lifetime {
    ($name:ident, $change:expr, $known:expr, $reason:expr) => {
        #[test]
        fn $name() {
            // A lifetime boundary must not leak counters into the current App's interval.
            let mut fixture = named::dense();
            named::publish(
                &mut fixture,
                1,
                &[named::member(7, b"worker", b"/A.app", usage(100, 10))],
            );
            let mut session = LiveSession::open(&fixture.root).unwrap();
            let mut row = named::member(7, b"worker", b"/A.app", usage(106, 14));
            row.threads = 8;
            $change(&mut row);
            named::publish(&mut fixture, 2, &[row]);
            session.poll().unwrap();
            session.select(1002, 0).unwrap();

            let actual = selected(&mut session);
            let detail = session.app_detail(KeyKind::Bundle, b"/A.app", 0).unwrap();

            assert_eq!(detail["processes"][0]["idle_wakeups"]["known"], $known);
            assert_eq!(
                detail["processes"][0]["idle_wakeups"]["unavailable"],
                $reason
            );
            assert_eq!(actual["points"][1]["idle_wakeups"]["measured_members"], 0);
            assert_eq!(actual["points"][1]["threads"]["known"], "8");
        }
    };
}

captured_lifetime!(
    reused_pids_keep_only_the_new_lifetimes_current_gauge,
    |row: &mut named::Named<'_>| row.unique_id += 1,
    Value::Null,
    "baseline"
);
captured_lifetime!(
    exec_keeps_process_counters_without_attributing_the_app_interval,
    |row: &mut named::Named<'_>| row.version += 1,
    "6",
    Value::Null
);

#[test]
fn zero_elapsed_keeps_current_threads_and_refuses_counter_rates() {
    // Two adjacent publications at the same clock cannot define a per-second rate.
    let mut fixture = named::dense();
    named::publish_with_interval(
        &mut fixture,
        1,
        &[named::member(7, b"worker", b"/A.app", usage(100, 10))],
        0,
    );
    let mut session = LiveSession::open(&fixture.root).unwrap();
    named::publish_with_interval(
        &mut fixture,
        2,
        &[named::member(7, b"worker", b"/A.app", usage(106, 14))],
        0,
    );
    session.poll().unwrap();
    session.select(1002, 0).unwrap();

    let actual = selected(&mut session);

    assert_eq!(actual["points"][1]["idle_wakeups_per_second"], Value::Null);
    assert_eq!(actual["points"][1]["pageins_per_second"], Value::Null);
    assert_eq!(actual["points"][1]["threads"]["known"], "1");
}

#[test]
fn all_added_measures_reuse_the_cpu_cohort_and_exact_other_members() {
    // Nine captured names select seven by CPU; the other two retain their own three measures.
    let mut fixture = named::dense();
    let names: Vec<_> = (1..=9).map(|pid| format!("worker-{pid}")).collect();
    let before: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            named::member(
                i32::try_from(index + 1).unwrap(),
                name.as_bytes(),
                b"/A.app",
                usage(100, 10),
            )
        })
        .collect();
    named::publish(&mut fixture, 1, &before);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let after: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let pid = u64::try_from(index + 1).unwrap();
            let mut captured = usage(100 + pid, 10 + 2 * pid);
            captured.user_ns += pid * 1_000_000;
            let mut row = named::member(
                i32::try_from(pid).unwrap(),
                name.as_bytes(),
                b"/A.app",
                captured,
            );
            row.threads = i32::try_from(pid).unwrap();
            row
        })
        .collect();
    named::publish(&mut fixture, 2, &after);
    session.poll().unwrap();
    session.select(1002, 0).unwrap();

    let history = session
        .app_process_groups_history(KeyKind::Bundle, b"/A.app")
        .unwrap();
    let other = history
        .groups
        .iter()
        .position(|group| {
            matches!(
                group,
                crate::app_process_groups_history::ProcessGroup::Other
            )
        })
        .unwrap();

    assert_eq!(history.named_group_count, 9);
    assert_eq!(history.groups.len(), 8);
    assert!(matches!(&history.groups[0],
        crate::app_process_groups_history::ProcessGroup::Named(bytes)
            if bytes.as_ref() == b"worker-9"));
    assert_eq!(history.points[1].readings[other].measures[7].known, 3);
    assert_eq!(history.points[1].readings[other].measures[8].known, 6);
    assert_eq!(history.points[1].readings[other].measures[9].known, 3);
}

#[test]
fn ordinary_app_shape_stays_compact_while_process_groups_keep_ten_measures() {
    // Compare public scope shapes and reopen ordinary App history with identical saved values.
    let mut fixture = named::dense();
    named::publish(
        &mut fixture,
        1,
        &[named::member(7, b"worker", b"/A.app", usage(100, 10))],
    );
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let mut row = named::member(7, b"worker", b"/A.app", usage(106, 14));
    row.threads = 8;
    named::publish(&mut fixture, 2, &[row]);
    session.poll().unwrap();
    session.select(1002, 0).unwrap();

    let app = session.app_history(KeyKind::Bundle, b"/A.app").unwrap();
    let group = selected(&mut session);
    let all = session
        .app_process_groups_history(KeyKind::Bundle, b"/A.app")
        .unwrap();

    let app_point = &app["points"][1];
    assert_eq!(app_point.as_object().unwrap().len(), 19);
    assert!(app_point.get("idle_wakeups").is_none());
    assert!(app_point.get("pageins").is_none());
    assert!(app_point.get("threads").is_none());
    assert!(app_point.get("idle_wakeups_per_second").is_none());
    assert!(app_point.get("pageins_per_second").is_none());
    let group_point = &group["points"][1];
    assert_eq!(group_point.as_object().unwrap().len(), 24);
    assert_eq!(
        [
            "cpu_ns",
            "energy_nj",
            "disk_read_bytes",
            "disk_written_bytes",
            "disk_io_bytes",
            "resident_bytes",
            "footprint_bytes",
            "idle_wakeups",
            "pageins",
            "threads",
        ]
        .map(|field| group_point[field]["known"].as_str().unwrap()),
        ["0", "0", "0", "0", "0", "1024", "2048", "6", "4", "8"]
    );
    assert_eq!(all.points[1].readings[0].measures.len(), 10);
    named::seal_selected(&fixture, &[1, 2]);
    drop(session);
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
    let mut reopened = LiveSession::open(&fixture.root).unwrap();
    reopened.select(1002, 0).unwrap();

    let mut saved = reopened.app_history(KeyKind::Bundle, b"/A.app").unwrap();
    // Segment IDs belong to a loaded history; recorded identity and values must still agree.
    saved["points"][0]["segment"] = app["points"][0]["segment"].clone();
    saved["points"][1]["segment"] = app["points"][1]["segment"].clone();

    assert_eq!(saved["points"], app["points"]);
}
