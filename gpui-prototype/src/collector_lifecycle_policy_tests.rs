use super::*;

fn existing(publication: u64) -> Source {
    Source {
        recording_route: true,
        recording: RecordingStatus::Ready,
        producer_alive: true,
        observation: Some(Observation {
            pid: 42,
            run: "00000000000000000000000000000001".into(),
            boot: "00000000000000000000000000000002".into(),
            publication,
        }),
    }
}

#[test]
fn advancing_existing_collector_is_attached_without_spawning() {
    // Goal: ordinary launch can share a healthy producer without taking shutdown ownership.
    // Method: observe two publications from one PID and recording identity.
    let mut policy = Policy::new(Mode::Auto);
    assert_eq!(policy.step(Duration::ZERO, &existing(1)), Action::None);

    let action = policy.step(Duration::from_secs(1), &existing(2));

    assert_eq!(action, Action::None);
    assert_eq!(policy.state, State::Attached);
    assert_eq!(policy.owned_pid, None);
}

#[test]
fn a_live_stalled_pid_fails_without_a_competing_spawn() {
    // Goal: a deadline cannot launch a second collector while its discovered owner lives.
    // Method: hold its publication fixed through the exact discovery deadline.
    let mut policy = Policy::new(Mode::Auto);
    policy.step(Duration::ZERO, &existing(1));

    let action = policy.step(DISCOVERY_LIMIT, &existing(1));

    assert_eq!(action, Action::None);
    assert_eq!(policy.state, State::Failed);
    assert!(policy.can_retry());
}

#[test]
fn missing_default_recording_requests_one_owned_spawn() {
    // Goal: first launch produces its own readings without requiring the Swift app.
    // Method: report the absent default recording on an actual readings route.
    let mut policy = Policy::new(Mode::Auto);
    let source = Source {
        recording_route: true,
        recording: RecordingStatus::Missing,
        ..Source::default()
    };

    let first = policy.step(Duration::ZERO, &source);
    let second = policy.step(Duration::from_millis(250), &source);

    assert_eq!(first, Action::Spawn);
    assert_eq!(second, Action::None);
}

#[test]
fn initial_computer_page_defers_auto_discovery_and_collection() {
    // Goal: current machine inventory remains independent of recording availability.
    // Method: leave Auto on Computer for longer than every startup bound.
    let mut policy = Policy::new(Mode::Auto);

    let action = policy.step(Duration::from_mins(15), &Source::default());

    assert_eq!(action, Action::None);
    assert_eq!(policy.state, State::Discovering);
    assert_eq!(policy.since, None);
}

#[test]
fn explicit_collection_starts_even_on_the_computer_page() {
    // Goal: an explicit start command retains its authority on every launch route.
    // Method: request explicit collection without a recording query.
    let mut policy = Policy::new(Mode::Explicit);

    let action = policy.step(Duration::ZERO, &Source::default());

    assert_eq!(action, Action::Spawn);
}

#[test]
fn attached_collection_survives_navigation_to_computer() {
    // Goal: navigation cannot turn a shared producer into an app-owned shutdown target.
    // Method: attach using two publications, then stop observing recording data.
    let mut policy = Policy::new(Mode::Auto);
    policy.step(Duration::ZERO, &existing(1));
    policy.step(Duration::from_secs(1), &existing(2));

    let mut source = existing(3);
    source.recording_route = false;
    let action = policy.step(Duration::from_secs(2), &source);

    assert_eq!(action, Action::None);
    assert_eq!(policy.state, State::Attached);
}

#[test]
fn retrying_a_stalled_shared_collector_never_competes_with_its_living_producer() {
    // Goal: explicit recovery still discovers a living producer before considering a spawn.
    // Method: stall after attachment, retry, and keep that PID alive through rediscovery.
    let mut policy = Policy::new(Mode::Auto);
    policy.step(Duration::ZERO, &existing(1));
    policy.step(Duration::from_secs(1), &existing(2));
    policy.step(DISCOVERY_LIMIT + Duration::from_secs(1), &existing(2));

    policy.retry();
    let first = policy.step(Duration::from_secs(22), &existing(2));
    let expired = policy.step(Duration::from_secs(42), &existing(2));

    assert_eq!(first, Action::None);
    assert_eq!(expired, Action::None);
    assert_eq!(policy.state, State::Failed);
    assert_eq!(policy.owned_pid, None);
    assert!(policy.can_retry());
}

#[test]
fn retrying_a_dead_shared_collector_can_resume_owned_recording() {
    // Goal: a stopped shared source becomes recoverable only after an explicit request.
    // Method: lose the producer after attachment, then retry discovery of the ready store.
    let mut policy = Policy::new(Mode::Auto);
    policy.step(Duration::ZERO, &existing(1));
    policy.step(Duration::from_secs(1), &existing(2));
    let stopped = Source {
        recording_route: true,
        recording: RecordingStatus::Ready,
        ..Source::default()
    };
    let lost = policy.step(Duration::from_secs(2), &stopped);
    assert_eq!(lost, Action::None);
    assert!(policy.can_retry());

    policy.retry();
    let resumed = policy.step(Duration::from_secs(3), &stopped);

    assert_eq!(resumed, Action::Spawn);
    assert_eq!(policy.state, State::Starting);
}

#[test]
fn explicit_attachment_stays_read_only_after_its_producer_stops() {
    // Goal: inspecting an explicit path never grants recording-start authority.
    // Method: lose an Attach source, attempt retry, and observe no ownership or spawn.
    let mut policy = Policy::new(Mode::Attach);
    policy.step(DISCOVERY_LIMIT, &Source::default());

    policy.retry();
    let action = policy.step(DISCOVERY_LIMIT * 2, &Source::default());

    assert_eq!(action, Action::None);
    assert_eq!(policy.state, State::Attached);
    assert!(!policy.can_retry());
    assert!(!policy.needs_health(true));
}

#[test]
fn startup_accepts_only_the_owned_producer() {
    // Goal: another producer's publications cannot confirm the child this app launched.
    // Method: supply advancing publications from PID42 while the owned child is PID99.
    let mut policy = Policy::new(Mode::Explicit);
    policy.launched(99, Duration::ZERO);
    policy.step(Duration::ZERO, &existing(1));
    policy.step(Duration::from_secs(1), &existing(2));

    let action = policy.step(STARTUP_LIMIT, &existing(3));

    assert_eq!(action, Action::Terminate);
    assert_eq!(policy.state, State::Stopping);
    assert!(policy.failure.is_some());
}

#[test]
fn publication_advancement_from_a_new_run_does_not_confirm_attachment() {
    // Goal: startup requires two observations from the same producer generation.
    // Method: change the run identity while increasing the publication counter.
    let mut policy = Policy::new(Mode::Auto);
    policy.step(Duration::ZERO, &existing(1));
    let mut changed = existing(2);
    changed.observation.as_mut().unwrap().run = "00000000000000000000000000000003".into();

    let action = policy.step(Duration::from_secs(1), &changed);

    assert_eq!(action, Action::None);
    assert_eq!(policy.state, State::Discovering);
}

#[test]
fn stopping_an_attached_collector_never_requests_a_signal() {
    // Goal: quitting an attached app leaves another app's recording owner alone.
    // Method: stop a session whose policy grants no child ownership.
    let mut policy = Policy::new(Mode::Attach);

    let action = policy.stop(Duration::ZERO);

    assert_eq!(action, Action::None);
    assert_eq!(policy.state, State::Stopped);
}

#[test]
fn repeated_stop_requests_do_not_extend_the_drain_deadline() {
    // Goal: repeated quit commands preserve a single absolute shutdown bound.
    // Method: repeat stop just before the original125-second deadline, then cross it.
    let mut policy = Policy::new(Mode::Explicit);
    policy.launched(99, Duration::ZERO);
    assert_eq!(policy.stop(Duration::ZERO), Action::Terminate);
    assert_eq!(policy.stop(Duration::from_secs(124)), Action::None);

    let action = policy.step(SHUTDOWN_LIMIT, &Source::default());

    assert_eq!(action, Action::Kill);
    assert_eq!(policy.state, State::Killing);
    assert!(policy.failure.is_some());
}

#[test]
fn unreaped_child_after_kill_reaches_a_reported_terminal_bound() {
    // Goal: quitting cannot wait forever for a child that survives every shutdown signal.
    // Method: leave ownership unreaped through the drain and kill deadlines.
    let mut policy = Policy::new(Mode::Explicit);
    policy.launched(99, Duration::ZERO);
    policy.stop(Duration::ZERO);
    policy.step(SHUTDOWN_LIMIT, &Source::default());

    let action = policy.step(SHUTDOWN_LIMIT + KILL_LIMIT, &Source::default());

    assert_eq!(action, Action::None);
    assert_eq!(policy.state, State::Stopped);
    assert!(policy.failure.is_some());
    assert!(!policy.can_retry());
}

#[test]
fn failed_collection_waits_for_an_explicit_retry() {
    // Goal: launch failures do not create silent repeated spawning loops.
    // Method: fail before child ownership and step again before requesting retry.
    let mut policy = Policy::new(Mode::Explicit);
    policy.fail("launch refused");
    assert_eq!(
        policy.step(Duration::from_secs(1), &Source::default()),
        Action::None
    );

    policy.retry();
    let action = policy.step(Duration::from_secs(2), &Source::default());

    assert_eq!(action, Action::Spawn);
    assert_eq!(policy.failure, None);
}

#[test]
fn nonzero_collector_exit_during_shutdown_is_reported_as_failure() {
    // Goal: a failed final persistence drain cannot be reported as successful shutdown.
    // Method: observe an unsuccessful child exit after requesting its orderly stop.
    let mut policy = Policy::new(Mode::Explicit);
    policy.launched(99, Duration::ZERO);
    policy.stop(Duration::ZERO);

    policy.exited("Collector exited with status 1", false);

    assert_eq!(policy.state, State::Failed);
    assert!(policy.failure.is_some());
    assert!(policy.can_retry());
}

#[test]
fn startup_deadline_rejects_late_advancing_publications() {
    // Goal: late progress cannot silently extend the absolute owned startup deadline.
    // Method: supply the confirming publication exactly at the20-second limit.
    let mut policy = Policy::new(Mode::Explicit);
    policy.launched(42, Duration::ZERO);
    policy.step(Duration::ZERO, &existing(1));

    let action = policy.step(STARTUP_LIMIT, &existing(2));

    assert_eq!(action, Action::Terminate);
    assert_eq!(policy.state, State::Stopping);
}

#[test]
fn discovery_deadline_rejects_late_advancing_publications() {
    // Goal: delayed progress from an alive producer cannot reset the discovery deadline.
    // Method: supply its confirming publication exactly at the20-second limit.
    let mut policy = Policy::new(Mode::Auto);
    policy.step(Duration::ZERO, &existing(1));

    let action = policy.step(DISCOVERY_LIMIT, &existing(2));

    assert_eq!(action, Action::None);
    assert_eq!(policy.state, State::Failed);
}

#[test]
fn owned_startup_deadline_survives_computer_navigation() {
    // Goal: Computer cannot suspend the absolute deadline of an already launched child.
    // Method: launch, observe one publication, then leave readings through the deadline.
    let mut policy = Policy::new(Mode::Explicit);
    policy.launched(42, Duration::ZERO);
    policy.step(Duration::ZERO, &existing(1));

    let action = policy.step(STARTUP_LIMIT, &Source::default());

    assert_eq!(action, Action::Terminate);
    assert_eq!(policy.state, State::Stopping);
}

#[test]
fn discovery_deadline_survives_computer_navigation() {
    // Goal: discovery started on a readings page has a bound on every later route.
    // Method: observe a live producer once and navigate to Computer until the deadline.
    let mut policy = Policy::new(Mode::Auto);
    policy.step(Duration::ZERO, &existing(1));

    let action = policy.step(DISCOVERY_LIMIT, &Source::default());

    assert_eq!(action, Action::None);
    assert_eq!(policy.state, State::Failed);
    assert_eq!(policy.owned_pid, None);
}
