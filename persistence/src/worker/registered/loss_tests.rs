use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use collector::recording::RegisteredRun;
use collector::ring::{ObservationTimes, Place, Readings, RingReader, RunIdentity, Shape, Writer};

use super::super::tests::forged_source::{ForgedWord, WritableSource};
use super::super::{Progress, RunState, Writers, persist_one};
use crate::canonical::ReadWork;
use crate::canonical::catalog::{Catalog, EventObject};
use crate::resources::ByteBudget;
use crate::sealed_apps::SealedApps;
use crate::store::PublicationStep;
use crate::{Encoder, Store};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// The header word that holds the newest publication's number, in every ring layout.
const HEADER_PUBLICATION_NEWEST: usize = 4;

/// More passes than a worker takes from any interrupted attempt to the end of the run.
const WORKER_PASSES_MAX: u32 = 8;

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
        for step in 0..steps {
            if proof.step().unwrap() == PublicationStep::Idle {
                return step;
            }
        }

        steps
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
        lost.interrupted_publication(u32::MAX)
    };
    eprintln!("a loss publication takes {total} steps");
    for drop in 0..=DROPS {
        let steps = total * drop / DROPS;
        let lost = LostRun::new();
        lost.interrupted_publication(steps);

        lost.finish(&mut RunState::default());

        let object = lost.only_object();
        assert_eq!(object.recovery.rows, 0, "after {steps} steps");
        assert_eq!(
            object.recovery.losses.publications, 1,
            "after {steps} steps"
        );
    }
}
