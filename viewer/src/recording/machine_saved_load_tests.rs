use std::path::Path;
use std::sync::Arc;

use persistence::engine::AdmittedTable;
use polars::polars_utils::compression::ZstdLevel;
use polars::prelude::{
    DataFrame, DataType, Null, ParquetCompression, ParquetWriter, StatisticsOptions, lit,
};

use super::{MachineMinute, SealedMinute, read_machine};
use crate::byte_budget::ByteBudget;
use crate::cancellation::Cancellation;
use crate::store::{CompleteMachineState, MachineSecond};

#[derive(Clone, Copy)]
enum SavedReader {
    Legacy,
    Admitted,
}

#[test]
fn legacy_finite_partial_load_average_remains_ordinary_absence() {
    // Goal: partial finite saved averages stay absent in the grouped summary.
    // Method: retain each real file cell through the legacy typed reader and shared View.
    assert_finite_saved_load(SavedReader::Legacy);
}

#[test]
fn admitted_finite_partial_load_average_remains_ordinary_absence() {
    // Goal: admission preserves each partial finite cell without creating a complete triple.
    // Method: decode real admitted bytes, inspect exact cells and require grouped absence.
    assert_finite_saved_load(SavedReader::Admitted);
}

#[test]
fn legacy_nan_beside_a_null_load_average_refuses_complete_projection() {
    // Goal: a missing saved average cannot hide a present NaN from numeric validation.
    // Method: decode real Parquet and compare the independent legacy numeric projection.
    assert_invalid_saved_load(f64::NAN, SavedReader::Legacy);
}

#[test]
fn admitted_nan_beside_a_null_load_average_refuses_complete_projection() {
    // Goal: a present NaN remains refused beside a null in an admitted source.
    // Method: decode admitted Parquet and compare the independent legacy numeric projection.
    assert_invalid_saved_load(f64::NAN, SavedReader::Admitted);
}

#[test]
fn legacy_infinity_beside_a_null_load_average_refuses_complete_projection() {
    // Goal: an infinite saved average remains refused beside an absent sibling.
    // Method: decode real Parquet and compare the independent legacy numeric projection.
    assert_invalid_saved_load(f64::INFINITY, SavedReader::Legacy);
}

#[test]
fn admitted_infinity_beside_a_null_load_average_refuses_complete_projection() {
    // Goal: an infinite saved average remains refused through admitted decoding.
    // Method: decode admitted Parquet and compare the independent legacy numeric projection.
    assert_invalid_saved_load(f64::INFINITY, SavedReader::Admitted);
}

fn assert_finite_saved_load(reader: SavedReader) {
    let fixture = crate::recording::sealed_system_minute();
    let marker = saved_marker(&fixture);
    let frame = replace_saved_load(&fixture.root, &marker, 0.5);
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let connection = crate::data::connection();
    connection.register("changed_ticks", frame).unwrap();
    let old = crate::data::query(
        &connection,
        "SELECT load_average_1, load_average_5, load_average_15 FROM changed_ticks",
        3,
        &operation,
    )
    .unwrap();

    let machine = read_saved_load(&fixture.root, &marker, &budget, &operation, reader);
    assert_eq!(budget.used(), 0);
    let mut readings = crate::store::readings(&machine);
    let previous = readings.next().unwrap();
    let reading = readings.next().unwrap();
    let view = crate::live_raw::View::machine(&reading, "ticks").unwrap();
    assert_eq!(
        view.cell(0, "load_average_1").unwrap().number().unwrap(),
        Some(0.5)
    );
    assert_eq!(
        view.cell(0, "load_average_5").unwrap().number().unwrap(),
        None
    );
    assert_eq!(
        view.cell(0, "load_average_15").unwrap().number().unwrap(),
        Some(1.0)
    );
    let second = MachineSecond::complete_of(&reading, Some(&previous), &budget)
        .unwrap()
        .unwrap();

    assert_eq!(
        old.number(&old.rows[1], "load_average_1").unwrap(),
        Some(0.5)
    );
    assert_eq!(old.number(&old.rows[1], "load_average_5").unwrap(), None);
    assert!(second.complete().unwrap().summary(&second)["load_average"].is_null());
    assert_eq!(second.cpu_percent, Some(75.0));
    drop(second);

    assert_eq!(budget.used(), 0);
}

fn assert_invalid_saved_load(one: f64, reader: SavedReader) {
    let fixture = crate::recording::sealed_system_minute();
    let marker = saved_marker(&fixture);
    let frame = replace_saved_load(&fixture.root, &marker, one);
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let connection = crate::data::connection();
    connection.register("changed_ticks", frame).unwrap();
    let old = crate::data::query(
        &connection,
        "SELECT load_average_1, load_average_5, load_average_15 FROM changed_ticks",
        3,
        &operation,
    );
    assert!(matches!(
        old,
        Err(persistence::Error::Bound("unsupported or nonfinite cell"))
    ));
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));

    let machine = read_saved_load(&fixture.root, &marker, &budget, &operation, reader);
    assert_eq!(budget.used(), 0);
    let mut readings = crate::store::readings(&machine);
    let previous = readings.next().unwrap();
    let reading = readings.next().unwrap();
    let second = MachineSecond::complete_of(&reading, Some(&previous), &budget)
        .unwrap()
        .unwrap();
    assert!(matches!(
        second.complete,
        CompleteMachineState::Refused("system load average")
    ));
    assert_invalid_saved_load_cpu(&second);
    assert_eq!(second.memory.unwrap().total_bytes, 2000);
    assert_eq!(second.power_watts, Some(12.0));
    drop(second);

    assert_eq!(budget.used(), 0);
}

fn assert_invalid_saved_load_cpu(second: &MachineSecond) {
    use crate::store::CpuCounter;
    use collector::machine::CPUTicks;

    assert_eq!(
        second.cpu_counters().unwrap(),
        [
            CpuCounter {
                cpu: 0,
                ticks: CPUTicks {
                    user: 20,
                    system: 21,
                    nice: 22,
                    idle: 23
                },
            },
            CpuCounter {
                cpu: 1,
                ticks: CPUTicks {
                    user: 25,
                    system: 26,
                    nice: 27,
                    idle: 28
                },
            },
        ]
    );
    assert_eq!(second.cpu_percent, Some(75.0));
    assert_eq!(
        second.cpu_interval().unwrap().unwrap().values,
        &[Some(75.0), Some(75.0)]
    );
}

fn saved_marker(fixture: &crate::live::identity_fixture::Fixture) -> std::path::PathBuf {
    persistence::Store::open(&fixture.root)
        .unwrap()
        .marker_path(&persistence::WindowIdentity {
            run: fixture.writer.identity().unwrap(),
            window: 0,
        })
}

fn read_saved_load(
    root: &Path,
    marker: &Path,
    budget: &Arc<ByteBudget>,
    operation: &crate::cancellation::Operation,
    reader: SavedReader,
) -> MachineMinute {
    let source = crate::recording::SealedSource { root, marker };
    let sealed = match reader {
        SavedReader::Legacy => SealedMinute::open(root, marker).unwrap(),
        SavedReader::Admitted => SealedMinute::open_with_inputs_using(
            &source,
            budget,
            operation,
            &AdmittedTable::from_file_budgeted,
        )
        .unwrap(),
    };
    let mut machine = MachineMinute::default();
    read_machine(&sealed, &mut machine).unwrap();
    machine
}

fn replace_saved_load(root: &Path, marker: &Path, one: f64) -> DataFrame {
    let manifest = persistence::read_committed(root, marker).unwrap();
    let path = root.join(manifest.data_directory).join("ticks.parquet");
    let mut frame = persistence::engine::scan(&path)
        .unwrap()
        .with_column(lit(one).alias("load_average_1"))
        .with_column(lit(Null {}).cast(DataType::Float64).alias("load_average_5"))
        .with_column(lit(1.0).alias("load_average_15"))
        .collect()
        .unwrap();
    let replacement = root.join("changed_ticks.parquet");
    let file = collector::recording::private::open_private_file(&replacement, true).unwrap();

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
    std::fs::rename(replacement, path).unwrap();
    frame
}
