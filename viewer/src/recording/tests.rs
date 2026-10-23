use std::sync::atomic::{AtomicU64, Ordering};

use collector::battery::BatteryReading;
use collector::clock_states::ClockTables;
use collector::disks::{DiskReading, DiskSnapshot, ScanState};
use collector::interfaces::{IFNAMSIZ, Interface, Name};
use collector::ioreport::States;
use collector::machine::{CPUTicks, Memory, Pressure};
use collector::recording::{RegisteredRun, Registry};
use collector::ring::{
    DictionaryCapacity, Dropped, Found, IOReportReadings, IdentityPublication, ObservationTimes,
    Place, Readings, RingReader, SmcReadings, SupplementalReadings, SystemPublication, Tick,
    Writer,
};
use collector::system::{FactText, MachineFacts, PerformanceLevel, SystemReading};
use persistence::schema::Table;
use polars::polars_utils::compression::ZstdLevel;
use polars::prelude::{
    DataFrame, DataType, Expr, IntoLazy, Null, ParquetCompression, ParquetWriter,
    StatisticsOptions, lit,
};

use super::{MachineMinute, Refused, SealedMinute, read_machine};
use crate::live::identity_fixture::Fixture;

/// Tells apart the rings the tests of this file make.
static NEXT: AtomicU64 = AtomicU64::new(0);

#[test]
fn a_sealed_minute_holds_the_machine_readings_the_ring_held() {
    // Goal: the recording and the ring hand the store the same machine readings, so one rate
    // rule serves both.
    // Method: publish three ticks with distinct readings, the second without IOReport and the
    // third without the SMC, take the ring's ticks as the store would, seal them as one
    // minute, and decode it.
    let mut fixture = Fixture::new_mode(false, 2, None);
    for step in 1..=3 {
        publish_machine(&mut fixture, step, None);
    }
    let mut expected = MachineMinute::default();
    for (publication, tick) in ring_ticks(&fixture) {
        expected.push_tick(publication, &tick);
    }
    fixture.seal_minute(0);
    let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();
    let mut decoded = MachineMinute::default();

    read_machine(&minute, &mut decoded).unwrap();

    let states = (expected.gpu_states.len(), expected.core_states.len());
    assert_eq!(
        (expected.ticks.len(), states, expected.fans.len()),
        (3, (2, 2), 2)
    );
    assert_eq!(decoded, expected);
}

#[test]
fn a_sealed_minute_holds_the_system_battery_and_disk_readings_the_ring_held() {
    // Goal: the recording and the ring hand the store the same thermal pressure, performance
    // levels, battery and disks, and the clock of each core state, so the store reads them
    // one way from either.
    // Method: publish three ticks into a ring that carries these readings, the first with all
    // of them, the second with disks alone and the third with no disks and no levels, take
    // the ring's ticks as the store would, seal them as one minute, and decode it.
    let mut fixture = Fixture::new_mode(false, 2, None);
    system_ring(&mut fixture);
    for step in 1..=3 {
        publish_supplemented(&mut fixture, step);
    }
    let mut expected = MachineMinute::default();
    for (publication, tick) in ring_ticks(&fixture) {
        expected.push_tick(publication, &tick);
    }
    fixture.seal_minute(0);
    let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();
    let mut decoded = MachineMinute::default();

    read_machine(&minute, &mut decoded).unwrap();

    let held: Vec<_> = expected
        .ticks
        .iter()
        .map(|tick| {
            let levels = tick.performance_levels.is_some();
            let source = (tick.battery.is_some(), tick.disks.is_some());
            (tick.thermal_pressure, levels, source)
        })
        .collect();
    let all = (Some(1), true, (true, true));
    let disks = (None, false, (false, true));
    let no_disks = (Some(3), false, (true, false));
    assert_eq!(held, [all, disks, no_disks]);
    let clocked = expected
        .core_states
        .iter()
        .filter(|state| state.frequency_khz.is_some())
        .count();
    assert_eq!(clocked, 2);
    assert_eq!(decoded, expected);
}

macro_rules! refuses_a_damaged_table {
    ($($name:ident, $table:expr, $reason:literal, $damage:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: a minute whose table holds what format 1 never writes is refused with the
            // reason, and never read as readings.
            // Method: seal a minute of system, battery and disk readings, replace one of its
            // tables with a copy the case damages, and decode it.
            let fixture = sealed_system_minute();
            damage_table(&fixture, $table, $damage);
            let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();

            let refused = read_machine(&minute, &mut MachineMinute::default());

            let refusal = matches!(
                refused,
                Err(Refused::Decode(persistence::Error::Bound($reason)))
            );
            assert!(refusal, "{refused:?}");
        }
    )*};
}

refuses_a_damaged_table! {
    refuses_a_column_group_missing_a_column, Table::Ticks, "incomplete column group",
        |frame| frame.drop("battery_adapter_watts").unwrap();
    refuses_another_battery_version, Table::Ticks, "battery version",
        |frame| with_column(frame, lit(2).cast(DataType::UInt8).alias("battery_version"));
    refuses_a_battery_reading_without_its_source_time, Table::Ticks,
        "battery payload without source time",
        |frame| with_column(frame, null("battery_source_continuous_ns", DataType::UInt64));
    refuses_part_of_the_performance_levels, Table::Ticks, "incomplete performance levels",
        |frame| {
            let logical_cpus = "machine_performance_level1_logical_cpus";
            with_column(frame, null(logical_cpus, DataType::UInt32))
        };
    refuses_an_empty_performance_level_name, Table::Ticks, "performance level name",
        |frame| with_column(frame, lit("").alias("machine_performance_level0_name"));
    refuses_disks_that_do_not_decode, Table::Ticks, "physical disk text extent",
        |frame| with_column(frame, lit("0").alias("physical_disks"));
    refuses_an_unknown_memory_pressure, Table::Memory, "memory pressure code",
        |frame| with_column(frame, lit(9).cast(DataType::UInt8).alias("pressure"));
    refuses_an_interface_name_longer_than_the_kernel_allows, Table::Interfaces, "interface name",
        |frame| with_column(frame, lit("utun_longer_than_ifnamsiz").alias("name"));
}

/// A fixture whose ring held the ticks `publish_supplemented` publishes, sealed as minute 0.
fn sealed_system_minute() -> Fixture {
    let mut fixture = Fixture::new_mode(false, 2, None);
    system_ring(&mut fixture);
    for step in 1..=3 {
        publish_supplemented(&mut fixture, step);
    }
    fixture.seal_minute(0);
    fixture
}

/// Replaces table `table` of `fixture`'s sealed minute 0 with the copy `damage` makes of it.
fn damage_table(fixture: &Fixture, table: Table, damage: fn(DataFrame) -> DataFrame) {
    let manifest = persistence::read_committed(&fixture.root, &marker(fixture, 0)).unwrap();
    let path = fixture
        .root
        .join(manifest.data_directory)
        .join(format!("{}.parquet", table.name()));
    let frame = persistence::engine::scan(&path).unwrap().collect().unwrap();
    let mut damaged = damage(frame);

    std::fs::remove_file(&path).unwrap();
    let file = collector::recording::private::open_private_file(&path, true).unwrap();
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
        .finish(&mut damaged)
        .unwrap();
}

/// `frame` with the column `column` names replaced by `column`.
fn with_column(frame: DataFrame, column: Expr) -> DataFrame {
    frame.lazy().with_column(column).collect().unwrap()
}

/// A column named `name` of type `data_type` holding no values.
fn null(name: &str, data_type: DataType) -> Expr {
    lit(Null {}).cast(data_type).alias(name)
}

/// Moves `fixture` onto a ring of its shape and run that also carries system readings,
/// battery readings and disks.
fn system_ring(fixture: &mut Fixture) {
    let identity = fixture.writer.identity().unwrap();
    let shape = RingReader::open_generation(fixture.writer.name())
        .unwrap()
        .shape();
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let place = Place {
        lock_path: fixture.root.join("system.lock"),
        name_prefix: format!("/fdrs{}x{serial}", std::process::id()),
    };
    let capacity = DictionaryCapacity {
        entries: 4,
        arena_bytes: 32_768,
    };
    let writer = Writer::create_with_system(&place, shape, capacity, &identity, 0).unwrap();

    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.writer = writer;
    Registry {
        runs: vec![RegisteredRun {
            identity,
            ring_name: fixture.writer.name().to_owned(),
            collector_pid: 0,
        }],
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
}

/// Publishes the `step`th tick as `publish_machine` does, with system readings and a battery
/// reading at steps 1 and 3, performance levels at step 1 alone, and disks at steps 1 and 2.
fn publish_supplemented(fixture: &mut Fixture, step: u64) {
    let reading = SystemReading {
        thermal_pressure: Some(u8::try_from(step).unwrap()),
        ..SystemReading::default()
    };
    let machine = MachineFacts {
        levels: (step == 1).then(levels),
        ..MachineFacts::default()
    };
    let khz = 2_000_000 + u32::try_from(step).unwrap();
    let clock = ClockTables::from_tables(vec![vec![khz]]);
    let system = SystemPublication {
        reading: &reading,
        machine: &machine,
        clock: Some(&clock),
        process_gpu_ns: &[],
    };
    let battery = battery(step);
    let disks = disks(step);
    let supplemental = SupplementalReadings {
        battery: (step != 2).then_some(&battery),
        disks: (step != 3).then_some(&disks),
        system: (step != 2).then_some(&system),
    };
    publish_machine(fixture, step, Some(&supplemental));
}

/// The two performance levels of an M5 Pro.
fn levels() -> [PerformanceLevel; 2] {
    let level = |name: &str, logical_cpus| PerformanceLevel {
        name: FactText::new(name.as_bytes()).unwrap(),
        logical_cpus,
    };
    [level("Super", 6), level("Performance", 12)]
}

/// The battery reading of the `step`th tick, taken just before it, each value distinct from
/// every other tick's.
fn battery(step: u64) -> BatteryReading {
    BatteryReading {
        source_continuous_ns: step * 1_000_000_000 - 500,
        charge_current: Some(100 * step),
        charge_maximum: Some(100 * step + 1),
        full_charge_capacity: Some(100 * step + 2),
        design_capacity: Some(100 * step + 3),
        cycle_count: Some(100 * step + 4),
        charging: Some(step == 1),
        external_power: Some(step != 1),
        voltage_mv: Some(100 * step + 5),
        current_ma: Some(-i64::try_from(step).unwrap()),
        adapter_watts: None,
    }
}

/// The disks of the `step`th tick, scanned just before it, each counter distinct from every
/// other tick's.
fn disks(step: u64) -> DiskSnapshot {
    let mut snapshot = DiskSnapshot {
        source_continuous_ns: step * 1_000_000_000 - 500,
        state: ScanState::Complete,
        count: 1,
        ..DiskSnapshot::default()
    };
    snapshot.records[0] = DiskReading {
        driver_id: 5,
        media_id: 7,
        read_bytes: Some(1_000 * step),
        write_bytes: Some(1_000 * step + 1),
        ..DiskReading::default()
    };
    snapshot.records[0].name[..5].copy_from_slice(b"disk0");
    snapshot
}

/// Publishes the `step`th tick of run minute 0, holding `machine(step)`, with no `IOReport`
/// readings at step 2 and no SMC readings at step 3, and with `supplemental` where the ring
/// carries it.
fn publish_machine(fixture: &mut Fixture, step: u64, supplemental: Option<&SupplementalReadings>) {
    let (cpus, memory, interfaces) = machine(step);
    let sources = sources(step);
    let readings = Readings {
        run_tick: step - 1,
        cpus: &cpus,
        memory: &memory,
        interfaces: &interfaces,
        ioreport: (step != 2).then_some(IOReportReadings {
            gpu_energy_nj: sources.gpu_energy_nj,
            gpu_active_ns: &sources.gpu_active_ns,
            cores: &sources.cores,
        }),
        smc: (step != 3).then_some(SmcReadings {
            power_watts: sources.power_watts,
            soc_celsius: sources.soc_celsius,
            cpu_celsius: Some(sources.cpu_celsius),
            gpu_celsius: None,
            fan_rpm: &sources.fan_rpm,
        }),
        processes: &[],
        processes_refused: 0,
        processes_gone: 0,
        coalitions: &[],
        events: &[],
        dropped: Dropped::default(),
    };
    let times = ObservationTimes {
        wall_ns: (1_000 + step) * 1_000_000_000,
        continuous_ns: step * 1_000_000_000,
        awake_ns: step * 900_000_000,
    };
    let Some(supplemental) = supplemental else {
        fixture.writer.publish_observed(&readings, &times);
        return;
    };

    let identities = IdentityPublication {
        processes: &[],
        events: &[],
    };
    fixture
        .writer
        .publish_with_disks(&readings, Some(&times), &identities, supplemental)
        .unwrap();
}

/// Every tick the ring holds, oldest first, each with its publication.
fn ring_ticks(fixture: &Fixture) -> Vec<(u64, Tick)> {
    let mut reader = RingReader::open_generation(fixture.writer.name()).unwrap();
    let shape = reader.shape();
    let mut ticks = Vec::with_capacity(8);
    for _index in 0..8 {
        let mut tick = persistence::tick_buffer(&shape).unwrap();
        match reader.read_into(&mut tick).unwrap() {
            Found::Nothing => break,
            Found::Missed { .. } => panic!("fixture source overwritten"),
            Found::Publication { publication, .. } => ticks.push((publication, tick)),
        }
    }
    ticks
}

/// The marker of the sealed window of run minute `minute`.
fn marker(fixture: &Fixture, minute: u64) -> std::path::PathBuf {
    let store = persistence::Store::open(&fixture.root).unwrap();
    store.marker_path(&persistence::WindowIdentity {
        run: fixture.writer.identity().unwrap(),
        window: minute,
    })
}

/// The machine readings of the `step`th tick, each counter distinct from every other tick's.
fn machine(step: u64) -> ([CPUTicks; 1], Memory, [Interface; 1]) {
    let small = u32::try_from(step).unwrap();
    let cpus = [CPUTicks {
        user: 10 * small,
        system: 10 * small + 1,
        nice: 10 * small + 2,
        idle: 10 * small + 3,
    }];
    let memory = Memory {
        total_bytes: 1_000 * step,
        free_bytes: 1_000 * step + 1,
        active_bytes: 1_000 * step + 2,
        inactive_bytes: 1_000 * step + 3,
        speculative_bytes: 1_000 * step + 4,
        wired_bytes: 1_000 * step + 5,
        compressed_bytes: 1_000 * step + 6,
        anonymous_bytes: 1_000 * step + 7,
        file_backed_bytes: 1_000 * step + 8,
        purgeable_bytes: 1_000 * step + 9,
        swap_total_bytes: 1_000 * step + 10,
        swap_used_bytes: 1_000 * step + 11,
        swapped_in_bytes: 1_000 * step + 12,
        swapped_out_bytes: 1_000 * step + 13,
        pageins: 1_000 * step + 14,
        pageouts: 1_000 * step + 15,
        compressions: 1_000 * step + 16,
        decompressions: 1_000 * step + 17,
        pressure: [Pressure::Normal, Pressure::Warning, Pressure::Critical]
            [usize::try_from(step % 3).unwrap()],
    };
    let mut name = [0; IFNAMSIZ];
    name[..4].copy_from_slice(b"utun");
    let interfaces = [Interface {
        index: 4,
        name: Name::from_bytes(name, 4).unwrap(),
        received_bytes: 100 * step,
        sent_bytes: 100 * step + 1,
        received_packets: 100 * step + 2,
        sent_packets: 100 * step + 3,
        receive_errors: 100 * step + 4,
        send_errors: 100 * step + 5,
        receive_drops: 100 * step + 6,
    }];
    (cpus, memory, interfaces)
}

/// The `IOReport` and SMC readings of the `step`th tick, each distinct from every other tick's.
struct Sources {
    gpu_energy_nj: u64,
    gpu_active_ns: [u64; 1],
    cores: [States; 1],
    power_watts: f32,
    soc_celsius: f32,
    cpu_celsius: f32,
    fan_rpm: [f32; 1],
}

fn sources(step: u64) -> Sources {
    let small = f32::from(u8::try_from(step).unwrap());
    Sources {
        gpu_energy_nj: 10_000 * step,
        gpu_active_ns: [10_000 * step + 1],
        cores: [States::with_active_ns(&[10_000 * step + 2])],
        power_watts: 10.0 + small,
        soc_celsius: 40.0 + small,
        cpu_celsius: 50.0 + small,
        fan_rpm: [1_000.0 + small],
    }
}
