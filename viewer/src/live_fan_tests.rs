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
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let first = session.follow_latest().unwrap();
    fan_publish(
        &mut fixture,
        &FanSample {
            tick: 2,
            second: 1001,
            channels: Some(&[0.0, 2345.0]),
        },
    );
    assert!(session.poll().unwrap());

    let next = session.follow_latest().unwrap();
    let selected = session.select(1000, 0).unwrap();

    assert_eq!(first["summary"]["fan_rpm"], json!([1234.0, 0.0]));
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

#[test]
fn keeps_acquired_fan_history_when_exact_committed_points_overlap() {
    // Goal: sealing retains measured fan arrays in following, selection, and its summary.
    // Method: acquire two real fan publications, commit them, then select the exact second.
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
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let before = session.follow_latest().unwrap();
    fan_seal(&fixture);
    session.poll().unwrap();
    assert_eq!(session.catalog()["windows"], 1);
    let mut saved = Session::open(&fixture.root).unwrap();
    let committed = saved.select(1001, 0).unwrap();
    assert_eq!(committed["trends"].as_array().unwrap().len(), 2);
    assert_eq!(committed["summary"]["publication"], 2);
    assert_eq!(committed["summary"]["fan_rpm"], json!([0.0, 2345.0]));
    assert_eq!(committed["trends"][0]["fan_rpm"], json!([1234.0, 0.0]));

    let selected = session.select(1001, 0).unwrap();
    let following = session.follow_latest().unwrap();

    assert_eq!(selected["summary"]["fan_rpm"], json!([0.0, 2345.0]));
    assert_eq!(selected["trends"][0]["fan_rpm"], json!([1234.0, 0.0]));
    assert_eq!(selected["trends"], before["trends"]);
    assert_eq!(following["trends"], before["trends"]);
    assert_eq!(following["summary"]["fan_rpm"], json!([0.0, 2345.0]));
    assert_eq!(selected["alternatives"].as_array().unwrap().len(), 1);
}

struct FanSeal {
    run: RunIdentity,
    wall_offset_ns: u64,
}

pub(super) fn fan_seal(fixture: &RingFixture) {
    fan_seal_with(
        fixture,
        &FanSeal {
            run: fixture.writer.identity().unwrap(),
            wall_offset_ns: 0,
        },
    );
}

fn fan_seal_with(fixture: &RingFixture, seal: &FanSeal) {
    fan_seal_into(fixture, seal, &fixture.root);
}

fn fan_seal_into(fixture: &RingFixture, seal: &FanSeal, root: &Path) {
    let mut reader = RingReader::open_generation(fixture.writer.name()).unwrap();
    let mut tick = persistence::tick_buffer(&reader.shape()).unwrap();
    let mut window = Window::new(
        WindowIdentity {
            run: seal.run,
            window: 0,
        },
        Limits::from_shape(&reader.shape()).unwrap(),
    );
    for _attempt in 0..=2 {
        match reader.read_into(&mut tick).unwrap() {
            Found::Nothing => break,
            Found::Missed { .. } => panic!("fixture publications overwritten"),
            Found::Publication { publication, .. } => {
                let times = tick.times.unwrap();
                window
                    .push_tick(
                        &persistence::Observation {
                            publication,
                            wall_ns: Some(times.wall_ns + seal.wall_offset_ns),
                            continuous_ns: Some(times.continuous_ns),
                            awake_ns: Some(times.awake_ns),
                        },
                        &tick,
                        &[],
                    )
                    .unwrap();
            }
        }
    }

    seal_window(root, &window);
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
            let mut session = LiveSession::open(&fixture.root).unwrap();

            let response = session.follow_latest().unwrap();
            fan_seal(&fixture);
            let mut saved = Session::open(&fixture.root).unwrap();
            let selected = saved.select(1000, 0).unwrap();
            let mut reopened = LiveSession::open(&fixture.root).unwrap();
            let restored = reopened.follow_latest().unwrap();

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
fn retains_fan_gauges_after_raw_observations_and_ring_slots_are_evicted() {
    // Goal: raw cache pressure and ring overwrite cannot erase scalar fan history.
    // Method: acquire 32 ticks of 12,000 processes through a two-slot real ring.
    let mut fixture = RingFixture::new_with_shape(&Shape {
        process_capacity: 12_000,
        ..FAN_SHAPE
    });
    let mut processes = Vec::with_capacity(12_000);
    fan_processes(&mut processes);
    fan_publish_processes(&mut fixture, 1, &processes);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    for tick in 2..=32 {
        fan_publish_processes(&mut fixture, tick, &processes);
        assert!(session.poll().unwrap());
    }
    assert!(session.observations.len() < 32);
    assert!(
        session
            .observations
            .front()
            .unwrap()
            .observation
            .publication
            > 1
    );

    let response = session.follow_latest().unwrap();

    assert_eq!(response["trends"].as_array().unwrap().len(), 32);
    assert_eq!(response["trends"][0]["fan_rpm"], json!([1.0, 0.0]));
    assert_eq!(response["summary"]["fan_rpm"], json!([32.0, 0.0]));
    assert!(session.catalog()["tail_cache_bytes"].as_u64().unwrap() <= 128 * 1024 * 1024);
}

fn fan_processes(processes: &mut Vec<collector::processes::Process>) {
    for pid in 1..=12_000 {
        processes.push(collector::processes::Process {
            pid,
            usage: collector::processes::Usage {
                awake_ns_at_start: 0,
                user_ns: 0,
                system_ns: 0,
                energy_nj: 0,
                instructions: 0,
                super_instructions: 0,
                cycles: 0,
                idle_wakeups: 0,
                interrupt_wakeups: 0,
                pageins: 0,
                resident_bytes: 0,
                footprint_bytes: 0,
                disk_read_bytes: 0,
                disk_written_bytes: 0,
            },
            task: collector::processes::Task {
                threads: 1,
                faults: 0,
                context_switches: 0,
                mach_syscalls: 0,
                unix_syscalls: 0,
            },
        });
    }
}

fn fan_publish_processes(
    fixture: &mut RingFixture,
    tick: u32,
    processes: &[collector::processes::Process],
) {
    let readings = Readings {
        run_tick: u64::from(tick),
        cpus: &[collector::machine::CPUTicks::default()],
        memory: &collector::machine::Memory::default(),
        interfaces: &[],
        ioreport: None,
        smc: Some(collector::ring::SmcReadings {
            power_watts: 5.0,
            soc_celsius: 40.0,
            cpu_celsius: Some(61.5),
            gpu_celsius: Some(52.25),
            fan_rpm: &[f32::from(u16::try_from(tick).unwrap()), 0.0],
        }),
        processes,
        processes_refused: 0,
        processes_gone: 0,
        coalitions: &[],
        events: &[],
        dropped: Dropped::default(),
    };
    fixture.writer.publish_observed(
        &readings,
        &ObservationTimes {
            wall_ns: (1000 + u64::from(tick)) * NANOSECONDS_PER_SECOND,
            continuous_ns: u64::from(tick) * NANOSECONDS_PER_SECOND,
            awake_ns: u64::from(tick) * NANOSECONDS_PER_SECOND,
        },
    );
}

#[test]
fn refuses_exact_overlap_when_a_committed_clock_changed() {
    // Goal: an equal publication key cannot combine readings with incompatible source clocks.
    // Method: acquire a real tick, then commit that key with its wall clock one ns later.
    let mut fixture = RingFixture::new_with_shape(&FAN_SHAPE);
    fan_publish(
        &mut fixture,
        &FanSample {
            tick: 1,
            second: 1000,
            channels: Some(&[1234.0]),
        },
    );
    let mut session = LiveSession::open(&fixture.root).unwrap();
    fan_seal_with(
        &fixture,
        &FanSeal {
            run: fixture.writer.identity().unwrap(),
            wall_offset_ns: 1,
        },
    );
    session.poll().unwrap();
    assert_eq!(session.catalog()["windows"], 1);

    let selected = session.select(1000, 0);
    let following = session.follow_latest();

    assert!(matches!(
        selected,
        Err(Error::Bound("changed committed point identity"))
    ));
    assert!(matches!(
        following,
        Err(Error::Bound("changed committed point identity"))
    ));
}

#[test]
fn keeps_different_run_alternatives_separate_without_copying_fan_readings() {
    // Goal: a saved point from another run never receives this live run's fan readings.
    // Method: commit a distinct run at the same second and retain both exact alternatives.
    let mut fixture = RingFixture::new_with_shape(&FAN_SHAPE);
    fan_publish(
        &mut fixture,
        &FanSample {
            tick: 1,
            second: 1000,
            channels: Some(&[1234.0]),
        },
    );
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let mut other = fixture.writer.identity().unwrap();
    other.run = collector::ring::Identity([17, 19]);
    let mut other_fixture = RingFixture::new_with_shape(&FAN_SHAPE);
    fan_publish(
        &mut other_fixture,
        &FanSample {
            tick: 1,
            second: 1000,
            channels: Some(&[2345.0, 0.0]),
        },
    );
    fan_seal_into(
        &other_fixture,
        &FanSeal {
            run: other,
            wall_offset_ns: 0,
        },
        &fixture.root,
    );
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
fn measures_bounded_fan_projection_and_retained_json_cost() {
    // Goal: record the isolated projection work and incremental serialized retained payload.
    // Method: project ten channels 10,000 times and serialize 900/4096 copies of one live point.
    let mut fixture = RingFixture::new_with_shape(&FAN_SHAPE);
    fan_publish(
        &mut fixture,
        &FanSample {
            tick: 1,
            second: 1000,
            channels: Some(&[1000.0; 10]),
        },
    );
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let response = session.follow_latest().unwrap();
    let source = session.observations.back().unwrap();
    let mut point = response["summary"].clone();
    let start = std::time::Instant::now();
    for _sample in 0..10_000 {
        point["fan_rpm"] = crate::live_fan_history::capture(
            &source.tables["ticks"],
            Some(&source.tables["fans"]),
            source.observation.publication,
        )
        .unwrap();
        std::hint::black_box(&point);
    }
    let projection_ns = start.elapsed().as_nanos();

    fan_measure_payload(&point, 900);
    fan_measure_payload(&point, 4096);

    println!(
        "{}",
        json!({"projection_iterations":10_000, "channels":10,
        "projection_ns":projection_ns,
        "profile":if cfg!(debug_assertions) {"debug"} else {"release"},
        "workload":"one acquired point; repeated fan-only projection; no additional queries"})
    );
}

fn fan_measure_payload(point: &Value, count: usize) {
    assert!(count <= OBSERVATIONS_MAX);

    let points = vec![point.clone(); count];
    let start = std::time::Instant::now();
    let with_fans = serde_json::to_vec(&points).unwrap().len();
    let serialized_ns = start.elapsed().as_nanos();
    let mut baseline = point.clone();
    baseline.as_object_mut().unwrap().remove("fan_rpm").unwrap();
    let without_fans = serde_json::to_vec(&vec![baseline; count]).unwrap().len();

    println!(
        "{}",
        json!({"points":count, "channels":10, "with_fans_json_bytes":with_fans,
        "without_fans_json_bytes":without_fans, "incremental_json_bytes":with_fans-without_fans,
        "with_fans_serialization_ns":serialized_ns,
        "workload":"repeated one real public point; serialization only; not a heap measurement"})
    );
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
    session.poll().unwrap();
    fan_publish(
        &mut fixture,
        &FanSample {
            tick: 3,
            second: 1900,
            channels: Some(&[3.0]),
        },
    );
    session.poll().unwrap();

    let following = session.follow_latest().unwrap();
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

struct FanExpectation {
    publication: u64,
    second: u64,
    channels: Value,
}

#[test]
fn keeps_nonfinite_fan_gaps_without_losing_the_publication_or_next_cursor() {
    // Goal: one invalid fan cannot suppress a healthy channel or the rest of the tick.
    // Method: follow/select NaN, infinity, then healthy readings from a real ring.
    let mut fixture = RingFixture::new_with_shape(&FAN_SHAPE);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    fan_publish(
        &mut fixture,
        &FanSample {
            tick: 1,
            second: 1000,
            channels: Some(&[f32::NAN, 2000.0]),
        },
    );

    assert!(session.poll().unwrap());
    assert_fan_publication(
        &mut session,
        &FanExpectation {
            publication: 1,
            second: 1000,
            channels: json!([null, 2000.0]),
        },
    );
    fan_publish(
        &mut fixture,
        &FanSample {
            tick: 2,
            second: 1001,
            channels: Some(&[f32::INFINITY, 0.0]),
        },
    );
    assert!(session.poll().unwrap());
    assert_fan_publication(
        &mut session,
        &FanExpectation {
            publication: 2,
            second: 1001,
            channels: json!([null, 0.0]),
        },
    );
    fan_publish(
        &mut fixture,
        &FanSample {
            tick: 3,
            second: 1002,
            channels: Some(&[1234.0, 2345.0]),
        },
    );
    assert!(session.poll().unwrap());
    assert_fan_publication(
        &mut session,
        &FanExpectation {
            publication: 3,
            second: 1002,
            channels: json!([1234.0, 2345.0]),
        },
    );
    assert!(!session.poll().unwrap());
}

fn assert_fan_publication(session: &mut LiveSession, expected: &FanExpectation) {
    let following = session.follow_latest().unwrap();
    let selected = session.select(expected.second, 0).unwrap();

    assert_eq!(following["summary"]["fan_rpm"], expected.channels);
    assert_eq!(selected["summary"]["fan_rpm"], expected.channels);
    assert_eq!(following["selected"]["publication"], expected.publication);
    assert_eq!(selected["selected"]["publication"], expected.publication);
    assert_eq!(following["summary"]["power_watts"], 5.0);
    assert_eq!(following["summary"]["soc_celsius"], 40.0);
    assert_eq!(selected["summary"]["power_watts"], 5.0);
    assert_eq!(selected["summary"]["soc_celsius"], 40.0);
    assert_eq!(following["summary"]["cpu_celsius"], 61.5);
    assert_eq!(selected["summary"]["gpu_celsius"], 52.25);
    assert_eq!(
        session.catalog()["latest_observation"]["publication"],
        expected.publication
    );
}

#[test]
fn reports_missing_saved_fan_source_while_retaining_live_readings() {
    // Goal: a missing saved fan source stays a visible history error, not measured fanless.
    // Method: hide only the genuine fan parquet, follow, then restore and retry the context.
    let mut fixture = RingFixture::new_following();
    follow_restoration_short(&mut fixture);
    let saved = Session::open(&fixture.root).unwrap();
    let fans = fixture
        .root
        .join(&saved.windows[0].data_directory)
        .join("fans.parquet");
    let held = fixture.root.join("held-fans.parquet");
    let mut session = LiveSession::open(&fixture.root).unwrap();
    std::fs::rename(&fans, &held).unwrap();

    let degraded = session.follow_latest().unwrap();

    assert!(degraded["context_error"].is_string());
    assert_eq!(degraded["summary"]["fan_rpm"], json!([62.0, 0.0]));
    assert!(session.restored.is_none());
    std::fs::rename(&held, &fans).unwrap();
    let recovered = session.follow_latest().unwrap();
    assert!(recovered["context_error"].is_null());
    assert_eq!(recovered["trends"][0]["fan_rpm"], json!([1.0, 0.0]));
    assert_eq!(recovered["selected"], degraded["selected"]);
}
