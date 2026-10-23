use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use collector::battery::BatteryReading;
use collector::clock_states::ClockTables;
use collector::disks::{DiskReading, DiskSnapshot, ScanState};
use collector::interfaces::{IFNAMSIZ, Interface, Name};
use collector::ioreport::States;
use collector::machine::{CPUTicks, Memory, Pressure};
use collector::processes::{Process, Task, Unreadable, Usage};
use collector::recording::{RegisteredRun, Registry};
use collector::ring::{
    DictionaryCapacity, Dropped, Found, IOReportReadings, IdentityObservation, IdentityPublication,
    ObservationTimes, Place, ProcessCreation, ProcessIdentity, ProgramBytes, ProgramReference,
    Readings, RingReader, RunIdentity, Shape, SmcReadings, SupplementalReadings, SystemPublication,
    Tick, UsageAvailability, Writer,
};
use collector::system::{FactText, MachineFacts, PerformanceLevel, SystemReading};
use collector::tracker::{Event, Noticed};
use model::apps::{Apps, Creation, Measurement, ProgramUnavailable};
use model::programs::{AppKey, Capacity, Programs};
use persistence::engine::{Engine, Row};
use persistence::query::{TableSource, table_source};
use persistence::schema::{APP_PARTS, Table};
use polars::polars_utils::compression::ZstdLevel;
use polars::prelude::{
    DataFrame, DataType, Expr, IntoLazy, Null, ParquetCompression, ParquetWriter,
    StatisticsOptions, col, lit, when,
};

use super::apps::{ROW_APP, ROW_HEADER, ROW_PROCESS, VERSION};
use super::processes::read_processes;
use super::{
    AppMinute, AppParts, MachineMinute, MinuteTicks, Refused, SealedMinute, derive_apps, read_apps,
    read_machine,
};
use crate::cancellation::Cancellation;
use crate::live::identity_fixture::Fixture;
use crate::live_apps::Frame;
use crate::live_apps::committed::Endpoint;

/// Tells apart the rings the tests of this file make.
static NEXT: AtomicU64 = AtomicU64::new(0);

#[test]
fn a_sealed_minute_holds_the_machine_readings_the_ring_held() {
    // Goal: the recording and the ring hand the store the same machine readings, so one rate
    // rule serves both.
    // Method: publish three ticks with distinct readings of two of each CPU, interface, GPU
    // state, core state and fan, the second without IOReport and the third without the SMC,
    // take the ring's ticks as the store would, seal them as one minute, and decode it.
    let mut fixture = Fixture::new_mode(false, 2, None);
    wide_ring(&mut fixture, |place, shape, identity| {
        Writer::create_recording(place, shape, identity, 0)
    });
    for step in 1..=3 {
        publish_machine(&mut fixture, step, None);
    }
    let mut expected = MachineMinute::default();
    for (publication, tick) in ring_ticks(&fixture) {
        expected.push_tick(publication, &tick);
    }
    fixture.seal_minute(0);
    let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();
    let mut decoded = MachineMinute::default();

    read_machine(&minute, &mut decoded).unwrap();

    let states = (expected.gpu_states.len(), expected.core_states.len());
    assert_eq!(
        (expected.ticks.len(), states, expected.fans.len()),
        (3, (4, 8), 4)
    );
    assert_eq!(decoded, expected);
}

#[test]
fn a_sealed_minute_holds_the_system_battery_and_disk_readings_the_ring_held() {
    // Goal: the recording and the ring hand the store the same thermal pressure, performance
    // levels, battery and disks, and the clock of each core state, so the store reads them
    // one way from either.
    // Method: publish three ticks into a ring that carries these readings, the first with all
    // of them, the second with disks alone and the third with no disks and no levels, take
    // the ring's ticks as the store would, seal them as one minute, and decode it.
    let mut fixture = Fixture::new_mode(false, 2, None);
    system_ring(&mut fixture);
    for step in 1..=3 {
        publish_supplemented(&mut fixture, step);
    }
    let mut expected = MachineMinute::default();
    for (publication, tick) in ring_ticks(&fixture) {
        expected.push_tick(publication, &tick);
    }
    fixture.seal_minute(0);
    let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();
    let mut decoded = MachineMinute::default();

    read_machine(&minute, &mut decoded).unwrap();

    let held: Vec<_> = expected
        .ticks
        .iter()
        .map(|tick| {
            let levels = tick.performance_levels.is_some();
            let source = (tick.battery.is_some(), tick.disks.is_some());
            (tick.thermal_pressure, levels, source)
        })
        .collect();
    let all = (Some(1), true, (true, true));
    let disks = (None, false, (false, true));
    let no_disks = (Some(3), false, (true, false));
    assert_eq!(held, [all, disks, no_disks]);
    let clocked = expected
        .core_states
        .iter()
        .filter(|state| state.frequency_khz.is_some())
        .count();
    assert_eq!(clocked, 8);
    assert_eq!(decoded, expected);
}

macro_rules! refuses_a_damaged_table {
    ($($name:ident, $table:expr, $reason:literal, $damage:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: a minute whose table holds what format 1 never writes is refused with the
            // reason, and never read as readings.
            // Method: seal a minute of system, battery and disk readings, replace one of its
            // tables with a copy the case damages, and decode it.
            let fixture = sealed_system_minute();
            damage_table(&fixture, $table.name(), $damage);
            let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();

            let refused = read_machine(&minute, &mut MachineMinute::default());

            let refusal = matches!(
                refused,
                Err(Refused::Decode(persistence::Error::Bound($reason)))
            );
            assert!(refusal, "{refused:?}");
        }
    )*};
}

refuses_a_damaged_table! {
    refuses_a_column_group_missing_a_column, Table::Ticks, "incomplete column group",
        |frame| frame.drop("battery_adapter_watts").unwrap();
    refuses_another_battery_version, Table::Ticks, "battery version",
        |frame| with_column(frame, lit(2).cast(DataType::UInt8).alias("battery_version"));
    refuses_a_battery_reading_without_its_source_time, Table::Ticks,
        "battery payload without source time",
        |frame| with_column(frame, null("battery_source_continuous_ns", DataType::UInt64));
    refuses_part_of_the_performance_levels, Table::Ticks, "incomplete performance levels",
        |frame| {
            let logical_cpus = "machine_performance_level1_logical_cpus";
            with_column(frame, null(logical_cpus, DataType::UInt32))
        };
    refuses_an_empty_performance_level_name, Table::Ticks, "performance level name",
        |frame| with_column(frame, lit("").alias("machine_performance_level0_name"));
    refuses_disks_that_do_not_decode, Table::Ticks, "physical disk text extent",
        |frame| with_column(frame, lit("0").alias("physical_disks"));
    refuses_an_unknown_memory_pressure, Table::Memory, "memory pressure code",
        |frame| with_column(frame, lit(9).cast(DataType::UInt8).alias("pressure"));
    refuses_an_interface_name_longer_than_the_kernel_allows, Table::Interfaces, "interface name",
        |frame| with_column(frame, lit("utun_longer_than_ifnamsiz").alias("name"));
    refuses_two_ticks_of_one_publication, Table::Ticks, "tick order",
        |frame| with_column(frame, col("publication").min().alias("publication"));
    refuses_a_reading_of_a_tick_the_minute_does_not_hold, Table::Cpus, "reading of no tick",
        |frame| with_column(frame, (col("publication") + lit(100)).alias("publication"));
    refuses_two_readings_of_one_cpu_at_a_tick, Table::Cpus, "reading order",
        |frame| with_column(frame, lit(0).cast(DataType::UInt32).alias("cpu"));
    refuses_cpus_out_of_their_order, Table::Cpus, "reading order",
        |frame| with_column(frame, (lit(1) - col("cpu")).cast(DataType::UInt32).alias("cpu"));
    refuses_core_states_out_of_their_order, Table::CoreStates, "reading order",
        |frame| {
            let state = (lit(1) - col("state")).cast(DataType::UInt32);
            with_column(frame, state.alias("state"))
        };
}

#[test]
fn a_sealed_minute_holds_a_row_for_every_app_its_seal_derived() {
    // Goal: a minute sealed with App parts gives the facts of each observation and a row for
    // every App the seal derived there, so the store holds its Apps without the App rule.
    // Method: seal three observations of several Apps with their App parts, and read the
    // minute's Apps.
    let fixture = sealed_apps_minute();
    let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();
    let mut apps = AppMinute::default();

    let parts = read_apps(&minute, &mut apps).unwrap();

    let counted: u64 = apps.samples.iter().map(|sample| sample.app_count).sum();
    assert_eq!(parts, AppParts::Read);
    assert_eq!(apps.samples.len(), 3);
    assert!(apps.apps.len() > apps.samples.len());
    assert_eq!(collector::u64_from_usize(apps.apps.len()), counted);
}

#[test]
fn a_minute_sealed_without_app_parts_gives_no_apps() {
    // Goal: a minute sealed without App parts says so and gives no Apps, so the store runs the
    // App rule on it.
    // Method: seal three observations without deriving App parts, and read the minute's Apps.
    let mut fixture = Fixture::new();
    for second in 1000..=1002 {
        fixture.publish_with_system_rows(second);
    }
    fixture.seal_minute(0);
    let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();
    let mut apps = AppMinute::default();

    let parts = read_apps(&minute, &mut apps).unwrap();

    assert_eq!((parts, apps), (AppParts::Missing, AppMinute::default()));
}

#[test]
fn a_minute_whose_app_parts_are_of_another_version_gives_no_apps() {
    // Goal: a build reads no Apps from App parts a later build wrote in columns it may not
    // know, and leaves the minute to the App rule.
    // Method: rewrite a sealed minute's App parts with the next version, and read its Apps.
    let fixture = sealed_apps_minute();
    damage_table(&fixture, APP_PARTS, |frame| {
        with_column(
            frame,
            lit(VERSION + 1).cast(DataType::UInt8).alias("version"),
        )
    });
    let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();
    let mut apps = AppMinute::default();

    let parts = read_apps(&minute, &mut apps).unwrap();

    assert_eq!(
        (parts, apps),
        (AppParts::Version(VERSION + 1), AppMinute::default())
    );
}

macro_rules! refuses_damaged_app_parts {
    ($($name:ident, $reason:literal, $damage:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: a minute whose App parts hold what format 1 never writes is refused with
            // the reason, and never read as Apps.
            // Method: seal a minute of several Apps with their App parts, replace the table
            // with a copy the case damages, and read the minute's Apps.
            let fixture = sealed_apps_minute();
            damage_table(&fixture, APP_PARTS, $damage);
            let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();

            let refused = read_apps(&minute, &mut AppMinute::default());

            let refusal = matches!(
                refused,
                Err(Refused::Decode(persistence::Error::Bound($reason)))
            );
            assert!(refusal, "{refused:?}");
        }
    )*};
}

refuses_damaged_app_parts! {
    refuses_app_parts_that_lost_a_row_the_seal_counted, "sealed parts rows",
        |frame| frame.head(Some(frame.height() - 1));
    refuses_app_parts_of_two_versions, "sealed parts version",
        |frame| with_column(frame, of_rows(ROW_HEADER, lit(VERSION + 1), "version"));
    refuses_an_observation_that_counts_an_app_more_than_it_keeps, "sealed parts Apps",
        |frame| with_column(frame, (col("app_count") + lit(1)).alias("app_count"));
    refuses_apps_out_of_their_order, "sealed parts App",
        |frame| with_column(frame, (col("app_index") + lit(1)).alias("app_index"));
    refuses_a_measure_that_counts_more_members_than_its_app_has, "sealed parts measure",
        |frame| {
            let unavailable = (col("members") + lit(1)).cast(DataType::UInt32);
            with_column(frame, unavailable.alias("app_cpu_ns_unavailable_members"))
        };
    refuses_an_app_of_more_members_than_a_history_holds, "sealed parts members",
        |frame| with_column(frame, lit(u32::MAX).cast(DataType::UInt32).alias("members"));
    refuses_an_unknown_app_kind, "sealed App kind",
        |frame| with_column(frame, lit(7).cast(DataType::UInt8).alias("app_kind"));
    refuses_a_row_of_an_unknown_kind, "sealed parts row kind",
        |frame| with_column(frame, of_rows(ROW_PROCESS, lit(3), "row_kind"));
    refuses_apps_before_the_facts_of_their_observation, "sealed parts header",
        |frame| {
            let later = col("publication") + lit(100);
            with_column(frame, of_rows(ROW_HEADER, later, "publication"))
        };
    refuses_apps_of_an_observation_the_facts_do_not_name, "sealed parts header",
        |frame| {
            let last = col("row_kind")
                .eq(lit(ROW_APP))
                .and(col("publication").eq(col("publication").max()));
            let later = when(last).then(col("publication") + lit(100));
            with_column(frame, later.otherwise(col("publication")).alias("publication"))
        };
    refuses_two_observations_of_one_publication, "sealed parts publication",
        |frame| {
            let second = col("publication").eq(col("publication").min() + lit(1));
            let first = when(second).then(col("publication").min());
            with_column(frame, first.otherwise(col("publication")).alias("publication"))
        };
    refuses_an_observation_the_minute_does_not_hold, "sealed parts publication",
        |frame| with_column(frame, (col("publication") + lit(100)).alias("publication"));
}

#[test]
fn a_sealed_minute_gives_the_frames_the_replay_of_its_files_gives() {
    // Goal: the store builds each observation's frame from a minute's processes and events
    // without SQL, exactly as the SQL replay of the minute builds it.
    // Method: seal three observations whose events start, exec and exit processes, exits of
    // measured, refused and gone usage among them, and compare the decoded frames with the
    // replay's over the same files.
    let fixture = sealed_events_minute();
    let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();
    let frames = decoded_frames(&minute).unwrap();

    assert_eq!((frames.len(), minute.rows("events")), (3, Some(6)));
    assert_eq!(frames, replayed_frames(&fixture));
}

#[test]
fn a_minute_sealed_before_process_identity_gives_the_frames_its_replay_gives() {
    // Goal: a minute sealed before processes carried their identity gives frames of processes
    // whose identity is unsupported, as its replay gives them.
    // Method: seal three observations of a ring without identity, and compare the decoded
    // frames with the replay's over the same files.
    let fixture = sealed_system_minute();
    let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();
    let frames = decoded_frames(&minute).unwrap();

    assert_eq!(frames.len(), 3);
    assert_eq!(frames, replayed_frames(&fixture));
}

#[test]
fn a_minute_sealed_before_process_identity_keeps_the_usage_its_events_held() {
    // Goal: an event of a minute sealed before processes carried their identity keeps the
    // usage availability it held, as its replay gives it.
    // Method: seal exits of measured, refused and gone usage from a ring without identity, and
    // compare the decoded frames with the replay's over the same files.
    let fixture = sealed_events_minute_of(Fixture::new_mode(false, 2, None));
    let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();
    let frames = decoded_frames(&minute).unwrap();

    assert_eq!(frames.len(), 3);
    assert_eq!(frames, replayed_frames(&fixture));
}

#[test]
fn a_minute_of_gpu_time_gives_the_frames_its_replay_gives() {
    // Goal: a minute whose processes carry their GPU time gives frames that hold it, as its
    // replay gives them.
    // Method: seal three observations of one process with GPU time and one without, and
    // compare the decoded frames with the replay's over the same files.
    let fixture = sealed_gpu_minute();
    let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();
    let frames = decoded_frames(&minute).unwrap();

    assert_eq!(frames.len(), 3);
    assert_eq!(frames, replayed_frames(&fixture));
}

#[test]
fn a_minute_sealed_without_gpu_time_gives_the_frames_its_replay_gives() {
    // Goal: a minute sealed before processes carried their GPU time gives frames of processes
    // of no GPU time, as its replay gives them.
    // Method: seal a minute of GPU time, drop the GPU time from its processes, and compare the
    // decoded frames with the replay's over the same files.
    let fixture = sealed_gpu_minute();
    damage_table(&fixture, "processes", |frame| frame.drop("gpu_ns").unwrap());
    let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();
    let frames = decoded_frames(&minute).unwrap();

    assert_eq!(frames.len(), 3);
    assert_eq!(frames, replayed_frames(&fixture));
}

#[test]
fn a_minute_derives_the_apps_its_app_parts_keep() {
    // Goal: the store gives a minute without App parts the Apps the seal would have kept of it,
    // by the App rule over the minute's processes and events.
    // Method: seal three observations of several Apps with their App parts, derive the
    // minute's Apps from its processes and events, and compare them with the Apps its parts
    // keep.
    let fixture = sealed_apps_minute();
    let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();
    let mut kept = AppMinute::default();
    read_apps(&minute, &mut kept).unwrap();
    let mut derived = AppMinute::default();

    derive_apps(&minute, &mut None, &mut derived).unwrap();

    assert_eq!(derived, kept);
}

#[test]
fn a_minute_takes_its_first_rates_from_the_minute_before_it() {
    // Goal: the first observation of a minute without App parts takes its rates from the last
    // observation of the minute before it in its run, as the seal derives them.
    // Method: seal two adjacent minutes of a run with their App parts, derive the first
    // minute's Apps and then the second's from what the first left, and compare the second's
    // with the Apps its parts keep.
    let fixture = sealed_adjacent_apps_minutes();
    let first = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();
    let second = SealedMinute::open(&fixture.root, &marker(&fixture, 1)).unwrap();
    let mut kept = AppMinute::default();
    read_apps(&second, &mut kept).unwrap();
    let mut before = None;
    derive_apps(&first, &mut before, &mut AppMinute::default()).unwrap();
    let mut derived = AppMinute::default();

    derive_apps(&second, &mut before, &mut derived).unwrap();

    assert!(kept.samples[0].previous.is_some());
    assert_eq!(derived, kept);
}

#[test]
fn a_minute_derived_without_the_one_before_it_gives_its_first_sample_no_rates() {
    // Goal: a minute whose Apps are derived alone gives its first observation no rates, since
    // the observation they would come from is not at hand.
    // Method: seal two adjacent minutes of a run, and derive the second minute's Apps alone.
    let fixture = sealed_adjacent_apps_minutes();
    let second = SealedMinute::open(&fixture.root, &marker(&fixture, 1)).unwrap();
    let mut derived = AppMinute::default();

    derive_apps(&second, &mut None, &mut derived).unwrap();

    assert_eq!(derived.samples[0].previous, None);
}

#[test]
fn a_refused_minute_leaves_no_observation_to_take_rates_from() {
    // Goal: a minute whose Apps are refused leaves the next minute no observation of it to take
    // rates from, and no Apps.
    // Method: derive one minute's Apps, then those of a minute whose processes name no tick.
    let fixture = sealed_adjacent_apps_minutes();
    let second = SealedMinute::open(&fixture.root, &marker(&fixture, 1)).unwrap();
    let mut before = None;
    derive_apps(&second, &mut before, &mut AppMinute::default()).unwrap();
    damage_table(&fixture, "processes", |frame| {
        with_column(frame, (col("publication") + lit(100)).alias("publication"))
    });
    let first = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();
    let mut derived = AppMinute::default();

    let refused = derive_apps(&first, &mut before, &mut derived);

    assert!(matches!(
        refused,
        Err(Refused::Decode(persistence::Error::Bound(
            "process of no tick"
        )))
    ));
    assert!(before.is_none());
    assert_eq!(derived, AppMinute::default());
}

macro_rules! refuses_damaged_processes {
    ($($name:ident, $table:literal, $reason:literal, $damage:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: a minute whose processes or events hold what format 1 never writes is
            // refused with the reason, and never read as frames.
            // Method: seal a minute of events, replace the table with a copy the case damages,
            // and read the minute's frames.
            let fixture = sealed_events_minute();
            damage_table(&fixture, $table, $damage);
            let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();

            let refused = decoded_frames(&minute);

            let refusal = matches!(
                refused,
                Err(Refused::Decode(persistence::Error::Bound($reason)))
            );
            assert!(refusal, "{refused:?}");
        }
    )*};
}

refuses_damaged_processes! {
    refuses_a_process_of_no_tick, "processes", "process of no tick",
        |frame| with_column(frame, (col("publication") + lit(100)).alias("publication"));
    refuses_an_event_of_no_tick, "events", "process of no tick",
        |frame| with_column(frame, (col("publication") + lit(100)).alias("publication"));
    refuses_a_process_twice_in_one_observation, "processes", "duplicate committed App PID",
        |frame| with_column(frame, lit(7).cast(DataType::Int32).alias("pid"));
    refuses_an_identity_of_another_version, "processes", "process identity version",
        |frame| with_column(frame, lit(2).cast(DataType::UInt8).alias("identity_version"));
    refuses_an_event_of_another_observations_tick, "events", "committed App event tick",
        |frame| with_column(frame, (col("run_tick") + lit(1)).alias("run_tick"));
    refuses_events_of_one_number, "events", "committed App event order",
        |frame| with_column(frame, col("event").min().alias("event"));
}

#[test]
fn a_sealed_minute_gives_the_rule_each_tick_of_its_processes() {
    // Goal: the rule reads each tick of a sealed minute with its processes, their creation,
    // program, usage and threads, as the collector read them.
    // Method: seal three ticks of a refused process and a measured one, and read the minute's
    // ticks into a fresh index.
    let fixture = sealed_events_minute();
    let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();

    let ticks = minute_ticks(&mut MinuteTicks::default(), &minute).unwrap();

    assert_eq!(
        ticks.iter().map(|tick| tick.tick).collect::<Vec<_>>(),
        [0, 1, 2]
    );
    let first = &ticks[0];
    assert_eq!(first.pid, [1, 7]);
    assert_eq!(
        first.creation,
        [Some(creation(1, -1)), Some(creation(7, -7))]
    );
    assert_eq!(
        first.program,
        [
            Ok(b"/Apps/Outer\xfe.app".to_vec()),
            Ok(b"/Apps/Current.app".to_vec())
        ]
    );
    assert_eq!(
        first.measurement,
        [Measurement::Refused, Measurement::Measured]
    );
    assert_eq!(first.user_ns, [0, 9_007_199_254_740_993]);
    assert_eq!(first.threads, [None, Some(1)]);
}

#[test]
fn a_sealed_minute_gives_the_rule_each_tick_of_its_events() {
    // Goal: the rule reads each tick's starts, execs and exits, each with its process's
    // creation and its program or last usage, in the orders it reads them.
    // Method: seal a start and an exec, then two exits in reverse pid order, then a gone exit
    // and an exec, and read the minute's ticks.
    let fixture = sealed_events_minute();
    let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();
    let departed = Ok(b"/Old.app".to_vec());

    let ticks = minute_ticks(&mut MinuteTicks::default(), &minute).unwrap();

    assert_eq!(
        ticks[0].starts,
        [(9, Some(creation(900, -90)), departed.clone())]
    );
    assert_eq!(
        ticks[0].execs,
        [(9, Some(creation(900, -90)), departed.clone())]
    );
    assert_eq!(
        ticks[1].exits,
        [(8, Measurement::Refused, 0), (9, Measurement::Measured, 2)]
    );
    assert_eq!(ticks[2].exits, [(6, Measurement::Gone, 0)]);
    assert_eq!(ticks[2].execs, [(7, Some(creation(700, -70)), departed)]);
}

#[test]
fn a_minute_sealed_before_process_identity_gives_the_rule_no_programs() {
    // Goal: the processes of a minute sealed before processes carried their identity reach
    // the rule with no creation and an unsupported program.
    // Method: seal three ticks of a measured process from a ring without identity, a start
    // among their events, and read the minute's ticks.
    let fixture = sealed_events_minute_of(Fixture::new_mode(false, 2, None));
    let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();

    let ticks = minute_ticks(&mut MinuteTicks::default(), &minute).unwrap();

    assert_eq!(ticks[0].pid, [7]);
    assert_eq!(ticks[0].creation, [None]);
    assert_eq!(ticks[0].program, [Err(ProgramUnavailable::Unsupported)]);
    assert_eq!(ticks[0].measurement, [Measurement::Measured]);
    assert_eq!(
        ticks[0].starts,
        [(9, None, Err(ProgramUnavailable::Unsupported))]
    );
}

#[test]
fn a_minute_gives_its_first_tick_the_last_tick_of_the_minute_before_it() {
    // Goal: the first tick of a minute takes its rates from the last tick of the minute before
    // it in its run, and every other tick from the one before it.
    // Method: seal two adjacent minutes of a run, a second apart, read the first and then the
    // second through the same buffers, and derive each tick's Apps.
    let fixture = sealed_adjacent_apps_minutes();
    let first = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();
    let second = SealedMinute::open(&fixture.root, &marker(&fixture, 1)).unwrap();
    let mut ticks = MinuteTicks::default();
    minute_ticks(&mut ticks, &first).unwrap();

    let read = minute_ticks(&mut ticks, &second).unwrap();

    assert_eq!(
        read.iter().map(|tick| tick.tick).collect::<Vec<_>>(),
        [60, 61]
    );
    assert_eq!(read[0].elapsed_ns, Some(1_000_000_000));
    assert_eq!(read[1].elapsed_ns, Some(1_000_000_000));
}

#[test]
fn a_minute_read_without_the_one_before_it_gives_its_first_tick_no_rates() {
    // Goal: the first tick of a minute read alone takes no rates, since the tick they would
    // come from is not at hand.
    // Method: seal two adjacent minutes of a run, and read the second alone.
    let fixture = sealed_adjacent_apps_minutes();
    let second = SealedMinute::open(&fixture.root, &marker(&fixture, 1)).unwrap();

    let read = minute_ticks(&mut MinuteTicks::default(), &second).unwrap();

    assert_eq!(read[0].elapsed_ns, None);
}

#[test]
fn a_refused_minute_leaves_no_tick_to_take_rates_from() {
    // Goal: a minute whose read is refused part way leaves the next minute no tick of it to
    // take rates from.
    // Method: refuse the first of two adjacent minutes at its last tick, then read the second.
    let fixture = sealed_adjacent_apps_minutes();
    let first = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();
    let second = SealedMinute::open(&fixture.root, &marker(&fixture, 1)).unwrap();
    let mut ticks = MinuteTicks::default();
    let mut programs = minute_programs();
    let refused = ticks.read(&first, &mut programs, |tick| {
        if tick.selected.stamp.tick == 59 {
            return Err(persistence::Error::Bound("test refusal"));
        }
        Ok(())
    });
    assert!(matches!(
        refused,
        Err(Refused::Decode(persistence::Error::Bound("test refusal")))
    ));

    let read = minute_ticks(&mut ticks, &second).unwrap();

    assert_eq!(read[0].elapsed_ns, None);
}

macro_rules! refuses_damaged_ticks {
    ($($name:ident, $table:literal, $reason:literal, $damage:expr;)*) => {$(
        #[test]
        fn $name() {
            // Goal: a minute whose processes or events hold what format 1 never writes is
            // refused with the reason, and never reaches the rule.
            // Method: seal a minute of events, replace the table with a copy the case damages,
            // and read the minute's ticks.
            let fixture = sealed_events_minute();
            damage_table(&fixture, $table, $damage);
            let minute = SealedMinute::open(&fixture.root, &marker(&fixture, 0)).unwrap();

            let refused = minute_ticks(&mut MinuteTicks::default(), &minute);

            let refusal = matches!(
                refused,
                Err(Refused::Decode(persistence::Error::Bound($reason)))
            );
            assert!(refusal, "{refused:?}");
        }
    )*};
}

refuses_damaged_ticks! {
    refuses_a_tick_of_a_process_of_no_tick, "processes", "process of no tick",
        |frame| with_column(frame, (col("publication") + lit(100)).alias("publication"));
    refuses_a_tick_of_a_process_twice, "processes", "sealed tick pid order",
        |frame| with_column(frame, lit(7).cast(DataType::Int32).alias("pid"));
    refuses_a_tick_of_an_event_of_another_tick, "events", "sealed tick event tick",
        |frame| with_column(frame, (col("run_tick") + lit(1)).alias("run_tick"));
    refuses_a_tick_of_events_of_one_number, "events", "sealed tick event order",
        |frame| with_column(frame, col("event").min().alias("event"));
    refuses_a_tick_of_an_event_of_no_kind, "events", "sealed tick event kind",
        |frame| with_column(frame, lit(9).cast(DataType::UInt8).alias("kind"));
}

/// What the rule reads of a tick of a sealed minute, kept past the read, each program as its
/// App's key.
#[derive(Debug)]
struct ReadTick {
    tick: u64,
    pid: Vec<i32>,
    creation: Vec<Option<Creation>>,
    program: Vec<ReadProgram>,
    measurement: Vec<Measurement>,
    user_ns: Vec<u64>,
    threads: Vec<Option<u64>>,
    /// Each exit's pid, measurement and user time.
    exits: Vec<(i32, Measurement, u64)>,
    /// Each start's pid, creation and program.
    starts: Vec<(i32, Option<Creation>, ReadProgram)>,
    execs: Vec<(i32, Option<Creation>, ReadProgram)>,
    /// The time the rule takes the tick's rates over.
    elapsed_ns: Option<u64>,
}

/// A program as its App's key, or why the collector did not read it.
type ReadProgram = Result<Vec<u8>, ProgramUnavailable>;

/// Each tick `ticks` reads of `minute` into a fresh index.
fn minute_ticks(ticks: &mut MinuteTicks, minute: &SealedMinute) -> Result<Vec<ReadTick>, Refused> {
    let mut programs = minute_programs();
    let mut read = Vec::new();
    ticks.read(minute, &mut programs, |tick| {
        let mut apps = Apps::new(16, tick.programs);
        apps.derive(tick.programs, &tick.previous, &tick.selected);
        let selected = &tick.selected;
        let key = |program: &Result<u32, ProgramUnavailable>| {
            program.map(|entry| {
                let app = tick.programs.app(entry);
                match tick.programs.key(app) {
                    AppKey::Bundle(bytes) | AppKey::Executable(bytes) => bytes.to_vec(),
                }
            })
        };
        let events = |pid: &[i32], creation: &[Option<Creation>], program| {
            let program: &[Result<u32, ProgramUnavailable>] = program;
            (0..pid.len())
                .map(|event| (pid[event], creation[event], key(&program[event])))
                .collect()
        };
        read.push(ReadTick {
            tick: selected.stamp.tick,
            pid: selected.processes.pid.to_vec(),
            creation: selected.processes.creation.to_vec(),
            program: selected.processes.program.iter().map(key).collect(),
            measurement: selected.processes.usage.measurement.to_vec(),
            user_ns: selected.processes.usage.user_ns.to_vec(),
            threads: selected.processes.threads.to_vec(),
            exits: (0..selected.exits.pid.len())
                .map(|exit| {
                    let usage = &selected.exits.usage;
                    (
                        selected.exits.pid[exit],
                        usage.measurement[exit],
                        usage.user_ns[exit],
                    )
                })
                .collect(),
            starts: events(
                selected.starts.pid,
                selected.starts.creation,
                selected.starts.program,
            ),
            execs: events(
                selected.execs.pid,
                selected.execs.creation,
                selected.execs.program,
            ),
            elapsed_ns: apps.elapsed_ns(),
        });
        Ok(())
    })?;
    Ok(read)
}

fn minute_programs() -> Programs {
    Programs::new(Capacity {
        entries: 16,
        program_bytes: 4096,
    })
}

fn creation(unique_id: u64, pid_version: i32) -> Creation {
    Creation {
        unique_id,
        pid_version,
    }
}

/// A fixture whose ring held three observations whose events start, exec and exit processes,
/// sealed as minute 0.
fn sealed_events_minute() -> Fixture {
    sealed_events_minute_of(Fixture::new())
}

/// `fixture` with the observations `sealed_events_minute` seals, sealed as minute 0.
fn sealed_events_minute_of(mut fixture: Fixture) -> Fixture {
    let last = Usage {
        awake_ns_at_start: 1,
        user_ns: 2,
        system_ns: 3,
        energy_nj: 4,
        instructions: 5,
        super_instructions: 6,
        cycles: 7,
        idle_wakeups: 8,
        interrupt_wakeups: 9,
        pageins: 10,
        resident_bytes: 11,
        footprint_bytes: 12,
        disk_read_bytes: 13,
        disk_written_bytes: 14,
    };
    let started = [
        Event::Start {
            pid: 9,
            coalition: Some(3),
        },
        Event::Exec { pid: 9 },
    ];
    let exited = [
        Event::Exit {
            pid: 9,
            last: Ok(last),
            noticed: Noticed::OnExit,
        },
        Event::Exit {
            pid: 8,
            last: Err(Unreadable::Refused),
            noticed: Noticed::AlreadyExited,
        },
    ];
    let gone = [
        Event::Exit {
            pid: 6,
            last: Err(Unreadable::Gone),
            noticed: Noticed::OnExit,
        },
        Event::Exec { pid: 7 },
    ];
    fixture.publish_with_events(1000, &started);
    fixture.publish_with_events(1001, &exited);
    fixture.publish_with_events(1002, &gone);
    fixture.seal_minute(0);
    fixture
}

/// A fixture whose system ring held three observations of a process with GPU time and one
/// without, sealed as minute 0.
fn sealed_gpu_minute() -> Fixture {
    let mut fixture = Fixture::new();
    system_ring(&mut fixture);
    for step in 1..=3 {
        publish_gpu_time(&mut fixture, step);
    }
    fixture.seal_minute(0);
    fixture
}

/// Publishes the `step`th tick of `fixture`'s system ring, of process 1 without GPU time and
/// process 7 with it.
fn publish_gpu_time(fixture: &mut Fixture, step: u64) {
    let mut program = None;
    fixture
        .writer
        .intern_program(
            &ProgramBytes {
                executable_name: b"render",
                app_path: Some(b"/Apps/Render.app"),
            },
            &mut program,
        )
        .unwrap();
    let (processes, identities) = gpu_processes(step, program.unwrap());
    let (cpus, memory, interfaces) = machine(step);
    let readings = Readings {
        run_tick: step - 1,
        cpus: &cpus,
        memory: &memory,
        interfaces: &interfaces,
        ioreport: None,
        smc: None,
        processes: &processes,
        processes_refused: 0,
        processes_gone: 0,
        coalitions: &[],
        events: &[],
        dropped: Dropped::default(),
    };
    let times = ObservationTimes {
        wall_ns: (1_000 + step) * 1_000_000_000,
        continuous_ns: step * 1_000_000_000,
        awake_ns: step * 900_000_000,
    };
    let system = SystemPublication {
        reading: &SystemReading::default(),
        machine: &MachineFacts::default(),
        clock: None,
        process_gpu_ns: &[None, Some(step * 500)],
    };
    let supplemental = SupplementalReadings {
        battery: None,
        disks: None,
        system: Some(&system),
    };
    fixture
        .writer
        .publish_with_disks(
            &readings,
            Some(&times),
            &IdentityPublication {
                processes: &identities,
                events: &[],
            },
            &supplemental,
        )
        .unwrap();
}

/// The processes 1 and 7 of the `step`th tick of a GPU minute, both running `program`.
fn gpu_processes(step: u64, program: ProgramReference) -> ([Process; 2], [IdentityObservation; 2]) {
    let processes = [1, 7].map(|pid| Process {
        pid,
        usage: Usage {
            awake_ns_at_start: 0,
            user_ns: step * 1_000,
            system_ns: step,
            energy_nj: 4,
            instructions: 5,
            super_instructions: 6,
            cycles: 7,
            idle_wakeups: 8,
            interrupt_wakeups: 9,
            pageins: 10,
            resident_bytes: 11,
            footprint_bytes: 12,
            disk_read_bytes: 13,
            disk_written_bytes: 14,
        },
        task: Task {
            threads: pid,
            faults: 2,
            context_switches: 3,
            mach_syscalls: 4,
            unix_syscalls: 5,
        },
    });
    let identities = [1, 7].map(|pid| IdentityObservation {
        pid,
        usage: UsageAvailability::Measured,
        identity: ProcessIdentity::Measured {
            creation: ProcessCreation {
                unique_id: u64::try_from(pid).unwrap(),
                pid_version: -pid,
            },
            program,
        },
    });
    (processes, identities)
}

/// The frames the decoder reads of `minute`, in publication order.
fn decoded_frames(minute: &SealedMinute) -> Result<Vec<Frame>, Refused> {
    let mut frames = Vec::new();
    read_processes(minute, |_, frame| {
        frames.push(*frame);
        Ok(())
    })?;
    Ok(frames)
}

/// The frames the SQL replay of `fixture`'s sealed minute 0 builds from the minute's files.
fn replayed_frames(fixture: &Fixture) -> Vec<Frame> {
    let manifest = persistence::read_committed(&fixture.root, &marker(fixture, 0)).unwrap();
    let directory = fixture.root.join(&manifest.data_directory);
    let connection = Engine::new();
    let source = |table: Table| {
        let path = directory.join(format!("{}.parquet", table.name()));
        table_source(&connection, table, TableSource::Parquet(&path)).unwrap()
    };
    let ticks = connection
        .query(&format!(
            "SELECT publication, run_tick, wall_ns, continuous_ns, awake_ns, processes_refused, \
            processes_gone, events_first, events_end FROM {} ORDER BY publication",
            source(Table::Ticks)
        ))
        .unwrap();
    let run = manifest.identity.run;
    let endpoints: Vec<_> = (0..ticks.height())
        .map(|index| {
            let row = Row::new(&ticks, index).unwrap();
            Endpoint {
                identity: run,
                stamp: crate::apps::Stamp {
                    boot: run.boot.0,
                    run: run.run.0,
                    publication: row.get(0).unwrap(),
                    continuous_ns: row.get(3).unwrap(),
                    awake_ns: row.get(4).unwrap(),
                },
                run_tick: row.get(1).unwrap(),
                wall_ns: row.get(2).unwrap(),
                usage_refused: row.get(5).unwrap(),
                usage_gone: row.get(6).unwrap(),
                events_first: row.get(7).unwrap(),
                events_end: row.get(8).unwrap(),
            }
        })
        .collect();
    let processes = source(Table::Processes);
    let events = source(Table::Events);
    let replay = crate::live_apps::committed::Sources {
        processes: &processes,
        events: &events,
    }
    .query();
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut frames = Vec::new();

    Frame::committed_many(&endpoints, &connection, &replay, &operation, |frame| {
        frames.push(*frame);
        Ok(())
    })
    .unwrap();

    frames
}

/// A fixture whose ring held three observations of several Apps, sealed as minute 0 with
/// their App parts.
fn sealed_apps_minute() -> Fixture {
    let mut fixture = Fixture::new();
    for second in 1000..=1002 {
        fixture.publish_with_system_rows(second);
    }
    fixture.seal_minute_derived(0, &crate::SealedApps::default());
    fixture
}

/// A fixture whose ring held the last two observations of run minute 0 and the first two of
/// minute 1, each minute sealed with its App parts.
fn sealed_adjacent_apps_minutes() -> Fixture {
    let mut fixture = Fixture::new();
    let derive = crate::SealedApps::default();
    fixture.start_at_run_tick(58);
    for second in 1058..=1059 {
        fixture.publish_with_system_rows(second);
    }
    fixture.seal_minute_derived(0, &derive);
    for second in 1060..=1061 {
        fixture.publish_with_system_rows(second);
    }
    fixture.seal_minute_derived(1, &derive);
    fixture
}

/// Column `name` holding `value` at the rows of kind `row_kind` and what it held at the
/// others.
fn of_rows(row_kind: u8, value: Expr, name: &str) -> Expr {
    when(col("row_kind").eq(lit(row_kind)))
        .then(value)
        .otherwise(col(name))
        .alias(name)
}

/// A fixture whose ring held the ticks `publish_supplemented` publishes, sealed as minute 0.
fn sealed_system_minute() -> Fixture {
    let mut fixture = Fixture::new_mode(false, 2, None);
    system_ring(&mut fixture);
    for step in 1..=3 {
        publish_supplemented(&mut fixture, step);
    }
    fixture.seal_minute(0);
    fixture
}

/// Replaces table `name` of `fixture`'s sealed minute 0 with the copy `damage` makes of it.
fn damage_table(fixture: &Fixture, name: &str, damage: fn(DataFrame) -> DataFrame) {
    let manifest = persistence::read_committed(&fixture.root, &marker(fixture, 0)).unwrap();
    let path = fixture
        .root
        .join(manifest.data_directory)
        .join(format!("{name}.parquet"));
    let frame = persistence::engine::scan(&path).unwrap().collect().unwrap();
    let mut damaged = damage(frame);

    std::fs::remove_file(&path).unwrap();
    let file = collector::recording::private::open_private_file(&path, true).unwrap();
    ParquetWriter::new(file)
        .with_compression(ParquetCompression::Zstd(Some(
            ZstdLevel::try_new(3).unwrap(),
        )))
        .with_statistics(StatisticsOptions {
            min_value: true,
            max_value: true,
            distinct_count: false,
            null_count: true,
            binary_statistics_truncate_length: None,
        })
        .with_row_group_size(Some(16_384))
        .with_data_page_size(None)
        .set_parallel(false)
        .with_key_value_metadata(None)
        .with_context_info(None)
        .finish(&mut damaged)
        .unwrap();
}

/// `frame` with the column `column` names replaced by `column`.
fn with_column(frame: DataFrame, column: Expr) -> DataFrame {
    frame.lazy().with_column(column).collect().unwrap()
}

/// A column named `name` of type `data_type` holding no values.
fn null(name: &str, data_type: DataType) -> Expr {
    lit(Null {}).cast(data_type).alias(name)
}

/// Moves `fixture` onto a ring of its run, as `wide_ring` makes it, that also carries system
/// readings, battery readings and disks.
fn system_ring(fixture: &mut Fixture) {
    wide_ring(fixture, |place, shape, identity| {
        let capacity = DictionaryCapacity {
            entries: 4,
            arena_bytes: 32_768,
        };
        Writer::create_with_system(place, shape, capacity, identity, 0)
    });
}

/// Moves `fixture` onto a ring of its run and shape, with room for two of each CPU, interface,
/// core, state and fan, that `create` makes.
fn wide_ring(
    fixture: &mut Fixture,
    create: fn(&Place, Shape, &RunIdentity) -> std::io::Result<Writer>,
) {
    let identity = fixture.writer.identity().unwrap();
    let shape = RingReader::open_generation(fixture.writer.name())
        .unwrap()
        .shape();
    let shape = Shape {
        cpu_capacity: 2,
        interface_capacity: 2,
        core_capacity: 2,
        state_capacity: 2,
        fan_capacity: 2,
        ..shape
    };
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let place = Place {
        lock_path: fixture.root.join("wide.lock"),
        name_prefix: format!("/fdrw{}x{serial}", std::process::id()),
    };
    let writer = create(&place, shape, &identity).unwrap();

    collector::ring::unlink_generation(fixture.writer.name()).unwrap();
    fixture.writer = writer;
    Registry {
        runs: vec![RegisteredRun {
            identity,
            ring_name: fixture.writer.name().to_owned(),
            collector_pid: 0,
        }],
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
}

/// Publishes the `step`th tick as `publish_machine` does, with system readings and a battery
/// reading at steps 1 and 3, performance levels at step 1 alone, and disks at steps 1 and 2.
fn publish_supplemented(fixture: &mut Fixture, step: u64) {
    let reading = SystemReading {
        thermal_pressure: Some(u8::try_from(step).unwrap()),
        ..SystemReading::default()
    };
    let machine = MachineFacts {
        levels: (step == 1).then(levels),
        ..MachineFacts::default()
    };
    let khz = 2_000_000 + u32::try_from(step).unwrap();
    let clock = ClockTables::from_tables(vec![vec![khz, khz + 1_000]]);
    let system = SystemPublication {
        reading: &reading,
        machine: &machine,
        clock: Some(&clock),
        process_gpu_ns: &[],
    };
    let battery = battery(step);
    let disks = disks(step);
    let supplemental = SupplementalReadings {
        battery: (step != 2).then_some(&battery),
        disks: (step != 3).then_some(&disks),
        system: (step != 2).then_some(&system),
    };
    publish_machine(fixture, step, Some(&supplemental));
}

/// The two performance levels of an M5 Pro.
fn levels() -> [PerformanceLevel; 2] {
    let level = |name: &str, logical_cpus| PerformanceLevel {
        name: FactText::new(name.as_bytes()).unwrap(),
        logical_cpus,
    };
    [level("Super", 6), level("Performance", 12)]
}

/// The battery reading of the `step`th tick, taken just before it, each value distinct from
/// every other tick's.
fn battery(step: u64) -> BatteryReading {
    BatteryReading {
        source_continuous_ns: step * 1_000_000_000 - 500,
        charge_current: Some(100 * step),
        charge_maximum: Some(100 * step + 1),
        full_charge_capacity: Some(100 * step + 2),
        design_capacity: Some(100 * step + 3),
        cycle_count: Some(100 * step + 4),
        charging: Some(step == 1),
        external_power: Some(step != 1),
        voltage_mv: Some(100 * step + 5),
        current_ma: Some(-i64::try_from(step).unwrap()),
        adapter_watts: None,
    }
}

/// The disks of the `step`th tick, scanned just before it, each counter distinct from every
/// other tick's.
fn disks(step: u64) -> DiskSnapshot {
    let mut snapshot = DiskSnapshot {
        source_continuous_ns: step * 1_000_000_000 - 500,
        state: ScanState::Complete,
        count: 1,
        ..DiskSnapshot::default()
    };
    snapshot.records[0] = DiskReading {
        driver_id: 5,
        media_id: 7,
        read_bytes: Some(1_000 * step),
        write_bytes: Some(1_000 * step + 1),
        ..DiskReading::default()
    };
    snapshot.records[0].name[..5].copy_from_slice(b"disk0");
    snapshot
}

/// Publishes the `step`th tick of run minute 0, holding `machine(step)`, with no `IOReport`
/// readings at step 2 and no SMC readings at step 3, and with `supplemental` where the ring
/// carries it.
fn publish_machine(fixture: &mut Fixture, step: u64, supplemental: Option<&SupplementalReadings>) {
    let (cpus, memory, interfaces) = machine(step);
    let sources = sources(step);
    let readings = Readings {
        run_tick: step - 1,
        cpus: &cpus,
        memory: &memory,
        interfaces: &interfaces,
        ioreport: (step != 2).then_some(IOReportReadings {
            gpu_energy_nj: sources.gpu_energy_nj,
            gpu_active_ns: &sources.gpu_active_ns,
            cores: &sources.cores,
        }),
        smc: (step != 3).then_some(SmcReadings {
            power_watts: sources.power_watts,
            soc_celsius: sources.soc_celsius,
            cpu_celsius: Some(sources.cpu_celsius),
            gpu_celsius: None,
            fan_rpm: &sources.fan_rpm,
        }),
        processes: &[],
        processes_refused: 0,
        processes_gone: 0,
        coalitions: &[],
        events: &[],
        dropped: Dropped::default(),
    };
    let times = ObservationTimes {
        wall_ns: (1_000 + step) * 1_000_000_000,
        continuous_ns: step * 1_000_000_000,
        awake_ns: step * 900_000_000,
    };
    let Some(supplemental) = supplemental else {
        fixture.writer.publish_observed(&readings, &times);
        return;
    };

    let identities = IdentityPublication {
        processes: &[],
        events: &[],
    };
    fixture
        .writer
        .publish_with_disks(&readings, Some(&times), &identities, supplemental)
        .unwrap();
}

/// Every tick the ring holds, oldest first, each with its publication.
fn ring_ticks(fixture: &Fixture) -> Vec<(u64, Tick)> {
    let mut reader = RingReader::open_generation(fixture.writer.name()).unwrap();
    let shape = reader.shape();
    let mut ticks = Vec::with_capacity(8);
    for _index in 0..8 {
        let mut tick = persistence::tick_buffer(&shape).unwrap();
        match reader.read_into(&mut tick).unwrap() {
            Found::Nothing => break,
            Found::Missed { .. } => panic!("fixture source overwritten"),
            Found::Publication { publication, .. } => ticks.push((publication, tick)),
        }
    }
    ticks
}

/// The marker of the sealed window of run minute `minute`.
fn marker(fixture: &Fixture, minute: u64) -> std::path::PathBuf {
    let store = persistence::Store::open(&fixture.root).unwrap();
    store.marker_path(&persistence::WindowIdentity {
        run: fixture.writer.identity().unwrap(),
        window: minute,
    })
}

/// The machine readings of the `step`th tick, of two CPUs and two interfaces, each counter
/// distinct from every other.
fn machine(step: u64) -> ([CPUTicks; 2], Memory, [Interface; 2]) {
    let small = u32::try_from(step).unwrap();
    let cpu = |first: u32| CPUTicks {
        user: 10 * small + first,
        system: 10 * small + first + 1,
        nice: 10 * small + first + 2,
        idle: 10 * small + first + 3,
    };
    let cpus = [cpu(0), cpu(5)];
    let memory = Memory {
        total_bytes: 1_000 * step,
        free_bytes: 1_000 * step + 1,
        active_bytes: 1_000 * step + 2,
        inactive_bytes: 1_000 * step + 3,
        speculative_bytes: 1_000 * step + 4,
        wired_bytes: 1_000 * step + 5,
        compressed_bytes: 1_000 * step + 6,
        anonymous_bytes: 1_000 * step + 7,
        file_backed_bytes: 1_000 * step + 8,
        purgeable_bytes: 1_000 * step + 9,
        swap_total_bytes: 1_000 * step + 10,
        swap_used_bytes: 1_000 * step + 11,
        swapped_in_bytes: 1_000 * step + 12,
        swapped_out_bytes: 1_000 * step + 13,
        pageins: 1_000 * step + 14,
        pageouts: 1_000 * step + 15,
        compressions: 1_000 * step + 16,
        decompressions: 1_000 * step + 17,
        pressure: [Pressure::Normal, Pressure::Warning, Pressure::Critical]
            [usize::try_from(step % 3).unwrap()],
    };
    let interface = |index: u16, text: &[u8], first: u64| {
        let mut name = [0; IFNAMSIZ];
        name[..text.len()].copy_from_slice(text);
        Interface {
            index,
            name: Name::from_bytes(name, text.len()).unwrap(),
            received_bytes: 100 * step + first,
            sent_bytes: 100 * step + first + 1,
            received_packets: 100 * step + first + 2,
            sent_packets: 100 * step + first + 3,
            receive_errors: 100 * step + first + 4,
            send_errors: 100 * step + first + 5,
            receive_drops: 100 * step + first + 6,
        }
    };
    let interfaces = [interface(4, b"utun", 0), interface(7, b"en0", 10)];
    (cpus, memory, interfaces)
}

/// The `IOReport` and SMC readings of the `step`th tick, of two GPU states, two cores of two
/// states and two fans, each distinct from every other.
struct Sources {
    gpu_energy_nj: u64,
    gpu_active_ns: [u64; 2],
    cores: [States; 2],
    power_watts: f32,
    soc_celsius: f32,
    cpu_celsius: f32,
    fan_rpm: [f32; 2],
}

fn sources(step: u64) -> Sources {
    let small = f32::from(u8::try_from(step).unwrap());
    Sources {
        gpu_energy_nj: 10_000 * step,
        gpu_active_ns: [10_000 * step + 1, 10_000 * step + 2],
        cores: [
            States::with_active_ns(&[10_000 * step + 3, 10_000 * step + 4]),
            States::with_active_ns(&[10_000 * step + 5, 10_000 * step + 6]),
        ],
        power_watts: 10.0 + small,
        soc_celsius: 40.0 + small,
        cpu_celsius: 50.0 + small,
        fan_rpm: [1_000.0 + small, 2_000.0 + small],
    }
}
