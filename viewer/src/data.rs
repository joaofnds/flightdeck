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
        for (column, name) in columns.iter().enumerate() {
            let value = cell(row.get_ref(column)?, name)?;
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

fn cell(value: ValueRef<'_>, column: &str) -> Result<Option<String>> {
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
        ValueRef::Blob(value) if matches!(column, "executable_name" | "app_path") => {
            return Ok(Some(cell_bytes(value)?));
        }
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

fn cell_bytes(value: &[u8]) -> Result<String> {
    if value.len() > persistence::schema::IDENTITY_PATH_BYTES_MAX {
        return Err(Error::Bound("captured path bytes"));
    }

    let mut text = String::with_capacity(value.len());

    crate::byte_display::append(&mut text, value);
    assert!(text.len() <= value.len() * 4);

    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::{Table, connection, query};
    use crate::Cancellation;
    use std::sync::Arc;

    #[test]
    fn displays_captured_bytes_without_merging_invalid_and_literal_escape_names() {
        // Goal: Unicode remains readable and invalid bytes never become replacement characters.
        // Method: query a real BLOB with Unicode, invalid bytes, control and literal backslash.
        let bytes = b"Analyse \xc3\xa9 \xff \\xff \n";

        let result = captured(bytes).unwrap();

        assert_eq!(
            result.rows,
            vec![vec![Some("Analyse é \\xff \\\\xff \\x0a".to_owned())]]
        );
    }

    #[test]
    fn displays_a_maximum_capture_after_escaping_expands_it() {
        // Goal: supported raw-byte paths remain displayable beyond the ordinary text limit.
        // Method: display 4095 invalid UTF-8 bytes, whose exact escape is four times longer.
        let bytes = vec![0xff_u8; 4095];

        let result = captured(&bytes).unwrap();

        assert_eq!(result.rows, vec![vec![Some("\\xff".repeat(4095))]]);
    }

    #[test]
    fn refuses_captured_bytes_exceeding_the_capture_limit() {
        // Goal: path display cannot bypass the raw-byte capture bound.
        // Method: query one byte beyond the permitted executable/App size.
        let bytes = vec![b'a'; 4096];

        let result = captured(&bytes);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{result:?}"
        );
    }

    #[test]
    fn preserves_a_nullable_app_path() {
        // Goal: outside-bundle processes remain distinct from an empty or invented App path.
        // Method: query a typed NULL through the same bounded presentation adapter.
        let connection = connection().unwrap();
        let operation = Arc::new(Cancellation::default()).operation(0).unwrap();

        let result = query(&connection, "SELECT NULL::BLOB app_path", 1, &operation).unwrap();

        assert_eq!(result.rows, vec![vec![None]]);
    }

    #[test]
    fn refuses_unrelated_blob_columns() {
        // Goal: captured path handling does not make arbitrary binary readings printable.
        // Method: query a BLOB under an unrelated column name.
        let connection = connection().unwrap();
        let operation = Arc::new(Cancellation::default()).operation(0).unwrap();

        let result = query(&connection, "SELECT 'abc'::BLOB unrelated", 1, &operation);

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{result:?}"
        );
    }

    #[test]
    fn keeps_the_ordinary_text_cell_limit() {
        // Goal: the narrowly bounded path adapter does not expand every text cell's budget.
        // Method: query ordinary text one byte beyond its original bound.
        let connection = connection().unwrap();
        let operation = Arc::new(Cancellation::default()).operation(0).unwrap();

        let result = query(
            &connection,
            "SELECT repeat('a',4097) ordinary",
            1,
            &operation,
        );

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{result:?}"
        );
    }

    fn captured(bytes: &[u8]) -> persistence::Result<Table> {
        let connection = connection()?;
        connection.execute_batch("CREATE TABLE captured(executable_name BLOB)")?;
        connection.execute("INSERT INTO captured VALUES(?)", duckdb::params![bytes])?;
        let operation = Arc::new(Cancellation::default()).operation(0)?;
        query(
            &connection,
            "SELECT executable_name FROM captured",
            1,
            &operation,
        )
    }
}
