//! Rows kept when their source can no longer supply their window, and what happens next.

use super::*;

#[test]
fn rows_kept_with_the_rest_of_their_minute_held_count_that_minute_as_lost() {
    // Goal: the ring overwriting only the start of an interrupted minute keeps its rows, counts the
    // minute's publications as lost once, and cannot stop the worker.
    // Method: stop a seal with its rows saved, publish past the minute's first publication alone,
    // restart, step once to keep the rows and once to save the next minute.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::EventIndexAbsent);
    drop(first);
    recording.publish_minutes(3..8);
    let restarted = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();

    let kept = recording.persist(&restarted, &mut state);
    let rest = recording.persist(&restarted, &mut state);

    assert!(matches!(kept, Progress::Sealed));
    assert!(matches!(rest, Progress::Sealed));
    assert!(!restarted.slot_occupied().unwrap());
    assert_eq!(recording.saved_fragment_event_numbers(false), [1, 2, 3]);
    assert_eq!(recording.saved_manifests()[0].publication_first, 3);
    assert_eq!(recording.saved_manifests()[0].losses.publications, 2);
    assert_eq!(recording.saved_event_rows(), [1]);
}

#[test]
fn rows_after_a_leading_gap_outlive_their_window() {
    // Goal: rows whose index starts after events the ring had already lost keep like any others,
    // since an index may start at its first row.
    // Method: stop a lagging seal with its index written, publish past the whole ring, restart, and
    // step until the slot is free.
    let mut recording = lagging_recording();
    let attempted = InterruptedWindow {
        number: 5,
        publication: 7,
        publications_lost: 4,
        unknown: false,
    };
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal_of(&first, &attempted, Interruption::EventsStaged);
    drop(first);
    recording.publish_minutes(13..25);
    let restarted = Store::open(&recording.root).unwrap();

    let kept = recording.persist(&restarted, &mut RunState::default());

    assert!(matches!(kept, Progress::Sealed));
    assert!(!restarted.slot_occupied().unwrap());
    assert_eq!(recording.saved_fragment_event_numbers(false), [8]);
}

/// Takes a run's turn with another run's ring under its name, which its identity refuses as a
/// rebuilt ring or a changed mode refuses, so the ring may still hold everything.
fn keep_persist_refused(
    recording: &Recording,
    other: &OtherRun,
    store: &Store,
    state: &mut RunState,
) -> Progress {
    let derive = SealedApps::default();
    let writers = Writers {
        encoder: Encoder,
        derive: &derive,
    };
    let refused = RegisteredRun {
        ring_name: other.writer.name().to_owned(),
        ..recording.registered()
    };

    persist_one(store, &writers, &refused, false, state).unwrap()
}

#[test]
fn an_ordinary_attempt_whose_source_refuses_waits_for_it() {
    // Goal: a refused open does not show the source can no longer supply the window, so the attempt
    // keeps its tables and its minute saves whole once the source reads.
    // Method: stop with the tables written, take the run's turn under a refusal and the step after
    // every run's turn, then step a new process with the source back.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let other = recording.another_run();
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::TablesWritten);
    drop(first);
    let refused = Store::open(&recording.root).unwrap();
    let mut state = RunState {
        identity: Some(recording.run),
        ..RunState::default()
    };

    let own = keep_persist_refused(&recording, &other, &refused, &mut state);
    let finished =
        super::super::persist_unowned(&refused, std::slice::from_ref(&state), 0).unwrap();
    drop(refused);
    let restarted = Store::open(&recording.root).unwrap();
    let saved = recording.persist(&restarted, &mut RunState::default());

    assert!(matches!(own, Progress::Unavailable));
    assert!(!finished);
    assert!(matches!(saved, Progress::Sealed));
    assert_eq!(recording.saved_event_rows(), [3]);
    assert_eq!(recording.saved_losses(), [crate::Losses::default()]);
}

#[test]
fn a_terminal_whose_source_refuses_waits_for_it() {
    // Goal: a refused open cannot end a run or count what the source still holds as lost.
    // Method: stop a terminal with its rows saved, take the run's turn under a refusal and the step
    // after every run's turn, then step a new process with the source back.
    let recording = recording_with_withheld_events();
    let other = recording.another_run();
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_terminal(&first, Interruption::EventsStaged);
    drop(first);
    let refused = Store::open(&recording.root).unwrap();
    let mut state = RunState {
        identity: Some(recording.run),
        ..RunState::default()
    };

    let own = keep_persist_refused(&recording, &other, &refused, &mut state);
    let finished =
        super::super::persist_unowned(&refused, std::slice::from_ref(&state), 0).unwrap();
    let held = refused.slot_occupied().unwrap();
    drop(refused);
    let restarted = Store::open(&recording.root).unwrap();
    persist_until_quiet(&recording, &restarted, &mut RunState::default());

    assert!(matches!(own, Progress::Unavailable));
    assert!(!finished);
    assert!(held);
    assert_eq!(recording.saved_terminal_event_numbers(false), [1, 2]);
    assert_eq!(
        recording.saved_terminal().lifecycle,
        Lifecycle::DeadGeneration
    );
}

#[test]
fn a_saved_batch_that_refuses_leaves_the_tables_in_place() {
    // Goal: a refusal of what an attempt saved removes nothing, so what it reports stays to read.
    // Method: stop with the tables written, put a table in the batch's place, remove the source and
    // take the step after every run's turn.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::TablesWritten);
    drop(first);
    let pending = refusal_tests::attempt_directory(&recording.root, "data", "pending");
    let batch = Recording::only_event_batch(&pending);
    std::fs::copy(pending.join("ticks.parquet"), &batch).unwrap();
    std::fs::remove_file(pending.join("events/index")).unwrap();
    recording.lose_source();
    let restarted = Store::open(&recording.root).unwrap();

    let result = super::super::persist_unowned(&restarted, &[], 0);

    assert!(result.is_err());
    assert!(pending.join("ticks.parquet").exists());
}

#[test]
fn tables_with_no_rows_and_no_source_clear_the_slot() {
    // Goal: tables with no proof go once their source cannot supply them, which frees the slot when
    // no row was saved.
    // Method: stop a seal of a minute with no events beside a torn table, remove the source, take
    // the step after every run's turn, and read the slot and the recording.
    let mut recording = Recording::new();
    recording.publish_minutes(0..2);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::TableMissing);
    drop(first);
    recording.lose_source();
    let restarted = Store::open(&recording.root).unwrap();

    let finished = super::super::persist_unowned(&restarted, &[], 0).unwrap();

    assert!(finished);
    assert!(!restarted.slot_occupied().unwrap());
    assert!(recording.saved_windows().is_empty());
}

#[test]
fn rows_kept_after_earlier_losses_leave_them_for_the_next_window_to_count() {
    // Goal: publications an attempt had lost before its rows stay counted once, by the next window
    // to save, since a fragment claims none.
    // Method: stop a lagging seal with its index written, publish past the whole ring, keep the
    // rows, save the next window, and compare its count with the publications between the frontier
    // and where it starts.
    let mut recording = lagging_recording();
    let attempted = InterruptedWindow {
        number: 5,
        publication: 7,
        publications_lost: 4,
        unknown: false,
    };
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal_of(&first, &attempted, Interruption::EventsStaged);
    drop(first);
    recording.publish_minutes(13..25);
    let restarted = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();

    recording.persist(&restarted, &mut state);
    let frontier = recording.frontier(&restarted).unwrap();
    recording.persist(&restarted, &mut state);

    let next = recording.saved_manifests().pop().unwrap();
    assert_eq!(
        next.losses.publications,
        next.publication_first - frontier.publication_end()
    );
}

#[test]
fn rows_kept_carry_the_unknown_extent_their_attempt_held() {
    // Goal: an unknown extent the attempt carried survives in the fragment that replaces it.
    // Method: stop a seal that holds the mark with its rows saved, publish past the whole ring,
    // keep the rows, and read what the fragment inherited.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let attempted = InterruptedWindow {
        number: 0,
        publication: 1,
        publications_lost: 0,
        unknown: true,
    };
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal_of(&first, &attempted, Interruption::EventIndexAbsent);
    drop(first);
    recording.publish_minutes(3..13);
    let restarted = Store::open(&recording.root).unwrap();

    recording.persist(&restarted, &mut RunState::default());

    let window = recording.saved_windows().pop().unwrap();
    let mut file = std::fs::File::open(window.join("fragment-0/metadata")).unwrap();
    let metadata = CanonicalMetadata::read(&mut file).unwrap();
    assert!(metadata.loss.inherited.unknown.is_some());
}

#[test]
fn a_kept_terminal_counts_what_it_did_not_save_as_lost() {
    // Goal: a kept terminal counts the part of its span it did not save, and keeps the publications
    // it inherited as lost.
    // Method: stop a dead writer's terminal with its batches saved and two publications lost before
    // it, remove all but its first batch, remove the source, and read what the terminal counts.
    let capacity = u64::try_from(EVENTS_PAST_TWO_BATCHES).unwrap();
    let rows = u64::try_from(crate::event_observation::BATCH_ROWS_MAX).unwrap();
    let published = capacity + 5;
    let mut recording = Recording::with_event_capacity(capacity);
    recording.publish_past_several_batches(EVENTS_PAST_TWO_BATCHES);
    recording.withhold_publications_past(2);
    let mut state = RunState::default();
    state.losses.publications = 2;
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_terminal_of(&first, &state, Interruption::EventsStaged);
    drop(first);
    let pending = refusal_tests::attempt_directory(&recording.root, "recovery-data", "pending");
    std::fs::remove_file(pending.join(crate::event_recovery::batch_name(2))).unwrap();
    std::fs::remove_file(pending.join(crate::event_recovery::batch_name(1))).unwrap();
    recording.lose_source();
    let restarted = Store::open(&recording.root).unwrap();

    super::super::persist_unowned(&restarted, &[], 0).unwrap();

    let Body::Events(events) = recording.saved_terminal().body else {
        panic!("a terminal holds events");
    };
    assert_eq!(events.recovery.rows, rows);
    assert_eq!(events.recovery.losses.publications, 2);
    assert_eq!(events.recovery.losses.events, published - rows);
}

/// Leaves in `pending` the proof of a process that ended while writing it.
fn keep_short_proof(pending: &Path) {
    use std::io::Write;
    let mut file = private::open_private_file(&pending.join("metadata"), true).unwrap();
    file.write_all(b"short").unwrap();
}

#[test]
fn a_short_proof_goes_with_the_tables_and_the_rows_stay() {
    // Goal: a process that ends while keeping rows leaves work the next one finishes.
    // Method: stop with the tables written and a short proof, remove the source, take the step
    // after every run's turn, and read the rows.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::TablesWritten);
    drop(first);
    keep_short_proof(&refusal_tests::attempt_directory(
        &recording.root,
        "data",
        "pending",
    ));
    recording.lose_source();
    let restarted = Store::open(&recording.root).unwrap();

    let finished = super::super::persist_unowned(&restarted, &[], 0).unwrap();

    assert!(finished);
    assert!(!restarted.slot_occupied().unwrap());
    assert_eq!(recording.saved_fragment_event_numbers(false), [1, 2, 3]);
}

#[test]
fn a_terminal_with_a_short_proof_keeps_its_rows_and_certifies_no_end() {
    // Goal: a process that ends while writing a kept terminal's proof leaves work the next one
    // finishes, and the proof claims no end only the source could prove.
    // Method: stop a terminal with its rows saved and a short proof, remove the source, take the
    // step after every run's turn, and read the terminal.
    let mut recording = recording_with_withheld_events();
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_terminal(&first, Interruption::EventsStaged);
    drop(first);
    keep_short_proof(&refusal_tests::attempt_directory(
        &recording.root,
        "recovery-data",
        "pending",
    ));
    recording.lose_source();
    let restarted = Store::open(&recording.root).unwrap();

    let finished = super::super::persist_unowned(&restarted, &[], 0).unwrap();

    assert!(finished);
    assert_eq!(recording.saved_terminal_event_numbers(false), [1, 2]);
    assert_eq!(recording.saved_terminal().lifecycle, Lifecycle::Active);
    assert_eq!(
        recording.saved_terminal().evidence,
        EndpointEvidence::SurvivingRow
    );
}
