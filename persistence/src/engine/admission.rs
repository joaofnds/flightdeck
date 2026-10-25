//! Admission reads bounded native Thrift metadata and page headers without decoding values.

mod protocol;
mod schema;

#[cfg(test)]
mod arrow_tests;
#[cfg(test)]
mod parser_tests;
#[cfg(test)]
mod tests;

use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom};
use std::path::Path;

use polars_parquet::parquet::metadata::ThriftFileMetadata;
use polars_parquet::parquet::page::ParquetPageHeader;
use polars_parquet_format::{
    ColumnChunk, ColumnMetaData, Encoding, FieldRepetitionType, PageType, RowGroup, SchemaElement,
    Type,
};

use crate::{Error, Result};
use protocol::BoundedProtocol;

pub(super) const FILE_BYTES_MAX: u64 = 512 * 1024 * 1024;
const FOOTER_BYTES_MAX: u64 = 4 * 1024 * 1024;
const PAGE_BYTES_MAX: u64 = 64 * 1024 * 1024;
const PAGE_HEADER_BYTES_MAX: u64 = 64 * 1024;
/// What a file may expand to once its pages are uncompressed, and what decoding them may take.
pub const EXPANDED_BYTES_MAX: u64 = 512 * 1024 * 1024;
const ROWS_MAX: u64 = 8_388_608;
/// Above the widest table, the App parts a window derives as it seals, which asserts it fits.
pub const COLUMNS_MAX: usize = 96;
pub(super) const COLUMN_NAME_BYTES_MAX: usize = 128;
const ROW_GROUPS_MAX: usize = 2048;
const PAGES_MAX: u64 = 65_536;

const _: () = {
    assert!(PAGE_HEADER_BYTES_MAX <= FOOTER_BYTES_MAX);
    assert!(PAGE_BYTES_MAX <= EXPANDED_BYTES_MAX);
    assert!(FOOTER_BYTES_MAX < FILE_BYTES_MAX);
    assert!(ROWS_MAX < 1u64 << 31);
    assert!(size_of::<polars_parquet_format::thrift::Error>() <= 40);
    assert!(size_of::<std::collections::TryReserveError>() <= 40);
};

struct Extent {
    start: u64,
    end: u64,
}

#[derive(Default)]
struct Totals {
    pages: u64,
    expanded_bytes: u64,
    decoded_bytes: u64,
}

#[derive(Default)]
struct PageMeasurements {
    values: u64,
    expanded_bytes: u64,
}

/// Requested layouts for the captured Cursor, not allocator overhead, stack or decoding.
/// Sources: parquet-format's `thrift/protocol/compact.rs`, Rust's `alloc/raw_vec` and
/// `io::default_read_to_end`, and Base64's `engine/mod.rs`. The certificate tests pin
/// Rust 1.98.0/aarch64-apple-darwin, Polars 22a147d, parquet-format 0.1.0 and Base64 0.22.1.
/// Recheck these source assumptions when changing the supported build or parser path.
pub(super) fn cursor_scratch_bytes() -> Result<usize> {
    let footer =
        usize::try_from(FOOTER_BYTES_MAX).map_err(|_| Error::Bound("parquet footer bytes"))?;
    let header =
        usize::try_from(PAGE_HEADER_BYTES_MAX).map_err(|_| Error::Bound("parquet page header"))?;
    let metadata = protocol::VECTOR_BYTES_MAX + 8 * footer;
    // Fresh native byte vectors request max(8, n) <= 8n; typed lists reserve exactly.
    // Take(n) and read_to_end's EOF probe prevent later growth, including on short input.
    // The cumulative ledgers cover replaced fields; footer metadata survives every phase.
    // Base64 is sequential, at most 3*ceil(F/4); only one page header is live at once.
    let arrow = metadata + 3 * footer.div_ceil(4);
    // The native i16 stack overlaps 16+32 bytes during growth. Messages have L<=88;
    // old/new formatting layouts total <=3L, plus a 19-byte original varint message.
    // Custom requests 40 bytes; max(StringError=24, Thrift=40, TryReserveError=16)=40.
    // These pinned layouts are checked by the cursor certificate tests on CI's test route.
    let error_bytes = 3 * 88 + 19 + 40 + 40;
    let rejection = metadata + 8 * header + 48 + error_bytes;
    assert!(rejection <= arrow);

    Ok(arrow)
}

pub(super) fn validate(path: &Path) -> Result<()> {
    let file = File::open(path)?;
    let size = file.metadata()?.len();
    if size > FILE_BYTES_MAX {
        return Err(Error::Bound("parquet file bytes"));
    }
    if size < 12 {
        return Err(Error::Bound("parquet file framing"));
    }

    let mut reader = BufReader::with_capacity(8192, file);
    validate_reader(&mut reader, size)
}

pub(super) fn validate_reader(reader: &mut (impl Read + Seek), size: u64) -> Result<()> {
    drop(validate_reader_metadata(reader, size)?);
    Ok(())
}

pub(super) fn validate_reader_metadata(
    reader: &mut (impl Read + Seek),
    size: u64,
) -> Result<ThriftFileMetadata> {
    if size > FILE_BYTES_MAX {
        return Err(Error::Bound("parquet file bytes"));
    }
    if size < 12 {
        return Err(Error::Bound("parquet file framing"));
    }

    let footer = validate_footer(reader, size)?;
    let metadata = validate_metadata(reader, &footer)?;
    validate_schema(&metadata.schema)?;
    schema::validate_arrow(&metadata)?;
    validate_rows(&metadata)?;
    let mut totals = Totals::default();

    for group in &metadata.row_groups {
        validate_group(reader, group, &footer, &mut totals)?;
    }

    Ok(metadata)
}

fn validate_footer(file: &mut (impl Read + Seek), size: u64) -> Result<Extent> {
    let mut magic = [0; 4];
    file.read_exact(&mut magic)?;
    if magic != *b"PAR1" {
        return Err(Error::Bound("parquet file framing"));
    }

    file.seek(SeekFrom::End(-8))?;
    let mut trailer = [0; 8];
    file.read_exact(&mut trailer)?;
    if trailer[4..] != *b"PAR1" {
        return Err(Error::Bound("parquet file framing"));
    }

    let length = u64::from(u32::from_le_bytes(
        trailer[..4]
            .try_into()
            .map_err(|_| Error::Bound("parquet footer length"))?,
    ));
    if length > FOOTER_BYTES_MAX {
        return Err(Error::Bound("parquet footer bytes"));
    }

    let end = size - 8;
    let start = end
        .checked_sub(length)
        .ok_or(Error::Bound("parquet footer extent"))?;
    if start < 4 {
        return Err(Error::Bound("parquet footer extent"));
    }

    Ok(Extent { start, end })
}

fn validate_metadata(file: &mut (impl Read + Seek), footer: &Extent) -> Result<ThriftFileMetadata> {
    file.seek(SeekFrom::Start(footer.start))?;
    let length = footer.end - footer.start;
    let budget =
        usize::try_from(FOOTER_BYTES_MAX).map_err(|_| Error::Bound("parquet footer bytes"))?;
    let mut protocol = BoundedProtocol::new(file.take(length), budget);
    ThriftFileMetadata::read_from_in_protocol(&mut protocol).map_err(thrift_error)
}

fn validate_schema(schema: &[SchemaElement]) -> Result<()> {
    if schema.len() < 2 {
        return Err(Error::Bound("parquet schema columns"));
    }
    if schema.len() > COLUMNS_MAX + 1 {
        return Err(Error::Bound("parquet schema columns"));
    }
    let columns =
        i32::try_from(schema.len() - 1).map_err(|_| Error::Bound("parquet schema columns"))?;
    if schema[0].num_children != Some(columns) {
        return Err(Error::Bound("parquet scalar schema"));
    }

    for field in &schema[1..] {
        if field.name.len() > COLUMN_NAME_BYTES_MAX {
            return Err(Error::Bound("parquet column name"));
        }
        if field.num_children.is_some() {
            return Err(Error::Bound("parquet scalar schema"));
        }
        match field.repetition_type {
            Some(FieldRepetitionType::REQUIRED | FieldRepetitionType::OPTIONAL) => {}
            _ => return Err(Error::Bound("parquet scalar schema")),
        }

        validate_scalar_type(field.type_)?;
        schema::validate_physical(field)?;
    }

    Ok(())
}

fn validate_scalar_type(kind: Option<Type>) -> Result<()> {
    match kind {
        Some(
            Type::BOOLEAN
            | Type::INT32
            | Type::INT64
            | Type::FLOAT
            | Type::DOUBLE
            | Type::BYTE_ARRAY,
        ) => Ok(()),
        _ => Err(Error::Bound("parquet scalar type")),
    }
}

fn validate_rows(metadata: &ThriftFileMetadata) -> Result<()> {
    let rows = bounded(metadata.num_rows, ROWS_MAX, "parquet file rows")?;
    if metadata.row_groups.len() > ROW_GROUPS_MAX {
        return Err(Error::Bound("parquet row groups"));
    }
    if metadata.encryption_algorithm.is_some() {
        return Err(Error::Bound("parquet encryption"));
    }
    let mut total = 0u64;

    for group in &metadata.row_groups {
        let count = bounded(group.num_rows, ROWS_MAX, "parquet group rows")?;
        total = total
            .checked_add(count)
            .ok_or(Error::Bound("parquet file rows"))?;
        if total > rows {
            return Err(Error::Bound("parquet file rows"));
        }
        if group.columns.len() != metadata.schema.len() - 1 {
            return Err(Error::Bound("parquet group columns"));
        }

        for (index, column) in group.columns.iter().enumerate() {
            let column = column
                .meta_data
                .as_ref()
                .ok_or(Error::Bound("parquet column metadata"))?;
            if Some(column.type_) != metadata.schema[index + 1].type_ {
                return Err(Error::Bound("parquet column type"));
            }
        }
    }

    if total != rows {
        return Err(Error::Bound("parquet file rows"));
    }

    Ok(())
}

fn validate_group(
    file: &mut (impl Read + Seek),
    group: &RowGroup,
    footer: &Extent,
    totals: &mut Totals,
) -> Result<()> {
    bounded(
        group.total_byte_size,
        EXPANDED_BYTES_MAX,
        "parquet group bytes",
    )?;

    for column in &group.columns {
        let metadata = validate_column(column, group.num_rows)?;
        let extent = validate_column_extent(metadata, footer)?;
        validate_auxiliary_extents(column, metadata, footer)?;
        validate_pages(file, metadata, &extent, totals)?;
    }

    Ok(())
}

fn validate_column(column: &ColumnChunk, rows: i64) -> Result<&ColumnMetaData> {
    if column.file_path.is_some() {
        return Err(Error::Bound("parquet external column"));
    }
    if column.crypto_metadata.is_some() {
        return Err(Error::Bound("parquet encryption"));
    }
    if column.encrypted_column_metadata.is_some() {
        return Err(Error::Bound("parquet encryption"));
    }
    let metadata = column
        .meta_data
        .as_ref()
        .ok_or(Error::Bound("parquet column metadata"))?;
    if metadata.num_values != rows {
        return Err(Error::Bound("parquet column values"));
    }

    bounded(metadata.num_values, ROWS_MAX, "parquet column values")?;
    if let Some(statistics) = &metadata.statistics {
        if let Some(count) = statistics.null_count {
            bounded(count, ROWS_MAX, "parquet null count")?;
        }
        if let Some(count) = statistics.distinct_count {
            bounded(count, ROWS_MAX, "parquet distinct count")?;
        }
    }
    bounded(
        metadata.total_uncompressed_size,
        EXPANDED_BYTES_MAX,
        "parquet column bytes",
    )?;
    Ok(metadata)
}

fn validate_column_extent(metadata: &ColumnMetaData, footer: &Extent) -> Result<Extent> {
    let data = bounded(
        metadata.data_page_offset,
        footer.start,
        "parquet column offset",
    )?;
    let start = match metadata.dictionary_page_offset {
        Some(offset) => bounded(offset, data, "parquet dictionary offset")?,
        None => data,
    };
    let size = bounded(
        metadata.total_compressed_size,
        FILE_BYTES_MAX,
        "parquet column bytes",
    )?;
    let end = start
        .checked_add(size)
        .ok_or(Error::Bound("parquet column extent"))?;
    if start < 4 {
        return Err(Error::Bound("parquet column extent"));
    }
    if end > footer.start {
        return Err(Error::Bound("parquet column extent"));
    }
    if data > end {
        return Err(Error::Bound("parquet column extent"));
    }

    Ok(Extent { start, end })
}

fn validate_auxiliary_extents(
    column: &ColumnChunk,
    metadata: &ColumnMetaData,
    footer: &Extent,
) -> Result<()> {
    validate_auxiliary_extent(
        column.offset_index_offset,
        column.offset_index_length,
        footer,
    )?;
    validate_auxiliary_extent(
        column.column_index_offset,
        column.column_index_length,
        footer,
    )?;
    validate_auxiliary_extent(
        metadata.bloom_filter_offset,
        metadata.bloom_filter_length,
        footer,
    )?;
    if let Some(offset) = metadata.index_page_offset {
        bounded(offset, footer.start, "parquet index offset")?;
    }

    Ok(())
}

fn validate_auxiliary_extent(
    offset: Option<i64>,
    length: Option<i32>,
    footer: &Extent,
) -> Result<()> {
    if let Some(offset) = offset {
        let start = bounded(offset, footer.start, "parquet auxiliary offset")?;
        let length = match length {
            Some(length) => bounded(
                i64::from(length),
                FOOTER_BYTES_MAX,
                "parquet auxiliary bytes",
            )?,
            None => return Ok(()),
        };
        let end = start
            .checked_add(length)
            .ok_or(Error::Bound("parquet auxiliary extent"))?;
        if end > footer.start {
            return Err(Error::Bound("parquet auxiliary extent"));
        }
    } else {
        if length.is_some() {
            return Err(Error::Bound("parquet auxiliary extent"));
        }
    }

    Ok(())
}

fn validate_pages(
    file: &mut (impl Read + Seek),
    column: &ColumnMetaData,
    extent: &Extent,
    totals: &mut Totals,
) -> Result<()> {
    let mut position = extent.start;
    let mut measurements = PageMeasurements::default();

    for _page in 0..=PAGES_MAX {
        if position == extent.end {
            return validate_pages_complete(column, &measurements, totals);
        }

        file.seek(SeekFrom::Start(position))?;
        let limit = (extent.end - position).min(PAGE_HEADER_BYTES_MAX);
        let budget = usize::try_from(PAGE_HEADER_BYTES_MAX)
            .map_err(|_| Error::Bound("parquet page header"))?;
        let mut protocol = BoundedProtocol::new((&mut *file).take(limit), budget);
        let header =
            ParquetPageHeader::read_from_in_protocol(&mut protocol).map_err(thrift_error)?;
        validate_page_offset(&header, column, position, extent)?;
        let payload = file.stream_position()?;
        let header_bytes = payload - position;
        let compressed = bounded(
            i64::from(header.compressed_page_size),
            PAGE_BYTES_MAX,
            "parquet compressed page bytes",
        )?;
        let decoded = bounded(
            i64::from(header.uncompressed_page_size),
            PAGE_BYTES_MAX,
            "parquet expanded page bytes",
        )?;
        position = payload
            .checked_add(compressed)
            .ok_or(Error::Bound("parquet page extent"))?;
        if position > extent.end {
            return Err(Error::Bound("parquet page extent"));
        }

        measurements.values = measurements
            .values
            .checked_add(validate_page_values(&header, column.num_values)?)
            .ok_or(Error::Bound("parquet page values"))?;
        measurements.expanded_bytes = measurements
            .expanded_bytes
            .checked_add(decoded + header_bytes)
            .ok_or(Error::Bound("parquet expanded bytes"))?;
        if measurements.expanded_bytes > EXPANDED_BYTES_MAX {
            return Err(Error::Bound("parquet expanded bytes"));
        }
        totals.pages += 1;
        if totals.pages > PAGES_MAX {
            return Err(Error::Bound("parquet page count"));
        }
    }

    Err(Error::Bound("parquet page count"))
}

fn validate_page_values(header: &ParquetPageHeader, maximum: i64) -> Result<u64> {
    let maximum = bounded(maximum, ROWS_MAX, "parquet column values")?;
    match header.type_ {
        PageType::DATA_PAGE => {
            let data = header
                .data_page_header
                .as_ref()
                .ok_or(Error::Bound("parquet data page header"))?;
            validate_data_encoding(data.encoding)?;
            validate_level_encoding(data.definition_level_encoding)?;
            validate_level_encoding(data.repetition_level_encoding)?;
            bounded(i64::from(data.num_values), maximum, "parquet page values")
        }
        PageType::DATA_PAGE_V2 => {
            let data = header
                .data_page_header_v2
                .as_ref()
                .ok_or(Error::Bound("parquet data page header"))?;
            validate_data_encoding(data.encoding)?;
            if data.num_rows != data.num_values {
                return Err(Error::Bound("parquet page rows"));
            }

            let values = bounded(i64::from(data.num_values), maximum, "parquet page values")?;
            bounded(i64::from(data.num_nulls), values, "parquet page nulls")?;
            validate_page_levels(data, header)?;
            Ok(values)
        }
        PageType::DICTIONARY_PAGE => {
            let dictionary = header
                .dictionary_page_header
                .as_ref()
                .ok_or(Error::Bound("parquet dictionary page header"))?;
            if dictionary.encoding != Encoding::PLAIN {
                return Err(Error::Bound("parquet dictionary encoding"));
            }
            bounded(
                i64::from(dictionary.num_values),
                maximum,
                "parquet dictionary values",
            )?;
            Ok(0)
        }
        _ => Err(Error::Bound("parquet page type")),
    }
}

fn validate_data_encoding(encoding: Encoding) -> Result<()> {
    // Prefix expansion and delta headers require body admission before native decoding is safe.
    match encoding {
        Encoding::PLAIN | Encoding::PLAIN_DICTIONARY | Encoding::RLE_DICTIONARY => Ok(()),
        _ => Err(Error::Bound("parquet data encoding")),
    }
}

fn validate_level_encoding(encoding: Encoding) -> Result<()> {
    match encoding {
        Encoding::RLE | Encoding::BIT_PACKED => Ok(()),
        _ => Err(Error::Bound("parquet level encoding")),
    }
}

fn validate_page_levels(
    data: &polars_parquet_format::DataPageHeaderV2,
    header: &ParquetPageHeader,
) -> Result<()> {
    let definition = bounded(
        i64::from(data.definition_levels_byte_length),
        PAGE_BYTES_MAX,
        "parquet page levels",
    )?;
    let repetition = bounded(
        i64::from(data.repetition_levels_byte_length),
        PAGE_BYTES_MAX,
        "parquet page levels",
    )?;
    let level_bytes = definition + repetition;
    let compressed = u64::try_from(header.compressed_page_size)
        .map_err(|_| Error::Bound("parquet compressed page bytes"))?;
    if level_bytes > compressed {
        return Err(Error::Bound("parquet page levels"));
    }
    let expanded = u64::try_from(header.uncompressed_page_size)
        .map_err(|_| Error::Bound("parquet expanded page bytes"))?;
    if level_bytes > expanded {
        return Err(Error::Bound("parquet page levels"));
    }

    Ok(())
}

fn validate_page_offset(
    header: &ParquetPageHeader,
    column: &ColumnMetaData,
    position: u64,
    extent: &Extent,
) -> Result<()> {
    if header.type_ == PageType::DICTIONARY_PAGE {
        if position != extent.start {
            return Err(Error::Bound("parquet dictionary offset"));
        }
        let offset =
            i64::try_from(position).map_err(|_| Error::Bound("parquet dictionary offset"))?;
        if column.dictionary_page_offset != Some(offset) {
            return Err(Error::Bound("parquet dictionary offset"));
        }
    } else {
        let data = u64::try_from(column.data_page_offset)
            .map_err(|_| Error::Bound("parquet column offset"))?;
        if position < data {
            return Err(Error::Bound("parquet column offset"));
        }
    }

    Ok(())
}

fn validate_pages_complete(
    column: &ColumnMetaData,
    measurements: &PageMeasurements,
    totals: &mut Totals,
) -> Result<()> {
    if measurements.values != bounded(column.num_values, ROWS_MAX, "parquet column values")? {
        return Err(Error::Bound("parquet page values"));
    }
    totals.expanded_bytes = totals
        .expanded_bytes
        .checked_add(measurements.expanded_bytes)
        .ok_or(Error::Bound("parquet expanded bytes"))?;
    if totals.expanded_bytes > EXPANDED_BYTES_MAX {
        return Err(Error::Bound("parquet expanded bytes"));
    }

    let decoded = validate_decoded_bytes(column, measurements)?;
    totals.decoded_bytes = totals
        .decoded_bytes
        .checked_add(decoded)
        .ok_or(Error::Bound("parquet decoded bytes"))?;
    if totals.decoded_bytes > EXPANDED_BYTES_MAX {
        return Err(Error::Bound("parquet decoded bytes"));
    }

    Ok(())
}

fn validate_decoded_bytes(column: &ColumnMetaData, measurements: &PageMeasurements) -> Result<u64> {
    let width = match column.type_ {
        Type::BOOLEAN => 1,
        Type::INT32 | Type::FLOAT => 4,
        Type::INT64 | Type::DOUBLE => 8,
        Type::BYTE_ARRAY => 16,
        _ => return Err(Error::Bound("parquet scalar type")),
    };
    let fixed = measurements
        .values
        .checked_mul(width)
        .ok_or(Error::Bound("parquet decoded bytes"))?;
    if column.type_ == Type::BYTE_ARRAY {
        fixed
            .checked_add(measurements.expanded_bytes)
            .ok_or(Error::Bound("parquet decoded bytes"))
    } else {
        Ok(fixed.max(measurements.expanded_bytes))
    }
}

fn bounded(value: i64, maximum: u64, bound: &'static str) -> Result<u64> {
    let value = u64::try_from(value).map_err(|_| Error::Bound(bound))?;
    if value > maximum {
        return Err(Error::Bound(bound));
    }

    Ok(value)
}

fn thrift_error(error: polars_parquet_format::thrift::Error) -> Error {
    io::Error::new(io::ErrorKind::InvalidData, error).into()
}
