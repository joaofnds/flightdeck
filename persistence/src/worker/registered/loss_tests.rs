use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use collector::recording::RegisteredRun;
use collector::ring::{ObservationTimes, Place, Readings, RingReader, RunIdentity, Shape, Writer};

use super::super::tests::forged_source::{ForgedWord, WritableSource};
use super::super::{Progress, RunState, SourceState, Writers, persist_one};
use super::tests::{SLOT_CHANGES_MAX, SlotChanges};
use crate::canonical::ReadWork;
use crate::canonical::catalog::{Catalog, EventObject};
use crate::resources::ByteBudget;
use crate::sealed_apps::SealedApps;
use crate::store::PublicationStep;
use crate::transaction::normal::NormalState;
use crate::{Encoder, Store};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// The header word that holds the newest publication's number, in every ring layout.
const HEADER_PUBLICATION_NEWEST: usize = 4;

/// More passes than a worker takes from any interrupted attempt to the end of the run.
const WORKER_PASSES_MAX: u32 = 8;

/// More steps than publishing a run's loss takes.
const PUBLICATION_STEPS_MAX: u32 = 1 << 20;

/// A run that ended with nothing readable: its one-slot ring overwrote its only finished reading
/// while the reading after it never finished, and it published no events.
struct LostRun {
    root: PathBuf,
    writer: Writer,
    snapshot: WritableSource,
    registered: RegisteredRun,
}

impl LostRun {
    fn new() -> Self {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("fd-lost-{}-{serial}", std::process::id()));
        let store = Store::open(&root).unwrap();
        let run = collector::recording::new_run_identity(store.recording()).unwrap();
        drop(store);
        let mut writer = new_writer(&root, serial, &run);
        for tick in 1..=2 {
            new_publish(&mut writer, tick);
        }
        let bytes = RingReader::open_generation(writer.name())
            .unwrap()
            .mapped_size_bytes();
        let snapshot = WritableSource::new(
            writer.name(),
            bytes,
            ForgedWord {
                watched: HEADER_PUBLICATION_NEWEST,
                expected: 2,
                index: HEADER_PUBLICATION_NEWEST,
                value: 1,
            },
        );
        let registered = RegisteredRun {
            identity: run,
            ring_name: snapshot.name.clone(),
            collector_pid: 0,
        };

        Self {
            root,
            writer,
            snapshot,
            registered,
        }
    }

    /// Runs a worker of `root` to the end of the run.
    fn finish(&self, state: &mut RunState) {
        self.snapshot.refresh();
        let store = Store::open(&self.root).unwrap();
        let derive = SealedApps::default();
        let writers = Writers {
            encoder: Encoder,
            derive: &derive,
        };
        for _pass in 0..WORKER_PASSES_MAX {
            let progress = persist_one(&store, &writers, &self.registered, true, state).unwrap();
            if matches!(progress, Progress::Complete) {
                return;
            }
        }
        panic!("the worker never reached the end of the run");
    }

    /// Takes `steps` steps of publishing the run's loss, or all it takes, then ends as a process
    /// that ended does, and gives the steps it took.
    fn interrupted_publication(&self, steps: u32) -> u32 {
        self.interrupted_publication_when(|taken| taken == steps)
    }

    /// Ends the run's publication once the attempt in the slot has chosen a census.
    fn interrupted_at_census(&self) {
        let recording = Store::open(&self.root).unwrap().recording();
        let slot = self.root.join("foreground");
        self.interrupted_publication_when(|_taken| {
            let bytes = std::fs::read(&slot).unwrap();
            NormalState::decode(&bytes, recording).is_ok_and(|state| state.census.kind == 1)
        });
    }

    fn interrupted_publication_when(&self, mut stop: impl FnMut(u32) -> bool) -> u32 {
        let budget = Arc::new(ByteBudget::new(1 << 28));
        let store = Store::open(&self.root).unwrap();
        let mut state = RunState::default();
        state.losses.publications = 1;
        let mut reader = RingReader::open_generation(&self.registered.ring_name).unwrap();
        reader.set_writer_gone();
        let (input, frontier) =
            super::seal_terminal_input(&store, &self.registered, &reader, &state, &budget).unwrap();
        let attempt = store
            .register_attempt_from_frontier(&input, &mut reader, frontier)
            .unwrap();
        let mut proof = attempt.prove_loss(reader, &budget).unwrap();
        for step in 0..PUBLICATION_STEPS_MAX {
            if stop(step) || proof.step().unwrap() == PublicationStep::Idle {
                return step;
            }
        }

        PUBLICATION_STEPS_MAX
    }

    /// Finishes from a new store what no run can, which takes no source, and expects a free slot.
    fn expect_slot_freed(&self) {
        let store = Store::open(&self.root).unwrap();

        super::persist_unowned(&store, &[], 0).unwrap();

        assert!(store.interrupted().unwrap().is_none());
        assert!(
            std::fs::read_dir(self.root.join("recovery-data"))
                .unwrap()
                .next()
                .is_none()
        );
    }

    /// Whatever a process left in the slot's waiting state, giving up the attempt frees the slot.
    fn expect_slot_freed_beside(&self, waiting: &[u8]) {
        use std::io::Write;
        self.interrupted_at_census();
        let mut file = collector::recording::private::open_private_file(
            &self.root.join("foreground.pending"),
            true,
        )
        .unwrap();
        file.write_all(waiting).unwrap();
        drop(file);

        self.expect_slot_freed();
    }

    fn only_object(&self) -> EventObject {
        let budget = Arc::new(ByteBudget::new(1 << 28));
        let catalog = Catalog::open(&self.root, &budget).unwrap();
        let mut page = catalog
            .event_objects_page(None, 8, &mut ReadWork::default())
            .unwrap();
        assert_eq!(page.objects.len(), 1);

        page.objects.swap_remove(0)
    }
}

impl Drop for LostRun {
    fn drop(&mut self) {
        collector::ring::unlink_generation(self.writer.name()).unwrap();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn new_writer(root: &std::path::Path, serial: u64, run: &RunIdentity) -> Writer {
    let place = Place {
        lock_path: root.join("ring.lock"),
        name_prefix: format!("/fdlost{}x{serial}", std::process::id()),
    };
    let shape = Shape {
        slot_count: 1,
        cpu_capacity: 1,
        interface_capacity: 1,
        core_capacity: 1,
        state_capacity: 1,
        fan_capacity: 1,
        process_capacity: 1,
        coalition_capacity: 1,
        event_capacity: 1,
    };

    Writer::create_recording(&place, shape, run, 0).unwrap()
}

fn new_publish(writer: &mut Writer, tick: u64) {
    writer.publish_observed(
        &Readings {
            run_tick: tick,
            cpus: &[],
            memory: &collector::machine::Memory::default(),
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: collector::ring::Dropped::default(),
        },
        &ObservationTimes {
            wall_ns: (1000 + tick) * 1_000_000_000,
            continuous_ns: tick * 1_000_000_000,
            awake_ns: tick * 1_000_000_000,
        },
    );
}

#[test]
fn a_run_that_kept_no_reading_ends_with_an_object_of_what_it_lost() {
    // Goal: a run the worker can read nothing of ends, and says how much it lost, so that the
    // recording goes on to the next run.
    // Method: run the whole worker over a forged end of a run, then list the recording's objects.
    let lost = LostRun::new();
    let mut state = RunState::default();

    lost.finish(&mut state);

    let object = lost.only_object();
    assert_eq!(object.recovery.run, lost.registered.identity);
    assert_eq!(object.recovery.rows, 0);
    assert_eq!(object.recovery.losses.publications, 1);
    assert_eq!(object.recovery.losses.events, 0);
    assert!(!object.recovery.losses.unknown);
}

#[test]
fn a_run_that_kept_no_reading_after_an_unknown_loss_still_ends() {
    // Goal: a loss of unknown extent before the run, which a loss object cannot carry, does not
    // stop the recording at a run that kept no reading.
    // Method: run the worker with the mark an eviction leaves, then list the recording's objects.
    let lost = LostRun::new();
    let mut state = RunState::default();
    state.losses.unknown = true;

    lost.finish(&mut state);

    let object = lost.only_object();
    assert_eq!(object.recovery.rows, 0);
    assert_eq!(object.recovery.losses.publications, 1);
    assert!(object.recovery.losses.unknown);
}

#[test]
fn a_loss_object_interrupted_along_its_publication_converges() {
    // Goal: a process that ends anywhere in publishing a run's loss leaves nothing that stops the
    // recording, and the next process finishes with the same object.
    // Method: count the steps one publication takes, then drop a publication after each of an even
    // spread of those steps, as a process that ended does, and run the whole worker from a new
    // store. Each drop costs most of a second, so walking all of a publication's 300 steps takes
    // minutes.
    const DROPS: u32 = 24;
    let total = {
        let lost = LostRun::new();
        lost.interrupted_publication(PUBLICATION_STEPS_MAX)
    };
    assert!(total > 0);
    assert!(total < PUBLICATION_STEPS_MAX);
    let uninterrupted = {
        let lost = LostRun::new();
        lost.interrupted_publication(PUBLICATION_STEPS_MAX);
        lost.only_object()
    };
    for drop in 0..=DROPS {
        let steps = total * drop / DROPS;
        let lost = LostRun::new();
        lost.interrupted_publication(steps);

        lost.finish(&mut RunState::default());

        let object = lost.only_object();
        assert_eq!(object.kind, uninterrupted.kind, "after {steps} steps");
        assert_eq!(
            object.recovery.rows, uninterrupted.recovery.rows,
            "after {steps} steps"
        );
        assert_eq!(
            object.recovery.losses, uninterrupted.recovery.losses,
            "after {steps} steps"
        );
    }
}

/// Ends a run's loss publication after the slot's `changes`th change, takes the step after every
/// run's turn with the run's source gone, and says whether the publication had ended by itself.
fn a_loss_stopped_after(changes: usize) -> bool {
    let lost = LostRun::new();
    let mut slot = SlotChanges::of(&lost.root);
    lost.interrupted_publication_when(|_taken| slot.reached(changes));
    let store = Store::open(&lost.root).unwrap();
    let ended = !store.slot_occupied().unwrap();
    let state = RunState {
        identity: Some(lost.registered.identity),
        source: SourceState::Gone,
        ..RunState::default()
    };

    super::persist_unowned(&store, std::slice::from_ref(&state), 0).unwrap();

    assert!(
        store.interrupted().unwrap().is_none(),
        "after {changes} changes"
    );
    ended
}

#[test]
fn a_loss_stopped_at_each_change_of_the_slot_frees_it_once_its_source_is_gone() {
    // Goal: a process that ends at any point of publishing a run's loss that the next one can tell
    // apart, whose source then goes, leaves nothing that keeps the other runs waiting.
    // Method: stop the publication after one more change of the slot each time, until it ends
    // first.
    let changes = (0..SLOT_CHANGES_MAX).find(|changes| a_loss_stopped_after(*changes));

    assert!(changes.is_some());
}

#[test]
fn a_loss_with_a_census_and_no_source_frees_the_slot() {
    // Goal: nothing can prove what a run lost once its source goes, so its census holds nothing up.
    // Method: end the publication once the slot names a census, and finish in a new store.
    let lost = LostRun::new();
    lost.interrupted_at_census();

    lost.expect_slot_freed();
}

#[test]
fn a_loss_whose_source_refuses_waits_for_it() {
    // Goal: a refused open does not show the source is gone, so the count it could still prove
    // stays.
    // Method: end the publication at its census, and take the step after every run's turn with the
    // run's source refused.
    let lost = LostRun::new();
    lost.interrupted_at_census();
    let store = Store::open(&lost.root).unwrap();
    let state = RunState {
        identity: Some(lost.registered.identity),
        source: SourceState::Unavailable,
        ..RunState::default()
    };

    let finished = super::persist_unowned(&store, std::slice::from_ref(&state), 0).unwrap();

    assert!(!finished);
    assert!(store.slot_occupied().unwrap());
}

impl LostRun {
    /// Where the attempt in the slot keeps its files.
    fn object(&self) -> PathBuf {
        std::fs::read_dir(self.root.join("recovery-data"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
    }

    /// Ends the run's publication once its census is on disk.
    fn interrupted_with_census_file(&self) {
        let root = self.root.join("recovery-data");
        self.interrupted_publication_when(|_taken| {
            std::fs::read_dir(&root).is_ok_and(|mut objects| {
                objects
                    .next()
                    .is_some_and(|object| object.unwrap().path().join("original-census").exists())
            })
        });
    }
}

#[test]
fn a_loss_given_up_refuses_a_whole_batch_beside_its_census() {
    // Goal: only a batch no row can be read from goes with a given up attempt, since a whole one
    // would be rows lost with no trace.
    // Method: end the publication with its census on disk, put a whole file in the first batch's
    // place, take the step after every run's turn, and read the refusal and the file.
    let lost = LostRun::new();
    lost.interrupted_with_census_file();
    let batch = lost
        .object()
        .join("pending")
        .join(crate::event_recovery::batch_name(0));
    let mut frame = polars::prelude::DataFrame::new(
        1,
        vec![polars::prelude::Column::new("event".into(), [1_u64])],
    )
    .unwrap();
    crate::encoder::encode_write(
        &mut frame,
        collector::recording::private::open_private_file(&batch, true).unwrap(),
    )
    .unwrap();
    let store = Store::open(&lost.root).unwrap();

    let result = super::persist_unowned(&store, &[], 0);

    assert!(result.is_err());
    assert!(batch.exists());
}

#[test]
fn a_loss_given_up_takes_its_census_torn_batch_and_short_proof() {
    // Goal: all that an attempt with no rows can leave goes with it, so the slot frees.
    // Method: end the publication with its census on disk, add a torn first batch and a short
    // proof, and finish in a new store.
    use std::io::Write;
    let lost = LostRun::new();
    lost.interrupted_with_census_file();
    let pending = lost.object().join("pending");
    for name in [crate::event_recovery::batch_name(0), "metadata".to_string()] {
        let mut file =
            collector::recording::private::open_private_file(&pending.join(name), true).unwrap();
        file.write_all(b"short").unwrap();
    }

    lost.expect_slot_freed();
}

#[test]
fn a_loss_given_up_after_its_object_went_frees_the_slot() {
    // Goal: a process that ends between removing the object and clearing the slot leaves work the
    // next one finishes.
    // Method: remove the object as that process did, and finish in a new store.
    let lost = LostRun::new();
    lost.interrupted_at_census();
    let object = std::fs::read_dir(lost.root.join("recovery-data"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    std::fs::remove_dir_all(object).unwrap();

    lost.expect_slot_freed();
}

#[test]
fn a_loss_given_up_beside_a_whole_idle_state_frees_the_slot() {
    // Goal: a process that ends while clearing the slot leaves work the next one finishes.
    // Method: leave the Idle state it wrote beside the slot, and finish in a new store.
    let lost = LostRun::new();
    let recording = Store::open(&lost.root).unwrap().recording();
    let mut idle = [0; 4096];
    collector::recording::bootstrap::codec::encode_idle(
        recording,
        collector::recording::bootstrap::codec::SlotKind::Foreground,
        &mut idle,
    )
    .unwrap();

    lost.expect_slot_freed_beside(&idle);
}

#[test]
fn a_loss_given_up_beside_a_short_state_frees_the_slot() {
    // Goal: the census's next state, torn as a process that ended leaves it, is intent and not
    // authority, so it does not stop the slot clearing.
    // Method: leave a short state beside the slot, and finish in a new store.
    let lost = LostRun::new();

    lost.expect_slot_freed_beside(&[7; 16]);
}
