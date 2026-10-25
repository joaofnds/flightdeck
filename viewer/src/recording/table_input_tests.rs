use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use persistence::engine::AdmittedTable;
use polars::prelude::{DataFrame, ParquetReader, SerReader, col, lit};

use super::{AppMinute, AppParts, ScopedSealedMinute, SealedMinute, SealedSource, read_apps};
use crate::cancellation::Cancellation;
use crate::live::identity_fixture::Fixture;
use crate::store::ByteBudget;

#[test]
fn sealed_minute_preserves_legacy_send_and_sync() {
    // Goal: adding a borrowed capture factory preserves the legacy minute's auto traits.
    // Method: require both the exact baseline fields and the complete current type to qualify.
    fn require_send_sync<T: Send + Sync>() {}
    struct Holder {
        minute: SealedMinute,
    }

    require_send_sync::<(
        std::path::PathBuf,
        std::path::PathBuf,
        persistence::Manifest,
        Option<super::ticks_input::TicksInput>,
    )>();
    require_send_sync::<SealedMinute>();
    require_send_sync::<ScopedSealedMinute<'static>>();
    require_send_sync::<Holder>();
    let fixture = parts_fixture();
    let holder = Holder {
        minute: SealedMinute::open(&fixture.root, &parts_marker(&fixture)).unwrap(),
    };
    drop(holder.minute);
}

#[test]
fn admitted_app_parts_reuse_one_source_for_version_and_payload_after_replacement() {
    // Goal: both App-parts queries consume the original immutable input.
    // Method: replace a private compatible table after capture and compare every typed value.
    let fixture = parts_fixture();
    let marker = parts_marker(&fixture);
    let source = SealedSource {
        root: &fixture.root,
        marker: &marker,
    };
    let path = parts_path(&source);
    let legacy = SealedMinute::open(source.root, source.marker).unwrap();
    let mut expected = AppMinute::default();
    assert_eq!(read_apps(&legacy, &mut expected).unwrap(), AppParts::Read);
    let mut changed = persistence::engine::scan(&path)
        .unwrap()
        .with_column((col("wall_ns") + lit(9_u64)).alias("wall_ns"))
        .collect()
        .unwrap();
    let replacement = fixture.root.join("parts-replacement.parquet");
    parts_write(&replacement, &mut changed);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let captures = AtomicUsize::new(0);
    let capture = |file, budget: &Arc<ByteBudget>| {
        let input = AdmittedTable::from_file_budgeted(file, budget)?;
        if input.contains_column("version") {
            captures.fetch_add(1, Ordering::Relaxed);
            std::fs::rename(&replacement, &path).unwrap();
        }

        Ok(input)
    };
    let minute =
        ScopedSealedMinute::open_with_inputs_using(&source, &budget, &operation, &capture).unwrap();
    let mut actual = AppMinute::default();

    let parts = read_apps(&minute, &mut actual).unwrap();

    assert_eq!(parts, AppParts::Read);
    assert_eq!(actual, expected);
    assert_eq!(actual.samples.len(), 2);
    assert_eq!(actual.samples[0].publication, 1);
    assert_eq!(actual.samples[1].publication, 2);
    assert_eq!(actual.samples[0].wall_ns, Some(100_000_000_000));
    assert_eq!(actual.samples[1].wall_ns, Some(100_000_000_000));
    minute.revalidate_full().unwrap();
    drop(minute);
    assert_eq!(captures.load(Ordering::Relaxed), 1);
    assert_eq!(budget.used(), 0);
}

#[test]
fn incompatible_app_parts_version_skips_a_key_page_that_cannot_decode() {
    // Goal: version fallback must not eagerly decode unused key payloads.
    // Method: corrupt compressed key bytes while preserving admitted headers and metadata.
    let fixture = parts_fixture();
    let marker = parts_marker(&fixture);
    let source = SealedSource {
        root: &fixture.root,
        marker: &marker,
    };
    let path = parts_path(&source);
    let version = persistence::app_tables::PARTS_VERSION + 1;
    let mut changed = persistence::engine::scan(&path)
        .unwrap()
        .with_column(
            lit(version)
                .cast(polars::prelude::DataType::UInt8)
                .alias("version"),
        )
        .collect()
        .unwrap();
    let key = changed.get_column_index("app_key").unwrap();
    std::fs::remove_file(&path).unwrap();
    parts_write(&path, &mut changed);
    parts_damage_key(&path, key);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let input =
        AdmittedTable::from_file_budgeted(std::fs::File::open(&path).unwrap(), &budget).unwrap();
    let (plan, names) = input.into_scan().unwrap();
    assert!(plan.select([col("app_key")]).collect().is_err());
    drop(names);
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let capture = AdmittedTable::from_file_budgeted;
    let minute =
        ScopedSealedMinute::open_with_inputs_using(&source, &budget, &operation, &capture).unwrap();
    let mut actual = AppMinute::default();

    let parts = read_apps(&minute, &mut actual).unwrap();

    assert_eq!(parts, AppParts::Version(version));
    assert_eq!(actual, AppMinute::default());
    minute.revalidate_full().unwrap();
    drop(minute);
    assert_eq!(budget.used(), 0);
}

#[test]
fn cancellation_inside_native_collection_discards_its_successful_result() {
    // Goal: native work that finishes after cancellation cannot return stale rows.
    // Method: cancel from a native map over a real admitted table, after it has decoded.
    let fixture = parts_fixture();
    let marker = parts_marker(&fixture);
    let source = SealedSource {
        root: &fixture.root,
        marker: &marker,
    };
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let cancellation = Arc::new(Cancellation::default());
    let operation = Arc::clone(&cancellation).operation(0).unwrap();
    let capture = AdmittedTable::from_file_budgeted;
    let minute =
        ScopedSealedMinute::open_with_inputs_using(&source, &budget, &operation, &capture).unwrap();
    let mut table = minute.table(persistence::schema::Table::Processes).unwrap();
    table.plan = table.plan.map(
        move |frame| {
            cancellation.cancel().unwrap();
            Ok(frame)
        },
        polars::prelude::AllowedOptimizations::empty(),
        None,
        Some("cancel_after_decode"),
    );

    let result = table.collect_plan();

    assert!(matches!(result,
        Err(super::Refused::Decode(persistence::Error::Io(error)))
        if error.kind() == std::io::ErrorKind::Interrupted));
    drop(minute);
    assert_eq!(budget.used(), 0);
}

pub(super) fn parts_fixture() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(100);
    fixture.seal_derived(&[], &persistence::sealed_apps::SealedApps::default());

    fixture
}

fn parts_marker(fixture: &Fixture) -> std::path::PathBuf {
    persistence::layout::WindowKey {
        run: fixture.writer.identity().unwrap().run,
        window: 0,
    }
    .marker(&fixture.root)
}

fn parts_path(source: &SealedSource<'_>) -> std::path::PathBuf {
    let manifest = persistence::read_committed(source.root, source.marker).unwrap();

    source
        .root
        .join(manifest.data_directory)
        .join("app_parts.parquet")
}

fn parts_damage_key(path: &Path, key: usize) {
    let mut reader = ParquetReader::new(std::fs::File::open(path).unwrap());
    let metadata = reader.get_metadata().unwrap();
    let range = metadata.row_groups[0].parquet_columns()[key].byte_range();
    let start = usize::try_from(range.start).unwrap();
    let end = usize::try_from(range.end).unwrap();
    let mut bytes = std::fs::read(path).unwrap();
    let compressed = bytes[start..end]
        .windows(4)
        .position(|word| word == [0x28, 0xb5, 0x2f, 0xfd])
        .unwrap();
    bytes[start + compressed..start + compressed + 4].fill(0);
    std::fs::write(path, bytes).unwrap();
}

fn parts_write(path: &Path, frame: &mut DataFrame) {
    use polars::polars_utils::compression::ZstdLevel;
    use polars::prelude::{ParquetCompression, ParquetWriter, StatisticsOptions};

    let file = collector::recording::private::open_private_file(path, true).unwrap();

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
        .finish(frame)
        .unwrap();
}
