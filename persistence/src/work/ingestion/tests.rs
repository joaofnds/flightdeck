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

#[test]
fn hands_the_trace_kept_tentative_work_as_recorded_and_only_once_kept() {
    // Goal: work whose result may yet be thrown away counts only once its result is kept, as it
    // was recorded, including the work of the threads it enrols.
    // Method: record a raw derivation in a tentative scope and one in an enrolled child, record
    // after the scope ends, then compare the trace before and after keeping.
    let trace = Trace::new(RUN.recording);
    let scope = trace.enter();
    let (tentative_scope, tentative) = tentative();
    let raw = purpose(Purpose::Raw);
    record_publication(RUN, 1, Kind::AppDerivation);
    let inherited = inherit_current();
    std::thread::spawn(move || {
        let scope = inherited.enter();
        record_publication(RUN, 2, Kind::AppDerivation);
        drop(scope);
    })
    .join()
    .unwrap();
    drop(raw);
    drop(tentative_scope);
    record_publication(RUN, 3, Kind::RingRead);
    let unsettled = events(&trace);

    tentative.keep();

    assert_eq!(unsettled.len(), 1);
    let publication = |publication| Key::Publication {
        run: RUN,
        publication,
    };
    let settled: Vec<_> = events(&trace)
        .iter()
        .map(|event| (event.key, event.purpose))
        .collect();
    assert_eq!(
        settled,
        [
            (publication(3), Purpose::Ordinary),
            (publication(1), Purpose::Raw),
            (publication(2), Purpose::Raw),
        ]
    );
    drop(scope);
}

#[test]
fn hands_the_trace_discarded_tentative_work_as_discarded() {
    // Goal: work whose result is thrown away is told apart from the work whose result is kept.
    // Method: record a derivation in a tentative scope and discard it.
    let trace = Trace::new(RUN.recording);
    let scope = trace.enter();
    let (tentative_scope, tentative) = tentative();
    record_publication(RUN, 1, Kind::AppDerivation);
    drop(tentative_scope);

    tentative.discard();

    let captured = events(&trace);
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].purpose, Purpose::Discarded);
    drop(scope);
}

#[test]
fn hands_the_trace_tentative_work_dropped_unkept_as_discarded() {
    // Goal: a path that never settles tentative work, such as an error, cannot count it as kept.
    // Method: record a derivation in a tentative scope and drop the tentative work unsettled.
    let trace = Trace::new(RUN.recording);
    let scope = trace.enter();
    let (tentative_scope, tentative) = tentative();
    record_publication(RUN, 1, Kind::AppDerivation);
    drop(tentative_scope);

    drop(tentative);

    let captured = events(&trace);
    assert_eq!(captured.len(), 1);
    assert_eq!(captured[0].purpose, Purpose::Discarded);
    drop(scope);
}

#[test]
fn refuses_a_truncated_observation_when_tentative_work_exhausts_event_capacity() {
    // Goal: tentative work cannot shorten the event list a work-count invariant is certified from.
    // Method: record one event more than the trace holds in a tentative scope, then keep it.
    let trace = Trace::new(RUN.recording);
    let scope = trace.enter();
    let (tentative_scope, tentative) = tentative();
    for _event in 0..=EVENTS_MAX {
        record_publication(RUN, 1, Kind::RingRead);
    }
    drop(tentative_scope);

    tentative.keep();

    let mut output = Vec::new();
    assert_eq!(
        trace.snapshot(&mut output),
        Err("ingestion trace capacity exhausted")
    );
    drop(scope);
}

#[test]
fn hands_work_kept_inside_tentative_work_to_the_work_around_it() {
    // Goal: tentative work begun inside other tentative work counts only as the outer work
    // settles, so a guess kept inside a load the listing refuses still counts as discarded.
    // Method: record a derivation in a tentative scope nested in another, keep the inner work,
    // then discard the outer work.
    let trace = Trace::new(RUN.recording);
    let scope = trace.enter();
    let (outer_scope, outer) = tentative();
    let (inner_scope, inner) = tentative();
    record_publication(RUN, 1, Kind::AppDerivation);
    drop(inner_scope);
    inner.keep();
    drop(outer_scope);

    outer.discard();

    let purposes: Vec<_> = events(&trace).iter().map(|event| event.purpose).collect();
    assert_eq!(purposes, [Purpose::Discarded]);
    drop(scope);
}

#[test]
fn hands_the_trace_work_discarded_inside_kept_tentative_work_as_discarded() {
    // Goal: work thrown away inside tentative work whose own result is kept stays discarded.
    // Method: record a derivation in a tentative scope and another in a scope nested in it,
    // discard the inner work, then keep the outer work.
    let trace = Trace::new(RUN.recording);
    let scope = trace.enter();
    let (outer_scope, outer) = tentative();
    record_publication(RUN, 1, Kind::AppDerivation);
    let (inner_scope, inner) = tentative();
    record_publication(RUN, 2, Kind::AppDerivation);
    drop(inner_scope);
    inner.discard();
    drop(outer_scope);

    outer.keep();

    let settled: Vec<_> = events(&trace)
        .iter()
        .map(|event| (event.key, event.purpose))
        .collect();
    let publication = |publication| Key::Publication {
        run: RUN,
        publication,
    };
    assert_eq!(
        settled,
        [
            (publication(1), Purpose::Ordinary),
            (publication(2), Purpose::Discarded),
        ]
    );
    drop(scope);
}

#[test]
fn refuses_a_truncated_observation_when_kept_tentative_work_fills_the_rest_of_the_trace() {
    // Goal: tentative work kept into a trace that already holds events can neither grow the trace
    // past its capacity nor shorten the event list a work-count invariant is certified from.
    // Method: record half the trace's capacity directly, keep tentative work that records the
    // other half and one event more, then take a snapshot.
    let trace = Trace::new(RUN.recording);
    let scope = trace.enter();
    for _event in 0..EVENTS_MAX / 2 {
        record_publication(RUN, 1, Kind::RingRead);
    }
    let (tentative_scope, tentative) = tentative();
    for _event in 0..=EVENTS_MAX / 2 {
        record_publication(RUN, 2, Kind::RingRead);
    }
    drop(tentative_scope);

    tentative.keep();

    let mut output = Vec::new();
    assert_eq!(
        trace.snapshot(&mut output),
        Err("ingestion trace capacity exhausted")
    );
    assert_eq!(
        trace.shared.state.lock().unwrap().events.capacity(),
        EVENTS_MAX
    );
    drop(scope);
}
