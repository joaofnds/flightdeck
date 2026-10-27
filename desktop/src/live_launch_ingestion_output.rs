use serde_json::{Value, json};

use crate::live_source::tests::native_measurements::payloads;

use super::{Event, Harness, Snapshot, request};

pub(super) fn record_route(route: &str) {
    let mut harness = None;
    Harness::baseline(&mut harness);
    let mut harness = harness.unwrap();
    harness.backend.as_ref().unwrap().worker.freeze_raw_pages();
    let mut snapshot = harness.read(route);
    let view = harness.view();
    let mut payloads = std::collections::BTreeMap::new();
    harness
        .backend
        .as_ref()
        .unwrap()
        .worker
        .frozen_payloads(&mut payloads);
    validate_raw(route, &payloads);
    let expanded_copy = expanded_copy(&mut snapshot, route, &payloads);
    let source_availability = payloads::report(&snapshot).unwrap();
    let window = window_values(&snapshot);
    let process = process_history(&snapshot);
    record_route_ready(route, &snapshot);
    let mut effective_request = request(route);
    crate::live_acceptance::normalize_offsets(&mut effective_request, &snapshot).unwrap();
    let presentation =
        crate::live_acceptance::Presentation::prepare(&effective_request, snapshot).unwrap();
    record_route_ready(route, &presentation.snapshot);
    record_route_panels(route, &presentation);
    let availability = payloads::report(&presentation.snapshot).unwrap();
    let copy = crate::native_workflows::selected_copy(
        Some(&presentation.snapshot),
        &presentation.request,
        None,
    );
    let snapshot = &presentation.snapshot;
    harness.teardown();
    let report = json!({"schemaVersion": 1, "route": route, "request_complete": true,
        "selected": snapshot.selection["selected"], "selection": snapshot.selection,
        "pages": snapshot.pages, "apps": snapshot.apps, "detail": snapshot.detail,
        "history": snapshot.history, "cpu_detail": snapshot.cpu_detail,
        "source_payloads": payloads, "source_availability": source_availability,
        "availability": availability,
        "rank_window_and_parts": window, "source_process_history": process,
        "process_history": process_history(snapshot), "copy": copy,
        "expanded_copy": expanded_copy,
        "panels": presentation.panels.iter().map(|panel| json!({"data": panel,
            "window": panel.window.map(|window| json!({"first": window.first,
                "span": window.span})), "detail_first": panel.detail_first,
            "reading_seconds": panel.reading_seconds, "sliding": panel.sliding}))
            .collect::<Vec<_>>(),
        "typed_view": {"seconds": format!("{:?}", view.seconds),
            "apps": format!("{:?}", view.apps), "earlier": format!("{:?}", view.earlier),
            "failure": format!("{:?}", view.failure),
            "left_out": format!("{:?}", view.left_out), "ring_complete": view.ring_complete},
        "diagnostics": {"catalog": snapshot.catalog,
            "input_root": harness.root, "input_owned": harness.input.is_some(),
            "input_registry": std::fs::read_to_string(harness.root.join("runs")).unwrap(),
            "trace_complete": true, "trace_overflowed": false,
            "trace_capacity_bytes": harness.trace.capacity_bytes(),
            "events": harness.events().iter().map(event).collect::<Vec<_>>()}});
    emit(route, &report);
    assert!(!snapshot.histories_pending);
}

fn record_route_ready(route: &str, snapshot: &Snapshot) {
    if route == "apps" {
        assert!(matches!(
            snapshot.process_history.as_deref().map(|value| &**value),
            Some(crate::live_process_history::ProcessHistory::All { .. })
        ));
    }
}

fn record_route_panels(route: &str, presentation: &crate::live_acceptance::Presentation) {
    if route != "apps" {
        return;
    }

    let ids: Vec<_> = presentation
        .panels
        .iter()
        .filter(|panel| panel.id.starts_with("live-process-groups-"))
        .map(|panel| panel.id.as_str())
        .collect();
    assert_eq!(
        ids,
        [
            "live-process-groups-cpu",
            "live-process-groups-resident",
            "live-process-groups-footprint",
            "live-process-groups-network",
            "live-process-groups-disk",
            "live-process-groups-gpu",
            "live-process-groups-power",
            "live-process-groups-wakeups",
            "live-process-groups-pageins",
            "live-process-groups-threads",
            "live-process-groups-files",
            "live-process-groups-sockets",
        ]
    );
}

fn validate_raw(route: &str, payloads: &std::collections::BTreeMap<&str, Value>) {
    if route == "self" {
        assert!(!payloads.contains_key("raw_pages"));
        return;
    }
    let tables = &payloads["raw_pages"];
    assert_eq!(tables.as_object().unwrap().len(), 11);
    let processes = &tables["processes"][0];
    assert_eq!(raw_cell(processes, "user_ns"), "9007199454740993");
    assert_eq!(raw_cell(processes, "cycles"), "18446744073709551606");
    assert_eq!(
        raw_cell(processes, "process_unique_id"),
        "18446744073709551615"
    );
    assert_eq!(
        raw_cell(processes, "executable_name"),
        "worker\\xff\\x0a\\\\"
    );
}

fn raw_cell<'page>(page: &'page Value, name: &str) -> &'page Value {
    let column = page["columns"]
        .as_array()
        .unwrap()
        .iter()
        .position(|column| column == name)
        .unwrap();
    &page["rows"][0][column]
}

fn expanded_copy(
    snapshot: &mut Snapshot,
    route: &str,
    payloads: &std::collections::BTreeMap<&str, Value>,
) -> Value {
    let mut copies = serde_json::Map::new();
    let saved = std::mem::take(&mut snapshot.pages);
    if let Some(tables) = payloads.get("raw_pages") {
        let tables = tables.as_object().unwrap();
        assert!(tables.len() <= 11);
        for (table, pages) in tables {
            let pages = pages.as_array().unwrap();
            assert!(pages.len() <= 128);
            let mut rows = Vec::new();
            for page in pages {
                snapshot.pages.insert(
                    table.clone(),
                    crate::live_source::Presentation::Legacy(page.clone()),
                );
                let count = page["rows"].as_array().unwrap().len();
                assert!(count <= 512);
                for row in 0..count {
                    rows.push(crate::native_workflows::selected_copy(
                        Some(snapshot),
                        &request("network"),
                        Some(&(table.clone(), row)),
                    ));
                }
            }
            copies.insert(table.clone(), json!(rows));
        }
    }
    snapshot.pages = saved;
    if route == "apps" {
        for kind in ["app_processes", "executables"] {
            copies.insert(
                kind.into(),
                json!(crate::native_workflows::selected_copy(
                    Some(snapshot),
                    &request(route),
                    Some(&(kind.into(), 0)),
                )),
            );
        }
    }
    json!(copies)
}

fn emit(route: &str, report: &Value) {
    let bytes = serde_json::to_vec_pretty(report).unwrap();
    assert!(bytes.len() <= 32 * 1024 * 1024);
    if let Some(directory) = std::env::var_os("FLIGHTDECK_ITEM2_OUTPUT") {
        let directory = std::path::Path::new(&directory);
        std::fs::create_dir_all(directory).unwrap();
        std::fs::write(directory.join(format!("{route}.json")), bytes).unwrap();
    }
}

pub(super) fn record_selections(harness: &Harness, snapshots: [&Snapshot; 4]) {
    emit(
        "selections",
        &json!({"schemaVersion": 1,
        "input_root": harness.root,
        "snapshots": snapshots.map(|snapshot| json!({
            "selection": snapshot.selection, "pages": snapshot.pages, "apps": snapshot.apps,
            "detail": snapshot.detail, "history": snapshot.history,
            "availability": payloads::report(snapshot)
                .unwrap(), "ranks_and_parts": window_values(snapshot),
            "process_history": process_history(snapshot)})),
        "trace_complete": true, "trace_overflowed": false,
        "events": harness.events().iter().map(event).collect::<Vec<_>>() }),
    );
}

pub(super) fn event(event: &Event) -> Value {
    let (recording, run, publication, first, end) = match event.key {
        persistence::work::ingestion::Key::Unattributed { recording } => {
            (recording, None, None, None, None)
        }
        persistence::work::ingestion::Key::Publication { run, publication } => (
            run.recording,
            Some(run),
            Some(publication.to_string()),
            None,
            None,
        ),
        persistence::work::ingestion::Key::Minute { run, first, end } => (
            run.recording,
            Some(run),
            None,
            Some(first.to_string()),
            Some(end.to_string()),
        ),
    };
    json!({"producer": format!("{:?}", event.producer),
        "purpose": format!("{:?}", event.purpose), "kind": format!("{:?}", event.kind),
        "subject_attributed": run.is_some(),
        "recording": collector::recording::identity_text(recording).to_string(),
        "run": run.map(|run| collector::recording::identity_text(run.run).to_string()),
        "boot": run.map(|run| collector::recording::identity_text(run.boot).to_string()),
        "publication": publication, "publication_first": first, "publication_end": end,
        "table": event.table, "bytes": event.bytes.to_string()})
}

pub(super) fn record_refusal(
    case: &str,
    harness: &Harness,
    view: &viewer::store::View,
    errors: &[&str],
) {
    assert!(!errors.is_empty());
    assert!(errors.len() <= 2);
    let events = harness.events();
    let failures = events
        .iter()
        .filter(|event| {
            event.producer == persistence::work::ingestion::Producer::Worker
                && event.kind == persistence::work::ingestion::Kind::AttemptFailed
        })
        .count();

    emit(
        case,
        &json!({"schemaVersion": 1, "case": case, "worker_error": errors[0],
        "worker_errors": errors, "explicit_refusal_results": errors.len(),
        "total_worker_attempt_failures": failures,
        "store_machine": format!("{:?}", view.seconds),
        "store_apps": format!("{:?}", view.apps),
        "trace_complete": harness.backend.is_none(), "trace_overflowed": false,
        "events": events.iter().map(event).collect::<Vec<_>>()}),
    );
}

pub(super) fn record_semantics(
    case: &str,
    harness: &Harness,
    snapshot: &Snapshot,
    view: &viewer::store::View,
) {
    let report = json!({"schemaVersion": 1, "case": case,
        "selection": snapshot.selection, "apps": snapshot.apps, "detail": snapshot.detail,
        "store_machine": format!("{:?}", view.seconds),
        "store_apps": format!("{:?}", view.apps),
        "input_root": harness.root, "input_owned": harness.input.is_some(),
        "ring": harness.input.as_ref().map(|input| &input.ring),
        "trace_complete": true, "trace_overflowed": false,
        "events": harness.events().iter().map(event).collect::<Vec<_>>()});
    println!("{report}");
    emit(case, &report);
}

pub(super) fn record_sequence(harness: &Harness, observations: &Value) {
    let report = json!({"schemaVersion": 1, "case": "refusal-successor",
        "input_root": harness.root, "ring": harness.input.as_ref().map(|input| &input.ring),
        "input": {"publications": 3, "slots": 4,
            "run_ticks": [0, 1, 2], "power_nonfinite_publication": 2},
        "observations": observations,
        "trace_complete": true, "trace_overflowed": false,
        "events": harness.events().iter().map(event).collect::<Vec<_>>()});
    println!("{report}");
    emit("refusal-successor", &report);
}

pub(super) fn record_sequence_case(case: &str, harness: &Harness, observations: &Value) {
    let report = json!({"schemaVersion": 1, "case": case,
        "input_root": harness.root, "ring": harness.input.as_ref().map(|input| &input.ring),
        "observations": observations, "trace_complete": true, "trace_overflowed": false,
        "events": harness.events().iter().map(event).collect::<Vec<_>>()});
    println!("{report}");
    emit(case, &report);
}

#[test]
fn preserves_full_integer_precision_in_structured_source_work() {
    // Goal: an ordinary JSON consumer cannot silently round trace publication or byte counts.
    // Method: round-trip a maximum-integer event and check its canonical string fields.
    use collector::ring::{Identity, RunIdentity};
    use persistence::work::ingestion::{Key, Kind, Producer, Purpose};
    let value = event(&Event {
        producer: Producer::Worker,
        purpose: Purpose::Raw,
        key: Key::Publication {
            run: RunIdentity {
                recording: Identity([u64::MAX, 0]),
                run: Identity([1, 2]),
                boot: Identity([3, 4]),
            },
            publication: u64::MAX,
        },
        kind: Kind::AdmissionRead,
        table: Some("ticks"),
        bytes: u64::MAX,
    });

    let decoded: Value = serde_json::from_slice(&serde_json::to_vec(&value).unwrap()).unwrap();

    assert_eq!(decoded["publication"], "18446744073709551615");
    assert_eq!(decoded["bytes"], "18446744073709551615");
    assert_eq!(decoded["recording"], "ffffffffffffffff0000000000000000");
    assert!(serde_json::to_vec(&value).unwrap().len() < 1024);
}

fn window_values(snapshot: &Snapshot) -> Value {
    let points = snapshot.selection["trends"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    assert!(points.len() <= 4096);
    json!(
        points
            .iter()
            .map(|point| {
                let key = viewer::ObservationKey {
                    run: collector::recording::parse_identity(point["run"].as_str().unwrap())
                        .unwrap()
                        .0,
                    boot: collector::recording::parse_identity(point["boot"].as_str().unwrap())
                        .unwrap()
                        .0,
                    publication: point["publication"].as_u64().unwrap(),
                };
                let ranks = snapshot
                    .app_ranks
                    .as_ref()
                    .and_then(|ranks| ranks.get(&key));
                let part = snapshot
                    .app_parts
                    .as_ref()
                    .and_then(|parts| parts.get(&key));
                let observation = crate::live_projection::observation_identity(point);
                assert!(observation.is_object());
                assert_eq!(observation["publication"].as_u64(), Some(key.publication));
                json!({"observation": observation,
                "ranks": ranks.map(ranked), "part": part.map(|part| {
                    part.detail(viewer::live_app_response::Page {offset: 0, limit: 512}).unwrap()
                })})
            })
            .collect::<Vec<_>>()
    )
}

fn ranked(ranks: &viewer::live_app_response::Ranks) -> Value {
    json!({"selected": ranks.selected.json(), "app_count": ranks.app_count,
        "membership_complete": ranks.membership_complete,
        "ungrouped_members": ranks.ungrouped_members,
        "metrics": ranks.metrics.iter().map(|metric| json!({
            "metric": format!("{:?}", metric.metric), "unavailable": metric.unavailable,
            "overflowed": metric.overflowed, "partial": metric.partial,
            "entries": metric.entries.iter().map(|entry| json!({
                "kind": format!("{:?}", entry.app.kind), "bytes": entry.app.bytes,
                "name": entry.app.display_name, "amount": entry.amount.to_string(),
                "rate": entry.rate, "measured_members": entry.measured_members,
                "unavailable_members": entry.unavailable_members})).collect::<Vec<_>>()
        })).collect::<Vec<_>>()})
}

fn process_history(snapshot: &Snapshot) -> Value {
    use crate::live_process_history::ProcessHistory;
    match snapshot.process_history.as_deref().map(|value| &**value) {
        None => Value::Null,
        Some(ProcessHistory::Unavailable(error)) => json!({"error": error}),
        Some(ProcessHistory::Ready { payload, .. }) => payload.clone(),
        Some(ProcessHistory::All { payload, .. }) => json!({
            "selected": payload.selected.json(), "app_kind": format!("{:?}", payload.app_kind),
            "app_bytes": payload.app_bytes, "context_seconds": payload.context_seconds,
            "named_group_count": payload.named_group_count,
            "context_observations_unavailable": payload.context_observations_unavailable,
            "groups": payload.groups.iter().map(|group| match group {
                viewer::app_process_groups_history::ProcessGroup::Named(bytes) => json!(bytes),
                viewer::app_process_groups_history::ProcessGroup::Other => Value::Null,
            }).collect::<Vec<_>>(),
            "points": payload.points.iter().map(|point| json!({
                "observation": point.observation.json(), "elapsed_ns": point.elapsed_ns,
                "source_available": point.source_available,
                "membership_complete": point.membership_complete,
                "unnamed_members": point.unnamed_members,
                "readings": point.readings.iter().map(|reading| json!({
                    "present": reading.present, "members": reading.members,
                    "measures": reading.measures.iter().map(
                        viewer::live_app_response::Measure::json)
                        .collect::<Vec<_>>()
                })).collect::<Vec<_>>()
            })).collect::<Vec<_>>()
        }),
    }
}
