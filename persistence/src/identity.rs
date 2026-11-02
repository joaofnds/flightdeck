use std::ops::Range;

use crate::engine::{Cells, Engine};
use polars::prelude::{
    BinaryType, BooleanType, DataFrame, DataType, Expr, Int32Type, UInt8Type, UInt64Type, col, lit,
};

use crate::schema::{
    IDENTITY_FIELDS, IDENTITY_ROWS_MAX, IDENTITY_VERSION, Table, availability,
    identity_availability,
};
use crate::{Error, Result};

mod event_frame;
pub(crate) use event_frame::event_validation_frame;

/// Event provenance cannot widen the existing process-table admission bound.
pub(crate) const COLUMNS_MAX: usize = model::catalog::PROCESSES.column_count();

pub(crate) fn normalize(connection: &Engine, table: Table, source: &str) -> Result<String> {
    let schema = connection.schema(source)?;
    let present = validate_fields(table, || {
        schema
            .iter()
            .map(|(name, kind)| (name.as_str(), kind.clone()))
    })?;
    if !present {
        if matches!(table, Table::Events) {
            normalize_validate_events(connection, source, false)?;
        }
        return Ok(normalize_legacy_source(table, source));
    }

    normalize_validate(connection, table, source)?;
    Ok(source.to_owned())
}

pub(crate) fn validate_fields<'a, Columns: Iterator<Item = (&'a str, DataType)>>(
    table: Table,
    columns: impl Fn() -> Columns,
) -> Result<bool> {
    let count = columns().count();
    let columns_max = if matches!(table, Table::Events) {
        let extended = crate::event_observation::extended_fields(columns())?;
        model::catalog::EVENTS.column_count()
            - if extended {
                0
            } else {
                crate::event_observation::EVENT_OBSERVATION_FIELDS.len()
            }
    } else {
        COLUMNS_MAX
    };
    if count > columns_max {
        return Err(Error::Bound("table column count"));
    }

    if matches!(table, Table::Events) {
        validate_fields_events(&columns)?;
    }

    let mut fields = [false; IDENTITY_FIELDS.len()];
    for (name, data_type) in columns() {
        if name.len() > 128 {
            return Err(Error::Bound("table column name"));
        }

        for (index, (field, kind)) in IDENTITY_FIELDS.iter().enumerate() {
            if name.eq_ignore_ascii_case(field) {
                if name != *field {
                    return Err(Error::Bound("process identity field spelling"));
                }

                if crate::query::storage_type(&data_type)? != *kind {
                    return Err(Error::Bound("process identity storage type"));
                }

                fields[index] = true;
            }
        }
    }

    let present = fields.into_iter().filter(|present| *present).count();
    if present == 0 {
        if count + IDENTITY_FIELDS.len() > columns_max {
            return Err(Error::Bound("normalized table column count"));
        }

        return Ok(false);
    }

    if present != IDENTITY_FIELDS.len() {
        return Err(Error::Bound("incomplete process identity extension"));
    }

    Ok(true)
}

fn validate_fields_events<'a, Columns: Iterator<Item = (&'a str, DataType)>>(
    columns: &impl Fn() -> Columns,
) -> Result<()> {
    for expected in model::catalog::EVENTS.columns {
        let (_, actual) = columns()
            .find(|(name, _)| *name == expected.name)
            .ok_or(Error::Bound("event base field"))?;
        if crate::query::storage_type(&actual)? != crate::schema::storage_name(expected.kind) {
            return Err(Error::Bound("event base storage type"));
        }
    }

    Ok(())
}

fn normalize_validate(connection: &Engine, table: Table, source: &str) -> Result<()> {
    if matches!(table, Table::Events) {
        return normalize_validate_events(connection, source, true);
    }

    let limit =
        u32::try_from(IDENTITY_ROWS_MAX + 1).map_err(|_| Error::Bound("process identity rows"))?;
    let plan = connection
        .plan(&format!("SELECT * FROM {source}"))?
        .select(validation_columns(table))
        .limit(limit);
    let frame = connection.collect(plan)?;
    if u64::try_from(frame.height()).map_err(|_| Error::Bound("process identity rows"))?
        > IDENTITY_ROWS_MAX
    {
        return Err(Error::Bound("process identity rows"));
    }

    validate(table, &frame, 0..frame.height())
}

fn normalize_validate_events(connection: &Engine, source: &str, identities: bool) -> Result<()> {
    use crate::event_observation::{BATCH_ROWS_MAX, ROWS_MAX};
    let schema = connection.schema(source)?;
    let extended = crate::event_observation::extended(schema.as_ref())?;
    let maximum = if extended {
        ROWS_MAX
    } else {
        usize::try_from(IDENTITY_ROWS_MAX).map_err(|_| Error::Bound("event rows"))?
    };
    let projection = if identities {
        validation_columns_with_event_observation(Table::Events, extended)
    } else {
        let mut projection = vec![col("publication")];
        projection.extend(crate::event_observation::columns(extended));
        projection
    };
    let plan = connection
        .plan(&format!("SELECT * FROM {source}"))?
        .select(projection);
    let mut associated = 0_u64;
    for offset in (0..=maximum).step_by(BATCH_ROWS_MAX) {
        let length = (maximum - offset + 1).min(BATCH_ROWS_MAX);
        let frame = connection.collect(plan.clone().slice(
            i64::try_from(offset).map_err(|_| Error::Bound("event row offset"))?,
            u32::try_from(length).map_err(|_| Error::Bound("event batch rows"))?,
        ))?;
        if offset + frame.height() > maximum {
            return Err(Error::Bound("event rows"));
        }
        if identities {
            validate(Table::Events, &frame, 0..frame.height())?;
        } else {
            crate::event_observation::validate(&frame, 0..frame.height())?;
        }
        let publication = if identities {
            "event_publication"
        } else {
            "publication"
        };
        associated +=
            collector::u64_from_usize(frame.height() - frame.column(publication)?.null_count());
        if associated > IDENTITY_ROWS_MAX {
            return Err(Error::Bound("associated event rows"));
        }
        if frame.height() < length {
            return Ok(());
        }
    }
    Err(Error::Bound("event rows"))
}

/// The columns `validate` reads of a process or event table that holds the identity fields,
/// each by the name `validate` reads it under.
#[must_use]
pub fn validation_columns(table: Table) -> Vec<Expr> {
    validation_columns_with_event_observation(table, false)
}

/// Projects every identity constraint plus explicit event provenance where it is present.
/// Legacy event projections remain strictly associated; callers must inspect the full group.
#[must_use]
pub fn validation_columns_with_event_observation(table: Table, extended: bool) -> Vec<Expr> {
    assert!(matches!(table, Table::Processes | Table::Events));

    let payload = validation_payload(table);
    let present = payload
        .iter()
        .fold(lit(true), |all, field| all.and(col(*field).is_not_null()));
    let absent = payload
        .iter()
        .fold(lit(true), |all, field| all.and(col(*field).is_null()));
    let availability = match table {
        Table::Events => "availability",
        _ => "usage_availability",
    };

    let mut columns = vec![
        col("identity_version"),
        col("identity_availability"),
        col("process_unique_id"),
        col("pid_version"),
        col("executable_name"),
        col("app_path"),
        col("usage_availability"),
        present.alias("usage_present"),
        absent.alias("usage_absent"),
        col(availability)
            .cast(DataType::UInt8)
            .alias("source_usage_availability"),
        col("pid")
            .is_not_null()
            .and(if matches!(table, Table::Events) {
                lit(true)
            } else {
                col("publication").is_not_null()
            })
            .alias("observation_present"),
    ];
    if matches!(table, Table::Events) {
        columns.push(col("publication").alias("event_publication"));
        columns.extend(crate::event_observation::columns(extended));
    }
    columns
}

fn validation_payload(table: Table) -> Vec<&'static str> {
    let fields: Vec<_> = table
        .columns()
        .skip_while(|field| *field != "awake_ns_at_start")
        .collect();
    assert!(!fields.is_empty());
    fields
}

/// Refuses the rows `rows` of `frame`, rows of `table` with the columns `validation_columns`
/// names, unless each holds an identity and a usage as format 1 writes them.
pub fn validate(table: Table, frame: &DataFrame, rows: Range<usize>) -> Result<()> {
    assert!(rows.end <= frame.height());
    if matches!(table, Table::Events) {
        crate::event_observation::validate_named(frame, rows.clone(), "event_publication")?;
    }

    let columns = ValidationColumns::new(frame)?;
    for row in rows {
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
