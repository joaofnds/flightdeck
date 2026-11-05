use super::super::{Progress, RunState, WindowIdentity, Writers, persist_one};
use crate::canonical::metadata::Body;
use crate::canonical::{CanonicalMetadata, ReadWork, SourceFrontier};
use crate::resources::ByteBudget;
use crate::sealed_apps::SealedApps;
use crate::store::{ProofStep, PublicationStep};
use crate::{Encoder, Store};
use collector::recording::private::{self, FileLock, PrivateDirectory};
use collector::recording::{RegisteredRun, bootstrap};
use collector::ring::{Identity, Place, RingReader, RunIdentity, Shape, Writer};
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};

static NEXT: AtomicU64 = AtomicU64::new(0);

/// The steps a publication takes from a complete proof to an idle slot.
const PUBLICATION_STEPS: u32 = 8;

/// How far a seal got before its process ended.
#[derive(Clone, Copy)]
enum Interruption {
    Registered,
    EventIndexAbsent,
    EventBatchTorn,
    EventsStaged,
    TableMissing,
    TableTorn,
    TablesWritten,
    Proved,
    PublicationSteps(u32),
}

fn shape() -> Shape {
    Shape {
        slot_count: 8,
        cpu_capacity: 1,
        interface_capacity: 1,
        core_capacity: 1,
        state_capacity: 1,
        fan_capacity: 1,
        process_capacity: 1,
        coalition_capacity: 1,
        event_capacity: 8,
    }
}

/// A second collector run over the same store, whose ring it unlinks when dropped.
struct OtherRun {
    writer: Writer,
    run: RunIdentity,
    tick: u64,
}

impl OtherRun {
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

    fn registered(&self) -> RegisteredRun {
        RegisteredRun {
            identity: self.run,
            ring_name: self.writer.name().to_owned(),
            collector_pid: 0,
        }
    }

    fn persist(&self, store: &Store, state: &mut RunState) -> Progress {
        let derive = SealedApps::default();
        let writers = Writers {
            encoder: Encoder,
            derive: &derive,
        };
        persist_one(store, &writers, &self.registered(), false, state).unwrap()
    }
}

impl Drop for OtherRun {
    fn drop(&mut self) {
        collector::ring::unlink_generation(self.writer.name()).unwrap();
    }
}

/// A registered recording and the live ring of one collector run, removed together.
struct Recording {
    base: PathBuf,
    root: PathBuf,
    writer: Writer,
    run: RunIdentity,
    tick: u64,
    captured: bool,
    source_lost: bool,
}

impl Recording {
    fn new() -> Self {
        Self::create(false)
    }

    /// A recording whose collector captures each process's identity and program.
    fn captured() -> Self {
        Self::create(true)
    }

    fn create(captured: bool) -> Self {
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
        let shape = shape();
        let writer = if captured {
            let capacity = collector::ring::DictionaryCapacity {
                entries: 4,
                arena_bytes: 16 * 1024,
            };
            Writer::create_with_identity(&place, shape, capacity, &run, 1).unwrap()
        } else {
            Writer::create_recording(&place, shape, &run, 1).unwrap()
        };
        Self {
            base,
            root,
            writer,
            run,
            tick: 0,
            captured,
            source_lost: false,
        }
    }

    /// Publishes one tick in the given minute of the run.
    fn publish_in_minute(&mut self, minute: u64) {
        self.publish_events_in_minute(minute, 0);
    }

    /// Publishes one tick in the given minute of the run, carrying that many exec events.
    fn publish_events_in_minute(&mut self, minute: u64, count: usize) {
        let memory = collector::machine::Memory::default();
        let events: Vec<_> = (0..count)
            .map(|_| collector::tracker::Event::Exec { pid: 41 })
            .collect();
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
            events: &events,
            dropped: collector::ring::Dropped::default(),
        };
        let times = collector::ring::ObservationTimes {
            wall_ns: minute * 60_000_000_000 + self.tick,
            continuous_ns: minute * 60_000_000_000 + self.tick,
            awake_ns: minute * 60_000_000_000 + self.tick,
        };
        if self.captured {
            let identities: Vec<_> = (0..count).map(|_| self.observed_identity()).collect();
            self.writer
                .publish_with_identity(
                    &readings,
                    Some(&times),
                    &collector::ring::IdentityPublication {
                        processes: &[],
                        events: &identities,
                    },
                )
                .unwrap();
        } else {
            self.writer.publish_observed(&readings, &times);
        }
    }

    fn observed_identity(&mut self) -> collector::ring::IdentityObservation {
        let mut reference = None;
        self.writer
            .intern_program(
                &collector::ring::ProgramBytes {
                    executable_name: b"program",
                    app_path: Some(b"/Applications/Example.app"),
                },
                &mut reference,
            )
            .unwrap();
        collector::ring::IdentityObservation {
            pid: 41,
            usage: collector::ring::UsageAvailability::NotApplicable,
            identity: collector::ring::ProcessIdentity::Measured {
                creation: collector::ring::ProcessCreation {
                    unique_id: 991,
                    pid_version: 7,
                },
                program: reference.unwrap(),
            },
        }
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

    /// A second collector run of the same recording, with a ring of its own.
    fn another_run(&self) -> OtherRun {
        let run = RunIdentity {
            run: Identity([18, 19]),
            ..self.run
        };
        let place = Place {
            lock_path: self.base.join("other-ring.lock"),
            name_prefix: format!(
                "/fdx{:x}{:x}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ),
        };
        let writer = Writer::create_recording(&place, shape(), &run, 1).unwrap();
        OtherRun {
            writer,
            run,
            tick: 0,
        }
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

    /// Every table and event batch saved so far, by window and name, for comparing recordings.
    /// The metadata and event index name the recording's own identity, so they are left out.
    fn saved_files(&self) -> Vec<Vec<(PathBuf, Vec<u8>)>> {
        let mut windows: Vec<Vec<(PathBuf, Vec<u8>)>> = self
            .saved_windows()
            .iter()
            .map(|window| {
                let ready = window.join("ready");
                let mut files = Self::saved_files_in(&ready);
                files.extend(Self::saved_files_in(&ready.join("events")));
                files.sort();
                files
            })
            .collect();
        windows.sort();
        windows
    }

    fn saved_files_in(directory: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        std::fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.is_file())
            .filter(|path| {
                path.file_name()
                    .is_some_and(|name| name != "metadata" && name != "index")
            })
            .map(|path| {
                let bytes = std::fs::read(&path).unwrap();
                (path.file_name().unwrap().into(), bytes)
            })
            .collect()
    }

    /// Removes the source as a reboot does, so no reader can open it again.
    fn lose_source(&mut self) {
        collector::ring::unlink_generation(self.writer.name()).unwrap();
        self.source_lost = true;
    }

    /// Publishes a tick in each of the minutes, past what the ring holds if there are enough.
    fn publish_minutes(&mut self, minutes: std::ops::Range<u64>) {
        for minute in minutes {
            self.publish_in_minute(minute);
        }
    }

    /// The counts of lost publications and events in each saved window's manifest, in order.
    fn saved_losses(&self) -> Vec<crate::Losses> {
        let mut manifests: Vec<_> = self
            .saved_windows()
            .iter()
            .map(|window| {
                let mut file = std::fs::File::open(window.join("ready/metadata")).unwrap();
                let metadata = CanonicalMetadata::read(&mut file).unwrap();
                let Body::Ordinary(manifest) = metadata.body else {
                    panic!("an ordinary window holds an ordinary body");
                };
                manifest
            })
            .collect();
        manifests.sort_by_key(|manifest| manifest.publication_first);
        manifests.iter().map(|manifest| manifest.losses).collect()
    }

    /// Takes the worker's own seal of the first window as far as the interruption, then stops
    /// as a process that ended there would, leaving what a torn write leaves.
    fn interrupted_seal(&self, store: &Store, interruption: Interruption) {
        let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
        let mut state = RunState::default();
        let mut reader = RingReader::open_generation(self.writer.name()).unwrap();
        let identity = WindowIdentity {
            run: self.run,
            window: 0,
        };
        let publication = reader.publication_first();
        let input =
            super::seal_input(store, &reader, &identity, publication, &state, &budget).unwrap();
        let attempt = store
            .register_attempt(&input, &mut reader, &budget)
            .unwrap();
        if matches!(interruption, Interruption::Registered) {
            return;
        }

        reader.seek_publication(publication).unwrap();
        reader.seek_event(input.losses.event_start).unwrap();
        let window = super::seal_stage(&attempt, &mut reader, identity, &[], &mut state).unwrap();
        let pending = self
            .root
            .join("data")
            .join(collector::recording::identity_text(attempt.transaction()).to_string())
            .join("pending");
        if Self::interrupted_seal_events(&pending, interruption) {
            return;
        }

        let derive = SealedApps::default();
        let writers = Writers {
            encoder: Encoder,
            derive: &derive,
        };
        let manifest = super::seal_write(&attempt, &writers, &window, &state).unwrap();
        drop(window);
        if Self::interrupted_seal_tables(&pending, interruption) {
            return;
        }

        let mut proof = attempt.prove_ordinary(&manifest, reader, &budget).unwrap();
        for _step in 0..1 << 20 {
            if proof.step().unwrap() == ProofStep::Complete {
                break;
            }
        }
        let mut publishing = proof.into_publication().unwrap();
        if let Interruption::PublicationSteps(count) = interruption {
            for _step in 0..count {
                if publishing.step().unwrap() == PublicationStep::Idle {
                    return;
                }
            }
        }
    }

    /// Leaves the events as the interruption does, and says whether the seal ends here.
    fn interrupted_seal_events(pending: &Path, interruption: Interruption) -> bool {
        match interruption {
            Interruption::EventIndexAbsent => {
                std::fs::remove_file(pending.join("events/index")).unwrap();
                true
            }
            Interruption::EventBatchTorn => {
                std::fs::remove_file(pending.join("events/index")).unwrap();
                Self::tear(&Self::only_event_batch(pending));
                true
            }
            Interruption::EventsStaged => true,
            _ => false,
        }
    }

    /// Leaves the tables as the interruption does, and says whether the seal ends here.
    fn interrupted_seal_tables(pending: &Path, interruption: Interruption) -> bool {
        match interruption {
            Interruption::TableMissing => {
                std::fs::remove_file(pending.join("processes.parquet")).unwrap();
                true
            }
            Interruption::TableTorn => {
                Self::tear(&pending.join("processes.parquet"));
                true
            }
            Interruption::TablesWritten => true,
            _ => false,
        }
    }

    /// Takes a recovery as far as staging the window again, then stops as a process would.
    fn interrupted_recovery(&self, store: &Store) {
        let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
        let mut state = RunState::default();
        let mut reader = RingReader::open_generation(self.writer.name()).unwrap();
        let mut attempt = store.resume_attempt().unwrap();
        let saved = attempt.saved(&budget).unwrap();
        attempt.restage(&saved).unwrap();
        let identity = WindowIdentity {
            run: self.run,
            window: 0,
        };

        reader.seek_publication(reader.publication_first()).unwrap();
        reader.seek_event(reader.event_first()).unwrap();
        super::seal_stage(&attempt, &mut reader, identity, &saved.batches, &mut state).unwrap();
    }

    fn only_event_batch(pending: &Path) -> PathBuf {
        let mut batches = std::fs::read_dir(pending.join("events"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| path.file_name().is_some_and(|name| name != "index"));
        let batch = batches.next().unwrap();
        assert!(batches.next().is_none());
        batch
    }

    /// Leaves the first half of a file, as a process that ended while writing it leaves.
    fn tear(path: &Path) {
        let length = std::fs::metadata(path).unwrap().len();
        assert!(length > 1);
        std::fs::OpenOptions::new()
            .write(true)
            .open(path)
            .unwrap()
            .set_len(length / 2)
            .unwrap();
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
        if !self.source_lost {
            collector::ring::unlink_generation(self.writer.name()).unwrap();
        }
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

    let refusal = result.err().unwrap();
    assert!(matches!(refusal, crate::Error::Io(error) if error.kind() == ErrorKind::NotFound));
}

/// Two ticks with events in the first minute, one in the second, and a third minute that closes
/// the second.
fn publish_three_minutes(recording: &mut Recording) {
    recording.publish_events_in_minute(0, 2);
    recording.publish_events_in_minute(0, 1);
    recording.publish_events_in_minute(1, 1);
    recording.publish_in_minute(2);
}

type SavedFiles = Vec<Vec<(PathBuf, Vec<u8>)>>;

/// What the two minutes come to with nothing in the way, which an interrupted seal must equal.
fn saved_without_interruption(mut recording: Recording) -> SavedFiles {
    publish_three_minutes(&mut recording);
    let store = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();
    recording.persist(&store, &mut state);
    recording.persist(&store, &mut state);
    recording.saved_files()
}

static UNINTERRUPTED: LazyLock<SavedFiles> =
    LazyLock::new(|| saved_without_interruption(Recording::new()));
static UNINTERRUPTED_CAPTURED: LazyLock<SavedFiles> =
    LazyLock::new(|| saved_without_interruption(Recording::captured()));

/// Runs the seal up to the interruption, restarts the worker, and takes two steps, which finish
/// the interrupted minute and then save the next one, exactly as if nothing had happened.
fn interrupted_seal_converges(
    mut recording: Recording,
    interruption: Interruption,
    uninterrupted: &SavedFiles,
) {
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, interruption);
    drop(first);
    let restarted = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();

    let finished = recording.persist(&restarted, &mut state);
    let next = recording.persist(&restarted, &mut state);

    assert!(matches!(finished, Progress::Sealed));
    assert!(matches!(next, Progress::Sealed));
    assert_eq!(recording.frontier(&restarted).unwrap().publication_end(), 4);
    assert_eq!(&recording.saved_files(), uninterrupted);
}

#[test]
fn a_seal_interrupted_after_registering_converges() {
    // Goal: a registration with nothing staged is finished at restart, not refused (P02.3).
    // Method: stop after registering, restart the worker, and compare with an uninterrupted run.
    interrupted_seal_converges(Recording::new(), Interruption::Registered, &UNINTERRUPTED);
}

#[test]
fn a_seal_interrupted_before_the_event_index_converges() {
    // Goal: a complete event batch without its index survives a restart (P02.3).
    // Method: stage the window, remove the index, and compare with an uninterrupted run.
    interrupted_seal_converges(
        Recording::new(),
        Interruption::EventIndexAbsent,
        &UNINTERRUPTED,
    );
}

#[test]
fn a_seal_interrupted_inside_an_event_batch_converges() {
    // Goal: a torn event batch holds no row a restart must keep, and is written again (P02.3).
    // Method: stage the window, tear its batch, and compare with an uninterrupted run.
    interrupted_seal_converges(
        Recording::new(),
        Interruption::EventBatchTorn,
        &UNINTERRUPTED,
    );
}

#[test]
fn a_seal_interrupted_after_staging_its_events_converges() {
    // Goal: complete events with no table yet are finished at restart (P02.3).
    // Method: stop after staging, restart the worker, and compare with an uninterrupted run.
    interrupted_seal_converges(Recording::new(), Interruption::EventsStaged, &UNINTERRUPTED);
}

#[test]
fn a_seal_interrupted_with_a_table_missing_converges() {
    // Goal: a partial table set is finished at restart (P02.3).
    // Method: write the tables, remove one, and compare with an uninterrupted run.
    interrupted_seal_converges(Recording::new(), Interruption::TableMissing, &UNINTERRUPTED);
}

#[test]
fn a_seal_interrupted_inside_a_table_converges() {
    // Goal: a torn table is written again at restart rather than refused (P02.3).
    // Method: write the tables, tear one, and compare with an uninterrupted run.
    interrupted_seal_converges(Recording::new(), Interruption::TableTorn, &UNINTERRUPTED);
}

#[test]
fn a_seal_interrupted_after_its_tables_converges() {
    // Goal: complete originals with no proof are proved and published at restart (P02.3).
    // Method: stop after the tables, restart the worker, and compare with an uninterrupted run.
    interrupted_seal_converges(
        Recording::new(),
        Interruption::TablesWritten,
        &UNINTERRUPTED,
    );
}

#[test]
fn a_seal_interrupted_after_its_proof_converges() {
    // Goal: durable proof metadata goes on to publication at restart (P02.3).
    // Method: stop after the proof, restart the worker, and compare with an uninterrupted run.
    interrupted_seal_converges(Recording::new(), Interruption::Proved, &UNINTERRUPTED);
}

#[test]
fn a_seal_interrupted_at_each_publication_step_converges() {
    // Goal: every durable step of a publication goes on to the same end at restart (P02.3).
    // Method: stop after each step count, restart the worker, and compare with an uninterrupted
    // run. The helper stops at Idle, so the last count covers a publication that has finished.
    for steps in 1..=PUBLICATION_STEPS {
        interrupted_seal_converges(
            Recording::new(),
            Interruption::PublicationSteps(steps),
            &UNINTERRUPTED,
        );
    }
}

#[test]
fn a_captured_seal_interrupted_before_the_event_index_converges() {
    // Goal: saved identities and programs survive a restart like the rest of a saved row.
    // Method: stage a captured window, remove the index, and compare with an uninterrupted run.
    interrupted_seal_converges(
        Recording::captured(),
        Interruption::EventIndexAbsent,
        &UNINTERRUPTED_CAPTURED,
    );
}

#[test]
fn a_captured_seal_interrupted_inside_a_table_converges() {
    // Goal: tables written again from a captured source equal the first writing.
    // Method: write a captured window's tables, tear one, and compare with an uninterrupted run.
    interrupted_seal_converges(
        Recording::captured(),
        Interruption::TableTorn,
        &UNINTERRUPTED_CAPTURED,
    );
}

#[test]
fn a_saved_event_batch_stays_when_its_minute_is_staged_again() {
    // Goal: a saved row outranks a fresh reading of it, so its batch is not written again.
    // Method: stage the window, remove the index and a table's worth of progress, restart, and
    // compare the batch's file identity before and after publication.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::EventIndexAbsent);
    drop(first);
    let window = recording.saved_windows().remove(0);
    let batch = Recording::only_event_batch(&window.join("pending"));
    let saved = std::fs::metadata(&batch).unwrap().ino();
    let restarted = Store::open(&recording.root).unwrap();

    recording.persist(&restarted, &mut RunState::default());

    let published = Recording::only_event_batch(&window.join("ready"));
    assert_eq!(std::fs::metadata(&published).unwrap().ino(), saved);
}

#[test]
fn complete_tables_stay_when_their_minute_is_recovered() {
    // Goal: tables that are whole are proved as they are rather than written again.
    // Method: stop after the tables, restart, and compare a table's file identity before and
    // after publication.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::TablesWritten);
    drop(first);
    let window = recording.saved_windows().remove(0);
    let saved = std::fs::metadata(window.join("pending/ticks.parquet"))
        .unwrap()
        .ino();
    let restarted = Store::open(&recording.root).unwrap();

    recording.persist(&restarted, &mut RunState::default());

    let published = std::fs::metadata(window.join("ready/ticks.parquet"))
        .unwrap()
        .ino();
    assert_eq!(published, saved);
}

#[test]
fn another_run_waits_while_an_interrupted_attempt_holds_the_slot() {
    // Goal: the run that does not own an interrupted attempt lets its owner finish first, since
    // the store has one slot and only the owner's source can finish it.
    // Method: interrupt a seal of one run, step a second run, then step the first and the second.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let mut other = recording.another_run();
    other.publish_in_minute(0);
    other.publish_in_minute(1);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::EventsStaged);
    drop(first);
    let restarted = Store::open(&recording.root).unwrap();
    let mut other_state = RunState::default();

    let deferred = other.persist(&restarted, &mut other_state);
    let finished = recording.persist(&restarted, &mut RunState::default());
    let continued = other.persist(&restarted, &mut other_state);

    assert!(matches!(deferred, Progress::Deferred));
    assert!(matches!(finished, Progress::Sealed));
    assert!(matches!(continued, Progress::Sealed));
}

#[test]
fn a_seal_interrupted_again_while_it_is_recovered_converges() {
    // Goal: recovery is itself interruptible and comes to the same end (P02.3).
    // Method: tear a table, stage the window again as a restart does, stop before its tables,
    // restart once more, and compare with an uninterrupted run.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::TableTorn);
    drop(first);
    let second = Store::open(&recording.root).unwrap();
    recording.interrupted_recovery(&second);
    drop(second);
    let restarted = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();

    recording.persist(&restarted, &mut state);
    recording.persist(&restarted, &mut state);

    assert_eq!(recording.saved_files(), *UNINTERRUPTED);
}

#[test]
fn a_seal_with_nothing_saved_counts_what_the_ring_overwrote_meanwhile() {
    // Goal: publications overwritten while the worker was down are counted once as lost (P02.3).
    // Method: stop after registering, publish past the ring's eight slots, restart, and read the
    // first saved window's counts, against the fourteen published and eight the ring holds.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::Registered);
    drop(first);
    recording.publish_minutes(3..13);
    let restarted = Store::open(&recording.root).unwrap();

    let progress = recording.persist(&restarted, &mut RunState::default());

    assert!(matches!(progress, Progress::Sealed));
    let losses = recording.saved_losses();
    assert_eq!(losses.len(), 1);
    assert_eq!(losses[0].publications, 6);
    assert_eq!(losses[0].events, 0);
    let objects = std::fs::read_dir(recording.root.join("data")).unwrap();
    assert_eq!(objects.count(), 1);
}

#[test]
fn a_seal_that_saved_tables_and_no_rows_is_finished_rather_than_abandoned() {
    // Goal: a minute with no events saves no rows, so its tables alone keep the attempt (P02.3).
    // Method: stop with all but one table written on such a minute, restart, read the frontier.
    let mut recording = Recording::new();
    recording.publish_minutes(0..2);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::TableMissing);
    drop(first);
    let restarted = Store::open(&recording.root).unwrap();

    let progress = recording.persist(&restarted, &mut RunState::default());

    assert!(matches!(progress, Progress::Sealed));
    assert_eq!(recording.frontier(&restarted).unwrap().publication_end(), 2);
}

#[test]
fn a_seal_abandoned_up_to_its_slot_is_abandoned_again() {
    // Goal: a process that ends after removing an empty attempt's directories, before its slot,
    // stops nothing, since a restart finds the same empty attempt (P02.3).
    // Method: stop after registering, remove those directories, restart, and read what remains.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::Registered);
    drop(first);
    for object in std::fs::read_dir(recording.root.join("data")).unwrap() {
        std::fs::remove_dir_all(object.unwrap().path()).unwrap();
    }
    let restarted = Store::open(&recording.root).unwrap();

    let progress = recording.persist(&restarted, &mut RunState::default());

    assert!(matches!(progress, Progress::Sealed));
    assert_eq!(recording.frontier(&restarted).unwrap().publication_end(), 3);
}

#[test]
fn a_seal_with_nothing_saved_lets_another_run_go_on_when_its_source_is_gone() {
    // Goal: a reboot during a seal that saved nothing cannot stop new minutes (P02.3).
    // Method: stop after registering, remove the source as a reboot does, and step a new run.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let mut other = recording.another_run();
    other.publish_in_minute(0);
    other.publish_in_minute(1);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::Registered);
    drop(first);
    recording.lose_source();
    let restarted = Store::open(&recording.root).unwrap();

    let progress = other.persist(&restarted, &mut RunState::default());

    assert!(matches!(progress, Progress::Sealed));
}

#[test]
fn a_torn_event_batch_counts_as_nothing_saved_when_its_source_is_gone() {
    // Goal: a batch no row can be read from does not hold the slot once the source is gone.
    // Method: tear the only batch, remove the source, and step a new run.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let mut other = recording.another_run();
    other.publish_in_minute(0);
    other.publish_in_minute(1);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::EventBatchTorn);
    drop(first);
    recording.lose_source();
    let restarted = Store::open(&recording.root).unwrap();

    let progress = other.persist(&restarted, &mut RunState::default());

    assert!(matches!(progress, Progress::Sealed));
}

#[test]
fn a_proved_seal_publishes_when_its_source_is_gone() {
    // Goal: durable proof metadata needs no source, so a reboot after it loses nothing (P02.3).
    // Method: stop after the proof, remove the source, restart, and read the frontier.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::Proved);
    drop(first);
    recording.lose_source();
    let restarted = Store::open(&recording.root).unwrap();

    let progress = recording.persist(&restarted, &mut RunState::default());

    assert!(matches!(progress, Progress::Sealed));
    assert_eq!(recording.frontier(&restarted).unwrap().publication_end(), 3);
}
