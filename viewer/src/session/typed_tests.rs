//! A saved recording's computation formats no cell on any thread. Only a raw page shows cells.

use persistence::Error;
use persistence::engine::AdmissionRefusal;
use polars::prelude::{Column, DataFrame, DataType, IdxCa, IdxSize, IntoLazy, col, lit, when};

use super::retention_tests::saved_seconds;
use super::{ContextRange, Session, minute_refusal};
use crate::LiveSession;
use crate::live::identity_fixture::Fixture;
use crate::live_raw::Cell;
use crate::metrics::{Minutes, READER_MINUTES_BYTES_MAX, minutes_bytes_check, trends_minute};
use crate::recording::{
    FanRow, MACHINE_TEXT_BYTES_MAX, MachineMinute, Refused, TABLE_ROWS_PER_TICK_MAX,
};

const SECOND: u64 = 1_005;
const LAST_SECOND: u64 = 1_008;

#[test]
fn a_helper_threads_work_counts_in_the_scope_that_started_it() {
    // Goal: a scope's zero means no helper thread did that work either.
    // Method: read a part on a helper thread that does work of every kind.
    let fixture = saved_seconds();
    let session = Session::open(&fixture.root).unwrap();
    let helper = persistence::work::Counts {
        staged_ticks: 1,
        frames: 2,
        sql_plans: 3,
        collections: 4,
        string_cells: 5,
        source_metadata_checks: 6,
        saved_tables: 7,
        minute_bytes: 8,
        minute_copy_bytes: 9,
    };
    let measured = persistence::work::observe();

    session
        .context_readers("test", vec![()], |_reader, ()| {
            persistence::work::merge(helper);
            Ok(())
        })
        .unwrap();

    assert_eq!(measured.counts(), helper);
}

#[test]
fn a_saved_selection_formats_no_cell() {
    // Goal: selecting a saved second computes its summary from typed counters.
    // Method: count the cells a production selection formats.
    let fixture = saved_seconds();
    let mut session = Session::open(&fixture.root).unwrap();
    let measured = persistence::work::observe();

    session.select(SECOND, 0).unwrap();

    assert_eq!(measured.counts().string_cells, 0);
}

#[test]
fn a_raw_page_formats_only_its_visible_rows() {
    // Goal: a raw page stays the presentation boundary, and its cells are counted apart.
    // Method: count the cells of one page after a selection, against what the page shows.
    let fixture = saved_seconds();
    let mut session = Session::open(&fixture.root).unwrap();
    session.select(SECOND, 0).unwrap();

    typed_damage(&fixture, "cpus", |frame| typed_repeated(&frame, 8 * 1024));
    let measured = persistence::work::observe();

    let page = session.page("cpus", 0).unwrap();

    let columns = page["columns"].as_array().unwrap().len();
    let rows = page["rows"].as_array().unwrap().len();
    assert!(page["total"].as_u64().unwrap() > u64::try_from(rows).unwrap());
    assert_eq!(
        measured.counts().string_cells,
        u64::try_from(columns * rows).unwrap()
    );
}

#[test]
fn a_saved_context_formats_no_cell() {
    // Goal: a saved context computes every point from typed counters, helper threads included.
    // Method: count the cells a production context read formats.
    let fixture = saved_seconds();
    let session = Session::open(&fixture.root).unwrap();
    let range = ContextRange::charted_ending_at(LAST_SECOND).unwrap();
    let mut points = Vec::new();
    let measured = persistence::work::observe();

    session.context(range, &mut points).unwrap();

    assert_eq!(points.len(), 8);
    assert_eq!(measured.counts().string_cells, 0);
}

#[test]
fn the_older_tail_formats_no_cell() {
    // Goal: the tail of older minutes computes every point from typed counters.
    // Method: count the cells of an older context whose detail lies past the saved seconds, so
    // every reading it shows comes from the tail, which keeps a reading every ten seconds.
    let mut fixture = Fixture::new();
    for second in 1000..=1020 {
        fixture.publish_with_system_rows(second);
    }
    fixture.seal();
    let mut live = LiveSession::open(&fixture.root).unwrap();
    let measured = persistence::work::observe();

    let earlier = live
        .earlier_context(1020 + crate::session::earlier::CHARTED_SECONDS)
        .unwrap();

    assert_eq!(measured.counts().string_cells, 0);
    assert!(!earlier["earlier_trends"].as_array().unwrap().is_empty());
}

#[test]
fn a_restored_selection_formats_no_cell() {
    // Goal: restoring a chosen reading checks it against typed clocks.
    // Method: count the cells of a restore in a fresh session.
    let fixture = saved_seconds();
    let mut session = Session::open(&fixture.root).unwrap();
    let selected = session.select(SECOND, 0).unwrap()["selected"].clone();
    let mut restored = Session::open(&fixture.root).unwrap();
    let measured = persistence::work::observe();

    restored.restore_selection(&selected).unwrap();

    assert_eq!(measured.counts().string_cells, 0);
    assert_eq!(
        restored.selected.unwrap().publication,
        selected["publication"]
    );
}

#[test]
fn a_restored_context_formats_no_cell() {
    // Goal: a session that follows a recording it reopened computes its points without text.
    // Method: open a live session over saved minutes alone and follow its newest reading.
    let fixture = saved_seconds();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let measured = persistence::work::observe();

    let response = session.follow_latest().unwrap();

    assert_eq!(measured.counts().string_cells, 0);
    assert!(!response["selected"].is_null());
}

#[test]
fn a_reading_of_every_gauge_formats_no_cell() {
    // Goal: a gauge reaches its reading as a number.
    // Method: count the cells of a selection on a minute holding system and battery readings.
    let fixture = crate::recording::sealed_system_minute();
    let mut session = Session::open(&fixture.root).unwrap();
    let measured = persistence::work::observe();

    let response = session.select(1001, 0).unwrap();

    assert_eq!(measured.counts().string_cells, 0);
    assert!(!response["summary"]["battery"]["power_watts"].is_null());
}

#[test]
fn a_context_decodes_each_saved_table_of_its_minute_once() {
    // Goal: a doubled decode, the cost this route cut, shows in the counts.
    // Method: read a context of one minute, which decodes its clocks and then the whole minute.
    let fixture = saved_seconds();
    let session = Session::open(&fixture.root).unwrap();
    let range = ContextRange::charted_ending_at(LAST_SECOND).unwrap();
    let mut points = Vec::new();
    let measured = persistence::work::observe();

    session.context(range, &mut points).unwrap();

    assert_eq!(measured.counts().saved_tables, 8);
}

#[test]
fn a_context_stops_at_its_last_second() {
    // Goal: a context reads no reading past the second it ends at.
    // Method: read a context that ends before the minute's last readings.
    let fixture = saved_seconds();
    let session = Session::open(&fixture.root).unwrap();
    let range = ContextRange::charted_ending_at(SECOND).unwrap();
    let mut points = Vec::new();

    session.context(range, &mut points).unwrap();

    let last = points.last().unwrap()["wall_ns"].as_u64().unwrap();
    assert_eq!(last / super::NS, SECOND);
}

#[test]
fn a_context_counts_the_bytes_of_the_minute_it_owns() {
    // Goal: a context's counts name the rows it owns, and it copies none out of its minute.
    // Method: compare the bytes a context counts with the size of a decoded minute.
    let fixture = saved_seconds();
    let session = Session::open(&fixture.root).unwrap();
    let minute = session.reader().machine_minute(0).unwrap();
    let range = ContextRange::charted_ending_at(LAST_SECOND).unwrap();
    let mut points = Vec::new();
    let measured = persistence::work::observe();

    session.context(range, &mut points).unwrap();

    let counts = measured.counts();
    assert!(counts.minute_bytes >= collector::u64_from_usize(minute.heap_bytes()));
    assert_eq!(counts.minute_copy_bytes, 0);
}

#[test]
fn the_older_tail_counts_what_it_copies_out_of_each_minute() {
    // Goal: the copy of a minute's last reading, which outlives the minute, shows in the counts.
    // Method: read the older tail over saved minutes and read its copied bytes.
    let mut fixture = Fixture::new();
    for second in 1000..=1020 {
        fixture.publish_with_system_rows(second);
    }
    fixture.seal();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let measured = persistence::work::observe();

    session
        .earlier_context(1020 + crate::session::earlier::CHARTED_SECONDS)
        .unwrap();

    assert!(measured.counts().minute_copy_bytes > 0);
}

#[test]
fn cpu_detail_formats_no_cell() {
    // Goal: the CPU detail of a saved second computes from typed tick counters.
    // Method: count the cells of a detail read after a selection.
    let fixture = saved_seconds();
    let mut session = Session::open(&fixture.root).unwrap();
    session.select(SECOND, 0).unwrap();
    let measured = persistence::work::observe();

    session.cpu_detail().unwrap();

    assert_eq!(measured.counts().string_cells, 0);
}

#[test]
fn saved_cpu_history_formats_no_cell() {
    // Goal: saved CPU history computes from typed tick counters.
    // Method: count the cells of a history read after a selection.
    let fixture = saved_seconds();
    let mut session = Session::open(&fixture.root).unwrap();
    session.select(LAST_SECOND, 0).unwrap();
    let measured = persistence::work::observe();

    session.cpu_history(0).unwrap();

    assert_eq!(measured.counts().string_cells, 0);
}

#[test]
fn a_saved_context_plans_no_query() {
    // Goal: a saved context reads typed rows rather than running a query per window.
    // Method: count the query plans of a production context read.
    let fixture = saved_seconds();
    let session = Session::open(&fixture.root).unwrap();
    let range = ContextRange::charted_ending_at(LAST_SECOND).unwrap();
    let mut points = Vec::new();
    let measured = persistence::work::observe();

    session.context(range, &mut points).unwrap();

    assert_eq!(measured.counts().sql_plans, 0);
}

#[test]
fn a_saved_discharge_reads_below_zero() {
    // Goal: a signed counter keeps its sign from the file to the reading it computes.
    // Method: save a battery current of -1 mA at 105 mV and read the power of its selection.
    let fixture = crate::recording::sealed_system_minute();
    let mut session = Session::open(&fixture.root).unwrap();

    let response = session.select(1001, 0).unwrap();

    assert_eq!(response["summary"]["battery"]["power_watts"], -0.000_105);
}

fn typed_two_runs() -> Fixture {
    let mut first = Fixture::new();
    for second in 1000..=1003 {
        first.publish_with_system_rows(second);
    }
    first.seal();
    let mut next = Fixture::new_mode(false, 2, Some(&first));
    for second in 1002..=1005 {
        next.publish_with_system_rows(second);
    }
    next.seal();
    first
}

/// The two readings of `second`, the later run's first reading first. Alternatives list in the
/// order of their random run identities, so the order is taken from what each one read.
fn typed_two_runs_readings(session: &mut Session, second: u64) -> [serde_json::Value; 2] {
    let mut readings = [
        session.select(second, 0).unwrap(),
        session.select(second, 1).unwrap(),
    ];
    readings.sort_by_key(|reading| reading["summary"]["continuous_ns"].as_u64());
    readings
}

#[test]
fn runs_sharing_a_second_read_their_own_clocks() {
    // Goal: two runs on one wall second stay two readings, each read from its own run.
    // Method: save a run and a later run that starts on the earlier run's third second, then
    // select both readings of that second.
    let fixture = typed_two_runs();
    let mut session = Session::open(&fixture.root).unwrap();

    let [later, earlier] = typed_two_runs_readings(&mut session, 1002);

    assert_ne!(later["selected"]["run"], earlier["selected"]["run"]);
    assert_eq!(later["summary"]["run"], later["selected"]["run"]);
    assert_eq!(earlier["summary"]["run"], earlier["selected"]["run"]);
    assert_eq!(later["summary"]["continuous_ns"], 0);
    assert_eq!(earlier["summary"]["continuous_ns"], 2_000_000_000_u64);
}

#[test]
fn a_rate_does_not_reach_across_runs_sharing_a_second() {
    // Goal: the first reading of a run has no rate, though another run read the second before it.
    // Method: select both readings of the second the later run starts on.
    let fixture = typed_two_runs();
    let mut session = Session::open(&fixture.root).unwrap();

    let [later, earlier] = typed_two_runs_readings(&mut session, 1002);

    assert_eq!(later["summary"]["cpu_percent"], serde_json::Value::Null);
    assert_eq!(earlier["summary"]["cpu_percent"], 50.0);
}

#[test]
fn a_context_keeps_both_runs_of_a_shared_second_apart() {
    // Goal: a context of two overlapping runs takes no rate across them, so each run's first
    // reading has none.
    // Method: read a context over both runs and count the points with no rate.
    let fixture = typed_two_runs();
    let session = Session::open(&fixture.root).unwrap();
    let range = ContextRange::charted_ending_at(1005).unwrap();
    let mut points = Vec::new();

    session.context(range, &mut points).unwrap();

    let without_rate = points
        .iter()
        .filter(|point| point["cpu_percent"].is_null())
        .count();
    assert_eq!(points.len(), 8);
    assert_eq!(without_rate, 2);
}

#[test]
fn a_reader_refuses_a_minute_past_what_it_may_hold() {
    // Goal: the minutes one reader holds stay inside their bound.
    // Method: lend a reader a minute at the bound, then read the first minute of a recording.
    let fixture = saved_seconds();
    let session = Session::open(&fixture.root).unwrap();
    let mut minutes = Minutes::new();
    let held = MachineMinute {
        fans: Vec::with_capacity(READER_MINUTES_BYTES_MAX / size_of::<FanRow>() + 1),
        ..MachineMinute::default()
    };
    minutes.insert(1, held);

    let result = trends_minute(&session.reader(), &mut minutes, 0).map(|_minute| ());

    assert!(
        matches!(result, Err(Error::Bound("context minute bytes"))),
        "{result:?}"
    );
}

/// Rewrites saved table `name` of every window of `fixture` through `damage`.
fn typed_damage(fixture: &Fixture, name: &str, damage: fn(DataFrame) -> DataFrame) {
    let session = Session::open(&fixture.root).unwrap();
    for manifest in &session.windows {
        let path = fixture
            .root
            .join(&manifest.data_directory)
            .join(format!("{name}.parquet"));
        crate::recording::damage_table_file(&path, damage);
    }
}

/// `frame`'s rows repeated in order to `height` rows.
fn typed_repeated(frame: &DataFrame, height: usize) -> DataFrame {
    let rows = frame.height();
    let indices: Vec<IdxSize> = (0..height)
        .map(|index| IdxSize::try_from(index % rows).unwrap())
        .collect();
    frame
        .take(&IdxCa::from_vec("index".into(), indices))
        .unwrap()
}

fn typed_column(mut frame: DataFrame, column: Column) -> DataFrame {
    frame.with_column(column).unwrap();
    frame
}

#[test]
fn a_reader_may_hold_minutes_up_to_its_bound_and_no_more() {
    // Goal: the bound on what a reader holds names the most it may hold.
    // Method: check the bound itself and a byte past it.
    let at_the_bound = minutes_bytes_check(READER_MINUTES_BYTES_MAX);
    let past_the_bound = minutes_bytes_check(READER_MINUTES_BYTES_MAX + 1);

    assert!(at_the_bound.is_ok(), "{at_the_bound:?}");
    assert!(
        matches!(past_the_bound, Err(Error::Bound("context minute bytes"))),
        "{past_the_bound:?}"
    );
}

#[test]
fn a_gauge_no_sensor_could_read_is_no_number() {
    // Goal: where a reading is read as a number, one that is no number counts as missing.
    // Method: read the cells of a gauge that is not a number and of one past every number.
    assert_eq!(Cell::Float32(f32::NAN).number().unwrap(), None);
    assert_eq!(Cell::Float32(f32::INFINITY).number().unwrap(), None);
}

/// A counter column `name` of `height` cells that hold nothing.
fn typed_null(name: &str, height: usize) -> Column {
    Column::new(name.into(), vec![None::<u64>; height])
}

#[test]
fn a_null_saved_counter_refuses_its_minute() {
    // Goal: a counter the format always holds, read as nothing, is damage rather than a gap.
    // Method: replace a saved memory counter with nulls and select a second of that minute.
    let fixture = saved_seconds();
    typed_damage(&fixture, "memory", |frame| {
        let height = frame.height();
        typed_column(frame, typed_null("pageins", height))
    });
    let mut session = Session::open(&fixture.root).unwrap();

    let result = session.select(SECOND, 0);

    assert!(
        matches!(result, Err(Error::Bound("query null cell"))),
        "{result:?}"
    );
}

#[test]
fn a_counter_saved_as_text_refuses_its_minute() {
    // Goal: a numeric string in a counter's column is not read as the number it spells.
    // Method: replace a saved memory counter with its decimal text and select a second of it.
    let fixture = saved_seconds();
    typed_damage(&fixture, "memory", |frame| {
        let cells = vec!["1"; frame.height()];
        typed_column(frame, Column::new("total_bytes".into(), cells))
    });
    let mut session = Session::open(&fixture.root).unwrap();

    let result = session.select(SECOND, 0);

    assert!(
        matches!(result, Err(Error::Bound("query column type"))),
        "{result:?}"
    );
}

#[test]
fn a_minute_of_more_ticks_than_a_minute_holds_refuses() {
    // Goal: a file cannot make a read decode more ticks than a minute can hold.
    // Method: repeat a minute's ticks past the most a minute holds and select a second of it.
    let fixture = saved_seconds();
    typed_damage(&fixture, "ticks", |frame| typed_repeated(&frame, 62));
    let mut session = Session::open(&fixture.root).unwrap();

    let result = session.select(SECOND, 0);

    assert!(
        matches!(result, Err(Error::Bound("ticks of a minute"))),
        "{result:?}"
    );
}

#[test]
fn a_minute_of_the_most_ticks_passes_the_bound_on_its_ticks() {
    // Goal: the bound on a minute's ticks is exact.
    // Method: repeat a minute's ticks to the most a minute holds, which their order refuses next.
    let fixture = saved_seconds();
    typed_damage(&fixture, "ticks", |frame| typed_repeated(&frame, 61));
    let mut session = Session::open(&fixture.root).unwrap();

    let result = session.select(SECOND, 0);

    assert!(
        matches!(result, Err(Error::Bound("tick order"))),
        "{result:?}"
    );
}

#[test]
fn a_table_of_more_rows_than_its_ticks_can_write_refuses() {
    // Goal: what a read decodes of a table is bounded by the ticks of its minute.
    // Method: repeat a table's rows one past the bound on its eight ticks and select a second.
    let fixture = saved_seconds();
    typed_damage(&fixture, "cpus", |frame| {
        typed_repeated(&frame, 8 * TABLE_ROWS_PER_TICK_MAX + 1)
    });
    let mut session = Session::open(&fixture.root).unwrap();

    let result = session.select(SECOND, 0);

    assert!(
        matches!(result, Err(Error::Bound("table rows"))),
        "{result:?}"
    );
}

#[test]
fn a_table_at_the_most_rows_its_ticks_can_write_passes_that_bound() {
    // Goal: the bound on a table's rows is exact.
    // Method: repeat a table's rows to the bound on its eight ticks, which their order refuses.
    let fixture = saved_seconds();
    typed_damage(&fixture, "cpus", |frame| {
        typed_repeated(&frame, 8 * TABLE_ROWS_PER_TICK_MAX)
    });
    let mut session = Session::open(&fixture.root).unwrap();

    let result = session.select(SECOND, 0);

    assert!(
        matches!(result, Err(Error::Bound("reading order"))),
        "{result:?}"
    );
}

#[test]
fn a_saved_fact_past_a_cells_bound_refuses_its_minute() {
    // Goal: one long text cannot be copied into every tick of a minute.
    // Method: save a model name one byte past the bound in each tick and select a second.
    let fixture = crate::recording::sealed_system_minute();
    typed_damage(&fixture, "ticks", |frame| {
        let model = "m".repeat(MACHINE_TEXT_BYTES_MAX + 1);
        let cells = vec![model; frame.height()];
        typed_column(frame, Column::new("machine_model".into(), cells))
    });
    let mut session = Session::open(&fixture.root).unwrap();

    let result = session.select(1001, 0);

    assert!(
        matches!(result, Err(Error::Bound("machine text"))),
        "{result:?}"
    );
}

#[test]
fn a_saved_fact_at_a_cells_bound_reads() {
    // Goal: the bound on a saved fact is exact.
    // Method: save a model name at the bound in each tick and select a second.
    let fixture = crate::recording::sealed_system_minute();
    typed_damage(&fixture, "ticks", |frame| {
        let model = "m".repeat(MACHINE_TEXT_BYTES_MAX);
        let cells = vec![model; frame.height()];
        typed_column(frame, Column::new("machine_model".into(), cells))
    });
    let mut session = Session::open(&fixture.root).unwrap();

    let result = session.select(1001, 0);

    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn a_tick_outside_its_minutes_publications_refuses_its_minute() {
    // Goal: a file cannot show a reading its minute's manifest does not hold.
    // Method: move every saved tick past the manifest's publications and select a second.
    let fixture = saved_seconds();
    typed_damage(&fixture, "ticks", |frame| {
        let publications: Vec<u64> = (0..frame.height())
            .map(|index| 1_000_000 + u64::try_from(index).unwrap())
            .collect();
        typed_column(frame, Column::new("publication".into(), publications))
    });
    let mut session = Session::open(&fixture.root).unwrap();

    let result = session.select(SECOND, 0);

    assert!(
        matches!(result, Err(Error::Bound("tick publication"))),
        "{result:?}"
    );
}

#[test]
fn cpu_rows_out_of_order_refuse_their_minute() {
    // Goal: a logical CPU's identity never rests on where its row lies in a file.
    // Method: save each reading's CPU 1 before its CPU 0 and select a second of them.
    let fixture = saved_seconds();
    typed_damage(&fixture, "cpus", |frame| {
        let higher = frame
            .clone()
            .lazy()
            .with_column(lit(1).cast(DataType::UInt32).alias("cpu"))
            .collect()
            .unwrap();
        higher.vstack(&frame).unwrap()
    });
    let mut session = Session::open(&fixture.root).unwrap();

    let result = session.select(SECOND, 0);

    assert!(
        matches!(result, Err(Error::Bound("reading order"))),
        "{result:?}"
    );
}

#[test]
fn a_gap_in_the_logical_cpus_refuses_the_reading() {
    // Goal: a missing CPU never reads as a CPU that did nothing.
    // Method: move a saved CPU to the next index, so CPU 0 is missing, and select a second.
    let fixture = saved_seconds();
    typed_damage(&fixture, "cpus", |frame| {
        frame
            .lazy()
            .with_column((col("cpu") + lit(1_u32)).alias("cpu"))
            .collect()
            .unwrap()
    });
    let mut session = Session::open(&fixture.root).unwrap();

    let result = session.select(SECOND, 0);

    assert!(
        matches!(result, Err(Error::Bound("incomplete logical CPU domain"))),
        "{result:?}"
    );
}

#[test]
fn saved_cpu_history_leaves_out_a_reading_before_its_range() {
    // Goal: CPU history reads only its range's readings, so one before the range cannot refuse it.
    // Method: leave a gap in the CPUs of an early reading, save a later run a range away so that
    // reading falls out of the range, and read the history.
    let fixture = saved_seconds();
    let mut later = Fixture::new_mode(false, 2, Some(&fixture));
    later.publish_with_system_rows(LAST_SECOND + 959);
    later.seal();
    typed_damage(&fixture, "cpus", |frame| {
        let cpu = when(col("publication").eq(lit(2_u64)))
            .then(col("cpu") + lit(1_u32))
            .otherwise(col("cpu"));
        frame
            .lazy()
            .with_column(cpu.alias("cpu"))
            .collect()
            .unwrap()
    });
    let mut session = Session::open(&fixture.root).unwrap();
    let range = ContextRange::ending_at(LAST_SECOND + 959).unwrap();

    let result = session.cpu_history_restore(range);

    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn a_temperature_no_sensor_reads_refuses_its_minute() {
    // Goal: an impossible temperature never reaches a chart.
    // Method: save die temperatures below zero, which no sensor reports, and select a second.
    let fixture = saved_seconds();
    typed_damage(&fixture, "ticks", |frame| {
        let cells = vec![-1.0_f32; frame.height()];
        let frame = typed_column(frame, Column::new("cpu_celsius".into(), cells.clone()));
        typed_column(frame, Column::new("gpu_celsius".into(), cells))
    });
    let mut session = Session::open(&fixture.root).unwrap();

    let result = session.select(SECOND, 0);

    assert!(
        matches!(result, Err(Error::Bound("temperature reading"))),
        "{result:?}"
    );
}

#[test]
fn a_table_file_that_is_no_table_refuses_its_minute() {
    // Goal: a file admission turns away stops its minute with admission's reason.
    // Method: overwrite a saved table with bytes that are no parquet file and select a second.
    let fixture = saved_seconds();
    let session = Session::open(&fixture.root).unwrap();
    let path = fixture
        .root
        .join(&session.windows[0].data_directory)
        .join("memory.parquet");
    std::fs::write(path, b"damaged").unwrap();
    let mut session = Session::open(&fixture.root).unwrap();

    let result = session.select(SECOND, 0);

    assert!(
        matches!(result, Err(Error::Bound("parquet file framing"))),
        "{result:?}"
    );
}

#[test]
fn a_read_past_its_allowance_refuses_with_that_reason() {
    // Goal: an allowance a read ran out of is named as such, not as damage.
    // Method: translate admission's refusal of a read past its budget.
    let refused = minute_refusal(Refused::Input(AdmissionRefusal::Budget));

    assert!(matches!(refused, Error::Bound("machine read budget")));
}

#[test]
fn a_nonfinite_saved_gauge_refuses_its_minute() {
    // Goal: a gauge no sensor could read stops the minute rather than reaching a chart.
    // Method: replace a saved power gauge with NaN and select a second of that minute.
    let fixture = saved_seconds();
    typed_damage(&fixture, "ticks", |frame| {
        let cells = vec![Some(f32::NAN); frame.height()];
        typed_column(frame, Column::new("power_watts".into(), cells))
    });
    let mut session = Session::open(&fixture.root).unwrap();

    let result = session.select(SECOND, 0);

    assert!(
        matches!(result, Err(Error::Bound("unsupported or nonfinite cell"))),
        "{result:?}"
    );
}

#[test]
fn a_battery_reading_without_its_source_time_refuses_its_minute() {
    // Goal: a battery group that holds readings and no source time is damage, not a gap.
    // Method: clear the source time of a saved battery group that holds its readings.
    let fixture = crate::recording::sealed_system_minute();
    typed_damage(&fixture, "ticks", |frame| {
        let height = frame.height();
        typed_column(frame, typed_null("battery_source_continuous_ns", height))
    });
    let mut session = Session::open(&fixture.root).unwrap();

    let result = session.select(1001, 0);

    assert!(
        matches!(
            result,
            Err(Error::Bound("battery payload without source time"))
        ),
        "{result:?}"
    );
}
