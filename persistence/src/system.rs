//! Optional tick columns for the Mac's system readings and the machine's facts.
use crate::engine::{Engine, Row};
use crate::schema::{SYSTEM_FIELDS, SYSTEM_ROWS_MAX, TICK_COLUMNS_MAX};
use crate::{Error, Result};
use collector::system::FACT_BYTES_MAX;

const LOAD_AVERAGES: [&str; 3] = ["load_average_1", "load_average_5", "load_average_15"];
const TEXTS: [&str; 5] = [
    "machine_model",
    "machine_chip",
    "machine_architecture",
    "machine_performance_level0_name",
    "machine_performance_level1_name",
];
/// The texts of `TEXTS` that name the performance levels, which come whole with these.
const LEVEL_NAMES: [usize; 2] = [3, 4];
const LEVEL_LOGICAL_CPUS: [&str; 2] = [
    "machine_performance_level0_logical_cpus",
    "machine_performance_level1_logical_cpus",
];

pub(crate) fn normalize(connection: &Engine, source: &str) -> Result<String> {
    let schema = connection.schema(source)?;
    if schema.len() > TICK_COLUMNS_MAX {
        return Err(Error::Bound("tick column count"));
    }

    let mut fields = [false; SYSTEM_FIELDS.len()];
    for (name, data_type) in schema.iter() {
        for (index, (field, kind)) in SYSTEM_FIELDS.iter().enumerate() {
            if name.eq_ignore_ascii_case(field) {
                if name.as_str() != *field {
                    return Err(Error::Bound("system field spelling"));
                }

                if crate::query::storage_type(data_type)? != *kind {
                    return Err(Error::Bound("system storage type"));
                }

                fields[index] = true;
            }
        }
    }

    let present = fields.into_iter().filter(|present| *present).count();
    if present == 0 {
        if schema.len() + SYSTEM_FIELDS.len() > TICK_COLUMNS_MAX {
            return Err(Error::Bound("normalized tick column count"));
        }

        return Ok(normalize_legacy_source(source));
    }

    if present != SYSTEM_FIELDS.len() {
        return Err(Error::Bound("incomplete system extension"));
    }

    normalize_validate(connection, source)?;

    Ok(source.to_owned())
}

fn normalize_validate(connection: &Engine, source: &str) -> Result<()> {
    let fields = [
        LOAD_AVERAGES.as_slice(),
        TEXTS.as_slice(),
        LEVEL_LOGICAL_CPUS.as_slice(),
    ]
    .concat()
    .join(", ");
    let maximum = SYSTEM_ROWS_MAX + 1;
    let frame = connection.query(&format!("SELECT {fields} FROM {source} LIMIT {maximum}"))?;
    if u64::try_from(frame.height()).map_err(|_| Error::Bound("system tick rows"))?
        > SYSTEM_ROWS_MAX
    {
        return Err(Error::Bound("system tick rows"));
    }

    for index in 0..frame.height() {
        let row = Row::new(&frame, index)?;
        normalize_validate_row(&row)?;
    }

    Ok(())
}

/// Refuses a row whose load average is no finite number at or above zero, whose text no
/// collector writes, or that holds part of the performance levels.
fn normalize_validate_row(row: &Row) -> Result<()> {
    for field in 0..LOAD_AVERAGES.len() {
        if let Some(average) = row.get::<Option<f64>>(field)? {
            if !average.is_finite() {
                return Err(Error::Bound("system load average"));
            }
            if average < 0.0 {
                return Err(Error::Bound("system load average"));
            }
        }
    }

    for field in LOAD_AVERAGES.len()..LOAD_AVERAGES.len() + TEXTS.len() {
        if let Some(text) = row.get::<Option<String>>(field)? {
            if text.is_empty() {
                return Err(Error::Bound("machine fact text"));
            }
            if text.len() > FACT_BYTES_MAX {
                return Err(Error::Bound("machine fact text"));
            }
        }
    }

    let first = LOAD_AVERAGES.len() + TEXTS.len();
    let names = LEVEL_NAMES.map(|text| LOAD_AVERAGES.len() + text);
    let mut levels_present = 0;
    for field in names
        .into_iter()
        .chain(first..first + LEVEL_LOGICAL_CPUS.len())
    {
        if row.value(field)? != polars::prelude::AnyValue::Null {
            levels_present += 1;
        }
    }
    if levels_present != 0 {
        if levels_present != names.len() + LEVEL_LOGICAL_CPUS.len() {
            return Err(Error::Bound("incomplete performance levels"));
        }
    }

    Ok(())
}

fn normalize_legacy_source(source: &str) -> String {
    let fields = SYSTEM_FIELDS
        .iter()
        .map(|(name, kind)| format!("NULL::{kind} {name}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("(SELECT *, {fields} FROM {source})")
}
