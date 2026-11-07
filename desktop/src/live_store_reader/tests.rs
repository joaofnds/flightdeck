use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::live_dashboard::input_tests::source_mode::LiveRecording;
use crate::live_source::{Earlier, Request, Snapshot, Update, Worker};

#[path = "lifecycle_tests.rs"]
mod lifecycle;

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

#[test]
fn a_held_second_is_asked_for_again_when_its_source_changes() {
    // Goal: a reader whose context names a source that ended asks anew, rather than name that
    // source until its wait ends and the saved minutes answer.
    // Method: restart the real source while the Primary of a held second waits at the Worker's
    // Pending checkpoint, let the Store take in the restart, and trace what the Worker read.
    let (mut harness, primary, hold) = primary_sealed(Some(1063));
    let source = primary.catalog["source_generation"].as_u64().unwrap();
    let selected = primary.selection["selected"].clone();
    drop(primary);
    harness.restart_source();
    harness.wait_until_the_store_reads_past(source);

    hold.release();
    let current = harness.ready_strict();

    assert_eq!(current.selection["selected"], selected);
    assert!(current.catalog["source_generation"].as_u64().unwrap() > source);
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
fn a_new_read_waits_for_a_store_behind_the_newest_reading() {
    // Goal: a read that reaches the Store just before it reads a new reading is not a failure.
    // Method: keep the Store a reading behind while the Worker asks, then let it catch up.
    let mut harness = Harness::new();
    harness.worker.request(request("memory")).unwrap();
    harness.ready();
    harness.store_behind_the_newest_reading();

    let generation = harness.worker.request(request("memory")).unwrap();

    harness.assert_no_update_over_several_polls();
    harness.store_active();
    assert_eq!(harness.ready_strict().generation, generation);
}

#[test]
fn a_kept_refresh_waits_for_a_store_behind_the_newest_reading() {
    // Goal: a held read that reaches the Store just before it reads a new reading stays held.
    // Method: refresh a held Primary while the Store stays a reading behind, then let it catch up.
    let (mut harness, primary, hold) = primary_sealed(Some(1063));
    harness.store_behind_the_newest_reading();
    let mut inspection = request("memory");
    inspection.second = Some(1063);
    inspection.identity = Some(primary.selection["selected"].clone());

    let generation = harness.worker.inspect(inspection).unwrap();
    hold.release();
    drop(hold);

    harness.assert_no_update_over_several_polls();
    harness.store_active();
    assert_eq!(harness.ready_strict().generation, generation);
}

#[test]
fn a_context_limit_met_for_a_moment_holds_the_page_without_a_failure() {
    // Goal: the window holds the context it shows while the next one arrives, which fills the
    // Store's two for a moment and is no failure of the page.
    // Method: keep both contexts while the Worker asks, then let one go.
    let mut harness = Harness::new();
    let (first, second) = harness.both_contexts_held();

    let generation = harness.worker.request(request("memory")).unwrap();

    harness.assert_no_update_over_several_polls();
    drop(first);
    assert_eq!(harness.ready_strict().generation, generation);
    drop(second);
}

#[test]
fn a_context_limit_that_lasts_says_so() {
    // Goal: a page never waits for ever on contexts something else keeps.
    // Method: keep both contexts while the Worker asks, past its bounded wait.
    let mut harness = Harness::new();
    let (first, second) = harness.both_contexts_held();

    harness.worker.request(request("memory")).unwrap();

    assert_eq!(
        harness.failure(),
        "Store readings unavailable: ContextLimit"
    );
    drop((first, second));
}

#[test]
fn an_inspected_overview_reading_needs_no_store_caught_up() {
    // Goal: a picked reading of the Overview shows at once from what the Store already answered.
    // Method: pick the older of two readings while the Store stays behind the newest one.
    let mut harness = Harness::new();
    harness.worker.request(request("overview")).unwrap();
    let shown = harness.ready();
    let older = shown.selection["trends"]
        .as_array()
        .unwrap()
        .iter()
        .find(|point| point["publication"] == 1)
        .unwrap()
        .clone();
    harness.store_behind_the_newest_reading();
    let mut inspection = request("overview");
    inspection.second = Some(older["wall_ns"].as_u64().unwrap() / 1_000_000_000);
    inspection.identity = Some(older);

    harness.worker.inspect(inspection).unwrap();

    let inspected = harness.ready_strict();
    assert_eq!(inspected.selection["selected"]["publication"], 1);
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

    assert!(
        super::read(
            &mut reader,
            std::path::Path::new(""),
            &mut command,
            &Shared::new()
        )
        .is_none()
    );

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
        gpu_state_count: 0,
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

    fn wait_until_the_store_reads_past(&self, source: u64) {
        let handle = self.store.as_ref().unwrap().query_handle().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        for _ in 0..1000 {
            if Self::ready_context(&handle).source.generation > source {
                return;
            }
            assert!(Instant::now() < deadline, "the Store kept its source");
        }
        panic!("the Store kept its source within its attempts");
    }

    fn store_behind_the_newest_reading(&mut self) {
        let store = self.store.as_ref().unwrap();
        store.set_active(false);
        Self::wait_until_paused(store);
        self.recording.as_mut().unwrap().publish_resources();
    }

    fn wait_until_paused(store: &viewer::store::Store) {
        let deadline = Instant::now() + Duration::from_secs(10);
        for _ in 0..1000 {
            if store.is_paused() {
                break;
            }
            assert!(Instant::now() < deadline, "Store never paused");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(store.is_paused());
    }

    fn store_active(&self) {
        self.store.as_ref().unwrap().set_active(true);
    }

    fn assert_no_update_over_several_polls(&mut self) {
        for _ in 0..3 {
            std::thread::sleep(crate::live_source::POLL_INTERVAL);
            match self.worker.take().unwrap() {
                None => {}
                Some(Update::Failed { message, .. }) => panic!("Store Worker failed: {message}"),
                Some(Update::Ready(_)) => panic!("Store Worker answered from a Store behind"),
            }
        }
    }

    fn both_contexts_held(
        &self,
    ) -> (
        Arc<viewer::store::QueryContext>,
        Arc<viewer::store::QueryContext>,
    ) {
        let handle = self.store.as_ref().unwrap().query_handle().unwrap();
        let first = Self::ready_context(&handle);
        let second = Self::ready_context(&handle);
        assert_eq!(handle.contexts_held(), 2);

        (first, second)
    }

    fn ready_context(handle: &viewer::store::QueryHandle) -> Arc<viewer::store::QueryContext> {
        let deadline = Instant::now() + Duration::from_secs(10);
        for _attempt in 0..1_000 {
            handle
                .submit(viewer::store::QueryRequest {
                    epoch: 0,
                    source: None,
                    selection: viewer::store::QuerySelection::FollowLatest,
                })
                .unwrap();
            std::thread::sleep(Duration::from_millis(10));
            if let Some(reply) = handle.take() {
                if let viewer::store::QueryOutcome::Ready(context) = reply.outcome {
                    return context;
                }
            }
            assert!(Instant::now() < deadline, "the Store gave no context");
        }
        panic!("the Store gave no context within its attempts");
    }

    fn failure(&mut self) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
        for _attempt in 0..2_000 {
            match self.worker.take().unwrap() {
                None => {}
                Some(Update::Failed { message, .. }) => return message,
                Some(Update::Ready(_)) => panic!("the Worker read past the limit"),
            }
            assert!(Instant::now() < deadline, "the Worker did not fail");
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the Worker did not fail within its attempts");
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

/// Derives no table, as a window saved before derived tables holds none.
struct SealUnderived;

impl persistence::worker::Derive for SealUnderived {
    fn derive(
        &self,
        _derivation: &persistence::worker::Derivation<'_>,
    ) -> persistence::Result<Vec<persistence::TableManifest>> {
        Ok(Vec::new())
    }
}

/// Saves what the ring holds of the run through `minute`, with the persister's own worker.
fn seal_recording(recording: &LiveRecording, minute: u64) {
    let registry = collector::recording::Registry::read(&recording.root).unwrap();
    let store = persistence::Store::open(&recording.root).unwrap();
    persistence::worker::seal_held_windows(
        &store,
        registry.runs.last().unwrap(),
        Some(minute),
        &mut persistence::worker::HeldWindows::default(),
        &SealUnderived,
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
