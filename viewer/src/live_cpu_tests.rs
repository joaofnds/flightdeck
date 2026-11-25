use collector::ring::{IdentityPublication, SupplementalReadings};

/// The tick of the run `cpu_publish` writes at which the clock of level 0 and of level 1, the
/// 1, 5 and 15 minute load averages, the open files and their share of the limit, and the CPU
/// sensor each read their highest, in that order. The open files and their share peak together,
/// since the limit stays the same.
const CPU_PEAK_TICKS: [u64; 8] = [31, 32, 33, 34, 35, 36, 36, 37];

/// The wall second of tick 0 of the run `cpu_publish` writes.
const WALL_SECOND_AT_TICK_ZERO: u64 = 1000;

/// The limit of open files macOS reports in the run `cpu_publish` writes.
const OPEN_FILES_LIMIT: u64 = 245_760;

#[test]
fn a_stretch_keeps_each_cpu_reading_the_session_tail_keeps() {
    // Goal: an older stretch keeps the lowest and highest of each level's clock, each load
    // average, the open files and their share of the limit, and the CPU sensor the CPU page
    // draws, with the reading that read the highest, as the session's older tail keeps them for
    // the same seconds, so a peak of the CPU charts drawn from a store is the peak the tail
    // draws. Method: each reading rises at its own pace and reads its highest at its own second
    // of the stretch of 1,030, and the stretch's summaries are set beside the reach of the
    // tail's point.
    let mut fixture = cpu_fixture();
    for tick in 1..=70 {
        cpu_publish(&mut fixture, tick);
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
    let held = [
        ("cpu_level_ghz/0", kept(&stretch.cpu_level_ghz[0])),
        ("cpu_level_ghz/1", kept(&stretch.cpu_level_ghz[1])),
        ("load_average/0", kept(&stretch.load_average[0])),
        ("load_average/1", kept(&stretch.load_average[1])),
        ("load_average/2", kept(&stretch.load_average[2])),
        ("open_files", kept(&stretch.open_files)),
        ("open_files_percent", kept(&stretch.open_files_percent)),
        ("cpu_celsius", kept(&stretch.cpu_celsius)),
    ];
    let reached = held.map(|(path, _)| {
        let reach = crate::number_reach(&point["reach"], path).unwrap().unwrap();
        (path, (reach.low, reach.high, reach.peak))
    });

    assert_eq!(held, reached);
    let peaks = held.map(|(_, kept)| kept.2.second());
    assert_eq!(peaks, CPU_PEAK_TICKS.map(|tick| WALL_SECOND_AT_TICK_ZERO + tick));
}

/// The CPU history the page draws from the store's View, set beside the one its reader reads of a
/// context that follows the newest second, for two runs of one recording, as across a collector
/// restart, given by the wall seconds of each.
macro_rules! view_cpu_history_cases {
    ($($name:ident: $first:expr, $second:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: the CPU history the page draws from the store's View is the one its reader
            // reads of a context that follows the newest second, over the seconds where the two
            // could part: a refused second, a run's first second and the oldest second, which
            // have no reading before them. Method: two runs of CPU readings, each with a refused
            // second, and the history of the first page of the logical CPUs built from the View
            // set beside the context's.
            let (fixture, _restarted) = cpu_two_runs($first, $second, NewestSecond::Refused);
            let store = crate::store::Store::start(&fixture.root).unwrap();
            let view = cpu_two_runs_view(&store, $first, $second);
            let context = crate::store::tests::followed_context(&store);
            let cancellation = Arc::new(Cancellation::default());
            assert_eq!(cpu_refused_count(&view), 2);
            assert_eq!(
                context.selected().observation.stamp.publication,
                view.seconds.last().unwrap().publication
            );

            let built = view.logical_cpus().unwrap().history;
            let read = context.cpu_history(0, &cancellation, 0).unwrap();

            assert_eq!(built.json(&Value::Null), read.json(&Value::Null));
        }
    )*};
}

mod a_view_builds_the_cpu_history_a_followed_context_builds {
    use super::*;

    view_cpu_history_cases! {
        when_the_runs_read_in_clock_order: 1001..=1005, 1010..=1013;
        // The second run's clock starts behind the first's, so the View holds its seconds in
        // another order than the context sorts them.
        when_the_clock_was_set_back_between_the_runs: 1001..=1005, 998..=1009;
        // The first run's last six seconds lie after the newest second, the second run's last,
        // so they are in the View but past the end of the history.
        when_an_older_run_reads_after_the_newest_second: 1001..=1010, 998..=1004;
        // The first run's first four seconds lie before the 900 seconds of the history.
        when_the_view_reaches_past_the_history: 1001..=1005, 1901..=1904;
    }
}

/// The logical CPUs of the newest second the page draws from the store's View, set beside those
/// its reader reads of a context that follows the newest second, for two runs of one recording
/// given by the wall seconds of each and whether the newest second's reading was refused.
macro_rules! view_cpu_detail_cases {
    ($($name:ident: $first:expr, $second:expr, $newest:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: the logical CPUs of the newest second the page draws from the store's View
            // are those its reader reads, with the reading their interval runs from. Method: two
            // runs of CPU readings, and the View's logical CPUs of the newest second set beside
            // the context's, leaving out the losses the reader names beside each identity.
            let (fixture, _restarted) = cpu_two_runs($first, $second, $newest);
            let store = crate::store::Store::start(&fixture.root).unwrap();
            let view = cpu_two_runs_view(&store, $first, $second);
            let context = crate::store::tests::followed_context(&store);

            let built = view.logical_cpus().unwrap().detail;

            assert_eq!(cpu_detail_json(&built), cpu_detail_read(&context));
        }
    )*};
}

mod a_view_builds_the_cpu_detail_a_followed_context_builds {
    use super::*;

    view_cpu_detail_cases! {
        // The newest second is refused, and the View holds the reading before it.
        when_the_newest_second_is_refused: 1001..=1005, 1010..=1013, NewestSecond::Refused;
        // The newest second is its run's first, so no reading of its run lies before it, though
        // the first run's last second does.
        when_the_newest_second_starts_its_run: 1001..=1005, 1010..=1010, NewestSecond::Read;
    }

    #[test]
    fn when_the_view_holds_one_second() {
        // Goal: the logical CPUs of a View of one second, which holds no reading before it, are
        // those the reader reads of it. Method: one run of one second, and the View's logical
        // CPUs set beside the context's.
        let mut fixture = cpu_fixture();
        cpu_publish_at(&mut fixture, 1, 1001, &cpu_two_runs_reading(false));
        let store = crate::store::Store::start(&fixture.root).unwrap();
        let view = crate::store::tests::view_where(&store, |view| view.seconds.len() == 1);
        let context = crate::store::tests::followed_context(&store);

        let built = view.logical_cpus().unwrap().detail;

        assert_eq!(cpu_detail_json(&built), cpu_detail_read(&context));
        assert!(built.previous.is_none());
    }

    #[test]
    fn when_the_clock_jumps_forward_past_the_history() {
        // Goal: after the clock jumps forward by more than the 900 seconds of the history, the
        // View still runs the interval from the run's tick before, as the reader does. Method:
        // one run whose third tick reads 1,001 seconds after its second, and the View's logical
        // CPUs set beside the context's.
        let mut fixture = cpu_fixture();
        for (tick, wall_second) in [(1, 1001), (2, 1002), (3, 2003)] {
            cpu_publish_at(&mut fixture, tick, wall_second, &cpu_two_runs_reading(false));
        }
        let store = crate::store::Store::start(&fixture.root).unwrap();
        let view = crate::store::tests::view_where(&store, |view| view.seconds.len() == 3);
        let context = crate::store::tests::followed_context(&store);

        let built = view.logical_cpus().unwrap().detail;

        assert_eq!(cpu_detail_json(&built), cpu_detail_read(&context));
        assert!(built.previous.is_some());
    }
}

/// The logical CPUs the reader reads of the newest second of `context`, but for the losses it
/// names beside each identity.
fn cpu_detail_read(context: &crate::store::QueryContext) -> Value {
    let cancellation = Arc::new(Cancellation::default());
    let mut read = context.cpu_detail(&cancellation, 0).unwrap().clone();
    for identity in ["observation", "previous_observation"] {
        if let Some(losses) = read[identity].as_object_mut() {
            losses.remove("reader_losses");
            losses.remove("window_losses");
        }
    }

    read
}

/// Whether the reading of the newest second of the runs `cpu_two_runs` writes is refused.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NewestSecond {
    Refused,
    Read,
}

/// Two runs of one recording, as across a collector restart, at the wall seconds of `first` and
/// of `second`: the first sealed, with its third second refused, and the second with its last
/// second refused as `newest` says. A second is refused by a load average below zero, which no
/// reading can hold.
fn cpu_two_runs(
    first: std::ops::RangeInclusive<u64>,
    second: std::ops::RangeInclusive<u64>,
    newest: NewestSecond,
) -> (RingFixture, RingFixture) {
    let mut fixture = cpu_fixture();
    for (tick, wall_second) in (1..).zip(first) {
        cpu_publish_at(&mut fixture, tick, wall_second, &cpu_two_runs_reading(tick == 3));
    }
    fixture.seal_held();
    let mut restarted = fixture.next_system_run();
    let last_wall_second = *second.end();
    for (tick, wall_second) in (1..).zip(second) {
        let refused = wall_second == last_wall_second && newest == NewestSecond::Refused;
        cpu_publish_at(&mut restarted, tick, wall_second, &cpu_two_runs_reading(refused));
    }

    (fixture, restarted)
}

/// The View of `store` once it holds every second of the two runs `cpu_two_runs` writes.
fn cpu_two_runs_view(
    store: &crate::store::Store,
    first: std::ops::RangeInclusive<u64>,
    second: std::ops::RangeInclusive<u64>,
) -> Arc<crate::store::View> {
    let count = first.count() + second.count();

    crate::store::tests::view_where(store, |view| view.seconds.len() == count)
}

fn cpu_two_runs_reading(refused: bool) -> collector::system::SystemReading {
    let one = if refused { -1.0 } else { 1.0 };

    collector::system::SystemReading {
        load_average: Some([one, 2.0, 3.0]),
        ..collector::system::SystemReading::default()
    }
}

fn cpu_refused_count(view: &crate::store::View) -> usize {
    view.seconds
        .iter()
        .filter(|second| {
            matches!(
                second.complete,
                crate::store::CompleteMachineState::Refused(_)
            )
        })
        .count()
}

/// `detail` as the reader writes the logical CPUs of a second, but for the losses beside each
/// identity.
fn cpu_detail_json(detail: &crate::store::CpuDetail) -> Value {
    let identity = |identity: &crate::store::CpuHistoryIdentity| {
        serde_json::json!({"run":identity.run,"boot":identity.boot,
            "publication":identity.publication,"run_tick":identity.run_tick,
            "wall_ns":identity.wall_ns})
    };
    let cpus = detail
        .busy_percent
        .iter()
        .enumerate()
        .map(|(cpu, busy)| serde_json::json!({"cpu":cpu,"busy_percent":busy}))
        .collect::<Vec<_>>();

    serde_json::json!({"observation":identity(&detail.identity),
        "previous_observation":detail.previous.as_ref().map(identity),
        "interval_available":detail.unavailable_reason.is_none(),
        "unavailable_reason":detail.unavailable_reason,"cpus":cpus})
}

/// A ring with the system records, which carry the load averages, the open files and the clock
/// of each core's states, and room for two cores of two states.
fn cpu_fixture() -> RingFixture {
    let shape = Shape {
        cpu_capacity: 2,
        core_capacity: 2,
        state_capacity: 2,
        ..one_of_each(120)
    };
    RingFixture::started_with("cpu", &shape, Registration::start_with_system)
}

/// Publishes tick `tick` of a run of two levels of one core each, whose clocks, load averages,
/// open files and CPU sensor each rise at their own pace and read their highest at their own
/// tick of `CPU_PEAK_TICKS`. Each core spends a millisecond a tick in its 1 GHz state and a
/// rising time in its 2 GHz state, much more at its level's peak.
fn cpu_publish(fixture: &mut RingFixture, tick: u64) {
    cpu_publish_at(
        fixture,
        tick,
        WALL_SECOND_AT_TICK_ZERO + tick,
        &cpu_publish_reading(tick),
    );
}

/// Publishes tick `tick` as `cpu_publish` does, read at `wall_second` with the system reading
/// `reading`.
fn cpu_publish_at(
    fixture: &mut RingFixture,
    tick: u64,
    wall_second: u64,
    reading: &collector::system::SystemReading,
) {
    let [_, _, _, _, _, _, _, sensor_peak] = CPU_PEAK_TICKS;
    let cores = cpu_publish_cores(tick);
    let sensor_rise = if tick == sensor_peak { 20.0 } else { 0.0 };
    let sensor = 50.0 + f32::from(u8::try_from(tick).unwrap()) / 10.0 + sensor_rise;
    let machine = cpu_publish_machine();
    let clock = collector::clock_states::ClockTables::from_tables(vec![vec![1_000_000, 2_000_000]]);
    let system = collector::ring::SystemPublication {
        reading,
        machine: &machine,
        clock: Some(&clock),
        process_gpu_ns: &[],
    };
    let cpu = collector::machine::CPUTicks {
        user: u32::try_from(10 * tick).unwrap(),
        idle: u32::try_from(90 * tick).unwrap(),
        ..collector::machine::CPUTicks::default()
    };
    fixture
        .writer
        .publish_with_disks(
            &Readings {
                run_tick: tick,
                cpus: &[cpu, cpu],
                memory: &collector::machine::Memory::default(),
                interfaces: &[],
                ioreport: Some(collector::ioreport::IOReportReadings {
                    gpu_energy_nj: 0,
                    gpu_active_ns: &[],
                    cores: &cores,
                }),
                smc: Some(collector::ring::SmcReadings {
                    power_watts: 5.0,
                    soc_celsius: 40.0,
                    cpu_celsius: Some(sensor),
                    gpu_celsius: None,
                    fan_rpm: &[],
                }),
                processes: &[],
                processes_refused: 0,
                processes_gone: 0,
                coalitions: &[],
                events: &[],
                dropped: Dropped::default(),
            },
            Some(&ObservationTimes {
                wall_ns: wall_second * NANOSECONDS_PER_SECOND,
                continuous_ns: tick * NANOSECONDS_PER_SECOND,
                awake_ns: tick * NANOSECONDS_PER_SECOND,
            }),
            &IdentityPublication {
                processes: &[],
                events: &[],
            },
            &SupplementalReadings {
                battery: None,
                disks: None,
                system: Some(&system),
            },
        )
        .unwrap();
}

/// The states of the two cores at tick `tick`, the core of the slower level first. Each spends a
/// millisecond a tick in its 1 GHz state and a rising time in its 2 GHz state, much more from
/// its level's tick of `CPU_PEAK_TICKS` on.
fn cpu_publish_cores(tick: u64) -> [collector::ioreport::States; 2] {
    let [fast_peak, slow_peak, _, _, _, _, _, _] = CPU_PEAK_TICKS;
    let core = |peak: u64| {
        let rise = if tick >= peak { 100_000_000 } else { 0 };
        collector::ioreport::States::with_active_ns(&[
            1_000_000 * tick,
            10_000 * tick * (tick + 1) / 2 + rise,
        ])
    };

    [core(slow_peak), core(fast_peak)]
}

/// The system reading of tick `tick`, whose load averages and open files each rise at their own
/// pace and read their highest at their own tick of `CPU_PEAK_TICKS`.
fn cpu_publish_reading(tick: u64) -> collector::system::SystemReading {
    let [_, _, one_peak, five_peak, fifteen_peak, files_peak, _, _] = CPU_PEAK_TICKS;
    let small = f64::from(u8::try_from(tick).unwrap());
    let load = |base: f64, peak: u64| {
        let rise = if tick == peak { 10.0 } else { 0.0 };
        base + small / 100.0 + rise
    };
    let files_rise = if tick == files_peak { 50_000 } else { 0 };

    collector::system::SystemReading {
        load_average: Some([
            load(1.0, one_peak),
            load(2.0, five_peak),
            load(3.0, fifteen_peak),
        ]),
        open_files: Some(10_000 + tick + files_rise),
        open_files_limit: Some(OPEN_FILES_LIMIT),
        ..collector::system::SystemReading::default()
    }
}

/// The machine's facts, two levels of one core each, the faster first.
fn cpu_publish_machine() -> collector::system::MachineFacts {
    let level = |name: &[u8]| collector::system::PerformanceLevel {
        name: collector::system::FactText::new(name).unwrap(),
        logical_cpus: 1,
    };

    collector::system::MachineFacts {
        levels: Some([level(b"Super"), level(b"Performance")]),
        ..collector::system::MachineFacts::default()
    }
}
