use collector::disks::{DiskReading, DiskSnapshot, ScanState};
use collector::ring::{DictionaryCapacity, IdentityPublication, SupplementalReadings};

#[test]
fn follows_selects_and_reopens_disk_values_with_observation_gaps() {
    // Goal: exact per-device rates and unavailable observations survive the full recorded path.
    // Method: publish through a registered extended ring, seal its ticks, and reopen both readers.
    let mut fixture = storage_fixture();
    storage_publish(&mut fixture, 1, Some(&storage_snapshot(1)));
    storage_publish(&mut fixture, 2, Some(&storage_snapshot(2)));
    storage_publish(&mut fixture, 3, None);
    storage_publish(&mut fixture, 4, Some(&storage_snapshot(4)));
    storage_publish(&mut fixture, 5, Some(&storage_snapshot(5)));
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let first = session.select(1001, 0).unwrap();
    let measured = session.select(1002, 0).unwrap();
    let missing = session.select(1003, 0).unwrap();
    let restarted = session.select(1004, 0).unwrap();
    let latest = session.select(1005, 0).unwrap();
    let mut saved = Session::open(&fixture.root).unwrap();
    let committed = saved.select(1005, 0).unwrap();
    let mut reopened = LiveSession::open(&fixture.root).unwrap();
    let restored = reopened.select(1005, 0).unwrap();

    assert_eq!(first["summary"]["storage"]["devices"][0]["driver_id"], 91);
    assert!(first["summary"]["storage"]["read_bps"].is_null());
    assert_eq!(measured["summary"]["storage"]["read_bps"], 4096.0);
    assert_eq!(measured["summary"]["storage"]["read_ops_per_second"], 2.0);
    assert_eq!(measured["summary"]["storage"]["read_mean_ms"], 3.0);
    assert!(missing["summary"]["storage"].is_null());
    assert!(restarted["summary"]["storage"]["read_bps"].is_null());
    assert_eq!(latest["summary"]["storage"]["read_bps"], 4096.0);
    assert_eq!(
        committed["summary"]["storage"],
        latest["summary"]["storage"]
    );
    assert_eq!(restored["summary"]["storage"], latest["summary"]["storage"]);
    assert_eq!(committed["trends"][2]["storage"], Value::Null);
    assert_eq!(committed["trends"][3]["storage"]["read_bps"], Value::Null);
}

fn storage_fixture() -> RingFixture {
    storage_fixture_with_slots(8)
}

fn storage_fixture_with_slots(slot_count: u64) -> RingFixture {
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "flightdeck-live-storage-{}-{serial}",
        std::process::id()
    ));
    drop(persistence::Store::open(&root).unwrap());
    let place = Place {
        lock_path: root.join("ring.lock"),
        name_prefix: format!("/fdsv{}x{serial}", std::process::id()),
    };
    let shape = Shape {
        slot_count,
        cpu_capacity: 1,
        interface_capacity: 1,
        core_capacity: 1,
        state_capacity: 1,
        fan_capacity: 1,
        process_capacity: 1,
        coalition_capacity: 1,
        event_capacity: 8,
    };
    let (writer, registered) = Registration::start_with_disks(
        &root,
        &place,
        shape,
        DictionaryCapacity {
            entries: 2,
            arena_bytes: 256,
        },
        0,
    )
    .unwrap()
    .into_parts();
    registered.unwrap();

    RingFixture {
        root,
        writer,
        following_fans: 2,
        owns_root: true,
        persister: std::sync::Mutex::default(),
    }
}

fn storage_snapshot(tick: u64) -> DiskSnapshot {
    let mut snapshot = DiskSnapshot {
        source_continuous_ns: tick * NANOSECONDS_PER_SECOND,
        state: ScanState::Complete,
        count: 1,
        ..DiskSnapshot::default()
    };
    snapshot.records[0] = DiskReading {
        driver_id: 91,
        media_id: 92,
        read_bytes: Some(tick * 4096),
        write_bytes: Some(tick * 8192),
        read_operations: Some(tick * 2),
        write_operations: Some(tick * 4),
        read_errors: Some(0),
        write_errors: Some(tick),
        read_time_ns: Some(tick * 6_000_000),
        write_time_ns: Some(tick * 20_000_000),
        ..DiskReading::default()
    };
    snapshot.records[0].name[..5].copy_from_slice(b"disk0");
    snapshot
}

fn storage_publish(fixture: &mut RingFixture, tick: u64, disks: Option<&DiskSnapshot>) {
    fixture
        .writer
        .publish_with_disks(
            &Readings {
                run_tick: tick,
                cpus: &[collector::machine::CPUTicks::default()],
                memory: &collector::machine::Memory::default(),
                interfaces: &[],
                ioreport: None,
                smc: None,
                processes: &[],
                processes_refused: 0,
                processes_gone: 0,
                coalitions: &[],
                events: &[],
                dropped: Dropped::default(),
            },
            Some(&ObservationTimes {
                wall_ns: (1000 + tick) * NANOSECONDS_PER_SECOND,
                continuous_ns: tick * NANOSECONDS_PER_SECOND,
                awake_ns: tick * NANOSECONDS_PER_SECOND,
            }),
            &IdentityPublication {
                processes: &[],
                events: &[],
            },
            &SupplementalReadings {
                battery: None,
                disks,
                system: None,
            },
        )
        .unwrap();
}

#[test]
fn reads_a_minute_without_disk_readings_beside_a_measured_one_without_fabricating_disks() {
    // Goal: a recording keeps measured disk history beside a minute that read no disks.
    // Method: save a measured minute, then a minute of one reading without disks, and read both.
    let mut fixture = storage_fixture();
    storage_publish(&mut fixture, 1, Some(&storage_snapshot(1)));
    storage_publish(&mut fixture, 2, Some(&storage_snapshot(2)));
    fixture.seal_held();
    storage_publish(&mut fixture, 61, None);
    fixture.seal_held();

    let mut saved = Session::open(&fixture.root).unwrap();
    let measured = saved.select(1002, 0).unwrap();
    let unmeasured = saved.select(1061, 0).unwrap();

    assert_eq!(saved.catalog()["windows"], 2);
    assert_eq!(measured["summary"]["storage"]["read_bps"], 4096.0);
    assert!(unmeasured["summary"]["storage"].is_null());
    assert_eq!(unmeasured["trends"][1]["storage"]["read_bps"], 4096.0);
    assert!(unmeasured["trends"][2]["storage"].is_null());
}

