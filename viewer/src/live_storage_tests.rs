use collector::disks::{DiskReading, DiskSnapshot, ScanState};
use collector::ring::{IdentityPublication, SupplementalReadings};

use crate::store::PointRate;

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
    RingFixture::started_with(
        "storage",
        &one_of_each(slot_count),
        Registration::start_with_disks,
    )
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

#[test]
fn a_followed_chart_of_the_most_disks_presents_every_charted_second() {
    // Goal: the Store presents the Overview's whole chart for the most disks the collector reads.
    // Method: follow a ring of 1,000 seconds of that many disks, then read the newest selection.
    let mut fixture = storage_fixture_with_slots(1024);
    for tick in 1..=1000 {
        let disks = storage_snapshot_of_disks(tick, collector::disks::DISKS_MAX);
        storage_publish(&mut fixture, tick, Some(&disks));
    }
    let store = crate::store::Store::start(&fixture.root).unwrap();
    let context = storage_context_at(&store, 2000);

    let selection = context.selection(&Arc::new(Cancellation::default()), 0);

    let trends = selection.unwrap().trends().unwrap().len();
    assert_eq!(u64::try_from(trends).unwrap(), CHARTED_SECONDS);
}

fn storage_snapshot_of_disks(tick: u64, count: usize) -> DiskSnapshot {
    let mut snapshot = DiskSnapshot {
        source_continuous_ns: tick * NANOSECONDS_PER_SECOND,
        state: ScanState::Complete,
        count,
        ..DiskSnapshot::default()
    };
    for (index, record) in snapshot.records[..count].iter_mut().enumerate() {
        let disk = u64::try_from(index).unwrap() + 1;
        *record = DiskReading {
            driver_id: disk,
            media_id: 100 + disk,
            read_bytes: Some(tick * 4096 * disk),
            write_bytes: Some(tick * 8192 * disk),
            read_operations: Some(tick * 2 * disk),
            write_operations: Some(tick * 4 * disk),
            read_errors: Some(0),
            write_errors: Some(0),
            read_time_ns: Some(tick * 6_000_000 * disk),
            write_time_ns: Some(tick * 20_000_000 * disk),
            ..DiskReading::default()
        };
        let name = format!("disk{index}");
        record.name[..name.len()].copy_from_slice(name.as_bytes());
    }
    snapshot
}

/// Follows the Store until its context selects `second`.
fn storage_context_at(store: &crate::store::Store, second: u64) -> Arc<crate::store::QueryContext> {
    let handle = store.query_handle();
    let request = crate::store::QueryRequest {
        epoch: 0,
        source: None,
        selection: crate::store::QuerySelection::FollowLatest,
    };
    handle.submit(request).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    for _ in 0..3000 {
        if let Some(reply) = handle.take() {
            match reply.outcome {
                crate::store::QueryOutcome::Ready(context)
                    if context.selected_second() == second =>
                {
                    return context;
                }
                crate::store::QueryOutcome::Unavailable(reason) => {
                    panic!("unavailable: {reason:?}")
                }
                _ => {
                    handle.submit(request).unwrap();
                }
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the Store never selected {second}"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    panic!("the Store never selected {second} within its attempts");
}

#[test]
fn a_stretch_keeps_each_storage_reading_the_session_tail_keeps() {
    // Goal: an older stretch keeps the lowest and highest of each operation, error and mean
    // service time rate the Storage page draws, with the reading that read the highest, as the
    // session's older tail keeps them for the same seconds, so a peak of the Storage charts drawn
    // from a store is the peak the tail draws. Method: each counter of one disk rises at its own
    // pace and jumps at its own second of the stretch of 1,030, and the stretch's summaries are
    // set beside the reach of the tail's point.
    let mut fixture = storage_fixture_with_slots(120);
    for tick in 1..=70 {
        storage_publish(&mut fixture, tick, Some(&storage_peaks(tick)));
    }
    fixture.seal_held();
    let store = crate::store::Store::start(&fixture.root).unwrap();
    // The stretch of 1,030 to 1,039, which the tail's point of 1,030 reaches over.
    let stretch_number = 103;
    let view = crate::store::tests::view_where(&store, |view| {
        view.seconds.len() == 70 && view.stretches.iter().any(|held| held.stretch == stretch_number)
    });
    let stretch = view
        .stretches
        .iter()
        .find(|held| held.stretch == stretch_number)
        .map(|held| held.machine.as_ref())
        .unwrap();
    let tail = LiveSession::open(&fixture.root)
        .unwrap()
        .earlier_context(1070 + CHARTED_SECONDS - 1)
        .unwrap();
    let point = tail["earlier_trends"]
        .as_array()
        .unwrap()
        .iter()
        .find(|point| {
            point["wall_ns"].as_u64().unwrap() / NANOSECONDS_PER_SECOND / 10 == stretch_number
        })
        .unwrap();

    let kept = |reduction: &model::summary::Reduction<crate::store::MachinePlace>| {
        let measured = reduction.measured.unwrap();
        (
            measured.minimum.value,
            measured.maximum.value,
            measured.maximum.place,
        )
    };
    let held =
        crate::store::StorageRate::ALL.map(|rate| (rate.key(), kept(stretch.storage_rate(rate))));
    let reached = held.map(|(key, _)| {
        let path = crate::session::number_path(Some("storage"), key, None);
        let reach = crate::number_reach(&point["reach"], &path).unwrap().unwrap();
        (key, (reach.low, reach.high, reach.peak))
    });

    assert_eq!(held, reached);
    let peaks = held.map(|(_, kept)| kept.2.second());
    assert_eq!(peaks, [1031, 1032, 1033, 1034, 1035, 1036]);
}

/// The disks of tick `tick` of a run whose one disk's operations, errors and times each rise at
/// their own pace and jump at their own tick of 31 to 36, so each rate reads its highest there.
fn storage_peaks(tick: u64) -> DiskSnapshot {
    let counter = |pace: u64, peak: u64| {
        let rise = if tick >= peak { 49 * pace } else { 0 };
        pace * tick + rise
    };
    let mut snapshot = storage_snapshot(tick);
    let record = &mut snapshot.records[0];
    record.read_operations = Some(counter(2, 31));
    record.write_operations = Some(counter(4, 32));
    record.read_errors = Some(counter(1, 33));
    record.write_errors = Some(counter(3, 34));
    record.read_time_ns = Some(counter(6_000_000, 35));
    record.write_time_ns = Some(counter(20_000_000, 36));
    snapshot
}
