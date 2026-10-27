use super::*;

#[test]
fn resumes_after_overwrites_exhaust_the_scan_budget() {
    // Goal: continuous overwrite yields at the scan bound without losing recovery state.
    // Method: advance a one-slot writer between actual scan steps, then finish and resume.
    let root = Directory::new();
    let store = Store::open(&root.0).unwrap();
    let mut registered = registered(store.recording());
    let mut ring = Ring::new(&root, &registered.identity, 1);
    registered.ring_name = ring.0.name().to_owned();
    let reader = RingReader::open_generation(ring.0.name()).unwrap();
    let mut state = RunState {
        next_publication: reader.next_publication(),
        loaded: true,
        ..RunState::default()
    };
    let mut scan = scanning::Scanning::new(reader, &registered, false).unwrap();
    for step in 0..3 {
        ring.publish(2 * step * 60);
        ring.publish((2 * step + 1) * 60);
        assert!(
            scan.read(&store, &registered, &mut state)
                .unwrap()
                .is_none()
        );
    }
    assert_eq!(state.next_publication, 6);
    assert_eq!(state.losses.publications, 5);
    ring.publish(6 * 60);

    let result = scan.read(&store, &registered, &mut state).unwrap();

    assert!(matches!(result, Some(Staged::Waiting)));
    assert_eq!(state.next_publication, 6);
    assert_eq!(state.losses.publications, 5);
    assert!(store.progress(&registered.identity).unwrap().is_none());
    drop(scan);
    ring.0.finish();
    let writers = Writers {
        encoder: Encoder,
        derive: &Underived,
    };
    assert!(matches!(
        persist_one(&store, &writers, &registered, false, &mut state).unwrap(),
        Progress::Sealed
    ));
    let saved = store.progress(&registered.identity).unwrap().unwrap();
    assert_eq!(saved.identity.window, 6);
    assert_eq!((saved.publication_first, saved.publication_end), (7, 8));
    assert_eq!(saved.losses.publications, 6);
    assert_eq!(state.losses, Losses::default());
    assert!(matches!(
        persist_one(&store, &writers, &registered, false, &mut state).unwrap(),
        Progress::Complete
    ));
}

#[test]
fn drains_a_final_publication_after_an_empty_read() {
    // Goal: completion observed after Nothing cannot retire an unread final publication.
    // Method: publish and finish between the empty read and the terminal decision.
    for publications in [0, 1] {
        let root = Directory::new();
        let store = Store::open(&root.0).unwrap();
        let mut registered = registered(store.recording());
        let mut ring = Ring::new(&root, &registered.identity, 1);
        registered.ring_name = ring.0.name().to_owned();
        let mut reader = RingReader::open_generation(ring.0.name()).unwrap();
        let mut tick = crate::tick_buffer(&reader.shape()).unwrap();
        let mut state = RunState {
            next_publication: reader.next_publication(),
            loaded: true,
            source: SourceState::Open,
            losses: Losses {
                publications,
                ..Losses::default()
            },
            ..RunState::default()
        };
        assert!(matches!(
            reader.read_into(&mut tick).unwrap(),
            Found::Nothing
        ));
        assert!(!reader.completed());
        ring.publish(0);
        ring.0.finish();

        let result = persist_one_stage_end(&reader, &registered, false, &mut state).unwrap();

        assert!(matches!(result, Staged::Waiting));
        assert_eq!(state.source, SourceState::Open);
        drop(reader);
        let writers = Writers {
            encoder: Encoder,
            derive: &Underived,
        };
        assert!(matches!(
            persist_one(&store, &writers, &registered, false, &mut state).unwrap(),
            Progress::Sealed
        ));
        let saved = store.progress(&registered.identity).unwrap().unwrap();
        assert_eq!((saved.publication_first, saved.publication_end), (1, 2));
        assert_eq!(saved.losses.publications, publications);
        assert!(matches!(
            persist_one(&store, &writers, &registered, false, &mut state).unwrap(),
            Progress::Complete
        ));
    }
}

#[test]
fn waits_for_the_open_successor_after_losing_the_selected_minute() {
    // Goal: losing a selected minute cannot seal its still-open successor.
    // Method: overwrite the selection, resume the worker, then finish and inspect its seal.
    let root = Directory::new();
    let store = Store::open(&root.0).unwrap();
    let mut registered = registered(store.recording());
    registered.collector_pid = std::process::id();
    let mut ring = Ring::new(&root, &registered.identity, 2);
    registered.ring_name = ring.0.name().to_owned();
    ring.publish(0);
    ring.publish(60);
    let mut reader = RingReader::open_generation(ring.0.name()).unwrap();
    let limits = Limits::from_shape(&reader.shape()).unwrap();
    let mut tick = crate::tick_buffer(&reader.shape()).unwrap();
    let selected = WindowIdentity {
        run: registered.identity,
        window: 0,
    };
    let mut newest = None;
    assert!(
        !persist_one_closed(&mut reader, &mut newest, registered.collector_pid, false).unwrap()
    );
    assert!(window_ready(&reader, newest.as_ref(), &selected, false).unwrap());
    ring.publish(61);
    reader.seek_publication(1).unwrap();
    let mut state = RunState {
        next_publication: 1,
        loaded: true,
        ..RunState::default()
    };
    let writers = Writers {
        encoder: Encoder,
        derive: &Underived,
    };

    assert!(
        persist_one_stage_selected(&mut reader, &mut tick, selected, limits, &mut state)
            .unwrap()
            .is_none()
    );
    drop(reader);
    assert!(matches!(
        persist_one(&store, &writers, &registered, false, &mut state).unwrap(),
        Progress::Waiting
    ));
    ring.0.finish();
    assert!(matches!(
        persist_one(&store, &writers, &registered, false, &mut state).unwrap(),
        Progress::Sealed
    ));

    assert!(store.existing(&selected).unwrap().is_none());
    let saved = store.progress(&registered.identity).unwrap().unwrap();
    assert_eq!(
        (
            saved.identity.window,
            saved.publication_first,
            saved.publication_end
        ),
        (1, 2, 4)
    );
    assert_eq!(saved.losses.publications, 1);
    assert_eq!(state.losses, Losses::default());
}

#[test]
fn leaves_a_missed_suffix_for_the_next_minute() {
    // Goal: a partial minute does not claim loss beyond its saved publication end.
    // Method: copy one tick, overwrite its successor, then reach the next minute.
    let root = Directory::new();
    let store = Store::open(&root.0).unwrap();
    let mut registered = registered(store.recording());
    let run = registered.identity;
    let mut ring = Ring::new(&root, &run, 3);
    registered.ring_name = ring.0.name().to_owned();
    ring.publish(0);
    ring.publish(1);
    ring.publish(60);
    let mut reader = RingReader::open_generation(ring.0.name()).unwrap();
    let limits = Limits::from_shape(&reader.shape()).unwrap();
    let mut tick = crate::tick_buffer(&reader.shape()).unwrap();
    let mut staged = staging::Staging::new(&reader, WindowIdentity { run, window: 0 }, limits);
    let mut losses = Losses::default();
    assert!(staged.read(&mut reader, &mut tick, &mut losses).unwrap());
    ring.publish(61);
    ring.publish(62);

    assert!(staged.read(&mut reader, &mut tick, &mut losses).unwrap());
    assert!(!staged.read(&mut reader, &mut tick, &mut losses).unwrap());
    let window = staged.finish(&mut losses);

    assert_eq!(window.publication_range(), 1..2);
    assert_eq!(losses.publications, 0);
    assert_eq!(reader.next_publication(), 3);
    drop(reader);
    let writers = Writers {
        encoder: Encoder,
        derive: &Underived,
    };
    assert!(
        seal(
            &store,
            &writers,
            &window,
            &RunState {
                losses,
                ..RunState::default()
            }
        )
        .unwrap()
    );
    drop(store);
    ring.0.finish();
    let recovered = Store::open(&root.0).unwrap();
    let mut state = RunState::default();
    assert!(matches!(
        persist_one(&recovered, &writers, &registered, false, &mut state).unwrap(),
        Progress::Sealed
    ));
    let saved = recovered.progress(&run).unwrap().unwrap();
    assert_eq!((saved.publication_first, saved.publication_end), (3, 6));
    assert_eq!(saved.losses.publications, 1);
    assert_eq!(state.losses, Losses::default());
}

#[test]
fn advances_a_wholly_lost_selection_without_returning_an_empty_window() {
    // Goal: an overwritten selection advances without entering the encoder.
    // Method: select minute zero, overwrite it, and use the worker's selected-minute step.
    let root = Directory::new();
    let store = Store::open(&root.0).unwrap();
    let run = registered(store.recording()).identity;
    let mut ring = Ring::new(&root, &run, 2);
    ring.publish(0);
    ring.publish(60);
    let mut reader = RingReader::open_generation(ring.0.name()).unwrap();
    let limits = Limits::from_shape(&reader.shape()).unwrap();
    let mut tick = crate::tick_buffer(&reader.shape()).unwrap();
    assert!(matches!(
        reader.read_into(&mut tick).unwrap(),
        Found::Publication { .. }
    ));
    let selected = WindowIdentity { run, window: 0 };
    let newest = persist_one_latest_timing(&mut reader).unwrap();
    assert!(window_ready(&reader, newest.as_ref(), &selected, false).unwrap());
    ring.publish(61);
    reader.seek_publication(1).unwrap();
    let mut state = RunState {
        next_publication: 1,
        loaded: true,
        ..RunState::default()
    };

    let result =
        persist_one_stage_selected(&mut reader, &mut tick, selected, limits, &mut state).unwrap();

    assert!(result.is_none());
    assert_eq!(state.next_publication, 2);
    assert_eq!(state.losses.publications, 1);
}

#[test]
fn carries_event_loss_only_with_the_minute_that_copied_its_reading() {
    // Goal: lost event records stay with their saved tick and are not counted twice.
    // Method: overwrite the first tick's event, then seal both sides of a minute boundary.
    let root = Directory::new();
    let store = Store::open(&root.0).unwrap();
    let mut registered = registered(store.recording());
    let mut ring = Ring::new(&root, &registered.identity, 3);
    registered.ring_name = ring.0.name().to_owned();
    let event = Event::Start {
        pid: 41,
        coalition: None,
    };
    ring.publish_events(0, std::slice::from_ref(&event));
    ring.publish_events(1, std::slice::from_ref(&event));
    ring.publish(60);
    ring.0.finish();
    let writers = Writers {
        encoder: Encoder,
        derive: &Underived,
    };
    let mut state = RunState::default();

    assert!(matches!(
        persist_one(&store, &writers, &registered, false, &mut state).unwrap(),
        Progress::Sealed
    ));
    let first = store.progress(&registered.identity).unwrap().unwrap();
    assert!(matches!(
        persist_one(&store, &writers, &registered, false, &mut state).unwrap(),
        Progress::Sealed
    ));
    let next = store.progress(&registered.identity).unwrap().unwrap();

    assert_eq!(first.identity.window, 0);
    assert_eq!(first.losses.events, 1);
    assert_eq!(next.identity.window, 1);
    assert_eq!(next.losses, Losses::default());
}

#[test]
fn refuses_a_lost_selection_that_has_not_advanced() {
    // Goal: an empty selected step cannot retry without making progress.
    // Method: ask the real source for a publication beyond its current end.
    let root = Directory::new();
    let store = Store::open(&root.0).unwrap();
    let run = registered(store.recording()).identity;
    let mut ring = Ring::new(&root, &run, 2);
    ring.publish(0);
    let mut reader = RingReader::open_generation(ring.0.name()).unwrap();
    let limits = Limits::from_shape(&reader.shape()).unwrap();
    let mut tick = crate::tick_buffer(&reader.shape()).unwrap();
    reader.seek_publication(2).unwrap();
    let mut state = RunState::default();

    let result = persist_one_stage_selected(
        &mut reader,
        &mut tick,
        WindowIdentity { run, window: 0 },
        limits,
        &mut state,
    );

    assert!(matches!(
        result,
        Err(crate::Error::Bound("lost selection without progress"))
    ));
    assert_eq!(state.next_publication, 0);
    assert_eq!(state.losses, Losses::default());
}

#[test]
fn completes_an_empty_source_with_only_prior_unknown_loss() {
    // Goal: prior registry eviction history does not invent known loss in an empty source.
    // Method: drain a closed empty real ring with only the prior-unknown flag set.
    let root = Directory::new();
    let store = Store::open(&root.0).unwrap();
    let mut registered = registered(store.recording());
    let ring = Ring::new(&root, &registered.identity, 1);
    registered.ring_name = ring.0.name().to_owned();
    let writers = Writers {
        encoder: Encoder,
        derive: &Underived,
    };
    let mut state = RunState {
        losses: Losses {
            unknown: true,
            ..Losses::default()
        },
        ..RunState::default()
    };

    let result = persist_one(&store, &writers, &registered, false, &mut state).unwrap();

    assert!(matches!(result, Progress::Complete));
    assert_eq!(state.source, SourceState::Complete);
    assert!(state.losses.unknown);
}

struct Ring(collector::ring::Writer, u64);

impl Ring {
    fn new(root: &Directory, identity: &RunIdentity, slot_count: u64) -> Self {
        let place = collector::ring::Place {
            lock_path: root.0.join("ring.lock"),
            name_prefix: format!("/fdlost{}", std::process::id()),
        };
        let shape = Shape {
            slot_count,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 1,
        };

        let awake_ns =
            collector::recording::observation_times(collector::clock::Timebase::read().unwrap())
                .unwrap()
                .awake_ns;

        Self(
            collector::ring::Writer::create_recording(&place, shape, identity, 1).unwrap(),
            awake_ns,
        )
    }

    fn publish(&mut self, run_tick: u64) {
        self.publish_events(run_tick, &[]);
    }

    fn publish_events(&mut self, run_tick: u64, events: &[Event]) {
        let memory = collector::machine::Memory::default();
        let readings = collector::ring::Readings {
            run_tick,
            cpus: &[],
            memory: &memory,
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events,
            dropped: collector::ring::Dropped::default(),
        };
        let elapsed_ns = run_tick * 1_000_000_000;

        self.0.publish_observed(
            &readings,
            &collector::ring::ObservationTimes {
                wall_ns: 1_000_000_000_000 + elapsed_ns,
                continuous_ns: 1 + elapsed_ns,
                awake_ns: self.1 + elapsed_ns,
            },
        );
    }
}

impl Drop for Ring {
    fn drop(&mut self) {
        collector::ring::unlink_generation(self.0.name()).unwrap();
    }
}
