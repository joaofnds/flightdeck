use super::*;

const RUN: RunIdentity = RunIdentity {
    recording: Identity([1, 2]),
    run: Identity([3, 4]),
    boot: Identity([5, 6]),
};

fn events(trace: &Trace) -> Vec<Event> {
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    events
}

#[test]
fn restores_nested_scopes_and_retains_enrolled_children_after_parent_scope_exit() {
    // Goal: one operation owns enrolled work without leaking into other threads or scopes.
    // Method: nest a second trace and move an inherited first-trace context to a child.
    let trace = Trace::new(RUN.recording);
    let scope = trace.enter();
    let inherited = inherit(Producer::Worker);
    let nested = Trace::new(RUN.recording);
    {
        let nested_scope = nested.enter();
        record_publication(RUN, 1, Kind::RingRead);
        drop(nested_scope);
    }
    record_publication(RUN, 2, Kind::RingRead);
    drop(scope);
    std::thread::spawn(move || {
        let scope = inherited.enter();
        record_publication(RUN, 3, Kind::FrameCapture);
        drop(scope);
        record_publication(RUN, 4, Kind::RingRead);
    })
    .join()
    .unwrap();
    record_publication(RUN, 5, Kind::RingRead);

    assert_eq!(events(&nested).len(), 1);
    let captured = events(&trace);
    assert_eq!(captured.len(), 2);
    assert_eq!(
        captured[0].key,
        Key::Publication {
            run: RUN,
            publication: 2
        }
    );
    assert_eq!(captured[1].producer, Producer::Worker);
    assert_eq!(
        captured[1].key,
        Key::Publication {
            run: RUN,
            publication: 3
        }
    );
    assert!(ACTIVE.with(|active| active.borrow().is_none()));
}

#[test]
fn refuses_a_truncated_observation_when_event_capacity_is_exhausted() {
    // Goal: a shortened event list can never certify a work-count invariant.
    // Method: fill the fixed event storage and record one more actual event.
    let trace = Trace::new(RUN.recording);
    let scope = trace.enter();
    for _event in 0..=EVENTS_MAX {
        record_publication(RUN, 1, Kind::RingRead);
    }
    let mut output = Vec::new();

    assert_eq!(
        trace.snapshot(&mut output),
        Err("ingestion trace capacity exhausted")
    );
    assert!(output.is_empty());
    assert_eq!(
        trace.shared.state.lock().unwrap().events.capacity(),
        EVENTS_MAX
    );
    drop(scope);
}

#[test]
fn restores_source_and_purpose_after_unwinding_a_nested_operation() {
    // Goal: failed demand work cannot relabel later ordinary ingestion.
    // Method: unwind a raw source scope and inspect the event that follows it.
    let trace = Trace::new(RUN.recording);
    let scope = trace.enter();
    let result = std::panic::catch_unwind(|| {
        let _purpose = purpose(Purpose::Raw);
        let _source = source(
            Key::Publication {
                run: RUN,
                publication: 1,
            },
            Some("ticks"),
        );
        record_content(Kind::ContentCapture, 12);
        panic!("test unwind");
    });
    record_publication(RUN, 2, Kind::RingRead);

    assert!(result.is_err());
    let captured = events(&trace);
    assert_eq!(captured[0].purpose, Purpose::Raw);
    assert_eq!(captured[0].table, Some("ticks"));
    assert_eq!(captured[1].purpose, Purpose::Ordinary);
    assert_eq!(captured[1].table, None);
    drop(scope);
}

#[test]
fn exposes_unattributed_work_and_rejects_foreign_recording_subjects() {
    // Goal: incomplete subject coverage is visible without admitting another recording's work.
    // Method: record a collect without a source and a publication of a different recording.
    let trace = Trace::new(RUN.recording);
    let scope = trace.enter();
    let mut foreign = RUN;
    foreign.recording = Identity([9, 10]);

    record_content(Kind::NativeCollect, 0);
    record_publication(foreign, 1, Kind::RingRead);
    std::thread::spawn(|| record_publication(RUN, 2, Kind::RingRead))
        .join()
        .unwrap();

    let captured = events(&trace);
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].kind, Kind::NativeCollect);
    assert_eq!(
        captured[0].key,
        Key::Unattributed {
            recording: RUN.recording
        }
    );
    drop(scope);
}
