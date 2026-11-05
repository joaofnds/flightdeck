use super::super::{Progress, RunState, Writers, persist_one};
use crate::canonical::{ReadWork, SourceFrontier};
use crate::resources::ByteBudget;
use crate::sealed_apps::SealedApps;
use crate::{Encoder, Store};
use collector::recording::private::{self, FileLock, PrivateDirectory};
use collector::recording::{RegisteredRun, bootstrap};
use collector::ring::{Identity, Place, RunIdentity, Shape, Writer};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A registered recording and the live ring of one collector run, removed together.
struct Recording {
    base: PathBuf,
    root: PathBuf,
    writer: Writer,
    run: RunIdentity,
    tick: u64,
}

impl Recording {
    fn new() -> Self {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let base = std::env::temp_dir().join(format!("fd-worker-{}-{serial}", std::process::id()));
        private::create_private_dir(&base).unwrap();
        let root = base.join("recording");
        private::create_private_dir(&root).unwrap();
        let directory = PrivateDirectory::open(&root).unwrap();
        let lock = FileLock::try_lock_in(&directory, "registration.lock").unwrap();
        let recording = bootstrap::initialize(&directory, &lock, &mut [0; 4097]).unwrap();
        drop(lock);
        let run = RunIdentity {
            recording,
            run: Identity([8, 9]),
            boot: Identity([10, 11]),
        };
        let place = Place {
            lock_path: base.join("ring.lock"),
            name_prefix: format!("/fdw{:x}{serial:x}", std::process::id()),
        };
        let shape = Shape {
            slot_count: 8,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 8,
        };
        let writer = Writer::create_recording(&place, shape, &run, 1).unwrap();
        Self {
            base,
            root,
            writer,
            run,
            tick: 0,
        }
    }

    /// Publishes one tick in the given minute of the run.
    fn publish_in_minute(&mut self, minute: u64) {
        let memory = collector::machine::Memory::default();
        self.tick += 1;
        let readings = collector::ring::Readings {
            run_tick: minute * 60,
            cpus: &[],
            memory: &memory,
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: collector::ring::Dropped::default(),
        };
        let times = collector::ring::ObservationTimes {
            wall_ns: minute * 60_000_000_000 + self.tick,
            continuous_ns: minute * 60_000_000_000 + self.tick,
            awake_ns: minute * 60_000_000_000 + self.tick,
        };
        self.writer.publish_observed(&readings, &times);
    }

    /// The run as the registry names it. No pid means the source's producer is gone.
    fn registered(&self) -> RegisteredRun {
        RegisteredRun {
            identity: self.run,
            ring_name: self.writer.name().to_owned(),
            collector_pid: 0,
        }
    }

    fn persist(&self, store: &Store, state: &mut RunState) -> Progress {
        self.persist_result(store, state).unwrap()
    }

    fn persist_result(&self, store: &Store, state: &mut RunState) -> crate::Result<Progress> {
        let derive = SealedApps::default();
        let writers = Writers {
            encoder: Encoder,
            derive: &derive,
        };
        persist_one(store, &writers, &self.registered(), false, state)
    }

    /// The directories that hold what has been saved so far, one for each window.
    fn saved_windows(&self) -> Vec<PathBuf> {
        std::fs::read_dir(self.root.join("data"))
            .unwrap()
            .map(|directory| directory.unwrap().path())
            .collect()
    }

    /// Removes the Parquet tables of the named saved windows, and returns how many it removed.
    fn remove_tables(saved: &[PathBuf]) -> usize {
        let mut removed = 0;
        for window in saved {
            for table in std::fs::read_dir(window.join("ready")).unwrap() {
                let path = table.unwrap().path();
                if path
                    .extension()
                    .is_some_and(|extension| extension == "parquet")
                {
                    std::fs::remove_file(path).unwrap();
                    removed += 1;
                }
            }
        }
        removed
    }

    fn frontier(&self, store: &Store) -> Option<SourceFrontier> {
        store
            .source_frontier(
                &self.run,
                &Arc::new(ByteBudget::new(256 * 1024 * 1024)),
                &mut ReadWork::default(),
            )
            .unwrap()
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        collector::ring::unlink_generation(self.writer.name()).unwrap();
        std::fs::remove_dir_all(&self.base).unwrap();
    }
}

#[test]
fn a_ready_minute_seals_through_the_worker_and_moves_the_frontier() {
    // Goal: the production worker saves a ready minute of a registered recording.
    // Method: publish a tick in each of two minutes, take one worker step, and read the
    // canonical frontier through a Store that did not write it.
    let mut recording = Recording::new();
    recording.publish_in_minute(0);
    recording.publish_in_minute(1);
    let store = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();

    let progress = recording.persist(&store, &mut state);

    assert!(matches!(progress, Progress::Sealed));
    drop(store);
    let reopened = Store::open(&recording.root).unwrap();
    let frontier = recording.frontier(&reopened).unwrap();
    assert_eq!(frontier.publication_end(), 2);
    assert_eq!(frontier.prior_window(), Some(0));
}

#[test]
fn a_restarted_worker_continues_at_the_frontier() {
    // Goal: a new persister takes up at the saved frontier and chains to the saved window.
    // Method: seal one minute, then seal the next through a new Store and new state, as a
    // restart does, and read the frontier through a third Store.
    let mut recording = Recording::new();
    for minute in 0..3 {
        recording.publish_in_minute(minute);
    }
    let first = Store::open(&recording.root).unwrap();
    let progress = recording.persist(&first, &mut RunState::default());
    assert!(matches!(progress, Progress::Sealed));
    drop(first);
    let restarted = Store::open(&recording.root).unwrap();

    let progress = recording.persist(&restarted, &mut RunState::default());

    assert!(matches!(progress, Progress::Sealed));
    drop(restarted);
    let reopened = Store::open(&recording.root).unwrap();
    let frontier = recording.frontier(&reopened).unwrap();
    assert_eq!(frontier.publication_end(), 3);
    assert_eq!(frontier.prior_window(), Some(1));
}

#[test]
fn an_unavailable_old_table_does_not_stop_a_new_minute() {
    // Goal: damage to a window behind the frontier cannot block new live minutes (P02.3).
    // Method: seal two minutes, remove every table of the older one, then seal the next minute
    // through a new Store and read the frontier through a third.
    let mut recording = Recording::new();
    for minute in 0..4 {
        recording.publish_in_minute(minute);
    }
    let first = Store::open(&recording.root).unwrap();
    recording.persist(&first, &mut RunState::default());
    let older = recording.saved_windows();
    recording.persist(&first, &mut RunState::default());
    drop(first);
    assert!(Recording::remove_tables(&older) > 0);
    let restarted = Store::open(&recording.root).unwrap();

    let progress = recording.persist(&restarted, &mut RunState::default());

    assert!(matches!(progress, Progress::Sealed));
    drop(restarted);
    let reopened = Store::open(&recording.root).unwrap();
    assert_eq!(recording.frontier(&reopened).unwrap().publication_end(), 4);
}

#[test]
fn an_unavailable_frontier_table_stops_the_next_minute_with_its_error() {
    // Goal: the frontier's own row proves where saving resumes, so losing it is reported.
    // Method: seal a minute, remove its tables, and take a worker step through a new Store.
    let mut recording = Recording::new();
    for minute in 0..3 {
        recording.publish_in_minute(minute);
    }
    let first = Store::open(&recording.root).unwrap();
    recording.persist(&first, &mut RunState::default());
    drop(first);
    assert!(Recording::remove_tables(&recording.saved_windows()) > 0);
    let restarted = Store::open(&recording.root).unwrap();

    let result = recording.persist_result(&restarted, &mut RunState::default());

    assert!(
        matches!(result, Err(crate::Error::Io(error)) if error.kind() == std::io::ErrorKind::NotFound)
    );
}
