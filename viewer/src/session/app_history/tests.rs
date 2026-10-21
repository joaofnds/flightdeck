use super::*;
use crate::live::identity_fixture::Fixture;
use crate::live_app_response::KeyKind;
use serde_json::json;

pub(super) fn selected(session: &Session, second: u64) -> Observation {
    let run = session.windows[0].identity.run;
    Observation {
        recording: run.recording.0,
        stamp: crate::apps::Stamp {
            boot: run.boot.0,
            run: run.run.0,
            publication: 2,
            continuous_ns: Some(1_000_000_000),
            awake_ns: Some(1_000_000_000),
        },
        run_tick: 1,
        wall_ns: Some(second * 1_000_000_000),
    }
}

pub(super) fn points(values: &[TracePoint]) -> Result<Value> {
    crate::live_app_response::bounded(json!(
        values
            .iter()
            .map(|point| point.json(0, false))
            .collect::<Vec<_>>()
    ))
}

#[test]
fn restores_both_committed_points_without_a_selected_sample_projection() {
    // Goal: a cold target trace restores exact saved membership and compatible counters.
    // Method: seal two real publications and request the trace directly without scalar selection.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal();
    let session = Session::open(&fixture.root).unwrap();
    let selected = selected(&session, 1002);
    let target = Target {
        kind: KeyKind::Bundle,
        bytes: b"/Apps/Current.app",
    };

    let restored = session
        .with_app_history(&Selected::ending_at(selected).unwrap(), &target, points)
        .unwrap();

    assert_eq!(restored.as_array().unwrap().len(), 2);
    assert_eq!(restored[0]["observation"]["stamp"]["publication"], "1");
    assert_eq!(restored[0]["cpu_ns"]["measured_members"], 0);
    assert_eq!(restored[1]["observation"], selected.json());
    assert_eq!(restored[1]["cpu_ns"]["known"], "1");
    assert_eq!(restored[1]["resident_bytes"]["known"], "11");
    assert!(session.app_sample_cache.borrow().is_none());
}

#[test]
fn normalizes_each_window_once_and_reuses_the_trace_while_the_live_end_advances() {
    // Goal: cold work scales with needed windows and warm following reuses the committed extent.
    // Method: restore two points, advance explicit selection beyond the immutable extent, repeat.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal();
    let session = Session::open(&fixture.root).unwrap();
    let mut selected = selected(&session, 1002);
    let target = Target {
        kind: KeyKind::Bundle,
        bytes: b"/Apps/Current.app",
    };
    let restored = session
        .with_app_history(&Selected::ending_at(selected).unwrap(), &target, points)
        .unwrap();
    let reads = session.source_reads.get();
    selected.stamp.publication = 3;
    selected.run_tick = 2;
    selected.wall_ns = Some(1_003_000_000_000);
    selected.stamp.continuous_ns = Some(2_000_000_000);
    selected.stamp.awake_ns = Some(2_000_000_000);

    let repeated = session
        .with_app_history(&Selected::ending_at(selected).unwrap(), &target, points)
        .unwrap();

    assert_eq!(reads, 3);
    assert_eq!(session.source_reads.get(), reads);
    assert_eq!(repeated, restored);
    assert!(session.app_history_cache.borrow().as_ref().unwrap().bytes() <= CACHE_BYTES_MAX);
}

#[test]
fn replaces_the_target_with_its_exact_non_utf8_byte_key() {
    // Goal: switching App byte keys replaces the target-specific facts without guessing identity.
    // Method: restore a measured App then the saved non-UTF-8 refused App from the same source.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal();
    let session = Session::open(&fixture.root).unwrap();
    let selected = selected(&session, 1002);
    let current = Target {
        kind: KeyKind::Bundle,
        bytes: b"/Apps/Current.app",
    };
    let refused = Target {
        kind: KeyKind::Bundle,
        bytes: b"/Apps/Outer\xfe.app",
    };
    session
        .with_app_history(&Selected::ending_at(selected).unwrap(), &current, points)
        .unwrap();

    let restored = session
        .with_app_history(&Selected::ending_at(selected).unwrap(), &refused, points)
        .unwrap();

    assert_eq!(restored[1]["members"], 1);
    assert_eq!(restored[1]["cpu_ns"]["measured_members"], 0);
    assert_eq!(restored[1]["cpu_ns"]["unavailable_members"], 1);
    assert_eq!(restored[1]["resident_bytes"]["measured_members"], 0);
    assert_eq!(session.source_reads.get(), 6);
    assert_eq!(
        session.app_history_cache.borrow().as_ref().unwrap().key,
        refused.bytes
    );
}

#[test]
fn restores_a_compatible_predecessor_across_committed_minutes() {
    // Goal: the trace retains exact rates when the previous publication is in another minute.
    // Method: seal ticks 59 and 60 separately and request their two-point committed trace.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal_minute(0);
    fixture.seal_minute(1);
    let session = Session::open(&fixture.root).unwrap();
    let mut selected = selected(&session, 1002);
    selected.run_tick = 60;
    selected.stamp.continuous_ns = Some(60_000_000_000);
    selected.stamp.awake_ns = Some(60_000_000_000);
    let target = Target {
        kind: KeyKind::Bundle,
        bytes: b"/Apps/Current.app",
    };

    let restored = session
        .with_app_history(&Selected::ending_at(selected).unwrap(), &target, points)
        .unwrap();

    assert_eq!(restored.as_array().unwrap().len(), 2);
    assert_eq!(restored[1]["elapsed_ns"], "1000000000");
    assert_eq!(restored[1]["cpu_ns"]["known"], "1");
    assert_eq!(session.source_reads.get(), 6);
}

#[test]
fn reads_the_exact_predecessor_outside_the_plotted_wall_range() {
    // Goal: a predecessor outside the chart domain can still establish its first plotted rate.
    // Method: move wall time forward 900 seconds while recorded interval clocks advance one.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1901);
    fixture.seal();
    let session = Session::open(&fixture.root).unwrap();
    let selected = selected(&session, 1901);
    let target = Target {
        kind: KeyKind::Bundle,
        bytes: b"/Apps/Current.app",
    };

    let restored = session
        .with_app_history(&Selected::ending_at(selected).unwrap(), &target, points)
        .unwrap();

    assert_eq!(restored.as_array().unwrap().len(), 1);
    assert_eq!(restored[0]["observation"], selected.json());
    assert_eq!(restored[0]["elapsed_ns"], "1000000000");
    assert_eq!(restored[0]["cpu_ns"]["known"], "1");
    assert_eq!(session.source_reads.get(), 3);
}

#[test]
fn leaves_rates_unavailable_across_a_missing_scheduled_tick() {
    // Goal: publication adjacency cannot hide a missing scheduled observation in history.
    // Method: seal run ticks zero and two and inspect the second point's retained gauge and gap.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.skip_run_ticks(1);
    fixture.publish_with_system_rows(1002);
    fixture.seal();
    let session = Session::open(&fixture.root).unwrap();
    let mut selected = selected(&session, 1002);
    selected.run_tick = 2;
    selected.stamp.continuous_ns = Some(2_000_000_000);
    selected.stamp.awake_ns = Some(2_000_000_000);
    let target = Target {
        kind: KeyKind::Bundle,
        bytes: b"/Apps/Current.app",
    };

    let restored = session
        .with_app_history(&Selected::ending_at(selected).unwrap(), &target, points)
        .unwrap();

    assert_eq!(restored[1]["elapsed_ns"], Value::Null);
    assert_eq!(restored[1]["cpu_ns"]["measured_members"], 0);
    assert_eq!(restored[1]["resident_bytes"]["known"], "11");
}

#[test]
fn retains_repeated_wall_second_observations_as_distinct_points() {
    // Goal: the trace preserves distinct publication identities within one wall-clock second.
    // Method: commit two adjacent observations with the same wall time and increasing clocks.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1002);
    fixture.publish_with_system_rows(1002);
    fixture.seal();
    let session = Session::open(&fixture.root).unwrap();
    let selected = selected(&session, 1002);
    let target = Target {
        kind: KeyKind::Bundle,
        bytes: b"/Apps/Current.app",
    };

    let restored = session
        .with_app_history(&Selected::ending_at(selected).unwrap(), &target, points)
        .unwrap();

    assert_eq!(restored.as_array().unwrap().len(), 2);
    assert_eq!(restored[0]["observation"]["stamp"]["publication"], "1");
    assert_eq!(restored[1]["observation"]["stamp"]["publication"], "2");
    assert_eq!(restored[1]["cpu_ns"]["known"], "1");
}

#[test]
fn rejects_a_changed_selected_identity_on_a_warm_cache_hit() {
    // Goal: explicit selection facts remain authoritative even when a target cache covers them.
    // Method: restore the target, then change selected run_tick without changing its identity key.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal();
    let session = Session::open(&fixture.root).unwrap();
    let mut selected = selected(&session, 1002);
    let target = Target {
        kind: KeyKind::Bundle,
        bytes: b"/Apps/Current.app",
    };
    session
        .with_app_history(&Selected::ending_at(selected).unwrap(), &target, points)
        .unwrap();
    selected.run_tick = 99;

    let result = session.with_app_history(&Selected::ending_at(selected).unwrap(), &target, points);

    assert!(matches!(
        result,
        Err(Error::Bound("committed App history selected identity"))
    ));
    assert!(session.app_history_cache.borrow().is_none());
}

#[test]
fn rejects_a_warm_trace_when_its_committed_marker_disappears() {
    // Goal: retained target facts revalidate their consumed source commitment.
    // Method: warm a real trace, remove its marker, and ask for the same target again.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal();
    let session = Session::open(&fixture.root).unwrap();
    let selected = selected(&session, 1002);
    let target = Target {
        kind: KeyKind::Bundle,
        bytes: b"/Apps/Current.app",
    };
    session
        .with_app_history(&Selected::ending_at(selected).unwrap(), &target, points)
        .unwrap();
    let marker =
        persistence::layout::WindowKey::from(&session.windows[0].identity).marker(&fixture.root);
    assert!(!std::fs::read(&marker).unwrap().is_empty());
    std::fs::remove_file(&marker).unwrap();

    let result = session.with_app_history(&Selected::ending_at(selected).unwrap(), &target, points);

    assert!(
        matches!(result, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
    );
    assert!(session.app_history_cache.borrow().is_none());
}

#[test]
fn refuses_trace_installation_when_canceled_after_projection() {
    // Goal: cancellation after projection cannot install a target trace.
    // Method: cancel the Session's operation at its deterministic installation checkpoint.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal();
    let cancellation = std::sync::Arc::new(crate::Cancellation::default());
    let session =
        Session::open_with(&fixture.root, std::sync::Arc::clone(&cancellation), 0).unwrap();
    let selected = selected(&session, 1002);
    let target = Target {
        kind: KeyKind::Bundle,
        bytes: b"/Apps/Current.app",
    };
    *session.before_app_history_install.borrow_mut() = Some(Box::new(move || {
        cancellation.cancel().unwrap();
    }));

    let result = session.with_app_history(&Selected::ending_at(selected).unwrap(), &target, points);

    assert!(
        matches!(result, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted)
    );
    assert!(session.app_history_cache.borrow().is_none());
}

#[test]
fn invalidates_a_warm_trace_when_begin_advances_the_epoch() {
    // Goal: a new Session epoch releases the preceding target cache without another App call.
    // Method: warm, cancel, then begin the next epoch directly.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal();
    let cancellation = std::sync::Arc::new(crate::Cancellation::default());
    let mut session =
        Session::open_with(&fixture.root, std::sync::Arc::clone(&cancellation), 0).unwrap();
    let selected = selected(&session, 1002);
    let target = Target {
        kind: KeyKind::Bundle,
        bytes: b"/Apps/Current.app",
    };
    session
        .with_app_history(&Selected::ending_at(selected).unwrap(), &target, points)
        .unwrap();
    assert!(session.app_history_cache.borrow().is_some());
    cancellation.cancel().unwrap();

    session.begin(1).unwrap();

    assert!(session.app_history_cache.borrow().is_none());
}

#[test]
fn leaves_a_clockless_predecessor_out_of_the_trace_and_its_rate_baseline() {
    // Goal: a clockless saved publication cannot become a plotted point or a rate baseline.
    // Method: retain predecessor counters with absent wall time and request the clocked selection.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal_with_observations(&[persistence::Observation {
        publication: 1,
        wall_ns: None,
        continuous_ns: Some(0),
        awake_ns: Some(0),
    }]);
    let session = Session::open(&fixture.root).unwrap();
    let selected = selected(&session, 1002);
    let target = Target {
        kind: KeyKind::Bundle,
        bytes: b"/Apps/Current.app",
    };

    let restored = session
        .with_app_history(&Selected::ending_at(selected).unwrap(), &target, points)
        .unwrap();

    assert_eq!(restored.as_array().unwrap().len(), 1);
    assert_eq!(restored[0]["observation"], selected.json());
    assert_eq!(restored[0]["elapsed_ns"], Value::Null);
    assert_eq!(restored[0]["cpu_ns"]["measured_members"], 0);
    assert_eq!(restored[0]["resident_bytes"]["known"], "11");
}

#[test]
fn leaves_rates_unavailable_when_saved_awake_time_decreases() {
    // Goal: increasing continuous time cannot conceal invalid awake ordering in a saved trace.
    // Method: commit predecessor awake two seconds and selected awake one second.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal_with_observations(&[persistence::Observation {
        publication: 1,
        wall_ns: Some(1_001_000_000_000),
        continuous_ns: Some(0),
        awake_ns: Some(2_000_000_000),
    }]);
    let session = Session::open(&fixture.root).unwrap();
    let selected = selected(&session, 1002);
    let target = Target {
        kind: KeyKind::Bundle,
        bytes: b"/Apps/Current.app",
    };

    let restored = session
        .with_app_history(&Selected::ending_at(selected).unwrap(), &target, points)
        .unwrap();

    assert_eq!(restored[1]["elapsed_ns"], Value::Null);
    assert_eq!(restored[1]["cpu_ns"]["measured_members"], 0);
    assert_eq!(restored[1]["resident_bytes"]["known"], "11");
}

#[test]
fn rejects_a_warm_cross_minute_trace_after_only_the_predecessor_marker_disappears() {
    // Goal: warm target history still requires its predecessor window's commitment.
    // Method: warm ticks 59/60, remove only minute zero, and retain the selected marker.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal_minute(0);
    fixture.seal_minute(1);
    let session = Session::open(&fixture.root).unwrap();
    let mut selected = selected(&session, 1002);
    selected.run_tick = 60;
    selected.stamp.continuous_ns = Some(60_000_000_000);
    selected.stamp.awake_ns = Some(60_000_000_000);
    let target = Target {
        kind: KeyKind::Bundle,
        bytes: b"/Apps/Current.app",
    };
    session
        .with_app_history(&Selected::ending_at(selected).unwrap(), &target, points)
        .unwrap();
    let predecessor =
        persistence::layout::WindowKey::from(&session.windows[0].identity).marker(&fixture.root);
    let current =
        persistence::layout::WindowKey::from(&session.windows[1].identity).marker(&fixture.root);
    assert!(!std::fs::read(&predecessor).unwrap().is_empty());
    std::fs::remove_file(&predecessor).unwrap();

    let result = session.with_app_history(&Selected::ending_at(selected).unwrap(), &target, points);

    assert!(!std::fs::read(&current).unwrap().is_empty());
    assert!(
        matches!(result, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
    );
    assert!(session.app_history_cache.borrow().is_none());
}

#[test]
fn revalidates_commitments_after_projection_before_installing_a_trace() {
    // Goal: the restore cannot publish facts whose commitment disappeared during projection.
    // Method: remove the known real marker at the installation checkpoint.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal();
    let session = Session::open(&fixture.root).unwrap();
    let selected = selected(&session, 1002);
    let target = Target {
        kind: KeyKind::Bundle,
        bytes: b"/Apps/Current.app",
    };
    let marker =
        persistence::layout::WindowKey::from(&session.windows[0].identity).marker(&fixture.root);
    assert!(!std::fs::read(&marker).unwrap().is_empty());
    *session.before_app_history_install.borrow_mut() = Some(Box::new(move || {
        std::fs::remove_file(&marker).unwrap();
    }));

    let result = session.with_app_history(&Selected::ending_at(selected).unwrap(), &target, points);

    assert!(
        matches!(result, Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
    );
    assert!(session.app_history_cache.borrow().is_none());
}

#[test]
fn refuses_an_explicit_selection_from_a_different_recording() {
    // Goal: a Session cannot supply its target facts under another recording's selected header.
    // Method: retain the real boot/run/publication and change only explicit recording identity.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal();
    let session = Session::open(&fixture.root).unwrap();
    let mut selected = selected(&session, 1002);
    selected.recording[0] ^= 1;
    let target = Target {
        kind: KeyKind::Bundle,
        bytes: b"/Apps/Current.app",
    };

    let result = session.with_app_history(&Selected::ending_at(selected).unwrap(), &target, points);

    assert!(matches!(
        result,
        Err(Error::Bound("committed App history recording"))
    ));
    assert!(session.app_history_cache.borrow().is_none());
}

#[test]
fn refuses_overlapping_commitment_candidates_instead_of_choosing_one() {
    // Goal: exact publication association cannot choose between overlapping commitments.
    // Method: give the real catalog a duplicate pinned manifest for the same saved source.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal();
    let mut session = Session::open(&fixture.root).unwrap();
    let selected = selected(&session, 1002);
    session.windows.push(session.windows[0].clone());
    session.verified.push(std::cell::Cell::new(false));
    let target = Target {
        kind: KeyKind::Bundle,
        bytes: b"/Apps/Current.app",
    };

    let result = session.with_app_history(&Selected::ending_at(selected).unwrap(), &target, points);

    assert!(matches!(
        result,
        Err(Error::Bound("duplicate committed App history commitment"))
    ));
    assert!(session.app_history_cache.borrow().is_none());
}
