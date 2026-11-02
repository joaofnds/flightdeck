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
fn underreported_expansion_is_refused_before_native_decode() {
    // Goal: a footer cannot understate the measured page expansion used by decode preflight.
    // Method: change only one real column's declared total while preserving its page bytes.
    use polars_parquet_format::thrift::protocol::TCompactOutputProtocol;
    let fixture = Fixture::new();
    let mut bytes = std::fs::read(&fixture.path).unwrap();
    let mut metadata = super::admission::validate_reader_metadata(
        &mut std::io::Cursor::new(&bytes),
        u64::try_from(bytes.len()).unwrap(),
    )
    .unwrap();
    metadata.row_groups[0].columns[0]
        .meta_data
        .as_mut()
        .unwrap()
        .total_uncompressed_size -= 1;
    let mut footer = Vec::new();
    metadata
        .write_to_out_protocol(&mut TCompactOutputProtocol::new(&mut footer))
        .unwrap();
    let trailer = bytes.len() - 8;
    let length = u32::from_le_bytes(bytes[trailer..trailer + 4].try_into().unwrap());
    bytes.truncate(trailer - usize::try_from(length).unwrap());
    bytes.extend_from_slice(&footer);
    bytes.extend_from_slice(&u32::try_from(footer.len()).unwrap().to_le_bytes());
    bytes.extend_from_slice(b"PAR1");
    std::fs::write(&fixture.path, bytes).unwrap();
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let result =
        AdmittedTable::from_file_budgeted_profiled(File::open(&fixture.path).unwrap(), &budget);
    assert!(matches!(
        result,
        Err(AdmissionRefusal::Bound("parquet declared expanded bytes"))
    ));
    assert_eq!(budget.used(), 0);
}

#[test]
fn profiled_slice_reserves_only_whole_intersecting_groups_before_decode() {
    // Goal: legacy slices charge their native row-group payload before values are decoded.
    // Method: select either or both real groups, then exhaust exactly one byte of work room.
    let fixture = Fixture::new();
    let mut frame = polars::prelude::DataFrame::new(
        124,
        vec![polars::prelude::Column::new(
            "counter".into(),
            (0_u64..124).collect::<Vec<_>>(),
        )],
    )
    .unwrap();
    std::fs::remove_file(&fixture.path).unwrap();
    write_groups(&fixture.path, &mut frame);
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let table =
        AdmittedTable::from_file_budgeted_profiled(File::open(&fixture.path).unwrap(), &budget)
            .unwrap();
    let first = table.reserve_slice_work(0, 1).unwrap();
    let second = table.reserve_slice_work(62, 1).unwrap();
    let both = table.reserve_slice_work(61, 2).unwrap();
    assert_eq!(both.bytes(), first.bytes() + second.bytes());
    let required = first.bytes();
    assert!(required > 0);
    drop((first, second, both));
    let absent = table.reserve_slice_work(124, 1).unwrap();
    assert_eq!(absent.bytes(), 0);
    drop(absent);
    let pressure = budget
        .reserve(budget.maximum() - budget.used() - required + 1)
        .unwrap();
    assert!(matches!(
        table.reserve_slice_work(0, 1),
        Err(AdmissionRefusal::Budget)
    ));
    drop(pressure);
    let work = table.reserve_slice_work(0, 1).unwrap();
    assert_eq!(
        table
            .read_slice_metadata_budgeted(&["counter"], 0, 1)
            .unwrap()
            .height(),
        1
    );
    drop(work);
    drop(table);
    assert_eq!(budget.used(), 0);
}

#[test]
fn strict_expansion_refusal_precedes_native_decode_and_releases_every_charge() {
    // Goal: a terminal batch cannot enter native decoding above its independently smaller cap.
    // Method: admit exact fixture bounds, then lower expanded bytes by one with the same bytes.
    let fixture = Fixture::new();
    let mut file = File::open(&fixture.path).unwrap();
    let size = file.metadata().unwrap().len();
    let metadata = super::admission::validate_reader_metadata(&mut file, size).unwrap();
    let expanded = metadata
        .row_groups
        .iter()
        .flat_map(|group| &group.columns)
        .map(|column| {
            u64::try_from(column.meta_data.as_ref().unwrap().total_uncompressed_size).unwrap()
        })
        .sum();
    let limits = super::DecodeLimits {
        compressed_bytes: size,
        expanded_bytes: expanded,
        rows: 1,
        row_groups: 1,
        columns: 1,
    };
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));

    let table = AdmittedTable::from_file_budgeted_with_limits(
        File::open(&fixture.path).unwrap(),
        &budget,
        &limits,
    )
    .unwrap();
    assert_eq!(table.read(&["counter"], Some(1)).unwrap().height(), 1);
    drop(table);
    assert_eq!(budget.used(), 0);
    let refused = AdmittedTable::from_file_budgeted_with_limits(
        File::open(&fixture.path).unwrap(),
        &budget,
        &super::DecodeLimits {
            expanded_bytes: expanded - 1,
            ..limits
        },
    );

    assert!(matches!(
        refused,
        Err(AdmissionRefusal::Bound("strict parquet expanded bytes"))
    ));
    assert_eq!(budget.used(), 0);
}

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
    assert_eq!(budget.used(), size + 130);
    assert_eq!(
        allocations.allocated_bytes - allocations.deallocated_bytes,
        size + 130
    );
    assert!(allocations.layouts[..allocations.allocations].contains(&(130, 1)));
    let ((), frees) = observe(&budget, || drop(table));
    assert_eq!(frees.deallocated_bytes, size + 130);
    assert!(frees.deallocation_charge_min >= 130);
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
    let allowed = Arc::new(ByteBudget::new(130));
    let types = super::admission::storage_types(&metadata).unwrap();

    let (result, refusal) = observe(&denied, || {
        super::ColumnNames::capture(&metadata.schema[1..], &types, &denied)
    });
    let (names, granted) = observe(&allowed, || {
        super::ColumnNames::capture(&metadata.schema[1..], &types, &allowed)
    });

    assert!(matches!(result, Err(AdmissionRefusal::Budget)));
    assert_eq!(refusal.allocations, 0);
    assert_eq!(&granted.layouts[..granted.allocations], &[(130, 1)]);
    assert_eq!(allowed.used(), 130);
    drop(names.unwrap());
    assert_eq!(allowed.used(), 0);
}
