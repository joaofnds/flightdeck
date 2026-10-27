use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::live_dashboard::input_tests::source_mode::LiveRecording;
use crate::live_source::{Earlier, Request, Snapshot, Update, Worker};

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
                    Update::Ready(snapshot) => return snapshot,
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
        use persistence::work::ingestion::{Kind, Producer};
        let mut events = Vec::new();
        self.trace.snapshot(&mut events).unwrap();
        let reads = events
            .iter()
            .filter(|event| event.producer == Producer::Worker)
            .filter(|event| {
                matches!(
                    event.kind,
                    Kind::RingReadAttempt
                        | Kind::RingRead
                        | Kind::FrameCaptureAttempt
                        | Kind::FrameCapture
                        | Kind::AppDerivation
                )
            })
            .count();

        assert_eq!(reads, 0);
    }
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
