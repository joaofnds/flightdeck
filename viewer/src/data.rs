use crate::cancellation::Operation;
use duckdb::{Connection, types::ValueRef};
use persistence::{Error, Result};

pub(crate) const ROWS_MAX: usize = 1_048_576;
const CELL_BYTES_MAX: usize = 4096;

#[derive(Clone, Debug)]
pub(crate) struct Table {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
}

impl Table {
    pub fn field<'a>(&self, row: &'a [Option<String>], name: &str) -> Result<Option<&'a str>> {
        let index = self
            .columns
            .iter()
            .position(|column| column == name)
            .ok_or(Error::Bound("missing schema column"))?;
        Ok(row.get(index).ok_or(Error::Bound("short row"))?.as_deref())
    }

    pub fn integer(&self, row: &[Option<String>], name: &str) -> Result<Option<u64>> {
        self.field(row, name)?
            .map(|value| value.parse().map_err(|_| Error::Bound("integer")))
            .transpose()
    }

    pub fn number(&self, row: &[Option<String>], name: &str) -> Result<Option<f64>> {
        let value = self
            .field(row, name)?
            .map(str::parse::<f64>)
            .transpose()
            .map_err(|_| Error::Bound("number"))?;
        if let Some(value) = value {
            if !value.is_finite() {
                return Err(Error::Bound("nonfinite reading"));
            }
        }
        Ok(value)
    }
}

pub(crate) fn connection() -> Result<Connection> {
    let config = duckdb::Config::default().threads(1)?;
    let connection = Connection::open_in_memory_with_flags(config)?;
    connection.execute_batch(
        "SET memory_limit='256MiB'; SET max_temp_directory_size='0B'; \
        SET autoinstall_known_extensions=false; SET autoload_known_extensions=false;",
    )?;
    Ok(connection)
}

pub(crate) fn query(
    connection: &Connection,
    sql: &str,
    maximum: usize,
    operation: &Operation,
) -> Result<Table> {
    assert!(maximum <= ROWS_MAX);
    operation.check()?;
    let mut statement = connection.prepare(sql)?;
    let mut cursor = statement.query([])?;
    let columns = cursor
        .as_ref()
        .ok_or(Error::Bound("query statement"))?
        .column_names();
    if columns.len() > 32 {
        return Err(Error::Bound("column count"));
    }
    let mut rows = Vec::new();
    let mut bytes = 0;
    for _index in 0..=maximum {
        operation.check()?;
        let Some(row) = cursor.next()? else {
            return Ok(Table { columns, rows });
        };
        if rows.len() == maximum {
            return Err(Error::Bound("query row limit"));
        }
        let mut values = Vec::with_capacity(columns.len());
        for column in 0..columns.len() {
            let value = cell(row.get_ref(column)?)?;
            bytes += value.as_ref().map_or(0, String::len);
            if bytes > 32 * 1024 * 1024 {
                return Err(Error::Bound("query text bytes"));
            }
            values.push(value);
        }
        rows.push(values);
    }
    Err(Error::Bound("query row limit"))
}

fn cell(value: ValueRef<'_>) -> Result<Option<String>> {
    let text = match value {
        ValueRef::Null => return Ok(None),
        ValueRef::Boolean(value) => value.to_string(),
        ValueRef::TinyInt(value) => value.to_string(),
        ValueRef::SmallInt(value) => value.to_string(),
        ValueRef::Int(value) => value.to_string(),
        ValueRef::BigInt(value) => value.to_string(),
        ValueRef::UTinyInt(value) => value.to_string(),
        ValueRef::USmallInt(value) => value.to_string(),
        ValueRef::UInt(value) => value.to_string(),
        ValueRef::UBigInt(value) => value.to_string(),
        ValueRef::Text(value) => std::str::from_utf8(value)
            .map_err(|_| Error::Bound("text encoding"))?
            .to_owned(),
        ValueRef::Float(value) if value.is_finite() => value.to_string(),
        ValueRef::Double(value) if value.is_finite() => value.to_string(),
        _ => return Err(Error::Bound("unsupported or nonfinite cell")),
    };
    if text.len() > CELL_BYTES_MAX {
        return Err(Error::Bound("cell bytes"));
    }
    Ok(Some(text))
}
