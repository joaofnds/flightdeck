use persistence::work::ingestion::{self, Key, Kind, Purpose, Trace};

use super::SealedMinute;
use crate::live::identity_fixture::Fixture;

#[test]
fn attributes_retained_ticks_projection_after_open_returns() {
    // Goal: a retained admitted input keeps source identity without freezing caller purpose.
    // Method: open ticks first, then project them under a later raw-demand purpose.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let marker = marker(&fixture);
    let manifest = persistence::read_committed(&fixture.root, &marker).unwrap();
    let budget = std::sync::Arc::new(persistence::resources::ByteBudget::new(64 * 1024 * 1024));
    let operation = std::sync::Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let trace = Trace::new(manifest.identity.run.recording);
    let scope = trace.enter();
    let minute = SealedMinute::open_with_ticks(
        &super::SealedSource {
            root: &fixture.root,
            marker: &marker,
        },
        &budget,
        &operation,
    )
    .unwrap();
    let purpose = ingestion::purpose(Purpose::Raw);

    let rows = minute
        .ticks()
        .unwrap()
        .collect(&["publication"], None)
        .unwrap();
    drop(purpose);
    drop(scope);
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();

    assert_eq!(rows.height(), 1);
    let decoded: Vec<_> = events
        .iter()
        .filter(|event| event.kind == Kind::ProjectionDecode)
        .map(|event| (event.key, event.table, event.purpose))
        .collect();
    assert_eq!(
        decoded,
        [(
            Key::Minute {
                run: manifest.identity.run,
                first: manifest.publication_first,
                end: manifest.publication_end,
            },
            Some("ticks"),
            Purpose::Raw
        )]
    );
}

#[test]
fn attributes_deferred_tables_to_their_sources_and_current_purpose() {
    // Goal: retained plans keep their source identity through later independent collection.
    // Method: construct two tables, then collect under a different ambient source and purpose.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let minute = SealedMinute::open(&fixture.root, &marker(&fixture)).unwrap();
    let trace = Trace::new(minute.manifest.identity.run.recording);
    let scope = trace.enter();
    let cpus = minute.scan("cpus").unwrap();
    let memory = minute.scan("memory").unwrap();
    let ambient = Key::Publication {
        run: minute.manifest.identity.run,
        publication: 99,
    };
    let source = ingestion::source(ambient, Some("events"));
    let purpose = ingestion::purpose(Purpose::Predecessor);

    cpus.collect(&["publication", "cpu"]).unwrap();
    memory.collect(&["publication", "total_bytes"]).unwrap();
    ingestion::record_content(Kind::MachineReduction, 0);
    drop(purpose);
    drop(source);
    drop(scope);
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();

    let expected = Key::Minute {
        run: minute.manifest.identity.run,
        first: minute.manifest.publication_first,
        end: minute.manifest.publication_end,
    };
    let collected: Vec<_> = events
        .iter()
        .filter(|event| event.kind == Kind::NativeCollect)
        .map(|event| (event.key, event.table, event.purpose))
        .collect();
    assert_eq!(
        collected,
        [
            (expected, Some("cpus"), Purpose::Predecessor),
            (expected, Some("memory"), Purpose::Predecessor),
        ]
    );
    let restored = events.last().unwrap();
    assert_eq!(restored.key, ambient);
    assert_eq!(restored.table, Some("events"));
    assert_eq!(restored.purpose, Purpose::Predecessor);
}

fn marker(fixture: &Fixture) -> std::path::PathBuf {
    persistence::layout::WindowKey {
        run: fixture.writer.identity().unwrap().run,
        window: 0,
    }
    .marker(&fixture.root)
}

#[test]
fn filters_foreign_deferred_table_collection() {
    // Goal: a deferred foreign table cannot inherit the observer's recording identity.
    // Method: collect a real sealed table while observing a guaranteed different recording.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let minute = SealedMinute::open(&fixture.root, &marker(&fixture)).unwrap();
    let recording = minute.manifest.identity.run.recording.0;
    let trace = Trace::new(collector::ring::Identity([recording[0] ^ 1, recording[1]]));
    let scope = trace.enter();
    let table = minute.scan("memory").unwrap();

    let rows = table.collect(&["publication", "total_bytes"]).unwrap();
    drop(scope);
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();

    assert_eq!(rows.height(), 1);
    assert!(events.is_empty());
}

#[test]
fn filters_foreign_retained_ticks_projection() {
    // Goal: projection of retained bytes keeps its own recording under a later observer.
    // Method: open native ticks first, then decode under a different recording's trace.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let marker = marker(&fixture);
    let budget = std::sync::Arc::new(persistence::resources::ByteBudget::new(64 * 1024 * 1024));
    let operation = std::sync::Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let minute = SealedMinute::open_with_ticks(
        &super::SealedSource {
            root: &fixture.root,
            marker: &marker,
        },
        &budget,
        &operation,
    )
    .unwrap();
    let recording = minute.manifest.identity.run.recording.0;
    let trace = Trace::new(collector::ring::Identity([recording[0] ^ 1, recording[1]]));
    let scope = trace.enter();

    let rows = minute
        .ticks()
        .unwrap()
        .collect(&["publication"], None)
        .unwrap();
    drop(scope);
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();

    assert_eq!(rows.height(), 1);
    assert!(events.is_empty());
}
