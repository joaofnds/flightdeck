use super::super::tests::forged_source::{ForgedWord, WritableSource};
use super::super::{Outcome, Progress, RunState, WindowIdentity, Writers, persist_one, run_paced};
use crate::canonical::metadata::{Body, Lifecycle};
use crate::canonical::{CanonicalMetadata, ReadWork, SourceFrontier};
use crate::resources::ByteBudget;
use crate::sealed_apps::SealedApps;
use crate::store::{ProofStep, PublicationStep, RegisteredAttempt};
use crate::transaction::{AttemptOperation, UnknownProvenance, UnknownReason};
use crate::{Encoder, Store};
use collector::recording::private::{self, FileLock, PrivateDirectory};
use collector::recording::{RegisteredRun, Registry, bootstrap};
use collector::ring::{Identity, Place, RingReader, RunIdentity, Shape, Writer};
use std::io::ErrorKind;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

static NEXT: AtomicU64 = AtomicU64::new(0);

/// The steps a publication takes from a complete proof to an idle slot.
const PUBLICATION_STEPS: u32 = 19;

/// A minute whose clocks run past any start of this test process, so its pid reads as the
/// collector that published it.
const LIVE_MINUTE: u64 = 1 << 20;

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

/// The header word that holds the newest publication's number, in every ring layout.
const HEADER_PUBLICATION_NEWEST: usize = 4;

/// The window a seal was interrupted on, and what scanning to it found lost.
struct InterruptedWindow {
    number: u64,
    publication: u64,
    publications_lost: u64,
    /// Whether the attempt registered with a mark of unknown extent.
    unknown: bool,
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
    withheld: Option<WritableSource>,
}

impl Recording {
    fn new() -> Self {
        Self::create(false, shape())
    }

    /// A recording whose collector captures each process's identity and program.
    fn captured() -> Self {
        Self::create(true, shape())
    }

    /// A recording whose ring holds this many events.
    fn with_event_capacity(event_capacity: u64) -> Self {
        Self::create(
            false,
            Shape {
                event_capacity,
                ..shape()
            },
        )
    }

    fn create(captured: bool, shape: Shape) -> Self {
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
            withheld: None,
        }
    }

    /// Replaces the source with a copy that hides every publication past `visible`, as a writer
    /// that died after writing its tick's events and before publishing the tick leaves it.
    fn withhold_publications_past(&mut self, visible: u64) {
        let reader = RingReader::open_generation(self.writer.name()).unwrap();
        self.withheld = Some(WritableSource::new(
            self.writer.name(),
            reader.mapped_size_bytes(),
            ForgedWord {
                watched: HEADER_PUBLICATION_NEWEST,
                expected: reader.publication_newest().unwrap(),
                index: HEADER_PUBLICATION_NEWEST,
                value: visible,
            },
        ));
    }

    fn ring_name(&self) -> &str {
        match &self.withheld {
            Some(copy) => &copy.name,
            None => self.writer.name(),
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
            ring_name: self.ring_name().to_owned(),
            collector_pid: 0,
        }
    }

    fn persist(&self, store: &Store, state: &mut RunState) -> Progress {
        self.persist_result(store, state).unwrap()
    }

    /// Takes a worker step with this test process standing in for a collector that still runs.
    fn persist_live(&self, store: &Store, state: &mut RunState) -> Progress {
        let derive = SealedApps::default();
        let writers = Writers {
            encoder: Encoder,
            derive: &derive,
        };
        let registered = RegisteredRun {
            collector_pid: std::process::id(),
            ..self.registered()
        };
        persist_one(store, &writers, &registered, false, state).unwrap()
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
        self.withheld = None;
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
        let manifests = self.saved_manifests();
        manifests.iter().map(|manifest| manifest.losses).collect()
    }

    /// The event rows each saved window holds, in order.
    fn saved_event_rows(&self) -> Vec<u64> {
        let manifests = self.saved_manifests();
        let rows = manifests.iter().map(|manifest| {
            let events = manifest.tables.iter().find(|table| table.name == "events");
            events.unwrap().rows
        });
        rows.collect()
    }

    /// Every saved window's manifest, in the order of its publications.
    fn saved_manifests(&self) -> Vec<crate::Manifest> {
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
        manifests
    }

    /// What each saved window names as the source of its unknown extent, in publication order.
    fn saved_unknown_provenance(&self) -> Vec<Option<UnknownProvenance>> {
        let mut named: Vec<_> = self
            .saved_windows()
            .iter()
            .map(|window| {
                let mut file = std::fs::File::open(window.join("ready/metadata")).unwrap();
                let metadata = CanonicalMetadata::read(&mut file).unwrap();
                let Body::Ordinary(manifest) = metadata.body else {
                    panic!("an ordinary window holds an ordinary body");
                };
                (manifest.publication_first, metadata.loss.inherited.unknown)
            })
            .collect();
        named.sort_by_key(|(publication, _)| *publication);
        named.into_iter().map(|(_, unknown)| unknown).collect()
    }

    /// Takes the worker's own seal of the first window as far as the interruption, then stops
    /// as a process that ended there would, leaving what a torn write leaves.
    fn interrupted_seal(&self, store: &Store, interruption: Interruption) {
        let attempted = InterruptedWindow {
            number: 0,
            publication: 1,
            publications_lost: 0,
            unknown: false,
        };
        self.interrupted_seal_of(store, &attempted, interruption);
    }

    /// The same for the named window, whose seal the worker reached past those lost publications.
    fn interrupted_seal_of(
        &self,
        store: &Store,
        attempted: &InterruptedWindow,
        interruption: Interruption,
    ) {
        let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
        let mut state = RunState::default();
        state.losses.publications = attempted.publications_lost;
        state.losses.unknown = attempted.unknown;
        let mut reader = RingReader::open_generation(self.writer.name()).unwrap();
        let identity = WindowIdentity {
            run: self.run,
            window: attempted.number,
        };
        let publication = attempted.publication;
        let input = super::seal_input(store, &reader, &identity, publication, &state, &budget)
            .unwrap()
            .0;
        let attempt = store
            .register_attempt(&input, &mut reader, &budget)
            .unwrap();
        if matches!(interruption, Interruption::Registered) {
            return;
        }

        reader.seek_publication(publication).unwrap();
        reader.seek_event(input.losses.event_start).unwrap();
        let window =
            super::seal_stage(&attempt, &mut reader, identity, &[], &mut state, &budget).unwrap();
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

        let proof = attempt.prove_ordinary(&manifest, reader, &budget).unwrap();
        Self::interrupted_publication(proof, interruption);
    }

    /// Takes the proof as far as the interruption, then stops as a process that ended there would.
    fn interrupted_publication(
        mut proof: crate::store::NormalProof<'_>,
        interruption: Interruption,
    ) {
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

    /// Takes the worker's own terminal seal as far as the interruption, then stops the same way.
    fn interrupted_terminal(&self, store: &Store, interruption: Interruption) {
        let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
        let state = RunState::default();
        let registered = self.registered();
        let mut reader = RingReader::open_generation(&registered.ring_name).unwrap();
        reader.set_writer_gone();
        let input = super::seal_terminal_input(store, &registered, &reader, &state, &budget)
            .unwrap()
            .0;
        let AttemptOperation::Terminal { event_end } = input.operation else {
            panic!("a terminal seal has a terminal operation");
        };
        let attempt = store
            .register_attempt(&input, &mut reader, &budget)
            .unwrap();
        if matches!(interruption, Interruption::Registered) {
            return;
        }

        let manifest = crate::event_recovery::write_tail_into(
            reader,
            &registered.identity,
            input.losses.event_start..event_end,
            state.losses,
            &budget,
            |index| Ok(attempt.create_event_batch(u32::try_from(index).unwrap())?),
        )
        .unwrap();
        let pending = self
            .root
            .join("recovery-data")
            .join(collector::recording::identity_text(attempt.transaction()).to_string())
            .join("pending");
        match interruption {
            Interruption::EventsStaged => return,
            Interruption::EventBatchTorn => {
                Self::tear(&pending.join(crate::event_recovery::batch_name(0)));
                return;
            }
            _ => {}
        }

        let mut reader = RingReader::open_generation(&registered.ring_name).unwrap();
        reader.set_writer_gone();
        let proof = attempt
            .prove_terminal(&manifest.recovery, &manifest.batches, reader, &budget)
            .unwrap();
        Self::interrupted_publication(proof, interruption);
    }

    /// The only terminal publication's metadata, read back through the canonical codec.
    fn saved_terminal(&self) -> CanonicalMetadata {
        let mut objects = std::fs::read_dir(self.root.join("recovery-data")).unwrap();
        let object = objects.next().unwrap().unwrap().path();
        assert!(objects.next().is_none());
        let mut file = std::fs::File::open(object.join("ready/metadata")).unwrap();
        CanonicalMetadata::read(&mut file).unwrap()
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
        super::seal_stage(
            &attempt,
            &mut reader,
            identity,
            &saved.batches,
            &mut state,
            &budget,
        )
        .unwrap();
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

#[test]
fn a_worker_saves_its_next_minute_without_reading_the_minute_it_just_saved() {
    // Goal: a minute costs what it saves and not the rows of the one before, which this process
    // proved before it published them (P02.4).
    // Method: seal a minute, remove its tables, and seal the next with the same Store and state.
    let mut recording = Recording::new();
    for minute in 0..3 {
        recording.publish_in_minute(minute);
    }
    let store = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();
    recording.persist(&store, &mut state);
    assert!(Recording::remove_tables(&recording.saved_windows()) > 0);

    let progress = recording.persist(&store, &mut state);

    assert!(matches!(progress, Progress::Sealed));
    assert_eq!(recording.frontier(&store).unwrap().publication_end(), 3);
}

#[test]
fn a_worker_finishes_a_run_without_reading_the_minute_it_just_saved() {
    // Goal: a run's end costs what its tail holds and not the rows of its last minute (P02.4).
    // Method: seal a finished run's only minute, remove its tables, and take the last step.
    let mut recording = Recording::new();
    recording.publish_in_minute(0);
    let store = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();
    assert!(matches!(
        recording.persist(&store, &mut state),
        Progress::Sealed
    ));
    assert!(Recording::remove_tables(&recording.saved_windows()) > 0);

    let progress = recording.persist(&store, &mut state);

    assert!(matches!(progress, Progress::Complete));
}

#[test]
fn a_worker_refuses_a_saved_minute_whose_proof_another_file_replaced() {
    // Goal: leaving a saved minute's rows unread leaves the identity of its proof checked (P02.5).
    // Method: seal a minute, swap its proof for a private copy of the same bytes, and take a step.
    let mut recording = Recording::new();
    for minute in 0..3 {
        recording.publish_in_minute(minute);
    }
    let store = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();
    recording.persist(&store, &mut state);
    let proof = recording.saved_windows()[0].join("ready/metadata");
    let bytes = std::fs::read(&proof).unwrap();
    std::fs::remove_file(&proof).unwrap();
    let mut copy = private::open_private_file(&proof, true).unwrap();
    std::io::Write::write_all(&mut copy, &bytes).unwrap();

    let refusal = recording.persist_result(&store, &mut state).err().unwrap();

    assert!(matches!(refusal, crate::Error::Io(error) if error.kind() == ErrorKind::InvalidData));
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
    // Method: stop after every third step, since the kill tests end a worker at each change a step
    // makes, restart the worker, and compare with an uninterrupted run.
    for steps in (1..PUBLICATION_STEPS).step_by(3) {
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

/// One minute saved, then published past the ring's eight ticks and eight events with as many
/// events as the ring lets a tick carry, up to three. The ring ends with publications 7 to 14 and
/// events 8 to 15, so publications 3 to 6 and events 4 to 7 are gone, and event 8 outlived its
/// publication.
fn lagging_recording() -> Recording {
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.persist(&first, &mut RunState::default());
    drop(first);
    for minute in 3..13 {
        let count = recording.writer.event_room().min(3);
        recording.publish_events_in_minute(minute, count);
    }
    let reader = RingReader::open_generation(recording.writer.name()).unwrap();
    assert_eq!(reader.publication_newest().unwrap(), 14);
    assert_eq!(reader.event_newest().unwrap(), 15);

    recording
}

#[test]
fn what_the_ring_overwrote_past_the_frontier_is_counted_once_across_restarts() {
    // Goal: publications and events overwritten while the worker lagged are counted as lost
    // exactly once, whichever process saves them, and every other event is saved (P02.3).
    // Method: save each ready minute of a lagging recording with a new Store and state, as
    // restarts do. Publication 14 has no later one to end its minute, so events 1 to 12 end up
    // accounted for across eight windows.
    let recording = lagging_recording();

    for _window in 0..7 {
        let restarted = Store::open(&recording.root).unwrap();
        let progress = recording.persist(&restarted, &mut RunState::default());
        assert!(matches!(progress, Progress::Sealed));
    }

    let losses = recording.saved_losses();
    let publications: Vec<u64> = losses.iter().map(|loss| loss.publications).collect();
    let events: Vec<u64> = losses.iter().map(|loss| loss.events).collect();
    assert_eq!(publications, [0, 4, 0, 0, 0, 0, 0, 0]);
    assert_eq!(events, [0, 4, 0, 0, 0, 0, 0, 0]);
    assert_eq!(recording.saved_event_rows(), [3, 1, 0, 0, 2, 1, 1, 0]);
    let reopened = Store::open(&recording.root).unwrap();
    let frontier = recording.frontier(&reopened).unwrap();
    assert_eq!(frontier.publication_end(), 14);
    assert_eq!(frontier.event_end(), 13);
}

/// Stops the seal of a lagging recording's next window at the interruption, restarts, and expects
/// the same counts and rows as a seal nothing interrupted.
fn interrupted_lagging_seal_counts_its_losses_once(interruption: Interruption) {
    let recording = lagging_recording();
    let attempted = InterruptedWindow {
        number: 5,
        publication: 7,
        publications_lost: 4,
        unknown: false,
    };
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal_of(&first, &attempted, interruption);
    drop(first);
    let restarted = Store::open(&recording.root).unwrap();

    let progress = recording.persist(&restarted, &mut RunState::default());

    assert!(matches!(progress, Progress::Sealed));
    let lagging = crate::Losses {
        publications: 4,
        events: 4,
        unknown: false,
    };
    assert_eq!(
        recording.saved_losses(),
        [crate::Losses::default(), lagging]
    );
    assert_eq!(recording.saved_event_rows(), [3, 1]);
    assert_eq!(recording.frontier(&restarted).unwrap().event_end(), 9);
}

#[test]
fn a_seal_past_lost_events_interrupted_before_its_event_index_counts_them_once() {
    // Goal: events lost before a batch that was saved stay counted after a restart (P02.3).
    // Method: stop with the batch saved and no index, restart, and compare the counts.
    interrupted_lagging_seal_counts_its_losses_once(Interruption::EventIndexAbsent);
}

#[test]
fn a_seal_past_lost_events_interrupted_with_a_table_missing_counts_them_once() {
    // Goal: staging such a window again keeps its batch and its counts (P02.3).
    // Method: stop with all but one table written, restart, and compare the counts.
    interrupted_lagging_seal_counts_its_losses_once(Interruption::TableMissing);
}

#[test]
fn a_seal_past_lost_events_interrupted_after_its_tables_counts_them_once() {
    // Goal: proving such a window from what it saved finds the same counts (P02.3).
    // Method: stop with every table written, restart, and compare the counts.
    interrupted_lagging_seal_counts_its_losses_once(Interruption::TablesWritten);
}

#[test]
fn a_seal_past_lost_events_abandoned_with_a_torn_batch_counts_them_once() {
    // Goal: an attempt that saved no row is abandoned and the next scan counts the same (P02.3).
    // Method: tear the only batch, restart, and compare the counts.
    interrupted_lagging_seal_counts_its_losses_once(Interruption::EventBatchTorn);
}

#[test]
fn a_seal_past_lost_events_interrupted_after_its_proof_counts_them_once() {
    // Goal: durable metadata carries the counts through publication (P02.3).
    // Method: stop after the proof, restart, and compare the counts.
    interrupted_lagging_seal_counts_its_losses_once(Interruption::Proved);
}

#[test]
fn a_seal_with_no_rows_past_lost_events_counts_them_when_staged_again() {
    // Goal: events lost before a window that saved no row are counted, not taken for the window
    // being overwritten, when the window is staged again (P02.3).
    // Method: push event 8 out of the ring too, so the next window, publication 8, saves no row
    // and loses publications 3 to 7 and events 4 to 8, then stop with a table missing and restart.
    let mut recording = lagging_recording();
    let count = recording.writer.event_room();
    recording.publish_events_in_minute(13, count);
    let attempted = InterruptedWindow {
        number: 6,
        publication: 8,
        publications_lost: 5,
        unknown: false,
    };
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal_of(&first, &attempted, Interruption::TableMissing);
    drop(first);
    let restarted = Store::open(&recording.root).unwrap();

    let progress = recording.persist(&restarted, &mut RunState::default());

    assert!(matches!(progress, Progress::Sealed));
    let lagging = crate::Losses {
        publications: 5,
        events: 5,
        unknown: false,
    };
    assert_eq!(
        recording.saved_losses(),
        [crate::Losses::default(), lagging]
    );
    assert_eq!(recording.saved_event_rows(), [3, 0]);
    assert_eq!(recording.frontier(&restarted).unwrap().event_end(), 9);
}

#[test]
fn a_seal_with_rows_saved_refuses_when_the_ring_overwrote_its_window() {
    // Goal: rows no boundary can be proved for stop the run rather than publish a wrong window.
    // Method: stop with the batch saved and no index, publish past the ring's eight ticks with
    // no events, restart, and read the refusal.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_seal(&first, Interruption::EventIndexAbsent);
    drop(first);
    recording.publish_minutes(3..13);
    let restarted = Store::open(&recording.root).unwrap();

    let result = recording.persist_result(&restarted, &mut RunState::default());

    let refusal = result.err().unwrap();
    assert!(matches!(
        refusal,
        crate::Error::Bound("interrupted window overwritten")
    ));
}

#[test]
fn a_finished_run_certifies_its_end_after_its_last_minute() {
    // Goal: a finished run ends in a canonical terminal publication, so whichever process comes
    // next knows it is complete rather than rereading its source (P01.5, P02.3).
    // Method: finish the writer after three minutes, step the worker past them, read the frontier
    // through a new Store, then step a new worker, which must find nothing to do.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    recording.writer.finish();
    let store = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();
    for _minute in 0..3 {
        assert!(matches!(
            recording.persist(&store, &mut state),
            Progress::Sealed
        ));
    }

    let progress = recording.persist(&store, &mut state);

    assert!(matches!(progress, Progress::Complete));
    drop(store);
    let reopened = Store::open(&recording.root).unwrap();
    let frontier = recording.frontier(&reopened).unwrap();
    assert!(frontier.complete());
    assert_eq!(frontier.publication_end(), 5);
    assert_eq!(frontier.event_end(), 5);
    let next = recording.persist(&reopened, &mut RunState::default());
    assert!(matches!(next, Progress::Complete));
    let again = recording.persist(&reopened, &mut state);
    assert!(matches!(again, Progress::Complete));
}

#[test]
fn a_run_finished_before_its_first_publication_certifies_its_end() {
    // Goal: an empty run is complete too, with no invented window (P02.3).
    // Method: finish the writer with nothing published, step the worker, read the frontier.
    let mut recording = Recording::new();
    recording.writer.finish();
    let store = Store::open(&recording.root).unwrap();

    let progress = recording.persist(&store, &mut RunState::default());

    assert!(matches!(progress, Progress::Complete));
    let frontier = recording.frontier(&store).unwrap();
    assert!(frontier.complete());
    assert_eq!(frontier.publication_end(), 1);
    assert_eq!(frontier.event_end(), 1);
    assert!(recording.saved_windows().is_empty());
    assert_eq!(
        recording.saved_terminal().lifecycle,
        Lifecycle::AcquiredCompletion
    );
}

#[test]
fn a_live_run_waits_at_its_newest_minute_until_it_finishes() {
    // Goal: the last minute of a running collector stays open, and so does its end (P01.2).
    // Method: with a live pid and clocks past this process's start, step over an empty ring,
    // then two minutes, then finish the writer and step to the end.
    let mut recording = Recording::new();
    let store = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();
    assert!(matches!(
        recording.persist_live(&store, &mut state),
        Progress::Waiting
    ));
    recording.publish_minutes(LIVE_MINUTE..LIVE_MINUTE + 2);

    let sealed = recording.persist_live(&store, &mut state);
    let waiting = recording.persist_live(&store, &mut state);
    recording.writer.finish();
    let last = recording.persist_live(&store, &mut state);
    let complete = recording.persist_live(&store, &mut state);

    assert!(matches!(sealed, Progress::Sealed));
    assert!(matches!(waiting, Progress::Waiting));
    assert!(matches!(last, Progress::Sealed));
    assert!(matches!(complete, Progress::Complete));
    assert_eq!(
        recording.saved_terminal().lifecycle,
        Lifecycle::AcquiredCompletion
    );
}

#[test]
fn a_run_whose_writer_died_saves_its_last_minute_and_certifies_its_end() {
    // Goal: a crashed collector costs nothing the ring holds, and its run still ends (P01.2).
    // Method: publish without finishing, step with a pid that is gone until the run completes,
    // and read that the terminal names a dead generation rather than a completion.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let store = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();
    for _minute in 0..3 {
        assert!(matches!(
            recording.persist(&store, &mut state),
            Progress::Sealed
        ));
    }

    let progress = recording.persist(&store, &mut state);

    assert!(matches!(progress, Progress::Complete));
    let frontier = recording.frontier(&store).unwrap();
    assert!(frontier.complete());
    assert_eq!(frontier.publication_end(), 5);
    assert_eq!(frontier.event_end(), 5);
    assert_eq!(
        recording.saved_terminal().lifecycle,
        Lifecycle::DeadGeneration
    );
}

#[test]
fn a_crashed_runs_last_minute_finds_its_end_when_staged_again() {
    // Goal: an interrupted last minute of a crashed collector still ends the run (P01.2).
    // Method: save two minutes, stop the third with a table torn, and restart with the pid gone,
    // which takes reopening the ring, then read the run through to its certified end.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let first = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();
    recording.persist(&first, &mut state);
    recording.persist(&first, &mut state);
    let last = InterruptedWindow {
        number: 2,
        publication: 4,
        publications_lost: 0,
        unknown: false,
    };
    recording.interrupted_seal_of(&first, &last, Interruption::TableTorn);
    drop(first);
    let restarted = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();

    let finished = recording.persist(&restarted, &mut state);
    let certified = recording.persist(&restarted, &mut state);

    assert!(matches!(finished, Progress::Sealed));
    assert!(matches!(certified, Progress::Complete));
    assert_eq!(
        recording.saved_terminal().lifecycle,
        Lifecycle::DeadGeneration
    );
}

#[test]
fn a_certified_run_stays_complete_when_its_source_is_gone() {
    // Goal: a certified end needs no source, so a reboot before retiring the run stops nothing.
    // Method: finish a run, certify it, remove its source, and step a new worker.
    let mut recording = Recording::new();
    recording.writer.finish();
    let first = Store::open(&recording.root).unwrap();
    recording.persist(&first, &mut RunState::default());
    drop(first);
    recording.lose_source();
    let restarted = Store::open(&recording.root).unwrap();

    let progress = recording.persist(&restarted, &mut RunState::default());

    assert!(matches!(progress, Progress::Complete));
}

/// A writer that died after writing two events of its first tick, before publishing the tick.
fn recording_with_withheld_events() -> Recording {
    let mut recording = Recording::new();
    recording.publish_events_in_minute(0, 2);
    recording.withhold_publications_past(0);
    recording
}

#[test]
fn a_captured_run_keeps_the_identities_of_events_its_writer_never_published() {
    // Goal: the terminal of a captured run saves each event's identity with its row (P02.5).
    // Method: withhold the only publication of a captured ring, step once, read the terminal.
    let mut recording = Recording::captured();
    recording.publish_events_in_minute(0, 2);
    recording.withhold_publications_past(0);
    let store = Store::open(&recording.root).unwrap();

    let progress = recording.persist(&store, &mut RunState::default());

    assert!(matches!(progress, Progress::Complete));
    let terminal = recording.saved_terminal();
    let Body::Events(events) = terminal.body else {
        panic!("a terminal holds events");
    };
    assert!(events.identities);
    assert_eq!(events.recovery.rows, 2);
}

#[test]
fn a_terminal_with_rows_saved_and_no_source_holds_the_slot() {
    // Goal: rows no proof covers are neither published nor dropped once the source is gone.
    // Method: stop with the batch whole and no proof, remove the source, restart, and step this
    // run and another, which both wait.
    let mut recording = recording_with_withheld_events();
    let mut other = recording.another_run();
    other.publish_in_minute(0);
    other.publish_in_minute(1);
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_terminal(&first, Interruption::EventsStaged);
    drop(first);
    recording.lose_source();
    let restarted = Store::open(&recording.root).unwrap();

    let own = recording.persist(&restarted, &mut RunState::default());
    let next = other.persist(&restarted, &mut RunState::default());

    assert!(matches!(own, Progress::Unavailable));
    assert!(matches!(next, Progress::Deferred));
}

/// Stops the terminal seal of two withheld events at the interruption, restarts, and expects the
/// same end as a seal nothing interrupted.
fn interrupted_terminal_converges(interruption: Interruption) {
    let recording = recording_with_withheld_events();
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_terminal(&first, interruption);
    drop(first);
    let restarted = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();

    let mut progress = recording.persist(&restarted, &mut state);
    if matches!(progress, Progress::Sealed) {
        progress = recording.persist(&restarted, &mut state);
    }

    assert!(matches!(progress, Progress::Complete));
    let frontier = recording.frontier(&restarted).unwrap();
    assert!(frontier.complete());
    assert_eq!(frontier.event_end(), 3);
    let terminal = recording.saved_terminal();
    let Body::Events(events) = terminal.body else {
        panic!("a terminal holds events");
    };
    assert_eq!(events.recovery.rows, 2);
    assert_eq!(events.recovery.losses, crate::Losses::default());
}

#[test]
fn events_a_dying_writer_never_published_are_saved_in_the_terminal() {
    // Goal: the worker saves events past the last publication and certifies them (P02.3).
    // Method: withhold the only publication, step once, and read the terminal's rows.
    let recording = recording_with_withheld_events();
    let store = Store::open(&recording.root).unwrap();

    let progress = recording.persist(&store, &mut RunState::default());

    assert!(matches!(progress, Progress::Complete));
    let terminal = recording.saved_terminal();
    assert_eq!(terminal.lifecycle, Lifecycle::DeadGeneration);
    let Body::Events(events) = terminal.body else {
        panic!("a terminal holds events");
    };
    assert_eq!(events.recovery.rows, 2);
    assert_eq!(recording.frontier(&store).unwrap().event_end(), 3);
}

#[test]
fn a_terminal_seal_interrupted_after_registering_converges() {
    // Goal: a process ending before any terminal row costs nothing (P02.3).
    // Method: stop after registering, restart, and read the terminal.
    interrupted_terminal_converges(Interruption::Registered);
}

#[test]
fn a_terminal_seal_interrupted_inside_its_rows_converges() {
    // Goal: a torn terminal batch is written again from the finished source (P02.3).
    // Method: tear the only batch, restart, and read the terminal.
    interrupted_terminal_converges(Interruption::EventBatchTorn);
}

#[test]
fn a_terminal_seal_interrupted_with_its_rows_saved_converges() {
    // Goal: rows with no proof are written again rather than trusted (P02.3).
    // Method: stop with the batch whole and no proof, restart, and read the terminal.
    interrupted_terminal_converges(Interruption::EventsStaged);
}

#[test]
fn a_terminal_seal_interrupted_after_its_proof_converges() {
    // Goal: durable terminal metadata finishes publishing at restart (P02.3).
    // Method: stop after the proof, restart, and read the terminal.
    interrupted_terminal_converges(Interruption::Proved);
}

#[test]
fn a_terminal_seal_interrupted_at_each_publication_step_converges() {
    // Goal: every durable step of a terminal publication goes on to the same end (P02.3).
    // Method: stop after every third step, since the kill tests end a worker at each change a step
    // makes, restart, and read the terminal. The helper stops at Idle.
    for steps in (1..=PUBLICATION_STEPS).step_by(3) {
        interrupted_terminal_converges(Interruption::PublicationSteps(steps));
    }
}

#[test]
fn a_proved_terminal_publishes_when_its_source_is_gone() {
    // Goal: durable terminal metadata needs no source, so a reboot after it loses nothing.
    // Method: stop after the proof, remove the source, restart, and read the frontier.
    let mut recording = recording_with_withheld_events();
    let first = Store::open(&recording.root).unwrap();
    recording.interrupted_terminal(&first, Interruption::Proved);
    drop(first);
    recording.lose_source();
    let restarted = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();

    let finished = recording.persist(&restarted, &mut state);
    let progress = recording.persist(&restarted, &mut state);

    assert!(matches!(finished, Progress::Sealed));
    assert!(matches!(progress, Progress::Complete));
    assert_eq!(recording.frontier(&restarted).unwrap().event_end(), 3);
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

/// Runs the whole worker over the runs the registry names, oldest first, until it exits.
fn whole_worker(root: &Path, runs: Vec<RegisteredRun>, pause: &mut dyn FnMut(Duration)) -> Outcome {
    let registry = Registry { runs, evictions: 0 };

    whole_worker_of(root, &registry, pause)
}

fn whole_worker_of(root: &Path, registry: &Registry, pause: &mut dyn FnMut(Duration)) -> Outcome {
    registry.write(root).unwrap();

    run_paced(
        root,
        crate::resources::WORKING_BYTES_MAX,
        &SealedApps::default(),
        pause,
        None,
    )
    .unwrap()
}

/// Takes a worker step for every saved minute until the worker has nothing ready, and says how
/// many it saved. A finished source ends in its terminal step, which saves no window.
fn persist_until_quiet(recording: &Recording, store: &Store, state: &mut RunState) -> usize {
    let mut steps = 0;
    while matches!(recording.persist(store, state), Progress::Sealed) {
        steps += 1;
        assert!(steps < 32, "the worker kept finding work");
    }

    steps
}

#[test]
fn a_selection_overwritten_before_registering_is_counted_once_by_the_next_scan() {
    // Goal: a worker catching up on a full ring selects the oldest retained publication, which the
    // collector can overwrite before the attempt registers. That costs the minute, never the
    // worker, and the loss is counted once (P02.3).
    // Method: seal a selection the ring has already overwritten, as that race leaves it, then run
    // the worker to the end and read each saved minute's losses.
    let mut recording = Recording::new();
    recording.publish_minutes(0..10);
    recording.writer.finish();
    let store = Store::open(&recording.root).unwrap();
    let derive = SealedApps::default();
    let writers = Writers {
        encoder: Encoder,
        derive: &derive,
    };
    let mut state = RunState {
        next_publication: 1,
        loaded: true,
        ..RunState::default()
    };
    let selection = super::Selection {
        identity: WindowIdentity {
            run: recording.run,
            window: 0,
        },
        publication: 1,
    };
    let reader = RingReader::open_generation(recording.writer.name()).unwrap();
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));

    super::seal(&store, &writers, reader, selection, &mut state, &budget).unwrap();

    assert_eq!(state.losses, crate::Losses::default());
    assert!(store.interrupted().unwrap().is_none());
    persist_until_quiet(&recording, &store, &mut state);
    let lost: Vec<_> = recording
        .saved_losses()
        .iter()
        .map(|l| l.publications)
        .collect();
    assert_eq!(lost, [2, 0, 0, 0, 0, 0, 0, 0]);
}

#[test]
fn a_selection_overwritten_after_registering_is_counted_once_by_the_next_scan() {
    // Goal: the same race a moment later leaves an attempt that names a publication the ring no
    // longer holds, with nothing saved, so it clears at once rather than holding the slot (P02.3).
    // Method: register the first minute, overwrite it, stage through the worker, then run the
    // worker to the end and read the losses.
    let mut recording = Recording::new();
    recording.publish_minutes(0..2);
    let store = Store::open(&recording.root).unwrap();
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let derive = SealedApps::default();
    let writers = Writers {
        encoder: Encoder,
        derive: &derive,
    };
    let mut state = RunState {
        next_publication: 1,
        loaded: true,
        ..RunState::default()
    };
    let selection = super::Selection {
        identity: WindowIdentity {
            run: recording.run,
            window: 0,
        },
        publication: 1,
    };
    let mut reader = RingReader::open_generation(recording.writer.name()).unwrap();
    let input = super::seal_input(&store, &reader, &selection.identity, 1, &state, &budget)
        .unwrap()
        .0;
    let attempt = store
        .register_attempt(&input, &mut reader, &budget)
        .unwrap();
    recording.publish_minutes(2..10);
    recording.writer.finish();

    super::seal_registered(
        &store, &writers, attempt, reader, selection, &mut state, &budget,
    )
    .unwrap();

    assert_eq!(state.losses, crate::Losses::default());
    assert!(store.interrupted().unwrap().is_none());
    persist_until_quiet(&recording, &store, &mut state);
    let lost: Vec<_> = recording
        .saved_losses()
        .iter()
        .map(|l| l.publications)
        .collect();
    assert_eq!(lost, [2, 0, 0, 0, 0, 0, 0, 0]);
}

#[test]
fn a_selection_whose_start_was_overwritten_is_selected_again_from_what_remains() {
    // Goal: when the ring overwrites only the start of the selected minute, the rest of it is
    // still saved, as a minute starting where the ring now starts, and every event is either
    // saved or counted lost, once (P02.3).
    // Method: register a minute of eight ticks with events, publish four more into that minute
    // and one past it, stage through the worker, then run it to the end and account for events.
    let mut recording = Recording::new();
    let mut events = 0;
    for _tick in 0..8 {
        let count = recording.writer.event_room().min(1);
        recording.publish_events_in_minute(0, count);
        events += u64::try_from(count).unwrap();
    }
    let store = Store::open(&recording.root).unwrap();
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let derive = SealedApps::default();
    let writers = Writers {
        encoder: Encoder,
        derive: &derive,
    };
    let mut state = RunState {
        next_publication: 1,
        loaded: true,
        ..RunState::default()
    };
    let selection = super::Selection {
        identity: WindowIdentity {
            run: recording.run,
            window: 0,
        },
        publication: 1,
    };
    let mut reader = RingReader::open_generation(recording.writer.name()).unwrap();
    let input = super::seal_input(&store, &reader, &selection.identity, 1, &state, &budget)
        .unwrap()
        .0;
    let attempt = store
        .register_attempt(&input, &mut reader, &budget)
        .unwrap();
    for _tick in 0..4 {
        let count = recording.writer.event_room().min(1);
        recording.publish_events_in_minute(0, count);
        events += u64::try_from(count).unwrap();
    }
    recording.publish_in_minute(1);
    recording.writer.finish();

    super::seal_registered(
        &store, &writers, attempt, reader, selection, &mut state, &budget,
    )
    .unwrap();

    assert!(store.interrupted().unwrap().is_none());
    persist_until_quiet(&recording, &store, &mut state);
    let losses = recording.saved_losses();
    assert_eq!(losses.iter().map(|l| l.publications).sum::<u64>(), 5);
    let rows: u64 = recording.saved_event_rows().iter().sum();
    let lost: u64 = losses.iter().map(|l| l.events).sum();
    assert_eq!(rows + lost, events);
}

/// The staging a seal runs, for a test that takes its reads one at a time.
fn staging_by_hand(
    attempt: &RegisteredAttempt<'_>,
    reader: &RingReader,
    identity: WindowIdentity,
) -> super::super::staging::Staging {
    let events = crate::event_table::writer::Writer::new(
        attempt.events_directory().unwrap(),
        identity,
        reader.next_event(),
        false,
        256 * 1024 * 1024,
    )
    .unwrap();

    super::super::staging::Staging::new(
        reader,
        identity,
        super::super::Limits::from_shape(&reader.shape()).unwrap(),
        events,
    )
}

#[test]
fn publications_overwritten_inside_a_minute_are_counted_in_that_minute() {
    // Goal: when the ring overwrites the middle of a minute while it is read, the minute keeps
    // what was read before and after, and counts the gap, since the attempt's start still holds
    // (P02.3).
    // Method: step the worker's staging by hand, publishing past the unread ticks between two
    // reads, then write and publish what it read and read the minute back.
    let mut recording = Recording::new();
    for _tick in 0..4 {
        recording.publish_in_minute(0);
    }
    let store = Store::open(&recording.root).unwrap();
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let derive = SealedApps::default();
    let writers = Writers {
        encoder: Encoder,
        derive: &derive,
    };
    let mut state = RunState {
        next_publication: 1,
        loaded: true,
        ..RunState::default()
    };
    let identity = WindowIdentity {
        run: recording.run,
        window: 0,
    };
    let mut reader = RingReader::open_generation(recording.writer.name()).unwrap();
    let input = super::seal_input(&store, &reader, &identity, 1, &state, &budget)
        .unwrap()
        .0;
    let attempt = store
        .register_attempt(&input, &mut reader, &budget)
        .unwrap();
    reader.seek_publication(1).unwrap();
    reader.seek_event(input.losses.event_start).unwrap();
    let shape = reader.shape();
    let mut tick = crate::tick_buffer(&shape).unwrap();
    let mut staged = staging_by_hand(&attempt, &reader, identity);

    assert!(
        staged
            .read(&mut reader, &mut tick, &mut state.losses)
            .unwrap()
    );
    for _tick in 0..8 {
        recording.publish_in_minute(0);
    }
    recording.publish_in_minute(1);
    recording.writer.finish();
    for _step in 0..=shape.slot_count {
        if !staged
            .read(&mut reader, &mut tick, &mut state.losses)
            .unwrap()
        {
            break;
        }
    }
    let window = staged.finish(&mut reader, &mut state.losses).unwrap();
    let manifest = super::seal_write(&attempt, &writers, &window, &state).unwrap();
    drop(window);
    super::seal_publish(attempt, &manifest, reader, &budget).unwrap();

    // Thirteen publications with eight retained leaves 6 to 13, so 2 to 5 are the gap.
    assert_eq!(manifest.publication_first, 1);
    assert_eq!(manifest.losses.publications, 4);
    drop(store);
    let reopened = Store::open(&recording.root).unwrap();
    assert_eq!(recording.frontier(&reopened).unwrap().publication_end(), 13);
}

#[test]
fn a_seal_charges_its_event_writer_to_the_one_budget_it_was_given() {
    // Goal: the worker's memory limit covers the event writer too, so a seal cannot reserve that
    // limit twice (P02.4).
    // Method: stage a registered attempt with a budget one byte short of the writer's working
    // memory, which must refuse, then with exactly that much, which must stage.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let store = Store::open(&recording.root).unwrap();
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let mut state = RunState::default();
    let identity = WindowIdentity {
        run: recording.run,
        window: 0,
    };
    let mut reader = RingReader::open_generation(recording.writer.name()).unwrap();
    let input = super::seal_input(&store, &reader, &identity, 1, &state, &budget)
        .unwrap()
        .0;
    let attempt = store
        .register_attempt(&input, &mut reader, &budget)
        .unwrap();
    let working = crate::event_recovery::batch::WORKING_BYTES;
    let short = Arc::new(ByteBudget::new(working - 1));
    let enough = Arc::new(ByteBudget::new(working));
    reader.seek_publication(1).unwrap();
    reader.seek_event(input.losses.event_start).unwrap();

    let refused = super::seal_stage(&attempt, &mut reader, identity, &[], &mut state, &short);
    let staged = super::seal_stage(&attempt, &mut reader, identity, &[], &mut state, &enough);

    assert!(matches!(
        refused,
        Err(crate::Error::Io(error)) if error.kind() == ErrorKind::OutOfMemory
    ));
    assert!(staged.is_ok());
}

#[test]
fn the_whole_worker_saves_a_finished_run_retires_it_and_exits() {
    // Goal: the worker's own loop drives a registered recording to a certified end and lets go of
    // the run, and a later worker finds nothing left to do (P01.5, P02.3).
    // Method: run the whole worker over a finished ring, read what it saved through a new Store,
    // then run it again with the ring gone, which only the retirement can answer.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    recording.writer.finish();
    let run = recording.registered();

    let outcome = whole_worker(&recording.root, vec![run.clone()], &mut |_pause| {
        panic!("a finished run gives the worker nothing to wait for")
    });

    assert!(matches!(outcome, Outcome::Complete));
    assert_eq!(recording.saved_windows().len(), 3);
    assert!(Registry::completed(&recording.root, &run.identity, &run.ring_name).unwrap());
    let store = Store::open(&recording.root).unwrap();
    assert!(recording.frontier(&store).unwrap().complete());
    drop(store);
    recording.lose_source();

    let again = whole_worker(&recording.root, vec![run], &mut |_pause| {
        panic!("a retired run gives the worker nothing to wait for")
    });

    assert!(matches!(again, Outcome::Complete));
    assert_eq!(recording.saved_windows().len(), 3);
}

#[test]
fn the_whole_worker_retires_a_run_certified_before_it_could_retire_it() {
    // Goal: a process that ended between certifying a run and retiring it leaves work a later
    // worker finishes from what is durable, so that worker still lets go and exits (P01.5).
    // Method: certify the run by stepping the worker, without retiring it, lose the ring, then
    // run the whole worker and read the retirement.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    recording.writer.finish();
    let run = recording.registered();
    let store = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();
    assert_eq!(persist_until_quiet(&recording, &store, &mut state), 3);
    drop(store);
    assert!(!Registry::completed(&recording.root, &run.identity, &run.ring_name).unwrap());
    recording.lose_source();

    let outcome = whole_worker(&recording.root, vec![run.clone()], &mut |_pause| {
        panic!("a certified run gives the worker nothing to wait for")
    });

    assert!(matches!(outcome, Outcome::Complete));
    assert!(Registry::completed(&recording.root, &run.identity, &run.ring_name).unwrap());
}

#[test]
fn the_whole_worker_paces_itself_until_a_live_run_finishes() {
    // Goal: a worker beside a live collector waits, and a stopping collector, which waits for the
    // worker to exit, waits for no further pause once the last minute and the end are saved.
    // Method: run the whole worker over a live ring, and in its first pause publish the next
    // minute and finish the ring as a stopping collector does.
    let mut recording = Recording::new();
    recording.publish_in_minute(LIVE_MINUTE);
    let root = recording.root.clone();
    let run = RegisteredRun {
        collector_pid: std::process::id(),
        ..recording.registered()
    };
    let mut pauses = 0;

    let outcome = whole_worker(&root, vec![run], &mut |_pause| {
        pauses += 1;
        assert_eq!(pauses, 1, "the worker paused after the ring finished");
        recording.publish_in_minute(LIVE_MINUTE + 1);
        recording.writer.finish();
    });

    assert!(matches!(outcome, Outcome::Complete));
    assert_eq!(pauses, 1);
    assert_eq!(recording.saved_windows().len(), 2);
    let store = Store::open(&recording.root).unwrap();
    assert!(recording.frontier(&store).unwrap().complete());
}

#[test]
fn the_whole_worker_marks_what_a_lost_run_may_have_held_and_goes_on() {
    // Goal: a run whose ring is gone stops nothing, and the extent it may have held stays visible
    // on the next saved minute rather than reading as no loss (P02.3).
    // Method: register a run with no ring ahead of a finished run, run the whole worker, and read
    // the losses each saved minute records.
    let mut lost = Recording::new();
    lost.lose_source();
    let mut next = lost.another_run();
    for minute in 0..3 {
        next.publish_in_minute(minute);
    }
    next.writer.finish();
    let runs = vec![lost.registered(), next.registered()];

    let outcome = whole_worker(&lost.root, runs, &mut |_pause| {
        panic!("every source is finished or gone")
    });

    assert!(matches!(outcome, Outcome::Complete));
    let unknown: Vec<_> = lost
        .saved_losses()
        .iter()
        .map(|loss| loss.unknown)
        .collect();
    assert_eq!(unknown, [true, false, false]);
    let named = UnknownProvenance {
        reason: UnknownReason::Predecessor,
        run: lost.run.run,
        boot: lost.run.boot,
    };
    assert_eq!(lost.saved_unknown_provenance(), [Some(named), None, None]);
}

#[test]
fn the_whole_worker_marks_what_an_evicted_ring_may_have_held() {
    // Goal: a restart beside evicted rings says that an extent is unknown even though nothing
    // names the rings, so the run after that extent stands as its last observed boundary (P02.3).
    // Method: run the whole worker over a finished run in a registry that counts one eviction.
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    recording.writer.finish();
    let registry = Registry {
        runs: vec![recording.registered()],
        evictions: 1,
    };

    let outcome = whole_worker_of(&recording.root, &registry, &mut |_pause| {
        panic!("a finished run gives the worker nothing to wait for")
    });

    assert!(matches!(outcome, Outcome::Complete));
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

#[path = "catalog_tests.rs"]
mod catalog_tests;

#[path = "kill_tests.rs"]
mod kill_tests;

#[path = "refusal_tests.rs"]
mod refusal_tests;
