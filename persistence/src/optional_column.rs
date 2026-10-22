//! One optional column a table gained after windows without it were sealed.
use crate::engine::Engine;
use crate::{Error, Result};

/// The source with `field` as it was saved, or with it NULL where the window predates it.
/// Refuses the field misspelled or of another type, and a source of more than `columns_max`
/// columns once it holds the field.
pub(crate) fn normalize(
    connection: &Engine,
    source: &str,
    field: (&str, &str),
    columns_max: usize,
) -> Result<String> {
    let (field, kind) = field;
    let schema = connection.schema(source)?;
    if schema.len() > columns_max {
        return Err(Error::Bound("table column count"));
    }

    let mut present = false;
    for (name, data_type) in schema.iter() {
        if name.eq_ignore_ascii_case(field) {
            if name.as_str() != field {
                return Err(Error::Bound("optional field spelling"));
            }

            if crate::query::storage_type(data_type)? != kind {
                return Err(Error::Bound("optional field storage type"));
            }

            present = true;
        }
    }

    if present {
        return Ok(source.to_owned());
    }

    if schema.len() + 1 > columns_max {
        return Err(Error::Bound("normalized table column count"));
    }

    Ok(format!("(SELECT *, NULL::{kind} {field} FROM {source})"))
}
