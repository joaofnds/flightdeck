use polars_parquet_format::thrift::protocol::{
    TCompactOutputProtocol, TListIdentifier, TOutputProtocol, TType,
};
use polars_parquet_format::{
    ColumnChunk, ColumnMetaData, CompressionCodec, Encoding, FieldRepetitionType, FileMetaData,
    RowGroup, SchemaElement, Type,
};

pub(super) fn metadata(names: &[&str], groups: usize) -> FileMetaData {
    assert!(names.len() <= crate::engine::admission::COLUMNS_MAX);
    assert!(groups <= 24);
    let mut schema = vec![SchemaElement {
        type_: None,
        type_length: None,
        repetition_type: None,
        name: "root".into(),
        num_children: Some(i32::try_from(names.len()).unwrap()),
        converted_type: None,
        scale: None,
        precision: None,
        field_id: None,
        logical_type: None,
    }];
    for name in names {
        schema.push(SchemaElement {
            type_: Some(Type::INT64),
            type_length: None,
            repetition_type: Some(FieldRepetitionType::REQUIRED),
            name: (*name).into(),
            num_children: None,
            converted_type: None,
            scale: None,
            precision: None,
            field_id: None,
            logical_type: None,
        });
    }
    let columns: Vec<_> = names.iter().map(|name| column(name)).collect();
    FileMetaData {
        version: 1,
        schema,
        num_rows: 0,
        row_groups: vec![
            RowGroup {
                columns,
                total_byte_size: 0,
                num_rows: 0,
                sorting_columns: None,
                file_offset: None,
                total_compressed_size: None,
                ordinal: None,
            };
            groups
        ],
        key_value_metadata: None,
        created_by: None,
        column_orders: None,
        encryption_algorithm: None,
        footer_signing_key_metadata: None,
    }
}

fn column(name: &str) -> ColumnChunk {
    ColumnChunk {
        file_path: None,
        file_offset: 4,
        meta_data: Some(ColumnMetaData {
            type_: Type::INT64,
            encodings: vec![Encoding::PLAIN],
            path_in_schema: vec![name.into()],
            codec: CompressionCodec::UNCOMPRESSED,
            num_values: 0,
            total_uncompressed_size: 0,
            total_compressed_size: 0,
            key_value_metadata: None,
            data_page_offset: 4,
            index_page_offset: None,
            dictionary_page_offset: None,
            statistics: None,
            encoding_stats: None,
            bloom_filter_offset: None,
            bloom_filter_length: None,
            size_statistics: None,
        }),
        offset_index_offset: None,
        offset_index_length: None,
        column_index_offset: None,
        column_index_length: None,
        crypto_metadata: None,
        encrypted_column_metadata: None,
    }
}

pub(super) fn encode(metadata: &FileMetaData) -> Vec<u8> {
    let mut footer = Vec::new();
    metadata
        .write_to_out_protocol(&mut TCompactOutputProtocol::new(&mut footer))
        .unwrap();
    footer
}

pub(super) fn file(footer: &[u8]) -> Vec<u8> {
    let mut file = b"PAR1".to_vec();
    file.extend_from_slice(footer);
    file.extend_from_slice(&u32::try_from(footer.len()).unwrap().to_le_bytes());
    file.extend_from_slice(b"PAR1");
    file
}

pub(super) fn replaced_values(metadata: &FileMetaData) -> Vec<u8> {
    replaced_list(metadata, 5)
}

pub(super) fn replaced_orders(metadata: &FileMetaData) -> Vec<u8> {
    replaced_list(metadata, 7)
}

fn replaced_list(metadata: &FileMetaData, identifier: i16) -> Vec<u8> {
    let mut footer = encode(metadata);
    assert_eq!(footer.pop(), Some(0));
    // A zero field delta makes the following signed identifier absolute.
    footer.push(0x09);
    let mut protocol = TCompactOutputProtocol::new(&mut footer);
    protocol.write_i16(identifier).unwrap();
    protocol
        .write_list_begin(&TListIdentifier::new(TType::Struct, 0))
        .unwrap();
    protocol.write_list_end().unwrap();
    protocol.write_field_stop().unwrap();
    footer
}
