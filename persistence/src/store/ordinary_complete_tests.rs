//! Complete nonempty raw payloads and independent field controls.
use super::ordinary_adapter_tests::encoder_fixture;
use super::*;

#[path = "ordinary_metric_oracle.rs"]
mod ordinary_metric_oracle;

#[path = "ordinary_supplemental_tests.rs"]
mod ordinary_supplemental_tests;

fn metric_fixture(captured: bool, slots: u64) -> Fixture {
    metric_fixture_with_processes(captured, slots, 40)
}

fn metric_fixture_with_processes(captured: bool, slots: u64, processes: u64) -> Fixture {
    let mut fixture = Fixture::new_profile(false, captured);
    let place = collector::ring::Place {
        lock_path: fixture.base.join("metrics.lock"),
        name_prefix: format!(
            "/fdmetric{:x}{:x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ),
    };
    let mut shape = encoder_fixture::profile(false);
    shape.slot_count = slots;
    shape.event_capacity = 4;
    shape.process_capacity = processes;
    let writer = if captured {
        collector::ring::Writer::create_with_identity(
            &place,
            shape,
            collector::ring::DictionaryCapacity {
                entries: 4,
                arena_bytes: 16 * 1024,
            },
            &fixture.run,
            1,
        )
        .unwrap()
    } else {
        collector::ring::Writer::create_recording(&place, shape, &fixture.run, 1).unwrap()
    };
    let previous = fixture.writer.name().to_owned();
    fixture.writer = writer;
    collector::ring::unlink_generation(&previous).unwrap();
    fixture.place = place;
    fixture.binding = fixture.reader().object_binding().unwrap();
    fixture.source = crate::transaction::SourceTiming::from_reader(&fixture.reader());
    publish_at(&mut fixture.writer, 0);
    fixture
}
fn metric_publish(fixture: &mut Fixture, run_tick: u64, events: bool, optional: bool) {
    metric_publish_with_supplemental(
        fixture,
        run_tick,
        events,
        optional,
        &collector::ring::SupplementalReadings::default(),
    );
}
fn metric_publish_with_supplemental(
    fixture: &mut Fixture,
    run_tick: u64,
    events: bool,
    optional: bool,
    supplemental: &collector::ring::SupplementalReadings<'_>,
) {
    let tick = encoder_fixture::tick(&fixture.reader().shape());
    let cores: Vec<_> = (0..tick.core_count())
        .map(|index| collector::ioreport::States::with_active_ns(tick.core_active_ns(index)))
        .collect();
    let captured = fixture.writer.dictionary_usage().unwrap().is_some();
    let identities = metric_publish_identities(fixture, &tick);
    let event = std::array::from_fn::<_, 2, _>(|_| collector::tracker::Event::Exec {
        pid: tick.processes[0].pid,
    });
    let event_identities = [if captured {
        collector::ring::IdentityObservation {
            usage: collector::ring::UsageAvailability::NotApplicable,
            ..identities[0]
        }
    } else {
        collector::ring::IdentityObservation::EMPTY
    }; 2];
    let readings = collector::ring::Readings {
        run_tick,
        cpus: &tick.cpus,
        memory: &tick.memory,
        interfaces: &tick.interfaces,
        ioreport: Some(collector::ring::IOReportReadings {
            gpu_energy_nj: tick.gpu_energy_nj,
            gpu_active_ns: &tick.gpu_active_ns,
            cores: &cores,
        }),
        smc: optional.then_some(collector::ring::SmcReadings {
            power_watts: 42.25,
            soc_celsius: 51.5,
            cpu_celsius: Some(52.25),
            gpu_celsius: None,
            fan_rpm: &tick.fan_rpm,
        }),
        processes: &tick.processes,
        processes_refused: 0,
        processes_gone: 0,
        coalitions: &tick.coalitions,
        events: if events { &event } else { &[] },
        dropped: collector::ring::Dropped::default(),
    };
    let times = collector::ring::ObservationTimes {
        wall_ns: run_tick + 10,
        continuous_ns: run_tick.min(59) * 1_000_000_000 + 1,
        awake_ns: run_tick * 1_000_000_000 + 3,
    };
    let identities = collector::ring::IdentityPublication {
        processes: &identities,
        events: if events { &event_identities } else { &[] },
    };
    metric_publish_output(
        &mut fixture.writer,
        captured,
        &readings,
        &times,
        &identities,
        supplemental,
    );
}
fn metric_publish_output(
    writer: &mut collector::ring::Writer,
    captured: bool,
    readings: &collector::ring::Readings<'_>,
    times: &collector::ring::ObservationTimes,
    identities: &collector::ring::IdentityPublication<'_>,
    supplemental: &collector::ring::SupplementalReadings<'_>,
) {
    if supplemental.battery.is_some()
        || supplemental.disks.is_some()
        || supplemental.system.is_some()
    {
        writer
            .publish_with_disks(readings, Some(times), identities, supplemental)
            .unwrap();
    } else if captured {
        writer
            .publish_with_identity(readings, Some(times), identities)
            .unwrap();
    } else {
        writer.publish_observed(readings, times);
    }
}

fn metric_publish_identities(
    fixture: &mut Fixture,
    tick: &collector::ring::Tick,
) -> Vec<collector::ring::IdentityObservation> {
    let mut program = None;
    let captured = fixture.writer.dictionary_usage().unwrap().is_some();
    if captured {
        fixture
            .writer
            .intern_program(
                &collector::ring::ProgramBytes {
                    executable_name: b"metric-\xff",
                    app_path: Some(b"/Apps/Metric-\xfe.app"),
                },
                &mut program,
            )
            .unwrap();
    }
    let identities: Vec<_> = tick
        .processes
        .iter()
        .filter(|_| captured)
        .map(|process| collector::ring::IdentityObservation {
            pid: process.pid,
            usage: collector::ring::UsageAvailability::Measured,
            identity: collector::ring::ProcessIdentity::Measured {
                creation: collector::ring::ProcessCreation {
                    unique_id: u64::try_from(process.pid).unwrap() + 100,
                    pid_version: -7,
                },
                program: program.unwrap(),
            },
        })
        .collect();
    identities
}

fn complete_input(fixture: &Fixture) -> AttemptInput {
    let mut input = fixture.input(false);
    input.losses.unknown = None;
    input.operation = AttemptOperation::Ordinary {
        window: 0,
        attempted_origin: 2,
    };
    input.losses.publication_inherited_end = 2;
    input.losses.publications = 1;
    input
}
fn complete_payload(attempt: &RegisteredAttempt<'_>, fixture: &Fixture, count: usize) -> Manifest {
    let (window, last) = complete_payload_window(attempt, fixture, count);
    let identity = window.identity;
    let mut reader = fixture.reader();
    let mut tables = Vec::with_capacity(10);
    for table in crate::schema::TABLES {
        if matches!(table, crate::schema::Table::Events) {
            continue;
        }
        let mut frame = crate::encoder::frame(table, &window)
            .unwrap()
            .sort(
                table.catalog().key.to_vec(),
                polars::prelude::SortMultipleOptions {
                    multithreaded: false,
                    maintain_order: true,
                    nulls_last: vec![true],
                    ..Default::default()
                },
            )
            .unwrap();
        tables.push(crate::TableManifest {
            name: table.name(),
            rows: collector::u64_from_usize(frame.height()),
        });
        crate::encoder::encode_write(
            &mut frame,
            attempt
                .create_file(&format!("{}.parquet", table.name()))
                .unwrap(),
        )
        .unwrap();
    }
    reader.seek_publication(2).unwrap();
    let first_tick = encode_payload_tick(&mut reader, 2);
    let rows = encode_original_rows(attempt, &mut reader, &first_tick);
    let index = encode_original_index(attempt, &rows, &last, identity);
    tables.push(crate::TableManifest {
        name: "events",
        rows: index.rows,
    });
    Manifest {
        identity,
        publication_first: 2,
        publication_end: 2 + collector::u64_from_usize(count),
        events_first: index.first,
        events_end: index.end,
        wall_ns: crate::metadata::clock_bounds(window.observations().map(|value| value.wall_ns)),
        continuous_ns: crate::metadata::clock_bounds(
            window.observations().map(|value| value.continuous_ns),
        ),
        awake_ns: crate::metadata::clock_bounds(window.observations().map(|value| value.awake_ns)),
        losses: crate::Losses {
            publications: 1,
            events: 0,
            unknown: false,
        },
        data_directory: format!(
            "data/{}/ready",
            collector::recording::identity_text(attempt.transaction())
        ),
        tables,
        event_batches: Some(1),
    }
}
fn complete_payload_window(
    attempt: &RegisteredAttempt<'_>,
    fixture: &Fixture,
    count: usize,
) -> (Window, collector::ring::Tick) {
    let mut reader = fixture.reader();
    let identity = WindowIdentity {
        run: fixture.run,
        window: 0,
    };
    let mut window = Window::new(identity, Limits::from_shape(&reader.shape()).unwrap());
    let mut last = None;
    for offset in 0..count {
        let publication = 2 + collector::u64_from_usize(offset);
        let mut tick = crate::tick_buffer(&reader.shape()).unwrap();
        let mut identities =
            Vec::with_capacity(usize::try_from(reader.shape().process_capacity).unwrap());
        reader.seek_publication(publication).unwrap();
        let found = if attempt.input().captured {
            reader
                .read_identity_into(&mut tick, &mut identities)
                .unwrap()
        } else {
            reader.read_into(&mut tick).unwrap()
        };
        assert!(matches!(found,
                collector::ring::Found::Publication { publication: actual, .. }
                    if actual == publication));
        let observation = original_observation(publication, &tick);
        if attempt.input().captured {
            window
                .push_tick_with_identity(
                    &observation,
                    &tick,
                    &[],
                    &crate::IdentitySource {
                        reader: &reader,
                        processes: &identities,
                        events: &[],
                    },
                )
                .unwrap();
        } else {
            window.push_tick(&observation, &tick, &[]).unwrap();
        }
        last = Some(tick);
    }
    (window, last.unwrap())
}

fn finish_recovery(owner: &mut OrdinaryRecovery<'_>) {
    for _ in 0..200_000 {
        if owner.step().unwrap() == PublicationStep::Idle {
            return;
        }
    }
    panic!("ordinary recovery did not finish within finite fixture bound");
}
fn check_metrics(
    object: &Path,
    captured: bool,
    count: usize,
    budget: &Arc<crate::resources::ByteBudget>,
) {
    ordinary_metric_oracle::check(object, captured, count, budget);
    let directory = PrivateDirectory::open(&object.join("ready")).unwrap();
    for (name, column, expected) in [("memory", "total_bytes", 1), ("coalitions", "cpu_ns", 3)] {
        let table = crate::engine::AdmittedTable::from_file_budgeted(
            directory.open_file(&format!("{name}.parquet")).unwrap(),
            budget,
        )
        .unwrap();
        let frame = table.read_metadata_budgeted(&[column], None).unwrap();
        assert_eq!(
            frame.column(column).unwrap().u64().unwrap().get(0),
            Some(expected)
        );
    }
    check_metrics_events(&directory, captured, budget);
    for name in [
        "original-census",
        "fragment-0",
        "fragment-1",
        "loss",
        "pending",
    ] {
        assert!(!object.join(name).exists());
    }
    let table = crate::engine::AdmittedTable::from_file_budgeted(
        directory.open_file("processes.parquet").unwrap(),
        budget,
    )
    .unwrap();
    assert_eq!(table.row_count(), collector::u64_from_usize(count * 40));
    if captured {
        let frame = table
            .read_metadata_budgeted(&["pid_version", "executable_name", "app_path"], Some(1))
            .unwrap();
        assert_eq!(
            frame.column("pid_version").unwrap().i32().unwrap().get(0),
            Some(-7)
        );
        assert_eq!(
            frame
                .column("executable_name")
                .unwrap()
                .binary()
                .unwrap()
                .get(0),
            Some(b"metric-\xff".as_slice())
        );
        assert_eq!(
            frame.column("app_path").unwrap().binary().unwrap().get(0),
            Some(b"/Apps/Metric-\xfe.app".as_slice())
        );
    }
}
fn check_metrics_events(
    directory: &PrivateDirectory,
    captured: bool,
    budget: &Arc<crate::resources::ByteBudget>,
) {
    let actual_events = crate::event_recovery::read::read_batch_file(
        directory
            .open_directory("events")
            .unwrap()
            .open_file("0000.parquet")
            .unwrap(),
        &crate::event_recovery::Batch {
            first: 1,
            end: 3,
            rows: 2,
        },
        captured,
        true,
        budget,
    )
    .unwrap();
    let expected_rows: Vec<_> = (1..=2)
        .map(|number| crate::event_recovery::batch::Row {
            publication: Some(2),
            event: crate::window::EventRecord {
                number,
                tick: 0,
                pid: 0,
                kind: crate::schema::event_kind::EXEC,
                coalition: None,
                usage: None,
                availability: crate::schema::availability::NOT_APPLICABLE,
                noticed: None,
            },
            identity: captured.then_some(crate::owned_identity::OwnedIdentity {
                pid: 0,
                creation: Some(collector::ring::ProcessCreation {
                    unique_id: 100,
                    pid_version: -7,
                }),
                availability: 0,
                usage: crate::schema::availability::NOT_APPLICABLE,
                program: None,
            }),
            name: captured.then(|| b"metric-\xff".to_vec()),
            app: captured.then(|| b"/Apps/Metric-\xfe.app".to_vec()),
        })
        .collect();
    assert!(
        actual_events.equals_missing(
            &crate::event_recovery::batch::frame(&expected_rows, captured).unwrap()
        )
    );
}

#[test]
fn ordinary_complete_numeric() {
    // Goal: nonempty numeric metrics survive loss of volatile ordinary metadata.
    // Method: reopen the real Store and compare preserved raw files plus independent fields.
    complete(false, 1);
}
#[test]
fn ordinary_complete_captured() {
    // Goal: native process identity/bytes and optional readings survive reconstruction.
    // Method: exercise the same complete ordinary Store path with captured source rows.
    complete(true, 1);
}
#[test]
fn ordinary_complete_61_tick_composition() {
    // Goal: all metric rows and optional groups survive a complete 61-tick reconstruction.
    // Method: recover in a fresh process, then compare every column and retained raw byte.
    if let Some(root) = std::env::var_os("FD_ORDINARY_61_ROOT") {
        complete_61_child(Path::new(&root));
        return;
    }
    // Goal: window-wide optional groups and bounded work compose across the full61tick bound.
    // Method: alternate optional source readings and reconstruct all complete raw tables.
    complete(true, 61);
}
fn complete(captured: bool, count: usize) {
    let mut fixture = metric_fixture(captured, collector::u64_from_usize(count));
    for index in 0..count {
        metric_publish(
            &mut fixture,
            collector::u64_from_usize(index),
            index == 0,
            index % 2 == 0,
        );
    }
    fixture.writer.finish();
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let object;
    let raw;
    {
        let store = Store::open(&fixture.root).unwrap();
        let attempt = store
            .register_attempt(&complete_input(&fixture), &mut fixture.reader(), &budget)
            .unwrap();
        object = fixture
            .root
            .join("data")
            .join(collector::recording::identity_text(attempt.transaction()).to_string());
        let manifest = complete_payload(&attempt, &fixture, count);
        assert_eq!(manifest.losses.events, 0);
        raw = raw_bytes(&object.join("pending"));
    }
    if count == 61 {
        complete_61_process(&fixture, &object);
        assert_eq!(raw_bytes(&object.join("ready")), raw);
        check_metrics(&object, captured, count, &budget);
        return;
    }
    let store = Store::open(&fixture.root).unwrap();
    let started = std::time::Instant::now();
    let mut owner = store
        .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
        .unwrap();
    finish_recovery(&mut owner);
    drop(owner);
    assert_eq!(raw_bytes(&object.join("ready")), raw);
    check_metrics(&object, captured, count, &budget);
    let frontier = store
        .source_frontier(
            &fixture.run,
            &budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        frontier.publication_end(),
        2 + collector::u64_from_usize(count)
    );
    assert_eq!(frontier.event_end(), 3);
    assert!(!frontier.complete());
    eprintln!(
        "ORDINARY-COMPLETE captured={captured} ticks={count} elapsed={:?} \
            reservation_peak={} footprint={:?} work={:?}",
        started.elapsed(),
        budget.used_peak(),
        crate::resources::Footprint::read().unwrap(),
        ledger.work()
    );
}
fn raw_bytes(path: &Path) -> Vec<(String, Vec<u8>)> {
    let mut bytes = Vec::with_capacity(11);
    for table in crate::schema::TABLES {
        if !matches!(table, crate::schema::Table::Events) {
            let name = format!("{}.parquet", table.name());
            bytes.push((name.clone(), fs::read(path.join(name)).unwrap()));
        }
    }
    for name in ["events/index", "events/0000.parquet"] {
        bytes.push((name.to_string(), fs::read(path.join(name)).unwrap()));
    }
    bytes
}

#[path = "ordinary_refusal_tests.rs"]
mod ordinary_refusal_tests;

#[path = "ordinary_budget_tests.rs"]
mod ordinary_budget_tests;

fn complete_61_process(fixture: &Fixture, object: &Path) {
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args([
            "--exact",
            concat!(
                "store::registration_tests::loss_proof_tests::ordinary_recovery_tests::",
                "ordinary_complete_tests::ordinary_complete_61_tick_composition"
            ),
            "--nocapture",
            "--test-threads=1",
        ])
        .env("FD_ORDINARY_61_ROOT", &fixture.root)
        .env("FD_ORDINARY_61_OBJECT", object)
        .env("FD_ORDINARY_61_SOURCE", fixture.writer.name());
    let mut child = OwnedChild(command.spawn().unwrap());
    assert!(child.wait().success());
}
fn complete_61_child(root: &Path) {
    let object = PathBuf::from(std::env::var_os("FD_ORDINARY_61_OBJECT").unwrap());
    let source = std::env::var("FD_ORDINARY_61_SOURCE").unwrap();
    let budget = registration_budget();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let store = Store::open(root).unwrap();
    let state = crate::transaction::normal::NormalState::decode(
        &fs::read(root.join("foreground")).unwrap(),
        store.recording(),
    )
    .unwrap();
    let started = std::time::Instant::now();
    let mut owner = store
        .resume_ordinary_proof(
            RingReader::open_generation(&source).unwrap(),
            &budget,
            &ledger,
        )
        .unwrap();
    finish_recovery(&mut owner);
    drop(owner);
    let elapsed = started.elapsed();
    let recovery_footprint = crate::resources::Footprint::read().unwrap();
    let recovery_reservation_peak = budget.used_peak();
    check_metrics(&object, true, 61, &budget);
    let frontier = store
        .source_frontier(
            &state.input.run,
            &budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.publication_end(), 63);
    assert_eq!(frontier.event_end(), 3);
    assert!(!frontier.complete());
    eprintln!(
        "ORDINARY-61-CHILD elapsed={:?} reservation_peak={} footprint={:?} work={:?}",
        elapsed,
        recovery_reservation_peak,
        recovery_footprint,
        ledger.work(),
    );
}
