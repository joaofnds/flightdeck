use std::io::{BufReader, Write};
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use polars::polars_utils::compression::ZstdLevel;
use polars::prelude::{
    Column, DataFrame, NamedFrom, ParquetCompression, ParquetWriter, Series, StatisticsOptions,
};
use polars_arrow_format::ipc;
use polars_parquet::parquet::metadata::ThriftFileMetadata;
use polars_parquet_format::thrift::protocol::TCompactOutputProtocol;
use polars_parquet_format::{
    ConvertedType, DecimalType, FieldRepetitionType, KeyValue, LogicalType,
};

use super::{validate_footer, validate_metadata};
use crate::Error;
use crate::engine::{Engine, Row};

const CHILD_PATH: &str = "FLIGHTDECK_ARROW_METADATA_CHILD_PATH";
const CHILD_SUCCESS: i32 = 71;
static NEXT_DIRECTORY: AtomicU64 = AtomicU64::new(0);

struct Fixture {
    root: PathBuf,
    path: PathBuf,
}

struct ArrowProbe<'a> {
    test: &'static str,
    metadata: &'a str,
}

#[test]
fn reads_current_arrow_metadata_without_changing_saved_counter_types() {
    // Goal: the admission guard preserves valid current Arrow schema metadata.
    // Method: query native saved unsigned counters, including their full-width maximum.
    let fixture = Fixture::new();

    let values = fixture.counters();

    assert_eq!(values, [0, u64::MAX]);
}

#[test]
fn reads_legacy_raw_arrow_metadata_without_changing_saved_counter_types() {
    // Goal: raw IPC metadata accepted by the native reader remains accepted by admission.
    // Method: remove only the native continuation envelope and query the saved counters.
    let fixture = Fixture::new();
    fixture.change_arrow_metadata(|metadata| {
        let bytes = STANDARD.decode(metadata.value.as_ref().unwrap()).unwrap();
        assert_eq!(&bytes[..4], &[255; 4]);
        metadata.value = Some(STANDARD.encode(&bytes[8..]));
    });

    let values = fixture.counters();

    assert_eq!(values, [0, u64::MAX]);
}

#[test]
fn reads_a_nonnullable_arrow_override_for_optional_physical_storage() {
    // Goal: validating nullable offsets preserves native override and storage differences.
    // Method: declare a nonnullable Arrow counter above the writer's optional physical field.
    let fixture = Fixture::new();
    let encoded = arrow_fields_metadata(vec![arrow_counter_field()]);
    fixture.change_metadata(|metadata| {
        assert_eq!(
            metadata.schema[1].repetition_type,
            Some(FieldRepetitionType::OPTIONAL)
        );
        let entry = metadata
            .key_value_metadata
            .as_mut()
            .unwrap()
            .iter_mut()
            .find(|entry| entry.key == "ARROW:schema")
            .unwrap();
        entry.value = Some(encoded);
    });

    let values = fixture.counters();

    assert_eq!(values, [0, u64::MAX]);
}

#[test]
fn reads_all_recording_primitives_from_current_arrow_metadata() {
    // Goal: recording scalar types, optional cells and binary identity remain readable.
    // Method: save and query every recording primitive through the native source adapter.
    let expected = recording_frame();
    let fixture = Fixture::with_frame(expected.clone());

    let actual = fixture.query();

    assert_eq!(actual, expected);
}

#[test]
fn reads_all_recording_primitives_without_an_arrow_schema_override() {
    // Goal: legacy physical scalar annotations retain their exact recording types.
    // Method: remove Arrow metadata from a native recording and query every primitive.
    let expected = recording_frame();
    let fixture = Fixture::with_frame(expected.clone());
    fixture.change_metadata(|metadata| metadata.key_value_metadata = None);

    let actual = fixture.query();

    assert_eq!(actual, expected);
}

#[test]
fn refuses_empty_arrow_metadata_without_aborting_the_native_source_reader() {
    // Goal: an empty Arrow metadata value returns an error before native unchecked slicing.
    // Method: open a rewritten native file in an isolated child that aborts on any panic.
    assert_arrow_refusal(&ArrowProbe {
        test: "engine::admission::arrow_tests::\
               refuses_empty_arrow_metadata_without_aborting_the_native_source_reader",
        metadata: "",
    });
}

#[test]
fn refuses_truncated_arrow_continuation_without_aborting_the_native_source_reader() {
    // Goal: a continuation marker without its length returns an error before native slicing.
    // Method: open a rewritten native file in an isolated child that aborts on any panic.
    assert_arrow_refusal(&ArrowProbe {
        test: "engine::admission::arrow_tests::\
               refuses_truncated_arrow_continuation_without_aborting_the_native_source_reader",
        metadata: "/////w==",
    });
}

#[test]
fn refuses_run_end_encoded_arrow_metadata_without_aborting() {
    // Goal: a valid IPC tag cannot reach the native RunEndEncoded todo branch.
    // Method: install generated IPC metadata over a real scalar file in an aborting child.
    let encoded = arrow_kind_metadata(ipc::Type::RunEndEncoded(Box::new(ipc::RunEndEncoded {})));

    assert_arrow_refusal(&ArrowProbe {
        test: "engine::admission::arrow_tests::\
               refuses_run_end_encoded_arrow_metadata_without_aborting",
        metadata: &encoded,
    });
}

#[test]
fn refuses_list_view_arrow_metadata_without_aborting() {
    // Goal: a valid IPC tag cannot reach the native ListView todo branch.
    // Method: install generated IPC metadata over a real scalar file in an aborting child.
    let encoded = arrow_kind_metadata(ipc::Type::ListView(Box::new(ipc::ListView {})));

    assert_arrow_refusal(&ArrowProbe {
        test: "engine::admission::arrow_tests::\
               refuses_list_view_arrow_metadata_without_aborting",
        metadata: &encoded,
    });
}

#[test]
fn refuses_large_list_view_arrow_metadata_without_aborting() {
    // Goal: a valid IPC tag cannot reach the native LargeListView todo branch.
    // Method: install generated IPC metadata over a real scalar file in an aborting child.
    let encoded = arrow_kind_metadata(ipc::Type::LargeListView(Box::new(ipc::LargeListView {})));

    assert_arrow_refusal(&ArrowProbe {
        test: "engine::admission::arrow_tests::\
               refuses_large_list_view_arrow_metadata_without_aborting",
        metadata: &encoded,
    });
}

#[test]
fn refuses_a_supported_arrow_kind_with_different_physical_storage() {
    // Goal: an accepted IPC primitive cannot override its physical column storage.
    // Method: declare a Boolean Arrow field above a real unsigned 64-bit Parquet column.
    let fixture = Fixture::new();
    let encoded = arrow_kind_metadata(ipc::Type::Bool(Box::new(ipc::Bool {})));
    fixture.change_arrow_metadata(|metadata| metadata.value = Some(encoded));

    let result = fixture.query_result();

    assert!(matches!(
        result,
        Err(Error::Bound("parquet Arrow schema metadata"))
    ));
}

#[test]
fn refuses_arrow_integer_widths_that_disagree_with_logical_annotations() {
    // Goal: types sharing INT32 storage cannot silently narrow a recorded counter.
    // Method: override a real UInt32 column with a generated unsigned eight-bit IPC field.
    let frame = DataFrame::new(1, vec![Column::new("counter".into(), [u32::MAX])]).unwrap();
    let fixture = Fixture::with_frame(frame);
    let encoded = arrow_kind_metadata(ipc::Type::Int(Box::new(ipc::Int {
        bit_width: 8,
        is_signed: false,
    })));
    fixture.change_arrow_metadata(|metadata| metadata.value = Some(encoded));

    let result = fixture.query_result();

    assert!(matches!(
        result,
        Err(Error::Bound("parquet Arrow schema metadata"))
    ));
}

#[test]
fn refuses_arrow_integer_signedness_that_disagrees_with_logical_annotations() {
    // Goal: Arrow metadata cannot reinterpret unsigned full-width counters as signed.
    // Method: override native UInt64 metadata with an otherwise matching signed IPC field.
    let fixture = Fixture::new();
    let encoded = arrow_kind_metadata(ipc::Type::Int(Box::new(ipc::Int {
        bit_width: 64,
        is_signed: true,
    })));
    fixture.change_arrow_metadata(|metadata| metadata.value = Some(encoded));

    let result = fixture.query_result();

    assert!(matches!(
        result,
        Err(Error::Bound("parquet Arrow schema metadata"))
    ));
}

#[test]
fn refuses_arrow_names_that_disagree_with_physical_column_names() {
    // Goal: an IPC override cannot substitute another named column for a stored counter.
    // Method: change only the generated IPC field name above an otherwise valid native file.
    let fixture = Fixture::new();
    let mut field = arrow_counter_field();
    field.name = Some("other_counter".to_owned());
    let encoded = arrow_fields_metadata(vec![field]);
    fixture.change_arrow_metadata(|metadata| metadata.value = Some(encoded));

    let result = fixture.query_result();

    assert!(matches!(
        result,
        Err(Error::Bound("parquet Arrow schema metadata"))
    ));
}

#[test]
fn refuses_an_arrow_nullability_offset_outside_the_schema_buffer() {
    // Goal: malformed nullability returns an inline refusal before native schema inference.
    // Method: corrupt only the nullable slot in an otherwise valid scalar Arrow override.
    let fixture = Fixture::new();
    let encoded = arrow_invalid_nullability_metadata();
    fixture.change_arrow_metadata(|metadata| metadata.value = Some(encoded));

    let result = fixture.query_result();

    assert!(
        matches!(result, Err(Error::Bound("parquet Arrow schema metadata"))),
        "{result:?}"
    );
}

#[test]
fn refuses_arrow_extension_overrides_on_recording_scalars() {
    // Goal: primitive tags cannot introduce native extension conversion paths.
    // Method: attach native object extension metadata to an otherwise ordinary integer.
    let fixture = Fixture::new();
    let mut field = arrow_counter_field();
    field.custom_metadata = Some(vec![ipc::KeyValue {
        key: Some("ARROW:extension:name".to_owned()),
        value: Some("_POLARS_PYTHON_OBJECT".to_owned()),
    }]);
    let encoded = arrow_fields_metadata(vec![field]);
    fixture.change_arrow_metadata(|metadata| metadata.value = Some(encoded));

    let result = fixture.query_result();

    assert!(matches!(
        result,
        Err(Error::Bound("parquet Arrow schema metadata"))
    ));
}

#[test]
fn refuses_arrow_metadata_amplification_through_repeated_string_references() {
    // Goal: native metadata copying remains bounded even when IPC offsets share one string.
    // Method: serialize repeated metadata using the native builder's shared string cache.
    let fixture = Fixture::new();
    let mut field = arrow_counter_field();
    field.custom_metadata = Some(
        (0u8..32)
            .map(|index| ipc::KeyValue {
                key: Some(format!("key_{index}")),
                value: Some("x".repeat(4096)),
            })
            .collect(),
    );
    let encoded = arrow_fields_metadata(vec![field]);
    assert!(STANDARD.decode(&encoded).unwrap().len() < 8192);
    fixture.change_arrow_metadata(|metadata| metadata.value = Some(encoded));

    let result = fixture.query_result();

    assert!(matches!(
        result,
        Err(Error::Bound("parquet Arrow schema metadata"))
    ));
}

#[test]
fn refuses_physical_decimal_annotations_without_arrow_metadata() {
    // Goal: scalar physical storage cannot reach a disabled Decimal conversion path.
    // Method: rewrite native integer metadata with a valid Decimal annotation and no Arrow key.
    let fixture = Fixture::new();
    fixture.change_metadata(|metadata| {
        metadata.key_value_metadata = None;
        let field = &mut metadata.schema[1];
        field.converted_type = Some(ConvertedType::DECIMAL);
        field.logical_type = Some(LogicalType::DECIMAL(DecimalType::new(0, 18)));
        field.precision = Some(18);
        field.scale = Some(0);
    });

    let result = fixture.query_result();

    assert!(matches!(
        result,
        Err(Error::Bound("parquet scalar annotations"))
    ));
}

#[test]
fn refuses_physical_temporal_annotations_without_arrow_metadata() {
    // Goal: legacy converted types cannot turn recording integers into temporal values.
    // Method: attach a valid converted timestamp annotation without an Arrow schema override.
    let fixture = Fixture::new();
    fixture.change_metadata(|metadata| {
        metadata.key_value_metadata = None;
        metadata.schema[1].logical_type = None;
        metadata.schema[1].converted_type = Some(ConvertedType::TIMESTAMP_MICROS);
    });

    let result = fixture.query_result();

    assert!(matches!(
        result,
        Err(Error::Bound("parquet scalar annotations"))
    ));
}

fn assert_arrow_refusal(probe: &ArrowProbe<'_>) {
    if let Some(path) = std::env::var_os(CHILD_PATH) {
        std::panic::set_hook(Box::new(|failure| {
            eprintln!("{failure}");
            std::process::abort();
        }));
        let engine = Engine::new();

        let result = engine
            .parquet_source(&PathBuf::from(path))
            .and_then(|source| engine.query(&format!("SELECT * FROM {source}")));

        assert!(matches!(
            result,
            Err(Error::Bound("parquet Arrow schema metadata"))
        ));
        std::process::exit(CHILD_SUCCESS);
    }

    let fixture = Fixture::new();
    fixture.change_arrow_metadata(|metadata| metadata.value = Some(probe.metadata.to_owned()));

    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", probe.test])
        .env(CHILD_PATH, &fixture.path)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(CHILD_SUCCESS), "{output:?}");
}

impl Fixture {
    fn new() -> Self {
        Self::with_frame(
            DataFrame::new(2, vec![Column::new("counter".into(), [0u64, u64::MAX])]).unwrap(),
        )
    }

    fn with_frame(mut frame: DataFrame) -> Self {
        let next = NEXT_DIRECTORY.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "flightdeck-arrow-metadata-{}-{next}",
            std::process::id()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .unwrap();
        let path = root.join("source.parquet");
        ParquetWriter::new(std::fs::File::create_new(&path).unwrap())
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
        Self { root, path }
    }

    fn counters(&self) -> [u64; 2] {
        let engine = Engine::new();
        let source = engine.parquet_source(&self.path).unwrap();
        let frame = engine
            .query(&format!("SELECT counter FROM {source} ORDER BY counter"))
            .unwrap();
        assert_eq!(frame.height(), 2);
        [
            Row::new(&frame, 0).unwrap().get::<u64>(0).unwrap(),
            Row::new(&frame, 1).unwrap().get::<u64>(0).unwrap(),
        ]
    }

    fn query(&self) -> DataFrame {
        self.query_result().unwrap()
    }

    fn query_result(&self) -> crate::Result<DataFrame> {
        let engine = Engine::new();
        let source = engine.parquet_source(&self.path)?;
        engine.query(&format!("SELECT * FROM {source}"))
    }

    fn change_arrow_metadata(&self, change: impl FnOnce(&mut KeyValue)) {
        self.change_metadata(|metadata| {
            let arrow = metadata
                .key_value_metadata
                .as_mut()
                .unwrap()
                .iter_mut()
                .find(|metadata| metadata.key == "ARROW:schema")
                .unwrap();
            change(arrow);
        });
    }

    fn change_metadata(&self, change: impl FnOnce(&mut ThriftFileMetadata)) {
        let file = std::fs::File::open(&self.path).unwrap();
        let size = file.metadata().unwrap().len();
        let mut reader = BufReader::with_capacity(8192, file);
        let footer = validate_footer(&mut reader, size).unwrap();
        let mut metadata = validate_metadata(&mut reader, &footer).unwrap();
        change(&mut metadata);
        drop(reader);

        let mut bytes = std::fs::read(&self.path).unwrap();
        bytes.truncate(usize::try_from(footer.start).unwrap());
        let start = bytes.len();
        metadata
            .write_to_out_protocol(&mut TCompactOutputProtocol::new(&mut bytes))
            .unwrap();
        let length = u32::try_from(bytes.len() - start).unwrap();
        bytes.write_all(&length.to_le_bytes()).unwrap();
        bytes.write_all(b"PAR1").unwrap();
        std::fs::write(&self.path, bytes).unwrap();
    }
}

fn recording_frame() -> DataFrame {
    let bytes: &[u8] = b"\xff\\x41";
    DataFrame::new(
        2,
        vec![
            Column::new("small".into(), [Some(u8::MAX), None]),
            Column::new("medium".into(), [Some(u16::MAX), None]),
            Column::new("large".into(), [Some(u32::MAX), None]),
            Column::new("counter".into(), [Some(u64::MAX), None]),
            Column::new("signed".into(), [Some(i32::MIN), None]),
            Column::new("signed_wide".into(), [Some(i64::MIN), None]),
            Column::new("single".into(), [Some(1.25f32), None]),
            Column::new("double".into(), [Some(-4.5f64), None]),
            Column::new("flag".into(), [Some(true), None]),
            Column::new("text".into(), [Some("recording"), None]),
            Series::new("identity".into(), [Some(bytes), None]).into(),
        ],
    )
    .unwrap()
}

fn arrow_kind_metadata(kind: ipc::Type) -> String {
    let mut field = arrow_counter_field();
    field.type_ = Some(kind);
    arrow_fields_metadata(vec![field])
}

fn arrow_counter_field() -> ipc::Field {
    ipc::Field {
        name: Some("counter".to_owned()),
        nullable: false,
        type_: Some(ipc::Type::Int(Box::new(ipc::Int {
            bit_width: 64,
            is_signed: false,
        }))),
        dictionary: None,
        children: None,
        custom_metadata: None,
    }
}

fn arrow_fields_metadata(fields: Vec<ipc::Field>) -> String {
    let message = ipc::Message {
        version: ipc::MetadataVersion::V5,
        header: Some(ipc::MessageHeader::Schema(Box::new(ipc::Schema {
            endianness: ipc::Endianness::Little,
            fields: Some(fields),
            custom_metadata: None,
            features: None,
        }))),
        body_length: 0,
        custom_metadata: None,
    };
    STANDARD.encode(ipc::planus::Builder::new().finish(&message, None))
}

impl Drop for Fixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

struct ArrowTableSlot {
    table: usize,
    index: usize,
}

fn arrow_invalid_nullability_metadata() -> String {
    let mut field = arrow_counter_field();
    field.nullable = true;
    let mut bytes = STANDARD.decode(arrow_fields_metadata(vec![field])).unwrap();
    let message = arrow_invalid_nullability_indirect(&bytes, 0);
    let schema_field = arrow_invalid_nullability_slot(
        &bytes,
        &ArrowTableSlot {
            table: message,
            index: 2,
        },
    );
    let schema = arrow_invalid_nullability_indirect(&bytes, schema_field);
    let fields_field = arrow_invalid_nullability_slot(
        &bytes,
        &ArrowTableSlot {
            table: schema,
            index: 1,
        },
    );
    let fields = arrow_invalid_nullability_indirect(&bytes, fields_field);
    let field = arrow_invalid_nullability_indirect(&bytes, fields + 4);
    let distance = i32::from_le_bytes(bytes[field..field + 4].try_into().unwrap());
    let vtable = usize::try_from(i64::try_from(field).unwrap() - i64::from(distance)).unwrap();
    let nullable_slot = vtable + 6;
    assert_ne!(&bytes[nullable_slot..nullable_slot + 2], &[0, 0]);
    assert!(bytes.len() < usize::from(u16::MAX));
    bytes[nullable_slot..nullable_slot + 2].copy_from_slice(&u16::MAX.to_le_bytes());
    STANDARD.encode(bytes)
}

fn arrow_invalid_nullability_indirect(bytes: &[u8], position: usize) -> usize {
    let relative = u32::from_le_bytes(bytes[position..position + 4].try_into().unwrap());
    position + usize::try_from(relative).unwrap()
}

fn arrow_invalid_nullability_slot(bytes: &[u8], slot: &ArrowTableSlot) -> usize {
    let distance = i32::from_le_bytes(bytes[slot.table..slot.table + 4].try_into().unwrap());
    let vtable = usize::try_from(i64::try_from(slot.table).unwrap() - i64::from(distance)).unwrap();
    let position = vtable + 4 + 2 * slot.index;
    let relative = u16::from_le_bytes(bytes[position..position + 2].try_into().unwrap());
    assert!(relative > 0);
    slot.table + usize::from(relative)
}
