//! Optional tick columns preserve independent raw battery fields and source time.
use collector::battery::BatteryReading;
use duckdb::{Connection, types::ValueRef};

use crate::schema::{BATTERY_FIELDS, BATTERY_ROWS_MAX, BATTERY_VERSION};
use crate::{Error, Result};

pub(crate) fn fields(reading: Option<&BatteryReading>, fields: &mut [ValueRef<'_>]) {
    assert_eq!(fields.len(), BATTERY_FIELDS.len());
    fields[0] = ValueRef::UTinyInt(BATTERY_VERSION);
    if let Some(reading) = reading {
        fields[1] = ValueRef::UBigInt(reading.source_continuous_ns);
        fields[2] = reading
            .charge_current
            .map_or(ValueRef::Null, ValueRef::UBigInt);
        fields[3] = reading
            .charge_maximum
            .map_or(ValueRef::Null, ValueRef::UBigInt);
        fields[4] = reading
            .full_charge_capacity
            .map_or(ValueRef::Null, ValueRef::UBigInt);
        fields[5] = reading
            .design_capacity
            .map_or(ValueRef::Null, ValueRef::UBigInt);
        fields[6] = reading
            .cycle_count
            .map_or(ValueRef::Null, ValueRef::UBigInt);
        fields[7] = reading.charging.map_or(ValueRef::Null, ValueRef::Boolean);
        fields[8] = reading
            .external_power
            .map_or(ValueRef::Null, ValueRef::Boolean);
        fields[9] = reading.voltage_mv.map_or(ValueRef::Null, ValueRef::UBigInt);
        fields[10] = reading.current_ma.map_or(ValueRef::Null, ValueRef::BigInt);
        fields[11] = reading
            .adapter_watts
            .map_or(ValueRef::Null, ValueRef::UBigInt);
    }
}

pub(crate) fn normalize(connection: &Connection, source: &str) -> Result<String> {
    let mut statement = connection.prepare(&format!("DESCRIBE SELECT * FROM {source}"))?;
    let mut rows = statement.query([])?;
    let mut fields = [false; BATTERY_FIELDS.len()];

    for count in 0..=32 {
        let Some(row) = rows.next()? else {
            let present = fields.into_iter().filter(|present| *present).count();
            if present == 0 {
                if count + BATTERY_FIELDS.len() > 32 {
                    return Err(Error::Bound("normalized tick column count"));
                }

                return Ok(normalize_legacy_source(source));
            }

            if present != BATTERY_FIELDS.len() {
                return Err(Error::Bound("incomplete battery extension"));
            }

            normalize_validate(connection, source)?;
            return Ok(source.to_owned());
        };

        if count == 32 {
            return Err(Error::Bound("tick column count"));
        }

        normalize_metadata(row, &mut fields)?;
    }

    Err(Error::Bound("tick column count"))
}

fn normalize_metadata(row: &duckdb::Row<'_>, fields: &mut [bool; 12]) -> Result<()> {
    let ValueRef::Text(name) = row.get_ref(0)? else {
        return Err(Error::Bound("tick column metadata"));
    };

    if name.len() > 128 {
        return Err(Error::Bound("tick column name"));
    }

    for (index, (field, kind)) in BATTERY_FIELDS.iter().enumerate() {
        if name.eq_ignore_ascii_case(field.as_bytes()) {
            if name != field.as_bytes() {
                return Err(Error::Bound("battery field spelling"));
            }

            if row.get_ref(1)? != ValueRef::Text(kind.as_bytes()) {
                return Err(Error::Bound("battery storage type"));
            }

            fields[index] = true;
            return Ok(());
        }
    }

    if name
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"battery_"))
    {
        return Err(Error::Bound("unknown battery field"));
    }

    Ok(())
}

fn normalize_validate(connection: &Connection, source: &str) -> Result<()> {
    let fields = BATTERY_FIELDS.map(|(name, _)| name).join(", ");
    let maximum = BATTERY_ROWS_MAX + 1;
    let mut statement = connection.prepare(&format!(
        "SELECT {fields}, publication IS NOT NULL FROM {source} LIMIT {maximum}"
    ))?;
    let mut rows = statement.query([])?;

    for index in 0..=BATTERY_ROWS_MAX {
        let Some(row) = rows.next()? else {
            return Ok(());
        };

        if index == BATTERY_ROWS_MAX {
            return Err(Error::Bound("battery tick rows"));
        }

        if row.get::<_, Option<u8>>(0)? != Some(BATTERY_VERSION) {
            return Err(Error::Bound("battery version"));
        }

        if !row.get::<_, bool>(12)? {
            return Err(Error::Bound("battery observation"));
        }

        if row.get::<_, Option<u64>>(1)?.is_none() {
            for field in 2..BATTERY_FIELDS.len() {
                if row.get_ref(field)? != ValueRef::Null {
                    return Err(Error::Bound("battery payload without source time"));
                }
            }
        }
    }

    Err(Error::Bound("battery tick rows"))
}

fn normalize_legacy_source(source: &str) -> String {
    let fields = BATTERY_FIELDS
        .iter()
        .map(|(name, kind)| format!("NULL::{kind} {name}"))
        .collect::<Vec<_>>()
        .join(", ");
    format!("(SELECT *, {fields} FROM {source})")
}
