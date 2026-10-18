//! Native borrowed IPC accessors admit only the recording's flat scalar schema.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use polars_arrow_format::ipc::{self, planus::ReadAsRoot};
use polars_parquet::parquet::metadata::ThriftFileMetadata;
use polars_parquet_format::{ConvertedType, LogicalType, SchemaElement, Type};

use crate::{Error, Result};

const METADATA_ENTRIES_MAX: usize = 32;
const METADATA_BYTES_MAX: usize = 64 * 1024;
const COLUMN_NAME_BYTES_MAX: usize = 128;

type MetadataEntries<'a> =
    Option<ipc::planus::Vector<'a, ipc::planus::Result<ipc::KeyValueRef<'a>>>>;

#[derive(PartialEq, Eq)]
struct Integer {
    width_bits: i32,
    signed: bool,
}

pub(super) fn validate_arrow(metadata: &ThriftFileMetadata) -> Result<()> {
    let physical = metadata
        .schema
        .get(1..)
        .ok_or(Error::Bound("parquet scalar schema"))?;
    if let Some(entries) = &metadata.key_value_metadata {
        for entry in entries {
            if entry.key == "ARROW:schema" {
                if let Some(encoded) = &entry.value {
                    validate_arrow_envelope(encoded, physical)?;
                }
            }
        }
    }

    Ok(())
}

pub(super) fn validate_physical(field: &SchemaElement) -> Result<()> {
    if field.type_length.is_some() {
        return Err(Error::Bound("parquet scalar annotations"));
    }
    if field.precision.is_some() {
        return Err(Error::Bound("parquet scalar annotations"));
    }
    if field.scale.is_some() {
        return Err(Error::Bound("parquet scalar annotations"));
    }
    validate_physical_logical(field)?;
    validate_physical_converted(field)
}

fn validate_arrow_envelope(encoded: &str, physical: &[SchemaElement]) -> Result<()> {
    // The enclosing footer is bounded before the native Base64 decoder allocates this buffer.
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| Error::Bound("parquet Arrow schema metadata"))?;
    if bytes.len() < 4 {
        return Err(Error::Bound("parquet Arrow schema metadata"));
    }
    let payload = if bytes[..4] == [255; 4] {
        bytes
            .get(8..)
            .ok_or(Error::Bound("parquet Arrow schema metadata"))?
    } else {
        bytes.as_slice()
    };

    validate_arrow_schema(payload, physical)
}

fn validate_arrow_schema(payload: &[u8], physical: &[SchemaElement]) -> Result<()> {
    let message = ipc::MessageRef::read_as_root(payload).map_err(arrow_error)?;
    let Some(ipc::MessageHeaderRef::Schema(schema)) = message.header().map_err(arrow_error)? else {
        return Err(Error::Bound("parquet Arrow schema metadata"));
    };
    if schema.endianness().map_err(arrow_error)? != ipc::Endianness::Little {
        return Err(Error::Bound("parquet Arrow schema metadata"));
    }
    let fields = schema
        .fields()
        .map_err(arrow_error)?
        .ok_or(Error::Bound("parquet Arrow schema metadata"))?;
    if fields.len() != physical.len() {
        return Err(Error::Bound("parquet Arrow schema metadata"));
    }
    if fields.len() > super::COLUMNS_MAX {
        return Err(Error::Bound("parquet Arrow schema metadata"));
    }
    let mut remaining = METADATA_BYTES_MAX;
    validate_arrow_metadata_entries(
        &schema.custom_metadata().map_err(arrow_error)?,
        &mut remaining,
    )?;
    for (index, field) in fields.iter().enumerate() {
        validate_arrow_field(
            &field.map_err(arrow_error)?,
            &physical[index],
            &mut remaining,
        )?;
    }

    Ok(())
}

fn validate_arrow_field(
    field: &ipc::FieldRef<'_>,
    physical: &SchemaElement,
    remaining: &mut usize,
) -> Result<()> {
    let name = field
        .name()
        .map_err(arrow_error)?
        .ok_or(Error::Bound("parquet Arrow schema metadata"))?;
    if name.len() > COLUMN_NAME_BYTES_MAX {
        return Err(Error::Bound("parquet Arrow schema metadata"));
    }
    if name != physical.name {
        return Err(Error::Bound("parquet Arrow schema metadata"));
    }
    if field.dictionary().map_err(arrow_error)?.is_some() {
        return Err(Error::Bound("parquet Arrow schema metadata"));
    }
    if let Some(children) = field.children().map_err(arrow_error)? {
        if !children.is_empty() {
            return Err(Error::Bound("parquet Arrow schema metadata"));
        }
    }
    validate_arrow_metadata_entries(&field.custom_metadata().map_err(arrow_error)?, remaining)?;
    let kind = field
        .type_()
        .map_err(arrow_error)?
        .ok_or(Error::Bound("parquet Arrow schema metadata"))?;
    validate_arrow_kind(&kind, physical)
}

fn validate_arrow_kind(kind: &ipc::TypeRef<'_>, physical: &SchemaElement) -> Result<()> {
    match kind {
        ipc::TypeRef::Int(integer) => validate_arrow_integer(integer, physical),
        ipc::TypeRef::FloatingPoint(float) => match float.precision().map_err(arrow_error)? {
            ipc::Precision::Single => validate_arrow_storage(Type::FLOAT, physical),
            ipc::Precision::Double => validate_arrow_storage(Type::DOUBLE, physical),
            ipc::Precision::Half => Err(Error::Bound("parquet Arrow schema metadata")),
        },
        ipc::TypeRef::Bool(_) => validate_arrow_storage(Type::BOOLEAN, physical),
        ipc::TypeRef::Binary(_) | ipc::TypeRef::LargeBinary(_) | ipc::TypeRef::BinaryView(_) => {
            validate_arrow_binary(physical)
        }
        ipc::TypeRef::Utf8(_) | ipc::TypeRef::LargeUtf8(_) | ipc::TypeRef::Utf8View(_) => {
            validate_arrow_storage(Type::BYTE_ARRAY, physical)
        }
        _ => Err(Error::Bound("parquet Arrow schema metadata")),
    }
}

fn validate_arrow_integer(integer: &ipc::IntRef<'_>, physical: &SchemaElement) -> Result<()> {
    let observed = Integer {
        width_bits: integer.bit_width().map_err(arrow_error)?,
        signed: integer.is_signed().map_err(arrow_error)?,
    };
    let storage = match observed.width_bits {
        8 | 16 | 32 => Type::INT32,
        64 => Type::INT64,
        _ => return Err(Error::Bound("parquet Arrow schema metadata")),
    };
    validate_arrow_storage(storage, physical)?;
    if let Some(LogicalType::INTEGER(expected)) = &physical.logical_type {
        if observed.width_bits != i32::from(expected.bit_width) {
            return Err(Error::Bound("parquet Arrow schema metadata"));
        }
        if observed.signed != expected.is_signed {
            return Err(Error::Bound("parquet Arrow schema metadata"));
        }
    }
    if let Some(converted) = physical.converted_type {
        if let Some(expected) = converted_integer(converted) {
            if observed != expected {
                return Err(Error::Bound("parquet Arrow schema metadata"));
            }
        }
    }

    Ok(())
}

fn validate_arrow_binary(physical: &SchemaElement) -> Result<()> {
    validate_arrow_storage(Type::BYTE_ARRAY, physical)?;
    if matches!(physical.logical_type, Some(LogicalType::STRING(_))) {
        return Err(Error::Bound("parquet Arrow schema metadata"));
    }
    if physical.converted_type == Some(ConvertedType::UTF8) {
        return Err(Error::Bound("parquet Arrow schema metadata"));
    }

    Ok(())
}

fn validate_arrow_storage(storage: Type, physical: &SchemaElement) -> Result<()> {
    if Some(storage) != physical.type_ {
        return Err(Error::Bound("parquet Arrow schema metadata"));
    }

    Ok(())
}

fn validate_arrow_metadata_entries(
    entries_option: &MetadataEntries<'_>,
    remaining: &mut usize,
) -> Result<()> {
    if let Some(entries) = entries_option {
        if entries.len() > METADATA_ENTRIES_MAX {
            return Err(Error::Bound("parquet Arrow schema metadata"));
        }
        for entry in *entries {
            let entry = entry.map_err(arrow_error)?;
            if let Some(key) = entry.key().map_err(arrow_error)? {
                if key.starts_with("ARROW:extension:") {
                    return Err(Error::Bound("parquet Arrow schema metadata"));
                }
                validate_arrow_metadata_bytes(key, remaining)?;
            }
            if let Some(value) = entry.value().map_err(arrow_error)? {
                validate_arrow_metadata_bytes(value, remaining)?;
            }
        }
    }

    Ok(())
}

fn validate_arrow_metadata_bytes(value: &str, remaining: &mut usize) -> Result<()> {
    *remaining = remaining
        .checked_sub(value.len())
        .ok_or(Error::Bound("parquet Arrow schema metadata"))?;
    Ok(())
}

fn validate_physical_logical(field: &SchemaElement) -> Result<()> {
    let kind = match &field.logical_type {
        None => return Ok(()),
        Some(LogicalType::STRING(_)) => Type::BYTE_ARRAY,
        Some(LogicalType::INTEGER(integer)) => match integer.bit_width {
            8 | 16 | 32 => Type::INT32,
            64 => Type::INT64,
            _ => return Err(Error::Bound("parquet scalar annotations")),
        },
        Some(_) => return Err(Error::Bound("parquet scalar annotations")),
    };
    if field.type_ != Some(kind) {
        return Err(Error::Bound("parquet scalar annotations"));
    }

    Ok(())
}

fn validate_physical_converted(field: &SchemaElement) -> Result<()> {
    let kind = match field.converted_type {
        None => return Ok(()),
        Some(ConvertedType::UTF8) => Type::BYTE_ARRAY,
        Some(converted) => match converted_integer(converted) {
            Some(integer) => match integer.width_bits {
                8 | 16 | 32 => Type::INT32,
                64 => Type::INT64,
                _ => return Err(Error::Bound("parquet scalar annotations")),
            },
            None => return Err(Error::Bound("parquet scalar annotations")),
        },
    };
    if field.type_ != Some(kind) {
        return Err(Error::Bound("parquet scalar annotations"));
    }

    Ok(())
}

fn converted_integer(kind: ConvertedType) -> Option<Integer> {
    let (width_bits, signed) = match kind {
        ConvertedType::INT_8 => (8, true),
        ConvertedType::INT_16 => (16, true),
        ConvertedType::INT_32 => (32, true),
        ConvertedType::INT_64 => (64, true),
        ConvertedType::UINT_8 => (8, false),
        ConvertedType::UINT_16 => (16, false),
        ConvertedType::UINT_32 => (32, false),
        ConvertedType::UINT_64 => (64, false),
        _ => return None,
    };

    Some(Integer { width_bits, signed })
}

fn arrow_error(_: ipc::planus::Error) -> Error {
    Error::Bound("parquet Arrow schema metadata")
}
