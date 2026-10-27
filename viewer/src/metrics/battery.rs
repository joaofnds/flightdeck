use collector::battery::BatteryReading;
use persistence::schema::{BATTERY_FIELDS, BATTERY_VERSION};
use persistence::{Error, Result};
use serde_json::{Value, json};

use crate::metric_view::View;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct BatteryPoint {
    reading: BatteryReading,
    age_seconds: f64,
    charge_percent: Option<f64>,
    health_percent: Option<f64>,
    power_watts: Option<f64>,
}

pub(super) fn capture(
    table: &View<'_>,
    row: usize,
    now: Option<u64>,
) -> Result<Option<BatteryPoint>> {
    let Some(reading) = source(table, row)? else {
        return Ok(None);
    };

    let Some(now) = now else {
        return Ok(None);
    };

    if !reading.is_fresh_at(now) {
        return Ok(None);
    }

    Ok(Some(BatteryPoint {
        age_seconds: super::number(now - reading.source_continuous_ns) / 1_000_000_000.0,
        charge_percent: charge_percent(&reading),
        health_percent: ratio(reading.full_charge_capacity, reading.design_capacity),
        power_watts: power_watts(&reading),
        reading,
    }))
}

impl BatteryPoint {
    pub fn json(&self) -> Value {
        json!({"source_continuous_ns": self.reading.source_continuous_ns,
            "age_seconds": self.age_seconds, "charge_percent": self.charge_percent,
            "health_percent": self.health_percent, "cycle_count": self.reading.cycle_count,
            "charging": self.reading.charging, "external_power": self.reading.external_power,
            "power_watts": self.power_watts, "adapter_watts": self.reading.adapter_watts})
    }
}

/// The charge `reading` found in the battery, as a share of what it holds when full.
pub(crate) fn charge_percent(reading: &BatteryReading) -> Option<f64> {
    ratio(reading.charge_current, reading.charge_maximum)
}

/// The battery's power as `reading` measured it, in watts, negative while it discharges.
pub(crate) fn power_watts(reading: &BatteryReading) -> Option<f64> {
    let (current, voltage) = reading.current_ma.zip(reading.voltage_mv)?;
    let magnitude = super::number(current.unsigned_abs());
    let signed = if current < 0 { -magnitude } else { magnitude };

    Some(signed * super::number(voltage) / 1_000_000.0)
}

fn source(table: &View<'_>, row: usize) -> Result<Option<BatteryReading>> {
    let count = BATTERY_FIELDS
        .iter()
        .filter(|(name, _kind)| table.holds(name))
        .count();
    if count == 0 {
        return Ok(None);
    }

    if count != BATTERY_FIELDS.len() {
        return Err(Error::Bound("incomplete battery fields"));
    }

    let version = table.integer(row, "battery_version")?;
    if let Some(version) = version {
        if version != u64::from(BATTERY_VERSION) {
            return Err(Error::Bound("battery version"));
        }
    }

    let source_time = table.integer(row, "battery_source_continuous_ns")?;
    if version.is_none() {
        source_empty(table, row, 1)?;
        return Ok(None);
    }

    let Some(source_continuous_ns) = source_time else {
        source_empty(table, row, 2)?;
        return Ok(None);
    };

    Ok(Some(BatteryReading {
        source_continuous_ns,
        charge_current: table.integer(row, "battery_charge_current")?,
        charge_maximum: table.integer(row, "battery_charge_maximum")?,
        full_charge_capacity: table.integer(row, "battery_full_charge_capacity")?,
        design_capacity: table.integer(row, "battery_design_capacity")?,
        cycle_count: table.integer(row, "battery_cycle_count")?,
        charging: boolean(table, row, "battery_charging")?,
        external_power: boolean(table, row, "battery_external_power")?,
        voltage_mv: table.integer(row, "battery_voltage_mv")?,
        current_ma: table.signed(row, "battery_current_ma")?,
        adapter_watts: table.integer(row, "battery_adapter_watts")?,
    }))
}

fn source_empty(table: &View<'_>, row: usize, skip: usize) -> Result<()> {
    for (name, _kind) in BATTERY_FIELDS.iter().skip(skip) {
        if table.field(row, name)?.is_some() {
            return Err(Error::Bound("battery payload without source"));
        }
    }

    Ok(())
}

fn boolean(table: &View<'_>, row: usize, name: &str) -> Result<Option<bool>> {
    table
        .field(row, name)?
        .map(str::parse::<bool>)
        .transpose()
        .map_err(|_| Error::Bound("battery boolean"))
}

fn ratio(current: Option<u64>, maximum: Option<u64>) -> Option<f64> {
    let (current, maximum) = current.zip(maximum)?;
    if maximum == 0 {
        return None;
    }

    Some(super::number(current) * 100.0 / super::number(maximum))
}
