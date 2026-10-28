use std::cell::Cell;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::sync::Arc;

use super::AdmittedTable;
use crate::allocation_tests::observe;
use crate::engine::AdmissionRefusal;
use crate::engine::source_tests::Fixture;
use crate::resources::ByteBudget;

#[test]
fn denied_capture_calls_neither_read_nor_seek_on_a_real_file() {
    // Goal: read/seek observation remains distinct from final position and layout observations.
    // Method: forward both traits to a real file through the constructor's actual capture helper.
    let fixture = Fixture::new();
    let file = File::open(&fixture.path).unwrap();
    let size = file.metadata().unwrap().len();
    let reads = Cell::new(0);
    let seeks = Cell::new(0);
    let observed = ObservedFile {
        file,
        reads: &reads,
        seeks: &seeks,
    };
    let budget = Arc::new(ByteBudget::new(0));

    let result = AdmittedTable::from_reader_budgeted(observed, size, &budget);

    assert!(matches!(result, Err(AdmissionRefusal::Budget)));
    assert_eq!(reads.get(), 0);
    assert_eq!(seeks.get(), 0);
}

struct ObservedFile<'a> {
    file: File,
    reads: &'a Cell<u64>,
    seeks: &'a Cell<u64>,
}

impl Read for ObservedFile<'_> {
    fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
        self.reads.set(self.reads.get() + 1);
        self.file.read(bytes)
    }
}

impl Seek for ObservedFile<'_> {
    fn seek(&mut self, position: SeekFrom) -> std::io::Result<u64> {
        self.seeks.set(self.seeks.get() + 1);
        self.file.seek(position)
    }
}

#[test]
fn prefix_does_not_decode_a_damaged_later_selected_row_group() {
    // Goal: a prefix reaches native decoding before any selected later page is materialized.
    // Method: damage row group two of a real two-group file, then compare prefix/full reads.
    let fixture = Fixture::new();
    let values: Vec<u64> = (0..124).collect();
    let mut frame = polars::prelude::DataFrame::new(
        124,
        vec![polars::prelude::Column::new("counter".into(), values)],
    )
    .unwrap();
    std::fs::remove_file(&fixture.path).unwrap();
    write_groups(&fixture.path, &mut frame);
    damage_second_group(&fixture.path);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let table =
        AdmittedTable::from_file_budgeted(File::open(&fixture.path).unwrap(), &budget).unwrap();

    assert_eq!(table.row_count(), 124);
    let prefix = table.read(&["counter"], Some(62)).unwrap();
    let full = table.read(&["counter"], None);

    assert_eq!(table.row_count(), 124);
    assert_eq!(prefix.height(), 62);
    assert_eq!(
        crate::engine::Row::new(&prefix, 61)
            .unwrap()
            .get::<u64>(0)
            .unwrap(),
        61
    );
    assert!(full.is_err());
}

fn write_groups(path: &std::path::Path, frame: &mut polars::prelude::DataFrame) {
    use polars::polars_utils::compression::ZstdLevel;
    use polars::prelude::{ParquetCompression, ParquetWriter, StatisticsOptions};
    ParquetWriter::new(File::create_new(path).unwrap())
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
        .with_row_group_size(Some(62))
        .with_data_page_size(None)
        .set_parallel(false)
        .with_key_value_metadata(None)
        .with_context_info(None)
        .finish(frame)
        .unwrap();
}

fn damage_second_group(path: &std::path::Path) {
    use polars_parquet_format::PageHeader;
    use polars_parquet_format::thrift::protocol::TCompactInputProtocol;
    use std::io::Cursor;
    let mut bytes = std::fs::read(path).unwrap();
    let metadata = polars_parquet::parquet::read::read_metadata(&mut Cursor::new(&bytes)).unwrap();
    assert_eq!(metadata.row_groups.len(), 2);
    let extent = metadata.row_groups[1].parquet_columns()[0].byte_range();
    let start = usize::try_from(extent.start).unwrap();
    let end = usize::try_from(extent.end).unwrap();
    let mut cursor = Cursor::new(&bytes[start..end]);
    let header =
        PageHeader::read_from_in_protocol(&mut TCompactInputProtocol::new(&mut cursor, 64 * 1024))
            .unwrap();
    let body = start + usize::try_from(cursor.position()).unwrap();
    let length = usize::try_from(header.compressed_page_size).unwrap();
    assert!(length > 4);
    bytes[body..body + length].fill(0);
    std::fs::write(path, bytes).unwrap();
}

#[test]
fn captures_from_zero_and_retains_exact_input_and_column_storage_until_drop() {
    // Goal: names and input remain charged independently of decoder results.
    // Method: start a real file at offset seven, observe capture, decode, then drop its owner.
    let fixture = Fixture::new();
    let mut file = File::open(&fixture.path).unwrap();
    file.seek(SeekFrom::Start(7)).unwrap();
    let size = usize::try_from(file.metadata().unwrap().len()).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));

    let (result, allocations) =
        observe(&budget, || AdmittedTable::from_file_budgeted(file, &budget));
    let table = result.unwrap();
    let frame = table.read(&["counter"], Some(1)).unwrap();

    assert!(table.contains_column("counter"));
    assert!(!table.contains_column("missing"));
    assert_eq!(budget.used(), size + 129);
    assert_eq!(
        allocations.allocated_bytes - allocations.deallocated_bytes,
        size + 129
    );
    assert!(allocations.layouts[..allocations.allocations].contains(&(129, 1)));
    let ((), frees) = observe(&budget, || drop(table));
    assert_eq!(frees.deallocated_bytes, size + 129);
    assert!(frees.deallocation_charge_min >= 129);
    assert_eq!(budget.used(), 0);
    assert_eq!(
        crate::engine::Row::new(&frame, 0)
            .unwrap()
            .get::<u64>(0)
            .unwrap(),
        42
    );
}

#[test]
fn denial_preserves_a_nonzero_file_position_without_allocating_or_reading() {
    // Goal: denial precedes seeking, content reads and compressed allocation.
    // Method: share a real descriptor at offset seven and observe an exhausted budget.
    let fixture = Fixture::new();
    let mut file = File::open(&fixture.path).unwrap();
    file.seek(SeekFrom::Start(7)).unwrap();
    let mut observed = file.try_clone().unwrap();
    let budget = Arc::new(ByteBudget::new(1024));
    let retained = budget.reserve(1024).unwrap();

    let (result, allocations) =
        observe(&budget, || AdmittedTable::from_file_budgeted(file, &budget));

    assert!(matches!(result, Err(AdmissionRefusal::Budget)));
    assert_eq!(observed.stream_position().unwrap(), 7);
    assert_eq!(allocations.allocations, 0);
    assert_eq!(budget.used(), retained.bytes());
    let mut next = [0];
    observed.read_exact(&mut next).unwrap();
    assert_eq!(observed.stream_position().unwrap(), 8);
}

#[test]
fn refuses_name_storage_before_its_allocation_and_releases_parser_input_owners() {
    // Goal: schema names cannot consume the parser allowance after it is released.
    // Method: allow exactly scratch plus input, leaving no room for the known name layout.
    let fixture = Fixture::new();
    let file = File::open(&fixture.path).unwrap();
    let size = usize::try_from(file.metadata().unwrap().len()).unwrap();
    let maximum = size + super::super::admission::cursor_scratch_bytes().unwrap();
    let budget = Arc::new(ByteBudget::new(maximum));

    let (result, allocations) =
        observe(&budget, || AdmittedTable::from_file_budgeted(file, &budget));

    assert!(matches!(result, Err(AdmissionRefusal::Budget)));
    assert_eq!(allocations.allocated_bytes, allocations.deallocated_bytes);
    assert_eq!(allocations.deallocation_charge_min, maximum);
    assert_eq!(budget.used(), 0);
}

#[test]
fn name_owner_denial_makes_no_backing_request_after_validated_metadata_setup() {
    // Goal: denial refuses the corresponding name request without confusing parser layouts.
    // Method: complete real metadata validation first, then observe only name ownership.
    let fixture = Fixture::new();
    let bytes = std::fs::read(&fixture.path).unwrap();
    let metadata = super::super::admission::validate_reader_metadata(
        &mut std::io::Cursor::new(&bytes),
        u64::try_from(bytes.len()).unwrap(),
    )
    .unwrap();
    let denied = Arc::new(ByteBudget::new(0));
    let allowed = Arc::new(ByteBudget::new(129));

    let (result, refusal) = observe(&denied, || {
        super::ColumnNames::capture(&metadata.schema[1..], &denied)
    });
    let (names, granted) = observe(&allowed, || {
        super::ColumnNames::capture(&metadata.schema[1..], &allowed)
    });

    assert!(matches!(result, Err(AdmissionRefusal::Budget)));
    assert_eq!(refusal.allocations, 0);
    assert_eq!(&granted.layouts[..granted.allocations], &[(129, 1)]);
    assert_eq!(allowed.used(), 129);
    drop(names.unwrap());
    assert_eq!(allowed.used(), 0);
}
