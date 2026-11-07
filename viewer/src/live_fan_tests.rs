const FAN_SHAPE: Shape = Shape {
    slot_count: 2,
    cpu_capacity: 1,
    interface_capacity: 1,
    core_capacity: 1,
    state_capacity: 1,
    fan_capacity: 10,
    process_capacity: 1,
    coalition_capacity: 1,
    event_capacity: 8,
};

struct FanSample<'a> {
    tick: u64,
    second: u64,
    channels: Option<&'a [f32]>,
}

#[test]
fn follows_independent_fan_channels_and_preserves_stopped_zero() {
    // Goal: fan channels keep independent acquired gauges, including a stopped fan.
    // Method: publish changing literal pairs through a real two-slot ring and public session.
    let mut fixture = RingFixture::new_with_shape(&FAN_SHAPE);
    fan_publish(
        &mut fixture,
        &FanSample {
            tick: 1,
            second: 1000,
            channels: Some(&[1234.0, 0.0]),
        },
    );
    fan_publish(
        &mut fixture,
        &FanSample {
            tick: 2,
            second: 1001,
            channels: Some(&[0.0, 2345.0]),
        },
    );
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();

    let next = select_newest(&mut session);
    let selected = session.select(1000, 0).unwrap();

    assert_eq!(next["summary"]["fan_rpm"], json!([0.0, 2345.0]));
    assert_eq!(next["trends"][0]["fan_rpm"], json!([1234.0, 0.0]));
    assert_eq!(selected["summary"]["fan_rpm"], json!([1234.0, 0.0]));
}

fn fan_publish(fixture: &mut RingFixture, sample: &FanSample<'_>) {
    fixture.writer.publish_observed(
        &Readings {
            run_tick: sample.tick,
            cpus: &[collector::machine::CPUTicks::default()],
            memory: &collector::machine::Memory::default(),
            interfaces: &[],
            ioreport: None,
            smc: sample.channels.map(|fan_rpm| collector::ring::SmcReadings {
                power_watts: 5.0,
                soc_celsius: 40.0,
                cpu_celsius: Some(61.5),
                gpu_celsius: Some(52.25),
                fan_rpm,
            }),
            processes: &[],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: Dropped::default(),
        },
        &ObservationTimes {
            wall_ns: sample.second * NANOSECONDS_PER_SECOND,
            continuous_ns: sample.tick * NANOSECONDS_PER_SECOND,
            awake_ns: sample.tick * NANOSECONDS_PER_SECOND,
        },
    );
}

macro_rules! fan_availability {
    ($name:ident, $channels:expr, $expected:expr) => {
        #[test]
        fn $name() {
            // Goal: fan availability preserves the difference between zero, gap, and no fans.
            // Method: select and reopen a genuinely sealed publication beside its live copy.
            let mut fixture = RingFixture::new_with_shape(&FAN_SHAPE);
            fan_publish(
                &mut fixture,
                &FanSample {
                    tick: 1,
                    second: 1000,
                    channels: $channels,
                },
            );
            fixture.seal_held();
            let mut session = LiveSession::open(&fixture.root).unwrap();

            let response = crate::live::select_newest(&mut session);
            fixture.seal_held();
            let mut saved = Session::open(&fixture.root).unwrap();
            let selected = saved.select(1000, 0).unwrap();
            let mut reopened = LiveSession::open(&fixture.root).unwrap();
            let restored = crate::live::select_newest(&mut reopened);

            assert_eq!(response["summary"]["fan_rpm"], $expected);
            assert_eq!(response["trends"][0]["fan_rpm"], $expected);
            assert_eq!(selected["summary"]["fan_rpm"], $expected);
            assert_eq!(selected["trends"][0]["fan_rpm"], $expected);
            assert_eq!(restored["summary"]["fan_rpm"], $expected);
            assert_eq!(restored["trends"][0]["fan_rpm"], $expected);
        }
    };
}
fan_availability!(keeps_unread_smc_as_a_gap, None, Value::Null);
fan_availability!(
    keeps_nan_channel_as_a_gap,
    Some(&[f32::NAN, 0.0]),
    json!([null, 0.0])
);
fan_availability!(
    keeps_infinite_channel_as_a_gap,
    Some(&[f32::INFINITY, 0.0]),
    json!([null, 0.0])
);
fan_availability!(
    keeps_measured_fanless_as_an_empty_array,
    Some(&[]),
    json!([])
);
fan_availability!(
    keeps_negative_channel_as_a_gap,
    Some(&[-1.0, 0.0]),
    json!([null, 0.0])
);
fan_availability!(
    keeps_all_ten_actual_channels,
    Some(&[0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]),
    json!([0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0])
);

#[test]
fn keeps_different_run_alternatives_separate_without_copying_fan_readings() {
    // Goal: a saved point from another run never receives this live run's fan readings.
    // Method: save the next run's reading of the same second and retain both exact alternatives.
    let mut fixture = RingFixture::new_with_shape(&FAN_SHAPE);
    fan_publish(
        &mut fixture,
        &FanSample {
            tick: 1,
            second: 1000,
            channels: Some(&[1234.0]),
        },
    );
    fixture.seal_held();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let mut other_fixture = fixture.next_run();
    fan_publish(
        &mut other_fixture,
        &FanSample {
            tick: 1,
            second: 1000,
            channels: Some(&[2345.0, 0.0]),
        },
    );
    other_fixture.seal_held();
    let other = other_fixture.writer.identity().unwrap();
    session.refresh_history().unwrap();

    let selected = session.select(1000, 0).unwrap();

    let points = selected["trends"].as_array().unwrap();
    assert_eq!(points.len(), 2);
    assert_eq!(selected["alternatives"].as_array().unwrap().len(), 2);
    let saved = points
        .iter()
        .find(|point| point["run"] == identity_text(other.run).to_string())
        .unwrap();
    let live = points
        .iter()
        .find(|point| point["run"] != saved["run"])
        .unwrap();
    assert_eq!(saved["fan_rpm"], json!([2345.0, 0.0]));
    assert_eq!(live["fan_rpm"], json!([1234.0]));
}

#[test]
fn trims_fan_history_to_the_inclusive_last_fifteen_minutes() {
    // Goal: acquired fan gauges follow the same inclusive 900-second window as other metrics.
    // Method: acquire points on either side of its boundary, then select its first second.
    let mut fixture = RingFixture::new_with_shape(&FAN_SHAPE);
    fan_publish(
        &mut fixture,
        &FanSample {
            tick: 1,
            second: 1000,
            channels: Some(&[1.0]),
        },
    );
    let mut session = LiveSession::open(&fixture.root).unwrap();
    fan_publish(
        &mut fixture,
        &FanSample {
            tick: 2,
            second: 1001,
            channels: Some(&[2.0]),
        },
    );
    session.list_saved().unwrap();
    fan_publish(
        &mut fixture,
        &FanSample {
            tick: 3,
            second: 1900,
            channels: Some(&[3.0]),
        },
    );
    fixture.seal_held();
    session.list_saved().unwrap();

    let following = select_newest(&mut session);
    let selected = session.select(1001, 0).unwrap();

    assert_eq!(following["context_seconds"], 900);
    assert_eq!(following["trends"].as_array().unwrap().len(), 2);
    assert_eq!(following["trends"][0]["fan_rpm"], json!([2.0]));
    assert_eq!(following["trends"][1]["fan_rpm"], json!([3.0]));
    assert_ne!(
        following["trends"][0]["segment"],
        following["trends"][1]["segment"]
    );
    assert_eq!(selected["summary"]["fan_rpm"], json!([2.0]));
}

