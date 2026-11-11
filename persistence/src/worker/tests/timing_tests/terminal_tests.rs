use super::*;
use std::sync::Arc;

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
    let budget = Arc::new(ByteBudget::new(1 << 28));
    let catalog = crate::canonical::catalog::Catalog::open(&source.root.0, &budget).unwrap();
    let page = catalog
        .event_objects_page(None, 8, &mut crate::canonical::ReadWork::default())
        .unwrap();
    assert_eq!(page.objects.len(), 1);
    assert_eq!(page.objects[0].recovery.run, source.registered.identity);
    assert_eq!(page.objects[0].recovery.rows, COUNT);
    assert!(
        source
            .store
            .saved_newest(&source.registered.identity)
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
            .saved_newest(&source.registered.identity)
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
        .saved_newest(&source.registered.identity)
        .unwrap()
        .unwrap();
    assert_eq!(last.identity.window, 1);
    assert_eq!(last.publication_end, 6);
    assert_eq!(last.events_end, 6);
    assert_eq!(source.state.committed, 5);
}

#[test]
fn reports_the_end_of_the_events_a_sealed_minute_saved() {
    // Goal: the persister reports saved the events its sealed minute holds and none after
    // them, since the collector's event log lets go of the events before the end reported.
    // Method: seal a minute of three ticks of which one logged an event, so the events end
    // where no count of ticks does, report progress into a control file and compare the
    // persister's record with the saved minute.
    let mut source = sealed_minute();

    let reported = reported_progress(&mut source);

    let saved = source
        .store
        .saved_newest(&source.registered.identity)
        .unwrap()
        .unwrap();
    assert_eq!(source.registered.identity.run, reported.run);
    assert_eq!(saved.publication_end, reported.publication + 1);
    assert_eq!(saved.events_end, reported.events_end);
}

#[test]
fn reports_the_same_end_of_saved_events_when_it_starts_again() {
    // Goal: a persister that starts again reports saved the events the recording holds and
    // none after them, although it sealed nothing itself.
    // Method: seal a minute, forget the run's state as a new process does, pass again with
    // nothing more to seal, and compare the persister's record with the saved minute.
    let mut source = sealed_minute();
    source.state = RunState::default();
    assert!(matches!(source.pass().unwrap(), Progress::Waiting));

    let reported = reported_progress(&mut source);

    let saved = source
        .store
        .saved_newest(&source.registered.identity)
        .unwrap()
        .unwrap();
    assert_eq!(saved.publication_end, reported.publication + 1);
    assert_eq!(saved.events_end, reported.events_end);
}

#[test]
fn reports_no_saved_events_of_a_run_with_no_saved_minute() {
    // Goal: the persister reports no event saved of a run it has read ticks of and sealed no
    // minute of, although it knows which event it saves first.
    // Method: pass over a source whose first minute is still open, report progress and read
    // the persister's record.
    let mut source = Scenario::new();
    source.publish(0);
    source.publish(1);
    assert!(matches!(source.pass().unwrap(), Progress::Waiting));
    assert!(source.state.next_event > 0);

    let reported = reported_progress(&mut source);

    assert_eq!(0, reported.events_end);
}

#[test]
fn reports_the_newest_run_with_a_saved_minute() {
    // Goal: with a saved minute of each of two runs, the persister reports the newer run's,
    // since the newer run's collector is the one whose log waits on the report.
    // Method: report the progress of an older and a newer run's state, each with a saved
    // minute, and read the persister's record.
    let run = |run: u64| RunIdentity {
        recording: Identity([1, 2]),
        run: Identity([run, 0]),
        boot: Identity([5, 6]),
    };
    let older = RunState {
        identity: Some(run(3)),
        committed: 3,
        next_event: 5,
        ..RunState::default()
    };
    let newer = RunState {
        identity: Some(run(4)),
        committed: 7,
        next_event: 11,
        ..RunState::default()
    };
    let recovery = Recovery {
        states: [older, newer],
        ..Recovery::default()
    };

    let reported = reported_recovery(&recovery);

    assert_eq!(Identity([4, 0]), reported.run);
    assert_eq!(7, reported.publication);
    assert_eq!(11, reported.events_end);
}

/// A source with one sealed minute of three ticks, of which only the first logged an event,
/// and a fourth tick that closed the minute.
fn sealed_minute() -> Scenario {
    let mut source = Scenario::new();
    source.publish(0);
    source.publish_quiet(1);
    source.publish_quiet(2);
    source.publish(60);
    assert!(matches!(source.pass().unwrap(), Progress::Sealed));
    source
}

/// The persister's record after it reports the progress of the run of `source`, whose state
/// the source keeps.
fn reported_progress(source: &mut Scenario) -> collector::recording::control::Health {
    // The worker names the run of a state when it gives the run that state, which a scenario
    // that passes one state by hand leaves out.
    let state = RunState {
        identity: Some(source.registered.identity),
        ..std::mem::take(&mut source.state)
    };
    let mut recovery = Recovery {
        states: [state, RunState::default()],
        ..Recovery::default()
    };

    let reported = reported_recovery(&recovery);

    source.state = std::mem::take(&mut recovery.states[0]);
    reported
}

/// The persister's record after it reports the progress of the runs of `recovery`.
fn reported_recovery(recovery: &Recovery) -> collector::recording::control::Health {
    use collector::recording::control::{Control, Reporter, Role};
    // A store holds its recording's persister lock, which refuses a new control file, so the
    // record is written beside a recording of its own.
    let root = Directory::new();
    let control = Control::create(&root.0).unwrap();
    let mut health = Some(Reporter::new(control, Role::Persister).unwrap());

    report_progress(&mut health, recovery).unwrap();

    health
        .as_ref()
        .unwrap()
        .control()
        .read(Role::Persister)
        .unwrap()
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
        .saved_newest(&source.registered.identity)
        .unwrap()
        .unwrap();
    assert_eq!(manifest.event_batches, 4);
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
