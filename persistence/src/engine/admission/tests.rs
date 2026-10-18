use std::io::Cursor;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use polars::prelude::{Column, DataFrame, ParquetWriter};
use polars_parquet_format::thrift::protocol::{
    TCompactOutputProtocol, TFieldIdentifier, TInputProtocol, TListIdentifier, TOutputProtocol,
    TType,
};
use polars_parquet_format::thrift::{Error as ThriftError, ProtocolErrorKind};
use polars_parquet_format::{
    ColumnChunk, ColumnMetaData, CompressionCodec, DataPageHeader, Encoding, FieldRepetitionType,
    FileMetaData, PageHeader, PageType, RowGroup, SchemaElement, Type,
};

use super::protocol::BoundedProtocol;
use super::{FOOTER_BYTES_MAX, PAGE_BYTES_MAX, ROWS_MAX, validate};
use crate::Error;

struct Directory(PathBuf);

pub(super) struct DeclaredFile {
    pub(super) rows: i64,
    pub(super) columns: usize,
    pub(super) first_offset: i64,
}

struct DeclaredColumn {
    rows: i64,
    offset: i64,
    bytes: i64,
}

static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

impl Directory {
    fn new() -> Self {
        let next = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "flightdeck-parquet-admission-{}-{next}",
            std::process::id()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();
        Self(path)
    }

    fn file(&self) -> PathBuf {
        self.0.join("source.parquet")
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn admits_current_native_parquet_without_decoding_values() {
    // Goal: ordinary saved scalar columns remain admissible with exact full-width counters.
    // Method: write a real Polars file and admit its native metadata and page headers.
    let directory = Directory::new();
    let path = directory.file();
    let mut frame =
        DataFrame::new(2, vec![Column::new("publication".into(), [0u64, u64::MAX])]).unwrap();
    ParquetWriter::new(std::fs::File::create(&path).unwrap())
        .finish(&mut frame)
        .unwrap();

    let result = validate(&path);

    result.unwrap();
}

#[test]
fn refuses_an_oversized_expanded_page_before_decoding_its_body() {
    // Goal: a small file cannot ask the decoder to allocate an oversized output page.
    // Method: write a native page header declaring 64 MiB plus one byte beside a tiny body.
    let directory = Directory::new();
    let path = directory.file();
    let expanded = i32::try_from(PAGE_BYTES_MAX + 1).unwrap();
    let mut header = page_header(1);
    header.uncompressed_page_size = expanded;
    write_declared_file(
        &path,
        &header,
        &DeclaredFile {
            rows: 1,
            columns: 1,
            first_offset: 4,
        },
    );

    let result = validate(&path);

    assert!(matches!(
        result,
        Err(Error::Bound("parquet expanded page bytes"))
    ));
}

#[test]
fn refuses_an_oversized_compressed_page_before_reading_its_body() {
    // Goal: a page header cannot cause an oversized compressed buffer allocation.
    // Method: declare 64 MiB plus one byte while storing only five bytes of page payload.
    let directory = Directory::new();
    let path = directory.file();
    let compressed = i32::try_from(PAGE_BYTES_MAX + 1).unwrap();
    let mut header = page_header(1);
    header.compressed_page_size = compressed;
    write_declared_file(
        &path,
        &header,
        &DeclaredFile {
            rows: 1,
            columns: 1,
            first_offset: 4,
        },
    );

    let result = validate(&path);

    assert!(matches!(
        result,
        Err(Error::Bound("parquet compressed page bytes"))
    ));
}

#[test]
fn refuses_an_oversized_footer_before_parsing_metadata() {
    // Goal: the footer length is checked before the native metadata parser receives it.
    // Method: store an oversized length in a twelve-byte file without a metadata body.
    let directory = Directory::new();
    let path = directory.file();
    let mut bytes = b"PAR1".to_vec();
    bytes.extend_from_slice(&u32::try_from(FOOTER_BYTES_MAX + 1).unwrap().to_le_bytes());
    bytes.extend_from_slice(b"PAR1");
    std::fs::write(&path, bytes).unwrap();

    let result = validate(&path);

    assert!(matches!(result, Err(Error::Bound("parquet footer bytes"))));
}

#[test]
fn bounds_thrift_containers_before_reserving_their_elements() {
    // Goal: a short metadata prefix cannot reserve an arbitrarily large structure vector.
    // Method: write a native list header with no elements and distinguish a size limit from EOF.
    let mut bytes = Vec::new();
    TCompactOutputProtocol::new(&mut bytes)
        .write_list_begin(&TListIdentifier::new(TType::Struct, 16_385))
        .unwrap();
    let mut protocol = BoundedProtocol::new(Cursor::new(bytes), 4 * 1024 * 1024);

    let result = protocol.read_list::<SchemaElement>();

    assert!(matches!(
        result,
        Err(ThriftError::Protocol(error)) if error.kind == ProtocolErrorKind::SizeLimit
    ));
}

#[test]
fn bounds_field_identifiers_before_a_later_delta_can_overflow() {
    // Goal: native compact-protocol field deltas never reach a signed-integer overflow.
    // Method: write the largest explicit field identifier and reject it before another field.
    let mut bytes = Vec::new();
    TCompactOutputProtocol::new(&mut bytes)
        .write_field_begin(&TFieldIdentifier::new("unknown", TType::I32, i16::MAX))
        .unwrap();
    let mut protocol = BoundedProtocol::new(Cursor::new(bytes), 4 * 1024 * 1024);

    let result = protocol.read_field_begin();

    assert!(matches!(
        result,
        Err(ThriftError::Protocol(error)) if error.kind == ProtocolErrorKind::SizeLimit
    ));
}

#[test]
fn refuses_negative_column_offsets_before_native_metadata_conversion() {
    // Goal: malformed signed offsets never reach the native range conversion that can panic.
    // Method: serialize complete native metadata with a negative data-page offset.
    let directory = Directory::new();
    let path = directory.file();
    write_declared_file(
        &path,
        &page_header(1),
        &DeclaredFile {
            rows: 1,
            columns: 1,
            first_offset: -1,
        },
    );

    let result = validate(&path);

    assert!(matches!(result, Err(Error::Bound("parquet column offset"))));
}

#[test]
fn refuses_declared_binary_columns_that_exceed_the_decoded_budget() {
    // Goal: short encoded pages cannot bypass the memory admission with huge binary row counts.
    // Method: declare five flat binary columns whose view descriptors alone exceed 512 MiB.
    let directory = Directory::new();
    let path = directory.file();
    let rows = i64::try_from(ROWS_MAX).unwrap();
    let header = page_header(i32::try_from(rows).unwrap());
    write_declared_file(
        &path,
        &header,
        &DeclaredFile {
            rows,
            columns: 5,
            first_offset: 4,
        },
    );

    let result = validate(&path);

    assert!(matches!(result, Err(Error::Bound("parquet decoded bytes"))));
}

pub(super) fn page_header(values: i32) -> PageHeader {
    PageHeader::new(
        PageType::DATA_PAGE,
        5,
        5,
        None,
        Some(DataPageHeader::new(
            values,
            Encoding::PLAIN,
            Encoding::RLE,
            Encoding::RLE,
            None,
        )),
        None,
        None,
        None,
    )
}

fn write_declared_file(path: &Path, header: &PageHeader, declared: &DeclaredFile) {
    write_declared_payload(path, header, declared, &[1, 0, 0, 0, b'a']);
}

pub(super) fn write_declared_payload(
    path: &Path,
    header: &PageHeader,
    declared: &DeclaredFile,
    payload: &[u8],
) {
    let mut bytes = b"PAR1".to_vec();
    let mut columns = Vec::new();
    let mut fields = vec![schema_field(
        "root".to_owned(),
        None,
        Some(i32::try_from(declared.columns).unwrap()),
    )];

    for index in 0..declared.columns {
        let start = if index == 0 {
            declared.first_offset
        } else {
            i64::try_from(bytes.len()).unwrap()
        };
        let first = bytes.len();
        header
            .write_to_out_protocol(&mut TCompactOutputProtocol::new(&mut bytes))
            .unwrap();
        bytes.extend_from_slice(payload);
        let length = i64::try_from(bytes.len() - first).unwrap();
        let name = format!("column_{index}");
        fields.push(schema_field(name.clone(), Some(Type::BYTE_ARRAY), None));
        columns.push(column_metadata(
            name,
            &DeclaredColumn {
                rows: declared.rows,
                offset: start,
                bytes: length,
            },
        ));
    }

    let group = RowGroup::new(columns, 5, declared.rows, None, None, None, None);
    let metadata = FileMetaData::new(
        1,
        fields,
        declared.rows,
        vec![group],
        None,
        None,
        None,
        None,
        None,
    );
    let first = bytes.len();
    metadata
        .write_to_out_protocol(&mut TCompactOutputProtocol::new(&mut bytes))
        .unwrap();
    let length = u32::try_from(bytes.len() - first).unwrap();
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.extend_from_slice(b"PAR1");
    std::fs::write(path, bytes).unwrap();
}

fn schema_field(name: String, kind: Option<Type>, children: Option<i32>) -> SchemaElement {
    SchemaElement::new(
        kind,
        None,
        kind.map(|_| FieldRepetitionType::REQUIRED),
        name,
        children,
        None,
        None,
        None,
        None,
        None,
    )
}

fn column_metadata(name: String, declared: &DeclaredColumn) -> ColumnChunk {
    ColumnChunk::new(
        None,
        0,
        Some(ColumnMetaData::new(
            Type::BYTE_ARRAY,
            vec![Encoding::PLAIN],
            vec![name],
            CompressionCodec::UNCOMPRESSED,
            declared.rows,
            declared.bytes,
            declared.bytes,
            None,
            declared.offset,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )),
        None,
        None,
        None,
        None,
        None,
        None,
    )
}
