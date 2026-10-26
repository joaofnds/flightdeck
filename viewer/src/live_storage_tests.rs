use collector::disks::{DiskReading, DiskSnapshot, ScanState};
use collector::ring::{DictionaryCapacity, IdentityPublication, SupplementalReadings};

#[test]
fn scalar_cache_refusal_preserves_accepted_readings_and_allows_a_smaller_retry() {
    // Goal: scalar pressure refuses atomically while keeping the accepted reading inspectable.
    // Method: fill a real disk ring's scalar context, then retry its refused tick without disks.
    let mut fixture = storage_fixture();
    storage_publish(&mut fixture, 1, Some(&storage_full_snapshot(1)));
    let mut session = LiveSession::open(&fixture.root).unwrap();
    for tick in 2..=900 {
        storage_publish(&mut fixture, tick, Some(&storage_full_snapshot(tick)));
        let accepted = session.observations.back().unwrap().json();
        let point_count = session.points.len();
        let bytes = session.cache_bytes;

        if let Err(error) = session.poll() {
            assert_eq!(session.cache_bytes, bytes, "refused candidate: {error}");
            assert_eq!(session.points.len(), point_count);
            assert!(session.cache_bytes <= CACHE_BYTES_MAX);
            assert_eq!(session.follow_latest().unwrap()["selected"], accepted);
            assert_eq!(session.page("ticks", 0).unwrap()["observation"], accepted);
            assert!(session.poll().is_err());
            assert_eq!(session.cache_bytes, bytes);
            assert_eq!(session.points.len(), point_count);
            assert_eq!(session.follow_latest().unwrap()["selected"], accepted);
            let Found::Publication { publication, .. } = session.sources[0].read_tick().unwrap()
            else {
                panic!("refused publication must remain readable");
            };
            assert_eq!(publication, tick);
            session.sources[0].tick.disks = None;
            assert!(session.poll_publication(0, publication).unwrap());
            assert_eq!(session.points.len(), point_count + 1);
            assert_eq!(
                session.follow_latest().unwrap()["selected"]["publication"],
                publication
            );
            let identities: BTreeSet<_> = session
                .points
                .iter()
                .map(|point| point["publication"].as_u64().unwrap())
                .collect();
            assert_eq!(identities.len(), session.points.len());
            assert!(session.cache_bytes <= CACHE_BYTES_MAX);
            return;
        }
        assert!(session.cache_bytes <= CACHE_BYTES_MAX);
    }
    panic!("sixteen-disk context must exercise scalar cache refusal");
}

fn storage_full_snapshot(tick: u64) -> DiskSnapshot {
    let mut snapshot = storage_snapshot(tick);
    snapshot.count = collector::disks::DISKS_MAX;
    for index in 1..snapshot.count {
        snapshot.records[index] = DiskReading {
            driver_id: 91 + u64::try_from(index).unwrap(),
            media_id: 92 + u64::try_from(index).unwrap(),
            ..snapshot.records[0]
        };
    }
    snapshot
}

#[test]
fn follows_selects_and_reopens_disk_values_with_observation_gaps() {
    // Goal: exact per-device rates and unavailable observations survive the full recorded path.
    // Method: publish through a registered extended ring, seal its ticks, and reopen both readers.
    let mut fixture = storage_fixture();
    storage_publish(&mut fixture, 1, Some(&storage_snapshot(1)));
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let first = session.follow_latest().unwrap();
    storage_publish(&mut fixture, 2, Some(&storage_snapshot(2)));
    session.poll().unwrap();
    let measured = session.follow_latest().unwrap();
    storage_publish(&mut fixture, 3, None);
    session.poll().unwrap();
    let missing = session.follow_latest().unwrap();
    storage_publish(&mut fixture, 4, Some(&storage_snapshot(4)));
    session.poll().unwrap();
    let restarted = session.follow_latest().unwrap();
    storage_publish(&mut fixture, 5, Some(&storage_snapshot(5)));
    session.poll().unwrap();
    let latest = session.follow_latest().unwrap();
    storage_seal(&fixture);
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
    private::create_private_dir(&root).unwrap();
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

fn storage_seal(fixture: &RingFixture) {
    let mut reader = RingReader::open_generation(fixture.writer.name()).unwrap();
    let mut tick = persistence::tick_buffer(&reader.shape()).unwrap();
    let mut window = Window::new(
        WindowIdentity {
            run: fixture.writer.identity().unwrap(),
            window: 0,
        },
        Limits::from_shape(&reader.shape()).unwrap(),
    );
    for _attempt in 0..=8 {
        match reader.read_into(&mut tick).unwrap() {
            Found::Nothing => break,
            Found::Missed { .. } => panic!("fixture publications overwritten"),
            Found::Publication { publication, .. } => {
                let times = tick.times.unwrap();
                window
                    .push_tick(
                        &persistence::Observation {
                            publication,
                            wall_ns: Some(times.wall_ns),
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
    seal_window(&fixture.root, &window);
}

#[test]
fn reads_mixed_extended_and_legacy_minutes_without_fabricating_disks() {
    // Goal: a recording can retain measured disk history beside a legacy unavailable minute.
    // Method: seal a disk minute, then an original-schema minute, and query both with Session.
    let mut fixture = storage_fixture();
    storage_publish(&mut fixture, 1, Some(&storage_snapshot(1)));
    storage_publish(&mut fixture, 2, Some(&storage_snapshot(2)));
    storage_seal(&fixture);
    let shape = RingReader::open_generation(fixture.writer.name())
        .unwrap()
        .shape();
    let mut tick = persistence::tick_buffer(&shape).unwrap();
    tick.run_tick = 61;
    let mut legacy = Window::new(
        WindowIdentity {
            run: fixture.writer.identity().unwrap(),
            window: 1,
        },
        Limits::from_shape(&shape).unwrap(),
    );
    legacy
        .push_tick(
            &persistence::Observation {
                publication: 3,
                wall_ns: Some(1061 * NANOSECONDS_PER_SECOND),
                continuous_ns: Some(61 * NANOSECONDS_PER_SECOND),
                awake_ns: Some(61 * NANOSECONDS_PER_SECOND),
            },
            &tick,
            &[],
        )
        .unwrap();
    seal_window(&fixture.root, &legacy);

    let mut saved = Session::open(&fixture.root).unwrap();
    let extended = saved.select(1002, 0).unwrap();
    let legacy = saved.select(1061, 0).unwrap();

    assert_eq!(saved.catalog()["windows"], 2);
    assert_eq!(extended["summary"]["storage"]["read_bps"], 4096.0);
    assert!(legacy["summary"]["storage"].is_null());
    assert_eq!(legacy["trends"][1]["storage"]["read_bps"], 4096.0);
    assert!(legacy["trends"][2]["storage"].is_null());
}

mod merge_tests {
    use super::*;
    include!("live_storage_merge_tests.rs");
}
