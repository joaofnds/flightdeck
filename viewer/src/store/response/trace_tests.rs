use std::sync::Arc;

use serde_json::{Value, json};

use super::tests::{context, native_context, saved_context};
use crate::app_process_groups_history::{AppProcessGroupsHistory, ProcessGroup};
use crate::live::identity_fixture::Fixture;
use crate::live::window::fixture::saved_only;
use crate::live_app_response::KeyKind;
use crate::store::{ByteBudget, Held, QueryContext};
use crate::{Cancellation, LiveSession};

#[test]
fn retained_app_trace_matches_the_saved_live_session() {
    // Goal: an App trace preserves every measurement and exact selected/context identity.
    // Method: compare shared trace JSON against a saved LiveSession after retiring sources.
    let fixture = saved_only();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1001, 0).unwrap();
    let expected = session
        .app_history(KeyKind::Bundle, b"/Apps/Current.app")
        .unwrap();
    let context = saved_context(&fixture);
    let cancellation = Arc::new(Cancellation::default());
    drop(session);
    drop(fixture);

    let shown = context
        .app_history(KeyKind::Bundle, b"/Apps/Current.app", &cancellation, 0)
        .unwrap();

    assert_eq!(trace_fields(&shown), trace_fields(&expected));
    assert_eq!(shown["points"][0]["segment"], "0");
    assert_eq!(shown["points"][1]["cpu_ns"]["known"], "1");
    assert_eq!(shown["points"][1]["resident_bytes"]["known"], "11");
}

#[test]
fn retained_named_process_trace_matches_the_saved_live_session() {
    // Goal: named process groups preserve the attribution, gauge and event-rate rules.
    // Method: compare every trace field and byte identity against a real saved LiveSession.
    let fixture = saved_only();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1001, 0).unwrap();
    let expected = session
        .app_process_history(KeyKind::Bundle, b"/Apps/Current.app", b"current")
        .unwrap();
    let context = saved_context(&fixture);
    let cancellation = Arc::new(Cancellation::default());

    let shown = context
        .app_process_history(
            KeyKind::Bundle,
            b"/Apps/Current.app",
            b"current",
            &cancellation,
            0,
        )
        .unwrap();

    assert_eq!(trace_fields(&shown), trace_fields(&expected));
    assert_eq!(shown["app_key"], expected["app_key"]);
    assert_eq!(shown["executable_name"], expected["executable_name"]);
    assert_eq!(shown["scope"], expected["scope"]);
}

#[test]
fn retained_process_cohort_matches_every_public_saved_reading() {
    // Goal: cohort selection, raw name bytes, Other and exact process measures cannot drift.
    // Method: compare every public typed result field against the same saved LiveSession.
    let fixture = saved_only();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1001, 0).unwrap();
    let expected = session
        .app_process_groups_history(KeyKind::Bundle, b"/Apps/Current.app")
        .unwrap();
    let context = saved_context(&fixture);
    let cancellation = Arc::new(Cancellation::default());

    let shown = context
        .app_process_groups_history(KeyKind::Bundle, b"/Apps/Current.app", &cancellation, 0)
        .unwrap();

    assert_eq!(cohort_fields(&shown), cohort_fields(&expected));
    assert_eq!(shown.named_group_count, 1);
    assert_eq!(shown.points[1].readings[0].measures[0].known, 1);
}

#[test]
fn retained_app_trace_matches_the_native_live_session() {
    // Goal: native App trace values use the same pure formatter as live source presentation.
    // Method: compare two real native publications after their source and Session are retired.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.poll().unwrap();
    session.select(101, 0).unwrap();
    let expected = session
        .app_history(KeyKind::Bundle, b"/Apps/Current.app")
        .unwrap();
    let context = native_context(&fixture);
    let cancellation = Arc::new(Cancellation::default());
    drop(session);
    drop(fixture);

    let shown = context
        .app_history(KeyKind::Bundle, b"/Apps/Current.app", &cancellation, 0)
        .unwrap();

    assert_eq!(trace_fields(&shown), trace_fields(&expected));
    assert_eq!(shown["points"][0]["segment"], "1");
    assert_eq!(shown["points"][1]["segment"], "1");
}

#[test]
fn missing_process_payloads_do_not_erase_independent_resident_history() {
    // Goal: payload admission failure remains explicit without erasing compact App facts.
    // Method: exhaust payload admission at only the second native publication, then query it.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    let context = missing_payload_context(&fixture);
    let cancellation = Arc::new(Cancellation::default());

    let trace = context
        .app_history(KeyKind::Bundle, b"/Apps/Current.app", &cancellation, 0)
        .unwrap();
    let resident = context.app_resident_history(&cancellation, 0).unwrap();
    let absent = context
        .app_history(KeyKind::Bundle, b"/Absent.app", &cancellation, 0)
        .unwrap();

    assert_eq!(trace["points"][1]["source_available"], false);
    assert_eq!(trace["coverage"]["retention_limited"], true);
    assert_eq!(resident["points"][1]["source_available"], true);
    assert_eq!(resident["coverage"]["retention_limited"], false);
    assert_eq!(absent["points"][1]["source_available"], true);
    assert_eq!(absent["coverage"]["retention_limited"], false);
}

fn missing_payload_context(fixture: &Fixture) -> Arc<QueryContext> {
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut held = Held::new();
    held.enable_payload(Arc::clone(&budget));
    let mut ring = crate::store::ring::Ring::new(fixture.root.clone());
    ring.follow().unwrap();
    ring.read(|run, second, apps| {
        if second.publication == 2 {
            let occupied = budget.reserve(budget.maximum() - budget.used()).unwrap();
            let accepted = held.take(run, second, apps);
            drop(occupied);

            accepted
        } else {
            held.take(run, second, apps)
        }
    })
    .unwrap();

    context(fixture, held, &budget)
}

fn trace_fields(value: &Value) -> [&Value; 3] {
    [
        &value["selected"],
        &value["context_seconds"],
        &value["points"],
    ]
}

fn cohort_fields(history: &AppProcessGroupsHistory) -> Value {
    let groups: Vec<_> = history
        .groups
        .iter()
        .map(|group| match group {
            ProcessGroup::Named(name) => json!({"name": name}),
            ProcessGroup::Other => json!({"other": true}),
        })
        .collect();
    let points: Vec<_> = history
        .points
        .iter()
        .map(|point| {
            let readings: Vec<_> = point
                .readings
                .iter()
                .map(|reading| {
                    json!({"present": reading.present, "members": reading.members,
                "measures": reading.measures.map(|measure| measure.json())})
                })
                .collect();

            json!({"observation": point.observation.json(), "elapsed_ns": point.elapsed_ns,
            "source_available": point.source_available,
            "membership_complete": point.membership_complete,
            "unnamed_members": point.unnamed_members, "readings": readings})
        })
        .collect();

    json!({"selected": history.selected.json(), "kind": format!("{:?}", history.app_kind),
        "bytes": history.app_bytes, "context_seconds": history.context_seconds,
        "named_group_count": history.named_group_count,
        "context_observations_unavailable": history.context_observations_unavailable,
        "groups": groups, "points": points})
}
