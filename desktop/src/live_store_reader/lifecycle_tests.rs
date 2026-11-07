use super::*;

const HISTORY_PROCESSES: u32 = 16_384;
const INITIAL_ROOM_BYTES: usize = 52 * 1024 * 1024;

#[test]
fn a_primary_worker_keeps_serving_while_history_admission_is_denied() {
    // Goal: real history backpressure keeps Primary usable without a Worker checkpoint.
    // Method: deny a larger saved process input, change route, then release its byte lease.
    let (mut harness, budget, lease) = Harness::denied_history();
    let mut selected = request("memory");
    selected.load_resident_history = true;
    let generation = harness.worker.request(selected).unwrap();

    let primary = harness.ready_primary(&budget);

    assert_eq!(primary.generation, generation);
    assert!(primary.histories_pending);
    assert!(!harness.worker.last_output_complete());
    assert_eq!(primary.selection["selected"]["publication"], 124);
    assert_eq!(primary.selection["summary"]["cpu_percent"], 50.0);
    assert!(primary.resident_history.is_none());
    denied_history_check(&harness.recording.as_ref().unwrap().root, &budget);
    assert!(
        harness
            .worker
            .shared
            .measurement_gate
            .lock()
            .unwrap()
            .is_none()
    );
    assert_eq!(harness.ordinary_app_work(), 0);
    let source = primary.catalog["source_generation"].clone();
    let identity = primary.selection["selected"].clone();
    drop(primary);
    let generation = harness.worker.request(request("cpu")).unwrap();
    let pending = harness.ready_primary(&budget);
    assert_eq!(pending.generation, generation);
    assert!(pending.histories_pending);
    assert_eq!(pending.catalog["source_generation"], source);
    assert_eq!(pending.selection["selected"], identity);
    assert_eq!(
        pending.cpu_detail.as_ref().unwrap().cpus,
        [crate::live_cpu::LogicalCPU {
            cpu: 0,
            busy_percent: Some(50.0),
        }]
    );
    assert_eq!(pending.cpu_history.as_ref().unwrap().observation_count(), 9);
    assert_eq!(harness.ordinary_app_work(), 0);
    drop(pending);
    drop(lease);
    let complete = harness.ready();

    assert_eq!(complete.generation, generation);
    assert!(!complete.histories_pending);
    assert!(harness.worker.last_output_complete());
    assert_eq!(complete.catalog["source_generation"], source);
    assert_eq!(complete.selection["selected"], identity);
    assert!(harness.ordinary_app_work() > 0);
    harness.assert_source_free();
    drop((complete, harness));
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_cancelled_kept_primary_refresh_cannot_replace_a_new_selection() {
    // Goal: an inspection awaiting Complete loses its reply when its operation is cancelled.
    // Method: submit a real kept query during history denial, cancel, then complete a selection.
    let (harness, budget, lease) = Harness::denied_history();
    let handle = harness.store.as_ref().unwrap().query_handle().unwrap();
    let mut adapter = Adapter::new(handle.clone());
    let primary = adapter.ready_for(viewer::store::QueryReadiness::Primary);
    let source = primary.catalog["source_generation"].clone();
    let selected = primary.selection["selected"].clone();
    let mut inspection = request("memory");
    inspection.second = Some(1000);
    inspection.identity = Some(primary.selection["trends"][0].clone());
    adapter.submit(inspection, crate::live_source::Context::Keep);

    assert!(adapter.read().is_none());
    assert!(handle.pending());
    adapter.shared.cancellation.cancel().unwrap();
    assert!(adapter.read().is_none());
    assert!(handle.take().is_none());
    adapter.submit(request("network"), crate::live_source::Context::Select);
    drop(primary);
    drop(lease);
    let complete = adapter.ready_for(viewer::store::QueryReadiness::Complete);

    assert_eq!(complete.generation, 3);
    assert_eq!(complete.catalog["source_generation"], source);
    assert_eq!(complete.selection["selected"], selected);
    assert_eq!(complete.selection["selected"]["publication"], 124);
    assert_eq!(complete.selection["summary"]["cpu_percent"], 50.0);
    harness.assert_source_free();
    drop((complete, adapter, handle, harness));
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_store_missing_its_ring_leaves_the_page_to_the_saved_minutes() {
    // Goal: a page never waits for ever on a Store that cannot prove what it holds.
    // Method: register a ring that does not exist beside a saved minute, and step the reader.
    let mut recording = LiveRecording::new();
    recording.publish_resources();
    recording.publish_resources();
    seal_recording(&recording, 0);
    let mut registry = collector::recording::Registry::read(&recording.root).unwrap();
    registry.runs[0].ring_name = String::from("/fdnoring");
    registry.write(&recording.root).unwrap();
    let store = viewer::store::Store::start_with_budget(
        &recording.root,
        Arc::new(viewer::store::ByteBudget::new(128 * 1024 * 1024)),
    )
    .unwrap();
    let mut adapter = Adapter::on_root(store.query_handle().unwrap(), &recording.root);

    let update = adapter.first_update();

    let Update::Ready(snapshot) = update else {
        panic!("the saved minutes did not answer");
    };
    assert_eq!(snapshot.selection["selected"]["publication"], 2);
}

#[test]
fn a_recording_that_never_appears_says_so() {
    // Goal: a page never waits for ever on a Store that has no recording to read.
    // Method: start the Store on a root nothing makes, and step the reader.
    let directory =
        std::env::temp_dir().join(format!("flightdeck-no-recording-{}", std::process::id()));
    let store = viewer::store::Store::start_with_budget(
        &directory,
        Arc::new(viewer::store::ByteBudget::new(128 * 1024 * 1024)),
    )
    .unwrap();
    let mut adapter = Adapter::on_root(store.query_handle().unwrap(), &directory);

    let update = adapter.first_update();

    assert!(matches!(update, Update::Failed { .. }));
}

#[test]
fn a_page_the_saved_minutes_answered_reads_them_again_a_second_later() {
    // Goal: a Store that stays behind costs the page one listing of the recording a second, and the
    // minute saved meanwhile still reaches it.
    // Method: read past the wait with the Store parked behind the ring, save a minute, and time the
    // next update from before the first.
    let (mut recording, _store, handle) = store_parked_behind_the_ring();
    let mut adapter = Adapter::on_root(handle, &recording.root);
    adapter.submit(request("network"), crate::live_source::Context::Select);
    let started = Instant::now();
    let Update::Ready(first) = adapter.first_update() else {
        panic!("the saved minutes did not answer");
    };
    assert_eq!(first.selection["selected"]["publication"], 2);
    for _ in 0..62 {
        recording.publish_resources();
    }
    seal_recording(&recording, 1);

    let next = adapter.update_within(Duration::from_secs(10));

    let Update::Ready(next) = next else {
        panic!("the saved minutes did not answer again");
    };
    assert!(started.elapsed() >= crate::live_source::RETRY_INTERVAL);
    assert_ne!(next.selection["selected"]["publication"], 2);
}

#[test]
fn a_store_that_catches_up_takes_the_page_back_from_the_saved_minutes() {
    // Goal: the saved minutes stand in for a Store that stays behind only until it catches up.
    // Method: read past the wait with the Store parked behind the ring, then let it go on.
    let (recording, store, handle) = store_parked_behind_the_ring();
    let mut adapter = Adapter::on_root(handle, &recording.root);
    adapter.submit(request("network"), crate::live_source::Context::Select);
    let Update::Ready(saved) = adapter.first_update() else {
        panic!("the saved minutes did not answer");
    };
    assert_eq!(saved.selection["selected"]["publication"], 2);

    store.set_active(true);
    let next = adapter.update_within(Duration::from_secs(10));

    let Update::Ready(live) = next else {
        panic!("the Store did not answer");
    };
    assert_eq!(live.selection["selected"]["publication"], 3);
}

#[test]
fn a_page_the_saved_minutes_answered_in_part_asks_again_at_the_workers_pace() {
    // Goal: the wait of a second between saved answers never delays the rest of a page's own read.
    // Method: read a page whose histories come a step later past the wait, with the Store parked
    // behind the ring, and look at when the reader may step again.
    let (recording, _store, handle) = store_parked_behind_the_ring();
    let mut adapter = Adapter::on_root(handle, &recording.root);
    let mut request = request("memory");
    request.load_resident_history = true;
    adapter.submit(request, crate::live_source::Context::Select);

    let Update::Ready(first) = adapter.first_update() else {
        panic!("the saved minutes did not answer");
    };

    assert!(first.histories_pending);
    assert!(adapter.reader.retry_after <= Instant::now());
}

#[test]
fn a_second_whose_apps_are_unread_holds_the_page_until_the_store_reads_them() {
    // Goal: a reading the Store is about to complete shows from the Store, not the saved minutes.
    // Method: ask for an older second of a Primary while history admission is denied, stepping
    // short of the bounded wait on a root with no recording, where a saved read fails, then let
    // the Store read on.
    let (harness, _budget, lease) = Harness::denied_history();
    let mut adapter = Adapter::new(harness.store.as_ref().unwrap().query_handle().unwrap());
    let primary = adapter.ready_for(viewer::store::QueryReadiness::Primary);
    let mut older = request("memory");
    older.second = Some(1000);
    adapter.submit(older, crate::live_source::Context::Select);

    let steps_short_of_the_wait = super::super::STORE_WAITS_MAX - 10;
    for _ in 0..steps_short_of_the_wait {
        assert!(adapter.read().is_none());
        std::thread::sleep(Duration::from_millis(10));
    }
    drop((primary, lease));
    let complete = adapter.ready_for(viewer::store::QueryReadiness::Complete);

    assert_eq!(complete.selection["selected"]["publication"], 1);
}

#[test]
fn an_incomplete_ring_holds_the_page_for_the_wait() {
    // Goal: a ring the Store is about to note as whole costs a wait, not a saved read.
    // Method: answer the reader directly, since a real Store answers this only in the moment
    // between reading the ring and noting that it did.
    let (_recording, _store, handle) = store_parked_behind_the_ring();
    let mut reader = super::super::StoreReader::new(handle);

    let answer = reader.needs_load(viewer::store::QueryNeedsLoad::IncompleteRing);

    assert!(matches!(answer, super::super::Answer::Waiting));
}

#[test]
fn a_second_whose_detail_the_store_let_go_reads_the_saved_minutes_at_once() {
    // Goal: waiting cannot bring back what the Store let go, so the page does not wait for it.
    // Method: answer the reader directly, which leaves it no reason to step again.
    let (_recording, _store, handle) = store_parked_behind_the_ring();
    let mut reader = super::super::StoreReader::new(handle);

    let answer = reader.needs_load(viewer::store::QueryNeedsLoad::FullDetailEvicted);

    assert!(matches!(answer, super::super::Answer::Saved));
}

#[test]
fn a_second_the_store_holds_no_reading_of_reads_the_saved_minutes_at_once() {
    // Goal: waiting cannot bring a reading the Store does not hold, so the page does not wait.
    // Method: answer the reader directly, which leaves it no reason to step again.
    let (_recording, _store, handle) = store_parked_behind_the_ring();
    let mut reader = super::super::StoreReader::new(handle);

    let answer = reader.needs_load(viewer::store::QueryNeedsLoad::MissingObservation);

    assert!(matches!(answer, super::super::Answer::Saved));
}

/// A Store parked a reading behind its ring, over a saved minute, and a handle to ask it.
fn store_parked_behind_the_ring() -> (
    LiveRecording,
    viewer::store::Store,
    viewer::store::QueryHandle,
) {
    let mut recording = LiveRecording::new();
    recording.publish_resources();
    recording.publish_resources();
    seal_recording(&recording, 0);
    let store = viewer::store::Store::start_with_budget(
        &recording.root,
        Arc::new(viewer::store::ByteBudget::new(128 * 1024 * 1024)),
    )
    .unwrap();
    let handle = store.query_handle().unwrap();
    Harness::ready_context(&handle);
    store.set_active(false);
    Harness::wait_until_paused(&store);
    recording.publish_resources();

    (recording, store, handle)
}

struct Adapter {
    root: std::path::PathBuf,
    reader: crate::live_source::Reader,
    command: crate::live_source::Command,
    shared: crate::live_source::Shared,
}

impl Adapter {
    fn new(handle: viewer::store::QueryHandle) -> Self {
        Self::on_root(handle, std::path::Path::new(""))
    }

    fn on_root(handle: viewer::store::QueryHandle, root: &std::path::Path) -> Self {
        Self {
            root: root.to_owned(),
            reader: crate::live_source::Reader {
                session: None,
                store: Some(super::super::StoreReader::new(handle)),
                last_catalog: None,
                last_generation: 0,
                last_route: String::new(),
                pending: None,
                retry_after: Instant::now(),
            },
            command: crate::live_source::Command {
                generation: 1,
                epoch: 0,
                mode: crate::live_source_mode::SourceMode::Live,
                request: request("memory"),
                context: crate::live_source::Context::Select,
            },
            shared: crate::live_source::Shared::new(),
        }
    }

    fn read(&mut self) -> Option<Update> {
        super::super::read(
            &mut self.reader,
            &self.root,
            &mut self.command,
            &self.shared,
        )
    }

    /// The first thing the reader publishes, stepping with no pause, so a test of its bounded wait
    /// takes none of the time that wait lasts at the worker's pace.
    fn first_update(&mut self) -> Update {
        for _ in 0..1_000 {
            if let Some(update) = self.read() {
                return update;
            }
        }
        panic!("the reader kept waiting");
    }

    fn update_within(&mut self, limit: Duration) -> Update {
        let deadline = Instant::now() + limit;
        for _ in 0..1_000 {
            if let Some(update) = self.read() {
                return update;
            }
            assert!(Instant::now() < deadline, "the reader kept waiting");
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the reader kept waiting within its attempts");
    }

    fn submit(&mut self, request: Request, context: crate::live_source::Context) {
        self.shared.cancellation.cancel().unwrap();
        self.command.generation += 1;
        self.command.epoch = self.shared.cancellation.epoch();
        self.command.request = request;
        self.command.context = context;
    }

    fn ready_for(&mut self, readiness: viewer::store::QueryReadiness) -> Box<Snapshot> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut failure = None;
        for _ in 0..1000 {
            if let Some(update) = self.read() {
                match update {
                    Update::Ready(snapshot) => {
                        if snapshot.histories_pending
                            == (readiness == viewer::store::QueryReadiness::Primary)
                        {
                            return snapshot;
                        }
                    }
                    Update::Failed { message, .. } => failure = Some(message),
                }
            }
            assert!(
                Instant::now() < deadline,
                "Adapter did not reply: {failure:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("Adapter exceeded bounded attempts: {failure:?}");
    }
}

impl Harness {
    fn denied_history() -> (
        Self,
        Arc<viewer::store::ByteBudget>,
        viewer::store::ByteReservation,
    ) {
        let mut recording = LiveRecording::with_process_capacity(u64::from(HISTORY_PROCESSES));
        recording.publish_resources_with_processes(&history_processes());
        seal_recording(&recording, 0);
        for _ in 0..63 {
            recording.publish_resources();
        }
        seal_recording(&recording, 1);
        for _ in 0..60 {
            recording.publish_resources();
        }
        seal_recording(&recording, 2);
        let budget = Arc::new(viewer::store::ByteBudget::new(128 * 1024 * 1024));
        let lease = budget
            .reserve(budget.maximum() - INITIAL_ROOM_BYTES)
            .unwrap();
        let trace = persistence::work::ingestion::Trace::new(
            collector::recording::read_identity(&recording.root).unwrap(),
        );
        let scope = trace.enter();
        let store =
            viewer::store::Store::start_with_budget(&recording.root, Arc::clone(&budget)).unwrap();
        let worker =
            Worker::start_from_store(recording.root.clone(), store.query_handle().unwrap())
                .unwrap();

        (
            Self {
                worker,
                store: Some(store),
                recording: Some(recording),
                trace,
                scope: Some(scope),
            },
            budget,
            lease,
        )
    }

    fn ordinary_app_work(&self) -> usize {
        use persistence::work::ingestion::{Kind, Producer, Purpose};
        let mut events = Vec::new();
        self.trace.snapshot(&mut events).unwrap();
        events
            .iter()
            .filter(|event| event.producer == Producer::Store)
            .filter(|event| event.purpose == Purpose::Ordinary)
            .filter(|event| event.kind == Kind::AppDerivation)
            .count()
    }

    fn ready_primary(&mut self, budget: &viewer::store::ByteBudget) -> Box<Snapshot> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut failure = None;
        for _ in 0..1000 {
            if let Some(update) = self.worker.take().unwrap() {
                match update {
                    Update::Ready(snapshot) => {
                        assert!(snapshot.histories_pending);
                        return snapshot;
                    }
                    Update::Failed { message, .. } => failure = Some(message),
                }
            }
            assert!(
                Instant::now() < deadline,
                "Primary did not reply: {failure:?}; budget {}/{}",
                budget.used(),
                budget.maximum(),
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("Primary exceeded bounded attempts: {failure:?}");
    }
}

fn denied_history_check(root: &std::path::Path, budget: &Arc<viewer::store::ByteBudget>) {
    let run = collector::recording::Registry::read(root).unwrap().runs[0].identity;
    let catalog_budget = Arc::new(persistence::resources::ByteBudget::new(1 << 28));
    let manifest = persistence::canonical::catalog::Catalog::open(root, &catalog_budget)
        .unwrap()
        .windows_of(&run, &mut persistence::canonical::ReadWork::default())
        .unwrap()
        .into_iter()
        .find(|manifest| manifest.identity.window == 0)
        .unwrap();
    let processes = root.join(manifest.data_directory).join("processes.parquet");
    assert!(matches!(
        persistence::engine::AdmittedTable::from_file_budgeted(
            std::fs::File::open(processes).unwrap(),
            budget,
        ),
        Err(persistence::engine::AdmissionRefusal::Budget)
    ));
}

fn history_processes() -> Vec<collector::processes::Process> {
    use collector::processes::{Process, Task, Usage};
    let mut seed = 0x6275_6467_6574_6564_u64;
    (1..=HISTORY_PROCESSES)
        .map(|pid| Process {
            pid: i32::try_from(pid).unwrap(),
            usage: Usage {
                awake_ns_at_start: 0,
                user_ns: history_counter(&mut seed),
                system_ns: history_counter(&mut seed),
                energy_nj: history_counter(&mut seed),
                instructions: history_counter(&mut seed),
                super_instructions: history_counter(&mut seed),
                cycles: history_counter(&mut seed),
                idle_wakeups: history_counter(&mut seed),
                interrupt_wakeups: history_counter(&mut seed),
                pageins: history_counter(&mut seed),
                resident_bytes: history_counter(&mut seed),
                footprint_bytes: history_counter(&mut seed),
                disk_read_bytes: history_counter(&mut seed),
                disk_written_bytes: history_counter(&mut seed),
            },
            task: Task {
                threads: history_task_counter(&mut seed),
                faults: history_task_counter(&mut seed),
                context_switches: history_task_counter(&mut seed),
                mach_syscalls: history_task_counter(&mut seed),
                unix_syscalls: history_task_counter(&mut seed),
            },
        })
        .collect()
}

fn history_counter(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

fn history_task_counter(seed: &mut u64) -> i32 {
    i32::try_from(history_counter(seed) & 0x3fff_ffff).unwrap()
}
