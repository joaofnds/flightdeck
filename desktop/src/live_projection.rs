use std::collections::BTreeMap;
use std::rc::Rc;

use num_traits::ToPrimitive;
use serde_json::Value;

use crate::data::{Line, Panel};
use crate::live_inventory::PerformanceLevel;

#[path = "live_resource_projection.rs"]
mod resource_projection;

pub const OBSERVATIONS_MAX: usize = 4096;
pub const OVERVIEW_PRIMARY_PANELS: usize = 6;
/// The Overview chart that sets load, power and heat side by side.
pub const CORRELATION_ID: &str = "live-correlation";
pub const OVERVIEW_PANELS_MAX: usize = 8;
pub const ROUTE_PANELS_MAX: usize = 7;
const _: () = assert!(OVERVIEW_PRIMARY_PANELS < OVERVIEW_PANELS_MAX);
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
    let process = source["scope"] == "selected_named_process_group";
    let limit = if process {
        8
    } else {
        if route == "overview" {
            OVERVIEW_PANELS_MAX
        } else {
            ROUTE_PANELS_MAX
        }
    };
    let mut result = Vec::with_capacity(limit);
    let specifications = if process {
        process_specifications()
    } else {
        specifications(route, points)?
    };
    for specification in specifications {
        let mut panel = panel(&specification, points, &coordinates)?;
        if specification.id == "live-memory-pressure" {
            pressure_cells(&mut panel, &domain, points, &coordinates)?;
        }
        if specification.id == THERMAL_STATE_ID {
            thermal_cells(&mut panel, &domain, points, &coordinates)?;
        }
        if specification.id == "live-battery-charging" {
            resource_projection::charging_cells(&mut panel, &domain, points, &coordinates)?;
        }

        result.push(Rc::new(panel));
    }
    if route == "gpu" {
        if let Some(panel) = gpu_apps_panel(selection, points, &coordinates)? {
            result.push(Rc::new(panel));
        }
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

/// A reading with fixed fraction digits, as the prototype's `hardwareReading` writes the
/// readings of its quantity lists, such as "58.1 °C" and "51.0%".
#[must_use]
pub fn hardware_reading(summary: &Value, key: &str, suffix: &str, digits: usize) -> String {
    match number(&summary[key]) {
        Ok(Some(value)) if value.is_finite() => format!("{}{suffix}", fixed_text(value, digits)),
        Ok(_) => "Unavailable".to_owned(),
        Err(error) => {
            eprintln!("flightdeck live hardware reading {key}: {error}");
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
    window: crate::chart_axes::TimeWindow,
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
    /// The line of text under the legend, as the prototype gives the battery's power flow.
    caption: Option<&'static str>,
}

struct Reading {
    key: &'static str,
    name: &'static str,
    color: &'static str,
    divisor: f32,
    negative: bool,
    dashed: bool,
    stack: Option<bool>,
    fill: Option<f32>,
    unit: Option<&'static str>,
    channel: Option<usize>,
    resource: Option<&'static str>,
    axis: Option<&'static str>,
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
        window: crate::chart_axes::TimeWindow {
            first: domain.first,
            span: u64::from(domain.span),
        },
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

pub(crate) fn identity(value: &Value) -> Result<[u64; 2], String> {
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

pub(crate) fn integer(value: &Value) -> Result<u64, String> {
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

/// A reading as the prototype's `axisValue` writes it: a unit below 1 takes its smaller
/// prefix, a count from 1000 takes k or M, and the number has no decimals at zero or from 100,
/// one from 1, and two significant digits below 1, with thousands grouped.
pub(crate) fn quantity_text(value: f32, unit: &str) -> String {
    if !value.is_finite() {
        return "Unavailable".to_owned();
    }
    let (value, unit) = quantity_text_scaled(value, unit);
    let number = quantity_text_number(value);
    if unit.is_empty() {
        return number;
    }
    if unit.starts_with('%') {
        return format!("{number}{unit}");
    }
    format!("{number} {unit}")
}

fn quantity_text_scaled(value: f32, unit: &str) -> (f32, String) {
    let magnitude = value.abs();
    // The prototype's `plotUnit` writes every percent unit, such as "% of one core", as "%".
    let unit = if unit.starts_with('%') { "%" } else { unit };
    match unit {
        "W" if magnitude == 0.0 => (value, "W".into()),
        "W" if magnitude < 1.0 => (value * 1000.0, "mW".into()),
        "GiB" if magnitude == 0.0 => (value, "B".into()),
        "GiB" if magnitude < 1.0 => (value * 1024.0, "MiB".into()),
        "MB/s" if magnitude < 0.001 => (value * 1_000_000.0, "B/s".into()),
        "MB/s" if magnitude < 1.0 => (value * 1000.0, "kB/s".into()),
        "GHz" if magnitude < 1.0 => (value * 1000.0, "MHz".into()),
        "" | "rpm" | "ops/s" | "work items" | "pps" | "/s" | "files" | "threads" | "sockets"
            if magnitude >= 1000.0 =>
        {
            let (scale, prefix) = if magnitude >= 1_000_000.0 {
                (1_000_000.0, "M")
            } else {
                (1000.0, "k")
            };
            let unit = if unit.is_empty() {
                prefix.to_owned()
            } else {
                format!("{prefix} {unit}")
            };
            (value / scale, unit)
        }
        _ => (value, unit.into()),
    }
}

/// The number with the prototype's fraction digits and trailing zeros dropped.
fn quantity_text_number(value: f32) -> String {
    assert!(value.is_finite());
    let magnitude = value.abs();
    let digits = match magnitude {
        0.0 | 100.0.. => 0,
        1.0.. => 1,
        _ => quantity_text_small_digits(magnitude),
    };
    let fixed = fixed_text(value, digits);
    if !fixed.contains('.') {
        return fixed;
    }

    fixed.trim_end_matches('0').trim_end_matches('.').to_owned()
}

/// The number with exactly `digits` fraction digits and its whole part grouped in thousands
/// with commas, as the prototype's `toLocaleString("en-GB")` writes it.
pub(crate) fn fixed_text(value: f32, digits: usize) -> String {
    assert!(value.is_finite());
    assert!(digits <= 6);
    let magnitude = value.abs();
    let fixed = format!("{magnitude:.digits$}");
    let (whole, fraction) = fixed.split_once('.').unwrap_or((fixed.as_str(), ""));
    let mut number = String::with_capacity(fixed.len() + fixed.len() / 3 + 2);
    if value < 0.0 {
        if fixed
            .bytes()
            .any(|byte| byte.is_ascii_digit() && byte != b'0')
        {
            number.push('-');
        }
    }
    for (index, digit) in whole.chars().enumerate() {
        if index > 0 {
            if (whole.len() - index) % 3 == 0 {
                number.push(',');
            }
        }
        number.push(digit);
    }
    if !fraction.is_empty() {
        number.push('.');
        number.push_str(fraction);
    }
    number
}

/// Two significant digits for a magnitude below 1, and at most six fraction digits, as the
/// prototype's `1 - floor(log10(magnitude))` gives them.
fn quantity_text_small_digits(magnitude: f32) -> usize {
    assert!(magnitude > 0.0);
    assert!(magnitude < 1.0);
    let mut scaled = magnitude;
    let mut zeros = 0;
    while scaled < 1.0 {
        if zeros == 6 {
            break;
        }
        scaled *= 10.0;
        zeros += 1;
    }
    (zeros + 1).clamp(2, 6)
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
            values.push(panel_reading_value(reading, point)?.map(|value| value / reading.divisor));
        }
        lines.push(Line {
            name: reading.name.to_owned(),
            color: reading.color.to_owned(),
            values,
            stack: reading.stack,
            negative: Some(reading.negative),
            dashed: Some(reading.dashed),
            axis: reading.axis.map(str::to_owned),
            unit: Some(
                reading
                    .unit
                    .map_or(specification.unit, |unit| unit)
                    .to_owned(),
            ),
            fill: reading.fill,
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
        caption: specification.caption.map(str::to_owned),
        help: Some(specification.help.to_owned()),
        source: Some("Live recording".into()),
        quantities: None,
        states: None,
        table_headings: None,
        table_rows: None,
        positions: Some(coordinates.positions.clone()),
        segments: Some(coordinates.segments.clone()),
        selected_index: coordinates.selected_index,
        window: Some(coordinates.window),
    })
}

const GPU_APPS_HELP: &str = "GPU time each App's processes took per interval, from the GPU \
    driver's own count, as a share of the interval. The eight Apps with the most GPU time over \
    the window lead, and Other sums the rest. The dashed GPU busy line is the GPU's own active \
    time, which work from several Apps at once can sit under. The app samples GPU time each \
    second while it runs, so time before it started has no reading.";

/// The stacked chart of GPU time by App: Other and each leader the app sampled, with the
/// GPU's own busy share dashed over them. A selection without sampled GPU time has none.
fn gpu_apps_panel(
    selection: &Value,
    points: &[Value],
    coordinates: &Coordinates,
) -> Result<Option<Panel>, String> {
    let Some(cohort) = selection["gpu_app_cohort"].as_array() else {
        return Ok(None);
    };
    if cohort.len() + 1 > APP_COLORS.len() {
        return Err("GPU App cohort exceeds eight leaders".into());
    }
    let mut names = Vec::with_capacity(cohort.len() + 1);
    names.push("Other");
    for name in cohort {
        names.push(name.as_str().ok_or("GPU App name unavailable")?);
    }
    let mut lines = Vec::with_capacity(names.len() + 1);
    for (channel, name) in names.into_iter().enumerate() {
        lines.push(gpu_apps_panel_line(name, channel, points)?);
    }
    let mut busy = Vec::with_capacity(points.len());
    for point in points {
        busy.push(number(&point["gpu_active_percent"])?);
    }
    lines.push(Line {
        name: "GPU busy".into(),
        color: "#ccccdc".into(),
        values: busy,
        stack: Some(false),
        negative: Some(false),
        dashed: Some(true),
        axis: None,
        unit: Some("%".into()),
        fill: Some(0.0),
    });
    Ok(Some(Panel {
        id: GPU_APPS_ID.to_owned(),
        title: "GPU time by app".to_owned(),
        unit: "%".to_owned(),
        kind: "line".to_owned(),
        lines,
        minimum: Some(0.0),
        maximum: None,
        stack: Some(true),
        fill: Some(1.0),
        caption: None,
        help: Some(GPU_APPS_HELP.to_owned()),
        source: Some("Sampled by the app".into()),
        quantities: None,
        states: None,
        table_headings: None,
        table_rows: None,
        positions: Some(coordinates.positions.clone()),
        segments: Some(coordinates.segments.clone()),
        selected_index: coordinates.selected_index,
        window: Some(coordinates.window),
    }))
}

/// The stacked line of one App's share of GPU time, at `channel` of each point's shares.
fn gpu_apps_panel_line(name: &str, channel: usize, points: &[Value]) -> Result<Line, String> {
    let mut values = Vec::with_capacity(points.len());
    for point in points {
        values.push(number(&point["gpu_app_percent"][channel])?);
    }

    Ok(Line {
        name: name.to_owned(),
        color: APP_COLORS[channel].to_owned(),
        values,
        stack: None,
        negative: Some(false),
        dashed: Some(false),
        axis: None,
        unit: Some("%".into()),
        fill: None,
    })
}

fn panel_reading_value(reading: &Reading, point: &Value) -> Result<Option<f32>, String> {
    if reading.key == "power_remainder_watts" {
        let system = number(&point["power_watts"])?;
        let gpu = number(&point["gpu_watts"])?;
        let Some((system, gpu)) = system.zip(gpu) else {
            return Ok(None);
        };

        let remainder = system - gpu;
        if !remainder.is_finite() {
            return Err("Nonfinite derived power remainder".into());
        }

        return Ok(Some(remainder));
    }

    if reading.key == FAN_MEAN_KEY {
        return fan_mean(&point["fan_rpm"]);
    }

    if reading.key == "memory_app_bytes" {
        return resource_projection::value(reading, &app_memory(point));
    }

    let source = reading.resource.map_or(point, |resource| &point[resource]);
    let value = match reading.channel {
        Some(channel) => &source[reading.key][channel],
        None => &source[reading.key],
    };

    resource_projection::value(reading, value)
}

/// App memory as Activity Monitor counts it: anonymous memory that macOS may not purge. A
/// purgeable count above the anonymous one leaves it unknown rather than zero.
pub(crate) fn app_memory(point: &Value) -> Value {
    let anonymous = point["memory_anonymous_bytes"].as_u64();
    let purgeable = point["memory_purgeable_bytes"].as_u64();
    anonymous
        .zip(purgeable)
        .and_then(|(anonymous, purgeable)| anonymous.checked_sub(purgeable))
        .map_or(Value::Null, Value::from)
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
        caption: None,
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
        stack: None,
        fill: None,
        unit: None,
        channel: None,
        resource: None,
        axis: None,
    }
}

/// CPU activity across all cores and within each performance level, the slower level first
/// as the prototype lists it. The level's name comes from `level_name`, since the Overview
/// names a level alone and the CPU page names its cores.
fn cpu(level_name: fn(&'static PerformanceLevel) -> &'static str) -> Specification {
    let mut lines = vec![reading("cpu_percent", "All cores", "#5794f2", 1.0)];
    if let Some(levels) = crate::live_inventory::performance_levels() {
        for (index, color) in [(1, "#73bf69"), (0, "#b877d9")] {
            let mut line = reading("cpu_level_percent", level_name(&levels[index]), color, 1.0);
            line.channel = Some(index);
            lines.push(line);
        }
    }
    let mut specification = specification("live-cpu", "CPU activity", "%", lines);
    specification.maximum = Some(100.0);
    specification.help = "Busy scheduler ticks divided by total ticks across logical CPUs. \
        Each level averages its own CPUs, as macOS groups them.";
    specification
}

pub const LOAD_AVERAGE_ID: &str = "live-load-average";
pub const OPEN_FILES_ID: &str = "live-open-files";

pub const FREQUENCY_ID: &str = "live-cpu-frequency";
pub const CPU_TEMPERATURE_ID: &str = "live-cpu-temperature";

/// The hottest sensor in the CPU area. The collector does not record it, so the chart has the
/// prototype's panel and no readings.
fn cpu_temperature() -> Specification {
    let mut specification = specification(
        CPU_TEMPERATURE_ID,
        "CPU area maximum",
        "°C",
        vec![reading("cpu_area_celsius", "CPU", "#5794f2", 1.0)],
    );
    specification.maximum = Some(125.0);
    specification.help = "The hottest measured sensor in the CPU area. This collector does \
        not record it yet, so no readings are shown.";
    specification
}

/// The mean clock of each performance level while its cores were active, from the time they
/// spent in each clock state.
fn frequency() -> Specification {
    let mut lines = Vec::with_capacity(2);
    if let Some(levels) = crate::live_inventory::performance_levels() {
        for (index, color) in [(1, "#73bf69"), (0, "#b877d9")] {
            let mut line = reading(
                "cpu_level_ghz",
                levels[index].cluster_cores.as_str(),
                color,
                1.0,
            );
            line.channel = Some(index);
            lines.push(line);
        }
    }
    let mut specification = specification(FREQUENCY_ID, "CPU active frequency", "GHz", lines);
    specification.help = "The mean clock of each core level while its cores were active, \
        weighting each clock state macOS reports by the time the cores spent in it. The clock \
        of each state comes from the power manager's own tables. macOS changes clock states as \
        the work changes, so this is a measure of the interval and not a fixed maximum.";
    specification
}

/// The load averages the app sampled each second, with the logical CPU count dashed, since
/// work above it waits for a core.
fn load_average() -> Specification {
    let mut lines = Vec::with_capacity(4);
    for (channel, name, color) in [
        (0, "1 minute", "#5794f2"),
        (1, "5 minutes", "#73bf69"),
        (2, "15 minutes", "#b877d9"),
    ] {
        let mut line = reading("load_average", name, color, 1.0);
        line.channel = Some(channel);
        lines.push(line);
    }
    let mut cores = reading("logical_cpu_count", "Cores", "#ccccdc", 1.0);
    cores.dashed = true;
    lines.push(cores);
    let mut specification = specification(LOAD_AVERAGE_ID, "Load average", "work items", lines);
    specification.fill = 0.0;
    specification.help = "Work running or waiting for a CPU, averaged by macOS over 1, 5 and \
        15 minutes. Above the dashed core count, work waits for a core. Flightdeck samples it \
        once a second while the app runs, so earlier seconds have no reading.";
    specification
}

/// The file entries open across the system, with their share of the most macOS allows on
/// its own axis.
fn open_files() -> Specification {
    let mut share = reading("open_files_percent", "Share of limit", "#ccccdc", 1.0);
    share.unit = Some("%");
    share.axis = Some("right1");
    share.dashed = true;
    share.fill = Some(0.0);
    let mut specification = specification(
        OPEN_FILES_ID,
        "Open files",
        "files",
        vec![reading("open_files", "Open", "#00ffff", 1.0), share],
    );
    specification.help = "File entries open across the system and their share of the \
        kernel's limit. Flightdeck samples them once a second while the app runs, so earlier \
        seconds have no reading.";
    specification
}

fn cpu_level_cores(level: &'static PerformanceLevel) -> &'static str {
    level.cores.as_str()
}

fn cpu_level_name(level: &'static PerformanceLevel) -> &'static str {
    level.name.as_str()
}

fn memory() -> Specification {
    let mut installed = reading("memory_total_bytes", "Installed", "#ccccdc", GIBIBYTE);
    installed.dashed = true;
    installed.stack = Some(false);
    installed.fill = Some(0.0);
    let mut specification = specification(
        "live-memory",
        "Memory in use",
        "GiB",
        vec![
            reading("memory_app_bytes", "App memory", "#73bf69", GIBIBYTE),
            reading("memory_wired_bytes", "Wired", "#5794f2", GIBIBYTE),
            reading("memory_compressed_bytes", "Compressed", "#b877d9", GIBIBYTE),
            installed,
        ],
    );
    specification.stack = true;
    specification.fill = 0.45;

    specification
}

fn system_power(id: &'static str) -> Specification {
    specification(
        id,
        "System power",
        "W",
        vec![reading("power_watts", "Whole machine", "#ff9830", 1.0)],
    )
}

fn gpu_power() -> Specification {
    specification(
        "live-gpu-power",
        "GPU power",
        "W",
        vec![reading("gpu_watts", "GPU", "#b877d9", 1.0)],
    )
}

/// The colors of an App chart's lines, Other first and then each of its eight leaders.
pub const APP_COLORS: [&str; 9] = [
    "#808080", "#f2cc0c", "#5794f2", "#ff9830", "#f2495c", "#b877d9", "#37872d", "#e0b400",
    "#1f60c4",
];

/// GPU time by App, from the shares the app sampled each second.
pub const GPU_APPS_ID: &str = "live-gpu-apps";

/// The unified memory the GPU driver reported in use and allocated, sampled by the app.
pub const GPU_MEMORY_ID: &str = "live-gpu-memory";

fn gpu_memory() -> Specification {
    let mut allocated = reading(
        "gpu_memory_allocated_bytes",
        "Allocated",
        "#ccccdc",
        GIBIBYTE,
    );
    allocated.dashed = true;
    allocated.fill = Some(0.0);

    let mut specification = specification(
        GPU_MEMORY_ID,
        "GPU memory",
        "GiB",
        vec![
            reading("gpu_memory_in_use_bytes", "In use", "#b877d9", GIBIBYTE),
            allocated,
        ],
    );
    specification.help = "The unified memory the GPU driver reports. In use is the memory \
        GPU work holds now, and allocated is what the driver has set aside for it. The GPU \
        shares RAM with the CPU, so this memory also counts in the Mac's memory in use. The \
        app samples the driver each second while it runs, so time before it started has no \
        reading.";
    specification
}

fn gpu_activity() -> Specification {
    specification(
        "live-gpu-activity",
        "GPU activity",
        "%",
        vec![reading("gpu_active_percent", "Busy", "#b877d9", 1.0)],
    )
}

pub const GPU_FREQUENCY_ID: &str = "live-gpu-frequency";
pub const GPU_TEMPERATURE_ID: &str = "live-gpu-temperature";

/// The clock the GPU ran at while active. The collector does not record it, so the chart has
/// the prototype's panel and no readings.
fn gpu_frequency() -> Specification {
    let mut specification = specification(
        GPU_FREQUENCY_ID,
        "GPU active frequency",
        "GHz",
        vec![reading("gpu_frequency_ghz", "Frequency", "#ccccdc", 1.0)],
    );
    specification.help = "The GPU's cycles divided by its active time. This collector does \
        not record it yet, so no readings are shown.";
    specification
}

/// The hottest sensor in the GPU area. The collector does not record it, so the chart has the
/// prototype's panel and no readings.
fn gpu_temperature() -> Specification {
    let mut specification = specification(
        GPU_TEMPERATURE_ID,
        "GPU area maximum",
        "°C",
        vec![reading("gpu_area_celsius", "GPU", "#b877d9", 1.0)],
    );
    specification.maximum = Some(125.0);
    specification.help = "The hottest measured sensor in the GPU area. This collector does \
        not record it yet, so no readings are shown.";
    specification
}

fn specifications(route: &str, points: &[Value]) -> Result<Vec<Specification>, String> {
    Ok(match route {
        "overview" => overview(),
        "cpu" => vec![
            cpu(cpu_level_cores),
            frequency(),
            load_average(),
            open_files(),
            cpu_temperature(),
        ],
        "memory" => memory_specifications(),
        "gpu" => vec![
            gpu_activity(),
            gpu_frequency(),
            gpu_power(),
            gpu_memory(),
            gpu_temperature(),
        ],
        "energy" => energy(points)?,
        "network" => network(),
        "storage" => {
            let mut specifications = resource_projection::storage();
            specifications.push(storage_temperature());
            specifications
        }
        "battery" => resource_projection::battery(),
        "apps" => app_specifications(),
        _ => Vec::new(),
    })
}

fn energy(points: &[Value]) -> Result<Vec<Specification>, String> {
    Ok(vec![
        system_power("live-energy"),
        gpu_power(),
        power_breakdown(),
        specification(
            "live-soc-temperature",
            "SoC temperature",
            "°C",
            vec![reading(
                "soc_celsius",
                "Source SoC group average",
                "#f2495c",
                1.0,
            )],
        ),
        fan_specification(points)?,
        thermal_state(),
        area_maxima(),
    ])
}

pub const THERMAL_STATE_ID: &str = "live-thermal";
pub const AREA_MAXIMA_ID: &str = "live-area-maxima";

fn thermal_state() -> Specification {
    specification(
        THERMAL_STATE_ID,
        "Thermal state",
        "",
        vec![reading("thermal_pressure", "Thermal state", "#73bf69", 1.0)],
    )
}

/// The hottest sensor of each area, which the collector does not record yet.
fn area_maxima() -> Specification {
    let mut specification = specification(
        AREA_MAXIMA_ID,
        "System area maxima",
        "°C",
        vec![
            reading("soc_max_celsius", "Source SoC group max", "#5794f2", 1.0),
            reading(
                "voltage_regulator_celsius",
                "Voltage regulators",
                "#ff9830",
                1.0,
            ),
            reading("display_celsius", "Display", "#8ab8ff", 1.0),
            reading("board_celsius", "Board", "#37872d", 1.0),
            reading("wireless_celsius", "Wireless", "#ca95e5", 1.0),
            reading("ambient_celsius", "Ambient", "#ccccdc", 1.0),
        ],
    );
    specification.maximum = Some(125.0);
    specification.fill = 0.0;
    specification.help = "The hottest measured sensor in each area of the system. This \
        collector does not record them yet, so no readings are shown.";
    specification
}

fn power_breakdown() -> Specification {
    let mut specification = specification(
        "live-power-breakdown",
        "Power",
        "W",
        vec![
            reading(
                "power_remainder_watts",
                "SoC and rest of system",
                "#ff9830",
                1.0,
            ),
            reading("gpu_watts", "GPU", "#b877d9", 1.0),
        ],
    );
    specification.stack = true;
    specification.fill = 0.45;
    specification.help = "SoC and rest of system is system power minus GPU power, \
        not independently measured CPU power. The remainder requires both readings; \
        GPU retains its own availability. The sources have different measurement scopes, \
        so a negative remainder is preserved. The headline sums both available components.";

    specification
}

fn fan_specification(points: &[Value]) -> Result<Specification, String> {
    const CHANNELS: [(&str, &str); 10] = [
        ("Fan 0", "#73bf69"),
        ("Fan 1", "#f2cc0c"),
        ("Fan 2", "#5794f2"),
        ("Fan 3", "#ff9830"),
        ("Fan 4", "#f2495c"),
        ("Fan 5", "#b877d9"),
        ("Fan 6", "#37872d"),
        ("Fan 7", "#e0b400"),
        ("Fan 8", "#1f60c4"),
        ("Fan 9", "#c15c17"),
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
        lines.push(line);
    }

    let mut specification = specification("live-fans", "Fan speed", "rpm", lines);
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

fn thermal_cells(
    panel: &mut Panel,
    domain: &Domain,
    points: &[Value],
    coordinates: &Coordinates,
) -> Result<(), String> {
    timeline_cells(panel, domain, points, coordinates, thermal_state_cell)?;
    panel.states = Some(vec![
        "Nominal".into(),
        "Fair".into(),
        "Serious".into(),
        "Critical".into(),
    ]);
    panel.help = Some(
        "macOS thermal pressure by observed second, as Nominal, Fair, Serious or Critical. \
        Blank means unavailable, including gaps."
            .to_owned(),
    );

    Ok(())
}

/// The thermal pressure levels macOS names Moderate, Heavy, Trapping and Sleeping read as the
/// states Fair, Serious and Critical, with the last two both critical.
fn thermal_state_cell(value: Option<f32>) -> Option<f32> {
    match value {
        Some(0.0) => Some(0.0),
        Some(1.0) => Some(1.0),
        Some(2.0) => Some(2.0),
        Some(3.0 | 4.0) => Some(3.0),
        _ => None,
    }
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
    let mut network = network_throughput();
    network.title = "Network throughput";
    let mut result = vec![
        cpu(cpu_level_name),
        gpu_activity(),
        memory(),
        system_power("live-power"),
        resource_projection::throughput(),
        network,
        memory_pressure(),
        correlation(),
    ];
    for specification in &mut result {
        if !specification.stack {
            specification.fill = 0.0;
        }
    }
    result
}

/// The prototype's overlay of load, power and heat. Power and temperature read on their own
/// right-hand axes, and the fans share a hidden axis, so each keeps its own shape.
fn correlation() -> Specification {
    let mut fans = reading(FAN_MEAN_KEY, "Fans", "#ccccdc", 1.0);
    fans.unit = Some("rpm");
    fans.axis = Some("hidden");
    fans.dashed = true;
    fans.fill = Some(0.0);
    let mut power = reading("power_watts", "Power", "#ff9830", 1.0);
    power.unit = Some("W");
    power.axis = Some("right1");
    let mut temperature = reading("soc_celsius", "SoC temperature", "#f2495c", 1.0);
    temperature.unit = Some("°C");
    temperature.axis = Some("right2");
    temperature.fill = Some(0.0);
    let mut specification = specification(
        CORRELATION_ID,
        "Load, power and heat together",
        "%",
        vec![
            reading("cpu_percent", "CPU", "#5794f2", 1.0),
            reading("gpu_active_percent", "GPU", "#b877d9", 1.0),
            fans,
            power,
            temperature,
        ],
    );
    specification.help = "CPU and GPU activity, whole-machine power, SoC temperature and the \
        mean speed of the measured fans at the same recorded times. Power and temperature \
        have their own axes. Reading them together helps investigate a workload, and does not \
        show that one reading caused another.";

    specification
}

const FAN_MEAN_KEY: &str = "fan_mean_rpm";
const FAN_CHANNELS_MAX: usize = 10;

/// The mean speed of the fan channels measured at one reading, or none when no channel was.
fn fan_mean(channels: &Value) -> Result<Option<f32>, String> {
    if channels.is_null() {
        return Ok(None);
    }
    let channels = channels.as_array().ok_or("Invalid fan channel array")?;
    if channels.len() > FAN_CHANNELS_MAX {
        return Err("Fan history exceeds ten channels".into());
    }
    let mut sum = 0.0_f32;
    let mut count = 0_u8;
    for channel in channels.iter().take(FAN_CHANNELS_MAX) {
        let Some(speed) = number(channel)? else {
            continue;
        };
        if speed < 0.0 {
            return Err("Negative fan speed".into());
        }
        sum += speed;
        count += 1;
    }
    if count == 0 {
        return Ok(None);
    }

    Ok(Some(sum / f32::from(count)))
}

fn memory_specifications() -> Vec<Specification> {
    vec![
        memory(),
        memory_pressure(),
        memory_swap(),
        memory_swap_flow(),
        memory_paging(),
        memory_temperature(),
    ]
}

pub const STORAGE_TEMPERATURE_ID: &str = "live-storage-temperature";

fn storage_temperature() -> Specification {
    let mut specification = specification(
        STORAGE_TEMPERATURE_ID,
        "Storage area maximum",
        "°C",
        vec![reading("storage_area_celsius", "Storage", "#fade2a", 1.0)],
    );
    specification.maximum = Some(80.0);
    specification.help = "The hottest measured sensor in the storage area. This collector does \
        not record it yet, so no readings are shown.";
    specification
}

pub const MEMORY_TEMPERATURE_ID: &str = "live-memory-temperature";

fn memory_temperature() -> Specification {
    let mut specification = specification(
        MEMORY_TEMPERATURE_ID,
        "Memory area maximum",
        "°C",
        vec![reading("memory_area_celsius", "Memory", "#73bf69", 1.0)],
    );
    specification.maximum = Some(125.0);
    specification.help = "The hottest measured sensor in the memory area. This collector does \
        not record it yet, so no readings are shown.";
    specification
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
    let mut allocated = reading("swap_total_bytes", "Allocated", "#ccccdc", GIBIBYTE);
    allocated.dashed = true;
    allocated.fill = Some(0.0);

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
    let pageins = reading("memory_pageins_per_second", "Page-ins", "#5794f2", 1.0);
    let compressions = reading(
        "memory_compressions_per_second",
        "Compressed",
        "#ff9830",
        1.0,
    );
    let decompressions = reading(
        "memory_decompressions_per_second",
        "Decompressed",
        "#b877d9",
        1.0,
    );

    let mut specification = specification(
        "live-paging",
        "Page-ins and compression",
        "/s",
        vec![pageins, compressions, decompressions],
    );
    specification.help = "Page-ins count memory pages brought in from disk, including app \
        code, and are not limited to swap. Compression and decompression count pages per \
        second too. Pages are not converted to bytes. \
        Unavailable counter intervals remain gaps.";

    specification
}

/// Swap traffic, as the prototype's Paging chart draws it. The collector counts pages written
/// out only in pages, so the prototype's bytes per second of them stay unavailable.
fn memory_swap_flow() -> Specification {
    let mut swap_out = reading(
        "swap_out_bytes_per_second",
        "Write to swap",
        "#f2495c",
        1_000_000.0,
    );
    swap_out.negative = true;

    let mut specification = specification(
        "live-swap-flow",
        "Paging",
        "MB/s",
        vec![
            reading(
                "swap_in_bytes_per_second",
                "Read from swap",
                "#b877d9",
                1_000_000.0,
            ),
            reading(
                "pageout_bytes_per_second",
                "Paged out",
                "#73bf69",
                1_000_000.0,
            ),
            swap_out,
        ],
    );
    specification.help = "Bytes read from and written to swap on disk each second. \
        The collector does not record pages written out in bytes, so that line is unavailable.";

    specification
}

/// Network traffic by App, which the collector does not record per process.
pub const NETWORK_APPS_ID: &str = "live-network-apps";

fn network_apps() -> Specification {
    let mut specification = specification(
        NETWORK_APPS_ID,
        "Network by app",
        "MB/s",
        vec![reading(
            "network_apps_bytes_per_second",
            "All traffic",
            "#ccccdc",
            1_000_000.0,
        )],
    );
    specification.help = "Traffic each App's processes moved over external interfaces. This \
        collector does not record it per process, so no readings are shown.";
    specification
}

fn network() -> Vec<Specification> {
    let mut sent_packets = reading("network_tx_packets_per_second", "Upload", "#73bf69", 1.0);
    sent_packets.negative = true;
    vec![
        network_throughput(),
        specification(
            "live-packets",
            "Packets",
            "pps",
            vec![
                reading("network_rx_packets_per_second", "Download", "#5794f2", 1.0),
                sent_packets,
            ],
        ),
        specification(
            "live-network-errors",
            "Network errors",
            "/s",
            vec![
                reading(
                    "network_rx_errors_per_second",
                    "Network receive",
                    "#73bf69",
                    1.0,
                ),
                reading(
                    "network_tx_errors_per_second",
                    "Network send",
                    "#f2cc0c",
                    1.0,
                ),
            ],
        ),
        network_apps(),
    ]
}

fn network_throughput() -> Specification {
    let mut sent = reading("network_tx_bps", "Send", "#73bf69", 1_000_000.0);
    sent.negative = true;

    specification(
        "live-network",
        "Network",
        "MB/s",
        vec![
            reading("network_rx_bps", "Receive", "#5794f2", 1_000_000.0),
            sent,
        ],
    )
}

/// One process group's histories in the prototype's by-process charts, which keep their titles
/// and order when one group is chosen and draw only its series, in the palette's first color.
/// Each line's name is the suffix after the group's name, which `ProcessHistory::panels` adds.
fn process_specifications() -> Vec<Specification> {
    const COLOR: &str = "#73bf69";
    let mut written = reading("disk_written_bps", " written", COLOR, 1_000_000.0);
    written.negative = true;
    let mut disk = specification(
        "live-app-disk",
        "Disk by process",
        "MB/s",
        vec![
            reading("disk_read_bps", " read", COLOR, 1_000_000.0),
            written,
        ],
    );
    disk.help = APP_DISK_HELP;
    let result = vec![
        specification(
            "live-app-cpu",
            "CPU by process",
            "%",
            vec![reading("cpu_percent_one_core", "", COLOR, 1.0)],
        ),
        specification(
            "live-app-resident",
            "Resident memory by process",
            "GiB",
            vec![reading("resident_bytes", "", COLOR, GIBIBYTE)],
        ),
        specification(
            "live-app-footprint",
            "Footprint by process",
            "GiB",
            vec![reading("footprint_bytes", "", COLOR, GIBIBYTE)],
        ),
        disk,
        specification(
            "live-app-power",
            "Power by process",
            "W",
            vec![reading("energy_watts", "", COLOR, 1.0)],
        ),
        specification(
            "live-app-wakeups",
            "Idle wakeups by process",
            "/s",
            vec![reading("idle_wakeups_per_second", "", COLOR, 1.0)],
        ),
        specification(
            "live-app-pageins",
            "Page-ins by process",
            "pages/s",
            vec![reading("pageins_per_second", "", COLOR, 1.0)],
        ),
        specification(
            "live-app-threads",
            "Threads by process",
            "threads",
            vec![reading("threads", "", COLOR, 1.0)],
        ),
    ];

    assert_eq!(result.len(), 8);

    result
}

const APP_DISK_HELP: &str = "Process-attributed read and write rates for each selected \
    observation interval. Unavailable intervals remain gaps. These are separate from \
    device-driver throughput.";

/// The selected App's totals in the prototype's order and colors. The collector records no
/// network bytes per process, so the prototype's Network chart has no source and is left out.
/// GPU time comes from the app's own GPU sampler, which `SystemSamples::annotate_app` adds.
fn app_specifications() -> Vec<Specification> {
    let mut disk = specification(
        "live-app-disk",
        "Disk I/O",
        "MB/s",
        vec![reading("disk_io_bps", "Disk I/O", "#fade2a", 1_000_000.0)],
    );
    disk.help = APP_DISK_HELP;
    let mut gpu = specification(
        "live-app-gpu",
        "GPU time",
        "%",
        vec![reading("gpu_percent", "GPU time", "#b877d9", 1.0)],
    );
    gpu.help = "The App's GPU time as a share of each second, read from the GPU driver while \
        Flightdeck runs. Seconds before it started, or that it did not sample, remain gaps.";
    let mut network = specification(
        "live-app-network",
        "Network",
        "MB/s",
        vec![reading("network_bps", "Network", "#00ffff", 1_000_000.0)],
    );
    network.help = "The App's network bytes per second. This collector does not record network \
        bytes per process, so no readings are shown.";
    vec![
        specification(
            "live-app-cpu",
            "CPU",
            "%",
            vec![reading("cpu_percent_one_core", "CPU", "#5794f2", 1.0)],
        ),
        specification(
            "live-app-resident",
            "Resident memory",
            "GiB",
            vec![reading(
                "resident_bytes",
                "Resident memory",
                "#73bf69",
                GIBIBYTE,
            )],
        ),
        network,
        disk,
        gpu,
        specification(
            "live-app-power",
            "Process power",
            "W",
            vec![reading("energy_watts", "Process power", "#ff9830", 1.0)],
        ),
    ]
}

#[cfg(test)]
#[path = "live_projection_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "live_resource_projection_tests.rs"]
mod resource_tests;

#[cfg(test)]
#[path = "live_app_disk_history_tests.rs"]
mod app_disk_history_tests;
