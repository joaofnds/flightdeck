//! Saved-schema type names, read as their exact Polars types.
use polars::prelude::DataType;

/// Reads a saved-schema type name, such as `UBIGINT`, as its exact Polars type.
/// Polars SQL reads `FLOAT` as a double, where the saved schema means a single.
pub fn data_type(kind: &str) -> DataType {
    match kind {
        "UTINYINT" => DataType::UInt8,
        "USMALLINT" => DataType::UInt16,
        "UINTEGER" => DataType::UInt32,
        "UBIGINT" => DataType::UInt64,
        "INTEGER" => DataType::Int32,
        "BIGINT" => DataType::Int64,
        "FLOAT" => DataType::Float32,
        "DOUBLE" => DataType::Float64,
        "BOOLEAN" => DataType::Boolean,
        "VARCHAR" => DataType::String,
        "BLOB" => DataType::Binary,
        _ => panic!("unknown saved type {kind}"),
    }
}
