use std::io::{BufReader, Cursor};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use polars::prelude::{Column, DataFrame, NamedFrom, ParquetWriter, Series};
use polars_parquet::parquet::encoding::delta_byte_array;
use polars_parquet::parquet::metadata::ThriftFileMetadata;
use polars_parquet_format::thrift::protocol::{
    TCompactOutputProtocol, TFieldIdentifier, TInputProtocol, TListIdentifier, TOutputProtocol,
    TStructIdentifier, TType,
};
use polars_parquet_format::thrift::{Error as ThriftError, ProtocolErrorKind};
use polars_parquet_format::{
    DataPageHeaderV2, DictionaryPageHeader, Encoding, PageHeader, PageType, Statistics,
};

use super::protocol::BoundedProtocol;
use super::tests::{DeclaredFile, page_header, write_declared_payload};
use super::{validate, validate_footer, validate_metadata};
use crate::Error;
use crate::engine::Engine;

const CHILD_PATH: &str = "FLIGHTDECK_PARSER_REFUSAL_CHILD_PATH";
const CHILD_SUCCESS: i32 = 72;
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    path: PathBuf,
}

#[test]
fn reads_current_dictionary_pages_with_exact_nullable_recording_values() {
    // Goal: current native dictionary encoding preserves full-width and binary values.
    // Method: save repeated nullable scalars, verify the emitted encoding, then query them.
    let expected = dictionary_frame();
    let fixture = Fixture::with_frame(expected.clone());
    assert!(fixture.encodings().contains(&Encoding::RLE_DICTIONARY));

    let actual = fixture.query();

    assert_eq!(actual, expected);
}

#[test]
fn reads_legacy_dictionary_pages_with_exact_nullable_recording_values() {
    // Goal: legacy dictionary pages retain their values under the recording policy.
    // Method: tag current dictionary pages with the legacy encoding earlier recordings saved,
    // then compare the native query directly.
    let fixture = Fixture::with_frame(dictionary_frame());
    fixture.relabel_dictionary_pages();
    let encodings = fixture.encodings();
    assert!(encodings.contains(&Encoding::PLAIN_DICTIONARY));
    assert!(!encodings.contains(&Encoding::RLE_DICTIONARY));

    let actual = fixture.query();

    assert_eq!(actual, dictionary_frame());
}

#[test]
fn refuses_statistics_boolean_fields_with_integer_wire_tags_without_aborting() {
    // Goal: malformed Boolean fields cannot abort the native handwritten footer parser.
    // Method: replace the exact Statistics field with its generated-reader-compatible I32 tag.
    let fixture = Fixture::new();
    fixture.rewrite(
        |metadata| {
            metadata.row_groups[0].columns[0]
                .meta_data
                .as_mut()
                .unwrap()
                .statistics = Some(Statistics::new(
                None,
                None,
                None,
                None,
                None,
                None,
                Some(true),
                None,
            ));
        },
        Some((&[0x71, 0], &[0x75, 1, 0])),
    );

    fixture.assert_refused_without_aborting(
        "engine::admission::parser_tests::\
         refuses_statistics_boolean_fields_with_integer_wire_tags_without_aborting",
    );
}

#[test]
fn refuses_integer_signedness_with_integer_wire_tags_without_aborting() {
    // Goal: malformed integer signedness cannot abort native logical-type conversion.
    // Method: preserve UInt64 semantics while replacing only the signedness Boolean tag.
    let fixture = Fixture::new();
    fixture.rewrite(
        |_| {},
        Some((&[0x13, 64, 0x12, 0], &[0x13, 64, 0x15, 2, 0])),
    );

    fixture.assert_refused_without_aborting(
        "engine::admission::parser_tests::\
         refuses_integer_signedness_with_integer_wire_tags_without_aborting",
    );
}

#[test]
fn refuses_integer_statistics_with_a_different_integer_wire_width() {
    // Goal: generated readers cannot reinterpret a declared scalar wire type.
    // Method: declare null_count as I32 although Statistics requires I64.
    let mut protocol = BoundedProtocol::new(Cursor::new([0x35, 0, 0]), 1024);

    let result = Statistics::read_from_in_protocol(&mut protocol);

    assert!(matches!(result, Err(ThriftError::Protocol(error))
        if error.kind == ProtocolErrorKind::InvalidData));
}

#[test]
fn refuses_struct_collections_with_integer_element_tags() {
    // Goal: collection elements obey their declared wire types as fields do.
    // Method: request Statistics from a list declaring I32 elements but carrying a struct Stop.
    let mut protocol = BoundedProtocol::new(Cursor::new([0x15, 0]), 1024);

    let result = protocol.read_list::<Statistics>();

    assert!(matches!(result, Err(ThriftError::Protocol(error))
        if error.kind == ProtocolErrorKind::InvalidData));
}

#[test]
fn skips_unknown_boolean_collections_before_reading_known_boolean_fields() {
    // Goal: valid compact Boolean collections remain readable while wire types are enforced.
    // Method: put a skipped Boolean list before an ordinary Statistics Boolean field.
    let mut bytes = Vec::new();
    let mut output = TCompactOutputProtocol::new(&mut bytes);
    output
        .write_struct_begin(&TStructIdentifier::new("Statistics"))
        .unwrap();
    output
        .write_field_begin(&TFieldIdentifier::new("unknown", TType::List, 100))
        .unwrap();
    output
        .write_list_begin(&TListIdentifier::new(TType::Bool, 2))
        .unwrap();
    output.write_bool(true).unwrap();
    output.write_bool(false).unwrap();
    output.write_list_end().unwrap();
    output.write_field_end().unwrap();
    output
        .write_field_begin(&TFieldIdentifier::new("is_max_value_exact", TType::Bool, 7))
        .unwrap();
    output.write_bool(true).unwrap();
    output.write_field_end().unwrap();
    output.write_field_stop().unwrap();
    output.write_struct_end().unwrap();
    let mut protocol = BoundedProtocol::new(Cursor::new(bytes), 1024);

    let statistics = Statistics::read_from_in_protocol(&mut protocol).unwrap();

    assert_eq!(statistics.is_max_value_exact, Some(true));
}

#[test]
fn skips_unknown_boolean_maps_before_reading_known_boolean_fields() {
    // Goal: Boolean map elements cannot inherit the type of their enclosing struct field.
    // Method: skip one Boolean pair in a compact map, then read Statistics field seven.
    let bytes = [0x0b, 0xc8, 1, 1, 0x11, 1, 2, 1, 14, 0];
    let mut protocol = BoundedProtocol::new(Cursor::new(bytes), 1024);

    let statistics = Statistics::read_from_in_protocol(&mut protocol).unwrap();

    assert_eq!(statistics.is_max_value_exact, Some(true));
}

#[test]
fn skips_unknown_boolean_sets_before_reading_known_boolean_fields() {
    // Goal: valid Boolean sets remain skippable while the declared wire type is enforced.
    // Method: skip two Boolean set elements, then read Statistics field seven.
    let bytes = [0x0a, 0xc8, 1, 0x21, 1, 2, 1, 14, 0];
    let mut protocol = BoundedProtocol::new(Cursor::new(bytes), 1024);

    let statistics = Statistics::read_from_in_protocol(&mut protocol).unwrap();

    assert_eq!(statistics.is_max_value_exact, Some(true));
}

#[test]
fn refuses_malformed_delta_length_pages_without_aborting() {
    // Goal: untrusted delta headers cannot reach a native block-size assertion.
    // Method: pass a one-byte delta block header through the production source admission.
    let fixture = Fixture::new();
    fixture.write_page(Encoding::DELTA_LENGTH_BYTE_ARRAY, 1, &[1]);

    fixture.assert_refused_without_aborting(
        "engine::admission::parser_tests::refuses_malformed_delta_length_pages_without_aborting",
    );
}

#[test]
fn refuses_prefix_expansion_beyond_the_decoded_budget_before_decoding_values() {
    // Goal: prefix-compressed values cannot expand past admission's decoded-memory budget.
    // Method: reuse one native prefix-encoded page across 32 columns without decoding values.
    let fixture = Fixture::new();
    let mut payload = Vec::new();
    let value = vec![b'a'; 65_536];
    delta_byte_array::encode(std::iter::repeat_n(value.as_slice(), 257), &mut payload);
    assert!(payload.len() < 128 * 1024);
    const { assert!(32u64 * 257 * 65_536 > super::EXPANDED_BYTES_MAX) };
    let mut header = page_header(257);
    header.data_page_header.as_mut().unwrap().encoding = Encoding::DELTA_BYTE_ARRAY;
    header.compressed_page_size = i32::try_from(payload.len()).unwrap();
    header.uncompressed_page_size = header.compressed_page_size;
    write_declared_payload(
        &fixture.path,
        &header,
        &DeclaredFile {
            rows: 257,
            columns: 32,
            first_offset: 4,
        },
        &payload,
    );

    let result = validate(&fixture.path);

    assert!(matches!(result, Err(Error::Bound("parquet data encoding"))));
}

#[test]
fn refuses_valid_small_delta_pages_under_the_same_recording_encoding_policy() {
    // Goal: admission's policy rejects unsupported encodings independently of their size.
    // Method: encode and locally verify two short repeated values before refusing the file.
    let fixture = Fixture::new();
    let value = b"small repeated binary value".as_slice();
    let mut payload = Vec::new();
    delta_byte_array::encode(std::iter::repeat_n(value, 2), &mut payload);
    let decoded = delta_byte_array::Decoder::try_new(&payload)
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(decoded, [value, value]);
    fixture.write_page(Encoding::DELTA_BYTE_ARRAY, 2, &payload);

    let result = validate(&fixture.path);

    assert!(matches!(result, Err(Error::Bound("parquet data encoding"))));
}

#[test]
fn refuses_delta_encoding_in_version_two_data_pages() {
    // Goal: a newer page header cannot bypass the recording's data-encoding restriction.
    // Method: replace a complete synthetic V1 header with a bounded V2 delta header.
    let fixture = Fixture::new();
    let mut header = page_header(1);
    header.type_ = PageType::DATA_PAGE_V2;
    header.data_page_header = None;
    header.data_page_header_v2 = Some(DataPageHeaderV2::new(
        1,
        0,
        1,
        Encoding::DELTA_BINARY_PACKED,
        0,
        0,
        Some(false),
        None,
    ));
    write_declared_payload(
        &fixture.path,
        &header,
        &DeclaredFile {
            rows: 1,
            columns: 1,
            first_offset: 4,
        },
        &[1, 0, 0, 0, b'a'],
    );

    let result = validate(&fixture.path);

    assert!(matches!(result, Err(Error::Bound("parquet data encoding"))));
}

#[test]
fn refuses_dictionary_pages_with_unsupported_body_encodings() {
    // Goal: a dictionary header cannot direct the consumer to an unadmitted value encoding.
    // Method: put a delta dictionary page before the declared data-page position.
    let fixture = Fixture::new();
    let mut header = page_header(1);
    header.type_ = PageType::DICTIONARY_PAGE;
    header.data_page_header = None;
    header.dictionary_page_header = Some(DictionaryPageHeader::new(
        1,
        Encoding::DELTA_BYTE_ARRAY,
        None,
    ));
    write_declared_payload(
        &fixture.path,
        &header,
        &DeclaredFile {
            rows: 1,
            columns: 1,
            first_offset: 4,
        },
        &[1, 0, 0, 0, b'a'],
    );
    fixture.rewrite(
        |metadata| {
            metadata.row_groups[0].columns[0]
                .meta_data
                .as_mut()
                .unwrap()
                .dictionary_page_offset = Some(4);
        },
        None,
    );

    let result = validate(&fixture.path);

    assert!(matches!(
        result,
        Err(Error::Bound("parquet dictionary encoding"))
    ));
}

#[test]
fn refuses_unsupported_definition_level_encodings() {
    // Goal: value encodings cannot be interpreted as definition-level bitstreams.
    // Method: give an otherwise ordinary plain data page delta-encoded definition levels.
    let fixture = Fixture::new();
    let mut header = page_header(1);
    header
        .data_page_header
        .as_mut()
        .unwrap()
        .definition_level_encoding = Encoding::DELTA_BINARY_PACKED;
    write_declared_payload(
        &fixture.path,
        &header,
        &DeclaredFile {
            rows: 1,
            columns: 1,
            first_offset: 4,
        },
        &[1, 0, 0, 0, b'a'],
    );

    let result = validate(&fixture.path);

    assert!(matches!(
        result,
        Err(Error::Bound("parquet level encoding"))
    ));
}

#[test]
fn refuses_unsupported_repetition_level_encodings() {
    // Goal: repetition levels obey the same wire-encoding contract as definition levels.
    // Method: give an otherwise ordinary plain data page delta-encoded repetition levels.
    let fixture = Fixture::new();
    let mut header = page_header(1);
    header
        .data_page_header
        .as_mut()
        .unwrap()
        .repetition_level_encoding = Encoding::DELTA_BINARY_PACKED;
    write_declared_payload(
        &fixture.path,
        &header,
        &DeclaredFile {
            rows: 1,
            columns: 1,
            first_offset: 4,
        },
        &[1, 0, 0, 0, b'a'],
    );

    let result = validate(&fixture.path);

    assert!(matches!(
        result,
        Err(Error::Bound("parquet level encoding"))
    ));
}

impl Fixture {
    fn new() -> Self {
        Self::with_frame(
            DataFrame::new(2, vec![Column::new("counter".into(), [0u64, u64::MAX])]).unwrap(),
        )
    }

    fn with_frame(mut frame: DataFrame) -> Self {
        run_refusal_child();
        let next = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "flightdeck-parser-admission-{}-{next}",
            std::process::id()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let path = root.join("source.parquet");
        ParquetWriter::new(std::fs::File::create_new(&path).unwrap())
            .finish(&mut frame)
            .unwrap();
        Self { root, path }
    }

    fn query(&self) -> DataFrame {
        let engine = Engine::new();
        let source = engine.parquet_source(&self.path).unwrap();
        engine.query(&format!("SELECT * FROM {source}")).unwrap()
    }

    fn encodings(&self) -> Vec<Encoding> {
        let file = std::fs::File::open(&self.path).unwrap();
        let size = file.metadata().unwrap().len();
        let mut reader = BufReader::with_capacity(8192, file);
        let footer = validate_footer(&mut reader, size).unwrap();
        let metadata = validate_metadata(&mut reader, &footer).unwrap();
        let encodings: Vec<_> = metadata
            .row_groups
            .iter()
            .flat_map(|group| &group.columns)
            .flat_map(|column| &column.meta_data.as_ref().unwrap().encodings)
            .copied()
            .collect();
        eprintln!("writer column encodings: {encodings:?}");
        encodings
    }

    fn rewrite(
        &self,
        change: impl FnOnce(&mut ThriftFileMetadata),
        replacement: Option<(&[u8], &[u8])>,
    ) {
        let file = std::fs::File::open(&self.path).unwrap();
        let size = file.metadata().unwrap().len();
        let mut reader = BufReader::with_capacity(8192, file);
        let footer = validate_footer(&mut reader, size).unwrap();
        let mut metadata = validate_metadata(&mut reader, &footer).unwrap();
        change(&mut metadata);
        let mut encoded = Vec::new();
        metadata
            .write_to_out_protocol(&mut TCompactOutputProtocol::new(&mut encoded))
            .unwrap();
        if let Some((original, malformed)) = replacement {
            let positions: Vec<_> = encoded
                .windows(original.len())
                .enumerate()
                .filter_map(|(index, bytes)| (bytes == original).then_some(index))
                .collect();
            assert_eq!(positions.len(), 1);
            let start = positions[0];
            encoded.splice(start..start + original.len(), malformed.iter().copied());
        }

        let mut bytes = std::fs::read(&self.path).unwrap();
        bytes.truncate(usize::try_from(footer.start).unwrap());
        bytes.extend_from_slice(&encoded);
        bytes.extend_from_slice(&u32::try_from(encoded.len()).unwrap().to_le_bytes());
        bytes.extend_from_slice(b"PAR1");
        std::fs::write(&self.path, bytes).unwrap();
    }

    /// Tags every dictionary-indexed data page, and the footer, with the legacy encoding.
    fn relabel_dictionary_pages(&self) {
        let file = std::fs::File::open(&self.path).unwrap();
        let size = file.metadata().unwrap().len();
        let mut reader = BufReader::with_capacity(8192, file);
        let footer = validate_footer(&mut reader, size).unwrap();
        let metadata = validate_metadata(&mut reader, &footer).unwrap();
        let mut bytes = std::fs::read(&self.path).unwrap();

        let mut relabeled = 0;
        for column in metadata.row_groups.iter().flat_map(|group| &group.columns) {
            let chunk = column.meta_data.as_ref().unwrap();
            let start = chunk
                .dictionary_page_offset
                .unwrap_or(chunk.data_page_offset);
            let end = start + chunk.total_compressed_size;
            relabeled += relabel_dictionary_pages_between(&mut bytes, start, end);
        }
        assert!(relabeled > 0);
        std::fs::write(&self.path, bytes).unwrap();

        self.rewrite(
            |metadata| {
                for column in metadata
                    .row_groups
                    .iter_mut()
                    .flat_map(|group| &mut group.columns)
                {
                    for encoding in &mut column.meta_data.as_mut().unwrap().encodings {
                        if *encoding == Encoding::RLE_DICTIONARY {
                            *encoding = Encoding::PLAIN_DICTIONARY;
                        }
                    }
                }
            },
            None,
        );
    }

    fn write_page(&self, encoding: Encoding, rows: i32, payload: &[u8]) {
        let mut header = page_header(rows);
        header.compressed_page_size = i32::try_from(payload.len()).unwrap();
        header.uncompressed_page_size = header.compressed_page_size;
        header.data_page_header.as_mut().unwrap().encoding = encoding;
        write_declared_payload(
            &self.path,
            &header,
            &DeclaredFile {
                rows: i64::from(rows),
                columns: 1,
                first_offset: 4,
            },
            payload,
        );
    }

    fn assert_refused_without_aborting(&self, test: &str) {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", test, "--nocapture"])
            .env(CHILD_PATH, &self.path)
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(CHILD_SUCCESS), "{output:?}");
        assert!(
            validate(&self.path).is_err(),
            "malformed file passed admission"
        );
    }
}

fn dictionary_frame() -> DataFrame {
    let mut text = vec![Some("repeated recording value"); 1024];
    let mut counters = vec![Some(u64::MAX); 1024];
    let mut flags = vec![Some(true); 1024];
    let mut bytes = vec![Some(b"\xff\\x41\0".as_slice()); 1024];
    text[0] = None;
    counters[0] = None;
    flags[0] = None;
    bytes[0] = None;
    DataFrame::new(
        1024,
        vec![
            Column::new("text".into(), text),
            Column::new("counter".into(), counters),
            Column::new("flag".into(), flags),
            Series::new("bytes".into(), bytes).into(),
        ],
    )
    .unwrap()
}

/// Relabels the dictionary-indexed data pages of one column chunk in place and counts them.
/// Both encoding tags take one byte, so every page keeps its offset.
fn relabel_dictionary_pages_between(bytes: &mut [u8], start: i64, end: i64) -> u64 {
    let mut position = usize::try_from(start).unwrap();
    let end = usize::try_from(end).unwrap();
    let mut relabeled = 0;

    for _page in 0..1024 {
        if position == end {
            return relabeled;
        }

        let mut cursor = Cursor::new(&bytes[position..end]);
        let mut header =
            PageHeader::read_from_in_protocol(&mut BoundedProtocol::new(&mut cursor, 1024))
                .unwrap();
        let header_bytes = usize::try_from(cursor.position()).unwrap();
        if let Some(data) = header.data_page_header.as_mut() {
            if data.encoding == Encoding::RLE_DICTIONARY {
                data.encoding = Encoding::PLAIN_DICTIONARY;
                relabeled += 1;
            }
        }
        let mut encoded = Vec::new();
        header
            .write_to_out_protocol(&mut TCompactOutputProtocol::new(&mut encoded))
            .unwrap();
        assert_eq!(encoded.len(), header_bytes);

        bytes[position..position + header_bytes].copy_from_slice(&encoded);
        position += header_bytes + usize::try_from(header.compressed_page_size).unwrap();
    }

    panic!("column chunk has over 1024 pages");
}

fn run_refusal_child() {
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        std::panic::set_hook(Box::new(|failure| {
            eprintln!("{failure}");
            std::process::abort();
        }));
        let engine = Engine::new();
        let result = engine
            .parquet_source(&PathBuf::from(path))
            .and_then(|source| engine.query(&format!("SELECT * FROM {source}")));
        assert!(result.is_err());
        std::process::exit(CHILD_SUCCESS);
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
