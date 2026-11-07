use super::*;
use collector::gpu_memory::GpuMemoryReading;
use collector::ring::SystemSnapshot;
use collector::system::{FactText, MachineFacts, PerformanceLevel, SystemReading};

#[test]
fn typed_presentation_preserves_legacy_machine_text_and_integer_ranges() {
    // Goal: typed summary capture does not add the native fact decoder's admission rules.
    // Method: keep an empty label, a long saved label and a full-width count in old output.
    let mut fixture = system_fixture();
    let long = "x".repeat(collector::system::FACT_BYTES_MAX + 1);
    let tick = &mut fixture.minute(2).ticks[0];
    tick.saved_computer = Some(Box::new([Some(String::new()), Some(long.clone()), None]));
    tick.system.as_mut().unwrap().machine.cpu_cores = Some(u32::MAX);

    let point = fixture.point();

    assert_eq!(point["machine_model"], "");
    assert_eq!(point["machine_chip"], long);
    assert_eq!(point["machine_cpu_cores"], u32::MAX);
}

#[test]
fn projects_the_system_readings_and_machine_facts_a_tick_recorded() {
    // Goal: every system reading and machine fact reaches the point as the collector recorded it.
    // Method: give the current tick a whole group of literal readings and read the point.
    let fixture = system_fixture();

    let point = fixture.point();

    assert_eq!(point["load_average"], json!([0.5, 1.0, 2.25]));
    assert_eq!(point["open_files"], json!(9_000));
    assert_eq!(point["open_files_limit"], json!(36_000));
    assert_eq!(point["open_files_percent"], json!(25.0));
    assert_eq!(point["thermal_pressure"], json!(4));
    assert_eq!(point["gpu_memory_in_use_bytes"], json!(1_024));
    assert_eq!(point["gpu_memory_allocated_bytes"], json!(2_048));
    assert_eq!(point["uptime_seconds"], json!(61));
    assert_eq!(point["machine_model"], json!("Mac17,1"));
    assert_eq!(point["machine_chip"], json!("Apple M5 Pro"));
    assert_eq!(point["machine_architecture"], json!("arm64"));
    assert_eq!(point["machine_cpu_cores"], json!(2));
    assert_eq!(point["machine_gpu_cores"], json!(20));
    assert_eq!(point["machine_memory_bytes"], json!(51_539_607_552_u64));
    assert_eq!(
        point["performance_levels"],
        json!([{"name": "Super", "logical_cpus": 1},
            {"name": "Performance", "logical_cpus": 1}])
    );
}

#[test]
fn leaves_a_failed_system_reading_null_beside_the_others() {
    // Goal: a reading the collector did not get reads null and takes no other reading with it.
    // Method: leave one load average, the open file limit and the performance levels unread.
    let mut fixture = system_fixture();
    let tick = &mut fixture.minute(2).ticks[0];
    tick.saved_load_average = Some([Some(0.5), None, Some(2.25)]);
    tick.performance_levels = None;
    let system = tick.system.as_mut().unwrap();
    system.reading.load_average = None;
    system.reading.open_files_limit = None;
    system.machine.levels = None;

    let point = fixture.point();

    assert!(point["load_average"].is_null());
    assert!(point["open_files_limit"].is_null());
    assert!(point["open_files_percent"].is_null());
    assert!(point["performance_levels"].is_null());
    assert_eq!(point["open_files"], json!(9_000));
    assert_eq!(point["machine_chip"], json!("Apple M5 Pro"));
}

#[test]
fn leaves_every_system_reading_null_for_a_source_without_the_group() {
    // Goal: a recording sealed before the system readings invents none of them.
    // Method: read the point of the unextended literal tick source.
    let fixture = Fixture::new();

    let point = fixture.point();

    for name in [
        "load_average",
        "open_files",
        "open_files_limit",
        "open_files_percent",
        "thermal_pressure",
        "gpu_memory_in_use_bytes",
        "gpu_memory_allocated_bytes",
        "uptime_seconds",
        "machine_model",
        "machine_chip",
        "machine_architecture",
        "machine_cpu_cores",
        "machine_gpu_cores",
        "machine_memory_bytes",
        "performance_levels",
        "cpu_level_percent",
    ] {
        assert!(point[name].is_null(), "{name}");
    }
}

#[test]
fn leaves_the_uptime_unknown_for_a_boot_after_the_tick() {
    // Goal: a boot time past the tick's own clock yields no uptime rather than a wrapped one.
    // Method: record a boot one second after the current tick's wall clock.
    let mut fixture = system_fixture();
    let system = fixture.minute(2).ticks[0].system.as_mut().unwrap();
    system.reading.boot_wall_ns = Some(1_002_000_000_000);

    let point = fixture.point();

    assert!(point["uptime_seconds"].is_null());
}

#[test]
fn splits_cpu_busy_by_the_levels_the_tick_recorded() {
    // Goal: the per-level CPU busy share takes its levels from the recording, not this Mac.
    // Method: record one slower and one faster logical CPU with different busy intervals.
    let mut fixture = system_fixture();
    for (publication, slower_busy, faster_busy) in [(1, 10, 10), (2, 20, 60)] {
        fixture.minute(publication).cpus = vec![
            level_cpu_row(publication, 0, slower_busy),
            level_cpu_row(publication, 1, faster_busy),
        ];
    }

    let point = fixture.point();

    assert_eq!(point["cpu_level_percent"], json!([50.0, 10.0]));
}

#[test]
fn leaves_the_level_cpu_busy_unknown_where_the_tick_recorded_no_levels() {
    // Goal: a tick whose levels the collector could not read splits no CPU time by level.
    // Method: leave one level's logical CPU count unread.
    let mut fixture = system_fixture();
    let tick = &mut fixture.minute(2).ticks[0];
    tick.performance_levels = None;
    tick.system.as_mut().unwrap().machine.levels = None;

    let point = fixture.point();

    assert!(point["cpu_level_percent"].is_null());
}

/// A fixture whose current tick holds the whole system group.
fn system_fixture() -> Fixture {
    let levels = [system_level("Super"), system_level("Performance")];
    let mut fixture = Fixture::new();
    let tick = &mut fixture.minute(2).ticks[0];
    tick.thermal_pressure = Some(4);
    tick.performance_levels = Some(levels);
    tick.system = Some(SystemSnapshot {
        reading: SystemReading {
            load_average: Some([0.5, 1.0, 2.25]),
            open_files: Some(9_000),
            open_files_limit: Some(36_000),
            thermal_pressure: Some(4),
            gpu_memory: Some(GpuMemoryReading {
                in_use_bytes: Some(1_024),
                allocated_bytes: Some(2_048),
            }),
            boot_wall_ns: Some(940_000_000_000),
        },
        machine: MachineFacts {
            model: FactText::new(b"Mac17,1"),
            chip: FactText::new(b"Apple M5 Pro"),
            architecture: FactText::new(b"arm64"),
            cpu_cores: Some(2),
            gpu_cores: Some(20),
            memory_bytes: Some(51_539_607_552),
            levels: Some(levels),
        },
    });

    fixture
}

fn system_level(name: &str) -> PerformanceLevel {
    PerformanceLevel {
        name: FactText::new(name.as_bytes()).unwrap(),
        logical_cpus: 1,
    }
}

/// A CPU row of 100 ticks a publication so far, of which `busy` are user time.
fn level_cpu_row(publication: u64, cpu: u32, busy: u32) -> CpuRow {
    let idle = u32::try_from(publication).unwrap() * 100 - busy;

    cpu_row(publication, cpu, busy, idle)
}
