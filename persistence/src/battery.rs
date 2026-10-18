//! Optional tick columns preserve independent raw battery fields and source time.
use crate::engine::{Engine, Row};
use crate::schema::{BATTERY_FIELDS, BATTERY_ROWS_MAX, BATTERY_VERSION};
use crate::{Error, Result};
use polars::prelude::{AnyValue, DataType};

pub(crate) fn normalize(connection: &Engine, source: &str) -> Result<String> {
    let schema = connection.schema(source)?;
    if schema.len() > 32 {
        return Err(Error::Bound("tick column count"));
    }

    let mut fields = [false; BATTERY_FIELDS.len()];
    for (name, data_type) in schema.iter() {
        normalize_metadata(name, data_type, &mut fields)?;
    }

    let present = fields.into_iter().filter(|present| *present).count();
    if present == 0 {
        if schema.len() + BATTERY_FIELDS.len() > 32 {
            return Err(Error::Bound("normalized tick column count"));
        }

        return Ok(normalize_legacy_source(source));
    }

    if present != BATTERY_FIELDS.len() {
        return Err(Error::Bound("incomplete battery extension"));
    }

    normalize_validate(connection, source)?;

    Ok(source.to_owned())
}

fn normalize_metadata(name: &str, data_type: &DataType, fields: &mut [bool; 12]) -> Result<()> {
    if name.len() > 128 {
        return Err(Error::Bound("tick column name"));
    }

    for (index, (field, kind)) in BATTERY_FIELDS.iter().enumerate() {
        if name.eq_ignore_ascii_case(field) {
            if name != *field {
                return Err(Error::Bound("battery field spelling"));
            }

            if crate::query::storage_type(data_type)? != *kind {
                return Err(Error::Bound("battery storage type"));
            }

            fields[index] = true;
            return Ok(());
        }
    }

    if name
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("battery_"))
    {
        return Err(Error::Bound("unknown battery field"));
    }

    Ok(())
}

fn normalize_validate(connection: &Engine, source: &str) -> Result<()> {
    let fields = BATTERY_FIELDS.map(|(name, _)| name).join(", ");
    let maximum = BATTERY_ROWS_MAX + 1;
    let frame = connection.query(&format!(
        "SELECT {fields}, publication IS NOT NULL AS observation_present \
        FROM {source} LIMIT {maximum}"
    ))?;
    if u64::try_from(frame.height()).map_err(|_| Error::Bound("battery tick rows"))?
        > BATTERY_ROWS_MAX
    {
        return Err(Error::Bound("battery tick rows"));
    }

    for index in 0..frame.height() {
        let row = Row::new(&frame, index)?;
        if row.get::<Option<u8>>(0)? != Some(BATTERY_VERSION) {
            return Err(Error::Bound("battery version"));
        }

        if !row.get::<bool>(12)? {
            return Err(Error::Bound("battery observation"));
        }

        if row.get::<Option<u64>>(1)?.is_none() {
            for field in 2..BATTERY_FIELDS.len() {
                if row.value(field)? != AnyValue::Null {
                    return Err(Error::Bound("battery payload without source time"));
                }
            }
        }
    }

    Ok(())
}

fn normalize_legacy_source(source: &str) -> String {
    let fields = BATTERY_FIELDS
        .iter()
        .map(|(name, kind)| format!("NULL::{kind} {name}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("(SELECT *, {fields} FROM {source})")
}
