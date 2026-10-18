use collector::battery::BatteryReading;
use collector::ring::{DictionaryCapacity, IdentityPublication};

#[test]
fn follows_and_reopens_battery_gauges_with_unavailable_publications() {
    // Goal: optional battery fields survive live projection, missing readings and disk restore.
    // Method: publish through a genuine extended ring, seal it and compare public responses.
    let mut fixture = battery_fixture();
    let battery = BatteryReading {
        source_continuous_ns: NANOSECONDS_PER_SECOND,
        charge_current: Some(55),
        charge_maximum: Some(100),
        full_charge_capacity: Some(8701),
        design_capacity: Some(8579),
        cycle_count: Some(11),
        charging: Some(false),
        external_power: Some(true),
        voltage_mv: Some(12000),
        current_ma: Some(0),
        adapter_watts: Some(94),
    };
    battery_publish(&mut fixture, 1, None);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    assert!(session.follow_latest().unwrap()["summary"]["battery"].is_null());
    battery_publish(&mut fixture, 2, Some(&battery));
    assert!(session.poll().unwrap());
    let first = session.follow_latest().unwrap();
    battery_publish(&mut fixture, 3, None);
    assert!(session.poll().unwrap());
    let missing = session.follow_latest().unwrap();
    battery_publish(&mut fixture, 4, Some(&battery));
    assert!(session.poll().unwrap());
    let last = session.follow_latest().unwrap();
    fan_history_tests::fan_seal(&fixture);
    let mut saved = Session::open(&fixture.root).unwrap();
    let sealed = saved.select(1004, 0).unwrap();
    let mut reopened = LiveSession::open(&fixture.root).unwrap();
    let restored = reopened.select(1004, 0).unwrap();

    assert_eq!(first["summary"]["battery"]["charge_percent"], 55.0);
    assert_eq!(first["summary"]["battery"]["cycle_count"], 11);
    assert_eq!(first["summary"]["battery"]["power_watts"], 0.0);
    assert_eq!(first["summary"]["battery"]["charging"], false);
    assert!(missing["summary"]["battery"].is_null());
    assert_eq!(sealed["summary"]["battery"], last["summary"]["battery"]);
    assert_eq!(restored["summary"]["battery"], last["summary"]["battery"]);
}

fn battery_fixture() -> RingFixture {
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir()
        .join(format!("flightdeck-live-battery-{}-{serial}", std::process::id()));
    private::create_private_dir(&root).unwrap();
    let place = Place {
        lock_path: root.join("ring.lock"),
        name_prefix: format!("/fdbv{}x{serial}", std::process::id()),
    };
    let shape = Shape {
        slot_count: 2,
        cpu_capacity: 1,
        interface_capacity: 1,
        core_capacity: 1,
        state_capacity: 1,
        fan_capacity: 1,
        process_capacity: 1,
        coalition_capacity: 1,
        event_capacity: 8,
    };
    let (writer, registered) = Registration::start_with_battery(
        &root,
        &place,
        shape,
        DictionaryCapacity { entries: 2, arena_bytes: 256 },
        0,
    ).unwrap().into_parts();
    registered.unwrap();

    RingFixture { root, writer, following_fans: 2 }
}

fn battery_publish(fixture: &mut RingFixture, tick: u64, battery: Option<&BatteryReading>) {
    fixture.writer.publish_with_battery(
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
        &IdentityPublication { processes: &[], events: &[] },
        battery,
    ).unwrap();
}
