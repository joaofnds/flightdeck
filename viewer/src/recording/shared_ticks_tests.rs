use std::path::PathBuf;
use std::sync::Arc;

use persistence::engine::AdmittedTable;
use persistence::resources::ByteBudget;
use polars::prelude::Column;

use super::{MachineMinute, ScopedSealedMinute, SealedSource, read_machine_and_endpoints};
use crate::cancellation::Cancellation;
use crate::live::identity_fixture::Fixture;
use crate::live_apps::committed::Endpoint;

#[test]
fn one_projection_preserves_complete_machine_rows_and_exact_endpoint_fields() {
    // Goal: canonical ticks retain complete machine readings and every endpoint-only field.
    // Method: read repeated-wall and clockless real rows, with independent endpoint expectations.
    let (fixture, marker) = shared_fixture();
    let original = ScopedSealedMinute::open(&fixture.root, &marker).unwrap();
    let mut expected = MachineMinute::default();
    super::read_machine(&original, &mut expected).unwrap();
    let manifest = persistence::read_committed(&fixture.root, &marker).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let trace = persistence::work::ingestion::Trace::new(manifest.identity.run.recording);
    let scope = trace.enter();
    let minute = ScopedSealedMinute::open_with_inputs_using(
        &SealedSource {
            root: &fixture.root,
            marker: &marker,
        },
        &budget,
        &operation,
        &AdmittedTable::from_file_budgeted,
    )
    .unwrap();
    let mut machine = MachineMinute::default();

    let endpoints = read_machine_and_endpoints(&minute, &mut machine).unwrap();

    assert_eq!(machine, expected);
    assert_endpoints(&endpoints, &manifest);
    assert_ticks_work(&trace);
    minute.revalidate_full().unwrap();
    drop((scope, minute, original, machine, expected, endpoints));
    assert_eq!(budget.used(), 0);
}

#[test]
fn shared_projection_matches_the_ring_for_all_populated_machine_groups() {
    // Goal: sharing a ticks frame preserves populated and absent optional machine groups.
    // Method: compare the combined reader against independently captured native ring ticks.
    let fixture = super::sealed_system_minute();
    let mut expected = MachineMinute::default();
    for (publication, tick) in super::tests::ring_ticks(&fixture) {
        expected.push_tick(publication, &tick);
    }
    let marker = persistence::layout::WindowKey {
        run: fixture.writer.identity().unwrap().run,
        window: 0,
    }
    .marker(&fixture.root);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let minute = ScopedSealedMinute::open_with_inputs_using(
        &SealedSource {
            root: &fixture.root,
            marker: &marker,
        },
        &budget,
        &operation,
        &AdmittedTable::from_file_budgeted,
    )
    .unwrap();
    let mut machine = MachineMinute::default();

    let endpoints = read_machine_and_endpoints(&minute, &mut machine).unwrap();

    assert_eq!(machine, expected);
    assert_eq!(endpoints.len(), 3);
    assert_eq!(machine.gpu_states.len(), 4);
    assert_eq!(machine.core_states.len(), 8);
    assert_eq!(machine.fans.len(), 4);
    assert!(machine.ticks[0].system.is_some());
    assert!(machine.ticks[1].system.is_none());
    assert!(machine.ticks[2].system.is_some());
    assert!(machine.ticks[0].battery.is_some());
    assert!(machine.ticks[1].battery.is_none());
    assert!(machine.ticks[2].battery.is_some());
    assert!(machine.ticks[0].disks.is_some());
    assert!(machine.ticks[1].disks.is_some());
    assert!(machine.ticks[2].disks.is_none());
    minute.revalidate_full().unwrap();
    drop((minute, machine, expected, endpoints));
    assert_eq!(budget.used(), 0);
}

fn assert_endpoints(endpoints: &[Endpoint], manifest: &persistence::Manifest) {
    let clocks: Vec<_> = endpoints
        .iter()
        .map(|endpoint| {
            (
                endpoint.stamp.publication,
                endpoint.run_tick,
                endpoint.wall_ns,
                endpoint.stamp.continuous_ns,
                endpoint.stamp.awake_ns,
            )
        })
        .collect();
    assert_eq!(
        clocks,
        [
            (1, 0, Some(100_000_000_000), Some(0), Some(0)),
            (
                2,
                1,
                Some(100_000_000_000),
                Some(1_000_000_000),
                Some(1_000_000_000)
            ),
            (3, 2, None, None, None),
        ]
    );
    let counters: Vec<_> = endpoints
        .iter()
        .map(|endpoint| (endpoint.usage_refused, endpoint.usage_gone))
        .collect();
    assert_eq!(counters, [(1, 7), (1, 5), (1, 3)]);
    let first = manifest.events_first;
    assert_eq!(manifest.events_end, first + 3);
    let ranges: Vec<_> = endpoints
        .iter()
        .map(|endpoint| (endpoint.events_first, endpoint.events_end))
        .collect();
    assert_eq!(
        ranges,
        [
            (first, first + 1),
            (first + 1, first + 2),
            (first + 2, first + 3)
        ]
    );
    for endpoint in endpoints {
        assert_eq!(endpoint.identity, manifest.identity.run);
    }
}

fn assert_ticks_work(trace: &persistence::work::ingestion::Trace) {
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    let captures = events
        .iter()
        .filter(|event| event.kind == persistence::work::ingestion::Kind::ContentCapture)
        .filter(|event| event.table == Some("ticks"))
        .count();
    let projections = events
        .iter()
        .filter(|event| event.kind == persistence::work::ingestion::Kind::ProjectionDecode)
        .filter(|event| event.table == Some("ticks"))
        .count();
    assert_eq!(captures, 1);
    assert_eq!(projections, 1);
}

fn shared_fixture() -> (Fixture, PathBuf) {
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    fixture.seal_with_observations(&[
        persistence::Observation {
            publication: 1,
            wall_ns: Some(100_000_000_000),
            continuous_ns: Some(0),
            awake_ns: Some(0),
        },
        persistence::Observation {
            publication: 2,
            wall_ns: Some(100_000_000_000),
            continuous_ns: Some(1_000_000_000),
            awake_ns: Some(1_000_000_000),
        },
        persistence::Observation {
            publication: 3,
            wall_ns: None,
            continuous_ns: None,
            awake_ns: None,
        },
    ]);
    let marker = persistence::layout::WindowKey {
        run: fixture.writer.identity().unwrap().run,
        window: 0,
    }
    .marker(&fixture.root);
    let manifest = persistence::read_committed(&fixture.root, &marker).unwrap();
    let path = fixture
        .root
        .join(manifest.data_directory)
        .join("ticks.parquet");
    let mut frame = persistence::engine::scan(&path).unwrap().collect().unwrap();
    frame
        .with_column(Column::new("processes_gone".into(), [7_u32, 5, 3]))
        .unwrap();
    std::fs::remove_file(&path).unwrap();
    super::ticks_input_tests::write_ticks(&path, frame);
    (fixture, marker)
}
