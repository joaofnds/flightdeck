use std::cell::Cell;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use polars::polars_utils::compression::ZstdLevel;
use polars::prelude::{Column, DataFrame, ParquetCompression, ParquetWriter, StatisticsOptions};
use polars_parquet_format::PageHeader;
use polars_parquet_format::thrift::protocol::TCompactInputProtocol;

use super::{Engine, PARQUET_SOURCES_MAX, Row};

#[test]
fn decodes_the_admitted_bytes_after_the_source_is_replaced() {
    // Goal: decoding cannot read different bytes from those admission checked.
    // Method: admit a real file, replace it, and read the original value from its owner.
    let fixture = Fixture::new();
    let admitted = super::AdmittedFile::open(&fixture.path, 1024 * 1024).unwrap();
    let replacement = fixture.root.join("replacement.parquet");
    write_counter(&replacement, 19);
    std::fs::rename(&replacement, &fixture.path).unwrap();

    let frame = admitted.read(&["counter"]).unwrap();

    assert_eq!(Row::new(&frame, 0).unwrap().get::<u64>(0).unwrap(), 42);
}

#[test]
fn decodes_the_admitted_bytes_after_the_same_file_is_overwritten() {
    // Goal: retaining an open file cannot substitute for immutable captured contents.
    // Method: overwrite the original inode after admission and read its original value.
    let fixture = Fixture::new();
    let admitted = super::AdmittedFile::open(&fixture.path, 1024 * 1024).unwrap();
    std::fs::write(&fixture.path, b"overwritten in place").unwrap();

    let frame = admitted.read(&["counter"]).unwrap();

    assert_eq!(Row::new(&frame, 0).unwrap().get::<u64>(0).unwrap(), 42);
}

#[test]
fn decodes_separate_projections_from_one_admitted_file_after_removal() {
    // Goal: a minute can reuse its admitted input across independent table projections.
    // Method: remove a two-column file, read both projections, then release their input.
    let fixture = Fixture::new();
    let counter = Column::new("counter".into(), [Some(u64::MAX), None]);
    let other = Column::new("other".into(), [Some(-7i64), Some(0)]);
    let mut stored = DataFrame::new(2, vec![counter.clone(), other.clone()]).unwrap();
    std::fs::remove_file(&fixture.path).unwrap();
    write_frame(&fixture.path, &mut stored);
    let admitted = super::AdmittedFile::open(&fixture.path, 1024 * 1024).unwrap();
    std::fs::remove_file(&fixture.path).unwrap();

    let first = admitted.read(&["counter"]).unwrap();
    let second = admitted.read(&["other"]).unwrap();
    drop(admitted);

    assert_eq!(first, DataFrame::new(2, vec![counter]).unwrap());
    assert_eq!(second, DataFrame::new(2, vec![other]).unwrap());
}

#[test]
fn refuses_a_file_larger_than_its_compressed_byte_allowance() {
    // Goal: an operation cannot retain a file larger than its caller reserved.
    // Method: give admission one byte less than a real saved file needs.
    let fixture = Fixture::new();
    let size = std::fs::metadata(&fixture.path).unwrap().len();

    let result = super::AdmittedFile::open(&fixture.path, size - 1);

    assert!(matches!(
        result,
        Err(crate::Error::Bound("parquet file bytes"))
    ));
}

#[test]
fn refuses_an_invalid_footer_in_owned_bytes_before_decoding() {
    // Goal: owning the compressed file does not bypass admission's framing checks.
    // Method: replace a real file with a framed but oversized footer declaration.
    let fixture = Fixture::new();
    let mut bytes = b"PAR1".to_vec();
    bytes.extend_from_slice(&u32::MAX.to_le_bytes());
    bytes.extend_from_slice(b"PAR1");
    std::fs::write(&fixture.path, bytes).unwrap();

    let result = super::AdmittedFile::open(&fixture.path, 1024 * 1024);

    assert!(matches!(
        result,
        Err(crate::Error::Bound("parquet footer bytes"))
    ));
}

#[test]
fn refuses_duplicate_projection_columns() {
    // Goal: repeated names cannot reach a native DataFrame requiring unique columns.
    // Method: request the same stored counter twice through the admitted owner.
    let fixture = Fixture::new();
    let admitted = super::AdmittedFile::open(&fixture.path, 1024 * 1024).unwrap();

    let result = admitted.read(&["counter", "counter"]);

    assert!(matches!(
        result,
        Err(crate::Error::Bound("duplicate parquet projection column"))
    ));
}

#[test]
fn refuses_projection_names_longer_than_an_admitted_field_name() {
    // Goal: a projection cannot copy names larger than any admitted schema permits.
    // Method: request a name one byte beyond the stored schema's name limit.
    let fixture = Fixture::new();
    let admitted = super::AdmittedFile::open(&fixture.path, 1024 * 1024).unwrap();
    let name = "x".repeat(129);

    let result = admitted.read(&[&name]);

    assert!(matches!(
        result,
        Err(crate::Error::Bound("parquet projection name bytes"))
    ));
}

#[test]
fn decodes_only_the_requested_columns_in_projection_order() {
    // Goal: the byte owner preserves projected values, missing values and requested order.
    // Method: select two columns from a real three-column file and compare the full result.
    let fixture = Fixture::new();
    let counter = Column::new("counter".into(), [Some(u64::MAX), None]);
    let other = Column::new("other".into(), [Some(-7i64), Some(0)]);
    let mut stored = DataFrame::new(
        2,
        vec![
            counter.clone(),
            other.clone(),
            Column::new("unused".into(), [1u64, 2]),
        ],
    )
    .unwrap();
    std::fs::remove_file(&fixture.path).unwrap();
    write_frame(&fixture.path, &mut stored);
    let admitted = super::AdmittedFile::open(&fixture.path, 1024 * 1024).unwrap();

    let actual = admitted.read(&["other", "counter"]).unwrap();

    assert_eq!(actual, DataFrame::new(2, vec![other, counter]).unwrap());
}

#[test]
fn does_not_decode_an_unrequested_damaged_column() {
    // Goal: a projection skips unrequested page bodies rather than decoding then selecting.
    // Method: damage only an omitted column's compressed body; select each column separately.
    let fixture = Fixture::new();
    let mut stored = DataFrame::new(
        1,
        vec![
            Column::new("counter".into(), [42u64]),
            Column::new("damaged".into(), [19u64]),
        ],
    )
    .unwrap();
    std::fs::remove_file(&fixture.path).unwrap();
    write_frame(&fixture.path, &mut stored);
    damage_column_body(&fixture.path, 1);

    let admitted = super::AdmittedFile::open(&fixture.path, 1024 * 1024).unwrap();
    let selected = admitted.read(&["counter"]).unwrap();
    assert_eq!(Row::new(&selected, 0).unwrap().get::<u64>(0).unwrap(), 42);

    let admitted = super::AdmittedFile::open(&fixture.path, 1024 * 1024).unwrap();
    assert!(admitted.read(&["damaged"]).is_err());
}

fn damage_column_body(path: &Path, column: usize) {
    let mut bytes = std::fs::read(path).unwrap();
    let metadata = polars_parquet::parquet::read::read_metadata(&mut Cursor::new(&bytes)).unwrap();
    let extent = metadata.row_groups[0].parquet_columns()[column].byte_range();
    let start = usize::try_from(extent.start).unwrap();
    let end = usize::try_from(extent.end).unwrap();
    let mut cursor = Cursor::new(&bytes[start..end]);
    let header =
        PageHeader::read_from_in_protocol(&mut TCompactInputProtocol::new(&mut cursor, 64 * 1024))
            .unwrap();
    let body = start + usize::try_from(cursor.position()).unwrap();
    let length = usize::try_from(header.compressed_page_size).unwrap();
    assert!(length >= 4);
    bytes[body..body + length].fill(0);
    std::fs::write(path, bytes).unwrap();
}

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    path: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "flightdeck-query-source-{}-{serial}",
            std::process::id()
        ));
        let path = root.join("site=west/day=2026-10-18/source.parquet");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        write_counter(&path, 42);
        Self { root, path }
    }

    fn copy(&self, index: usize) -> PathBuf {
        let path = self.root.join(format!("source-{index}.parquet"));
        assert!(!path.exists());
        std::fs::copy(&self.path, &path).unwrap();
        path
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn write_counter(path: &Path, value: u64) {
    let mut frame = DataFrame::new(1, vec![Column::new("counter".into(), [value])]).unwrap();
    write_frame(path, &mut frame);
}

fn write_frame(path: &Path, frame: &mut DataFrame) {
    ParquetWriter::new(std::fs::File::create_new(path).unwrap())
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

#[test]
fn reads_only_stored_columns_under_hive_shaped_parent_directories() {
    // Goal: directory names cannot synthesize unrecorded partition columns or feature panics.
    // Method: query a real saved source under two key=value parents through production routing.
    let fixture = Fixture::new();
    let engine = Engine::new();

    let source = crate::query::table_source(
        &engine,
        crate::schema::Table::Fans,
        crate::query::TableSource::Parquet(&fixture.path),
    )
    .unwrap();
    let frame = engine.query(&format!("SELECT * FROM {source}")).unwrap();

    assert_eq!(frame.width(), 1);
    assert_eq!(frame.columns()[0].name().as_str(), "counter");
    assert_eq!(Row::new(&frame, 0).unwrap().get::<u64>(0).unwrap(), 42);
}

#[test]
fn keeps_equal_file_names_as_distinct_saved_relations() {
    // Goal: composing files of one table never replaces the preceding file's relation.
    // Method: query two different files named source.parquet in one SQL statement.
    let fixture = Fixture::new();
    let other = fixture.root.join("other/source.parquet");
    std::fs::create_dir(other.parent().unwrap()).unwrap();
    write_counter(&other, 19);
    let engine = Engine::new();
    let first = engine.parquet_source(&fixture.path).unwrap();
    let second = engine.parquet_source(&other).unwrap();

    let frame = engine
        .query(&format!(
            "SELECT * FROM {first} UNION ALL SELECT * FROM {second} ORDER BY counter"
        ))
        .unwrap();

    assert_ne!(first, second);
    assert_eq!(frame.height(), 2);
    assert_eq!(Row::new(&frame, 0).unwrap().get::<u64>(0).unwrap(), 19);
    assert_eq!(Row::new(&frame, 1).unwrap().get::<u64>(0).unwrap(), 42);
}

#[test]
fn rebinds_an_unchanged_source_with_the_same_alias_after_cache_eviction() {
    // Goal: a bounded cache can restore SQL retained by a longer viewing operation.
    // Method: evict one real source, then request the unchanged file and execute its old SQL.
    let fixture = Fixture::new();
    let engine = Engine::new();
    let original = engine.parquet_source(&fixture.path).unwrap();
    let sql = format!("SELECT * FROM {original}");
    for index in 0..PARQUET_SOURCES_MAX {
        engine.parquet_source(&fixture.copy(index)).unwrap();
    }
    assert!(engine.query(&sql).is_err());

    let rebound = engine.parquet_source(&fixture.path).unwrap();
    let frame = engine.query(&sql).unwrap();

    assert_eq!(rebound, original);
    assert_eq!(Row::new(&frame, 0).unwrap().get::<u64>(0).unwrap(), 42);
}

#[test]
fn retains_the_most_recently_used_source_when_the_cache_reaches_its_bound() {
    // Goal: cache replacement evicts the least recently used relation within a fixed limit.
    // Method: fill 128 entries, reuse the first, add one, then check the oldest untouched alias.
    let fixture = Fixture::new();
    let engine = Engine::new();
    let first = engine.parquet_source(&fixture.path).unwrap();
    let oldest = engine.parquet_source(&fixture.copy(0)).unwrap();
    for index in 1..PARQUET_SOURCES_MAX - 1 {
        engine.parquet_source(&fixture.copy(index)).unwrap();
    }
    engine.parquet_source(&fixture.path).unwrap();

    engine
        .parquet_source(&fixture.copy(PARQUET_SOURCES_MAX))
        .unwrap();

    assert!(engine.query(&format!("SELECT * FROM {first}")).is_ok());
    assert!(engine.query(&format!("SELECT * FROM {oldest}")).is_err());
}

#[test]
fn changes_the_source_alias_when_a_file_is_replaced() {
    // Goal: cached admission cannot silently reuse a different file at the same path.
    // Method: replace a saved file with another inode and request its relation again.
    let fixture = Fixture::new();
    let engine = Engine::new();
    let first = engine.parquet_source(&fixture.path).unwrap();
    let replacement = fixture.root.join("replacement.parquet");
    write_counter(&replacement, 19);
    std::fs::rename(&replacement, &fixture.path).unwrap();

    let second = engine.parquet_source(&fixture.path).unwrap();
    let frame = engine.query(&format!("SELECT * FROM {second}")).unwrap();

    assert_ne!(first, second);
    assert_eq!(Row::new(&frame, 0).unwrap().get::<u64>(0).unwrap(), 19);
}

#[test]
fn shares_one_immutable_registration_between_hard_links() {
    // Goal: evicting one name's entry cannot unregister the relation a hard link still uses.
    // Method: request two hard links, fill the rest of the cache, then query the second link.
    let fixture = Fixture::new();
    let second = fixture.root.join("linked.parquet");
    std::fs::hard_link(&fixture.path, &second).unwrap();
    let engine = Engine::new();
    let first = engine.parquet_source(&fixture.path).unwrap();
    let linked = engine.parquet_source(&second).unwrap();
    for index in 0..PARQUET_SOURCES_MAX - 1 {
        engine.parquet_source(&fixture.copy(index)).unwrap();
    }

    let retained = engine.parquet_source(&second).unwrap();
    let frame = engine.query(&format!("SELECT * FROM {retained}")).unwrap();

    assert_eq!(first, linked);
    assert_eq!(retained, linked);
    assert_eq!(Row::new(&frame, 0).unwrap().get::<u64>(0).unwrap(), 42);
}

#[test]
fn refuses_a_redirected_plan_path_when_another_name_retains_the_original_file() {
    // Goal: a fingerprint cache hit cannot use a native plan whose first path was redirected.
    // Method: register a symlink, redirect it while its old target stays, then use that target.
    let fixture = Fixture::new();
    let redirected = fixture.root.join("redirected.parquet");
    std::os::unix::fs::symlink(&fixture.path, &redirected).unwrap();
    let replacement = fixture.root.join("other.parquet");
    write_counter(&replacement, 19);
    let engine = Engine::new();
    engine.parquet_source(&redirected).unwrap();
    std::fs::remove_file(&redirected).unwrap();
    std::os::unix::fs::symlink(&replacement, &redirected).unwrap();

    let result = engine.parquet_source(&fixture.path);

    assert!(matches!(
        result,
        Err(crate::Error::Bound("changed cached query source"))
    ));
}

#[test]
fn derives_a_relation_once_while_its_unchanged_file_is_retained() {
    // Goal: repeated reads of an immutable saved file reuse its validated relation.
    // Method: request one file's derived relation twice and count the derivations.
    let fixture = Fixture::new();
    let engine = Engine::new();
    let derivations = Cell::new(0);
    let derive = |_: &Engine, source: &str| {
        derivations.set(derivations.get() + 1);
        Ok(format!("(SELECT counter + 1 AS counter FROM {source})"))
    };

    let first = engine.parquet_relation(&fixture.path, "counters", derive);
    let second = engine.parquet_relation(&fixture.path, "counters", derive);

    assert_eq!(derivations.get(), 1);
    assert_eq!(counter(&engine, &second.unwrap()), 43);
    assert_eq!(counter(&engine, &first.unwrap()), 43);
}

#[test]
fn derives_the_relation_again_when_a_file_is_replaced() {
    // Goal: validation of an earlier file never stands in for a different file at its path.
    // Method: derive a relation, replace the file with another inode, then request it again.
    let fixture = Fixture::new();
    let engine = Engine::new();
    let derivations = Cell::new(0);
    let derive = |_: &Engine, source: &str| {
        derivations.set(derivations.get() + 1);
        Ok(source.to_owned())
    };
    engine
        .parquet_relation(&fixture.path, "counters", derive)
        .unwrap();
    let replacement = fixture.root.join("replacement.parquet");
    write_counter(&replacement, 19);
    std::fs::rename(&replacement, &fixture.path).unwrap();

    let relation = engine
        .parquet_relation(&fixture.path, "counters", derive)
        .unwrap();

    assert_eq!(derivations.get(), 2);
    assert_eq!(counter(&engine, &relation), 19);
}

#[test]
fn derives_the_relation_again_for_another_table_kind() {
    // Goal: one table's validation never stands in for another table read from the same file.
    // Method: derive relations for two kinds from one file and count the derivations.
    let fixture = Fixture::new();
    let engine = Engine::new();
    let derivations = Cell::new(0);
    let derive = |_: &Engine, source: &str| {
        derivations.set(derivations.get() + 1);
        Ok(source.to_owned())
    };

    engine
        .parquet_relation(&fixture.path, "counters", derive)
        .unwrap();
    engine
        .parquet_relation(&fixture.path, "gauges", derive)
        .unwrap();

    assert_eq!(derivations.get(), 2);
}

#[test]
fn derives_the_relation_again_after_a_refused_derivation() {
    // Goal: a refused file is refused on every read rather than cached as accepted.
    // Method: refuse the first derivation, then request the same unchanged file again.
    let fixture = Fixture::new();
    let engine = Engine::new();
    let derivations = Cell::new(0);
    let derive = |_: &Engine, source: &str| {
        derivations.set(derivations.get() + 1);
        if derivations.get() == 1 {
            return Err(crate::Error::Bound("refused fixture"));
        }
        Ok(source.to_owned())
    };
    let refused = engine.parquet_relation(&fixture.path, "counters", derive);

    let accepted = engine.parquet_relation(&fixture.path, "counters", derive);

    assert!(matches!(
        refused,
        Err(crate::Error::Bound("refused fixture"))
    ));
    assert_eq!(derivations.get(), 2);
    assert_eq!(counter(&engine, &accepted.unwrap()), 42);
}

#[test]
fn derives_the_relation_again_after_its_derivation_evicts_the_file() {
    // Goal: a relation naming an alias evicted while it was derived is never reused.
    // Method: evict the file during its first derivation, then request the file twice more.
    let fixture = Fixture::new();
    let engine = Engine::new();
    let derivations = Cell::new(0);
    let derive = |engine: &Engine, source: &str| {
        derivations.set(derivations.get() + 1);
        if derivations.get() == 1 {
            for index in 0..PARQUET_SOURCES_MAX {
                engine.parquet_source(&fixture.copy(index))?;
            }
        }
        Ok(format!("(SELECT counter FROM {source})"))
    };
    let evicted = engine
        .parquet_relation(&fixture.path, "counters", derive)
        .unwrap();

    let rederived = engine.parquet_relation(&fixture.path, "counters", derive);
    let retained = engine.parquet_relation(&fixture.path, "counters", derive);

    assert!(engine.query(&format!("SELECT * FROM {evicted}")).is_ok());
    assert_eq!(derivations.get(), 2);
    assert_eq!(counter(&engine, &rederived.unwrap()), 42);
    assert_eq!(counter(&engine, &retained.unwrap()), 42);
}

fn counter(engine: &Engine, relation: &str) -> u64 {
    let frame = engine
        .query(&format!("SELECT counter FROM {relation}"))
        .unwrap();
    Row::new(&frame, 0).unwrap().get::<u64>(0).unwrap()
}
