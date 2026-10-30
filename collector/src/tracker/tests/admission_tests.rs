use super as tests;
use super::super::*;

#[test]
fn skips_payload_for_an_already_exited_process_when_its_start_fills_the_tick() {
    // Goal: a rejected registration-time Exit retires lifecycle state without acquisition.
    // Method: observe an unreaped owned child with room for its Start alone.
    let mut tracker = tests::tracker_with_event_capacity(1);
    let mut child = tests::ExitChild(processes::child_that_has_exited());
    let pid = child.0.id().cast_signed();
    let before = tracker.counts();
    let attempted = tracker.exit_diagnostics().attempted;

    let allocations = crate::recording::discovery::tests::allocator_calls_in(|| {
        tracker.observe(&[pid], &mut Meter::default()).unwrap();
    });
    let added = tracker.counts().since(&before);
    let retained: Vec<_> = tracker.drain_observations().collect();
    child.0.wait().unwrap();

    eprintln!(
        "saturated AlreadyExited admitted={} dropped={} exit_attempted={} \
         diagnostics={:?} rust_heap_calls={allocations}",
        retained.len(),
        added.dropped_exits,
        tracker.exit_diagnostics().attempted - attempted,
        tracker.exit_diagnostics(),
    );
    assert_eq!(1, retained.len());
    assert!(matches!(retained[0].event, Event::Start { pid: found, .. } if found == pid));
    assert_eq!(1, added.dropped_exits);
    assert_eq!(0, added.dropped_starts);
    assert_eq!(0, added.dropped_execs);
    assert!(tracker.known[usize::try_from(pid).unwrap()].exited);
    assert!(!tracker.known[usize::try_from(pid).unwrap()].watch.active);
    assert_eq!(0, allocations);
    assert_eq!(attempted, tracker.exit_diagnostics().attempted);
}

#[test]
fn skips_payload_for_a_watched_exit_when_its_start_fills_the_tick() {
    // Goal: a rejected native exit notice retires the child's watch without final acquisition.
    // Method: retain a held child's Start, release it, and follow its installed native watch.
    let mut tracker = tests::tracker_with_event_capacity(1);
    let mut child = tests::ExitChild(tests::spawn_held("/bin/cat", &[]));
    let pid = child.0.id().cast_signed();
    tracker.observe(&[pid], &mut Meter::default()).unwrap();
    let attempted = tracker.exit_diagnostics().attempted;
    let dropped = tracker.counts().dropped_exits;
    tests::release(&mut child.0);

    let allocations = crate::recording::discovery::tests::allocator_calls_in(|| {
        follow_until_exited(&mut tracker, pid);
    });
    let retained: Vec<_> = tracker.drain_observations().collect();
    child.0.wait().unwrap();

    eprintln!(
        "saturated OnExit admitted={} dropped={} exit_attempted={} \
         diagnostics={:?} rust_heap_calls={allocations}",
        retained.len(),
        tracker.counts().dropped_exits - dropped,
        tracker.exit_diagnostics().attempted - attempted,
        tracker.exit_diagnostics(),
    );
    assert_eq!(1, retained.len());
    assert!(matches!(retained[0].event, Event::Start { pid: found, .. } if found == pid));
    assert_eq!(1, tracker.counts().dropped_exits - dropped);
    assert!(!tracker.known[usize::try_from(pid).unwrap()].watch.active);
    assert_eq!(0, allocations);
    assert_eq!(attempted, tracker.exit_diagnostics().attempted);
}

#[test]
fn preserves_exit_diagnostics_when_a_later_exit_is_rejected() {
    // Goal: rejected Exits change neither acquisition totals nor the last actual failure.
    // Method: acquire an unavailable Exit, then fill the store before a native successful Exit.
    let mut tracker = tests::tracker_with_event_capacity(2);
    let gone = tests::unlisted_pid(&tests::current_listing());
    tracker.observe(&[gone], &mut Meter::default()).unwrap();
    tracker.drain_observations().for_each(drop);
    let before = tracker.exit_diagnostics();
    let counts_before = (before.attempted, before.verified, before.unavailable);
    let failure = before.last_failure.as_ref().unwrap();
    let failure_before = (
        failure.pid,
        failure.acquisition.creation_before,
        failure.acquisition.creation_after,
        failure.acquisition.exited,
    );
    let mut child = tests::ExitChild(processes::child_that_has_exited());
    let pid = child.0.id().cast_signed();
    let own = std::process::id().cast_signed();
    let dropped = tracker.counts().dropped_exits;

    tracker.observe(&[own, pid], &mut Meter::default()).unwrap();
    child.0.wait().unwrap();

    let after = tracker.exit_diagnostics();
    assert_eq!(
        counts_before,
        (after.attempted, after.verified, after.unavailable)
    );
    let failure = after.last_failure.as_ref().unwrap();
    assert_eq!(
        failure_before,
        (
            failure.pid,
            failure.acquisition.creation_before,
            failure.acquisition.creation_after,
            failure.acquisition.exited,
        )
    );
    assert_eq!(1, tracker.counts().dropped_exits - dropped);
    assert_eq!(2, tracker.drain_observations().count());
}

#[test]
fn admits_no_more_events_than_the_ring_s_room_below_the_tick_s_capacity() {
    // Goal: a tick admits no more events than the ring's room although its list has room for
    // more, and counts the rest as dropped by kind.
    // Method: give a tracker with capacity for two a room of one, and observe an unreaped
    // owned child, whose start and exit the list could both hold.
    let mut tracker = tests::tracker_with_event_capacity(2);
    let mut child = tests::ExitChild(processes::child_that_has_exited());
    let pid = child.0.id().cast_signed();
    let before = tracker.counts();

    tracker.set_event_room(1);
    tracker.observe(&[pid], &mut Meter::default()).unwrap();
    let added = tracker.counts().since(&before);
    let retained: Vec<_> = tracker.drain_observations().collect();
    child.0.wait().unwrap();

    assert_eq!(1, retained.len());
    assert!(matches!(retained[0].event, Event::Start { pid: found, .. } if found == pid));
    assert_eq!(1, added.dropped_exits);
}

#[test]
fn admits_no_more_events_than_the_tick_s_capacity_below_the_ring_s_room() {
    // Goal: a ring's room larger than the tick's capacity leaves the capacity as the bound.
    // Method: give a tracker with capacity for one a room of a million, and observe an
    // unreaped owned child, whose start fills the tick.
    let mut tracker = tests::tracker_with_event_capacity(1);
    let mut child = tests::ExitChild(processes::child_that_has_exited());
    let pid = child.0.id().cast_signed();
    let before = tracker.counts();

    tracker.set_event_room(1_000_000);
    tracker.observe(&[pid], &mut Meter::default()).unwrap();
    let added = tracker.counts().since(&before);
    let retained: Vec<_> = tracker.drain_observations().collect();
    child.0.wait().unwrap();

    assert_eq!(1, retained.len());
    assert!(matches!(retained[0].event, Event::Start { pid: found, .. } if found == pid));
    assert_eq!(1, added.dropped_exits);
}

#[test]
fn drops_the_newest_events_past_a_room_given_after_they_were_admitted() {
    // Goal: events admitted before the ring gave its room, as the tracker's start admits the
    // exit of a process that ends before its watch, stay within that room, and the rest count
    // as dropped by kind.
    // Method: let a tracker with capacity for two admit an unreaped owned child's start and
    // exit, then give it a room of one.
    let mut tracker = tests::tracker_with_event_capacity(2);
    let mut child = tests::ExitChild(processes::child_that_has_exited());
    let pid = child.0.id().cast_signed();
    tracker.observe(&[pid], &mut Meter::default()).unwrap();
    let before = tracker.counts();

    tracker.set_event_room(1);
    let added = tracker.counts().since(&before);
    let retained: Vec<_> = tracker.drain_observations().collect();
    child.0.wait().unwrap();

    assert_eq!(1, retained.len());
    assert!(matches!(retained[0].event, Event::Start { pid: found, .. } if found == pid));
    assert_eq!(1, added.dropped_exits);
}

fn follow_until_exited(tracker: &mut Tracker, pid: i32) {
    let index = usize::try_from(pid).unwrap();
    for _attempt in 0..tests::STEPS_MAX {
        tests::follow_for(tracker, &mut Meter::default(), tests::STEP_MS);
        if tracker.known[index].exited {
            return;
        }
    }

    panic!("no exit for owned pid={pid}");
}

#[test]
fn counts_a_rejected_acquisition_without_invoking_it() {
    // Goal: a full event store skips acquisition, including its possible error.
    // Method: offer a counted failing acquisition while the only slot is occupied.
    let mut events = tests::full_event_list();
    let mut scratch = EventObservation::EMPTY;
    let mut counts = Counts::default();
    let mut acquisitions = 0;
    let room = events.capacity();

    let recorded = record(
        &mut events,
        room,
        &mut counts,
        &mut scratch,
        EventKind::Start,
        |_observation| {
            acquisitions += 1;
            Err(io::ErrorKind::Interrupted.into())
        },
    );

    assert!(recorded.is_ok());
    assert_eq!(0, acquisitions);
    assert_eq!(tests::full_event_list(), events);
    assert_eq!(
        Counts {
            dropped_starts: 1,
            ..Counts::default()
        },
        counts
    );
}

#[test]
fn keeps_a_partial_failed_acquisition_out_of_retained_observations() {
    // Goal: an acquisition error exposes no partial record and counts no overflow.
    // Method: fill scratch partially, then return an error with one retained slot still free.
    let mut events = Vec::with_capacity(2);
    events.push(Event::Exec { pid: 1 }.into());
    let mut scratch = EventObservation::EMPTY;
    let mut counts = Counts::default();
    let room = events.capacity();

    let recorded = record(
        &mut events,
        room,
        &mut counts,
        &mut scratch,
        EventKind::Start,
        |observation| {
            observation.event = Event::Start {
                pid: 2,
                coalition: None,
            };
            Err(io::ErrorKind::Interrupted.into())
        },
    );

    assert_eq!(io::ErrorKind::Interrupted, recorded.unwrap_err().kind());
    assert_eq!(tests::full_event_list(), events);
    assert_eq!(Counts::default(), counts);
    assert_eq!(2, events.capacity());
}

#[test]
fn retains_the_observation_prefix_and_counts_each_rejected_kind_once() {
    // Goal: mixed overflow preserves order and lifecycle while counting each rejected notice.
    // Method: admit Start/Exec, reject another Exec/Exit/Start, then inspect the retained prefix.
    let mut tracker = tests::tracker_with_event_capacity(2);
    let first = tests::ExitChild(tests::spawn_held("/bin/cat", &[]));
    let first_pid = first.0.id().cast_signed();
    tracker
        .observe(&[first_pid], &mut Meter::default())
        .unwrap();
    let second = tests::ExitChild(tests::spawn_held("/bin/cat", &[]));
    let second_pid = second.0.id().cast_signed();
    let notices = [
        tests::process_event(usize::try_from(first_pid).unwrap(), NOTE_EXEC | NOTE_EXIT),
        tests::process_event(usize::try_from(second_pid).unwrap(), NOTE_EXEC),
    ];
    let before = tracker.counts();

    let allocations = crate::recording::discovery::tests::allocator_calls_in(|| {
        let wake = follow_until_handle(
            &notices,
            &mut tracker.events,
            tracker.event_room,
            &mut tracker.event_scratch,
            &mut tracker.counts,
            &mut tracker.exits,
        )
        .unwrap();
        assert_eq!(Wake::default(), wake);
        tracker.read_exits(&mut Meter::default()).unwrap();
        tracker
            .observe(&[second_pid], &mut Meter::default())
            .unwrap();
    });
    let added = tracker.counts().since(&before);
    let retained: Vec<_> = tracker.drain_observations().collect();

    assert_eq!(2, retained.len());
    assert!(matches!(retained[0].event, Event::Start { pid, .. } if pid == first_pid));
    assert_eq!(Event::Exec { pid: first_pid }, retained[1].event);
    assert_eq!(first_pid, retained[1].identity.pid);
    assert_eq!(EventCreation::NotApplicable, retained[1].identity.creation);
    assert_eq!(1, added.dropped_starts);
    assert_eq!(1, added.dropped_execs);
    assert_eq!(1, added.dropped_exits);
    assert!(tracker.known[usize::try_from(first_pid).unwrap()].exited);
    assert!(
        !tracker.known[usize::try_from(first_pid).unwrap()]
            .watch
            .active
    );
    assert!(
        tracker.known[usize::try_from(second_pid).unwrap()]
            .watch
            .active
    );
    assert_eq!(0, allocations);
}
