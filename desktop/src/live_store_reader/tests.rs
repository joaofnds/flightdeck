use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::live_dashboard::input_tests::source_mode::LiveRecording;
use crate::live_source::{Earlier, Request, Snapshot, Update, Worker};

#[test]
fn a_store_worker_presents_primary_then_complete_from_a_queued_sealed_request() {
    // Goal: real budget backpressure preserves a request until Primary, then fills its history.
    // Method: reserve every admitted byte before construction and use the existing Pending hold.
    let (mut harness, lease, handle) = Harness::sealed_blocked();
    let hold = harness
        .worker
        .hold_measurement_stage(crate::live_source::measurement::Stage::Pending);
    let mut request = request("memory");
    request.second = Some(1063);
    request.load_resident_history = true;
    let generation = harness.worker.request(request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    for _ in 0..1000 {
        if handle.pending() {
            break;
        }
        assert!(Instant::now() < deadline, "Store request never queued");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(handle.pending());
    drop(lease);

    let primary = harness.ready_strict();

    assert_eq!(primary.generation, generation);
    assert_eq!(primary.selection["selected"]["publication"], 64);
    assert_eq!(
        primary.selection["summary"]["memory_total_bytes"],
        9_007_199_254_740_993_u64
    );
    assert!(primary.histories_pending);
    assert!(!harness.worker.last_output_complete());
    hold.release();
    let complete = harness.ready_strict();
    assert_eq!(complete.generation, generation);
    assert_eq!(
        complete.selection["selected"],
        primary.selection["selected"]
    );
    assert!(!complete.histories_pending);
    assert!(complete.resident_history.is_some());
    assert!(harness.worker.last_output_complete());
    harness.assert_source_free();
}

#[test]
fn a_kept_selected_inspection_refreshes_primary_without_moving_its_endpoint() {
    // Goal: a kept selected read cannot retain an immutable Primary forever.
    // Method: supersede the held Primary with inspection, append native data, then restore.
    let (mut harness, primary, hold) = primary_sealed(Some(1063));
    let identity = primary.selection["selected"].clone();
    inspect_primary(&mut harness, &primary, hold, 1063, identity);
}

#[test]
fn a_kept_unread_sibling_waits_for_complete_before_formatting_its_apps() {
    // Goal: an unread sibling is a continuation rather than a generic missing-ranks failure.
    // Method: inspect publication one from the immutable Primary after its request is superseded.
    let (mut harness, primary, hold) = primary_sealed(Some(1063));
    let identity = primary.selection["trends"]
        .as_array()
        .unwrap()
        .iter()
        .find(|point| point["publication"] == 1)
        .unwrap()
        .clone();
    inspect_primary(&mut harness, &primary, hold, 1000, identity);
}

fn primary_sealed(
    selected_second: Option<u64>,
) -> (
    Harness,
    Box<Snapshot>,
    crate::live_source::measurement::Hold,
) {
    let (mut harness, lease, handle) = Harness::sealed_blocked();
    let hold = harness
        .worker
        .hold_measurement_stage(crate::live_source::measurement::Stage::Pending);
    let mut request = request("memory");
    request.second = selected_second;
    request.load_resident_history = true;
    harness.worker.request(request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    for _ in 0..1000 {
        if handle.pending() {
            break;
        }
        assert!(Instant::now() < deadline, "Store request never queued");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(handle.pending());
    drop(lease);
    let primary = harness.ready_strict();
    assert!(primary.histories_pending);
    (harness, primary, hold)
}

#[test]
fn a_following_primary_keeps_following_native_progress_through_complete() {
    // Goal: detached Primary ownership cannot silently turn FollowLatest into an exact pin.
    // Method: append a native successor while the actual Primary output is held.
    let (mut harness, primary, hold) = primary_sealed(None);
    harness.recording.as_mut().unwrap().publish_resources();
    hold.release();

    for _ in 0..8 {
        let snapshot = harness.ready_strict();
        if snapshot.selection["selected"]["publication"] == 65 {
            assert_eq!(snapshot.generation, primary.generation);
            assert_eq!(snapshot.selection["context_last_second"], 1064);
            assert!(!snapshot.histories_pending);
            assert!(harness.worker.last_output_complete());
            harness.assert_source_free();
            return;
        }
    }
    panic!("following context did not advance through Complete");
}

#[test]
fn a_followed_complete_context_reacquires_the_source_after_a_restart() {
    // Goal: following a new checked source needs no new Worker request.
    // Method: replace the completed real ring and wait for its run under the original request.
    let mut harness = Harness::new();
    let generation = harness.worker.request(request("memory")).unwrap();
    let previous = harness.ready();
    let source = previous.catalog["source_generation"].as_u64().unwrap();
    drop(previous);

    let run = harness.restart_source();
    let current = harness.ready_for_run(run);

    assert_eq!(current.generation, generation);
    assert!(current.catalog["source_generation"].as_u64().unwrap() > source);
    assert_eq!(current.selection["selected"]["publication"], 2);
    assert_eq!(current.selection["selected"]["run_tick"], 1);
    assert_eq!(
        current.selection["selected"]["wall_ns"],
        1_003_000_000_000_u64
    );
    assert_eq!(current.selection["summary"]["cpu_percent"], 50.0);
    harness.assert_source_free();
}

#[test]
fn a_followed_primary_context_reacquires_the_source_after_a_restart() {
    // Goal: a detached Primary keeps logical following through source replacement.
    // Method: restart the real source while the existing Worker Pending checkpoint is held.
    let (mut harness, primary, hold) = primary_sealed(None);
    let generation = primary.generation;
    let source = primary.catalog["source_generation"].as_u64().unwrap();
    drop(primary);

    let run = harness.restart_source();
    hold.release();
    let current = harness.ready_for_run(run);

    assert_eq!(current.generation, generation);
    assert!(current.catalog["source_generation"].as_u64().unwrap() > source);
    assert_eq!(current.selection["selected"]["publication"], 2);
    assert_eq!(current.selection["selected"]["run_tick"], 1);
    assert_eq!(
        current.selection["selected"]["wall_ns"],
        1_065_000_000_000_u64
    );
    assert_eq!(current.selection["summary"]["cpu_percent"], 50.0);
    harness.assert_source_free();
}

fn inspect_primary(
    harness: &mut Harness,
    primary: &Snapshot,
    hold: crate::live_source::measurement::Hold,
    second: u64,
    identity: serde_json::Value,
) {
    harness.recording.as_mut().unwrap().publish_resources();
    let mut inspection = request("memory");
    inspection.second = Some(second);
    let publication = identity["publication"].clone();
    inspection.identity = Some(identity);
    let generation = harness.worker.inspect(inspection).unwrap();
    hold.release();
    drop(hold);
    let inspected = harness.ready_strict();
    assert_eq!(inspected.generation, generation);
    assert_eq!(inspected.selection["selected"]["publication"], publication);
    assert!(!inspected.histories_pending);
    assert!(harness.worker.last_output_complete());
    let mut restore = request("memory");
    restore.second = Some(1063);
    restore.identity = Some(primary.selection["selected"].clone());
    harness.worker.request(restore).unwrap();
    let restored = harness.ready_strict();
    assert_eq!(restored.selection["context_last_second"], 1063);
    assert_eq!(
        restored.selection["selected"],
        primary.selection["selected"]
    );
    harness.assert_source_free();
}

#[test]
fn a_store_continuation_with_no_reply_uses_the_nonzero_polling_cadence() {
    // Goal: asynchronous Store work cannot consume the Worker's local zero-delay lease.
    // Method: submit through the actual no-reply adapter while all source admission is denied.
    use crate::live_source::{Command, Context, Histories, Pending, Reader, Shared};
    let (harness, lease, handle) = Harness::sealed_blocked();
    let mut reader = Reader {
        session: None,
        store: Some(super::StoreReader::new(handle.clone())),
        last_catalog: None,
        last_generation: 1,
        last_route: String::from("memory"),
        pending: Some(Pending {
            generation: 1,
            histories: Histories::Defer,
        }),
        retry_after: Instant::now(),
    };
    let mut command = Command {
        generation: 1,
        epoch: 0,
        mode: crate::live_source_mode::SourceMode::Live,
        request: request("memory"),
        context: Context::Select,
    };

    assert!(super::read(&mut reader, &mut command, &Shared::new()).is_none());

    assert!(handle.pending());
    assert_eq!(
        crate::live_source::worker_pause(&reader, &command),
        crate::live_source::POLL_INTERVAL
    );
    assert!(crate::live_source::POLL_INTERVAL > Duration::ZERO);
    drop(reader);
    drop(lease);
    drop(harness);
}

#[test]
fn opt_in_construction_allocates_fewer_bytes_than_legacy_app_row_pools() {
    // Goal: budgeted construction does not allocate the legacy per-second App vectors.
    // Method: count allocations on the calling thread of both actual public constructors.
    let recording = LiveRecording::new();
    let (legacy, legacy_counts) =
        crate::allocation_tests::measure(|| viewer::store::Store::start(&recording.root).unwrap());
    drop(legacy);
    let budget = Arc::new(viewer::store::ByteBudget::new(128 * 1024 * 1024));
    let lease = budget.reserve(budget.maximum()).unwrap();

    let (payload, payload_counts) = crate::allocation_tests::measure(|| {
        viewer::store::Store::start_with_budget(&recording.root, budget).unwrap()
    });

    eprintln!("Store constructor allocations legacy={legacy_counts:?} opt_in={payload_counts:?}");
    assert!(payload_counts.bytes < legacy_counts.bytes);
    assert!(payload_counts.calls < legacy_counts.calls);
    drop(lease);
    drop(payload);
}

#[test]
fn a_store_worker_delivers_network_without_reading_or_deriving_the_source() {
    // Goal: the staged Worker presents complete Network facts from the single Store owner.
    // Method: publish real native readings, run both owners, and trace Worker source work.
    let mut harness = Harness::new();
    harness.worker.request(request("network")).unwrap();

    let snapshot = harness.ready();

    assert_eq!(snapshot.selection["selected"]["publication"], 2);
    assert_eq!(snapshot.selection["summary"]["network_rx_bps"], 1.0);
    assert_eq!(snapshot.selection["summary"]["network_tx_bps"], 1.0);
    harness.assert_source_free();
}

#[test]
fn a_store_worker_delivers_memory_and_the_requested_resident_history() {
    // Goal: Memory carries its full captured integer and complete requested App history.
    // Method: present native memory readings with resident history through the staged Worker.
    let mut harness = Harness::new();
    let mut request = request("memory");
    request.load_resident_history = true;
    harness.worker.request(request).unwrap();

    let snapshot = harness.ready();

    assert_eq!(
        snapshot.selection["summary"]["memory_total_bytes"].as_u64(),
        Some(9_007_199_254_740_993)
    );
    let history = snapshot.resident_history.as_ref().unwrap();
    assert!(history.panel().is_some(), "{}", history.coverage(None));
    assert!(snapshot.rankings.is_some());
    assert!(!snapshot.histories_pending);
    harness.assert_source_free();
}

#[test]
fn a_store_worker_delivers_cpu_detail_and_history_with_matching_identity() {
    // Goal: CPU uses complete Store counters for both selected detail and its history.
    // Method: request CPU after two native publications, then validate existing decoders' output.
    let mut harness = Harness::new();
    harness.worker.request(request("cpu")).unwrap();

    let snapshot = harness.ready();

    let detail = snapshot.cpu_detail.as_ref().unwrap();
    assert_eq!(detail.cpus.len(), 1);
    assert_eq!(detail.cpus[0].busy_percent, Some(50.0));
    assert!(snapshot.cpu_history.is_some());
    harness.assert_source_free();
}

#[test]
fn cached_rankings_do_not_prevent_visiting_another_resource_page() {
    // Goal: cached resource pages cannot exhaust the Store's replaceable query contexts.
    // Method: retain Memory and CPU snapshots while requesting Energy through the real Worker.
    let mut harness = Harness::new();
    harness.worker.request(request("memory")).unwrap();
    let memory = harness.ready();
    harness.worker.request(request("cpu")).unwrap();
    let cpu = harness.ready();

    harness.worker.request(request("energy")).unwrap();
    let energy = harness.ready();

    assert!(memory.rankings.is_some());
    assert!(cpu.rankings.is_some());
    assert!(energy.rankings.is_some());
    assert_eq!(memory.selection["selected"], energy.selection["selected"]);
    assert_eq!(cpu.selection["selected"], energy.selection["selected"]);
    harness.assert_source_free();
}

#[test]
fn excessive_saved_core_states_are_refused_within_the_machine_scratch_allowance() {
    // Goal: a shape-valid saved state domain cannot allocate beyond admitted reducer scratch.
    // Method: supply a million distinct states, prepared outside the calling-thread measure.
    let before = core_reading(1, 1_000_000, 1);
    let current = core_reading(2, 1_000_000, 1);
    let budget = Arc::new(viewer::store::ByteBudget::new(16 * 1024 * 1024));

    let (result, allocated) = crate::allocation_tests::measure(|| {
        viewer::store::MachineSecond::complete_of(
            &viewer::store::readings(&current).next().unwrap(),
            Some(&viewer::store::readings(&before).next().unwrap()),
            &budget,
        )
    });

    let second = result.unwrap().unwrap();
    assert!(matches!(
        second.complete,
        viewer::store::CompleteMachineState::Refused("core level states")
    ));
    assert!(allocated.bytes <= 8 * 1024 * 1024, "{allocated:?}");
}

#[test]
fn a_full_core_state_domain_still_combines_other_cores_without_another_key() {
    // Goal: reaching the key limit preserves valid accumulation across cores.
    // Method: two cores fill 512 keys, gaining one unit at 1 GHz and three at 3 GHz.
    let before = core_reading_weighted(1);
    let current = core_reading_weighted(2);
    let budget = Arc::new(viewer::store::ByteBudget::new(16 * 1024 * 1024));

    let second = viewer::store::MachineSecond::complete_of(
        &viewer::store::readings(&current).next().unwrap(),
        Some(&viewer::store::readings(&before).next().unwrap()),
        &budget,
    )
    .unwrap()
    .unwrap();

    assert_eq!(
        second.complete().unwrap().summary(&second)["cpu_level_ghz"],
        serde_json::json!([2.5, null])
    );
}

fn core_reading_weighted(publication: u64) -> viewer::recording::MachineMinute {
    let mut reading = core_reading(publication, 512, 2);
    for row in &mut reading.core_states {
        let gained = match (row.core, row.state) {
            (12, 0) => 1,
            (13, 1) => 3,
            _ => 0,
        };
        row.active_ns = (publication - 1) * gained;
        row.frequency_khz = Some(match row.state {
            0 => 1_000_000,
            1 => 3_000_000,
            _ => 2_000_000,
        });
    }

    reading
}

fn core_reading(publication: u64, states: u32, cores: u32) -> viewer::recording::MachineMinute {
    use viewer::recording::{CoreStateRow, MachineMinute, MemoryRow};
    let core_states = (12..12 + cores)
        .flat_map(|core| {
            (0..states).map(move |state| CoreStateRow {
                publication,
                core,
                state,
                active_ns: publication,
                frequency_khz: Some(2_000_000),
            })
        })
        .collect();

    MachineMinute {
        ticks: vec![core_tick(publication)],
        cpus: (0..18)
            .map(|cpu| viewer::recording::CpuRow {
                publication,
                cpu,
                ticks: collector::machine::CPUTicks {
                    user: u32::try_from(publication).unwrap(),
                    idle: u32::try_from(publication).unwrap(),
                    ..collector::machine::CPUTicks::default()
                },
            })
            .collect(),
        memory: vec![MemoryRow {
            publication,
            memory: collector::machine::Memory::default(),
        }],
        core_states,
        ..MachineMinute::default()
    }
}

fn core_tick(publication: u64) -> viewer::recording::TickRow {
    viewer::recording::TickRow {
        publication,
        run_tick: publication,
        wall_ns: Some(publication * 1_000_000_000),
        continuous_ns: Some(publication * 1_000_000_000),
        awake_ns: Some(publication * 1_000_000_000),
        ioreport_read: true,
        smc_read: false,
        gpu_energy_nj: Some(0),
        power_watts: None,
        soc_celsius: None,
        cpu_celsius: None,
        gpu_celsius: None,
        thermal_pressure: None,
        performance_levels: Some(crate::live_store_fixtures::store_levels_named(
            "Fast", "Slow",
        )),
        system: Some(collector::ring::SystemSnapshot {
            machine: collector::system::MachineFacts {
                levels: Some(crate::live_store_fixtures::store_levels_named(
                    "Fast", "Slow",
                )),
                ..collector::system::MachineFacts::default()
            },
            ..collector::ring::SystemSnapshot::default()
        }),
        saved_computer: None,
        saved_load_average: None,
        battery: None,
        disks: None,
    }
}

struct Harness {
    worker: Worker,
    store: Option<viewer::store::Store>,
    recording: Option<LiveRecording>,
    trace: persistence::work::ingestion::Trace,
    scope: Option<persistence::work::ingestion::Scope>,
}

impl Harness {
    fn restart_source(&mut self) -> collector::ring::Identity {
        let recording = self.recording.as_mut().unwrap();
        let identity = recording.restart();
        recording.publish_resources();
        recording.publish_resources();

        identity.run
    }

    fn ready_for_run(&mut self, run: collector::ring::Identity) -> Box<Snapshot> {
        let run = collector::recording::identity_text(run).to_string();
        for _ in 0..8 {
            let snapshot = self.ready();
            if snapshot.selection["selected"]["run"] == run {
                return snapshot;
            }
        }

        panic!("Store Worker did not follow the replacement run");
    }

    fn sealed_blocked() -> (
        Self,
        viewer::store::ByteReservation,
        viewer::store::QueryHandle,
    ) {
        let mut recording = LiveRecording::new();
        recording.publish_resources();
        recording.publish_resources();
        seal_recording(&recording, 0);
        for _ in 0..62 {
            recording.publish_resources();
        }
        seal_recording(&recording, 1);
        let budget = Arc::new(viewer::store::ByteBudget::new(128 * 1024 * 1024));
        let lease = budget.reserve(budget.maximum()).unwrap();
        let trace = persistence::work::ingestion::Trace::new(
            collector::recording::read_identity(&recording.root).unwrap(),
        );
        let scope = trace.enter();
        let store = viewer::store::Store::start_with_budget(&recording.root, budget).unwrap();
        let handle = store.query_handle().unwrap();
        let worker = Worker::start_from_store(recording.root.clone(), handle.clone()).unwrap();
        (
            Self {
                worker,
                store: Some(store),
                recording: Some(recording),
                trace,
                scope: Some(scope),
            },
            lease,
            handle,
        )
    }

    fn ready_strict(&mut self) -> Box<Snapshot> {
        let deadline = Instant::now() + Duration::from_secs(10);
        for _ in 0..1000 {
            if let Some(update) = self.worker.take().unwrap() {
                match update {
                    Update::Ready(snapshot) => return snapshot,
                    Update::Failed { message, .. } => panic!("Store Worker failed: {message}"),
                }
            }
            assert!(Instant::now() < deadline, "Store Worker did not reply");
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("Store Worker exceeded bounded attempts");
    }
    fn new() -> Self {
        let mut recording = LiveRecording::new();
        recording.publish_resources();
        recording.publish_resources();
        let identity = collector::recording::read_identity(&recording.root).unwrap();
        let trace = persistence::work::ingestion::Trace::new(identity);
        let scope = trace.enter();
        let store = viewer::store::Store::start_with_budget(
            &recording.root,
            Arc::new(viewer::store::ByteBudget::new(128 * 1024 * 1024)),
        )
        .unwrap();
        let worker =
            Worker::start_from_store(recording.root.clone(), store.query_handle().unwrap())
                .unwrap();

        Self {
            worker,
            store: Some(store),
            recording: Some(recording),
            trace,
            scope: Some(scope),
        }
    }

    fn ready(&mut self) -> Box<Snapshot> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut failure = None;
        for _attempt in 0..1_000 {
            if let Some(update) = self.worker.take().unwrap() {
                match update {
                    Update::Ready(snapshot) => {
                        if !snapshot.histories_pending {
                            return snapshot;
                        }
                    }
                    Update::Failed { message, .. } => failure = Some(message),
                }
            }
            assert!(
                Instant::now() < deadline,
                "Store worker did not become ready: {failure:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("Store worker exceeded its bounded attempts: {failure:?}");
    }

    fn assert_source_free(&self) {
        use persistence::work::ingestion::Producer;
        let mut events = Vec::new();
        self.trace.snapshot(&mut events).unwrap();
        let reads = events
            .iter()
            .filter(|event| event.producer == Producer::Worker)
            .count();

        assert_eq!(reads, 0);
    }
}

fn seal_recording(recording: &LiveRecording, minute: u64) {
    use collector::ring::{Found, RingReader};
    let registry = collector::recording::Registry::read(&recording.root).unwrap();
    let registered = registry.runs.last().unwrap();
    let mut reader = RingReader::open_generation(&registered.ring_name).unwrap();
    let mut tick = persistence::tick_buffer(&reader.shape()).unwrap();
    let mut window = persistence::Window::new(
        persistence::WindowIdentity {
            run: registered.identity,
            window: minute,
        },
        persistence::Limits::from_shape(&reader.shape()).unwrap(),
    );
    for _ in 0..4 {
        let publication = match reader.read_into(&mut tick).unwrap() {
            Found::Publication { publication, .. } => publication,
            Found::Nothing => break,
            Found::Missed { .. } => panic!("seal fixture overwritten"),
        };
        assert!(tick.events.is_empty());
        let times = tick.times.unwrap();
        window
            .push_tick(
                &persistence::Observation {
                    publication,
                    wall_ns: Some(times.wall_ns),
                    continuous_ns: Some(times.continuous_ns),
                    awake_ns: Some(times.awake_ns),
                },
                &tick,
                &[],
            )
            .unwrap();
    }
    let store = persistence::Store::open(&recording.root).unwrap();
    let persistence::Begin::Pending(pending) = store.begin(&window.identity).unwrap() else {
        panic!("new seal");
    };
    let tables = persistence::Encoder
        .encode(&window, pending.path())
        .unwrap();
    let publications = window.publication_range();
    let first = window.observations().next().unwrap().wall_ns.unwrap();
    let last = window.observations().last().unwrap().wall_ns.unwrap();
    store
        .commit(
            pending,
            persistence::Manifest {
                identity: window.identity,
                publication_first: publications.start,
                publication_end: publications.end,
                events_first: 0,
                events_end: 0,
                wall_ns: Some(persistence::TimeBounds {
                    min: first,
                    max: last,
                }),
                continuous_ns: None,
                awake_ns: None,
                losses: persistence::Losses::default(),
                data_directory: String::new(),
                tables,
            },
        )
        .unwrap();
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.worker.stop();
        drop(self.store.take());
        drop(self.recording.take());
        drop(self.scope.take());
    }
}

fn request(route: &str) -> Request {
    Request {
        route: route.into(),
        second: None,
        identity: None,
        table: String::new(),
        offset: 0,
        app: None,
        apps_offset: 0,
        members_offset: 0,
        cpu_offset: 0,
        load_resident_history: false,
        load_app_attribution_history: false,
        load_process_history: false,
        earlier: Earlier::Skip,
        executable: None,
    }
}
