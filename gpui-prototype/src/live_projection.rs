use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::Value;

use crate::data::{Line, Panel};

const OBSERVATIONS_MAX: usize = 4096;
const GIBIBYTE: f32 = 1_073_741_824.0;
const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;

pub fn inspection_identity(selection: &Value, second: u64) -> Option<Value> {
    if selection["second"].as_u64() == Some(second) {
        if !selection["selected"].is_null() {
            return Some(selection["selected"].clone());
        }
    }
    selection["trends"]
        .as_array()?
        .iter()
        .take(OBSERVATIONS_MAX)
        .find(|point| {
            point["wall_ns"]
                .as_u64()
                .is_some_and(|wall| wall / NANOSECONDS_PER_SECOND == second)
        })
        .cloned()
}

pub fn panels(
    route: &str,
    selection: &Value,
    history: Option<&Value>,
) -> Result<Vec<Rc<Panel>>, String> {
    let source = if route == "apps" {
        let Some(history) = history else {
            return Ok(Vec::new());
        };
        history
    } else {
        selection
    };
    let key = if route == "apps" { "points" } else { "trends" };
    let Some(points) = source[key].as_array() else {
        return Ok(Vec::new());
    };
    if points.is_empty() {
        return Ok(Vec::new());
    }
    if points.len() > OBSERVATIONS_MAX {
        return Err("Live chart exceeds 4096 observations".to_owned());
    }
    let domain = Domain::read(selection)?;
    let coordinates = coordinates(points, &domain, route == "apps", &source["selected"])?;
    let mut result = Vec::with_capacity(7);
    for specification in specifications(route) {
        result.push(Rc::new(panel(&specification, points, &coordinates)?));
    }
    assert!(result.len() <= 7);
    Ok(result)
}

pub fn quantity(summary: &Value, key: &str, unit: &str, divisor: f32) -> String {
    if !divisor.is_finite() {
        return "Unavailable".to_owned();
    }
    if divisor <= 0.0 {
        return "Unavailable".to_owned();
    }
    match number(&summary[key]) {
        Ok(Some(value)) => quantity_text(value / divisor, unit),
        Ok(None) => "Unavailable".to_owned(),
        Err(error) => {
            eprintln!("flightdeck live quantity {key}: {error}");
            "Unavailable".to_owned()
        }
    }
}

pub fn sample_second(selection: &Value, fraction: f32) -> Option<u64> {
    if !fraction.is_finite() {
        return None;
    }
    if !(0.0..=1.0).contains(&fraction) {
        return None;
    }
    let Ok(domain) = Domain::read(selection) else {
        return None;
    };
    let offset = fraction * f32::from(domain.span);
    // The right edge belongs to the selected second, not the next second.
    for second in (0..domain.span).rev() {
        if f32::from(second) <= offset {
            return domain.first.checked_add(u64::from(second));
        }
    }
    None
}

struct Domain {
    first: u64,
    end: u64,
    span: u16,
}

struct Coordinates {
    positions: Vec<f32>,
    segments: Vec<u64>,
    selected_index: Option<usize>,
}

struct Specification {
    id: &'static str,
    title: &'static str,
    unit: &'static str,
    lines: Vec<Reading>,
    maximum: Option<f32>,
    stack: bool,
    fill: f32,
    help: &'static str,
}

struct Reading {
    key: &'static str,
    name: &'static str,
    color: &'static str,
    divisor: f32,
    negative: bool,
    dashed: bool,
}

impl Domain {
    fn read(selection: &Value) -> Result<Self, String> {
        let second = integer(&selection["second"])?;
        let first = second.saturating_sub(899);
        let end = second.checked_add(1).ok_or("Live chart time overflow")?;
        let span = u16::try_from(end - first).map_err(|error| error.to_string())?;
        assert!(span > 0);
        assert!(span <= 900);
        Ok(Self { first, end, span })
    }

    fn position(&self, wall_ns: u64) -> Result<f32, String> {
        let second = wall_ns / NANOSECONDS_PER_SECOND;
        if !(self.first..self.end).contains(&second) {
            return Err("Live observation is outside the chart window".to_owned());
        }
        let seconds = u16::try_from(second - self.first).map_err(|error| error.to_string())?;
        let remainder =
            u32::try_from(wall_ns % NANOSECONDS_PER_SECOND).map_err(|error| error.to_string())?;
        let fraction = remainder
            .to_string()
            .parse::<f32>()
            .map_err(|error| error.to_string())?
            / 1_000_000_000.0;
        Ok((f32::from(seconds) + fraction) / f32::from(self.span))
    }
}

fn coordinates(
    points: &[Value],
    domain: &Domain,
    app: bool,
    selected: &Value,
) -> Result<Coordinates, String> {
    assert!(points.len() <= OBSERVATIONS_MAX);
    let mut positions = Vec::with_capacity(points.len());
    let mut segments = Vec::with_capacity(points.len());
    let mut identities = BTreeMap::new();
    let target = if selected.is_null() {
        None
    } else {
        Some(observation_signature(selected)?)
    };
    let mut selected_index = None;
    for (index, point) in points.iter().enumerate() {
        let observation = if app { &point["observation"] } else { point };
        positions.push(domain.position(integer(&observation["wall_ns"])?)?);
        let stamp = if app {
            &observation["stamp"]
        } else {
            observation
        };
        let key = (
            identity(&stamp["run"])?,
            identity(&stamp["boot"])?,
            integer(&point["segment"])?,
        );
        let next = u64::try_from(identities.len()).map_err(|error| error.to_string())?;
        segments.push(*identities.entry(key).or_insert(next));
        if let Some(target) = &target {
            if observation_signature(observation)? == *target {
                if selected_index.is_some() {
                    return Err("Duplicate selected live chart observation".to_owned());
                }
                selected_index = Some(index);
            }
        }
    }
    Ok(Coordinates {
        positions,
        segments,
        selected_index,
    })
}

fn observation_signature(value: &Value) -> Result<(String, String, u64), String> {
    let stamp = match value.get("stamp") {
        Some(stamp) => stamp,
        None => value,
    };
    Ok((
        identity(&stamp["run"])?,
        identity(&stamp["boot"])?,
        integer(&stamp["publication"])?,
    ))
}

fn identity(value: &Value) -> Result<String, String> {
    if let Some(text) = value.as_str() {
        if text.len() == 32 {
            if text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Ok(text.to_ascii_lowercase());
            }
        }
        return Err("Invalid live chart identity".to_owned());
    }
    let words = value.as_array().ok_or("Missing live chart identity")?;
    if words.len() != 2 {
        return Err("Invalid live chart identity word count".to_owned());
    }
    Ok(format!(
        "{:016x}{:016x}",
        integer(&words[0])?,
        integer(&words[1])?
    ))
}

fn integer(value: &Value) -> Result<u64, String> {
    match value {
        Value::String(text) => {
            if text.len() > 20 {
                return Err("Live chart integer exceeds its bound".to_owned());
            }
            text.parse::<u64>().map_err(|error| error.to_string())
        }
        value => value
            .as_u64()
            .ok_or_else(|| "Missing live chart integer".to_owned()),
    }
}

fn number(value: &Value) -> Result<Option<f32>, String> {
    if value.is_null() {
        return Ok(None);
    }
    if value.is_object() {
        if value["overflowed"] == true {
            return Ok(None);
        }
        if value["measured_members"] == 0 {
            return Ok(None);
        }
        return scalar(&value["known"]);
    }
    scalar(value)
}

fn scalar(value: &Value) -> Result<Option<f32>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let text = match value {
        Value::String(text) => text.clone(),
        Value::Number(value) => value.to_string(),
        _ => return Err("Invalid live chart quantity".to_owned()),
    };
    if text.len() > 64 {
        return Err("Live chart quantity exceeds its bound".to_owned());
    }
    let value = text.parse::<f32>().map_err(|error| error.to_string())?;
    if !value.is_finite() {
        return Err("Nonfinite live chart quantity".to_owned());
    }
    Ok(Some(value))
}

fn quantity_text(value: f32, unit: &str) -> String {
    if !value.is_finite() {
        return "Unavailable".to_owned();
    }
    let magnitude = value.abs();
    let (value, unit) = match unit {
        "W" if (f32::MIN_POSITIVE..1.0).contains(&magnitude) => (value * 1000.0, "mW"),
        "GiB" if (f32::MIN_POSITIVE..1.0).contains(&magnitude) => (value * 1024.0, "MiB"),
        "MB/s" if (f32::MIN_POSITIVE..0.001).contains(&magnitude) => (value * 1_000_000.0, "B/s"),
        "MB/s" if (f32::MIN_POSITIVE..1.0).contains(&magnitude) => (value * 1000.0, "kB/s"),
        "% of one core" | "% of interval" => (value, "%"),
        _ => (value, unit),
    };
    if unit.is_empty() {
        format!("{value:.1}")
    } else {
        format!("{value:.1} {unit}")
    }
}

fn panel(
    specification: &Specification,
    points: &[Value],
    coordinates: &Coordinates,
) -> Result<Panel, String> {
    let mut lines = Vec::with_capacity(specification.lines.len());
    for reading in &specification.lines {
        let mut values = Vec::with_capacity(points.len());
        for point in points {
            values.push(number(&point[reading.key])?.map(|value| value / reading.divisor));
        }
        lines.push(Line {
            name: reading.name.to_owned(),
            color: reading.color.to_owned(),
            values,
            stack: None,
            negative: Some(reading.negative),
            dashed: Some(reading.dashed),
            axis: None,
            unit: Some(specification.unit.to_owned()),
            fill: if reading.key == "swap_total_bytes" {
                Some(0.0)
            } else {
                None
            },
        });
    }
    let maximum = if specification.id == "live-gpu-activity" {
        Some(
            lines[0]
                .values
                .iter()
                .flatten()
                .fold(100.0_f32, |high, value| high.max(*value)),
        )
    } else {
        specification.maximum
    };
    Ok(Panel {
        id: specification.id.to_owned(),
        title: specification.title.to_owned(),
        unit: specification.unit.to_owned(),
        kind: "line".to_owned(),
        lines,
        minimum: Some(0.0),
        maximum,
        stack: Some(specification.stack),
        fill: Some(specification.fill),
        caption: None,
        help: Some(specification.help.to_owned()),
        source: Some("Live recording".into()),
        quantities: None,
        states: None,
        table_headings: None,
        table_rows: None,
        positions: Some(coordinates.positions.clone()),
        segments: Some(coordinates.segments.clone()),
        selected_index: coordinates.selected_index,
    })
}

fn specification(
    id: &'static str,
    title: &'static str,
    unit: &'static str,
    lines: Vec<Reading>,
) -> Specification {
    Specification {
        id,
        title,
        unit,
        lines,
        maximum: None,
        stack: false,
        fill: 0.1,
        help: "Recorded readings over the last 15 minutes. Unavailable intervals remain gaps.",
    }
}

fn reading(key: &'static str, name: &'static str, color: &'static str, divisor: f32) -> Reading {
    Reading {
        key,
        name,
        color,
        divisor,
        negative: false,
        dashed: false,
    }
}

fn cpu() -> Specification {
    let mut specification = specification(
        "live-cpu",
        "CPU activity",
        "%",
        vec![reading("cpu_percent", "CPU activity", "#5794f2", 1.0)],
    );
    specification.maximum = Some(100.0);
    specification.help = "Busy scheduler ticks divided by total ticks across logical CPUs.";
    specification
}

fn memory() -> Specification {
    specification(
        "live-memory",
        "Recorded memory categories",
        "GiB",
        vec![
            reading("memory_active_bytes", "Active", "#5794f2", GIBIBYTE),
            reading("memory_wired_bytes", "Wired", "#ff9830", GIBIBYTE),
            reading("memory_compressed_bytes", "Compressed", "#b877d9", GIBIBYTE),
        ],
    )
}

fn gpu_power() -> Specification {
    specification(
        "live-gpu-power",
        "GPU power",
        "W",
        vec![reading("gpu_watts", "GPU power", "#b877d9", 1.0)],
    )
}

fn specifications(route: &str) -> Vec<Specification> {
    match route {
        "overview" => overview(),
        "cpu" => vec![cpu()],
        "memory" => memory_specifications(),
        "gpu" => vec![
            specification(
                "live-gpu-activity",
                "GPU activity",
                "%",
                vec![reading(
                    "gpu_active_percent",
                    "GPU activity",
                    "#b877d9",
                    1.0,
                )],
            ),
            gpu_power(),
        ],
        "energy" => vec![specification(
            "live-energy",
            "Recorded power",
            "W",
            vec![
                reading("power_watts", "System power", "#ff9830", 1.0),
                reading("gpu_watts", "GPU power", "#b877d9", 1.0),
            ],
        )],
        "network" => network(),
        "apps" => app_specifications(),
        _ => Vec::new(),
    }
}

fn overview() -> Vec<Specification> {
    let mut memory = memory();
    memory.title = "Memory";
    memory.stack = true;
    memory.fill = 1.0;
    let mut result = vec![
        cpu(),
        memory,
        gpu_power(),
        specification(
            "live-received",
            "External interfaces · received",
            "KiB/s",
            vec![reading("network_rx_bps", "Received", "#73bf69", 1024.0)],
        ),
        specification(
            "live-sent",
            "External interfaces · sent",
            "KiB/s",
            vec![reading("network_tx_bps", "Sent", "#b877d9", 1024.0)],
        ),
        specification(
            "live-power",
            "System power",
            "W",
            vec![reading("power_watts", "System power", "#ff9830", 1.0)],
        ),
        specification(
            "live-temperature",
            "SoC temperature",
            "°C",
            vec![reading("soc_celsius", "SoC temperature", "#f2495c", 1.0)],
        ),
    ];
    for specification in &mut result {
        if !specification.stack {
            specification.fill = 0.0;
        }
    }
    result
}

fn memory_specifications() -> Vec<Specification> {
    let mut allocated = reading("swap_total_bytes", "Allocated", "#d1d1de", GIBIBYTE);
    allocated.dashed = true;
    let mut compressions = reading(
        "memory_compressions_per_second",
        "Compressed",
        "#ff9830",
        1.0,
    );
    compressions.dashed = true;
    let mut decompressions = reading(
        "memory_decompressions_per_second",
        "Decompressed",
        "#b877d9",
        1.0,
    );
    decompressions.dashed = true;
    vec![
        memory(),
        specification(
            "live-swap",
            "Swap",
            "GiB",
            vec![
                reading("swap_used_bytes", "Used", "#b877d9", GIBIBYTE),
                allocated,
            ],
        ),
        specification(
            "live-paging",
            "Page-ins and compression",
            "/s",
            vec![
                reading("memory_pageins_per_second", "Page-ins", "#5794f2", 1.0),
                compressions,
                decompressions,
            ],
        ),
    ]
}

fn network() -> Vec<Specification> {
    let mut sent = reading("network_tx_bps", "Sent", "#b877d9", 1024.0);
    sent.negative = true;
    let mut sent_packets = reading("network_tx_packets_per_second", "Sent", "#b877d9", 1.0);
    sent_packets.negative = true;
    sent_packets.dashed = true;
    let mut sent_errors = reading(
        "network_tx_errors_per_second",
        "Send errors",
        "#ff9830",
        1.0,
    );
    sent_errors.dashed = true;
    vec![
        specification(
            "live-network",
            "External interfaces",
            "KiB/s",
            vec![
                reading("network_rx_bps", "Received", "#73bf69", 1024.0),
                sent,
            ],
        ),
        specification(
            "live-packets",
            "Packets",
            "packets/s",
            vec![
                reading("network_rx_packets_per_second", "Received", "#73bf69", 1.0),
                sent_packets,
            ],
        ),
        specification(
            "live-network-errors",
            "Network errors",
            "errors/s",
            vec![
                reading(
                    "network_rx_errors_per_second",
                    "Receive errors",
                    "#f2495c",
                    1.0,
                ),
                sent_errors,
            ],
        ),
    ]
}

fn app_specifications() -> Vec<Specification> {
    vec![
        specification(
            "live-app-cpu",
            "CPU",
            "%",
            vec![reading("cpu_percent_one_core", "CPU", "#5794f2", 1.0)],
        ),
        specification(
            "live-app-resident",
            "Resident",
            "GiB",
            vec![reading("resident_bytes", "Resident", "#5794f2", GIBIBYTE)],
        ),
        specification(
            "live-app-footprint",
            "Footprint",
            "GiB",
            vec![reading("footprint_bytes", "Footprint", "#ff9830", GIBIBYTE)],
        ),
        specification(
            "live-app-power",
            "Average power",
            "W",
            vec![reading("energy_watts", "Average power", "#ff9830", 1.0)],
        ),
    ]
}

#[cfg(test)]
#[path = "live_projection_tests.rs"]
mod tests;
