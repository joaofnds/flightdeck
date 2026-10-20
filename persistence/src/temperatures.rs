//! Optional tick columns for the temperature of the CPU and of the GPU, in degrees Celsius.
use crate::engine::{Engine, Row};
use crate::schema::{TEMPERATURE_FIELDS, TEMPERATURE_ROWS_MAX, TICK_COLUMNS_MAX};
use crate::{Error, Result};

pub(crate) fn normalize(connection: &Engine, source: &str) -> Result<String> {
    let schema = connection.schema(source)?;
    if schema.len() > TICK_COLUMNS_MAX {
        return Err(Error::Bound("tick column count"));
    }

    let mut fields = [false; TEMPERATURE_FIELDS.len()];
    for (name, data_type) in schema.iter() {
        for (index, (field, kind)) in TEMPERATURE_FIELDS.iter().enumerate() {
            if name.eq_ignore_ascii_case(field) {
                if name.as_str() != *field {
                    return Err(Error::Bound("temperature field spelling"));
                }

                if crate::query::storage_type(data_type)? != *kind {
                    return Err(Error::Bound("temperature storage type"));
                }

                fields[index] = true;
            }
        }
    }

    let present = fields.into_iter().filter(|present| *present).count();
    if present == 0 {
        if schema.len() + TEMPERATURE_FIELDS.len() > TICK_COLUMNS_MAX {
            return Err(Error::Bound("normalized tick column count"));
        }

        return Ok(normalize_legacy_source(source));
    }

    if present != TEMPERATURE_FIELDS.len() {
        return Err(Error::Bound("incomplete temperature extension"));
    }

    normalize_validate(connection, source)?;

    Ok(source.to_owned())
}

fn normalize_validate(connection: &Engine, source: &str) -> Result<()> {
    let fields = TEMPERATURE_FIELDS.map(|(name, _)| name).join(", ");
    let maximum = TEMPERATURE_ROWS_MAX + 1;
    let frame = connection.query(&format!("SELECT {fields} FROM {source} LIMIT {maximum}"))?;
    if u64::try_from(frame.height()).map_err(|_| Error::Bound("temperature tick rows"))?
        > TEMPERATURE_ROWS_MAX
    {
        return Err(Error::Bound("temperature tick rows"));
    }

    for index in 0..frame.height() {
        let row = Row::new(&frame, index)?;
        for field in 0..TEMPERATURE_FIELDS.len() {
            let Some(celsius) = row.get::<Option<f32>>(field)? else {
                continue;
            };

            if collector::smc::usable_celsius(celsius).is_none() {
                return Err(Error::Bound("temperature reading"));
            }
        }
    }

    Ok(())
}

fn normalize_legacy_source(source: &str) -> String {
    let fields = TEMPERATURE_FIELDS
        .iter()
        .map(|(name, _)| format!("NULL::REAL {name}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("(SELECT *, {fields} FROM {source})")
}
