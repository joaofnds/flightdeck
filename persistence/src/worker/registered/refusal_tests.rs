//! What a restart must leave standing: damage stops the worker, and a refusal keeps its evidence.

use super::{
    InterruptedWindow, Interruption, LIVE_MINUTE, PUBLICATION_STEPS, Recording, UNINTERRUPTED,
    publish_three_minutes, recording_with_withheld_events, whole_worker, whole_worker_of,
};
use crate::Store;
use crate::resources::ByteBudget;
use crate::sealed_apps::SealedApps;
use crate::store::PublicationLedger;
use crate::transaction::{UnknownProvenance, UnknownReason};
use crate::worker::registered::Selection;
use crate::worker::{Progress, RunState, WindowIdentity, Writers, persist_one};
use crate::{Encoder, Error};
use collector::recording::bootstrap::codec;
use collector::recording::{RegisteredRun, Registry};
use collector::ring::{Identity, Place, RingReader, RunIdentity, Writer};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// The one attempt's directory under `base`, whichever of its places it has reached.
fn attempt_directory(root: &Path, base: &str, child: &str) -> PathBuf {
    let mut objects = std::fs::read_dir(root.join(base)).unwrap();
    let object = objects.next().unwrap().unwrap().path();
    assert!(objects.next().is_none());

    object.join(child)
}

#[test]
fn a_saved_batch_that_reads_as_no_batch_stops_the_worker() {
    // Goal: a whole file in a batch's place whose rows do not read is damage, never a write a
    // process left torn, so it is reported rather than removed with no trace (P02.3).
    // Method: put a saved table in the batch's place with nothing else saved, remove the source so
    // that nothing could rewrite it, restart, and read the refusal and the file.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::TablesWritten);
    drop(first);
    let pending = attempt_directory(&recording.root, "data", "pending");
    let batch = Recording::only_event_batch(&pending);
    std::fs::rename(pending.join("ticks.parquet"), &batch).unwrap();
    std::fs::remove_file(pending.join("events/index")).unwrap();
    for table in std::fs::read_dir(&pending).unwrap() {
        let path = table.unwrap().path();
        if path.is_file() {
            std::fs::remove_file(path).unwrap();
        }
    }
    recording.lose_source();
    let restarted = Store::open(&recording.root).unwrap();

    let result = recording.persist_result(&restarted, &mut RunState::default());

    assert!(result.is_err());
    assert!(batch.exists());
}

#[test]
fn a_batch_saved_past_a_missing_one_stops_the_worker() {
    // Goal: the writer creates batches in order, so a batch past a missing ordinal is not what a
    // process leaves, and it stays to be read rather than being taken for nothing saved (P02.3).
    // Method: stop before the event index, move the only batch one ordinal on, restart, and read
    // the refusal and the batch.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::EventIndexAbsent);
    drop(first);
    let pending = attempt_directory(&recording.root, "data", "pending");
    let moved = pending
        .join("events")
        .join(crate::event_recovery::batch_name(1));
    std::fs::rename(Recording::only_event_batch(&pending), &moved).unwrap();
    let restarted = Store::open(&recording.root).unwrap();

    let result = recording.persist_result(&restarted, &mut RunState::default());

    assert!(matches!(result, Err(Error::Io(error)) if error.kind() == ErrorKind::Unsupported));
    assert!(moved.exists());
}

#[test]
fn a_batch_its_index_names_that_is_gone_stops_the_worker() {
    // Goal: the writer finishes every batch before their index, so an index that names a batch no
    // longer there is damage, which stays to be read rather than being staged over (P02.3).
    // Method: stop with the index and tables written, remove the batch, restart with the source
    // still holding the minute, and read the refusal and the index.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::TablesWritten);
    drop(first);
    let pending = attempt_directory(&recording.root, "data", "pending");
    std::fs::remove_file(Recording::only_event_batch(&pending)).unwrap();
    let restarted = Store::open(&recording.root).unwrap();

    let result = recording.persist_result(&restarted, &mut RunState::default());

    assert!(result.is_err());
    assert!(pending.join("events/index").exists());
}

#[test]
fn a_whole_proof_that_does_not_read_stops_the_worker_with_its_tables_in_place() {
    // Goal: a proof file of its full length that does not decode is damage, not a short write, so
    // recovery neither rewrites nor removes anything around it (P02.3).
    // Method: stop with the tables written and no proof, give the attempt a proof taken from a
    // twin recording with its first byte damaged, restart, and read the refusal and that the
    // tables are the same files.
    let mut twin = Recording::new();
    publish_three_minutes(&mut twin);
    let twin_store = Store::open(&twin.root).unwrap();
    twin.interrupted_seal(&twin_store, Interruption::Proved);
    drop(twin_store);
    let mut damaged =
        std::fs::read(attempt_directory(&twin.root, "data", "pending").join("metadata")).unwrap();
    damaged[0] ^= 0xff;
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::TablesWritten);
    drop(first);
    let pending = attempt_directory(&recording.root, "data", "pending");
    let table = pending.join("processes.parquet");
    let before = std::fs::metadata(&table).unwrap().ino();
    let mut proof = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(pending.join("metadata"))
        .unwrap();
    proof.write_all(&damaged).unwrap();
    drop(proof);
    let restarted = Store::open(&recording.root).unwrap();

    let result = recording.persist_result(&restarted, &mut RunState::default());

    assert!(matches!(result, Err(Error::Io(error)) if error.kind() == ErrorKind::InvalidData));
    assert_eq!(std::fs::metadata(&table).unwrap().ino(), before);
    assert!(pending.join("events/index").exists());
}

#[test]
fn a_recovery_that_fails_while_it_publishes_stages_nothing_again() {
    // Goal: a recovery that has begun to publish needs every original it proved, so a failure from
    // there on stops the worker with them in place, whatever kind of failure it is (P02.3).
    // Method: stop with the tables written and no proof, put a file in the known namespace, which
    // only publication reads, restart, and read the refusal and that the table is the same file.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::TablesWritten);
    drop(first);
    let pending = attempt_directory(&recording.root, "data", "pending");
    let table = pending.join("processes.parquet");
    let before = std::fs::metadata(&table).unwrap().ino();
    let damage = recording.root.join("observations-known/damage");
    std::fs::write(&damage, []).unwrap();
    let restarted = Store::open(&recording.root).unwrap();

    let result = recording.persist_result(&restarted, &mut RunState::default());

    assert!(matches!(result, Err(Error::Io(error)) if error.kind() == ErrorKind::InvalidData));
    let saved = attempt_directory(&recording.root, "data", "ready");
    assert_eq!(
        std::fs::metadata(saved.join("processes.parquet"))
            .unwrap()
            .ino(),
        before
    );
    assert!(saved.join("metadata").exists());
}

#[test]
fn a_refused_seal_keeps_what_it_saved_and_refuses_the_same_way_again() {
    // Goal: the refusal of a window the ring overwrote changes nothing on disk, so the slot still
    // holds every piece of evidence a later route could use (P02.3).
    // Method: stop with the tables written, overwrite the window, restart twice, and after each
    // read the refusal, the slot, the batch, the index and the tables.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::TablesWritten);
    drop(first);
    recording.publish_minutes(3..13);
    let pending = attempt_directory(&recording.root, "data", "pending");

    for _restart in 0..2 {
        let restarted = Store::open(&recording.root).unwrap();

        let refusal = recording
            .persist_result(&restarted, &mut RunState::default())
            .err()
            .unwrap();

        assert!(matches!(
            refusal,
            Error::Bound("interrupted window overwritten")
        ));
        assert!(restarted.interrupted().unwrap().is_some());
        assert!(pending.join("events/index").exists());
        assert!(pending.join("processes.parquet").exists());
        assert!(Recording::only_event_batch(&pending).exists());
    }
}

#[test]
fn a_mark_of_unknown_extent_outlives_a_minute_staged_again() {
    // Goal: a minute staged again carries what its attempt registered, so a mark raised since
    // goes to the next minute rather than vanishing (P02.3).
    // Method: stop a seal registered with no mark inside a table, count an eviction in the registry
    // as one that happened while the worker was down, run the whole worker, and read what each
    // minute names.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    recording.writer.finish();
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::TableTorn);
    drop(first);
    let registry = Registry {
        runs: vec![recording.registered()],
        evictions: 1,
    };

    whole_worker_of(&recording.root, &registry, &mut |_pause| {
        panic!("a finished run gives the worker nothing to wait for")
    });

    let named = UnknownProvenance {
        reason: UnknownReason::Predecessor,
        run: recording.run.run,
        boot: recording.run.boot,
    };
    assert_eq!(
        recording.saved_unknown_provenance(),
        [None, Some(named), None]
    );
}

#[test]
fn a_mark_its_attempt_registered_stays_on_the_minute_staged_again() {
    // Goal: a mark the attempt registered is the minute's own, so the next minute claims no unknown
    // extent it does not have (P02.3).
    // Method: stop a seal registered with the mark inside a table, restart the whole worker in a
    // registry that still counts the eviction, and read what each minute names.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    recording.writer.finish();
    let first = Store::open(&recording.root).unwrap();
    let attempted = InterruptedWindow {
        number: 0,
        publication: 1,
        publications_lost: 0,
        unknown: true,
    };
    recording.interrupted_seal_of(&first, &attempted, Interruption::TableTorn);
    drop(first);
    let registry = Registry {
        runs: vec![recording.registered()],
        evictions: 1,
    };

    whole_worker_of(&recording.root, &registry, &mut |_pause| {
        panic!("a finished run gives the worker nothing to wait for")
    });

    let named = UnknownProvenance {
        reason: UnknownReason::Predecessor,
        run: recording.run.run,
        boot: recording.run.boot,
    };
    assert_eq!(
        recording.saved_unknown_provenance(),
        [Some(named), None, None]
    );
}

#[test]
fn the_whole_worker_saves_a_run_it_deferred_once_the_slot_comes_free() {
    // Goal: a run deferred behind another run's interrupted attempt loses nothing and is not
    // retired early (P02.3).
    // Method: list the deferred run first, run the whole worker over both finished runs, and read
    // what each saved and whether each was retired.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    recording.writer.finish();
    let mut other = recording.another_run();
    other.publish_in_minute(0);
    other.publish_in_minute(1);
    other.writer.finish();
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::TableTorn);
    drop(first);
    let runs = vec![other.registered(), recording.registered()];

    whole_worker(&recording.root, runs, &mut |_pause| {
        panic!("finished runs give the worker nothing to wait for")
    });

    assert_eq!(recording.saved_windows().len(), 5);
    for run in [other.registered(), recording.registered()] {
        assert!(Registry::completed(&recording.root, &run.identity, &run.ring_name).unwrap());
    }
}

#[test]
fn a_recovery_the_machine_refuses_says_so_rather_than_staging_again() {
    // Goal: running out of memory says nothing about what the attempt saved, so it is not a reason
    // to write any of it again (P02.3).
    // Method: stop with the tables written, and prove them with a budget too small to read them.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::TablesWritten);
    drop(first);
    let restarted = Store::open(&recording.root).unwrap();
    let reader = RingReader::open_generation(recording.writer.name()).unwrap();
    let short = Arc::new(ByteBudget::new(512 * 1024));
    let ledger = PublicationLedger::new(&short).unwrap();

    let result =
        crate::worker::registered::recover_staging_proof(&restarted, reader, &short, &ledger);

    assert!(matches!(
        result,
        Err(Error::Io(error)) if error.kind() == ErrorKind::OutOfMemory
    ));
}

#[test]
fn a_deferred_run_reads_only_whether_the_slot_is_still_held() {
    // Goal: a held slot costs a deferred run one read each pass, not a walk of what the attempt
    // saved, which would repeat every second for as long as the slot stays held (P02.3).
    // Method: defer a run, damage what the attempt saved, which any walk of it would report, and
    // step the run again.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let mut other = recording.another_run();
    other.publish_in_minute(0);
    other.publish_in_minute(1);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::EventIndexAbsent);
    drop(first);
    let restarted = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();
    assert!(matches!(
        other.persist(&restarted, &mut state),
        Progress::Deferred
    ));
    let pending = attempt_directory(&recording.root, "data", "pending");
    std::fs::write(pending.join("events/unexpected"), b"").unwrap();

    let again = other.persist(&restarted, &mut state);

    assert!(matches!(again, Progress::Deferred));
}

#[test]
fn a_ring_with_no_identity_is_a_source_the_worker_goes_on_without() {
    // Goal: a ring a collector wrote with no registered identity cannot prove what it holds, and
    // that must not stop every later run (P02.3).
    // Method: register a ring created with no identity under an identity of its own, as capturing
    // the previous ring does, and take a worker step.
    let recording = Recording::new();
    let store = Store::open(&recording.root).unwrap();
    let place = Place {
        lock_path: recording.base.join("bare.lock"),
        name_prefix: format!(
            "/fdb{:x}{:x}",
            std::process::id(),
            super::NEXT.fetch_add(1, Ordering::Relaxed)
        ),
    };
    let bare = Writer::create(&place, super::shape()).unwrap();
    let registered = RegisteredRun {
        identity: RunIdentity {
            recording: store.recording(),
            run: Identity([28, 29]),
            boot: Identity([30, 31]),
        },
        ring_name: bare.name().to_owned(),
        collector_pid: 0,
    };
    let derive = SealedApps::default();
    let writers = Writers {
        encoder: Encoder,
        derive: &derive,
    };

    let progress = persist_one(
        &store,
        &writers,
        &registered,
        false,
        &mut RunState::default(),
    );

    collector::ring::unlink_generation(bare.name()).unwrap();
    assert!(matches!(progress, Ok(Progress::Unavailable)));
}

#[test]
fn a_source_too_large_to_save_is_one_the_worker_goes_on_without() {
    // Goal: a run no window could hold stops nothing, so the runs after it go on being saved.
    // Method: name a real ring with a section past the largest a window holds in the run the
    // worker steps.
    let recording = Recording::new();
    let store = Store::open(&recording.root).unwrap();
    let place = Place {
        lock_path: recording.base.join("oversized.lock"),
        name_prefix: format!(
            "/fdo{:x}{:x}",
            std::process::id(),
            super::NEXT.fetch_add(1, Ordering::Relaxed)
        ),
    };
    let run = RunIdentity {
        recording: store.recording(),
        run: Identity([40, 41]),
        boot: Identity([42, 43]),
    };
    let shape = collector::ring::Shape {
        cpu_capacity: 257,
        ..super::shape()
    };
    let oversized = Writer::create_recording(&place, shape, &run, 0).unwrap();
    let registered = RegisteredRun {
        identity: run,
        ring_name: oversized.name().to_owned(),
        collector_pid: 0,
    };
    let derive = SealedApps::default();
    let writers = Writers {
        encoder: Encoder,
        derive: &derive,
    };

    let progress = persist_one(
        &store,
        &writers,
        &registered,
        false,
        &mut RunState::default(),
    );

    collector::ring::unlink_generation(oversized.name()).unwrap();
    assert!(matches!(progress, Ok(Progress::Unavailable)));
}

#[test]
fn a_run_of_another_recording_is_a_source_the_worker_goes_on_without() {
    // Goal: a registry entry from another recording stops nothing and saves nothing here.
    // Method: name another recording in the run the worker steps.
    let recording = Recording::new();
    let store = Store::open(&recording.root).unwrap();
    let mut registered = recording.registered();
    registered.identity.recording = Identity([38, 39]);
    let derive = SealedApps::default();
    let writers = Writers {
        encoder: Encoder,
        derive: &derive,
    };

    let progress = persist_one(
        &store,
        &writers,
        &registered,
        false,
        &mut RunState::default(),
    );

    assert!(matches!(progress, Ok(Progress::Unavailable)));
}

#[test]
fn an_idle_scratch_beside_a_registered_slot_is_taken_for_an_attempt_given_up() {
    // Goal: a process that ended while giving up an attempt, its directories gone and its Idle
    // scratch not yet renamed, leaves a state a restart finishes the same way (P02.3).
    // Method: leave exactly that, restart, and compare with an uninterrupted run.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::Registered);
    let recording_identity = first.recording();
    drop(first);
    let pending = attempt_directory(&recording.root, "data", "pending");
    let object = pending.parent().unwrap().to_owned();
    std::fs::remove_dir(&pending).unwrap();
    std::fs::remove_dir(&object).unwrap();
    let mut idle = [0; 4096];
    codec::encode_idle(recording_identity, codec::SlotKind::Foreground, &mut idle).unwrap();
    let mut scratch = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(recording.root.join("foreground.pending"))
        .unwrap();
    scratch.write_all(&idle).unwrap();
    drop(scratch);
    let restarted = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();

    recording.persist(&restarted, &mut state);
    recording.persist(&restarted, &mut state);

    assert_eq!(recording.saved_files(), *UNINTERRUPTED);
}

#[test]
fn a_later_minute_interrupted_inside_a_table_converges() {
    // Goal: recovery stages the minute the attempt names, not the oldest the ring holds (P02.3).
    // Method: save the first minute, stop the second inside a table with the first still in the
    // ring, restart, and compare with an uninterrupted run.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.persist(&first, &mut RunState::default());
    let second = InterruptedWindow {
        number: 1,
        publication: 3,
        publications_lost: 0,
        unknown: false,
    };
    recording.interrupted_seal_of(&first, &second, Interruption::TableTorn);
    drop(first);
    let restarted = Store::open(&recording.root).unwrap();

    let progress = recording.persist(&restarted, &mut RunState::default());

    assert!(matches!(progress, Progress::Sealed));
    assert_eq!(recording.frontier(&restarted).unwrap().publication_end(), 4);
    assert_eq!(recording.saved_files(), *UNINTERRUPTED);
}

#[test]
fn a_publication_reaches_idle_in_the_steps_the_interruption_tests_walk() {
    // Goal: the tests that stop a publication at each step cover all of its steps.
    // Method: take the last count they walk, and read an idle slot.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let store = Store::open(&recording.root).unwrap();

    recording.interrupted_seal(&store, Interruption::PublicationSteps(PUBLICATION_STEPS));

    assert!(store.interrupted().unwrap().is_none());
}

#[test]
fn a_registration_refused_with_its_selection_retained_stops_the_worker() {
    // Goal: only an overwritten selection may pass as nothing lost. Any other refusal is reported.
    // Method: seal a selection the ring still holds under a window number it does not have.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let store = Store::open(&recording.root).unwrap();
    let derive = SealedApps::default();
    let writers = Writers {
        encoder: Encoder,
        derive: &derive,
    };
    let reader = RingReader::open_generation(recording.writer.name()).unwrap();
    let selection = Selection {
        identity: WindowIdentity {
            run: recording.run,
            window: LIVE_MINUTE,
        },
        publication: 1,
    };
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));

    let result = crate::worker::registered::seal(
        &store,
        &writers,
        reader,
        selection,
        &mut RunState::default(),
        &budget,
    );

    assert!(result.is_err());
    assert!(store.interrupted().unwrap().is_none());
}

#[test]
fn a_terminal_charges_its_event_writer_to_the_one_budget_it_was_given() {
    // Goal: a terminal's writer draws on the worker's budget, like a window's.
    // Method: give a terminal seal room to register and not to write, and read the refusal and
    // that it wrote no batch.
    let recording = recording_with_withheld_events();
    let store = Store::open(&recording.root).unwrap();
    let registered = recording.registered();
    let mut reader = RingReader::open_generation(&registered.ring_name).unwrap();
    reader.set_writer_gone();
    let short = Arc::new(ByteBudget::new(1024 * 1024));

    let result = crate::worker::registered::seal_terminal(
        &store,
        &registered,
        reader,
        &mut RunState::default(),
        &short,
    );

    assert!(matches!(
        result,
        Err(Error::Io(error)) if error.kind() == ErrorKind::OutOfMemory
    ));
    let pending = attempt_directory(&recording.root, "recovery-data", "pending");
    assert_eq!(std::fs::read_dir(pending).unwrap().count(), 0);
}

#[test]
fn a_torn_frontier_table_stops_the_next_minute_with_its_error() {
    // Goal: the frontier's own table torn is reported as its removal is (P02.3).
    // Method: seal a minute, tear its ticks, and take a worker step through a new Store.
    let mut recording = Recording::new();
    recording.publish_minutes(0..3);
    let first = Store::open(&recording.root).unwrap();
    recording.persist(&first, &mut RunState::default());
    drop(first);
    let ready = attempt_directory(&recording.root, "data", "ready");
    Recording::tear(&ready.join("ticks.parquet"));
    let restarted = Store::open(&recording.root).unwrap();

    let result = recording.persist_result(&restarted, &mut RunState::default());

    assert!(result.is_err());
}

#[test]
fn a_terminal_with_saved_rows_stays_while_its_source_has_not_ended() {
    // Goal: rows are given up only for a source that reads the same again, so one that has not
    // ended keeps them (P02.3).
    // Method: stop a terminal with its rows saved, and ask a restarted store to give it up with a
    // reader that does not say the writer is gone.
    let recording = recording_with_withheld_events();
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_terminal(&first, Interruption::EventsStaged);
    drop(first);
    let restarted = Store::open(&recording.root).unwrap();
    let reader = RingReader::open_generation(recording.ring_name()).unwrap();
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));

    let result = restarted.abandon_terminal_attempt(&reader, &budget);

    assert!(result.is_err());
    let pending = attempt_directory(&recording.root, "recovery-data", "pending");
    assert_eq!(std::fs::read_dir(pending).unwrap().count(), 1);
}
