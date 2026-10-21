use super::*;

/// A build no code of this repository fingerprints to, standing for another checkout's.
const OTHER_BUILD: u64 = 7;

fn existing(publication: u64) -> Source {
    Source {
        recording_route: true,
        recording: RecordingStatus::Ready,
        producer_alive: true,
        build: Build::Expected,
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
    // Goal: first launch produces its own readings without requiring another client.
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
    // Method: report a recording no collector holds while no readings route is open.
    let mut policy = Policy::new(Mode::Explicit);
    let source = Source {
        recording_route: false,
        recording: RecordingStatus::Ready,
        ..Source::default()
    };

    let action = policy.step(Duration::ZERO, &source);

    assert_eq!(action, Action::Spawn);
}

#[test]
fn explicit_collection_looks_for_a_collector_on_every_route() {
    // Goal: an explicit start learns of a running collector before it could spawn another.
    // Method: ask for health on Computer before the first discovery step.
    let policy = Policy::new(Mode::Explicit);

    assert!(policy.needs_health(false));
}

#[test]
fn explicit_collection_attaches_to_an_advancing_collector_without_spawning() {
    // Goal: a second start command shares the collector already recording, never a duplicate.
    // Method: observe two publications of one producer before any spawn.
    let mut policy = Policy::new(Mode::Explicit);
    assert_eq!(policy.step(Duration::ZERO, &existing(1)), Action::None);

    let action = policy.step(Duration::from_secs(1), &existing(2));

    assert_eq!(action, Action::None);
    assert_eq!(policy.state, State::Attached);
    assert_eq!(policy.owned_pid, None);
}

#[test]
fn explicit_collection_offers_recovery_when_its_shared_collector_stops() {
    // Goal: a start command attached to another app's collector can resume recording itself.
    // Method: attach, then lose the producer while the recording stays ready.
    let mut policy = Policy::new(Mode::Explicit);
    policy.step(Duration::ZERO, &existing(1));
    policy.step(Duration::from_secs(1), &existing(2));
    let stopped = Source {
        recording: RecordingStatus::Ready,
        ..Source::default()
    };

    let action = policy.step(Duration::from_secs(2), &stopped);

    assert_eq!(action, Action::None);
    assert_eq!(policy.state, State::Failed);
    assert!(policy.can_retry());
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
    let source = Source {
        recording: RecordingStatus::Ready,
        ..Source::default()
    };
    let action = policy.step(Duration::from_secs(2), &source);

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

#[test]
fn a_collector_built_from_other_code_fails_discovery_without_spawning() {
    // Goal: the app never reads a ring its own code may misread, nor competes with its writer.
    // Method: report a holder announcing another build on the first discovery step.
    let mut policy = Policy::new(Mode::Auto);
    let source = Source {
        recording_route: true,
        recording: RecordingStatus::Ready,
        build: Build::Other(OTHER_BUILD),
        ..Source::default()
    };

    let action = policy.step(Duration::ZERO, &source);

    assert_eq!(action, Action::None);
    assert_eq!(policy.state, State::Failed);
    assert_eq!(policy.failure, Some(other_build_failure(OTHER_BUILD)));
    assert!(policy.can_retry());
}

#[test]
fn a_collector_announcing_another_format_fails_discovery_without_spawning() {
    // Goal: the app never reads a ring of a version whose announcement it cannot read.
    // Method: report a holder announcing another format on the first discovery step.
    let mut policy = Policy::new(Mode::Auto);
    let source = Source {
        recording_route: true,
        build: Build::Unrecognized,
        ..Source::default()
    };

    let action = policy.step(Duration::ZERO, &source);

    assert_eq!(action, Action::None);
    assert_eq!(policy.state, State::Failed);
    assert_eq!(policy.failure.as_deref(), Some(UNRECOGNIZED_FAILURE));
}

#[test]
fn another_builds_failure_names_both_builds() {
    // Goal: the user can tell which code built each side.
    // Method: look for both fingerprints in the failure for another build.
    let failure = other_build_failure(OTHER_BUILD);

    assert!(failure.contains(&format!("{OTHER_BUILD:016x}")));
    assert!(failure.contains(&format!("{:016x}", collector::recording::BUILD)));
}

#[test]
fn the_owned_build_failure_names_both_builds_and_the_rebuild() {
    // Goal: the user can tell a stale collector executable from another app's collector.
    // Method: look for both fingerprints and the remedy in the failure for an owned child.
    let failure = owned_build_failure(OTHER_BUILD);

    assert!(failure.contains(&format!("{OTHER_BUILD:016x}")));
    assert!(failure.contains(&format!("{:016x}", collector::recording::BUILD)));
    assert!(failure.contains("Rebuild"));
}

#[test]
fn an_unannounced_holder_fails_discovery_at_its_deadline() {
    // Goal: a collector that never announces itself fails discovery, and never meets a spawn.
    // Method: hold the recording without an announcement through the discovery deadline.
    let mut policy = Policy::new(Mode::Auto);
    let source = Source {
        recording_route: true,
        build: Build::Unannounced,
        ..Source::default()
    };
    policy.step(Duration::ZERO, &source);

    let action = policy.step(DISCOVERY_LIMIT, &source);

    assert_eq!(action, Action::None);
    assert_eq!(policy.state, State::Failed);
    assert_eq!(policy.failure.as_deref(), Some(UNANNOUNCED_FAILURE));
}

#[test]
fn a_finishing_holder_is_awaited_past_the_discovery_deadline() {
    // Goal: a launch soon after a quit waits while the last run's collector saves its readings,
    // however long that takes within its own deadline, and then starts collecting.
    // Method: report a finishing holder through the discovery deadline, then a ready recording.
    let mut policy = Policy::new(Mode::Auto);
    let finishing = Source {
        recording_route: true,
        recording: RecordingStatus::Finishing,
        ..Source::default()
    };
    let ready = Source {
        recording_route: true,
        recording: RecordingStatus::Ready,
        ..Source::default()
    };
    policy.step(Duration::ZERO, &finishing);

    let waiting = policy.step(DISCOVERY_LIMIT, &finishing);
    let starting = policy.step(DISCOVERY_LIMIT + Duration::from_secs(1), &ready);

    assert_eq!(waiting, Action::None);
    assert_eq!(starting, Action::Spawn);
}

#[test]
fn a_finishing_holder_fails_discovery_at_the_collectors_stop_deadline() {
    // Goal: a collector that never lets go of the recording fails discovery once its own
    // deadline has passed, and never meets a spawn.
    // Method: report a finishing holder just before and then at that deadline.
    let mut policy = Policy::new(Mode::Auto);
    let finishing = Source {
        recording_route: true,
        recording: RecordingStatus::Finishing,
        ..Source::default()
    };
    policy.step(Duration::ZERO, &finishing);

    policy.step(
        FINISHING_LIMIT.checked_sub(Duration::from_secs(1)).unwrap(),
        &finishing,
    );
    let before = policy.state;
    let action = policy.step(FINISHING_LIMIT, &finishing);

    assert_eq!(before, State::Discovering);
    assert_eq!(action, Action::None);
    assert_eq!(policy.state, State::Failed);
    assert!(policy.can_retry());
}

#[test]
fn an_owned_collector_built_from_other_code_is_stopped() {
    // Goal: a stale collector executable beside this app fails at once, not at the deadline.
    // Method: report the owned child announcing another build on its first startup step.
    let mut policy = Policy::new(Mode::Explicit);
    policy.launched(42, Duration::ZERO);
    let source = Source {
        build: Build::Other(OTHER_BUILD),
        ..Source::default()
    };

    let action = policy.step(Duration::from_secs(1), &source);

    assert_eq!(action, Action::Terminate);
    assert_eq!(policy.state, State::Stopping);
    assert_eq!(policy.failure, Some(owned_build_failure(OTHER_BUILD)));
}

#[test]
fn an_owned_collector_announcing_another_format_is_stopped() {
    // Goal: a collector executable of another version beside this app fails at once.
    // Method: report a holder announcing another format on the child's first startup step.
    let mut policy = Policy::new(Mode::Explicit);
    policy.launched(42, Duration::ZERO);
    let source = Source {
        build: Build::Unrecognized,
        ..Source::default()
    };

    let action = policy.step(Duration::from_secs(1), &source);

    assert_eq!(action, Action::Terminate);
    assert_eq!(policy.state, State::Stopping);
    assert_eq!(policy.failure.as_deref(), Some(UNRECOGNIZED_FAILURE));
}
