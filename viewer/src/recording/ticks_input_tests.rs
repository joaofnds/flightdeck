use std::cell::{Cell, RefCell};
use std::fs::File;
use std::io::Seek;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use collector::recording::private;
use persistence::engine::{AdmissionRefusal, AdmittedTable};
use persistence::resources::ByteBudget;
use polars::polars_utils::compression::ZstdLevel;
use polars::prelude::{
    DataFrame, IntoLazy, ParquetCompression, ParquetWriter, StatisticsOptions, col, lit,
};

use super::{MachineMinute, SealedMinute, SealedSource, read_machine};
use crate::cancellation::Cancellation;
use crate::live::identity_fixture::Fixture;

#[path = "ticks_input_tests/allocation.rs"]
mod allocation;

#[test]
fn captured_optional_columns_override_an_incomplete_replacement_schema() {
    // Goal: optional groups come from captured admission, even before machine decoding.
    // Method: capture a complete pair, remove one column in the replacement, then decode.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let marker = marker(&fixture);
    let manifest = persistence::read_committed(&fixture.root, &marker).unwrap();
    let ticks = fixture
        .root
        .join(manifest.data_directory)
        .join("ticks.parquet");
    let frame = persistence::engine::scan(&ticks)
        .unwrap()
        .collect()
        .unwrap();
    let frame = frame
        .lazy()
        .with_columns([
            lit(5.0f32)
                .cast(polars::prelude::DataType::Float32)
                .alias("cpu_celsius"),
            lit(7.0f32)
                .cast(polars::prelude::DataType::Float32)
                .alias("gpu_celsius"),
        ])
        .collect()
        .unwrap();
    std::fs::remove_file(&ticks).unwrap();
    write_ticks(&ticks, frame.clone());
    let replacement = fixture.root.join("replacement.parquet");
    write_ticks(&replacement, frame.drop("cpu_celsius").unwrap());
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let minute = SealedMinute::open_with_ticks(
        &SealedSource {
            root: &fixture.root,
            marker: &marker,
        },
        &budget,
        &operation,
    )
    .unwrap();

    std::fs::rename(replacement, &ticks).unwrap();
    let mut machine = MachineMinute::default();
    read_machine(&minute, &mut machine).unwrap();
    let legacy = SealedMinute::open(&fixture.root, &marker).unwrap();
    let refused = read_machine(&legacy, &mut MachineMinute::default());

    assert_eq!(machine.ticks[0].cpu_celsius, Some(5.0));
    assert_eq!(machine.ticks[0].gpu_celsius, Some(7.0));
    assert!(matches!(
        refused,
        Err(super::Refused::Decode(persistence::Error::Bound(
            "incomplete column group"
        )))
    ));
    assert!(budget.used() > 0);
    drop(minute);
    assert_eq!(budget.used(), 0);
}

#[test]
fn cancellation_between_consumers_prevents_any_second_native_allocation() {
    // Goal: cancellation between machine and endpoint consumers starts no second native read.
    // Method: cancel the shared epoch after machine decode and observe the endpoint refusal.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let cancellation = Arc::new(Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    let minute = SealedMinute::open_with_ticks(
        &SealedSource {
            root: &fixture.root,
            marker: &marker(&fixture),
        },
        &budget,
        &operation,
    )
    .unwrap();
    read_machine(&minute, &mut MachineMinute::default()).unwrap();
    cancellation.cancel().unwrap();

    let (result, allocations) =
        allocation::observe(|| super::processes::read_processes_endpoints(&minute));

    assert!(
        matches!(result, Err(super::Refused::Decode(persistence::Error::Io(error)))
        if error.kind() == std::io::ErrorKind::Interrupted)
    );
    assert_eq!(allocations, 0);
    drop(minute);
    assert_eq!(budget.used(), 0);
}

#[test]
fn process_endpoints_reuse_the_machine_ticks_after_the_source_path_is_replaced() {
    // Goal: two consumers use one captured ticks input without collapsing same-second siblings.
    // Method: capture ticks, replace the path, then decode both real consumers.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(100);
    fixture.seal_minute(0);
    let marker = marker(&fixture);
    let manifest = persistence::read_committed(&fixture.root, &marker).unwrap();
    let ticks = fixture
        .root
        .join(&manifest.data_directory)
        .join("ticks.parquet");
    let replacement = prepare_replacement(&fixture.root, &ticks);
    let original_size = std::fs::metadata(&ticks).unwrap().len();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let captures = CountedCapture::default();
    let minute = SealedMinute::open_with_ticks_using(
        &SealedSource {
            root: &fixture.root,
            marker: &marker,
        },
        &budget,
        &operation,
        |file, budget| captures.capture(file, budget),
    )
    .unwrap();
    let mut machine = MachineMinute::default();

    std::fs::rename(&replacement, &ticks).unwrap();
    read_machine(&minute, &mut machine).unwrap();
    let endpoints = super::processes::read_processes_endpoints(&minute).unwrap();
    minute.revalidate_full().unwrap();

    let machine_keys: Vec<_> = machine
        .ticks
        .iter()
        .map(|tick| {
            (
                tick.publication,
                tick.run_tick,
                tick.wall_ns,
                tick.continuous_ns,
                tick.awake_ns,
            )
        })
        .collect();
    let process_keys: Vec<_> = endpoints
        .iter()
        .map(|endpoint| {
            (
                endpoint.stamp.publication,
                endpoint.run_tick,
                endpoint.wall_ns,
                endpoint.stamp.continuous_ns,
                endpoint.stamp.awake_ns,
            )
        })
        .collect();
    let expected = original_clocks();
    assert_eq!(process_keys, expected);
    assert_eq!(machine_keys, expected);
    assert_eq!(captures.attempts.get(), 1);
    assert_eq!(captures.completed.get(), 1);
    assert_eq!(captures.position(), original_size);
}

type Clocks = (u64, u64, Option<u64>, Option<u64>, Option<u64>);

fn original_clocks() -> [Clocks; 2] {
    [
        (1, 0, Some(100_000_000_000), Some(0), Some(0)),
        (
            2,
            1,
            Some(100_000_000_000),
            Some(1_000_000_000),
            Some(1_000_000_000),
        ),
    ]
}

#[test]
fn one_endpoint_above_the_admitted_limit_is_refused_before_native_decoding() {
    // Goal: the first invalid count is refused before native endpoint projection.
    // Method: trace a real 62-row input and retain the legacy prefix control.
    assert_admitted_endpoint_count_refused(62);
}

#[test]
fn an_overlong_admitted_minute_is_refused_before_native_decoding() {
    // Goal: a longer invalid minute is refused before native endpoint projection.
    // Method: trace a real 124-row input and retain the legacy prefix control.
    assert_admitted_endpoint_count_refused(124);
}

#[test]
fn an_empty_admitted_endpoint_table_remains_readable() {
    // Goal: early count refusal preserves empty endpoint tables.
    // Method: read the empty projection from a real captured file.
    assert_admitted_endpoint_count_readable(0);
}

#[test]
fn an_admitted_endpoint_table_at_the_limit_remains_readable() {
    // Goal: early count refusal preserves the maximum accepted endpoint table.
    // Method: read every publication from a real captured 61-row file.
    assert_admitted_endpoint_count_readable(61);
}

fn assert_admitted_endpoint_count_refused(count: u64) {
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    replace_ticks_count(&fixture, count);
    let marker = marker(&fixture);
    let manifest = persistence::read_committed(&fixture.root, &marker).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let minute = SealedMinute::open_with_ticks(
        &SealedSource {
            root: &fixture.root,
            marker: &marker,
        },
        &budget,
        &operation,
    )
    .unwrap();
    let mut before = None;
    let mut accepted = 0;
    let trace = persistence::work::ingestion::Trace::new(manifest.identity.run.recording);

    let observation = trace.enter();
    let endpoints = minute.endpoints();
    drop(observation);
    let result = minute.visit_samples_with_operation(&mut before, &operation, |_, _| {
        accepted += 1;
        Ok(())
    });

    assert!(matches!(
        endpoints,
        Err(super::Refused::Decode(persistence::Error::Bound(
            "committed App batch endpoints"
        )))
    ));
    assert_eq!(endpoint_decode_entries(&trace), 0);
    assert!(result.is_err());
    assert_eq!(accepted, 0);
    assert!(before.is_none());
    let legacy = SealedMinute::open(&fixture.root, &marker).unwrap();
    let endpoints = super::processes::read_processes_endpoints(&legacy).unwrap();
    assert_eq!(endpoints.len(), 62);
    assert_eq!(endpoints.last().unwrap().stamp.publication, 62);
}

fn assert_admitted_endpoint_count_readable(count: u64) {
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    replace_ticks_count(&fixture, count);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let minute = SealedMinute::open_with_ticks(
        &SealedSource {
            root: &fixture.root,
            marker: &marker(&fixture),
        },
        &budget,
        &operation,
    )
    .unwrap();

    let endpoints = minute.endpoints().unwrap();

    let publications: Vec<_> = endpoints
        .iter()
        .map(|endpoint| endpoint.stamp.publication)
        .collect();
    assert_eq!(publications, (1..=count).collect::<Vec<_>>());
    drop(minute);
    assert_eq!(budget.used(), 0);
}

fn endpoint_decode_entries(trace: &persistence::work::ingestion::Trace) -> usize {
    let mut events = Vec::new();
    trace.snapshot(&mut events).unwrap();
    events
        .iter()
        .filter(|event| event.kind == persistence::work::ingestion::Kind::ProjectionDecode)
        .count()
}

fn replace_ticks_count(fixture: &Fixture, count: u64) {
    assert!(count <= 124);
    let manifest = persistence::read_committed(&fixture.root, &marker(fixture)).unwrap();
    let ticks = fixture
        .root
        .join(manifest.data_directory)
        .join("ticks.parquet");
    let row = persistence::engine::scan(&ticks)
        .unwrap()
        .collect()
        .unwrap();
    let mut frame = row.head(Some(0));
    for _row in 0..count {
        frame.vstack_mut(&row).unwrap();
    }
    frame
        .with_column(polars::prelude::Column::new(
            "publication".into(),
            (1..=count).collect::<Vec<_>>(),
        ))
        .unwrap();
    std::fs::remove_file(&ticks).unwrap();
    write_ticks(&ticks, frame);
}

// This is a forwarding spy on the concrete File capture boundary. It owns no fake input.
// A shared clone observes captured bytes independently of capture counts.
// Counts are successful compressed captures, not calls to File::read/read_exact/EOF checks.
#[derive(Default)]
struct CountedCapture {
    attempts: Cell<u64>,
    completed: Cell<u64>,
    descriptor: RefCell<Option<File>>,
}

#[test]
fn captured_absent_group_stays_absent_after_a_complete_replacement() {
    // Goal: optional absence remains a fact of the captured schema.
    // Method: replace an absent group with a complete group before holds.
    assert_captured_group(0);
}

#[test]
fn captured_partial_group_stays_refused_after_a_complete_replacement() {
    // Goal: an incomplete captured group remains refused after replacement.
    // Method: replace a partial group with a complete group before holds.
    assert_captured_group(1);
}

fn assert_captured_group(count: u8) {
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let marker = marker(&fixture);
    let manifest = persistence::read_committed(&fixture.root, &marker).unwrap();
    let ticks = fixture
        .root
        .join(manifest.data_directory)
        .join("ticks.parquet");
    let complete = persistence::engine::scan(&ticks)
        .unwrap()
        .collect()
        .unwrap()
        .lazy()
        .with_columns([
            lit(5.0f32)
                .cast(polars::prelude::DataType::Float32)
                .alias("cpu_celsius"),
            lit(7.0f32)
                .cast(polars::prelude::DataType::Float32)
                .alias("gpu_celsius"),
        ])
        .collect()
        .unwrap();
    let mut original = complete.drop("gpu_celsius").unwrap();
    if count == 0 {
        original = original.drop("cpu_celsius").unwrap();
    }
    std::fs::remove_file(&ticks).unwrap();
    write_ticks(&ticks, original);
    let replacement = fixture.root.join("complete-ticks.parquet");
    write_ticks(&replacement, complete);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let minute = SealedMinute::open_with_ticks(
        &SealedSource {
            root: &fixture.root,
            marker: &marker,
        },
        &budget,
        &operation,
    )
    .unwrap();
    std::fs::rename(replacement, ticks).unwrap();
    let table = minute.ticks().unwrap();

    let (result, requests) = allocation::observe(|| table.holds(&["cpu_celsius", "gpu_celsius"]));

    assert_eq!(requests, 0);
    match count {
        0 => assert!(matches!(result, Ok(false))),
        1 => assert!(matches!(
            result,
            Err(super::Refused::Decode(persistence::Error::Bound(
                "incomplete column group"
            )))
        )),
        _ => panic!("unexpected fixture column count"),
    }
}

#[test]
fn input_budget_denial_completes_no_capture_and_preserves_existing_usage() {
    // Goal: the viewer's opt-in boundary returns typed denial without legacy retry.
    // Method: exhaust the shared budget and forward capture to the real concrete-file owner.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let budget = Arc::new(ByteBudget::new(1024));
    let retained = budget.reserve(1024).unwrap();
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let captures = CountedCapture::default();

    let result = SealedMinute::open_with_ticks_using(
        &SealedSource {
            root: &fixture.root,
            marker: &marker(&fixture),
        },
        &budget,
        &operation,
        |file, budget| captures.capture(file, budget),
    );

    assert!(matches!(
        result,
        Err(super::Refused::Input(AdmissionRefusal::Budget))
    ));
    assert_eq!(captures.attempts.get(), 1);
    assert_eq!(captures.completed.get(), 0);
    assert_eq!(captures.position(), 0);
    assert_eq!(budget.used(), retained.bytes());
}

#[test]
fn changed_marker_after_both_consumers_refuses_final_acceptance() {
    // Goal: previously decoded immutable bytes cannot bless an invalid commitment.
    // Method: finish both consumers, unlink the marker, then perform final validation.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let marker = marker(&fixture);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let minute = SealedMinute::open_with_ticks(
        &SealedSource {
            root: &fixture.root,
            marker: &marker,
        },
        &budget,
        &operation,
    )
    .unwrap();
    read_machine(&minute, &mut MachineMinute::default()).unwrap();
    assert_eq!(
        super::processes::read_processes_endpoints(&minute)
            .unwrap()
            .len(),
        1
    );
    std::fs::remove_file(marker).unwrap();

    let result = minute.revalidate_full();

    assert!(matches!(result, Err(super::Refused::Commitment(_))));
    drop(minute);
    assert_eq!(budget.used(), 0);
}

impl CountedCapture {
    fn capture(
        &self,
        file: File,
        budget: &Arc<ByteBudget>,
    ) -> Result<AdmittedTable, AdmissionRefusal> {
        self.attempts.set(self.attempts.get() + 1);
        assert!(self.descriptor.borrow().is_none());
        *self.descriptor.borrow_mut() = Some(file.try_clone().unwrap());
        let result = AdmittedTable::from_file_budgeted(file, budget);
        if result.is_ok() {
            self.completed.set(self.completed.get() + 1);
        }

        result
    }

    fn position(&self) -> u64 {
        self.descriptor
            .borrow_mut()
            .as_mut()
            .unwrap()
            .stream_position()
            .unwrap()
    }
}

fn marker(fixture: &Fixture) -> PathBuf {
    persistence::layout::WindowKey {
        run: fixture.writer.identity().unwrap().run,
        window: 0,
    }
    .marker(&fixture.root)
}

fn prepare_replacement(root: &Path, ticks: &Path) -> PathBuf {
    // Prepare outside the data directory before observation; no fixture writes join capture counts.
    // Keep publication numbers so process endpoint interpretation succeeds and exposes bad clocks.
    let frame = persistence::engine::scan(ticks).unwrap().collect().unwrap();
    let frame: DataFrame = frame
        .lazy()
        .with_columns([
            (col("wall_ns") + lit(10_000_000_000u64)).alias("wall_ns"),
            (col("continuous_ns") + lit(10_000_000_000u64)).alias("continuous_ns"),
            (col("awake_ns") + lit(10_000_000_000u64)).alias("awake_ns"),
        ])
        .collect()
        .unwrap();
    let replacement = root.join("replacement-ticks.parquet");
    write_ticks(&replacement, frame);

    replacement
}

fn write_ticks(path: &Path, mut frame: DataFrame) {
    let file = private::open_private_file(path, true).unwrap();
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
        .finish(&mut frame)
        .unwrap();
}
