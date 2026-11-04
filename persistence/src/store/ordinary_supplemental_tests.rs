//! Supplemental snapshots survive absent ticks and independent full-column admission.
use super::*;
use polars::prelude::*;

#[test]
fn ordinary_supplemental_complete_source_recovers_all_groups() {
    // Goal: battery, disk and system groups retain values and absent-tick nulls through Store.
    // Method: publish supported supplemental/absent ticks and compare every canonical raw column.
    let mut fixture = supplemental_fixture();
    supplemental_publish(&mut fixture);
    metric_publish(&mut fixture, 1, false, false);
    fixture.writer.finish();
    eprintln!(
        "ORDINARY-SUPPLEMENTAL root={} source={}",
        fixture.base.display(),
        fixture.writer.name()
    );
    let budget = registration_budget();
    let object;
    {
        let store = Store::open(&fixture.root).unwrap();
        let attempt = store
            .register_attempt(&complete_input(&fixture), &mut fixture.reader(), &budget)
            .unwrap();
        object = fixture
            .root
            .join("data")
            .join(collector::recording::identity_text(attempt.transaction()).to_string());
        complete_payload(&attempt, &fixture, 2);
    }
    let raw = raw_bytes(&object.join("pending"));
    let store = Store::open(&fixture.root).unwrap();
    let ledger = PublicationLedger::new(&budget).unwrap();
    let mut owner = store
        .resume_ordinary_proof(fixture.reader(), &budget, &ledger)
        .unwrap();
    finish_recovery(&mut owner);
    drop(owner);
    assert_eq!(raw_bytes(&object.join("ready")), raw);
    supplemental_check(&object, &budget);
    let frontier = store
        .source_frontier(
            &fixture.run,
            &budget,
            &mut crate::canonical::ReadWork::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(frontier.publication_end(), 4);
    assert_eq!(frontier.event_end(), 3);
}

fn supplemental_fixture() -> Fixture {
    let mut fixture = Fixture::new_profile(false, true);
    let place = collector::ring::Place {
        lock_path: fixture.base.join("supplemental.lock"),
        name_prefix: format!(
            "/fdsupp{:x}{:x}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ),
    };
    let mut shape = encoder_fixture::profile(false);
    shape.slot_count = 2;
    shape.event_capacity = 4;
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
    fixture.place = place;
    fixture.binding = fixture.reader().object_binding().unwrap();
    fixture.source = crate::transaction::SourceTiming::from_reader(&fixture.reader());
    publish_at(&mut fixture.writer, 0);
    fixture
}

fn supplemental_publish(fixture: &mut Fixture) {
    let battery = collector::battery::BatteryReading {
        source_continuous_ns: 17,
        charge_current: Some(31),
        charge_maximum: Some(79),
        full_charge_capacity: None,
        design_capacity: Some(93),
        cycle_count: Some(7),
        charging: Some(false),
        external_power: Some(true),
        voltage_mv: Some(12_345),
        current_ma: Some(-913),
        adapter_watts: None,
    };
    let disks = supplemental_publish_disks();
    let reading = collector::system::SystemReading {
        load_average: Some([1.25, 2.5, 3.75]),
        open_files: Some(401),
        open_files_limit: None,
        thermal_pressure: Some(2),
        gpu_memory: Some(collector::gpu_memory::GpuMemoryReading {
            in_use_bytes: Some(702),
            allocated_bytes: None,
        }),
        boot_wall_ns: Some(803),
    };
    let machine = supplemental_publish_machine();
    let clock =
        collector::clock_states::ClockTables::from_tables(vec![vec![100_000, 200_000, 300_000]]);
    let process_gpu: Vec<_> = (0..40)
        .map(|index| (index % 2 == 0).then_some(index + 700u64))
        .collect();
    let system = collector::ring::SystemPublication {
        reading: &reading,
        machine: &machine,
        clock: Some(&clock),
        process_gpu_ns: &process_gpu,
    };
    metric_publish_with_supplemental(
        fixture,
        0,
        true,
        true,
        &collector::ring::SupplementalReadings {
            battery: Some(&battery),
            disks: Some(&disks),
            system: Some(&system),
        },
    );
}

fn supplemental_publish_disks() -> collector::disks::DiskSnapshot {
    let mut disks = collector::disks::DiskSnapshot {
        source_continuous_ns: 19,
        state: collector::disks::ScanState::Partial,
        count: 1,
        ..collector::disks::DiskSnapshot::default()
    };
    let mut name = [0; 32];
    name[..5].copy_from_slice(b"disk9");
    disks.records[0] = collector::disks::DiskReading {
        driver_id: 11,
        media_id: 12,
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
    disks
}

fn supplemental_publish_machine() -> collector::system::MachineFacts {
    use collector::system::{FactText, MachineFacts, PerformanceLevel};
    MachineFacts {
        model: FactText::new(b"ModelFixture"),
        chip: None,
        architecture: FactText::new(b"armFixture"),
        cpu_cores: Some(2),
        gpu_cores: Some(5),
        memory_bytes: Some(1 << 34),
        levels: Some([
            PerformanceLevel {
                name: FactText::new(b"Fast").unwrap(),
                logical_cpus: 1,
            },
            PerformanceLevel {
                name: FactText::new(b"Quiet").unwrap(),
                logical_cpus: 1,
            },
        ]),
    }
}

fn supplemental_check(object: &Path, budget: &Arc<crate::resources::ByteBudget>) {
    for name in [
        "ticks",
        "memory",
        "cpus",
        "interfaces",
        "gpu_states",
        "core_states",
        "fans",
        "processes",
        "coalitions",
    ] {
        let mut expected = ordinary_metric_oracle::expected(name, true, 2);
        match name {
            "ticks" => supplemental_check_ticks(&mut expected),
            "core_states" => {
                let values: Vec<_> = (0..6)
                    .flat_map(|index| [Some((index % 3 + 1) * 100_000u32), None])
                    .collect();
                expected
                    .with_column(Column::new("frequency_khz".into(), values))
                    .unwrap();
            }
            "processes" => {
                let values: Vec<_> = (0..40)
                    .flat_map(|pid| [(pid % 2 == 0).then_some(pid + 700u64), None])
                    .collect();
                expected
                    .with_column(Column::new("gpu_ns".into(), values))
                    .unwrap();
            }
            _ => {}
        }
        let table = crate::engine::AdmittedTable::from_file_budgeted(
            File::open(object.join("ready").join(format!("{name}.parquet"))).unwrap(),
            budget,
        )
        .unwrap();
        let names: Vec<_> = expected
            .columns()
            .iter()
            .map(|column| column.name().as_str())
            .collect();
        let actual = table.read_metadata_budgeted(&names, None).unwrap();
        assert_eq!(table.fields().count(), expected.width(), "{name} columns");
        assert_eq!(actual.height(), expected.height(), "{name} rows");
        for column in expected.columns() {
            let actual = actual.column(column.name().as_str()).unwrap();
            assert_eq!(
                actual.dtype(),
                column.dtype(),
                "{name}/{} type",
                column.name()
            );
            assert!(
                actual.equals_missing(column),
                "{name}/{} literal values",
                column.name()
            );
        }
    }
}

fn supplemental_check_ticks(expected: &mut DataFrame) {
    use std::fmt::Write;
    let battery = df!(
        "battery_version" => [1u8, 1],
        "battery_source_continuous_ns" => [Some(17u64), None],
        "battery_charge_current" => [Some(31u64), None],
        "battery_charge_maximum" => [Some(79u64), None],
        "battery_full_charge_capacity" => [None::<u64>, None],
        "battery_design_capacity" => [Some(93u64), None],
        "battery_cycle_count" => [Some(7u64), None],
        "battery_charging" => [Some(false), None],
        "battery_external_power" => [Some(true), None],
        "battery_voltage_mv" => [Some(12_345u64), None],
        "battery_current_ma" => [Some(-913i64), None],
        "battery_adapter_watts" => [None::<u64>, None],
    )
    .unwrap();
    expected.hstack_mut(battery.columns()).unwrap();
    let words = [
        0x4644_4449_534b_3031_u64,
        19,
        3,
        1,
        11,
        12,
        255,
        0x0039_6b73_6964,
        0,
        0,
        0,
        101,
        102,
        103,
        104,
        105,
        106,
        107,
        108,
    ];
    let mut literal = String::with_capacity(words.len() * 16);
    for byte in words.iter().flat_map(|word| word.to_le_bytes()) {
        write!(&mut literal, "{byte:02x}").unwrap();
    }
    expected
        .with_column(Column::new(
            "physical_disks".into(),
            [Some(literal.as_str()), None],
        ))
        .unwrap();
    let system = supplemental_check_system();
    expected.hstack_mut(system.columns()).unwrap();
}

fn supplemental_check_system() -> DataFrame {
    df!(
        "load_average_1" => [Some(1.25f64), None],
        "load_average_5" => [Some(2.5f64), None],
        "load_average_15" => [Some(3.75f64), None],
        "open_files" => [Some(401u64), None],
        "open_files_limit" => [None::<u64>, None],
        "thermal_pressure" => [Some(2u8), None],
        "gpu_memory_in_use_bytes" => [Some(702u64), None],
        "gpu_memory_allocated_bytes" => [None::<u64>, None],
        "boot_wall_ns" => [Some(803u64), None],
        "machine_model" => [Some("ModelFixture"), None],
        "machine_chip" => [None::<&str>, None],
        "machine_architecture" => [Some("armFixture"), None],
        "machine_cpu_cores" => [Some(2u32), None],
        "machine_gpu_cores" => [Some(5u32), None],
        "machine_memory_bytes" => [Some(1u64 << 34), None],
        "machine_performance_level0_name" => [Some("Fast"), None],
        "machine_performance_level0_logical_cpus" => [Some(1u32), None],
        "machine_performance_level1_name" => [Some("Quiet"), None],
        "machine_performance_level1_logical_cpus" => [Some(1u32), None],
    )
    .unwrap()
}
