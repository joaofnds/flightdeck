use serde_json::Value;

const DEVICES_MAX: usize = collector::disks::DISKS_MAX;

#[derive(Debug, PartialEq)]
pub struct Field {
    pub name: &'static str,
    pub reading: String,
}

struct Quantity<'a> {
    source: &'a Value,
    name: &'static str,
    key: &'a str,
    unit: &'a str,
    divisor: f32,
}

pub fn devices(selection: &Value) -> &[Value] {
    match crate::live_views::matching_summary(selection)["storage"]["devices"].as_array() {
        Some(devices) if devices.len() <= DEVICES_MAX => devices,
        _ => &[],
    }
}

pub fn device_key(selection: &Value, index: usize) -> String {
    let Some(device) = devices(selection).get(index) else {
        return format!("storage-device-unavailable-{index}");
    };

    format!(
        "storage-device-{}-{}",
        integer(&device["driver_id"]),
        integer(&device["media_id"])
    )
}

pub fn device_name(device: &Value) -> String {
    match device["name"].as_str() {
        Some(name) => crate::bounded::bounded(name, 64),
        None => "Unavailable name".into(),
    }
}

pub fn device_fields(device: &Value) -> Vec<Field> {
    vec![
        quantity(&Quantity {
            source: device,
            name: "Read",
            key: "read_bps",
            unit: "MB/s",
            divisor: 1_000_000.0,
        }),
        quantity(&Quantity {
            source: device,
            name: "Write",
            key: "write_bps",
            unit: "MB/s",
            divisor: 1_000_000.0,
        }),
        quantity(&Quantity {
            source: device,
            name: "Read operations",
            key: "read_ops_per_second",
            unit: "ops/s",
            divisor: 1.0,
        }),
        quantity(&Quantity {
            source: device,
            name: "Write operations",
            key: "write_ops_per_second",
            unit: "ops/s",
            divisor: 1.0,
        }),
        quantity(&Quantity {
            source: device,
            name: "Read mean",
            key: "read_mean_ms",
            unit: "ms",
            divisor: 1.0,
        }),
        quantity(&Quantity {
            source: device,
            name: "Write mean",
            key: "write_mean_ms",
            unit: "ms",
            divisor: 1.0,
        }),
        quantity(&Quantity {
            source: device,
            name: "Read errors",
            key: "read_errors_per_second",
            unit: "errors/s",
            divisor: 1.0,
        }),
        quantity(&Quantity {
            source: device,
            name: "Write errors",
            key: "write_errors_per_second",
            unit: "errors/s",
            divisor: 1.0,
        }),
    ]
}

/// What the page must say about readings it cannot show whole: a missing battery, or a
/// physical-disk scan that failed, ran past its bounds or covered only some disks.
pub fn notice(route: &str, selection: &Value) -> Option<String> {
    let selected = crate::live_views::matching_summary(selection);
    match route {
        "battery" => selected["battery"]
            .is_null()
            .then(|| "No battery reading is available at this observation.".into()),
        "storage" => storage_notice(&selected["storage"]),
        _ => None,
    }
}

fn storage_notice(storage: &Value) -> Option<String> {
    if storage.is_null() {
        return Some("This source does not include physical-disk readings.".into());
    }

    if storage["devices"]
        .as_array()
        .is_some_and(|devices| devices.len() > DEVICES_MAX)
    {
        return Some(format!(
            "Physical-disk inventory exceeds {DEVICES_MAX} devices."
        ));
    }

    match storage["state"].as_str() {
        Some("complete") => {
            if storage["devices"].as_array().is_some_and(Vec::is_empty) {
                Some("No physical disks reported at this tick.".into())
            } else {
                None
            }
        }
        Some("failed") => {
            Some("The physical-disk scan failed. Readings remain unavailable.".into())
        }
        Some("exhausted") => Some(
            "The physical-disk scan exceeded its capture bounds. \
            Totals are incomplete."
                .into(),
        ),
        Some("partial") => Some(
            "Some physical-disk readings are unavailable. \
            Totals are incomplete."
                .into(),
        ),
        _ => Some("The physical-disk scan state is unavailable. Totals may be incomplete.".into()),
    }
}

fn quantity(quantity: &Quantity<'_>) -> Field {
    field(
        quantity.name,
        crate::live_projection::quantity(
            quantity.source,
            quantity.key,
            quantity.unit,
            quantity.divisor,
        ),
    )
}

fn field(name: &'static str, reading: String) -> Field {
    Field { name, reading }
}

fn integer(value: &Value) -> String {
    match value.as_u64() {
        Some(value) => value.to_string(),
        None => "Unavailable".into(),
    }
}

#[cfg(test)]
#[path = "live_resource_data_tests.rs"]
mod tests;
