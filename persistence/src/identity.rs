use crate::engine::{Cells, Engine};
use polars::prelude::{BinaryType, BooleanType, DataFrame, Int32Type, UInt8Type, UInt64Type};

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
        CAST({availability} AS UTINYINT) AS source_usage_availability, \
        (publication IS NOT NULL AND pid IS NOT NULL) AS observation_present \
        FROM {source} LIMIT {maximum}"
    ))?;
    if u64::try_from(frame.height()).map_err(|_| Error::Bound("process identity rows"))?
        > IDENTITY_ROWS_MAX
    {
        return Err(Error::Bound("process identity rows"));
    }

    let columns = ValidationColumns::new(&frame)?;
    for row in 0..frame.height() {
        if columns.observation_present.optional(row)? != Some(true) {
            return Err(Error::Bound("process identity observation"));
        }

        normalize_validate_usage(&columns, row, table)?;

        if columns.identity_version.optional(row)? != Some(IDENTITY_VERSION) {
            return Err(Error::Bound("process identity version"));
        }

        let availability = columns
            .identity_availability
            .optional(row)?
            .ok_or(Error::Bound("process identity availability"))?;
        if availability > identity_availability::EXHAUSTED {
            return Err(Error::Bound("process identity availability"));
        }

        let unique = columns.process_unique_id.optional(row)?;
        let version = columns.pid_version.optional(row)?;
        if unique.is_some() != version.is_some() {
            return Err(Error::Bound("process creation identity"));
        }

        if availability == identity_availability::MEASURED {
            if unique.is_none() {
                return Err(Error::Bound("process creation identity"));
            }

            normalize_validate_program(&columns, row)?;
        } else {
            normalize_validate_program_unavailable(&columns, row)?;
        }
    }

    Ok(())
}

/// The columns of the validation query, each read whole in the type the identity fields'
/// storage types and the query's own expressions give it.
struct ValidationColumns<'frame> {
    identity_version: Cells<'frame, UInt8Type>,
    identity_availability: Cells<'frame, UInt8Type>,
    process_unique_id: Cells<'frame, UInt64Type>,
    pid_version: Cells<'frame, Int32Type>,
    executable_name: Cells<'frame, BinaryType>,
    app_path: Cells<'frame, BinaryType>,
    usage_availability: Cells<'frame, UInt8Type>,
    usage_present: Cells<'frame, BooleanType>,
    usage_absent: Cells<'frame, BooleanType>,
    source_usage_availability: Cells<'frame, UInt8Type>,
    observation_present: Cells<'frame, BooleanType>,
}

impl<'frame> ValidationColumns<'frame> {
    /// The columns of `frame`, by the names the validation query gives them.
    fn new(frame: &'frame DataFrame) -> Result<Self> {
        Ok(Self {
            identity_version: Cells::named(frame, "identity_version")?,
            identity_availability: Cells::named(frame, "identity_availability")?,
            process_unique_id: Cells::named(frame, "process_unique_id")?,
            pid_version: Cells::named(frame, "pid_version")?,
            executable_name: Cells::named(frame, "executable_name")?,
            app_path: Cells::named(frame, "app_path")?,
            usage_availability: Cells::named(frame, "usage_availability")?,
            usage_present: Cells::named(frame, "usage_present")?,
            usage_absent: Cells::named(frame, "usage_absent")?,
            source_usage_availability: Cells::named(frame, "source_usage_availability")?,
            observation_present: Cells::named(frame, "observation_present")?,
        })
    }
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

fn normalize_validate_usage(
    columns: &ValidationColumns<'_>,
    row: usize,
    table: Table,
) -> Result<()> {
    let availability = columns
        .usage_availability
        .optional(row)?
        .ok_or(Error::Bound("process usage availability"))?;
    let maximum = match table {
        Table::Events => availability::NOT_APPLICABLE,
        _ => availability::GONE,
    };

    if availability > maximum {
        return Err(Error::Bound("process usage availability"));
    }

    if columns.source_usage_availability.optional(row)? != Some(availability) {
        return Err(Error::Bound("event usage availability"));
    }

    let payload = if availability == availability::MEASURED {
        &columns.usage_present
    } else {
        &columns.usage_absent
    };

    if payload.optional(row)? != Some(true) {
        return Err(Error::Bound("process usage payload"));
    }

    Ok(())
}

fn normalize_validate_program(columns: &ValidationColumns<'_>, row: usize) -> Result<()> {
    let executable = columns
        .executable_name
        .optional(row)?
        .ok_or(Error::Bound("process executable bytes"))?;
    let app_path = columns.app_path.optional(row)?;

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

fn normalize_validate_program_unavailable(
    columns: &ValidationColumns<'_>,
    row: usize,
) -> Result<()> {
    if columns.executable_name.optional(row)?.is_some() {
        return Err(Error::Bound("unavailable process executable"));
    }

    if columns.app_path.optional(row)?.is_some() {
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
