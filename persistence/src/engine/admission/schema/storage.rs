//! Compact scalar tags from metadata already accepted by the bounded schema validator.
use super::{
    ConvertedType, Error, LogicalType, Result, SchemaElement, ThriftFileMetadata, Type,
    arrow_error, converted_integer, ipc, with_arrow_schema,
};

#[derive(Clone, Copy)]
#[repr(u8)]
pub(crate) enum Storage {
    Boolean,
    UInt8,
    UInt16,
    UInt32,
    UInt64,
    Int8,
    Int16,
    Int32,
    Int64,
    Float32,
    Float64,
    Binary,
    BinaryOffset,
    String,
}

impl Storage {
    pub(crate) fn data_type(self) -> polars::prelude::DataType {
        use polars::prelude::DataType;
        match self {
            Self::Boolean => DataType::Boolean,
            Self::UInt8 => DataType::UInt8,
            Self::UInt16 => DataType::UInt16,
            Self::UInt32 => DataType::UInt32,
            Self::UInt64 => DataType::UInt64,
            Self::Int8 => DataType::Int8,
            Self::Int16 => DataType::Int16,
            Self::Int32 => DataType::Int32,
            Self::Int64 => DataType::Int64,
            Self::Float32 => DataType::Float32,
            Self::Float64 => DataType::Float64,
            Self::Binary => DataType::Binary,
            Self::BinaryOffset => DataType::BinaryOffset,
            Self::String => DataType::String,
        }
    }
}

pub(crate) fn storage_types(
    metadata: &ThriftFileMetadata,
) -> Result<[Storage; super::super::COLUMNS_MAX]> {
    let physical = &metadata.schema[1..];
    assert!(physical.len() <= super::super::COLUMNS_MAX);
    let mut types = [Storage::Boolean; super::super::COLUMNS_MAX];
    for (index, field) in physical.iter().enumerate() {
        types[index] = storage_types_physical(field)?;
    }
    if let Some(entries) = &metadata.key_value_metadata {
        for entry in entries {
            if entry.key == "ARROW:schema" {
                if let Some(encoded) = &entry.value {
                    storage_types_arrow(encoded, &mut types[..physical.len()])?;
                }
            }
        }
    }

    Ok(types)
}

fn storage_types_physical(field: &SchemaElement) -> Result<Storage> {
    // The native decoder prefers logical annotations, then converted annotations.
    if let Some(LogicalType::INTEGER(integer)) = &field.logical_type {
        return integer_storage(i32::from(integer.bit_width), integer.is_signed);
    }

    if matches!(field.logical_type, Some(LogicalType::STRING(_))) {
        return Ok(Storage::String);
    }

    if let Some(converted) = field.converted_type {
        if let Some(integer) = converted_integer(converted) {
            return integer_storage(integer.width_bits, integer.signed);
        }
        if converted == ConvertedType::UTF8 {
            return Ok(Storage::String);
        }
    }

    match field.type_ {
        Some(Type::BOOLEAN) => Ok(Storage::Boolean),
        Some(Type::INT32) => Ok(Storage::Int32),
        Some(Type::INT64) => Ok(Storage::Int64),
        Some(Type::FLOAT) => Ok(Storage::Float32),
        Some(Type::DOUBLE) => Ok(Storage::Float64),
        Some(Type::BYTE_ARRAY) => Ok(Storage::Binary),
        _ => Err(Error::Bound("parquet scalar type")),
    }
}

fn integer_storage(width: i32, signed: bool) -> Result<Storage> {
    match (width, signed) {
        (8, false) => Ok(Storage::UInt8),
        (16, false) => Ok(Storage::UInt16),
        (32, false) => Ok(Storage::UInt32),
        (64, false) => Ok(Storage::UInt64),
        (8, true) => Ok(Storage::Int8),
        (16, true) => Ok(Storage::Int16),
        (32, true) => Ok(Storage::Int32),
        (64, true) => Ok(Storage::Int64),
        _ => Err(Error::Bound("parquet scalar annotations")),
    }
}

fn storage_types_arrow(encoded: &str, types: &mut [Storage]) -> Result<()> {
    // Admission already validated the whole override, including its names and physical types.
    // This second borrowed pass runs under the same cursor scratch allowance and keeps no IPC.
    with_arrow_schema(encoded, |schema| storage_types_arrow_schema(schema, types))
}

fn storage_types_arrow_schema(schema: &ipc::SchemaRef<'_>, types: &mut [Storage]) -> Result<()> {
    let fields = schema
        .fields()
        .map_err(arrow_error)?
        .ok_or(Error::Bound("parquet Arrow schema metadata"))?;
    if fields.len() != types.len() {
        return Err(Error::Bound("parquet Arrow schema metadata"));
    }

    for (index, field) in fields.iter().enumerate() {
        let field = field.map_err(arrow_error)?;
        let kind = field
            .type_()
            .map_err(arrow_error)?
            .ok_or(Error::Bound("parquet Arrow schema metadata"))?;
        types[index] = storage_types_arrow_schema_kind(&kind, &field)?;
    }

    Ok(())
}

fn storage_types_arrow_schema_kind(
    kind: &ipc::TypeRef<'_>,
    field: &ipc::FieldRef<'_>,
) -> Result<Storage> {
    match kind {
        ipc::TypeRef::Int(integer) => integer_storage(
            integer.bit_width().map_err(arrow_error)?,
            integer.is_signed().map_err(arrow_error)?,
        ),
        ipc::TypeRef::FloatingPoint(float) => match float.precision().map_err(arrow_error)? {
            ipc::Precision::Single => Ok(Storage::Float32),
            ipc::Precision::Double => Ok(Storage::Float64),
            ipc::Precision::Half => Err(Error::Bound("parquet Arrow schema metadata")),
        },
        ipc::TypeRef::Bool(_) => Ok(Storage::Boolean),
        ipc::TypeRef::LargeBinary(_)
            if storage_types_arrow_schema_kind_maintains_binary(field)? =>
        {
            Ok(Storage::BinaryOffset)
        }
        ipc::TypeRef::Binary(_) | ipc::TypeRef::LargeBinary(_) | ipc::TypeRef::BinaryView(_) => {
            Ok(Storage::Binary)
        }
        ipc::TypeRef::Utf8(_) | ipc::TypeRef::LargeUtf8(_) | ipc::TypeRef::Utf8View(_) => {
            Ok(Storage::String)
        }
        _ => Err(Error::Bound("parquet Arrow schema metadata")),
    }
}

fn storage_types_arrow_schema_kind_maintains_binary(field: &ipc::FieldRef<'_>) -> Result<bool> {
    use polars_arrow::datatypes::{MAINTAIN_PL_TYPE, PL_KEY};

    let mut maintain = false;
    if let Some(entries) = field.custom_metadata().map_err(arrow_error)? {
        for entry in entries {
            let entry = entry.map_err(arrow_error)?;
            if let (Some(key), Some(value)) = (
                entry.key().map_err(arrow_error)?,
                entry.value().map_err(arrow_error)?,
            ) {
                // Native IPC skips incomplete pairs and lets the last complete key win.
                if key == PL_KEY {
                    maintain = value == MAINTAIN_PL_TYPE;
                }
            }
        }
    }

    Ok(maintain)
}
