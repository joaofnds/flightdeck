use collector::battery::BatteryReading;
use collector::ring::IdentityPublication;

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
    battery_publish(&mut fixture, 2, Some(&battery));
    battery_publish(&mut fixture, 3, None);
    battery_publish(&mut fixture, 4, Some(&battery));
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let unavailable = session.select(1001, 0).unwrap();
    let first = session.select(1002, 0).unwrap();
    let missing = session.select(1003, 0).unwrap();
    let last = session.select(1004, 0).unwrap();
    let mut saved = Session::open(&fixture.root).unwrap();
    let sealed = saved.select(1004, 0).unwrap();
    let mut reopened = LiveSession::open(&fixture.root).unwrap();
    let restored = reopened.select(1004, 0).unwrap();

    assert!(unavailable["summary"]["battery"].is_null());
    assert_eq!(first["summary"]["battery"]["charge_percent"], 55.0);
    assert_eq!(first["summary"]["battery"]["cycle_count"], 11);
    assert_eq!(first["summary"]["battery"]["power_watts"], 0.0);
    assert_eq!(first["summary"]["battery"]["charging"], false);
    assert!(missing["summary"]["battery"].is_null());
    assert_eq!(sealed["summary"]["battery"], last["summary"]["battery"]);
    assert_eq!(restored["summary"]["battery"], last["summary"]["battery"]);
}

fn battery_fixture() -> RingFixture {
    RingFixture::started_with("battery", &one_of_each(8), Registration::start_with_battery)
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
