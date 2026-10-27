use super::*;

#[test]
fn typed_presentation_preserves_legacy_machine_text_and_integer_ranges() {
    // Goal: typed summary capture does not add the native fact decoder's admission rules.
    // Method: keep an empty label, a long saved label and a full-width count in old output.
    let mut fixture = system_fixture(&[]);
    let long = "x".repeat(collector::system::FACT_BYTES_MAX + 1);
    set(fixture.row("ticks", 2), 0, "machine_model", Some(""));
    set(fixture.row("ticks", 2), 0, "machine_chip", Some(&long));
    set(
        fixture.row("ticks", 2),
        0,
        "machine_cpu_cores",
        Some("18446744073709551615"),
    );

    let point = fixture.point();

    assert_eq!(point["machine_model"], "");
    assert_eq!(point["machine_chip"], long);
    assert_eq!(point["machine_cpu_cores"], u64::MAX);
}

#[test]
fn projects_the_system_readings_and_machine_facts_a_tick_recorded() {
    // Goal: every system reading and machine fact reaches the point as the collector recorded it.
    // Method: give the current tick a whole group of literal readings and read the point.
    let fixture = system_fixture(&[]);

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
    // Method: leave one load average, the open file limit and one level name unread.
    let fixture = system_fixture(&[
        "load_average_5",
        "open_files_limit",
        "machine_performance_level1_name",
    ]);

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
    let mut fixture = system_fixture(&[]);
    set(
        fixture.row("ticks", 2),
        0,
        "boot_wall_ns",
        Some("1002000000000"),
    );

    let point = fixture.point();

    assert!(point["uptime_seconds"].is_null());
}

#[test]
fn splits_cpu_busy_by_the_levels_the_tick_recorded() {
    // Goal: the per-level CPU busy share takes its levels from the recording, not this Mac.
    // Method: record one slower and one faster logical CPU with different busy intervals.
    let mut fixture = system_fixture(&[]);
    for (publication, slower_busy, faster_busy) in [(1, "10", "10"), (2, "20", "60")] {
        let cpus = fixture.row("cpus", publication);
        cpus.rows = vec![
            cpu_row(publication, 0, slower_busy),
            cpu_row(publication, 1, faster_busy),
        ];
    }

    let point = fixture.point();

    assert_eq!(point["cpu_level_percent"], json!([50.0, 10.0]));
}

#[test]
fn leaves_the_level_cpu_busy_unknown_where_the_tick_recorded_no_levels() {
    // Goal: a tick whose levels the collector could not read splits no CPU time by level.
    // Method: leave one level's logical CPU count unread.
    let fixture = system_fixture(&["machine_performance_level0_logical_cpus"]);

    let point = fixture.point();

    assert!(point["cpu_level_percent"].is_null());
}

/// A fixture whose current tick holds the whole system group, with `unread` read as failed.
fn system_fixture(unread: &[&str]) -> Fixture {
    let mut fixture = Fixture::new();
    let tick = fixture.row("ticks", 2);

    for (name, value) in [
        ("load_average_1", "0.5"),
        ("load_average_5", "1"),
        ("load_average_15", "2.25"),
        ("open_files", "9000"),
        ("open_files_limit", "36000"),
        ("thermal_pressure", "4"),
        ("gpu_memory_in_use_bytes", "1024"),
        ("gpu_memory_allocated_bytes", "2048"),
        ("boot_wall_ns", "940000000000"),
        ("machine_model", "Mac17,1"),
        ("machine_chip", "Apple M5 Pro"),
        ("machine_architecture", "arm64"),
        ("machine_cpu_cores", "2"),
        ("machine_gpu_cores", "20"),
        ("machine_memory_bytes", "51539607552"),
        ("machine_performance_level0_name", "Super"),
        ("machine_performance_level0_logical_cpus", "1"),
        ("machine_performance_level1_name", "Performance"),
        ("machine_performance_level1_logical_cpus", "1"),
    ] {
        tick.columns.push(name.to_owned());
        tick.rows[0].push((!unread.contains(&name)).then(|| value.to_owned()));
    }

    fixture
}

/// A CPU row of 100 ticks a publication so far, of which `busy` are user time.
fn cpu_row(publication: u64, cpu: u64, busy: &str) -> Vec<Option<String>> {
    let idle = publication * 100 - busy.parse::<u64>().unwrap();
    [
        publication.to_string(),
        cpu.to_string(),
        busy.to_owned(),
        "0".to_owned(),
        "0".to_owned(),
        idle.to_string(),
    ]
    .map(Some)
    .to_vec()
}
