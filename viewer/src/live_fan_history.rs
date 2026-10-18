use collector::smc::FANS_MAX;
use persistence::{Error, Result};
use serde_json::Value;

use crate::data::Table;

pub(crate) fn capture(ticks: &Table, fans: Option<&Table>, publication: u64) -> Result<Value> {
    if ticks.rows.len() != 1 {
        return Err(Error::Bound("fan tick rows"));
    }

    if ticks.integer(&ticks.rows[0], "publication")? != Some(publication) {
        return Err(Error::Bound("fan tick publication"));
    }

    if let Some(fans) = fans {
        source(fans)?;
        if fans.rows.len() > usize::from(FANS_MAX) {
            return Err(Error::Bound("fan channels"));
        }
    }

    match ticks.field(&ticks.rows[0], "smc_read")? {
        Some("true") => {
            let mut channels = Vec::with_capacity(fans.map_or(0, |fans| fans.rows.len()));

            if let Some(fans) = fans {
                capture_channels(fans, publication, &mut channels)?;
            }

            Ok(Value::Array(channels))
        }
        Some("false") => {
            if fans.is_some_and(|fans| !fans.rows.is_empty()) {
                return Err(Error::Bound("unread fan rows"));
            }

            Ok(Value::Null)
        }
        _ => Err(Error::Bound("fan SMC availability")),
    }
}

pub(crate) fn source(fans: &Table) -> Result<()> {
    for column in ["publication", "fan", "rpm"] {
        if !fans.columns.iter().any(|name| name == column) {
            return Err(Error::Bound("missing schema column"));
        }
    }
    Ok(())
}

fn capture_channels(fans: &Table, publication: u64, values: &mut Vec<Value>) -> Result<()> {
    assert!(fans.rows.len() <= usize::from(FANS_MAX));
    assert!(values.is_empty());

    for (index, row) in fans.rows.iter().enumerate() {
        if fans.integer(row, "publication")? != Some(publication) {
            return Err(Error::Bound("fan row publication"));
        }

        if fans.integer(row, "fan")?
            != Some(u64::try_from(index).unwrap_or_else(|_| unreachable!()))
        {
            return Err(Error::Bound("fan channel order"));
        }

        let value = capture_rpm(fans, row)?;

        values.push(value.map_or(Value::Null, Value::from));
    }

    Ok(())
}

fn capture_rpm(fans: &Table, row: &[Option<String>]) -> Result<Option<f64>> {
    Ok(fans
        .field(row, "rpm")?
        .map(str::parse::<f64>)
        .transpose()
        .map_err(|_| Error::Bound("number"))?
        .filter(|value| value.is_finite())
        .filter(|value| *value >= 0.0))
}

#[cfg(test)]
#[path = "live_fan_projection_tests.rs"]
mod tests;
