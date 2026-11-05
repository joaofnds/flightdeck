use super::forged_source::{ForgedWord, HEADER_EVENT_NEWEST, WritableSource};
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
    assert!(store.saved_newest(&registered.identity).unwrap().is_none());
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
    let saved = store.saved_newest(&registered.identity).unwrap().unwrap();
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
fn waits_for_the_open_successor_after_losing_the_selected_minute() {
    // Goal: losing a selected minute cannot seal its still-open successor.
    // Method: overwrite the minute before the worker reads it, then finish and inspect its seal.
    let root = Directory::new();
    let store = Store::open(&root.0).unwrap();
    let mut registered = registered(store.recording());
    registered.collector_pid = std::process::id();
    let mut ring = Ring::new(&root, &registered.identity, 2);
    registered.ring_name = ring.0.name().to_owned();
    ring.publish(0);
    ring.publish(60);
    ring.publish(61);
    let selected = WindowIdentity {
        run: registered.identity,
        window: 0,
    };
    let mut state = RunState::default();
    let writers = Writers {
        encoder: Encoder,
        derive: &Underived,
    };

    assert!(matches!(
        persist_one(&store, &writers, &registered, false, &mut state).unwrap(),
        Progress::Waiting
    ));
    ring.0.finish();
    assert!(matches!(
        persist_one(&store, &writers, &registered, false, &mut state).unwrap(),
        Progress::Sealed
    ));

    assert!(store.saved(&selected).unwrap().is_none());
    let saved = store.saved_newest(&registered.identity).unwrap().unwrap();
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
fn carries_event_loss_only_with_the_minute_that_copied_its_reading() {
    // Goal: lost event records stay with their saved tick and are not counted twice.
    // Method: give the first tick an event, forge a copy of the finished ring whose header names
    // a later event in the one-event log, as a writer before the event room guard left it while
    // the ring still held that tick, then seal both sides of a minute boundary from the copy.
    let root = Directory::new();
    let store = Store::open(&root.0).unwrap();
    let mut registered = registered(store.recording());
    let mut ring = Ring::new(&root, &registered.identity, 3);
    ring.publish_events(
        0,
        &[Event::Start {
            pid: 41,
            coalition: None,
        }],
    );
    ring.publish(1);
    ring.publish(60);
    ring.0.finish();
    let reader = RingReader::open_generation(ring.0.name()).unwrap();
    let forged = WritableSource::new(
        ring.0.name(),
        reader.mapped_size_bytes(),
        ForgedWord {
            watched: HEADER_EVENT_NEWEST,
            expected: 1,
            index: HEADER_EVENT_NEWEST,
            value: 2,
        },
    );
    registered.ring_name.clone_from(&forged.name);
    let writers = Writers {
        encoder: Encoder,
        derive: &Underived,
    };
    let mut state = RunState::default();

    assert!(matches!(
        persist_one(&store, &writers, &registered, false, &mut state).unwrap(),
        Progress::Sealed
    ));
    let first = store.saved_newest(&registered.identity).unwrap().unwrap();
    assert!(matches!(
        persist_one(&store, &writers, &registered, false, &mut state).unwrap(),
        Progress::Sealed
    ));
    let next = store.saved_newest(&registered.identity).unwrap().unwrap();

    assert_eq!(first.identity.window, 0);
    assert_eq!(first.losses.events, 1);
    assert_eq!(next.identity.window, 1);
    assert_eq!(next.losses, Losses::default());
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
    let durable = crate::event_recovery::read_page(&root.0, &registered.identity, 1, 1).unwrap();
    assert_eq!(durable.rows.height(), 0);
    assert_eq!(
        durable.losses,
        Losses {
            unknown: true,
            ..Losses::default()
        }
    );
    assert!(store.saved_newest(&registered.identity).unwrap().is_none());
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
