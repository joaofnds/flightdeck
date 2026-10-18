use serde_json::Value;

use super::{Coordinates, Domain, Panel, Reading, Specification, reading, specification};

pub(super) fn battery() -> Vec<Specification> {
    let mut charge = specification(
        "live-battery-charge",
        "Battery charge",
        "%",
        vec![resource_reading(&ResourceReading {
            source: "battery",
            key: "charge_percent",
            name: "Charge",
            color: "#73bf69",
        })],
    );
    charge.maximum = Some(100.0);
    charge.help = "Charge as a share of reported full capacity. The recorded controller reading \
        is held for at most 60 seconds in the same run and boot. Missing readings remain gaps.";
    let mut flow = specification(
        "live-battery-flow",
        "Battery power flow",
        "W",
        vec![resource_reading(&ResourceReading {
            source: "battery",
            key: "power_watts",
            name: "Into battery",
            color: "#73bf69",
        })],
    );
    flow.help = "Positive is power into the battery; negative is discharge. Derived from \
        controller current and voltage. Adapter rating is separate from measured battery flow.";
    let charging = specification(
        "live-battery-charging",
        "Charging state",
        "",
        vec![resource_reading(&ResourceReading {
            source: "battery",
            key: "charging",
            name: "Charging",
            color: "#f2cc0c",
        })],
    );
    vec![charge, flow, charging]
}

pub(super) fn value(reading: &Reading, value: &Value) -> Result<Option<f32>, String> {
    if reading.resource == Some("battery") {
        if reading.key == "charging" {
            return match value {
                Value::Null => Ok(None),
                Value::Bool(true) => Ok(Some(1.0)),
                Value::Bool(false) => Ok(Some(0.0)),
                _ => Err("Invalid battery charging state".into()),
            };
        }
    }

    super::number(value)
}

pub(super) fn charging_cells(
    panel: &mut Panel,
    domain: &Domain,
    points: &[Value],
    coordinates: &Coordinates,
) -> Result<(), String> {
    super::timeline_cells(panel, domain, points, coordinates, |value| value)?;
    panel.states = Some(vec!["Not charging".into(), "Charging".into()]);
    panel.help = Some(
        "Battery-controller charging state by observed second. The chosen \
        same-second publication takes priority; otherwise the latest reading is shown. \
        Blank means unavailable or unobserved. This state does not imply external power."
            .into(),
    );
    Ok(())
}

pub(super) fn storage() -> Vec<Specification> {
    vec![
        throughput(),
        storage_pair(&StoragePair {
            id: "live-storage-operations",
            title: "Disk operations",
            unit: "ops/s",
            keys: ["read_ops_per_second", "write_ops_per_second"],
            mirrored: true,
            help: "Completed block-driver requests per second. Writes are mirrored below zero; \
             inspected values remain positive. These are separate from process I/O calls.",
        }),
        storage_pair(&StoragePair {
            id: "live-storage-mean",
            title: "Mean I/O time",
            unit: "ms",
            keys: ["read_mean_ms", "write_mean_ms"],
            mirrored: false,
            help: "Total completed-request duration divided by completed operations, weighted \
             across devices. This is not queue wait, a latency percentile or busy percentage. \
             Intervals without completed requests have no mean.",
        }),
        storage_pair(&StoragePair {
            id: "live-storage-errors",
            title: "Disk errors",
            unit: "errors/s",
            keys: ["read_errors_per_second", "write_errors_per_second"],
            mirrored: false,
            help: "Changes in block-driver error counters per second. Missing or reset counters \
             leave gaps. A measured zero means no new errors in that interval.",
        }),
    ]
}

pub(super) fn throughput() -> Specification {
    let mut read = resource_reading(&ResourceReading {
        source: "storage",
        key: "read_bps",
        name: "Read",
        color: "#f2cc0c",
    });
    read.divisor = 1_000_000.0;
    let mut write = resource_reading(&ResourceReading {
        source: "storage",
        key: "write_bps",
        name: "Write",
        color: "#ff9830",
    });
    write.divisor = 1_000_000.0;
    write.negative = true;
    let mut result = specification(
        "live-storage-throughput",
        "Disk throughput",
        "MB/s",
        vec![read, write],
    );
    result.help = "Bytes transferred by physical block devices per second. Writes are mirrored \
        below zero; inspected values remain positive. Disk images are excluded. Totals require \
        complete matching devices and valid counter pairs for every device.";
    result
}

struct StoragePair {
    id: &'static str,
    title: &'static str,
    unit: &'static str,
    keys: [&'static str; 2],
    mirrored: bool,
    help: &'static str,
}

struct ResourceReading {
    source: &'static str,
    key: &'static str,
    name: &'static str,
    color: &'static str,
}

fn storage_pair(pair: &StoragePair) -> Specification {
    let read = resource_reading(&ResourceReading {
        source: "storage",
        key: pair.keys[0],
        name: "Read",
        color: "#f2cc0c",
    });
    let mut write = resource_reading(&ResourceReading {
        source: "storage",
        key: pair.keys[1],
        name: "Write",
        color: "#ff9830",
    });
    write.negative = pair.mirrored;
    write.dashed = !pair.mirrored;
    let mut result = specification(pair.id, pair.title, pair.unit, vec![read, write]);
    result.help = pair.help;
    result
}

fn resource_reading(resource: &ResourceReading) -> Reading {
    let mut result = reading(resource.key, resource.name, resource.color, 1.0);
    result.resource = Some(resource.source);
    result
}
