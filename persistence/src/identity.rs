use crate::engine::{Engine, Row};
use polars::prelude::AnyValue;

use crate::schema::{
    IDENTITY_FIELDS, IDENTITY_ROWS_MAX, IDENTITY_VERSION, Table, availability,
    identity_availability,
};
use crate::{Error, Result};

/// The most columns a process or event source has once every optional extension is in it.
pub(crate) const COLUMNS_MAX: usize = 32;

pub(crate) fn normalize(connection: &Engine, table: Table, source: &str) -> Result<String> {
    let schema = connection.schema(source)?;
    if schema.len() > COLUMNS_MAX {
        return Err(Error::Bound("table column count"));
    }

    let mut fields = [false; IDENTITY_FIELDS.len()];
    for (name, data_type) in schema.iter() {
        if name.len() > 128 {
            return Err(Error::Bound("table column name"));
        }

        for (index, (field, kind)) in IDENTITY_FIELDS.iter().enumerate() {
            if name.eq_ignore_ascii_case(field) {
                if name.as_str() != *field {
                    return Err(Error::Bound("process identity field spelling"));
                }

                if crate::query::storage_type(data_type)? != *kind {
                    return Err(Error::Bound("process identity storage type"));
                }

                fields[index] = true;
            }
        }
    }

    let present = fields.into_iter().filter(|present| *present).count();
    if present == 0 {
        if schema.len() + IDENTITY_FIELDS.len() > COLUMNS_MAX {
            return Err(Error::Bound("normalized table column count"));
        }

        return Ok(normalize_legacy_source(table, source));
    }

    if present != IDENTITY_FIELDS.len() {
        return Err(Error::Bound("incomplete process identity extension"));
    }

    normalize_validate(connection, table, source)?;

    Ok(source.to_owned())
}

fn normalize_validate(connection: &Engine, table: Table, source: &str) -> Result<()> {
    let maximum = IDENTITY_ROWS_MAX + 1;
    let payload = normalize_validate_payload(table);
    let availability = match table {
        Table::Events => "availability",
        _ => "usage_availability",
    };

    let frame = connection.query(&format!(
        "SELECT identity_version, identity_availability, process_unique_id, pid_version, \
        executable_name, app_path, usage_availability, {payload}, \
        {availability} AS source_usage_availability, \
        (publication IS NOT NULL AND pid IS NOT NULL) AS observation_present \
        FROM {source} LIMIT {maximum}"
    ))?;
    if u64::try_from(frame.height()).map_err(|_| Error::Bound("process identity rows"))?
        > IDENTITY_ROWS_MAX
    {
        return Err(Error::Bound("process identity rows"));
    }

    for index in 0..frame.height() {
        let row = Row::new(&frame, index)?;

        if !row.get::<bool>(10)? {
            return Err(Error::Bound("process identity observation"));
        }

        normalize_validate_usage(&row, table)?;

        if row.get::<Option<u8>>(0)? != Some(IDENTITY_VERSION) {
            return Err(Error::Bound("process identity version"));
        }

        let availability = row
            .get::<Option<u8>>(1)?
            .ok_or(Error::Bound("process identity availability"))?;
        if availability > identity_availability::EXHAUSTED {
            return Err(Error::Bound("process identity availability"));
        }

        let unique = row.get::<Option<u64>>(2)?;
        let version = row.get::<Option<i32>>(3)?;
        if unique.is_some() != version.is_some() {
            return Err(Error::Bound("process creation identity"));
        }

        if availability == identity_availability::MEASURED {
            if unique.is_none() {
                return Err(Error::Bound("process creation identity"));
            }

            normalize_validate_program(&row)?;
        } else {
            normalize_validate_program_unavailable(&row)?;
        }
    }

    Ok(())
}

fn normalize_validate_payload(table: Table) -> String {
    assert!(matches!(table, Table::Processes | Table::Events));

    let fields: Vec<_> = table
        .columns()
        .skip_while(|field| *field != "awake_ns_at_start")
        .collect();
    assert!(!fields.is_empty());

    let known = fields
        .iter()
        .map(|field| format!("{field} IS NOT NULL"))
        .collect::<Vec<_>>()
        .join(" AND ");
    let absent = fields
        .iter()
        .map(|field| format!("{field} IS NULL"))
        .collect::<Vec<_>>()
        .join(" AND ");

    format!("({known}) AS usage_present, ({absent}) AS usage_absent")
}

fn normalize_validate_usage(row: &Row<'_>, table: Table) -> Result<()> {
    let availability = row
        .get::<Option<u8>>(6)?
        .ok_or(Error::Bound("process usage availability"))?;
    let maximum = match table {
        Table::Events => availability::NOT_APPLICABLE,
        _ => availability::GONE,
    };

    if availability > maximum {
        return Err(Error::Bound("process usage availability"));
    }

    if row.get::<Option<u8>>(9)? != Some(availability) {
        return Err(Error::Bound("event usage availability"));
    }

    let payload = if availability == availability::MEASURED {
        7
    } else {
        8
    };

    if !row.get::<bool>(payload)? {
        return Err(Error::Bound("process usage payload"));
    }

    Ok(())
}

fn normalize_validate_program(row: &Row<'_>) -> Result<()> {
    let executable_value = row.value(4)?;
    let executable = match &executable_value {
        AnyValue::Binary(value) => *value,
        AnyValue::BinaryOwned(value) => value.as_slice(),
        _ => return Err(Error::Bound("process executable bytes")),
    };

    let app_value = row.value(5)?;
    let app_path = match &app_value {
        AnyValue::Null => None,
        AnyValue::Binary(path) => Some(*path),
        AnyValue::BinaryOwned(path) => Some(path.as_slice()),
        _ => return Err(Error::Bound("process app bytes")),
    };

    collector::ring::ProgramBytes {
        executable_name: executable,
        app_path,
    }
    .validate()
    .map_err(|error| {
        use collector::ring::ProgramBytesError;
        Error::Bound(match error {
            ProgramBytesError::ExecutableBytes => "process executable bytes",
            ProgramBytesError::ExecutableBasename => "process executable basename",
            ProgramBytesError::AppBytes => "process app bytes",
            ProgramBytesError::AppPath => "process app path",
            ProgramBytesError::AppOutermost => "outermost process app",
        })
    })
}

fn normalize_validate_program_unavailable(row: &Row<'_>) -> Result<()> {
    if row.value(4)? != AnyValue::Null {
        return Err(Error::Bound("unavailable process executable"));
    }

    if row.value(5)? != AnyValue::Null {
        return Err(Error::Bound("unavailable process app"));
    }

    Ok(())
}

fn normalize_legacy_source(table: Table, source: &str) -> String {
    let unsupported = identity_availability::UNSUPPORTED;
    let usage = match table {
        Table::Events => "availability",
        _ => "0::UTINYINT",
    };

    format!(
        "(SELECT *, NULL::UTINYINT identity_version, \
        {unsupported}::UTINYINT identity_availability, \
        {usage} usage_availability, NULL::UBIGINT process_unique_id, NULL::INTEGER pid_version, \
        NULL::BLOB executable_name, NULL::BLOB app_path FROM {source})"
    )
}
