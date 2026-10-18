use polars::prelude::{Column, DataFrame, DataType, NamedFrom, Series};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use super::{Engine, Row};
use crate::Error;

#[test]
fn preserves_full_width_counters_and_binary_identity() {
    // Goal: materialization preserves counters and identity bytes without text conversion.
    // Method: query the maximum counter beside invalid UTF-8 and a literal escape sequence.
    let engine = Engine::new();
    let bytes: &[u8] = b"\xff\\x41";
    engine
        .register(
            "captured",
            DataFrame::new(
                1,
                vec![
                    Column::new("counter".into(), [u64::MAX]),
                    Series::new("identity".into(), [bytes]).into(),
                ],
            )
            .unwrap(),
        )
        .unwrap();

    let frame = engine.query("SELECT * FROM captured").unwrap();
    let row = Row::new(&frame, 0).unwrap();

    assert_eq!(row.get::<u64>(0).unwrap(), u64::MAX);
    assert_eq!(row.get::<Vec<u8>>(1).unwrap(), bytes);
}

#[test]
fn preserves_missing_values_with_their_storage_types() {
    // Goal: absent counters retain the schema needed for legacy recording validation.
    // Method: collect a typed NULL and inspect its public schema and materialized value.
    let engine = Engine::new();

    let frame = engine.query("SELECT NULL::UBIGINT counter").unwrap();

    assert_eq!(frame.column("counter").unwrap().dtype(), &DataType::UInt64);
    assert_eq!(
        Row::new(&frame, 0).unwrap().get::<Option<u64>>(0).unwrap(),
        None
    );
}

#[test]
fn rejects_a_counter_that_does_not_fit_its_destination() {
    // Goal: narrowed counters cannot wrap during typed materialization.
    // Method: ask for an eight-bit value from a full-width maximum.
    let engine = Engine::new();
    let frame = engine
        .query("SELECT 18446744073709551615::UBIGINT counter")
        .unwrap();

    let result = Row::new(&frame, 0).unwrap().get::<u8>(0);

    assert!(matches!(result, Err(Error::Bound("query integer range"))));
}

#[test]
fn retains_a_planned_source_after_unregistering_its_alias() {
    // Goal: a bounded query context can release aliases without invalidating pending work.
    // Method: plan a registered source, remove its alias, then materialize the plan.
    let engine = Engine::new();
    engine
        .register(
            "captured",
            DataFrame::new(1, vec![Column::new("counter".into(), [u64::MAX])]).unwrap(),
        )
        .unwrap();
    let plan = engine.plan("SELECT * FROM captured").unwrap();
    engine.unregister("captured").unwrap();

    let frame = engine.collect(plan).unwrap();

    assert_eq!(
        Row::new(&frame, 0).unwrap().get::<u64>(0).unwrap(),
        u64::MAX
    );
}

#[test]
fn qualifies_rows_before_removing_window_partition_columns() {
    // Goal: saved-second deduplication keeps the higher-priority terminal observation.
    // Method: project only pid/value while QUALIFY orders a partition on an omitted column.
    let engine = Engine::new();
    engine
        .register(
            "captured",
            DataFrame::new(
                2,
                vec![
                    Column::new("pid".into(), [7i32, 7]),
                    Column::new("value".into(), [11u64, 19]),
                    Column::new("priority".into(), [0u8, 1]),
                ],
            )
            .unwrap(),
        )
        .unwrap();

    let frame = engine
        .query(
            "WITH observations AS (SELECT * FROM captured) \
            SELECT pid, value FROM observations QUALIFY row_number() OVER \
            (PARTITION BY pid ORDER BY priority DESC)=1",
        )
        .unwrap();

    assert_eq!(frame.height(), 1);
    assert_eq!(Row::new(&frame, 0).unwrap().get::<u64>(1).unwrap(), 19);
}

#[test]
fn preserves_float32_nan_and_negative_zero() {
    // Goal: stored sensor readings preserve NaN and the sign of zero.
    // Method: materialize a real Float32 column and compare its bit-level observations.
    let engine = Engine::new();
    engine
        .register(
            "captured",
            DataFrame::new(2, vec![Column::new("sensor".into(), [f32::NAN, -0.0f32])]).unwrap(),
        )
        .unwrap();

    let frame = engine.query("SELECT * FROM captured").unwrap();

    assert!(Row::new(&frame, 0).unwrap().get::<f32>(0).unwrap().is_nan());
    assert_eq!(
        Row::new(&frame, 1)
            .unwrap()
            .get::<f32>(0)
            .unwrap()
            .to_bits(),
        (-0.0f32).to_bits()
    );
}

#[test]
fn interrupts_native_work_and_reuses_the_engine_afterward() {
    // Goal: interruption stops native work even without a viewer epoch fence.
    // Method: collect a gated native plan, interrupt before its next executor, then reuse.
    let engine = Arc::new(Engine::new());
    let interrupt = engine.interrupt_handle();
    let (started, ready) = mpsc::sync_channel(1);
    let (proceed, permitted) = mpsc::sync_channel(1);
    let collecting = Arc::clone(&engine);
    let worker = std::thread::spawn(move || {
        collecting.collect(native_cancellation_plan(started, permitted))
    });
    let started = ready.recv_timeout(Duration::from_secs(5));
    native_cancellation_wait_for_installation(&interrupt);

    let interrupted = interrupt.interrupt();
    let released = proceed.send(());
    let result = worker.join().unwrap();

    started.unwrap();
    interrupted.unwrap();
    released.unwrap();
    assert!(matches!(
        result,
        Err(Error::Engine(polars::error::PolarsError::ComputeError(message)))
            if message.as_ref() == "query interrupted"
    ));
    let frame = engine.query("SELECT 23::UBIGINT value").unwrap();
    assert_eq!(Row::new(&frame, 0).unwrap().get::<u64>(0).unwrap(), 23);
}

fn native_cancellation_wait_for_installation(interrupt: &super::InterruptHandle) {
    // Native work may start before collect installs its handle. Observe, never mutate it.
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(1) {
        match interrupt.query.lock() {
            Ok(query) => {
                if query.is_some() {
                    return;
                }
            }
            Err(_) => return,
        }
        std::thread::yield_now();
    }
}

fn native_cancellation_plan(
    started: mpsc::SyncSender<()>,
    permitted: mpsc::Receiver<()>,
) -> polars::prelude::LazyFrame {
    use polars::prelude::{AllowedOptimizations, IntoLazy, UnionArgs, col, concat, lit};
    let permitted = Mutex::new(permitted);
    let first = DataFrame::new(1, vec![Column::new("value".into(), [11u64])])
        .unwrap()
        .lazy()
        .map(
            move |frame| {
                let error =
                    |failure: String| polars::error::PolarsError::ComputeError(failure.into());
                started
                    .send(())
                    .map_err(|failure| error(failure.to_string()))?;
                permitted
                    .lock()
                    .map_err(|failure| error(failure.to_string()))?
                    .recv_timeout(Duration::from_secs(5))
                    .map_err(|failure| error(failure.to_string()))?;
                Ok(frame)
            },
            AllowedOptimizations::empty(),
            None,
            Some("native_cancellation_checkpoint"),
        );
    let second = DataFrame::new(1, vec![Column::new("value".into(), [17u64])])
        .unwrap()
        .lazy()
        .select([(col("value") + lit(1u64)).alias("value")]);

    concat(
        [first, second],
        UnionArgs {
            parallel: false,
            rechunk: false,
            to_supertypes: false,
            diagonal: false,
            strict: true,
            from_partitioned_ds: false,
            maintain_order: true,
        },
    )
    .unwrap()
    .without_optimizations()
}
