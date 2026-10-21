use std::rc::Rc;

use num_traits::ToPrimitive;
use serde_json::Value;
use viewer::app_process_groups_history::{
    AppProcessGroupsHistory, ProcessGroup, ProcessGroupMetric,
};

use crate::data::{Line, Panel};
use crate::live_hover::Target;
use crate::live_source::{AppKey, Request, identity_validate};

pub fn prepare(
    payload: Box<AppProcessGroupsHistory>,
    request: &Request,
    selection: &Value,
) -> Result<super::ProcessHistory, String> {
    if request.route != "apps" {
        return Err("Process groups require the Apps route".into());
    }

    if request.executable.is_some() {
        return Err("All process groups require the all-groups selection".into());
    }

    let app = request.app.as_ref().ok_or("Selected App unavailable")?;
    let kind = crate::live_source::app_key_kind(&app.kind)?;
    if kind != payload.app_kind {
        return Err("Process groups belong to another App kind".into());
    }

    if crate::live_source::app_key_bytes(&app.bytes_hex)?.as_slice() != payload.app_bytes.as_ref() {
        return Err("Process groups belong to another App key".into());
    }

    identity_validate(&payload.selected.json(), &selection["selected"])?;
    if payload.context_seconds != 900 {
        return Err("Process groups require 900 seconds of context".into());
    }

    if payload.points.len() > 4096 {
        return Err("Process groups exceed 4096 observations".into());
    }

    if payload.groups.len() > 8 {
        return Err("Process groups exceed eight chart groups".into());
    }

    if payload.bytes() > viewer::app_process_groups_history::RESULT_BYTES_MAX {
        return Err("Process groups exceed the owned result byte bound".into());
    }

    Ok(super::ProcessHistory::All {
        app: app.clone(),
        payload,
    })
}

pub fn matches(app: &AppKey, request: &Request) -> bool {
    if request.executable.is_some() {
        return false;
    }

    request.app.as_ref() == Some(app)
}

struct PanelDefinition {
    suffix: &'static str,
    title: &'static str,
    unit: &'static str,
    metrics: &'static [ProcessGroupMetric],
    /// Whether the groups stack as areas, as they do for every metric whose groups add up to
    /// the App's total. Disk draws read above the axis and written below it, so it does not
    /// stack.
    stacked: bool,
    /// The names, after each group's, of the readings the collector does not record per
    /// process, which the chart lists with no values. Empty for a recorded metric.
    missing: &'static [&'static str],
}

const PANEL_DEFINITIONS: [PanelDefinition; 12] = [
    PanelDefinition {
        suffix: "cpu",
        title: "CPU by process",
        unit: "%",
        metrics: &[ProcessGroupMetric::Cpu],
        stacked: true,
        missing: &[],
    },
    PanelDefinition {
        suffix: "resident",
        title: "Resident memory by process",
        unit: "GiB",
        metrics: &[ProcessGroupMetric::Resident],
        stacked: true,
        missing: &[],
    },
    PanelDefinition {
        suffix: "footprint",
        title: "Footprint by process",
        unit: "GiB",
        metrics: &[ProcessGroupMetric::Footprint],
        stacked: true,
        missing: &[],
    },
    PanelDefinition {
        suffix: "network",
        title: "Network by process",
        unit: "MB/s",
        metrics: &[],
        stacked: false,
        missing: &[" received", " sent"],
    },
    PanelDefinition {
        suffix: "disk",
        title: "Disk by process",
        unit: "MB/s",
        metrics: &[
            ProcessGroupMetric::DiskRead,
            ProcessGroupMetric::DiskWritten,
        ],
        stacked: false,
        missing: &[],
    },
    PanelDefinition {
        suffix: "gpu",
        title: "GPU by process",
        unit: "%",
        metrics: &[],
        stacked: true,
        missing: &[""],
    },
    PanelDefinition {
        suffix: "power",
        title: "Power by process",
        unit: "W",
        metrics: &[ProcessGroupMetric::Power],
        stacked: true,
        missing: &[],
    },
    PanelDefinition {
        suffix: "wakeups",
        title: "Idle wakeups by process",
        unit: "/s",
        metrics: &[ProcessGroupMetric::Wakeups],
        stacked: true,
        missing: &[],
    },
    PanelDefinition {
        suffix: "pageins",
        title: "Page-ins by process",
        unit: "pages/s",
        metrics: &[ProcessGroupMetric::Pageins],
        stacked: true,
        missing: &[],
    },
    PanelDefinition {
        suffix: "threads",
        title: "Threads by process",
        unit: "threads",
        metrics: &[ProcessGroupMetric::Threads],
        stacked: true,
        missing: &[],
    },
    PanelDefinition {
        suffix: "files",
        title: "Open files by process",
        unit: "files",
        metrics: &[],
        stacked: true,
        missing: &[""],
    },
    PanelDefinition {
        suffix: "sockets",
        title: "Sockets by process",
        unit: "sockets",
        metrics: &[],
        stacked: true,
        missing: &[""],
    },
];

pub fn panels(
    payload: &AppProcessGroupsHistory,
    context_last_second: u64,
) -> Result<Vec<Rc<Panel>>, String> {
    let mut result = Vec::with_capacity(PANEL_DEFINITIONS.len());

    for definition in &PANEL_DEFINITIONS {
        result.push(Rc::new(panels_panel(
            payload,
            definition,
            context_last_second,
        )?));
    }

    Ok(result)
}

fn panels_panel(
    payload: &AppProcessGroupsHistory,
    definition: &PanelDefinition,
    context_last_second: u64,
) -> Result<Panel, String> {
    let first = context_last_second.saturating_sub(899);
    let span = u16::try_from(context_last_second - first + 1).map_err(|error| error.to_string())?;
    let mut positions = Vec::with_capacity(payload.points.len());
    let mut segments = Vec::with_capacity(payload.points.len());

    panels_panel_positions(payload, first, span, &mut positions, &mut segments)?;
    let lines = panels_panel_lines(payload, definition);

    // The prototype mirrors written bytes below the axis, so that chart has no zero floor.
    let mirrored = definition
        .metrics
        .iter()
        .any(|metric| matches!(metric, ProcessGroupMetric::DiskWritten));

    Ok(Panel {
        id: format!("live-process-groups-{}", definition.suffix),
        title: definition.title.into(),
        unit: definition.unit.into(),
        kind: "line".into(),
        lines,
        minimum: if mirrored || definition.suffix == "network" {
            None
        } else {
            Some(0.0)
        },
        maximum: None,
        stack: Some(definition.stacked),
        fill: Some(if definition.stacked { 0.45 } else { 0.1 }),
        caption: None,
        help: Some(
            "Seven groups with highest known mean CPU over captured context; Other includes \
            every remaining captured named group. Unknown names are excluded and reported. \
            Missing readings remain gaps. Groups use executable bytes across PID lifetimes."
                .into(),
        ),
        source: Some("Captured process intervals".into()),
        quantities: None,
        states: None,
        table_headings: None,
        table_rows: None,
        positions: Some(positions),
        segments: Some(segments),
        selected_index: payload
            .points
            .iter()
            .position(|point| point.observation == payload.selected),
        window: Some(crate::chart_axes::TimeWindow {
            first,
            span: u64::from(span),
        }),
        detail_first: None,
        sliding: false,
        chart_width: std::cell::Cell::new(None),
    })
}

fn panels_panel_positions(
    payload: &AppProcessGroupsHistory,
    first: u64,
    span: u16,
    positions: &mut Vec<f32>,
    segments: &mut Vec<u64>,
) -> Result<(), String> {
    let mut segment = 0_u64;

    for (index, point) in payload.points.iter().enumerate() {
        let wall = point
            .observation
            .wall_ns
            .ok_or("Process group clock unavailable")?;
        let seconds =
            u16::try_from(wall / 1_000_000_000 - first).map_err(|error| error.to_string())?;
        let fraction = u32::try_from(wall % 1_000_000_000)
            .map_err(|error| error.to_string())?
            .to_f32()
            .ok_or("Fraction exceeds f32")?;

        positions.push((f32::from(seconds) + fraction / 1_000_000_000.0) / f32::from(span));

        if let Some(previous) = index
            .checked_sub(1)
            .and_then(|index| payload.points.get(index))
        {
            if !panels_panel_positions_consecutive(ObservationPair {
                previous: &previous.observation,
                current: &point.observation,
            }) {
                segment += 1;
            }
        }

        segments.push(segment);
    }

    assert_eq!(positions.len(), payload.points.len());
    assert_eq!(segments.len(), payload.points.len());

    Ok(())
}

/// The groups in the order the prototype lists them, by name with Other last, which also
/// gives each its color.
fn group_order(payload: &AppProcessGroupsHistory) -> Vec<usize> {
    let mut order = Vec::with_capacity(payload.groups.len());
    order.extend(0..payload.groups.len());
    order.sort_by_cached_key(|index| match &payload.groups[*index] {
        ProcessGroup::Named(bytes) => (false, panels_panel_line_display(bytes).to_lowercase()),
        ProcessGroup::Other => (true, String::new()),
    });
    order
}

fn group_name(identity: &ProcessGroup) -> String {
    match identity {
        ProcessGroup::Named(bytes) => panels_panel_line_display(bytes),
        ProcessGroup::Other => "Other".into(),
    }
}

/// A line for each group under each of the panel's metrics, then each group's missing-reading
/// line.
fn panels_panel_lines(
    payload: &AppProcessGroupsHistory,
    definition: &PanelDefinition,
) -> Vec<Line> {
    let order = group_order(payload);
    let mut lines = Vec::with_capacity(order.len() * definition.metrics.len().max(2));

    for metric in definition.metrics {
        for (position, group) in order.iter().enumerate() {
            lines.push(panels_panel_line(payload, (position, *group), *metric));
        }
    }
    for suffix in definition.missing {
        for (position, group) in order.iter().enumerate() {
            lines.push(panels_panel_line_missing(
                payload,
                (position, *group),
                suffix,
            ));
        }
    }

    lines
}

/// A line of the group at `group.1` colored by its place `group.0` in the listing.
fn panels_panel_line(
    payload: &AppProcessGroupsHistory,
    group: (usize, usize),
    metric: ProcessGroupMetric,
) -> Line {
    let (position, group) = group;
    let identity = &payload.groups[group];
    let suffix = match metric {
        ProcessGroupMetric::DiskRead => " read",
        ProcessGroupMetric::DiskWritten => " written",
        _ => "",
    };

    Line {
        name: format!("{}{suffix}", group_name(identity)),
        color: panels_panel_line_color(position, identity).into(),
        values: payload
            .points
            .iter()
            .map(|point| {
                let value =
                    metric.value(&point.readings[group], point.elapsed_ns)? / divisor(metric);
                value.to_f32()
            })
            .collect(),
        stack: None,
        negative: Some(matches!(metric, ProcessGroupMetric::DiskWritten)),
        dashed: Some(false),
        axis: None,
        follows: None,
        unit: None,
        fill: None,
    }
}

/// A line of a reading the collector does not record per process, so every second is a gap.
fn panels_panel_line_missing(
    payload: &AppProcessGroupsHistory,
    group: (usize, usize),
    suffix: &str,
) -> Line {
    let (position, group) = group;
    let identity = &payload.groups[group];

    Line {
        name: format!("{}{suffix}", group_name(identity)),
        color: panels_panel_line_color(position, identity).into(),
        values: vec![None; payload.points.len()],
        stack: None,
        negative: Some(suffix == " sent"),
        dashed: Some(false),
        axis: None,
        follows: None,
        unit: None,
        fill: None,
    }
}

fn divisor(metric: ProcessGroupMetric) -> f64 {
    match metric {
        ProcessGroupMetric::Resident | ProcessGroupMetric::Footprint => 1_073_741_824.0,
        ProcessGroupMetric::DiskRead
        | ProcessGroupMetric::DiskWritten
        | ProcessGroupMetric::DiskTotal => 1_000_000.0,
        _ => 1.0,
    }
}

pub fn index(payload: &AppProcessGroupsHistory, target: &Target) -> Option<usize> {
    target.identity.as_ref()?;

    let signature = || -> Result<_, String> {
        let (run, boot, publication) = crate::live_source::identity_signature(
            target
                .identity
                .as_ref()
                .ok_or("Hover identity unavailable")?,
        )?;
        let run = collector::recording::parse_identity(&run)
            .map_err(|error| error.to_string())?
            .0;
        let boot = collector::recording::parse_identity(&boot)
            .map_err(|error| error.to_string())?
            .0;
        Ok((run, boot, publication))
    };
    let (run, boot, publication) = match signature() {
        Ok(identity) => identity,
        Err(error) => {
            eprintln!("Process group hover identity: {error}");
            return None;
        }
    };

    payload.points.iter().rposition(|point| {
        if point.observation.stamp.run != run {
            return false;
        }

        if point.observation.stamp.boot != boot {
            return false;
        }

        point.observation.stamp.publication == publication
    })
}

pub fn reading(
    payload: &AppProcessGroupsHistory,
    panel: &str,
    line: usize,
    index: Option<usize>,
) -> String {
    let Some((position, metric)) = reading_line_metric(payload, panel, line) else {
        return "Unavailable".into();
    };
    let group = group_order(payload)[position];

    let Some(point) = index.and_then(|index| payload.points.get(index)) else {
        return "Unavailable".into();
    };

    let reading = &point.readings[group];
    if !reading.present {
        return "Unavailable".into();
    }

    let measure = &reading.measures[metric.index()];
    if measure.overflowed {
        return "Unavailable · counter sum overflow".into();
    }

    if measure.measured_members == 0 {
        return format!("Unavailable · {} unavailable", measure.unavailable_members);
    }

    let coverage = if measure.unavailable_members > 0 {
        format!(
            " · known subtotal · {} measured · {} unavailable",
            measure.measured_members, measure.unavailable_members
        )
    } else {
        String::new()
    };

    if matches!(metric, ProcessGroupMetric::Threads) {
        return format!("{} threads{coverage}", measure.known);
    }

    let human = reading_human(reading, metric, point.elapsed_ns);
    format!("{human}{coverage}")
}

fn reading_human(
    reading: &viewer::app_process_groups_history::ProcessGroupReading,
    metric: ProcessGroupMetric,
    elapsed_ns: Option<u64>,
) -> String {
    let unit = match metric {
        ProcessGroupMetric::Cpu => "%",
        ProcessGroupMetric::Power => "W",
        ProcessGroupMetric::Wakeups => "/s",
        ProcessGroupMetric::Pageins => "pages/s",
        ProcessGroupMetric::Threads => "threads",
        ProcessGroupMetric::Resident | ProcessGroupMetric::Footprint => "GiB",
        ProcessGroupMetric::DiskRead
        | ProcessGroupMetric::DiskWritten
        | ProcessGroupMetric::DiskTotal => "MB/s",
    };

    metric
        .value(reading, elapsed_ns)
        .and_then(|value| (value / divisor(metric)).to_f32())
        .map_or_else(
            || "Unavailable".into(),
            |value| crate::live_projection::quantity_text(value, unit),
        )
}

fn reading_line_metric(
    payload: &AppProcessGroupsHistory,
    panel: &str,
    line: usize,
) -> Option<(usize, ProcessGroupMetric)> {
    let groups = payload.groups.len();
    if groups == 0 {
        return None;
    }

    let metric = match panel.strip_prefix("live-process-groups-")? {
        "cpu" => ProcessGroupMetric::Cpu,
        "resident" => ProcessGroupMetric::Resident,
        "footprint" => ProcessGroupMetric::Footprint,
        "power" => ProcessGroupMetric::Power,
        "wakeups" => ProcessGroupMetric::Wakeups,
        "pageins" => ProcessGroupMetric::Pageins,
        "threads" => ProcessGroupMetric::Threads,
        "disk" if line < groups => ProcessGroupMetric::DiskRead,
        "disk" => ProcessGroupMetric::DiskWritten,
        _ => return None,
    };

    Some((line % groups, metric))
}

#[derive(Clone, Copy)]
struct ObservationPair<'observation> {
    previous: &'observation viewer::live_app_response::Observation,
    current: &'observation viewer::live_app_response::Observation,
}

fn panels_panel_positions_consecutive(pair: ObservationPair<'_>) -> bool {
    if pair.previous.recording != pair.current.recording {
        return false;
    }

    if pair.previous.stamp.boot != pair.current.stamp.boot {
        return false;
    }

    if pair.previous.stamp.run != pair.current.stamp.run {
        return false;
    }

    if pair.previous.stamp.publication.checked_add(1) != Some(pair.current.stamp.publication) {
        return false;
    }

    if pair.previous.run_tick.checked_add(1) != Some(pair.current.run_tick) {
        return false;
    }

    let (Some(before), Some(after)) = (pair.previous.wall_ns, pair.current.wall_ns) else {
        return false;
    };

    if before >= after {
        return false;
    }

    if after / 1_000_000_000 > before / 1_000_000_000 + 1 {
        return false;
    }

    let (Some(before), Some(after)) = (
        pair.previous.stamp.continuous_ns,
        pair.current.stamp.continuous_ns,
    ) else {
        return false;
    };

    if before >= after {
        return false;
    }

    let (Some(before), Some(after)) = (pair.previous.stamp.awake_ns, pair.current.stamp.awake_ns)
    else {
        return false;
    };

    before <= after
}

fn panels_panel_line_display(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 4);

    for byte in bytes {
        if (b' '..=b'~').contains(byte) {
            if *byte != b'\\' {
                output.push(char::from(*byte));
                continue;
            }
        }

        output.push_str("\\x");
        output.push(char::from(DIGITS[usize::from(byte >> 4)]));
        output.push(char::from(DIGITS[usize::from(byte & 15)]));
    }

    output
}

/// The prototype's classic palette, in the groups' order, and grey for Other.
fn panels_panel_line_color(group: usize, identity: &ProcessGroup) -> &'static str {
    const CLASSIC: [&str; 10] = [
        "#73bf69", "#f2cc0c", "#5794f2", "#ff9830", "#f2495c", "#b877d9", "#37872d", "#e0b400",
        "#1f60c4", "#c15c17",
    ];
    if let ProcessGroup::Other = identity {
        return "#898995";
    }

    CLASSIC[group % CLASSIC.len()]
}
