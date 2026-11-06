//! Native adapter experiment; fixture construction stays outside the measured child.
use super::*;

#[path = "ordinary_process_order_tests.rs"]
mod ordinary_process_order_tests;

#[allow(dead_code)]
pub(super) mod encoder_fixture {
    use crate as persistence;
    include!("../../tests/encoder_fixture/mod.rs");
}

#[test]
fn ordinary_metric_adapter_experiment() {
    // Goal: expose whole-row-group work that cannot coexist with the source metric adapter.
    // Method: encode a real maximum-process captured tick outside the child, then decode slices.
    if let Some(root) = std::env::var_os("FD_ORDINARY_ADAPTER_ROOT") {
        adapter_child(&PathBuf::from(root));
        return;
    }
    let fixture = adapter_fixture();
    adapter_encode(&fixture);
    adapter_captured_limit(&fixture);
    let library = native_registration_build(&fixture);
    let descriptors = fixture.base.join("adapter-fds");
    fs::write(&descriptors, []).unwrap();
    let test_name = concat!(module_path!(), "::ordinary_metric_adapter_experiment")
        .strip_prefix("persistence::")
        .unwrap();
    let output_path = fixture.base.join("adapter-child.log");
    let output = File::create(&output_path).unwrap();
    let mut child = OwnedChild(
        std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test_name, "--nocapture", "--test-threads=1"])
            .env("DYLD_INSERT_LIBRARIES", library)
            .env("FLIGHTDECK_FD_ROOT", fixture.base.canonicalize().unwrap())
            .env("FLIGHTDECK_FD_LOG", &descriptors)
            .env("FD_ORDINARY_ADAPTER_ROOT", &fixture.base)
            .env("FD_ORDINARY_ADAPTER_SOURCE", fixture.writer.name())
            .stdout(std::process::Stdio::from(output.try_clone().unwrap()))
            .stderr(std::process::Stdio::from(output))
            .spawn()
            .unwrap(),
    );
    let status = child.wait();
    let output = fs::read_to_string(output_path).unwrap();
    eprintln!("{output}");
    assert!(output.contains("running 1 test"));
    adapter_descriptors(&descriptors);
    assert!(
        status.success(),
        "bounded adapter child refused production table"
    );
}

fn adapter_descriptors(path: &Path) {
    assert!(fs::metadata(path).unwrap().len() < 64 * 1024 * 1024);
    let bytes = fs::read(path).unwrap();
    assert!(!bytes.is_empty());
    assert_eq!(bytes.len() % 4, 0);
    let mut namespaces = 0;
    let mut raw = 0;
    for record in bytes.as_chunks::<4>().0 {
        namespaces = namespaces.max(u16::from_le_bytes([record[0], record[1]]));
        raw = raw.max(u16::from_le_bytes([record[2], record[3]]));
    }
    assert!(namespaces <= 8);
    assert!(raw > 0);
    eprintln!("ADAPTER-DESCRIPTORS namespace={namespaces} raw={raw}");
}

fn adapter_captured_limit(fixture: &Fixture) {
    let budget = registration_budget();
    let result = crate::engine::AdmittedTable::from_file_budgeted(
        File::open(fixture.base.join("raw/processes.parquet")).unwrap(),
        &budget,
    );
    assert!(matches!(
        result,
        Err(crate::engine::AdmissionRefusal::Bound(
            "parquet expanded bytes"
        ))
    ));
    assert_eq!(budget.used(), 0);
}

fn adapter_fixture() -> Fixture {
    let mut fixture = Fixture::new_profile(false, true);
    let place = collector::ring::Place {
        lock_path: fixture.base.join("adapter.lock"),
        name_prefix: format!("/fdadapter{:x}", std::process::id()),
    };
    let shape = Shape {
        slot_count: 1,
        cpu_capacity: 256,
        interface_capacity: 16_384,
        core_capacity: 256,
        state_capacity: 256,
        fan_capacity: 256,
        process_capacity: 65_536,
        coalition_capacity: 131_072,
        event_capacity: 4,
    };
    let writer = collector::ring::Writer::create_with_system(
        &place,
        shape,
        collector::ring::DictionaryCapacity {
            entries: 4,
            arena_bytes: 16 * 1024,
        },
        &fixture.run,
        1,
    )
    .unwrap();
    let previous = fixture.writer.name().to_owned();
    fixture.writer = writer;
    collector::ring::unlink_generation(&previous).unwrap();
    let writer = &mut fixture.writer;
    eprintln!(
        "ADAPTER fixture={} source={}",
        fixture.base.display(),
        writer.name()
    );
    adapter_fixture_publish(writer, &shape);
    fixture
}
fn adapter_fixture_publish(writer: &mut collector::ring::Writer, shape: &Shape) {
    let tick = encoder_fixture::tick(shape);
    let identities = adapter_fixture_publish_identities(writer, &tick);
    let cores: Vec<_> = (0..tick.core_count())
        .map(|index| {
            collector::ioreport::States::with_active_ns(
                &tick.core_active_ns(index)[..collector::ioreport::ACTIVE_STATES_MAX],
            )
        })
        .collect();
    adapter_fixture_publish_source(
        writer,
        &collector::ring::Readings {
            run_tick: 0,
            cpus: &tick.cpus,
            memory: &tick.memory,
            interfaces: &tick.interfaces,
            ioreport: Some(collector::ring::IOReportReadings {
                gpu_energy_nj: tick.gpu_energy_nj,
                gpu_active_ns: &tick.gpu_active_ns[..collector::ioreport::ACTIVE_STATES_MAX],
                cores: &cores,
            }),
            smc: Some(collector::ring::SmcReadings {
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
            events: &[],
            dropped: collector::ring::Dropped::default(),
        },
        &identities,
    );
    drop(tick);
    drop(identities);
}
fn adapter_fixture_publish_identities(
    writer: &mut collector::ring::Writer,
    tick: &collector::ring::Tick,
) -> Vec<collector::ring::IdentityObservation> {
    let name = vec![0xff; collector::ring::PROGRAM_PATH_BYTES_MAX];
    let mut app = vec![0xfe; 4095];
    app[0] = b'/';
    app[4091..].copy_from_slice(b".app");
    let mut program = None;
    writer
        .intern_program(
            &collector::ring::ProgramBytes {
                executable_name: &name,
                app_path: Some(&app),
            },
            &mut program,
        )
        .unwrap();
    tick.processes
        .iter()
        .map(|process| collector::ring::IdentityObservation {
            pid: process.pid,
            usage: collector::ring::UsageAvailability::Measured,
            identity: collector::ring::ProcessIdentity::Measured {
                creation: collector::ring::ProcessCreation {
                    unique_id: u64::try_from(process.pid).unwrap(),
                    pid_version: 3,
                },
                program: program.unwrap(),
            },
        })
        .collect()
}

fn adapter_fixture_publish_source(
    writer: &mut collector::ring::Writer,
    readings: &collector::ring::Readings<'_>,
    identities: &[collector::ring::IdentityObservation],
) {
    let battery = collector::battery::BatteryReading {
        source_continuous_ns: 1,
        charge_current: Some(41),
        charge_maximum: Some(79),
        full_charge_capacity: Some(81),
        design_capacity: Some(93),
        cycle_count: Some(7),
        charging: Some(true),
        external_power: Some(true),
        voltage_mv: Some(12_345),
        current_ma: Some(-913),
        adapter_watts: Some(95),
    };
    let disks = adapter_fixture_publish_disks();
    let reading = collector::system::SystemReading {
        load_average: Some([1.25, 2.5, 3.75]),
        open_files: Some(401),
        open_files_limit: Some(1001),
        thermal_pressure: Some(2),
        gpu_memory: Some(collector::gpu_memory::GpuMemoryReading {
            in_use_bytes: Some(702),
            allocated_bytes: Some(802),
        }),
        boot_wall_ns: Some(803),
    };
    let machine = adapter_fixture_publish_machine();
    let clock = collector::clock_states::ClockTables::from_tables(vec![
        (1..=64).map(|index| index * 1000).collect(),
    ]);
    let process_gpu = vec![Some(700); identities.len()];
    let system = collector::ring::SystemPublication {
        reading: &reading,
        machine: &machine,
        clock: Some(&clock),
        process_gpu_ns: &process_gpu,
    };
    writer
        .publish_with_disks(
            readings,
            Some(&collector::ring::ObservationTimes {
                wall_ns: 7,
                continuous_ns: 1,
                awake_ns: 3,
            }),
            &collector::ring::IdentityPublication {
                processes: identities,
                events: &[],
            },
            &collector::ring::SupplementalReadings {
                battery: Some(&battery),
                disks: Some(&disks),
                system: Some(&system),
            },
        )
        .unwrap();
}

fn adapter_fixture_publish_disks() -> collector::disks::DiskSnapshot {
    let mut disks = collector::disks::DiskSnapshot {
        source_continuous_ns: 1,
        state: collector::disks::ScanState::Complete,
        count: 16,
        ..collector::disks::DiskSnapshot::default()
    };
    for (index, record) in disks.records.iter_mut().enumerate() {
        let mut name = [b'z'; 32];
        name[0] = b'a' + u8::try_from(index).unwrap();
        name[31] = 0;
        *record = collector::disks::DiskReading {
            driver_id: collector::u64_from_usize(index) + 1,
            media_id: collector::u64_from_usize(index) + 20,
            name,
            failed: false,
            read_bytes: Some(101),
            write_bytes: Some(102),
            read_operations: Some(103),
            write_operations: Some(104),
            read_errors: Some(105),
            write_errors: Some(106),
            read_time_ns: Some(107),
            write_time_ns: Some(108),
        };
    }
    disks
}

fn adapter_fixture_publish_machine() -> collector::system::MachineFacts {
    use collector::system::{FactText, MachineFacts, PerformanceLevel};
    let text = FactText::new(&[b'x'; 64]).unwrap();
    MachineFacts {
        model: Some(text),
        chip: Some(text),
        architecture: Some(text),
        cpu_cores: Some(256),
        gpu_cores: Some(256),
        memory_bytes: Some(1 << 34),
        levels: Some([
            PerformanceLevel {
                name: text,
                logical_cpus: 128,
            },
            PerformanceLevel {
                name: text,
                logical_cpus: 128,
            },
        ]),
    }
}

fn adapter_encode(fixture: &Fixture) {
    let writer = &fixture.writer;
    let shape = fixture.reader().shape();
    let mut reader = RingReader::open_generation(writer.name()).unwrap();
    let mut tick = crate::tick_buffer(&shape).unwrap();
    let mut identities = Vec::with_capacity(65_536);
    assert!(matches!(
        reader
            .read_identity_into(&mut tick, &mut identities)
            .unwrap(),
        collector::ring::Found::Publication { publication: 1, .. }
    ));
    let mut window = Window::new(
        WindowIdentity {
            run: fixture.run,
            window: 0,
        },
        Limits::from_shape(&shape).unwrap(),
    );
    window
        .push_tick_with_identity(
            &original_observation(1, &tick),
            &tick,
            &[],
            &crate::IdentitySource {
                reader: &reader,
                processes: &identities,
                events: &[],
            },
        )
        .unwrap();
    fs::create_dir(fixture.base.join("raw")).unwrap();
    adapter_encode_processes(&window, &fixture.base.join("raw/processes.parquet"));
    drop(window);
    drop(tick);
    drop(identities);
    drop(reader);
}

fn adapter_encode_processes(window: &Window, path: &Path) {
    let mut first = crate::identity_encoder::processes_projection(window, 0, 16_384).unwrap();
    let mut writer = crate::encoder::TableWriter::new(&first, File::create(path).unwrap()).unwrap();
    writer.write(&mut first).unwrap();
    drop(first);
    for offset in (16_384..65_536).step_by(16_384) {
        let mut frame =
            crate::identity_encoder::processes_projection(window, offset, 16_384).unwrap();
        writer.write(&mut frame).unwrap();
    }
    writer.finish().unwrap();
}

fn adapter_child(root: &Path) {
    let started = std::time::Instant::now();
    let budget = Arc::new(crate::resources::ByteBudget::new(256 * 1024 * 1024));
    let source_name = std::env::var("FD_ORDINARY_ADAPTER_SOURCE").unwrap();
    let mut reader = RingReader::open_generation(&source_name).unwrap();
    let identity = WindowIdentity {
        run: reader.identity().unwrap(),
        window: 0,
    };
    let source = crate::encoder::metric_projection::MetricTick::read(
        &mut reader,
        &identity,
        1,
        &budget,
        None,
    )
    .unwrap();
    adapter_child_source_shape(&source);
    let expected = source
        .frame(crate::schema::Table::Processes, 0, 1024, &budget)
        .unwrap();
    for name in ["executable_name", "app_path"] {
        assert_eq!(
            expected
                .frame
                .column(name)
                .unwrap()
                .binary()
                .unwrap()
                .get(0)
                .unwrap()
                .len(),
            4095
        );
    }
    let table = crate::engine::AdmittedTable::from_file_streaming(
        File::open(root.join("raw/processes.parquet")).unwrap(),
        &budget,
    )
    .unwrap();
    let shape = table.slice_work_shape(0, 1024).unwrap();
    eprintln!(
        "ADAPTER rows={} compressed={} source={} slice-work={shape:?} budget={} footprint={:?}",
        table.row_count(),
        fs::metadata(root.join("raw/processes.parquet"))
            .unwrap()
            .len(),
        source.window.staged_bytes(),
        budget.used(),
        crate::resources::Footprint::read().unwrap()
    );
    adapter_child_columns(&table, &expected.frame, &budget);
    let footprint = crate::resources::Footprint::read().unwrap();
    eprintln!(
        "ADAPTER-FINAL elapsed={:?} reservation_peak={} footprint={footprint:?}",
        started.elapsed(),
        budget.used_peak()
    );
    assert!(footprint.lifetime_bytes_max <= 256 * 1024 * 1024);
}

fn adapter_child_source_shape(source: &crate::encoder::metric_projection::MetricTick) {
    let tick = &source.window.ticks[0];
    assert_eq!(tick.processes.len(), 65_536);
    assert!(tick.ioreport_read);
    assert_eq!(tick.cores.len(), 256);
    assert!(
        tick.cores
            .iter()
            .all(|states| states.len() == collector::ioreport::ACTIVE_STATES_MAX)
    );
    assert_eq!(tick.disks.as_ref().unwrap().count, 16);
    assert!(tick.battery.is_some());
    assert!(tick.system.is_some());
}

fn adapter_child_columns(
    table: &crate::engine::AdmittedTable,
    expected: &polars::prelude::DataFrame,
    budget: &Arc<crate::resources::ByteBudget>,
) {
    for (name, _) in table.fields() {
        let workspace = table
            .reserve_column_work(0, 1024, name)
            .expect("one column must coexist with retained source");
        let frame = table
            .read_slice_metadata_budgeted(&[name], 0, 1024)
            .unwrap();
        assert_eq!(frame.height(), 1024);
        assert!(
            frame
                .column(name)
                .unwrap()
                .equals_missing(expected.column(name).unwrap())
        );
        eprintln!(
            "ADAPTER column={name} workspace={} peak={} footprint={:?}",
            workspace.bytes(),
            budget.used_peak(),
            crate::resources::Footprint::read().unwrap()
        );
    }
}
