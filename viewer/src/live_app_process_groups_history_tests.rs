use std::sync::Arc;

use collector::processes::Usage;
use serde_json::{Value, json};

use crate::app_process_groups_history::{AppProcessGroupsHistory, ProcessGroup};
use crate::live::identity_fixture::Fixture;
use crate::live_app_response::KeyKind;
use crate::{Cancellation, LiveSession};

#[path = "live_app_process_groups_history_tests/fixture.rs"]
pub(crate) mod named;

#[path = "live_app_process_groups_history_tests/kept.rs"]
mod kept;

#[test]
fn retains_two_names_and_exact_directional_measures_after_cold_reopen() {
    // Odd read and write intervals above 2^53 survive live, sealed cold and warm queries.
    let mut fixture = named::dense();
    let initial = usage();
    let rows = [
        named::member(7, b"worker", b"/A.app", initial),
        named::member(8, b"worker", b"/A.app", initial),
        named::member(9, b"helper", b"/A.app", initial),
    ];
    named::publish(&mut fixture, 1, &rows);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let mut first = usage();
    first.user_ns += 120_000_000;
    first.energy_nj += 400_000_000;
    first.disk_read_bytes += 9_007_199_254_740_993;
    first.disk_written_bytes += 9_007_199_254_740_995;
    let mut second = usage();
    second.user_ns += 240_000_000;
    second.energy_nj += 600_000_000;
    second.disk_read_bytes += 2048;
    second.disk_written_bytes += 512;
    second.resident_bytes = 2000;
    second.footprint_bytes = 5000;
    let mut helper = usage();
    helper.user_ns += 60_000_000;
    named::publish(
        &mut fixture,
        2,
        &[
            named::member(7, b"worker", b"/A.app", first),
            named::member(8, b"worker", b"/A.app", second),
            named::member(9, b"helper", b"/A.app", helper),
        ],
    );
    session.poll().unwrap();
    session.select(1002, 0).unwrap();
    let expected = json!({"cpu":360_000_000,"energy":1_000_000_000,
        "read":9_007_199_254_743_041_u64,"written":9_007_199_254_741_507_u64,
        "disk":18_014_398_509_484_548_u64,"resident":3000,"footprint":8000});

    let live = session
        .app_process_groups_history(KeyKind::Bundle, b"/A.app")
        .unwrap();
    assert_eq!(values(&live, b"worker", 1), expected);

    named::seal_selected(&fixture, &[1, 2]);
    drop(session);
    retire(&fixture);
    let mut reopened = LiveSession::open(&fixture.root).unwrap();
    reopened.select(1002, 0).unwrap();
    let saved = reopened
        .app_process_groups_history(KeyKind::Bundle, b"/A.app")
        .unwrap();
    assert_eq!(values(&saved, b"worker", 1), expected);

    let warm = reopened
        .app_process_groups_history(KeyKind::Bundle, b"/A.app")
        .unwrap();

    assert_eq!(live.named_group_count, 2);
    assert_eq!(values(&warm, b"worker", 1), expected);
    equal(&saved, &warm);
    assert_eq!(saved.points[1].observation, live.points[1].observation);
    assert_eq!(saved.points[1].elapsed_ns, Some(1_000_000_000));
    assert_eq!(saved.points[1].unnamed_members, Some(0));
    assert!(saved.bytes() <= crate::app_process_groups_history::RESULT_BYTES_MAX);
}

#[test]
fn includes_all_513_members_outside_a_loaded_member_page() {
    // Load only member 513, while the cohort sums every captured member of the named group.
    let mut fixture = named::dense();
    let initial = usage();
    let rows: Vec<_> = (1..=513)
        .map(|pid| named::member(pid, b"worker", b"/A.app", initial))
        .collect();
    named::publish(&mut fixture, 1, &rows);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let mut next = usage();
    next.disk_read_bytes += 1024;
    next.disk_written_bytes += 256;
    let rows: Vec<_> = (1..=513)
        .map(|pid| named::member(pid, b"worker", b"/A.app", next))
        .collect();
    named::publish(&mut fixture, 2, &rows);
    session.poll().unwrap();
    session.select(1002, 0).unwrap();
    let detail = session.app_detail(KeyKind::Bundle, b"/A.app", 512).unwrap();

    let cohort = session
        .app_process_groups_history(KeyKind::Bundle, b"/A.app")
        .unwrap();

    assert_eq!(detail["processes"].as_array().unwrap().len(), 1);
    assert_eq!(cohort.points[1].readings[0].members, 513);
    assert_eq!(cohort.points[1].readings[0].measures[2].known, 525_312);
    assert_eq!(cohort.points[1].readings[0].measures[3].known, 131_328);
    assert_eq!(cohort.points[1].readings[0].measures[5].known, 513_000);
}

#[test]
fn retains_an_app_outside_the_first_512_apps() {
    // The exact selected App key is independent of whichever Apps page was loaded.
    let mut fixture = named::dense();
    let paths: Vec<_> = (0..513)
        .map(|index| format!("/Apps/{index:04}.app"))
        .collect();
    let rows: Vec<_> = paths
        .iter()
        .enumerate()
        .map(|(index, path)| {
            named::member(
                i32::try_from(index + 1).unwrap(),
                b"worker",
                path.as_bytes(),
                usage(),
            )
        })
        .collect();
    named::publish(&mut fixture, 1, &rows);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    named::publish(&mut fixture, 2, &rows);
    session.poll().unwrap();
    session.select(1002, 0).unwrap();
    let first = session.apps_page(0).unwrap();

    let cohort = session
        .app_process_groups_history(KeyKind::Bundle, b"/Apps/0512.app")
        .unwrap();

    assert_eq!(first["apps"].as_array().unwrap().len(), 512);
    assert_eq!(cohort.app_bytes.as_ref(), b"/Apps/0512.app");
    assert_eq!(cohort.points[1].readings[0].measures[5].known, 1000);
}

#[test]
fn preserves_group_identity_across_pid_reuse_and_absence_at_the_selected_second() {
    // A new lifetime with the same raw name joins the group; a missing latest group remains a gap.
    let mut fixture = named::dense();
    named::publish(
        &mut fixture,
        1,
        &[named::member(7, b"worker\xff", b"/A.app", usage())],
    );
    let mut session = LiveSession::open(&fixture.root).unwrap();
    named::publish(&mut fixture, 2, &[]);
    session.poll().unwrap();
    let mut reused = named::member(7, b"worker\xff", b"/A.app", usage());
    reused.version = 2;
    reused.usage.as_mut().unwrap().resident_bytes = 4000;
    named::publish(&mut fixture, 3, &[reused]);
    session.poll().unwrap();
    named::publish(&mut fixture, 4, &[]);
    session.poll().unwrap();
    session.select(1004, 0).unwrap();

    let cohort = session
        .app_process_groups_history(KeyKind::Bundle, b"/A.app")
        .unwrap();

    assert_eq!(cohort.named_group_count, 1);
    assert!(matches!(&cohort.groups[0],
        ProcessGroup::Named(bytes) if bytes.as_ref() == b"worker\xff"));
    assert_eq!(cohort.points[0].readings[0].measures[5].known, 1000);
    assert_eq!(cohort.points[2].readings[0].measures[5].known, 4000);
    assert!(!cohort.points[3].readings[0].present);
    assert!(cohort.points[3].source_available);
}

#[test]
fn includes_every_nonleader_in_typed_other_with_sixteen_disk_lines_available() {
    // Nine groups rank by distinct CPU means; two groups contribute independently to Other.
    let mut fixture = named::dense();
    let names: Vec<_> = (1..=9).map(|index| format!("worker{index}")).collect();
    let rows: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            named::member(
                i32::try_from(index + 1).unwrap(),
                name.as_bytes(),
                b"/A.app",
                usage(),
            )
        })
        .collect();
    named::publish(&mut fixture, 1, &rows);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let rows: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let mut value = usage();
            value.user_ns += u64::try_from(index + 1).unwrap() * 1_000_000;
            value.disk_read_bytes += u64::try_from(index + 1).unwrap() * 1024;
            named::member(
                i32::try_from(index + 1).unwrap(),
                name.as_bytes(),
                b"/A.app",
                value,
            )
        })
        .collect();
    named::publish(&mut fixture, 2, &rows);
    session.poll().unwrap();
    session.select(1002, 0).unwrap();

    let cohort = session
        .app_process_groups_history(KeyKind::Bundle, b"/A.app")
        .unwrap();

    assert_eq!(cohort.named_group_count, 9);
    assert_eq!(cohort.groups.len(), 8);
    assert!(matches!(cohort.groups[7], ProcessGroup::Other));
    assert_eq!(cohort.points[1].readings[7].measures[0].known, 3_000_000);
    assert_eq!(cohort.points[1].readings[7].measures[2].known, 3072);
    assert_eq!(cohort.points[1].readings[7].members, 2);
}

#[test]
fn cancels_without_publishing_a_partial_cohort() {
    // A superseded epoch refuses the public call even when selected live facts are available.
    let mut fixture = named::dense();
    named::publish(
        &mut fixture,
        1,
        &[named::member(7, b"worker", b"/A.app", usage())],
    );
    let cancellation = Arc::new(Cancellation::default());
    let mut session = LiveSession::open_with(&fixture.root, Arc::clone(&cancellation), 0).unwrap();
    session.select(1001, 0).unwrap();
    cancellation.cancel().unwrap();

    let result = session.app_process_groups_history(KeyKind::Bundle, b"/A.app");

    assert!(matches!(result, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
}

#[test]
fn refuses_an_app_key_larger_than_the_captured_source_bound() {
    // A public input over the captured key bound fails before allocating the name dictionary.
    let mut fixture = named::dense();
    named::publish(
        &mut fixture,
        1,
        &[named::member(7, b"worker", b"/A.app", usage())],
    );
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1001, 0).unwrap();
    let key = vec![b'x'; 4096];

    let result = session.app_process_groups_history(KeyKind::Bundle, &key);

    assert!(matches!(
        result,
        Err(persistence::Error::Bound("process cohort App key"))
    ));
}

#[test]
fn unavailable_identity_never_fabricates_an_app_membership_or_named_series() {
    // The collector refuses empty names; an unavailable identity cannot be assigned to this App.
    let mut fixture = named::dense();
    let rows = [
        named::member(7, b"worker", b"/A.app", usage()),
        named::member(8, b"", b"/A.app", usage()),
    ];
    named::publish(&mut fixture, 1, &rows);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    named::publish(&mut fixture, 2, &rows);
    session.poll().unwrap();
    session.select(1002, 0).unwrap();

    let cohort = session
        .app_process_groups_history(KeyKind::Bundle, b"/A.app")
        .unwrap();

    assert_eq!(cohort.named_group_count, 1);
    assert_eq!(cohort.groups.len(), 1);
    assert_eq!(cohort.points[1].unnamed_members, Some(0));
    assert!(!cohort.points[1].membership_complete);
    assert_eq!(cohort.points[1].readings[0].members, 1);
}

#[test]
fn preserves_known_directional_subtotals_and_unavailable_member_counts() {
    // One refused member cannot turn the measured member's bytes into complete group coverage.
    let mut fixture = named::dense();
    let rows = [
        named::member(7, b"worker", b"/A.app", usage()),
        named::member(8, b"worker", b"/A.app", usage()),
    ];
    named::publish(&mut fixture, 1, &rows);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let mut measured = usage();
    measured.disk_read_bytes += 1024;
    measured.disk_written_bytes += 256;
    let mut refused = named::member(8, b"worker", b"/A.app", usage());
    refused.usage = None;
    named::publish(
        &mut fixture,
        2,
        &[named::member(7, b"worker", b"/A.app", measured), refused],
    );
    session.poll().unwrap();
    session.select(1002, 0).unwrap();

    let cohort = session
        .app_process_groups_history(KeyKind::Bundle, b"/A.app")
        .unwrap();

    assert_eq!(cohort.points[1].readings[0].measures[2].known, 1024);
    assert_eq!(cohort.points[1].readings[0].measures[2].measured_members, 1);
    assert_eq!(
        cohort.points[1].readings[0].measures[2].unavailable_members,
        1
    );
    assert_eq!(cohort.points[1].readings[0].measures[3].known, 256);
}

fn values(history: &AppProcessGroupsHistory, name: &[u8], point: usize) -> Value {
    let group = history
        .groups
        .iter()
        .position(|group| matches!(group, ProcessGroup::Named(bytes) if bytes.as_ref() == name))
        .unwrap();
    let measures = &history.points[point].readings[group].measures;
    json!({"cpu":measures[0].known,"energy":measures[1].known,"read":measures[2].known,
        "written":measures[3].known,"disk":measures[4].known,"resident":measures[5].known,
        "footprint":measures[6].known})
}

fn retire(fixture: &Fixture) {
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
}

pub(crate) fn usage() -> Usage {
    Usage {
        awake_ns_at_start: 0,
        user_ns: 100,
        system_ns: 200,
        energy_nj: 300,
        instructions: 0,
        super_instructions: 0,
        cycles: 0,
        idle_wakeups: 0,
        interrupt_wakeups: 0,
        pageins: 0,
        resident_bytes: 1000,
        footprint_bytes: 3000,
        disk_read_bytes: 9_007_199_254_740_993,
        disk_written_bytes: 0,
    }
}

#[test]
fn warm_public_cohort_reuses_saved_facts_without_decoding_a_saved_frame() {
    // Repeat the real cold public query, checking literal amounts and the saved-frame boundary.
    let cold = cold(Arc::new(Cancellation::default()));
    let first = cold
        .session
        .app_process_groups_history(KeyKind::Bundle, b"/A.app")
        .unwrap();
    assert_eq!(
        values(&first, b"worker", 1),
        json!({"cpu":0,"energy":0,"read":0,
        "written":0,"disk":0,"resident":1000,"footprint":3000})
    );
    let reached = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let checkpoint = Arc::clone(&reached);
    assert!(
        cold.session
            .history
            .as_ref()
            .unwrap()
            .after_process_groups_frame
            .replace(Some(Box::new(move || {
                checkpoint.store(true, std::sync::atomic::Ordering::Release);
            })))
            .is_none()
    );

    let repeated = cold
        .session
        .app_process_groups_history(KeyKind::Bundle, b"/A.app")
        .unwrap();

    assert_eq!(
        values(&repeated, b"worker", 1),
        values(&first, b"worker", 1)
    );
    equal(&first, &repeated);
    assert!(
        !reached.load(std::sync::atomic::Ordering::Acquire),
        "A warm selected-App cohort must not decode any saved Frame"
    );
}

#[test]
fn advancing_public_live_endpoint_reuses_saved_facts_and_exact_bytes() {
    // Resume a retired source and select its next live endpoint without replaying saved Frames.
    let mut cold = cold(Arc::new(Cancellation::default()));
    cold.session
        .app_process_groups_history(KeyKind::Bundle, b"/A.app")
        .unwrap();
    collector::recording::Registry {
        runs: vec![collector::recording::RegisteredRun {
            identity: cold.fixture.writer.identity().unwrap(),
            ring_name: cold.fixture.writer.name().to_owned(),
            collector_pid: 0,
        }],
        evictions: 0,
    }
    .write(&cold.fixture.root)
    .unwrap();
    let mut current = usage();
    current.disk_read_bytes += 160;
    current.disk_written_bytes += 80;
    current.resident_bytes = 4000;
    named::publish(
        &mut cold.fixture,
        3,
        &[named::member(7, b"worker", b"/A.app", current)],
    );
    cold.session.poll().unwrap();
    cold.session.select(1003, 0).unwrap();
    let before = crate::session::ProcessGroupsReplay::test_frame_decodes();

    let result = cold
        .session
        .app_process_groups_history(KeyKind::Bundle, b"/A.app")
        .unwrap();

    assert_eq!(
        crate::session::ProcessGroupsReplay::test_frame_decodes(),
        before
    );
    assert_eq!(result.points.len(), 3);
    assert_eq!(values(&result, b"worker", 0)["resident"], 1000);
    assert_eq!(values(&result, b"worker", 2)["read"], 160);
    assert_eq!(values(&result, b"worker", 2)["written"], 80);
    assert_eq!(values(&result, b"worker", 2)["resident"], 4000);
}

#[test]
fn switching_selected_raw_app_or_key_kind_never_reuses_another_cohort() {
    // One History owner replaces its target; raw non-UTF8 bytes and key kind remain independent.
    let mut fixture = named::dense();
    let mut helper = usage();
    helper.resident_bytes = 4000;
    let rows = [
        named::member(7, b"worker", b"/A.app", usage()),
        named::member(8, b"helper", b"/B\xff.app", helper),
    ];
    named::publish(&mut fixture, 1, &rows);
    named::publish(&mut fixture, 2, &rows);
    named::seal_selected(&fixture, &[1, 2]);
    retire(&fixture);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1002, 0).unwrap();
    let first = session
        .app_process_groups_history(KeyKind::Bundle, b"/A.app")
        .unwrap();
    let other = session
        .app_process_groups_history(KeyKind::Bundle, b"/B\xff.app")
        .unwrap();
    let escaped = session
        .app_process_groups_history(KeyKind::Bundle, b"/B\\xff.app")
        .unwrap();
    let executable = session
        .app_process_groups_history(KeyKind::Executable, b"/A.app")
        .unwrap();
    let repeated = session
        .app_process_groups_history(KeyKind::Bundle, b"/A.app")
        .unwrap();

    assert_eq!(values(&first, b"worker", 1)["resident"], 1000);
    assert_eq!(values(&other, b"helper", 1)["resident"], 4000);
    assert_eq!(escaped.named_group_count, 0);
    assert_eq!(executable.named_group_count, 0);
    assert_eq!(
        values(&repeated, b"worker", 1),
        values(&first, b"worker", 1)
    );
}

#[test]
fn cancels_after_a_saved_frame_without_returning_a_partial_cohort() {
    // Cancel only after a real saved frame has entered the cohort reduction.
    let cancellation = Arc::new(Cancellation::default());
    let cold = cold(Arc::clone(&cancellation));
    let reached = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let checkpoint_reached = Arc::clone(&reached);
    let saved = cold.session.history.as_ref().unwrap();
    assert!(
        saved
            .after_process_groups_frame
            .replace(Some(Box::new(move || {
                checkpoint_reached.store(true, std::sync::atomic::Ordering::Release);
                cancellation.cancel().unwrap();
            })))
            .is_none()
    );

    let result = cold
        .session
        .app_process_groups_history(KeyKind::Bundle, b"/A.app");

    assert!(reached.load(std::sync::atomic::Ordering::Acquire));
    assert!(matches!(result, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
}

#[test]
fn refuses_a_saved_file_replaced_after_a_frame_has_entered_the_reduction() {
    // Replace identical Parquet bytes mid-replay to test source identity after partial reduction.
    let cold = cold(Arc::new(Cancellation::default()));
    let saved = cold.session.history.as_ref().unwrap();
    let path = cold
        .fixture
        .root
        .join(&saved.windows[0].data_directory)
        .join("processes.parquet");
    let original = cold.fixture.root.join("original.parquet");
    let reached = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let checkpoint_reached = Arc::clone(&reached);
    assert!(
        saved
            .after_process_groups_frame
            .replace(Some(Box::new(move || {
                std::fs::rename(&path, &original).unwrap();
                assert!(std::fs::copy(&original, &path).unwrap() > 0);
                checkpoint_reached.store(true, std::sync::atomic::Ordering::Release);
            })))
            .is_none()
    );

    let result = cold
        .session
        .app_process_groups_history(KeyKind::Bundle, b"/A.app");

    assert!(reached.load(std::sync::atomic::Ordering::Acquire));
    assert!(
        matches!(
            result,
            Err(persistence::Error::Bound(
                "changed committed App history source"
            ))
        ),
        "replacement error: {:?}",
        result.err()
    );
}

struct Cold {
    session: LiveSession,
    fixture: Fixture,
}

fn cold(cancellation: Arc<Cancellation>) -> Box<Cold> {
    let mut fixture = named::dense();
    let rows = [
        named::member(7, b"worker", b"/A.app", usage()),
        named::member(8, b"helper", b"/A.app", usage()),
    ];
    named::publish(&mut fixture, 1, &rows);
    named::publish(&mut fixture, 2, &rows);
    named::seal_selected(&fixture, &[1, 2]);
    retire(&fixture);
    let mut session = LiveSession::open_with(&fixture.root, cancellation, 0).unwrap();
    session.select(1002, 0).unwrap();
    Box::new(Cold { session, fixture })
}

pub(crate) fn equal(left: &AppProcessGroupsHistory, right: &AppProcessGroupsHistory) {
    assert_eq!(left.selected, right.selected);
    assert_eq!(left.app_kind, right.app_kind);
    assert_eq!(left.app_bytes, right.app_bytes);
    assert_eq!(left.context_seconds, right.context_seconds);
    assert_eq!(left.named_group_count, right.named_group_count);
    assert_eq!(
        left.context_observations_unavailable,
        right.context_observations_unavailable
    );
    assert_eq!(left.groups.len(), right.groups.len());
    for (left, right) in left.groups.iter().zip(&right.groups) {
        match (left, right) {
            (ProcessGroup::Named(left), ProcessGroup::Named(right)) => assert_eq!(left, right),
            (ProcessGroup::Other, ProcessGroup::Other) => {}
            _ => panic!("different raw cohort identity"),
        }
    }
    assert_eq!(left.points.len(), right.points.len());
    for (left, right) in left.points.iter().zip(&right.points) {
        assert_eq!(left.observation, right.observation);
        assert_eq!(left.elapsed_ns, right.elapsed_ns);
        assert_eq!(left.source_available, right.source_available);
        assert_eq!(left.membership_complete, right.membership_complete);
        assert_eq!(left.unnamed_members, right.unnamed_members);
        assert_eq!(left.live, right.live);
        for (left, right) in left.readings.iter().zip(&right.readings) {
            assert_eq!(left.present, right.present);
            assert_eq!(left.members, right.members);
            for (left, right) in left.measures.iter().zip(&right.measures) {
                assert_eq!(left.known, right.known);
                assert_eq!(left.measured_members, right.measured_members);
                assert_eq!(left.unavailable_members, right.unavailable_members);
                assert_eq!(left.overflowed, right.overflowed);
            }
        }
    }
}

#[path = "captured_process_metrics_tests.rs"]
mod captured_process_metrics_tests;

#[path = "gpu_time_tests.rs"]
mod gpu_time_tests;
