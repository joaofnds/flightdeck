use collector::smc::FANS_MAX;
use persistence::{Error, Result};
use serde_json::Value;

use crate::data::Table;

#[cfg(test)]
pub(crate) fn capture(ticks: &Table, fans: Option<&Table>, publication: u64) -> Result<Value> {
    let fans = fans.map(crate::metric_view::View::Saved);
    capture_view(
        &crate::metric_view::View::Saved(ticks),
        fans.as_ref(),
        publication,
    )
}

pub(crate) fn capture_view(
    ticks: &crate::metric_view::View<'_>,
    fans: Option<&crate::metric_view::View<'_>>,
    publication: u64,
) -> Result<Value> {
    if ticks.len() != 1 {
        return Err(Error::Bound("fan tick rows"));
    }

    if ticks.integer(0, "publication")? != Some(publication) {
        return Err(Error::Bound("fan tick publication"));
    }

    if let Some(fans) = fans {
        source_view(fans)?;
        if fans.len() > usize::from(FANS_MAX) {
            return Err(Error::Bound("fan channels"));
        }
    }

    match ticks.field(0, "smc_read")? {
        Some("true") => {
            let mut channels = Vec::with_capacity(fans.map_or(0, crate::metric_view::View::len));

            if let Some(fans) = fans {
                capture_channels(fans, publication, &mut channels)?;
            }

            Ok(Value::Array(channels))
        }
        Some("false") => {
            if fans.is_some_and(|fans| fans.len() > 0) {
                return Err(Error::Bound("unread fan rows"));
            }

            Ok(Value::Null)
        }
        _ => Err(Error::Bound("fan SMC availability")),
    }
}

pub(crate) fn source(fans: &Table) -> Result<()> {
    source_view(&crate::metric_view::View::Saved(fans))
}

fn source_view(fans: &crate::metric_view::View<'_>) -> Result<()> {
    for column in ["publication", "fan", "rpm"] {
        if !fans.holds(column) {
            return Err(Error::Bound("missing schema column"));
        }
    }
    Ok(())
}

fn capture_channels(
    fans: &crate::metric_view::View<'_>,
    publication: u64,
    values: &mut Vec<Value>,
) -> Result<()> {
    assert!(fans.len() <= usize::from(FANS_MAX));
    assert!(values.is_empty());

    for (index, row) in fans.rows().enumerate() {
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

fn capture_rpm(fans: &crate::metric_view::View<'_>, row: usize) -> Result<Option<f64>> {
    Ok(fans
        .fan_rpm(row)?
        .filter(|value| value.is_finite())
        .filter(|value| *value >= 0.0))
}

#[cfg(test)]
#[path = "live_fan_projection_tests.rs"]
mod tests;
