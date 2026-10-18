use serde_json::json;

#[test]
fn selected_app_disk_history_preserves_directional_interval_rates_and_gaps() {
    // Project an unavailable point and a two-second interval through the real App chart adapter.
    let run = "00000000000000010000000000000002";
    let boot = "00000000000000030000000000000004";
    let selected = json!({"wall_ns":"1000000000000","stamp":{"run":run,"boot":boot,
        "publication":"2","continuous_ns":"3000000000","awake_ns":"700000000"}});
    let selection = json!({"second":1000,"selected":selected});
    let measure = |known: u64| {
        json!({"known":known.to_string(),"measured_members":1,
            "unavailable_members":0,"overflowed":false})
    };
    let unavailable = json!({"observation":{"wall_ns":"998000000000",
        "stamp":{"run":run,"boot":boot,"publication":"1",
            "continuous_ns":"1000000000","awake_ns":"200000000"}},
        "segment":"0","source_available":false,"elapsed_ns":null,
        "members":null,"membership_complete":false,
        "cpu_ns":null,"energy_nj":null,"resident_bytes":null,"footprint_bytes":null,
        "cpu_percent_one_core":null,"energy_joules":null,"energy_watts":null,
        "disk_read_bytes":null,"disk_written_bytes":null,"disk_io_bytes":null,
        "disk_read_bps":null,"disk_written_bps":null,"disk_io_bps":null});
    let measured = json!({"observation":selected,"segment":"0","source_available":true,
        "elapsed_ns":"2000000000","members":1,"membership_complete":true,
        "cpu_ns":measure(0),"energy_nj":measure(0),"resident_bytes":measure(1024),
        "footprint_bytes":measure(1024),"cpu_percent_one_core":0.0,
        "energy_joules":0.0,"energy_watts":0.0,
        "disk_read_bytes":measure(4096),"disk_written_bytes":measure(1024),
        "disk_io_bytes":measure(5120),"disk_read_bps":2048.0,
        "disk_written_bps":512.0,"disk_io_bps":2560.0});
    let history = json!({"selected":selected,"points":[unavailable,measured]});

    let panels = crate::live_projection::panels("apps", &selection, Some(&history)).unwrap();

    let disk = panels
        .iter()
        .find(|panel| panel.title == "Disk I/O")
        .expect("Selected-App Disk I/O history is missing");
    assert_eq!(disk.unit, "MB/s");
    assert_eq!(disk.selected_index, Some(1));
    assert_eq!(disk.lines.len(), 2);
    let read = disk.lines.iter().find(|line| line.name == "Read").unwrap();
    let write = disk.lines.iter().find(|line| line.name == "Write").unwrap();
    assert_eq!(read.values, [None, Some(0.002_048)]);
    assert_eq!(write.values, [None, Some(0.000_512)]);
    assert!(
        disk.help
            .as_ref()
            .unwrap()
            .contains("selected observation interval")
    );
}
