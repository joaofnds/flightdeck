use duckdb::{Connection, types::ValueRef};

use crate::schema::{
    IDENTITY_FIELDS, IDENTITY_ROWS_MAX, IDENTITY_VERSION, Table, availability,
    identity_availability,
};
use crate::{Error, Result};

pub(crate) fn normalize(connection: &Connection, table: Table, source: &str) -> Result<String> {
    let mut statement = connection.prepare(&format!("DESCRIBE SELECT * FROM {source}"))?;
    let mut rows = statement.query([])?;
    let mut fields = [false; IDENTITY_FIELDS.len()];

    for index in 0..=32 {
        let Some(row) = rows.next()? else {
            let present = fields.into_iter().filter(|present| *present).count();
            if present == 0 {
                if index + IDENTITY_FIELDS.len() > 32 {
                    return Err(Error::Bound("normalized table column count"));
                }

                return Ok(normalize_legacy_source(table, source));
            }

            if present != IDENTITY_FIELDS.len() {
                return Err(Error::Bound("incomplete process identity extension"));
            }

            normalize_validate(connection, table, source)?;
            return Ok(source.to_owned());
        };

        if index == 32 {
            return Err(Error::Bound("table column count"));
        }

        let ValueRef::Text(name) = row.get_ref(0)? else {
            return Err(Error::Bound("table column metadata"));
        };

        if name.len() > 128 {
            return Err(Error::Bound("table column name"));
        }

        for (index, (field, kind)) in IDENTITY_FIELDS.iter().enumerate() {
            if name.eq_ignore_ascii_case(field.as_bytes()) {
                if name != field.as_bytes() {
                    return Err(Error::Bound("process identity field spelling"));
                }

                if row.get_ref(1)? != ValueRef::Text(kind.as_bytes()) {
                    return Err(Error::Bound("process identity storage type"));
                }

                fields[index] = true;
            }
        }
    }

    Err(Error::Bound("table column count"))
}

fn normalize_validate(connection: &Connection, table: Table, source: &str) -> Result<()> {
    let maximum = IDENTITY_ROWS_MAX + 1;
    let payload = normalize_validate_payload(table);
    let availability = match table {
        Table::Events => "availability",
        _ => "usage_availability",
    };

    let mut statement = connection.prepare(&format!(
        "SELECT identity_version, identity_availability, process_unique_id, pid_version, \
        executable_name, app_path, usage_availability, {payload}, {availability}, \
        (publication IS NOT NULL AND pid IS NOT NULL) \
        FROM {source} LIMIT {maximum}"
    ))?;
    let mut rows = statement.query([])?;

    for index in 0..=IDENTITY_ROWS_MAX {
        let Some(row) = rows.next()? else {
            return Ok(());
        };

        if index == IDENTITY_ROWS_MAX {
            return Err(Error::Bound("process identity rows"));
        }

        if !row.get::<_, bool>(10)? {
            return Err(Error::Bound("process identity observation"));
        }

        normalize_validate_usage(row, table)?;

        if row.get::<_, Option<u8>>(0)? != Some(IDENTITY_VERSION) {
            return Err(Error::Bound("process identity version"));
        }

        let availability = row
            .get::<_, Option<u8>>(1)?
            .ok_or(Error::Bound("process identity availability"))?;
        if availability > identity_availability::EXHAUSTED {
            return Err(Error::Bound("process identity availability"));
        }

        let unique = row.get::<_, Option<u64>>(2)?;
        let version = row.get::<_, Option<i32>>(3)?;
        if unique.is_some() != version.is_some() {
            return Err(Error::Bound("process creation identity"));
        }

        if availability == identity_availability::MEASURED {
            if unique.is_none() {
                return Err(Error::Bound("process creation identity"));
            }

            normalize_validate_program(row)?;
        } else {
            normalize_validate_program_unavailable(row)?;
        }
    }

    Err(Error::Bound("process identity rows"))
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

    format!("({known}), ({absent})")
}

fn normalize_validate_usage(row: &duckdb::Row<'_>, table: Table) -> Result<()> {
    let availability = row
        .get::<_, Option<u8>>(6)?
        .ok_or(Error::Bound("process usage availability"))?;
    let maximum = match table {
        Table::Events => availability::NOT_APPLICABLE,
        _ => availability::GONE,
    };

    if availability > maximum {
        return Err(Error::Bound("process usage availability"));
    }

    if row.get::<_, Option<u8>>(9)? != Some(availability) {
        return Err(Error::Bound("event usage availability"));
    }

    let payload = if availability == availability::MEASURED {
        7
    } else {
        8
    };

    if !row.get::<_, bool>(payload)? {
        return Err(Error::Bound("process usage payload"));
    }

    Ok(())
}

fn normalize_validate_program(row: &duckdb::Row<'_>) -> Result<()> {
    let ValueRef::Blob(executable) = row.get_ref(4)? else {
        return Err(Error::Bound("process executable bytes"));
    };

    let app_path = match row.get_ref(5)? {
        ValueRef::Null => None,
        ValueRef::Blob(path) => Some(path),
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

fn normalize_validate_program_unavailable(row: &duckdb::Row<'_>) -> Result<()> {
    if row.get_ref(4)? != ValueRef::Null {
        return Err(Error::Bound("unavailable process executable"));
    }

    if row.get_ref(5)? != ValueRef::Null {
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
