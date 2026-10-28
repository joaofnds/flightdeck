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

struct Adapter {
    reader: crate::live_source::Reader,
    command: crate::live_source::Command,
    shared: crate::live_source::Shared,
}

impl Adapter {
    fn new(handle: viewer::store::QueryHandle) -> Self {
        Self {
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
        super::super::read(&mut self.reader, &mut self.command, &self.shared)
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
    let path = persistence::layout::WindowKey {
        run: collector::recording::Registry::read(root).unwrap().runs[0]
            .identity
            .run,
        window: 0,
    }
    .marker(root);
    let manifest = persistence::read_committed(root, &path).unwrap();
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
