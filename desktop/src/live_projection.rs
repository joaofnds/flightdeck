use std::borrow::Cow;
use std::collections::BTreeMap;
use std::rc::Rc;

use num_traits::ToPrimitive;
use serde_json::Value;

use crate::data::{Line, Panel};
use crate::live_inventory::PerformanceLevel;

#[path = "live_held_projection.rs"]
pub(crate) mod held_projection;

pub use held_projection::{
    OverviewAppPoint, OverviewApps, overview_app_points, overview_panels, overview_point_second,
    overview_seconds,
};
#[path = "live_resource_projection.rs"]
mod resource_projection;

pub const OBSERVATIONS_MAX: usize = 4096;
/// The seconds a selection with an older tail holds: the loaded hour and the lead-in before it.
const HELD_SECONDS: u64 = viewer::LOADED_SECONDS + viewer::LEAD_IN_SECONDS;
/// The seconds of full-detail readings a selection holds, which end at the last second of its
/// context: the 15 minutes of the shortest range and their lead-in.
const DETAIL_SECONDS: u64 = viewer::CHARTED_SECONDS;
const _: () = assert!(DETAIL_SECONDS < HELD_SECONDS);
pub const OVERVIEW_PRIMARY_PANELS: usize = 6;
/// The Overview chart that sets load, power and heat side by side.
pub const CORRELATION_ID: &str = "live-correlation";
pub const OVERVIEW_PANELS_MAX: usize = 9;
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
    let recent = selection["trends"]
        .as_array()?
        .iter()
        .take(OBSERVATIONS_MAX)
        .rfind(|point| wall_second_of(point) == Some(second));
    if recent.is_some() {
        return recent.map(observation_identity);
    }

    older_reading(selection, second).map(observation_identity)
}

/// The older reading that stands for `second`, since the older tail keeps one reading in every
/// stretch of seconds.
fn older_reading(selection: &Value, second: u64) -> Option<&Value> {
    selection["earlier_trends"]
        .as_array()?
        .get(older_index(selection, second)?)
}

/// The index among the older readings of the one that stands for `second`.
#[must_use]
pub fn older_index(selection: &Value, second: u64) -> Option<usize> {
    let stride = selection["earlier_stride_seconds"].as_u64()?;
    selection["earlier_trends"]
        .as_array()?
        .iter()
        .take(OBSERVATIONS_MAX)
        .rposition(|point| {
            wall_second_of(point).is_some_and(|first| (first..first + stride).contains(&second))
        })
}

/// How many older readings sit before the recent ones in every chart, so an index among the
/// recent readings plus this count is the index in a chart.
#[must_use]
pub fn older_count(selection: &Value) -> usize {
    selection["earlier_trends"]
        .as_array()
        .map_or(0, |older| older.len().min(OBSERVATIONS_MAX))
}

/// The second `second` is inspected as: the second its observation was made, which differs
/// from `second` only inside an older stretch that one reading stands for.
#[must_use]
pub fn inspection_second(selection: &Value, second: u64) -> u64 {
    inspection_identity(selection, second)
        .and_then(|identity| wall_second_of(&identity))
        .unwrap_or(second)
}

/// The second a step of `delta` from `second` shows: the first second in that direction that
/// shows another reading than `second` does, or a gap, since every second of an older stretch
/// shows the one reading that stands for it.
#[must_use]
pub fn stepped_second(selection: &Value, second: u64, delta: i64) -> u64 {
    let shown = inspection_second(selection, second);
    let mut next = second;
    // A stretch spans the viewer's stride, so a step that far always leaves it.
    for _ in 0..viewer::EARLIER_STRIDE_SECONDS {
        next = next.saturating_add_signed(delta);
        if inspection_second(selection, next) != shown {
            return next;
        }
    }

    next
}

/// Whether the selection holds `second`, as a reading or a gap, so the page shows that second
/// without a read: the seconds its charts hold, on both sides of the selected second.
#[must_use]
pub fn holds_second(selection: &Value, second: u64) -> bool {
    let Some(last) = context_last_second(selection) else {
        return false;
    };

    let first = first_held_second(last, selection["earlier_trends"].is_array());
    (first..=last).contains(&second)
}

/// The last second the selection's charts hold, which runs past the selected second where the
/// recording holds later seconds.
#[must_use]
pub fn context_last_second(selection: &Value) -> Option<u64> {
    selection["context_last_second"].as_u64()
}

/// The first second the charts hold for a context that ends at `last_second`: the first of its
/// full-detail seconds, or of the lead-in before the loaded hour when the selection carries an
/// older tail.
fn first_held_second(last_second: u64, longer: bool) -> u64 {
    if longer {
        return last_second.saturating_sub(HELD_SECONDS - 1);
    }

    last_second.saturating_sub(DETAIL_SECONDS - 1)
}

fn wall_second_of(point: &Value) -> Option<u64> {
    point["wall_ns"]
        .as_u64()
        .map(|wall| wall / NANOSECONDS_PER_SECOND)
}

/// The fields of a trend point that name its observation, which the reader accepts as the
/// selection. The rest of the point holds the observation's readings and is far larger than
/// the reader's bound on an identity.
pub(crate) fn observation_identity(point: &Value) -> Value {
    let mut identity = serde_json::Map::with_capacity(OBSERVATION_FIELDS.len());
    for field in OBSERVATION_FIELDS {
        if let Some(value) = point.get(field) {
            identity.insert(field.to_owned(), value.clone());
        }
    }

    Value::Object(identity)
}

const OBSERVATION_FIELDS: [&str; 7] = [
    "run",
    "boot",
    "publication",
    "run_tick",
    "wall_ns",
    "window_losses",
    "reader_losses",
];

/// The older tail followed by the recent readings, which every index of a chart counts through.
fn joined_points<'a>(earlier: Option<&'a [Value]>, recent: &'a [Value]) -> Cow<'a, [Value]> {
    match earlier {
        Some(older) if !older.is_empty() => Cow::Owned([older, recent].concat()),
        _ => Cow::Borrowed(recent),
    }
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
    let Some(recent) = source[key].as_array() else {
        return Ok(Vec::new());
    };
    if recent.is_empty() {
        // A reading with no trend points still draws its page's charts, with nothing in them,
        // so the charts the page drew before its first reading stay in place.
        return empty_panels(route);
    }
    let earlier = if route == "apps" {
        None
    } else {
        selection["earlier_trends"].as_array()
    };
    let joined = joined_points(earlier.map(Vec::as_slice), recent);
    let points: &[Value] = &joined;
    if points.len() > OBSERVATIONS_MAX {
        return Err("Live chart exceeds 4096 observations".to_owned());
    }
    let domain = Domain::read(selection, earlier.is_some())?;
    let coordinates = coordinates(points, &domain, route == "apps", &source["selected"])?;
    let process = source["scope"] == "selected_named_process_group";
    let limit = panels_limit(route, process);
    let mut result = Vec::with_capacity(limit);
    let specifications = if process {
        process_specifications()
    } else {
        specifications(route, points)?
    };
    for specification in specifications {
        let panel = panels_panel(&specification, &domain, points, &coordinates)?;

        result.push(Rc::new(panel));
    }
    assert!(result.len() <= limit);
    Ok(result)
}

/// The charts `route` shows, each with its lines and no readings, which the page lays out
/// before its first reading arrives so that reading moves nothing. The Apps page charts the
/// selected App, which only its readings name, so it has none.
pub fn empty_panels(route: &str) -> Result<Vec<Rc<Panel>>, String> {
    if route == "apps" {
        return Ok(Vec::new());
    }

    let coordinates = Coordinates {
        window: crate::chart_axes::TimeWindow { first: 0, span: 1 },
        detail_first: 0,
        positions: Vec::new(),
        segments: Vec::new(),
        selected_index: None,
    };
    // One second, which holds no reading, so a state chart draws as its timeline.
    let domain = Domain {
        first: 0,
        end: 1,
        span: 1,
        recent_first: 0,
        stride: 1,
    };
    let points: &[Value] = &[];
    let specifications = specifications(route, points)?;
    let mut result = Vec::with_capacity(specifications.len());
    for specification in &specifications {
        let mut panel = panels_panel(specification, &domain, points, &coordinates)?;
        // No reading names the seconds a chart spans yet.
        panel.window = None;
        panel.detail_first = None;

        result.push(Rc::new(panel));
    }
    assert!(result.len() <= panels_limit(route, false));
    Ok(result)
}

/// The most panels a route shows, or the most a process scope shows.
fn panels_limit(route: &str, process: bool) -> usize {
    if process {
        return 8;
    }
    if route == "overview" {
        OVERVIEW_PANELS_MAX
    } else {
        ROUTE_PANELS_MAX
    }
}

/// The panel of `specification` with the cells its timeline kind adds.
fn panels_panel<O: Observations + ?Sized>(
    specification: &Specification,
    domain: &Domain,
    points: &O,
    coordinates: &Coordinates,
) -> Result<Panel, String> {
    let mut panel = panel(specification, points, coordinates)?;
    if specification.id == "live-memory-pressure" {
        pressure_cells(&mut panel, domain, points, coordinates)?;
    }
    if specification.id == THERMAL_STATE_ID {
        thermal_cells(&mut panel, domain, points, coordinates)?;
    }
    if specification.id == "live-battery-charging" {
        resource_projection::charging_cells(&mut panel, domain, points, coordinates)?;
    }

    Ok(panel)
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

struct Domain {
    first: u64,
    end: u64,
    span: u16,
    /// The first second of the full-detail readings. Seconds before it hold older readings that
    /// each stand for `stride` seconds.
    recent_first: u64,
    stride: u64,
}

struct Coordinates {
    window: crate::chart_axes::TimeWindow,
    detail_first: u64,
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
    name: Cow<'static, str>,
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
    follows: Option<&'static str>,
}

impl Domain {
    /// The seconds the charts hold: the full-detail ones, or the loaded hour and its lead-in
    /// when the selection carries an older tail.
    fn read(selection: &Value, longer: bool) -> Result<Self, String> {
        let last = integer(&selection["context_last_second"])?;
        let older_stride = if longer {
            Some(integer(&selection["earlier_stride_seconds"])?)
        } else {
            None
        };

        Self::ending_at(last, older_stride)
    }

    /// The seconds the charts of a context that ends at `last` hold: the full-detail ones, or
    /// the loaded hour and its lead-in where older readings stand `older_stride` seconds apart.
    fn ending_at(last: u64, older_stride: Option<u64>) -> Result<Self, String> {
        let longer = older_stride.is_some();
        let recent_first = first_held_second(last, false);
        let first = first_held_second(last, longer);
        let end = last.checked_add(1).ok_or("Live chart time overflow")?;
        let span = u16::try_from(end - first).map_err(|error| error.to_string())?;
        assert!(span > 0);
        assert!(u64::from(span) <= HELD_SECONDS);
        let stride = older_stride.unwrap_or(1);
        assert!(stride > 0);
        Ok(Self {
            first,
            end,
            span,
            recent_first,
            stride,
        })
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
        detail_first: domain.recent_first,
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

/// The observations a chart draws, oldest first: the trend points of a selection, or the
/// seconds the store holds.
trait Observations {
    fn count(&self) -> usize;

    /// The wall clock of observation `index`, in nanoseconds.
    fn wall_ns(&self, index: usize) -> Result<u64, String>;

    /// What `reading` reads at observation `index`, before its divisor.
    fn value(&self, reading: &Reading, index: usize) -> Result<Option<f32>, String>;
}

impl Observations for [Value] {
    fn count(&self) -> usize {
        self.len()
    }

    fn wall_ns(&self, index: usize) -> Result<u64, String> {
        integer(&self[index]["wall_ns"])
    }

    fn value(&self, reading: &Reading, index: usize) -> Result<Option<f32>, String> {
        panel_reading_value(reading, &self[index])
    }
}

fn panel<O: Observations + ?Sized>(
    specification: &Specification,
    points: &O,
    coordinates: &Coordinates,
) -> Result<Panel, String> {
    let mut lines = Vec::with_capacity(specification.lines.len());
    for reading in &specification.lines {
        let mut values = Vec::with_capacity(points.count());
        for index in 0..points.count() {
            let value = points.value(reading, index)?;
            values.push(value.map(|value| value / reading.divisor));
        }
        lines.push(Line {
            name: reading.name.clone().into_owned(),
            color: reading.color.to_owned(),
            values,
            stack: reading.stack,
            negative: Some(reading.negative),
            dashed: Some(reading.dashed),
            axis: reading.axis.map(str::to_owned),
            follows: reading.follows.map(str::to_owned),
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
        detail_first: Some(coordinates.detail_first),
        reading_seconds: crate::charts::TICK_SECONDS,
        sliding: false,
        chart_width: std::cell::Cell::new(None),
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
        name: Cow::Borrowed(name),
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
        follows: None,
    }
}

/// CPU activity across all cores and within each performance level, the slower level first
/// as the prototype lists it. The level's name comes from `level_name`, since the Overview
/// names a level alone and the CPU page names its cores.
fn cpu(
    levels: Option<&[PerformanceLevel; 2]>,
    level_name: fn(&PerformanceLevel) -> &str,
) -> Specification {
    let mut lines = vec![reading("cpu_percent", "All cores", "#5794f2", 1.0)];
    for (index, color) in [(1, "#73bf69"), (0, "#b877d9")] {
        let mut line = reading("cpu_level_percent", "", color, 1.0);
        line.name = level_line_name(levels, index, level_name);
        line.channel = Some(index);
        lines.push(line);
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
/// The summary key of the CPU core sensor the collector records.
pub const CPU_TEMPERATURE_KEY: &str = "cpu_celsius";

/// The one CPU core sensor the collector reads each tick.
fn cpu_temperature() -> Specification {
    let mut specification = specification(
        CPU_TEMPERATURE_ID,
        "CPU core sensor",
        "°C",
        vec![reading(CPU_TEMPERATURE_KEY, "CPU", "#5794f2", 1.0)],
    );
    specification.maximum = Some(125.0);
    specification.help = "The CPU core sensor that read hottest when each collector run \
        started, read each tick. A second without a reading leaves a gap.";
    specification
}

/// The mean clock of each performance level while its cores were active, from the time they
/// spent in each clock state.
fn frequency(levels: Option<&[PerformanceLevel; 2]>) -> Specification {
    let mut lines = Vec::with_capacity(2);
    for (index, color) in [(1, "#73bf69"), (0, "#b877d9")] {
        let mut line = reading("cpu_level_ghz", "", color, 1.0);
        line.name = level_line_name(levels, index, cpu_level_cluster_cores);
        line.channel = Some(index);
        lines.push(line);
    }
    let mut specification = specification(FREQUENCY_ID, "CPU active frequency", "GHz", lines);
    specification.help = "The mean clock of each core level while its cores were active, \
        weighting each clock state macOS reports by the time the cores spent in it. The clock \
        of each state comes from the power manager's own tables. macOS changes clock states as \
        the work changes, so this is a measure of the interval and not a fixed maximum.";
    specification
}

/// The load averages the collector read each second, with the logical CPU count dashed, since
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
        15 minutes. Above the dashed core count, work waits for a core. The collector reads it \
        each tick, so a second it did not collect has no reading.";
    specification
}

/// The file entries open across the system, with their share of the most macOS allows on
/// its own axis.
fn open_files() -> Specification {
    let mut share = reading("open_files_percent", "Share of limit", "#ccccdc", 1.0);
    share.unit = Some("%");
    share.axis = Some("right1");
    share.follows = Some("Open");
    share.dashed = true;
    share.fill = Some(0.0);
    let mut specification = specification(
        OPEN_FILES_ID,
        "Open files",
        "files",
        vec![reading("open_files", "Open", "#00ffff", 1.0), share],
    );
    specification.help = "File entries open across the system and their share of the \
        kernel's limit. The collector reads them each tick, so a second it did not collect \
        has no reading.";
    specification
}

/// The name of performance level `index` as `level_name` gives it, or its index where the
/// readings name no levels, so a chart keeps a line for each level before its first reading.
fn level_line_name(
    levels: Option<&[PerformanceLevel; 2]>,
    index: usize,
    level_name: fn(&PerformanceLevel) -> &str,
) -> Cow<'static, str> {
    match levels {
        Some(levels) => Cow::Owned(level_name(&levels[index]).to_owned()),
        None => Cow::Owned(format!("Core level {index}")),
    }
}

fn cpu_level_cores(level: &PerformanceLevel) -> &str {
    level.cores.as_str()
}

fn cpu_level_cluster_cores(level: &PerformanceLevel) -> &str {
    level.cluster_cores.as_str()
}

fn cpu_level_name(level: &PerformanceLevel) -> &str {
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

/// The unified memory the GPU driver reported in use and allocated.
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
        shares RAM with the CPU, so this memory also counts in the Mac's memory in use.";
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
/// The summary key of the GPU sensor the collector records.
pub const GPU_TEMPERATURE_KEY: &str = "gpu_celsius";

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

/// The one GPU sensor the collector reads each tick.
fn gpu_temperature() -> Specification {
    let mut specification = specification(
        GPU_TEMPERATURE_ID,
        "GPU sensor",
        "°C",
        vec![reading(GPU_TEMPERATURE_KEY, "GPU", "#b877d9", 1.0)],
    );
    specification.maximum = Some(125.0);
    specification.help = "The GPU sensor that read hottest when each collector run started, \
        read each tick. A second without a reading leaves a gap.";
    specification
}

fn specifications(route: &str, points: &[Value]) -> Result<Vec<Specification>, String> {
    let levels = points
        .last()
        .and_then(crate::live_inventory::performance_levels);
    Ok(match route {
        "overview" => overview(levels.as_ref()),
        "cpu" => vec![
            cpu(levels.as_ref(), cpu_level_cores),
            frequency(levels.as_ref()),
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
        soc_temperature(),
        fan_specification(points)?,
        thermal_state(),
        area_maxima(),
    ])
}

/// The one system-on-chip sensor the collector reads each tick.
fn soc_temperature() -> Specification {
    let mut specification = specification(
        "live-soc-temperature",
        "SoC temperature",
        "°C",
        vec![reading("soc_celsius", "SoC sensor", "#f2495c", 1.0)],
    );
    specification.help = "The SoC sensor that read hottest when each collector run started, \
        read each tick. A second without a reading leaves a gap.";
    specification
}

pub const THERMAL_STATE_ID: &str = "live-thermal";
pub const FANS_ID: &str = "live-fans";
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

    let mut specification = specification(FANS_ID, "Fan speed", "rpm", lines);
    specification.help = "SMC channels retain their numeric indices, without physical-side \
        attribution. Zero means measured stopped; unavailable channels remain gaps. \
        Channel count can change between observations.";

    Ok(specification)
}

fn pressure_cells<O: Observations + ?Sized>(
    panel: &mut Panel,
    domain: &Domain,
    points: &O,
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

fn thermal_cells<O: Observations + ?Sized>(
    panel: &mut Panel,
    domain: &Domain,
    points: &O,
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

fn timeline_cells<O: Observations + ?Sized>(
    panel: &mut Panel,
    domain: &Domain,
    points: &O,
    coordinates: &Coordinates,
    state: impl Fn(Option<f32>) -> Option<f32>,
) -> Result<(), String> {
    let span = usize::from(domain.span);
    let mut cells = vec![None; span];
    let mut selected = None;

    for index in 0..points.count() {
        let second = points.wall_ns(index)? / NANOSECONDS_PER_SECOND;
        let cell = usize::try_from(second - domain.first).map_err(|error| error.to_string())?;
        assert!(cell < span);
        if Some(cell) != selected {
            let value = state(panel.lines[0].values[index]);
            if second < domain.recent_first {
                let stretch = domain.stride.min(domain.recent_first - second);
                let stretch = usize::try_from(stretch).map_err(|error| error.to_string())?;
                cells[cell..cell + stretch].fill(value);
            } else {
                cells[cell] = value;
            }
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

fn overview(levels: Option<&[PerformanceLevel; 2]>) -> Vec<Specification> {
    let mut network = network_throughput();
    network.title = "Network throughput";
    let mut result = vec![
        cpu(levels, cpu_level_name),
        gpu_activity(),
        memory(),
        system_power("live-power"),
        resource_projection::throughput(),
        network,
        memory_pressure(),
        correlation(),
        thermal_state(),
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
    gpu.help = "The App's GPU time as a share of each interval, from the GPU driver's own \
        count. An interval without a GPU reading, or an App with no GPU client, remains a gap.";
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
