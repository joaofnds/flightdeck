use std::collections::BTreeMap;
use std::rc::Rc;

use num_traits::ToPrimitive;
use serde_json::Value;

use crate::data::{Line, Panel};

#[path = "live_resource_projection.rs"]
mod resource_projection;

const OBSERVATIONS_MAX: usize = 4096;
pub const OVERVIEW_PANELS_MAX: usize = 9;
pub const ROUTE_PANELS_MAX: usize = 7;
const GIBIBYTE: f32 = 1_073_741_824.0;
const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;

#[must_use]
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
        .rfind(|point| {
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
    let limit = if route == "overview" {
        OVERVIEW_PANELS_MAX
    } else {
        ROUTE_PANELS_MAX
    };
    let mut result = Vec::with_capacity(limit);
    for specification in specifications(route, points)? {
        let mut panel = panel(&specification, points, &coordinates)?;
        if specification.id == "live-memory-pressure" {
            pressure_cells(&mut panel, &domain, points, &coordinates)?;
        }
        if specification.id == "live-battery-charging" {
            resource_projection::charging_cells(&mut panel, &domain, points, &coordinates)?;
        }

        result.push(Rc::new(panel));
    }
    assert!(result.len() <= limit);
    Ok(result)
}

#[must_use]
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

#[must_use]
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
    channel: Option<usize>,
    resource: Option<&'static str>,
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
            .to_f32()
            .ok_or("Live chart fractional second exceeds f32")?
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
            let observed = (key.0, key.1, integer(&stamp["publication"])?);
            if observed == *target {
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

fn observation_signature(value: &Value) -> Result<([u64; 2], [u64; 2], u64), String> {
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

fn identity(value: &Value) -> Result<[u64; 2], String> {
    if let Some(text) = value.as_str() {
        if text.len() == 32 {
            if text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Ok([
                    u64::from_str_radix(&text[..16], 16).map_err(|error| error.to_string())?,
                    u64::from_str_radix(&text[16..], 16).map_err(|error| error.to_string())?,
                ]);
            }
        }
        return Err("Invalid live chart identity".to_owned());
    }
    let words = value.as_array().ok_or("Missing live chart identity")?;
    if words.len() != 2 {
        return Err("Invalid live chart identity word count".to_owned());
    }
    Ok([integer(&words[0])?, integer(&words[1])?])
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
        Value::String(text) => return scalar_text(text),
        Value::Number(value) => value.to_string(),
        _ => return Err("Invalid live chart quantity".to_owned()),
    };
    scalar_text(&text)
}

fn scalar_text(text: &str) -> Result<Option<f32>, String> {
    if text.len() > 64 {
        return Err("Live chart quantity exceeds its bound".to_owned());
    }

    let value = text.parse::<f32>().map_err(|error| error.to_string())?;
    if !value.is_finite() {
        return Err("Nonfinite live chart quantity".to_owned());
    }

    Ok(Some(value))
}

pub(crate) fn quantity_text(value: f32, unit: &str) -> String {
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
    if value != 0.0 {
        if value.abs() < 1.0 {
            let number = quantity_text_small(value);
            return if unit.is_empty() {
                number
            } else {
                format!("{number} {unit}")
            };
        }
    }
    if unit.is_empty() {
        format!("{value:.1}")
    } else {
        format!("{value:.1} {unit}")
    }
}

fn quantity_text_small(value: f32) -> String {
    assert!(value.is_finite());
    assert!(value != 0.0);
    assert!(value.abs() < 1.0);
    let mut magnitude = value.abs();
    for precision in 2..14_usize {
        if magnitude >= 1.0 {
            return format!("{value:.precision$}")
                .trim_end_matches('0')
                .trim_end_matches('.')
                .to_owned();
        }
        magnitude *= 10.0;
    }

    format!("{value:.2e}")
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
            let source = reading.resource.map_or(point, |resource| &point[resource]);
            let value = match reading.channel {
                Some(channel) => &source[reading.key][channel],
                None => &source[reading.key],
            };
            values.push(
                resource_projection::value(reading, value)?.map(|value| value / reading.divisor),
            );
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
        channel: None,
        resource: None,
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

fn gpu_activity() -> Specification {
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
    )
}

fn specifications(route: &str, points: &[Value]) -> Result<Vec<Specification>, String> {
    Ok(match route {
        "overview" => overview(),
        "cpu" => vec![cpu()],
        "memory" => memory_specifications(),
        "gpu" => vec![gpu_activity(), gpu_power()],
        "energy" => energy(points)?,
        "network" => network(),
        "storage" => resource_projection::storage(),
        "battery" => resource_projection::battery(),
        "apps" => app_specifications(),
        _ => Vec::new(),
    })
}

fn energy(points: &[Value]) -> Result<Vec<Specification>, String> {
    Ok(vec![
        specification(
            "live-energy",
            "Recorded power",
            "W",
            vec![
                reading("power_watts", "System power", "#ff9830", 1.0),
                reading("gpu_watts", "GPU power", "#b877d9", 1.0),
            ],
        ),
        specification(
            "live-soc-temperature",
            "SoC temperature",
            "°C",
            vec![reading("soc_celsius", "SoC temperature", "#f2495c", 1.0)],
        ),
        fan_specification(points)?,
    ])
}

fn fan_specification(points: &[Value]) -> Result<Specification, String> {
    const CHANNELS: [(&str, &str); 10] = [
        ("Fan 0", "#5794f2"),
        ("Fan 1", "#ff9830"),
        ("Fan 2", "#b877d9"),
        ("Fan 3", "#73bf69"),
        ("Fan 4", "#f2495c"),
        ("Fan 5", "#f2cc0c"),
        ("Fan 6", "#56b9c3"),
        ("Fan 7", "#f28eb9"),
        ("Fan 8", "#d1d1de"),
        ("Fan 9", "#9d8bd2"),
    ];
    let mut channels = 0;

    for point in points {
        if !point["fan_rpm"].is_null() {
            let values = point["fan_rpm"]
                .as_array()
                .ok_or("Invalid fan channel array")?;
            if values.len() > CHANNELS.len() {
                return Err("Fan history exceeds ten channels".into());
            }

            channels = channels.max(values.len());
        }
    }

    let mut lines = Vec::with_capacity(channels);
    for (channel, (name, color)) in CHANNELS.into_iter().take(channels).enumerate() {
        let mut line = reading("fan_rpm", name, color, 1.0);
        line.channel = Some(channel);
        line.dashed = channel % 2 == 1;
        lines.push(line);
    }

    let mut specification = specification("live-fans", "Fan speed", "RPM", lines);
    specification.fill = 0.0;
    specification.help = "SMC channels retain their numeric indices, without physical-side \
        attribution. Zero means measured stopped; unavailable channels remain gaps. \
        Channel count can change between observations.";

    Ok(specification)
}

fn pressure_cells(
    panel: &mut Panel,
    domain: &Domain,
    points: &[Value],
    coordinates: &Coordinates,
) -> Result<(), String> {
    timeline_cells(panel, domain, points, coordinates, pressure_state)?;
    panel.states = Some(vec!["Normal".into(), "Warning".into(), "Critical".into()]);
    panel.help = Some(
        "macOS memory pressure by observed second. The chosen same-second \
        publication takes priority; otherwise the latest reading is shown. Blank means \
        unavailable or unrecognized, including gaps."
            .to_owned(),
    );

    Ok(())
}

fn timeline_cells(
    panel: &mut Panel,
    domain: &Domain,
    points: &[Value],
    coordinates: &Coordinates,
    state: impl Fn(Option<f32>) -> Option<f32>,
) -> Result<(), String> {
    let span = usize::from(domain.span);
    let mut cells = vec![None; span];
    let mut selected = None;

    for (index, point) in points.iter().enumerate() {
        let second = integer(&point["wall_ns"])? / NANOSECONDS_PER_SECOND;
        let cell = usize::try_from(second - domain.first).map_err(|error| error.to_string())?;
        assert!(cell < span);
        if Some(cell) != selected {
            cells[cell] = state(panel.lines[0].values[index]);
        }
        if Some(index) == coordinates.selected_index {
            selected = Some(cell);
        }
    }

    "timeline".clone_into(&mut panel.kind);
    panel.lines[0].values = cells;
    panel.positions = Some(
        (0..domain.span)
            .map(|cell| (f32::from(cell) + 0.5) / f32::from(domain.span))
            .collect(),
    );
    panel.segments = None;
    panel.selected_index = selected;
    Ok(())
}

fn pressure_state(value: Option<f32>) -> Option<f32> {
    match value {
        Some(0.0 | 1.0 | 2.0) => value,
        _ => None,
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
        gpu_activity(),
        resource_projection::throughput(),
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
        memory_pressure(),
    ];
    for specification in &mut result {
        if !specification.stack {
            specification.fill = 0.0;
        }
    }
    result
}

fn memory_specifications() -> Vec<Specification> {
    vec![
        memory(),
        memory_pressure(),
        memory_swap(),
        memory_paging(),
        memory_swap_flow(),
    ]
}

fn memory_pressure() -> Specification {
    specification(
        "live-memory-pressure",
        "Memory pressure",
        "",
        vec![reading("pressure", "Memory pressure", "#73bf69", 1.0)],
    )
}

fn memory_swap() -> Specification {
    let mut allocated = reading("swap_total_bytes", "Allocated", "#d1d1de", GIBIBYTE);
    allocated.dashed = true;

    specification(
        "live-swap",
        "Swap",
        "GiB",
        vec![
            reading("swap_used_bytes", "Used", "#b877d9", GIBIBYTE),
            allocated,
        ],
    )
}

fn memory_paging() -> Specification {
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

    specification(
        "live-paging",
        "Page-ins and compression",
        "/s",
        vec![
            reading("memory_pageins_per_second", "Page-ins", "#5794f2", 1.0),
            compressions,
            decompressions,
        ],
    )
}

fn memory_swap_flow() -> Specification {
    let mut swap_out = reading(
        "swap_out_bytes_per_second",
        "Swap-out",
        "#b877d9",
        1_048_576.0,
    );
    swap_out.negative = true;

    specification(
        "live-swap-flow",
        "Swap traffic",
        "MiB/s",
        vec![
            reading(
                "swap_in_bytes_per_second",
                "Swap-in",
                "#5794f2",
                1_048_576.0,
            ),
            swap_out,
        ],
    )
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

#[cfg(test)]
#[path = "live_resource_projection_tests.rs"]
mod resource_tests;
