use super::*;
use std::sync::Arc;

macro_rules! terminal_fault {
    ($name:ident, $boundary:ident) => {
        #[test]
        fn $name() {
            // Goal: every terminal interruption recovers one exact canonical event set.
            // Method: interrupt the Store boundary, reopen ownership, then run recovery.
            recover_terminal_fault(crate::CommitBoundary::$boundary);
        }
    };
}

terminal_fault!(recovers_terminal_before_files_sync, BeforeFilesSync);
terminal_fault!(recovers_terminal_after_files_sync, FilesSynced);
terminal_fault!(recovers_terminal_before_metadata, BeforeMetadataWrite);
terminal_fault!(recovers_terminal_after_metadata, MetadataSynced);
terminal_fault!(recovers_terminal_before_pending_sync, BeforePendingSync);
terminal_fault!(recovers_terminal_after_pending_sync, PendingSynced);
terminal_fault!(recovers_terminal_before_ready_rename, BeforeReadyRename);
terminal_fault!(recovers_terminal_after_ready_rename, ReadyRenamed);
terminal_fault!(recovers_terminal_before_ready_sync, BeforeReadySync);
terminal_fault!(recovers_terminal_after_ready_sync, ReadySynced);
terminal_fault!(recovers_terminal_before_marker_link, BeforeMarkerLink);
terminal_fault!(recovers_terminal_after_marker_link, MarkerLinked);
terminal_fault!(recovers_terminal_before_marker_sync, BeforeMarkerSync);
terminal_fault!(recovers_terminal_after_marker_sync, MarkerSynced);

macro_rules! ordinary_fault {
    ($name:ident, $boundary:ident) => {
        #[test]
        fn $name() {
            // Goal: event parts and observations recover together at every canonical boundary.
            // Method: interrupt the production window commit, reopen and check the exact set.
            recover_ordinary_fault(crate::CommitBoundary::$boundary);
        }
    };
}

ordinary_fault!(recovers_parts_before_files_sync, BeforeFilesSync);
ordinary_fault!(recovers_parts_after_files_sync, FilesSynced);
ordinary_fault!(recovers_parts_before_metadata, BeforeMetadataWrite);
ordinary_fault!(recovers_parts_after_metadata, MetadataSynced);
ordinary_fault!(recovers_parts_before_pending_sync, BeforePendingSync);
ordinary_fault!(recovers_parts_after_pending_sync, PendingSynced);
ordinary_fault!(recovers_parts_before_ready_rename, BeforeReadyRename);
ordinary_fault!(recovers_parts_after_ready_rename, ReadyRenamed);
ordinary_fault!(recovers_parts_before_ready_sync, BeforeReadySync);
ordinary_fault!(recovers_parts_after_ready_sync, ReadySynced);
ordinary_fault!(recovers_parts_before_marker_link, BeforeMarkerLink);
ordinary_fault!(recovers_parts_after_marker_link, MarkerLinked);
ordinary_fault!(recovers_parts_before_marker_sync, BeforeMarkerSync);
ordinary_fault!(recovers_parts_after_marker_sync, MarkerSynced);

fn recover_ordinary_fault(boundary: crate::CommitBoundary) {
    let mut source = Scenario::at_retention(Directory::new(), 8, 32);
    for tick in 0..40 {
        source.publish(tick);
    }
    source.ring.0.finish();
    let mut reader = RingReader::open_generation(source.ring.0.name()).unwrap();
    // Match persist_one's initial durable cursor, rather than the reader's retained floor.
    reader.seek_publication(reader.publication_first()).unwrap();
    let Staged::Window(staged) = persist_one_stage(
        reader,
        &source.store,
        &source.registered,
        true,
        &mut source.state,
    )
    .unwrap() else {
        panic!("ordinary selected window")
    };
    let tables = Encoder
        .encode(&staged.window, staged.pending.path())
        .unwrap();
    let manifest = seal_manifest(&staged.window, tables, source.state.losses).unwrap();
    let failed = source
        .store
        .commit_observed(staged.pending, manifest, &mut |stage| {
            if stage == boundary {
                Err(io::Error::other("owned ordinary commit fault"))
            } else {
                Ok(())
            }
        });
    assert!(failed.is_err(), "{boundary:?}");
    drop(staged.window);
    drop(source.store);
    source.store = Store::open(&source.root.0).unwrap();
    source.state = RunState::default();
    assert!(matches!(
        source.pass_retired(true).unwrap(),
        Progress::Sealed | Progress::Complete
    ));
    assert!(matches!(
        source.pass_retired(true).unwrap(),
        Progress::Complete
    ));
    recover_ordinary_assert(&source);
}

fn recover_ordinary_assert(source: &Scenario) {
    let manifest = source
        .store
        .progress(&source.registered.identity)
        .unwrap()
        .unwrap();
    assert_eq!(manifest.publication_first..manifest.publication_end, 33..41);
    assert_eq!(manifest.events_first..manifest.events_end, 9..41);
    assert_eq!(manifest.event_batches, Some(1));
    assert_eq!(
        manifest.losses,
        Losses {
            publications: 32,
            events: 8,
            unknown: false
        }
    );
    let budget = Arc::new(ByteBudget::new(128 * 1024 * 1024));
    let table =
        crate::event_table::EventTable::open_budgeted(&source.root.0, &manifest, &budget).unwrap();
    let part = table.part(0).unwrap();
    let frame = part.read_slice(0, 16_384, &budget).unwrap();
    let events: Vec<_> = frame
        .rows()
        .column("event")
        .unwrap()
        .u64()
        .unwrap()
        .into_no_null_iter()
        .collect();
    assert_eq!(events, (9..41).collect::<Vec<_>>());
    assert_eq!(frame.rows().column("publication").unwrap().null_count(), 24);
    drop(frame);
    drop(part);
    drop(table);
    assert_eq!(budget.used(), 0);
}

fn recover_terminal_fault(boundary: crate::CommitBoundary) {
    let mut source = event_only_source();
    let reader = RingReader::open_generation(&source.registered.ring_name).unwrap();
    let pending = match source
        .store
        .begin_events(&source.registered.identity)
        .unwrap()
    {
        crate::store::EventBegin::Pending(pending) => pending,
        crate::store::EventBegin::Committed(_) => panic!("fresh terminal unexpectedly committed"),
    };
    let manifest = crate::event_recovery::write_tail(
        &pending.path,
        reader,
        &source.registered.identity,
        1..3,
        Losses::default(),
        usize::try_from(WORKING_BYTES_MAX).unwrap(),
    )
    .unwrap();
    let failed = source.store.commit_events(pending, manifest, &mut |stage| {
        if stage == boundary {
            Err(io::Error::other("owned terminal commit fault"))
        } else {
            Ok(())
        }
    });
    assert!(failed.is_err(), "{boundary:?}");
    drop(source.store);
    source.store = Store::open(&source.root.0).unwrap();
    source.state = RunState::default();

    assert!(matches!(
        source.pass_retired(true).unwrap(),
        Progress::Complete
    ));
    assert!(matches!(
        source.pass_retired(true).unwrap(),
        Progress::Complete
    ));

    let page = crate::event_recovery::read_page(&source.root.0, &source.registered.identity, 1, 8)
        .unwrap();
    let events: Vec<_> = page
        .rows
        .column("event")
        .unwrap()
        .u64()
        .unwrap()
        .into_no_null_iter()
        .collect();
    assert_eq!(events, vec![1, 2], "{boundary:?}");
    assert_eq!(page.rows.column("publication").unwrap().null_count(), 2);
    assert_eq!(page.coverage, 1..3);
    assert_eq!(page.losses, Losses::default());
    assert!(
        source
            .store
            .progress(&source.registered.identity)
            .unwrap()
            .is_none()
    );
}

fn event_only_source() -> Scenario {
    let mut source = Scenario::new();
    source.publish(0);
    source.publish(1);
    let reader = RingReader::open_generation(source.ring.0.name()).unwrap();
    let snapshot = WritableSource::new(
        source.ring.0.name(),
        reader.mapped_size_bytes(),
        ForgedWord {
            watched: 4,
            expected: 2,
            index: 4,
            value: 0,
        },
    );
    source.registered.ring_name.clone_from(&snapshot.name);
    source.snapshot = Some(snapshot);
    source
}

#[test]
fn a_multibatch_event_only_run_pages_every_event_after_registry_eviction() {
    // Goal: bounded terminal batches preserve raw history without saved minutes or registry.
    // Method: recover three full batches plus eight real identity events, evict registry and page.
    const COUNT: u64 = 49_160;
    let mut source = Scenario::at_retention(Directory::new(), 4, COUNT);
    for tick in 0..COUNT {
        source.publish(tick);
    }
    withhold_publication(&mut source, COUNT, 0);

    assert!(matches!(
        source.pass_retired(true).unwrap(),
        Progress::Complete
    ));
    Registry::default().write(&source.root.0).unwrap();

    assert_terminal_events(&source, 1..COUNT + 1, Losses::default());
    let mut buffer: Box<[_; collector::recording::discovery::DIRECTORY_BUFFER_WORDS]> =
        vec![0; collector::recording::discovery::DIRECTORY_BUFFER_WORDS]
            .into_boxed_slice()
            .try_into()
            .unwrap();
    let mut discovery =
        crate::event_recovery::Discovery::open(&source.root.0, &mut buffer).unwrap();
    let page = discovery.next_page(8).unwrap();
    assert!(page.complete);
    assert_eq!(page.runs.len(), 1);
    assert_eq!(page.runs[0].run, source.registered.identity);
    assert_eq!(page.runs[0].rows, COUNT);
    assert!(
        source
            .store
            .progress(&source.registered.identity)
            .unwrap()
            .is_none()
    );
}

#[test]
fn overwriting_the_only_observation_preserves_both_events_and_publication_loss() {
    // Goal: an in-flight overwrite cannot erase events or invent a replacement observation.
    // Method: close after event2 overwrites slot1 but before publication2 becomes visible.
    let mut source = Scenario::at_retention(Directory::new(), 1, 2);
    source.publish(0);
    source.publish(1);
    withhold_publication(&mut source, 2, 1);

    assert!(matches!(
        source.pass_retired(true).unwrap(),
        Progress::Complete
    ));

    assert_terminal_events(
        &source,
        1..3,
        Losses {
            publications: 1,
            ..Losses::default()
        },
    );
    assert!(
        source
            .store
            .progress(&source.registered.identity)
            .unwrap()
            .is_none()
    );
}

#[test]
fn the_event_after_the_last_sealed_observation_commits_without_an_extra_minute() {
    // Goal: terminal event durability never changes the final observed publication/time range.
    // Method: seal an ordinary minute, then withhold the last publication of its closed tail.
    let mut source = Scenario::new();
    source.publish(0);
    source.publish(60);
    assert!(matches!(source.pass().unwrap(), Progress::Sealed));
    for tick in 61..65 {
        source.publish(tick);
    }
    withhold_publication(&mut source, 6, 5);

    assert!(matches!(
        source.pass_retired(true).unwrap(),
        Progress::Sealed
    ));
    assert!(matches!(
        source.pass_retired(true).unwrap(),
        Progress::Complete
    ));

    assert_terminal_events(&source, 6..7, Losses::default());
    let last = source
        .store
        .progress(&source.registered.identity)
        .unwrap()
        .unwrap();
    assert_eq!(last.identity.window, 1);
    assert_eq!(last.publication_end, 6);
    assert_eq!(last.events_end, 6);
    assert_eq!(source.state.committed, 5);
}

fn withhold_publication(source: &mut Scenario, published: u64, visible: u64) {
    let reader = RingReader::open_generation(source.ring.0.name()).unwrap();
    let snapshot = WritableSource::new(
        source.ring.0.name(),
        reader.mapped_size_bytes(),
        ForgedWord {
            watched: 4,
            expected: published,
            index: 4,
            value: visible,
        },
    );
    source.registered.ring_name.clone_from(&snapshot.name);
    source.snapshot = Some(snapshot);
}

fn assert_terminal_events(source: &Scenario, expected: std::ops::Range<u64>, losses: Losses) {
    let mut first = expected.start;
    let mut actual = Vec::new();
    for _page in 0..=crate::event_observation::ROWS_MAX.div_ceil(8192) {
        let page = crate::event_recovery::read_page(
            &source.root.0,
            &source.registered.identity,
            first,
            8192,
        )
        .unwrap();
        assert!(page.rows.height() <= 8192);
        assert_eq!(
            page.rows.column("publication").unwrap().null_count(),
            page.rows.height()
        );
        assert_eq!(page.coverage, expected);
        assert_eq!(page.losses, losses);
        actual.extend(
            page.rows
                .column("event")
                .unwrap()
                .u64()
                .unwrap()
                .into_no_null_iter(),
        );
        if let Some(next) = page.next {
            first = next;
        } else {
            assert_eq!(actual, expected.collect::<Vec<_>>());
            return;
        }
    }
    panic!("terminal paging did not reach EOF");
}

#[test]
fn a_large_retained_prefix_streams_parts_inside_one_ordinary_window_commit() {
    // Goal: admitted detached history never requires a whole-window event frame or file.
    // Method: seal49,160events with only8observations and read all physical parts in order.
    const COUNT: u64 = 49_160;
    let mut source = Scenario::at_retention(Directory::new(), 8, COUNT);
    for tick in 0..COUNT {
        source.publish(tick);
    }
    source.ring.0.finish();

    assert!(matches!(source.pass().unwrap(), Progress::Sealed));

    let manifest = source
        .store
        .progress(&source.registered.identity)
        .unwrap()
        .unwrap();
    assert_eq!(manifest.event_batches, Some(4));
    assert_eq!(
        manifest.publication_first..manifest.publication_end,
        COUNT - 7..COUNT + 1
    );
    assert_eq!(manifest.events_first..manifest.events_end, 1..COUNT + 1);
    assert_eq!(
        manifest.losses,
        Losses {
            publications: COUNT - 8,
            ..Losses::default()
        }
    );
    let budget = Arc::new(ByteBudget::new(128 * 1024 * 1024));
    let table =
        crate::event_table::EventTable::open_budgeted(&source.root.0, &manifest, &budget).unwrap();
    let mut expected = 1;
    let mut associated = 0;
    for index in 0..table.len() {
        let part = table.part(index).unwrap();
        let frame = part.read_slice(0, 16_384, &budget).unwrap();
        assert!(frame.rows().height() <= 16_384);
        associated +=
            frame.rows().height() - frame.rows().column("publication").unwrap().null_count();
        for number in frame
            .rows()
            .column("event")
            .unwrap()
            .u64()
            .unwrap()
            .into_no_null_iter()
        {
            assert_eq!(number, expected);
            expected += 1;
        }
    }
    assert_eq!(expected, COUNT + 1);
    assert_eq!(associated, 8);
    drop(table);
    assert_eq!(budget.used(), 0);
}
