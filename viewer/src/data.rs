use crate::cancellation::Operation;
use persistence::engine::Engine;
use persistence::{Error, Result};
use polars::prelude::{AnyValue, DataFrame};

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

pub(crate) fn connection() -> Engine {
    Engine::new()
}

pub(crate) fn query(
    connection: &Engine,
    sql: &str,
    maximum: usize,
    operation: &Operation,
) -> Result<Table> {
    assert!(maximum <= ROWS_MAX);
    operation.attach(connection)?;

    let frame = query_frame(connection, sql, maximum, operation)?;
    operation.check()?;

    let columns: Vec<_> = frame
        .columns()
        .iter()
        .map(|column| column.name().to_string())
        .collect();
    if columns.len() > persistence::schema::TICK_COLUMNS_MAX {
        return Err(Error::Bound("column count"));
    }

    let mut rows = Vec::new();
    let mut bytes = 0;
    if frame.height() > maximum {
        return Err(Error::Bound("query row limit"));
    }

    for index in 0..frame.height() {
        operation.check()?;

        let mut values = Vec::with_capacity(columns.len());
        for (column, name) in columns.iter().enumerate() {
            let value = cell(frame.columns()[column].get(index)?, name)?;
            bytes += value.as_ref().map_or(0, String::len);
            if bytes > 32 * 1024 * 1024 {
                return Err(Error::Bound("query text bytes"));
            }

            values.push(value);
        }
        rows.push(values);
    }

    Ok(Table { columns, rows })
}

fn query_frame(
    connection: &Engine,
    sql: &str,
    maximum: usize,
    operation: &Operation,
) -> Result<DataFrame> {
    assert!(maximum <= ROWS_MAX);
    let mut plan = connection.plan(sql)?;
    if plan.collect_schema()?.len() > persistence::schema::TICK_COLUMNS_MAX {
        return Err(Error::Bound("column count"));
    }

    let count = u32::try_from(maximum + 1).map_err(|_| Error::Bound("query row limit"))?;
    operation.check()?;

    connection.collect(plan.limit(count))
}

fn cell(value: AnyValue<'_>, column: &str) -> Result<Option<String>> {
    let text = match value {
        AnyValue::Null => return Ok(None),
        AnyValue::Boolean(value) => value.to_string(),
        AnyValue::Int8(value) => value.to_string(),
        AnyValue::Int16(value) => value.to_string(),
        AnyValue::Int32(value) => value.to_string(),
        AnyValue::Int64(value) => value.to_string(),
        AnyValue::Int128(value) => value.to_string(),
        AnyValue::UInt8(value) => value.to_string(),
        AnyValue::UInt16(value) => value.to_string(),
        AnyValue::UInt32(value) => value.to_string(),
        AnyValue::UInt64(value) => value.to_string(),
        AnyValue::Binary(value) if matches!(column, "executable_name" | "app_path") => {
            return Ok(Some(cell_bytes(value)?));
        }
        AnyValue::BinaryOwned(value) if matches!(column, "executable_name" | "app_path") => {
            return Ok(Some(cell_bytes(&value)?));
        }
        AnyValue::String(value) => value.to_owned(),
        AnyValue::StringOwned(value) => value.to_string(),
        AnyValue::Float32(value) if value.is_finite() => value.to_string(),
        AnyValue::Float64(value) if value.is_finite() => value.to_string(),
        AnyValue::Float32(_) | AnyValue::Float64(_) if column == "rpm" => return Ok(None),
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
    fn keeps_maximum_unsigned_counters_exact_and_unavailable_values_empty() {
        // Goal: native materialization preserves every counter bit and its unavailable state.
        // Method: project UInt64::MAX and NULL through the same table adapter used by pages.
        use polars::prelude::{Column, DataFrame};
        let connection = connection();
        let values = vec![Some(u64::MAX), None];
        connection
            .register(
                "counters",
                DataFrame::new(2, vec![Column::new("counter".into(), values)]).unwrap(),
            )
            .unwrap();
        let operation = Arc::new(Cancellation::default()).operation(0).unwrap();

        let result = query(&connection, "SELECT counter FROM counters", 2, &operation).unwrap();

        assert_eq!(
            result.rows,
            vec![vec![Some(u64::MAX.to_string())], vec![None]]
        );
    }

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
        let connection = connection();
        let operation = Arc::new(Cancellation::default()).operation(0).unwrap();

        let result = query(&connection, "SELECT NULL::BLOB app_path", 1, &operation).unwrap();

        assert_eq!(result.rows, vec![vec![None]]);
    }

    #[test]
    fn refuses_unrelated_blob_columns() {
        // Goal: captured path handling does not make arbitrary binary readings printable.
        // Method: query a BLOB under an unrelated column name.
        let connection = connection();
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
        let connection = connection();
        let operation = Arc::new(Cancellation::default()).operation(0).unwrap();

        let result = query(
            &connection,
            &format!("SELECT '{}' ordinary", "a".repeat(4097)),
            1,
            &operation,
        );

        assert!(
            matches!(result, Err(persistence::Error::Bound(_))),
            "{result:?}"
        );
    }

    fn captured(bytes: &[u8]) -> persistence::Result<Table> {
        use polars::prelude::{Column, DataFrame};
        let connection = connection();
        connection.register(
            "captured",
            DataFrame::new(1, vec![Column::new("executable_name".into(), &[bytes])])?,
        )?;
        let operation = Arc::new(Cancellation::default()).operation(0)?;
        query(
            &connection,
            "SELECT executable_name FROM captured",
            1,
            &operation,
        )
    }

    macro_rules! nonfinite_query_refusal {
        ($name:ident, $sql:literal) => {
            #[test]
            fn $name() {
                // Goal: fan-specific gap handling cannot admit other nonfinite readings.
                // Method: query a nonfinite value under an unrelated source column.
                let connection = connection();
                let operation = Arc::new(Cancellation::default()).operation(0).unwrap();

                let result = query(&connection, $sql, 1, &operation);

                assert!(matches!(
                    result,
                    Err(persistence::Error::Bound("unsupported or nonfinite cell"))
                ));
            }
        };
    }
    nonfinite_query_refusal!(refuses_nan_power, "SELECT 'NaN'::FLOAT power_watts");
    nonfinite_query_refusal!(
        refuses_infinite_temperature,
        "SELECT 'Infinity'::DOUBLE soc_celsius"
    );
    nonfinite_query_refusal!(
        refuses_nonfinite_unrelated_columns,
        "SELECT 'NaN'::DOUBLE unrelated"
    );

    #[test]
    fn keeps_a_nonfinite_double_rpm_cell_nullable() {
        // Goal: floating width does not change the nullable invalid fan gauge contract.
        // Method: query a nonfinite double through the real bounded table adapter.
        let connection = connection();
        let operation = Arc::new(Cancellation::default()).operation(0).unwrap();

        let result = query(&connection, "SELECT 'Infinity'::DOUBLE rpm", 1, &operation).unwrap();

        assert_eq!(result.rows, vec![vec![None]]);
    }
}
