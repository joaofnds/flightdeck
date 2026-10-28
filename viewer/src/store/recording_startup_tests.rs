use super::*;

#[path = "startup_boundary_tests.rs"]
mod boundaries;

#[path = "startup_pending_tests.rs"]
mod pending;

#[test]
fn staged_machine_work_counts_the_refused_projection_and_its_compatibility_reduction() {
    // Goal: staged source-work accounting includes the existing compatibility fallback.
    // Method: decode a real saved NaN/null load triple and count both reductions per tick.
    let fixture = crate::recording::sealed_system_minute();
    refuse_saved_load(&fixture);
    let (found, machine) = machine_source(&fixture);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let trace = persistence::work::ingestion::Trace::new(found.run.recording);
    let scope = trace.enter();

    let mut seconds = Vec::new();
    payload::project_machine(&found, &machine, None, &budget, |second| {
        seconds.push(second);
    })
    .unwrap();

    assert_eq!(seconds.len(), 3);
    assert!(seconds.iter().all(|second| matches!(
        second.complete,
        crate::store::CompleteMachineState::Refused(_)
    )));
    assert_eq!(seconds.last().unwrap().cpu_percent, Some(75.0));
    assert_eq!(machine_work(&trace), 6);
    drop(scope);
}

#[test]
fn staged_machine_work_counts_an_admission_refused_attempt_without_accepting_it() {
    // Goal: budget denial cannot hide attempted machine work from the source trace.
    // Method: pass real decoded sealed rows through the existing zero-byte machine admission.
    let fixture = crate::recording::sealed_system_minute();
    let (found, machine) = machine_source(&fixture);
    let budget = Arc::new(ByteBudget::new(0));
    let trace = persistence::work::ingestion::Trace::new(found.run.recording);
    let scope = trace.enter();

    let result =
        payload::project_machine(&found, &machine, None, &budget, |_| {}).map_err(machine_failure);

    assert!(matches!(result, Err(Failure::Blocked)));
    assert_eq!(machine_work(&trace), 1);
    assert_eq!(budget.used(), 0);
    drop(scope);
}

fn machine_work(trace: &persistence::work::ingestion::Trace) -> usize {
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    events
        .iter()
        .filter(|event| event.kind == persistence::work::ingestion::Kind::MachineReduction)
        .count()
}

fn machine_source(fixture: &crate::live::identity_fixture::Fixture) -> (Found, MachineMinute) {
    let marker = persistence::layout::WindowKey {
        run: fixture.writer.identity().unwrap().run,
        window: 0,
    }
    .marker(&fixture.root);
    let manifest = persistence::read_committed(&fixture.root, &marker).unwrap();
    let sealed = ScopedSealedMinute::open(&fixture.root, &marker).unwrap();
    let mut machine = MachineMinute::default();
    read_machine(&sealed, &mut machine).unwrap();
    (
        Found {
            run: manifest.identity.run,
            marker,
            wall_ns_max: manifest.wall_ns.map(|wall| wall.max),
            publication_end: manifest.publication_end,
            previous_source: None,
        },
        machine,
    )
}

fn refuse_saved_load(fixture: &crate::live::identity_fixture::Fixture) {
    use polars::polars_utils::compression::ZstdLevel;
    use polars::prelude::{
        DataType, Null, ParquetCompression, ParquetWriter, StatisticsOptions, lit,
    };
    let marker = persistence::layout::WindowKey {
        run: fixture.writer.identity().unwrap().run,
        window: 0,
    }
    .marker(&fixture.root);
    let manifest = persistence::read_committed(&fixture.root, &marker).unwrap();
    let path = fixture
        .root
        .join(manifest.data_directory)
        .join("ticks.parquet");
    let mut frame = persistence::engine::scan(&path)
        .unwrap()
        .with_column(lit(f64::NAN).alias("load_average_1"))
        .with_column(lit(Null {}).cast(DataType::Float64).alias("load_average_5"))
        .with_column(lit(1.0).alias("load_average_15"))
        .collect()
        .unwrap();
    let replacement = fixture.root.join("changed_ticks.parquet");
    let file = collector::recording::private::open_private_file(&replacement, true).unwrap();
    ParquetWriter::new(file)
        .with_compression(ParquetCompression::Zstd(Some(
            ZstdLevel::try_new(3).unwrap(),
        )))
        .with_statistics(StatisticsOptions {
            min_value: true,
            max_value: true,
            distinct_count: false,
            null_count: true,
            binary_statistics_truncate_length: None,
        })
        .with_row_group_size(Some(16_384))
        .with_data_page_size(None)
        .set_parallel(false)
        .with_key_value_metadata(None)
        .with_context_info(None)
        .finish(&mut frame)
        .unwrap();
    std::fs::rename(replacement, path).unwrap();
}

#[test]
fn a_changed_manifest_after_machine_loading_refuses_the_process_upgrade() {
    // Goal: admitted machine headers cannot certify process input from another commitment.
    // Method: alter a real seal's loss word after the machine pass and before selected open.
    let fixture = crate::store::startup_tests::primary_fixture();
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut held = Held::with_payload(Arc::clone(&budget));
    held.startup = crate::store::StartupState::Loading;
    let mut ring = crate::store::native_acceptance::make_ring(&fixture.root, Some(&budget));
    let mut startup = Startup::new();
    let input = Input {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
    };
    for _ in 0..MINUTES_MAX {
        startup.advance(&input, &mut held, &mut ring).unwrap();
        if startup.phase == Phase::SelectedBoundary {
            break;
        }
    }
    assert!(startup.phase == Phase::SelectedBoundary);
    let marker = &startup.minutes[startup.selected.unwrap()].found.marker;
    change_loss_word(&fixture.root, marker);

    startup.advance(&input, &mut held, &mut ring).unwrap();
    let result = startup.advance(&input, &mut held, &mut ring);

    assert!(
        matches!(result, Err(Failure::Read(Refused::Commitment(error)))
        if error.kind() == std::io::ErrorKind::InvalidData)
    );
    assert!(held.startup == crate::store::StartupState::Loading);
    assert!(
        held.apps
            .held()
            .all(|apps| apps.missing == Some(Missing::Unread))
    );
    drop((startup, held, ring));
    assert_eq!(budget.used(), 0);
}

fn change_loss_word(root: &Path, marker: &Path) {
    let mut bytes = std::fs::read(marker).unwrap();
    // The format puts loss words after identities and the five window/range words.
    let first = b"FDSEAL01".len()
        + 2 * size_of::<u64>()
        + 3 * size_of::<collector::ring::Identity>()
        + 5 * size_of::<u64>();
    bytes[first..first + size_of::<u64>()].copy_from_slice(&1_u64.to_le_bytes());
    std::fs::write(marker, bytes).unwrap();
    assert_eq!(
        persistence::read_committed(root, marker)
            .unwrap()
            .losses
            .publications,
        1
    );
}

#[test]
fn sealed_candidate_survives_pool_denial_without_repeating_accepted_app_work() {
    // Goal: an admitted sealed result survives a real retained pool owner until installation.
    // Method: pin the selected placeholder, stall history twice, then release and finish.
    let fixture = crate::store::startup_tests::primary_fixture();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let trace =
        persistence::work::ingestion::Trace::new(fixture.writer.identity().unwrap().recording);
    let scope = trace.enter();
    let mut stage = GuardStage::new(&fixture, &budget);
    let pinned = stage.primary_with_no_spare(&fixture, &budget);
    let source_generation = stage.held.source_generation;
    let accepted = stage.accepted();
    let identity = stage.held.apps.get(0).unwrap().observation.unwrap();

    assert!(matches!(
        stage.advance(&fixture, &budget).unwrap(),
        Advance::Progress
    ));
    assert!(matches!(
        stage.advance(&fixture, &budget).unwrap(),
        Advance::Blocked
    ));
    assert_eq!(stage.accepted(), accepted);
    assert!(matches!(
        stage.advance(&fixture, &budget).unwrap(),
        Advance::Blocked
    ));
    assert_eq!(stage.accepted(), accepted);
    assert_eq!(app_work(&trace, &identity), 1);
    drop(pinned);
    stage.complete(&fixture, &budget);

    assert_eq!(stage.held.source_generation, source_generation);
    assert!(stage.held.startup == crate::store::StartupState::Complete);
    assert_eq!(app_work(&trace, &identity), 1);
    assert_stalled_identity(&stage.held, &identity);
    let apps = stage.held.apps.get(1).unwrap();
    assert_eq!(apps.observation.unwrap().stamp.publication, 2);
    assert_eq!(apps.elapsed_ns, Some(1_000_000_000));
    let payload = apps.processes.as_ref().unwrap();
    assert_eq!(payload.previous.unwrap().publication, 1);
    let process = payload.iter().find(|process| process.pid == 7).unwrap();
    assert_eq!(process.disk_read_bytes(), crate::apps::Amount::Known(17));
    assert_eq!(process.disk_written_bytes(), crate::apps::Amount::Known(43));
    let ranks = apps.ranks().unwrap();
    let disk = ranks
        .metrics
        .iter()
        .find(|rank| rank.metric == crate::live_app_response::Metric::Disk)
        .unwrap();
    let app = disk
        .entries
        .iter()
        .find(|entry| entry.app.bytes.as_slice() == b"/Apps/Current.app")
        .unwrap();
    assert_eq!(app.amount, 60);
    assert!((app.rate.unwrap() - 60.0).abs() <= f64::EPSILON * 60.0);
    drop((stage, ranks, scope));
    assert_eq!(budget.used(), 0);
}

fn assert_stalled_identity(held: &Held, identity: &Observation) {
    let apps = held.apps.get(0).unwrap();
    assert_eq!(apps.observation, Some(*identity));
    assert_eq!(apps.missing, None);
    let payload = apps.processes.as_ref().unwrap();
    let process = payload.iter().find(|process| process.pid == 7).unwrap();
    assert_eq!(process.resident_bytes(), crate::apps::Amount::Known(11));
    assert_eq!(process.footprint_bytes(), crate::apps::Amount::Known(12));
    let ranks = apps.ranks().unwrap();
    assert_eq!(ranks.selected, *identity);
    let resident = ranks
        .metrics
        .iter()
        .find(|rank| rank.metric == crate::live_app_response::Metric::Resident)
        .unwrap();
    let app = resident
        .entries
        .iter()
        .find(|entry| entry.app.bytes.as_slice() == b"/Apps/Current.app")
        .unwrap();
    assert_eq!(app.amount, 11);
}

#[test]
fn sealed_candidate_rejects_a_changed_witness_before_installation() {
    // Goal: a pending admitted sealed result cannot certify a changed source.
    // Method: stall history with a real pool owner, change its seal, then release the owner.
    let fixture = crate::store::startup_tests::primary_fixture();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut stage = GuardStage::new(&fixture, &budget);
    let pinned = stage.primary_with_no_spare(&fixture, &budget);
    stage.advance(&fixture, &budget).unwrap();
    assert!(matches!(
        stage.advance(&fixture, &budget).unwrap(),
        Advance::Blocked
    ));
    let accepted = stage.accepted();
    let source_generation = stage.held.source_generation;
    let marker = persistence::layout::WindowKey {
        run: fixture.writer.identity().unwrap().run,
        window: 0,
    }
    .marker(&fixture.root);
    change_loss_word(&fixture.root, &marker);
    drop(pinned);

    let result = stage.advance(&fixture, &budget);

    assert!(
        matches!(result, Err(Failure::Read(Refused::Commitment(error)))
        if error.kind() == std::io::ErrorKind::InvalidData)
    );
    assert_eq!(stage.accepted(), accepted);
    assert_eq!(stage.held.source_generation, source_generation);
    assert!(stage.held.startup == crate::store::StartupState::Primary);
    drop(stage);
    assert_eq!(budget.used(), 0);
}

struct GuardStage {
    operation: Operation,
    startup: Startup,
    held: Held,
    ring: crate::store::Ring,
}

impl GuardStage {
    fn new(fixture: &crate::live::identity_fixture::Fixture, budget: &Arc<ByteBudget>) -> Self {
        let mut held = Held::with_payload(Arc::clone(budget));
        held.startup = crate::store::StartupState::Loading;
        Self {
            operation: Arc::new(crate::Cancellation::default())
                .operation(0)
                .unwrap(),
            startup: Startup::new(),
            held,
            ring: crate::store::native_acceptance::make_ring(&fixture.root, Some(budget)),
        }
    }

    fn advance(
        &mut self,
        fixture: &crate::live::identity_fixture::Fixture,
        budget: &Arc<ByteBudget>,
    ) -> Result<Advance, Failure> {
        self.startup.advance(
            &Input {
                root: &fixture.root,
                budget,
                operation: &self.operation,
            },
            &mut self.held,
            &mut self.ring,
        )
    }

    fn primary_with_no_spare(
        &mut self,
        fixture: &crate::live::identity_fixture::Fixture,
        budget: &Arc<ByteBudget>,
    ) -> Arc<AppsSecond> {
        self.held.apps =
            crate::store::Span::new(6, crate::store::Buffers::new(7, AppsSecond::default));
        for _ in 0..MINUTES_MAX {
            self.advance(fixture, budget).unwrap();
            if self.startup.phase == Phase::SelectedBoundary {
                break;
            }
        }
        assert!(self.startup.phase == Phase::SelectedBoundary);
        let pinned = Arc::clone(self.held.apps.newest_held().unwrap());
        crate::store::startup_tests::drive_primary(
            &mut self.startup,
            &Input {
                root: &fixture.root,
                budget,
                operation: &self.operation,
            },
            &mut self.held,
            &mut self.ring,
        );
        assert_eq!(self.held.apps.free(), 0);
        pinned
    }

    fn complete(
        &mut self,
        fixture: &crate::live::identity_fixture::Fixture,
        budget: &Arc<ByteBudget>,
    ) {
        crate::store::startup_tests::drive_complete(
            &mut self.startup,
            &Input {
                root: &fixture.root,
                budget,
                operation: &self.operation,
            },
            &mut self.held,
            &mut self.ring,
        );
    }

    fn accepted(&self) -> Vec<(Option<Observation>, Option<Missing>)> {
        self.held
            .apps
            .held()
            .map(|apps| (apps.observation, apps.missing.clone()))
            .collect()
    }
}

fn app_work(trace: &persistence::work::ingestion::Trace, observation: &Observation) -> usize {
    use persistence::work::ingestion::{Key, Kind};
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    events
        .iter()
        .filter(|event| {
            event.kind == Kind::AppDerivation
                && matches!(event.key,
            Key::Publication { run, publication }
            if run.recording.0 == observation.recording
                && run.boot.0 == observation.stamp.boot
                && run.run.0 == observation.stamp.run
                && publication == observation.stamp.publication)
        })
        .count()
}

#[test]
fn endpoint_stage_accepts_the_legal_61_endpoint_control() {
    // Goal: early header validation admits the full legal boundary.
    // Method: supply 61 ordered saved ticks with otherwise valid machine and header columns.
    let fixture = endpoint_fixture(61, false);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut stage = GuardStage::new(&fixture, &budget);
    stage.advance(&fixture, &budget).unwrap();

    assert!(matches!(
        stage.advance(&fixture, &budget).unwrap(),
        Advance::Progress
    ));

    assert_eq!(stage.held.span.len(), 61);
    assert_eq!(stage.held.apps.len(), 61);
    assert_eq!(stage.held.span.newest().unwrap().publication, 61);
    drop(stage);
    assert_eq!(budget.used(), 0);
}

#[test]
fn endpoint_stage_refuses_the_62nd_sentinel_before_accepting_machine_state() {
    // Goal: the sentinel header cannot become accepted machine or App state.
    // Method: save 62 otherwise legal ordered endpoints and drive discovery and machine loading.
    assert_endpoint_refusal(62, false, "committed App batch endpoints");
}

#[test]
fn endpoint_stage_refuses_a_reversed_event_range_before_accepting_machine_state() {
    // Goal: process header integrity is checked before machine or source acceptance.
    // Method: reverse one saved event range while preserving every other valid input field.
    assert_endpoint_refusal(1, true, "committed App event range");
}

fn assert_endpoint_refusal(count: u64, reversed: bool, reason: &'static str) {
    let fixture = endpoint_fixture(count, reversed);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let trace =
        persistence::work::ingestion::Trace::new(fixture.writer.identity().unwrap().recording);
    let scope = trace.enter();
    let mut stage = GuardStage::new(&fixture, &budget);
    stage.advance(&fixture, &budget).unwrap();

    let result = stage.advance(&fixture, &budget);

    assert!(
        matches!(result, Err(Failure::Read(Refused::Decode(Error::Bound(value))))
        if value == reason)
    );
    assert!(stage.held.span.is_empty());
    assert!(stage.held.apps.is_empty());
    assert!(stage.held.checked_catalog.is_none());
    assert_eq!(stage.held.source_generation, 0);
    assert!(stage.held.startup == crate::store::StartupState::Loading);
    assert_eq!(machine_work(&trace), 0);
    drop((stage, scope));
    assert_eq!(budget.used(), 0);
}

fn endpoint_fixture(count: u64, reversed: bool) -> crate::live::identity_fixture::Fixture {
    use polars::prelude::{Column, DataType, IntoLazy, lit};
    let mut fixture = crate::live::identity_fixture::Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.seal();
    let marker = persistence::layout::WindowKey {
        run: fixture.writer.identity().unwrap().run,
        window: 0,
    }
    .marker(&fixture.root);
    let manifest = persistence::read_committed(&fixture.root, &marker).unwrap();
    let path = fixture
        .root
        .join(manifest.data_directory)
        .join("ticks.parquet");
    let row = persistence::engine::scan(&path).unwrap().collect().unwrap();
    let mut frame = row.clone();
    for _ in 1..count {
        frame.vstack_mut(&row).unwrap();
    }
    frame
        .with_column(Column::new(
            "publication".into(),
            (1..=count).collect::<Vec<_>>(),
        ))
        .unwrap();
    frame
        .with_column(Column::new(
            "run_tick".into(),
            (0..count).collect::<Vec<_>>(),
        ))
        .unwrap();
    let mut frame = frame
        .lazy()
        .with_column(
            lit(u64::from(reversed))
                .cast(DataType::UInt64)
                .alias("events_first"),
        )
        .with_column(lit(0_u64).cast(DataType::UInt64).alias("events_end"))
        .collect()
        .unwrap();
    let replacement = fixture.root.join("changed_ticks.parquet");
    write_ticks(&replacement, &mut frame);
    std::fs::rename(replacement, path).unwrap();
    fixture
}

fn write_ticks(path: &Path, frame: &mut polars::prelude::DataFrame) {
    use polars::polars_utils::compression::ZstdLevel;
    use polars::prelude::{ParquetCompression, ParquetWriter, StatisticsOptions};
    let file = collector::recording::private::open_private_file(path, true).unwrap();
    ParquetWriter::new(file)
        .with_compression(ParquetCompression::Zstd(Some(
            ZstdLevel::try_new(3).unwrap(),
        )))
        .with_statistics(StatisticsOptions {
            min_value: true,
            max_value: true,
            distinct_count: false,
            null_count: true,
            binary_statistics_truncate_length: None,
        })
        .with_row_group_size(Some(16_384))
        .with_data_page_size(None)
        .set_parallel(false)
        .with_key_value_metadata(None)
        .with_context_info(None)
        .finish(frame)
        .unwrap();
}

#[test]
fn selected_startup_payloads_share_one_input_name_owner_after_startup_retires() {
    // Goal: the staged startup caller uses the same bounded ownership rule as full loading.
    // Method: retain both selected-minute payloads, inspect per-capture work, and retire startup.
    let fixture = crate::store::startup_tests::primary_fixture();
    let operation = Arc::new(crate::Cancellation::default())
        .operation(0)
        .unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let observer = Arc::new(ByteBudget::new(64 * 1024));
    let trace = crate::store::payload::NameTrace::new(4, &observer);
    let mut held = Held::with_payload(Arc::clone(&budget));
    held.startup = crate::store::StartupState::Loading;
    let mut ring = crate::store::native_acceptance::make_ring(&fixture.root, Some(&budget));
    let mut startup = Startup::new();
    let input = Input {
        root: &fixture.root,
        budget: &budget,
        operation: &operation,
    };

    crate::store::startup_tests::drive_primary(&mut startup, &input, &mut held, &mut ring);
    let first = Arc::clone(held.apps.get(4).unwrap().processes.as_ref().unwrap());
    let second = Arc::clone(held.apps.get(5).unwrap().processes.as_ref().unwrap());
    let records = trace.finish();

    assert_eq!(first.selected.stamp.publication, 5);
    assert_eq!(second.selected.stamp.publication, 6);
    assert_eq!(first.selected.run_tick, 120);
    assert_eq!(second.selected.run_tick, 121);
    assert_selected_payload_identity(&first, &fixture.writer.identity().unwrap());
    assert_selected_payload_identity(&second, &fixture.writer.identity().unwrap());
    assert_eq!(first.previous.unwrap().publication, 4);
    assert_eq!(second.previous.unwrap().publication, 5);
    assert_eq!(records.values().len(), 2);
    assert_eq!(records.values()[0].index_capacity, 9);
    assert_eq!(records.values()[1].index_capacity, 9);
    assert_eq!(records.values()[0].work.new_names, 2);
    assert_eq!(records.values()[1].work.new_names, 0);
    let mut name_bytes = 0;
    for (first, second) in first.iter().zip(second.iter()) {
        assert_eq!(first.pid, second.pid);
        let first_name = first.executable_name().unwrap();
        assert!(std::ptr::eq(first_name, second.executable_name().unwrap()));
        name_bytes += first_name.reserved_bytes();
    }
    println!("startup_name_work={:?}", records.values());
    drop((startup, held, ring, fixture));
    assert_eq!(budget.used(), first.bytes() + second.bytes() + name_bytes);
    drop((first, second, records));
    assert_eq!(budget.used(), 0);
    assert_eq!(observer.used(), 0);
}

fn assert_selected_payload_identity(
    payload: &crate::store::Payload,
    identity: &collector::ring::RunIdentity,
) {
    assert_eq!(payload.selected.recording, identity.recording.0);
    assert_eq!(payload.selected.stamp.run, identity.run.0);
    assert_eq!(payload.selected.stamp.boot, identity.boot.0);
    let continuous_ns = payload.selected.run_tick * 1_000_000_000;
    assert_eq!(payload.selected.stamp.continuous_ns, Some(continuous_ns));
    assert_eq!(payload.selected.stamp.awake_ns, Some(continuous_ns));
    assert_eq!(
        payload.selected.wall_ns,
        Some((1000 + payload.selected.run_tick) * 1_000_000_000)
    );
    assert_eq!(payload.len(), 2);
    for (row, (pid, name)) in payload
        .iter()
        .zip([(1, &b"refused\xff"[..]), (7, &b"current"[..])])
    {
        assert_eq!(row.pid, pid);
        assert_eq!(
            row.creation,
            Some(collector::ring::ProcessCreation {
                unique_id: u64::try_from(pid).unwrap(),
                pid_version: -pid,
            })
        );
        assert_eq!(row.executable_name().unwrap().bytes, name);
    }
}
