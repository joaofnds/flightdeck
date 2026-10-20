//! What the Self page shows of the collector's own reports, worded and formatted as the
//! approved prototype shows its saved diagnostic run.

use std::rc::Rc;

use num_traits::ToPrimitive;

use crate::chart_axes::TimeWindow;
use crate::charts::{BLUE, ORANGE, PURPLE, RED, TEXT, YELLOW};
use crate::data::{Line, Panel};
use crate::live_projection::fixed_text;
use crate::live_views::summary::Card;
use crate::self_reports::{MinuteReport, SelfReports, TickReport};

/// The recording's CPU budget, collector and persister together, in percent of one core.
pub const BUDGET_PERCENT: f32 = 5.0;
pub const CHARTS_COUNT: usize = 6;

#[derive(Clone, Copy)]
enum Group {
    Ticks,
    Minutes,
}

#[derive(Clone, Copy)]
enum Field {
    MinuteCollector,
    MinutePersister,
    MinuteCombined,
    Collector,
    LateAverage,
    LateMaximum,
    Footprint,
    FootprintMaximum,
    Observed,
    Refused,
    Gone,
    Slips,
    Drops,
}

struct Definition {
    id: &'static str,
    title: &'static str,
    unit: &'static str,
    maximum: Option<f32>,
    digits: usize,
    group: Group,
    fields: &'static [(Field, &'static str, u32)],
    /// Whether the chart draws the recording's CPU budget.
    budget: bool,
    caption: &'static str,
}

const DEFINITIONS: [Definition; CHARTS_COUNT] = [
    Definition {
        id: "self-budget",
        title: "Recording CPU budget",
        unit: "%",
        maximum: Some(5.5),
        digits: 2,
        group: Group::Minutes,
        fields: &[
            (Field::MinuteCollector, "Collector", BLUE),
            (Field::MinutePersister, "Persister", PURPLE),
            (Field::MinuteCombined, "Combined", ORANGE),
        ],
        budget: true,
        caption: "Completed one-minute intervals · 100% is one CPU core. The 5% budget covers \
                  collector + persister, excluding the viewer.",
    },
    Definition {
        id: "self-collector",
        title: "Collector CPU",
        unit: "%",
        maximum: None,
        digits: 2,
        group: Group::Ticks,
        fields: &[(Field::Collector, "Collector", BLUE)],
        budget: false,
        caption: "Ten-second report averages · 100% is one CPU core. These are separate \
                  intervals from the minute budget.",
    },
    Definition {
        id: "self-late",
        title: "Tick scheduling lateness",
        unit: "ms",
        maximum: None,
        digits: 1,
        group: Group::Ticks,
        fields: &[
            (Field::LateAverage, "Average", BLUE),
            (Field::LateMaximum, "Maximum", YELLOW),
        ],
        budget: false,
        caption: "Delay after the scheduled tick deadline; not collection duration or a \
                  percentile.",
    },
    Definition {
        id: "self-footprint",
        title: "Persister memory footprint",
        unit: "MiB",
        maximum: None,
        digits: 2,
        group: Group::Minutes,
        fields: &[
            (Field::Footprint, "Current footprint", PURPLE),
            (Field::FootprintMaximum, "Lifetime maximum", TEXT),
        ],
        budget: false,
        caption: "Minute observations of process footprint, not RSS or DuckDB buffer usage. \
                  Native hard working-memory limit: 2 GiB.",
    },
    Definition {
        id: "self-processes",
        title: "Process observation coverage",
        unit: "processes",
        maximum: None,
        digits: 0,
        group: Group::Ticks,
        fields: &[
            (Field::Observed, "Observed", BLUE),
            (Field::Refused, "Usage refused", ORANGE),
            (Field::Gone, "Gone before usage read", TEXT),
        ],
        budget: false,
        caption: "Counts at the last tick of each ten-second report. Refused and gone readings \
                  remain explicit; observed does not mean fully readable.",
    },
    Definition {
        id: "self-loss",
        title: "Collection gaps and event drops",
        unit: "count",
        maximum: Some(1.0),
        digits: 0,
        group: Group::Ticks,
        fields: &[
            (Field::Slips, "Tick slips", ORANGE),
            (Field::Drops, "Event-list drops", RED),
        ],
        budget: false,
        caption: "Counts per ten-second report. A measured zero here does not rule out missed \
                  processes or losses elsewhere.",
    },
];

pub const HELP: &str = "Reports the collector this app started writes of its own cost: every \
    ten seconds, every minute with the persister's, and for every window it seals. Ten-second \
    reports, one-minute CPU and memory observations and completed seals have different scopes. \
    Uncaptured signals remain unavailable.";

/// One chart of the Self page with the readings its legend shows for the inspected report.
pub struct Chart {
    pub panel: Rc<Panel>,
    pub legend: Vec<Legend>,
    /// Where the budget line stands, as a fraction of the plot's height.
    pub budget: Option<f32>,
    pub maximum: f32,
    pub whole: bool,
}

pub struct Legend {
    pub name: &'static str,
    pub color: u32,
    pub reading: String,
}

/// The four cards above the pipeline, or what the app cannot show without its own collector.
#[must_use]
pub fn cards(reports: &SelfReports) -> Vec<Card> {
    if reports.started.is_none() {
        return [
            "Recording CPU",
            "Scheduled tick slips",
            "Sealed windows",
            "Lost publications / events",
        ]
        .into_iter()
        .map(|label| card(label, "Not captured".into(), NOT_OWNED.into()))
        .collect();
    }

    let recording = reports.minutes.back().map_or_else(
        || {
            card(
                "Recording CPU",
                "Unavailable".into(),
                "No complete minute yet".into(),
            )
        },
        |minute| {
            card(
                "Recording CPU",
                format!("{}%", fixed_text(minute.combined_percent, 2)),
                "Last complete minute · 5% of one core budget".into(),
            )
        },
    );
    let slips = reports
        .ticks
        .iter()
        .fold(0_u64, |total, tick| total.saturating_add(tick.slips));
    let totals = reports.seal_totals;
    let full = totals.windows.saturating_sub(totals.partial);
    vec![
        recording,
        card(
            "Scheduled tick slips",
            slips.to_string(),
            format!(
                "Across the {}",
                counted(
                    u64::try_from(reports.ticks.len()).unwrap_or(u64::MAX),
                    "ten-second report"
                )
            ),
        ),
        card(
            "Sealed windows",
            totals.windows.to_string(),
            format!(
                "{} + {}",
                counted(full, "full window"),
                counted(totals.partial, "partial window")
            ),
        ),
        card(
            "Lost publications / events",
            format!("{} / {}", totals.publications_lost, totals.events_lost),
            "Reported by seals; unknown-extent flag not captured".into(),
        ),
    ]
}

/// A count with its noun, which takes an s for any count but one.
fn counted(count: u64, noun: &str) -> String {
    let plural = if count == 1 { "" } else { "s" };
    format!("{count} {noun}{plural}")
}

const NOT_OWNED: &str = "Collector not started by this app";

fn card(label: &'static str, reading: String, note: String) -> Card {
    Card {
        label,
        reading,
        note,
        color: TEXT,
        route: "self",
    }
}

/// The value axis of a chart: its scale in quarters, labeled as the prototype labels it.
#[must_use]
pub fn axis(chart: &Chart) -> Vec<(f32, String)> {
    (0..=4_u8)
        .map(|quarter| {
            let fraction = f32::from(quarter) / 4.0;
            let value = chart.maximum * fraction;
            let label = if chart.whole {
                fixed_text(value, 0)
            } else {
                crate::chart_axes::axis_label(value, "")
            };
            (fraction, label)
        })
        .collect()
}

/// The ten-second report the charts inspect: the one at `inspected`, else the newest.
#[must_use]
pub fn inspected(reports: &SelfReports, inspected: Option<u64>) -> Option<&TickReport> {
    if let Some(second) = inspected {
        if let Some(tick) = reports
            .ticks
            .iter()
            .rev()
            .find(|tick| tick.second == second)
        {
            return Some(tick);
        }
    }
    reports.ticks.back()
}

/// The ten-second report nearest `second`, as the prototype picks one under the pointer.
#[must_use]
pub fn nearest(reports: &SelfReports, second: u64) -> Option<u64> {
    reports
        .ticks
        .iter()
        .min_by_key(|tick| tick.second.abs_diff(second))
        .map(|tick| tick.second)
}

#[must_use]
pub fn charts(reports: &SelfReports, window: TimeWindow, inspected: Option<u64>) -> Vec<Chart> {
    DEFINITIONS
        .iter()
        .map(|definition| chart(reports, window, inspected, definition))
        .collect()
}

fn chart(
    reports: &SelfReports,
    window: TimeWindow,
    inspected: Option<u64>,
    definition: &Definition,
) -> Chart {
    let rows = chart_rows(reports, window, definition);
    let whole = definition.digits == 0;
    let measured = rows
        .iter()
        .flat_map(|(_, values)| values.iter().flatten())
        .fold(1.0_f32, |maximum, value| maximum.max(*value));
    let ceiling = definition.maximum.unwrap_or(measured * 1.15);
    let maximum = if whole {
        (ceiling / 4.0).ceil() * 4.0
    } else {
        ceiling
    };
    let selected = rows
        .iter()
        .position(|(second, _)| Some(*second) == inspected);
    let legend = definition
        .fields
        .iter()
        .enumerate()
        .map(|(index, (_, name, color))| {
            let reading = selected.map_or_else(
                || "No report".into(),
                |row| reading(rows[row].1[index], definition),
            );
            Legend {
                name,
                color: *color,
                reading,
            }
        })
        .collect();
    Chart {
        panel: Rc::new(chart_panel(definition, &rows, window, maximum)),
        legend,
        budget: definition.budget.then_some(BUDGET_PERCENT / maximum),
        maximum,
        whole,
    }
}

/// Each report in the window with its second and the value of every field the chart draws.
fn chart_rows(
    reports: &SelfReports,
    window: TimeWindow,
    definition: &Definition,
) -> Vec<(u64, Vec<Option<f32>>)> {
    let end = window.first.saturating_add(window.span);
    let within = |second: u64| (window.first..=end).contains(&second);
    let fields = definition.fields.iter().map(|(field, _, _)| *field);
    match definition.group {
        Group::Ticks => reports
            .ticks
            .iter()
            .filter(|tick| within(tick.second))
            .map(|tick| {
                (
                    tick.second,
                    fields.clone().map(|f| tick_value(f, tick)).collect(),
                )
            })
            .collect(),
        Group::Minutes => reports
            .minutes
            .iter()
            .filter(|minute| within(minute.second))
            .map(|minute| {
                let values = fields.clone().map(|f| minute_value(f, minute)).collect();
                (minute.second, values)
            })
            .collect(),
    }
}

fn chart_panel(
    definition: &Definition,
    rows: &[(u64, Vec<Option<f32>>)],
    window: TimeWindow,
    maximum: f32,
) -> Panel {
    let span = window.span.to_f32().unwrap_or(f32::MAX);
    let positions = rows
        .iter()
        .map(|(second, _)| {
            let offset = second.saturating_sub(window.first).to_f32().unwrap_or(0.0);
            offset / span
        })
        .collect();
    let lines = definition
        .fields
        .iter()
        .enumerate()
        .map(|(index, (_, name, color))| Line {
            name: (*name).into(),
            color: format!("{color:06x}"),
            values: rows.iter().map(|(_, values)| values[index]).collect(),
            stack: None,
            negative: None,
            dashed: None,
            axis: None,
            unit: None,
            fill: None,
        })
        .collect();
    Panel {
        selected_index: None,
        positions: Some(positions),
        segments: None,
        id: definition.id.into(),
        title: definition.title.into(),
        unit: definition.unit.into(),
        kind: "self-line".into(),
        lines,
        minimum: Some(0.0),
        maximum: Some(maximum),
        stack: None,
        // The prototype draws the Self charts as bare lines.
        fill: Some(0.0),
        caption: Some(definition.caption.into()),
        help: Some(HELP.into()),
        source: None,
        quantities: None,
        states: None,
        table_headings: None,
        table_rows: None,
        window: Some(window),
    }
}

fn reading(value: Option<f32>, definition: &Definition) -> String {
    value.map_or_else(
        || "Unavailable".into(),
        |value| {
            format!(
                "{} {}",
                fixed_text(value, definition.digits),
                definition.unit
            )
        },
    )
}

fn tick_value(field: Field, tick: &TickReport) -> Option<f32> {
    match field {
        Field::Collector => Some(tick.collector_cpu_percent),
        Field::LateAverage => Some(tick.late_average_ms),
        Field::LateMaximum => Some(tick.late_maximum_ms),
        Field::Observed => tick.processes_observed.to_f32(),
        Field::Refused => tick.processes_refused.to_f32(),
        Field::Gone => tick.processes_gone.to_f32(),
        Field::Slips => tick.slips.to_f32(),
        Field::Drops => tick.event_drops.to_f32(),
        _ => None,
    }
}

fn minute_value(field: Field, minute: &MinuteReport) -> Option<f32> {
    match field {
        Field::MinuteCollector => share(minute.collector_cpu_ns, minute.elapsed_ns),
        Field::MinutePersister => share(minute.persister_cpu_ns, minute.elapsed_ns),
        Field::MinuteCombined => Some(minute.combined_percent),
        Field::Footprint => mebibytes(minute.footprint_bytes?),
        Field::FootprintMaximum => mebibytes(minute.footprint_bytes_max?),
        _ => None,
    }
}

/// CPU time as a percentage of one core over `elapsed_ns`.
fn share(cpu_ns: u64, elapsed_ns: u64) -> Option<f32> {
    if elapsed_ns == 0 {
        return None;
    }
    let share = cpu_ns.to_f64()? / elapsed_ns.to_f64()? * 100.0;
    share.to_f32()
}

fn mebibytes(bytes: u64) -> Option<f32> {
    (bytes.to_f64()? / 1_048_576.0).to_f32()
}

/// The inspected report's CPU cost by source, the costliest first, as the prototype names them.
#[must_use]
pub fn sources(tick: &TickReport) -> Vec<(String, f32)> {
    let mut sources: Vec<(String, f32)> = tick
        .sources
        .iter()
        .map(|(name, value)| {
            let name = match name.as_str() {
                "ioreport" => "IOReport".into(),
                "smc" => "SMC".into(),
                _ => name.clone(),
            };
            (name, *value)
        })
        .collect();
    sources.sort_by(|left, right| right.1.total_cmp(&left.1));
    sources
}

pub const SEAL_HEADINGS: [&str; 6] = [
    "Window",
    "Publications",
    "Seal time",
    "Staged data",
    "Lost publications",
    "Lost events",
];

#[must_use]
pub fn seal_rows(reports: &SelfReports) -> Vec<Vec<String>> {
    reports
        .seals
        .iter()
        .map(|seal| {
            let window = if seal.publication_end.saturating_sub(seal.publication_first) < 60 {
                format!("{} · partial", seal.window)
            } else {
                seal.window.to_string()
            };
            vec![
                window,
                format!("{}…{}", seal.publication_first, seal.publication_end),
                value(seal.sealing_ns.to_f64().map(|ns| ns / 1e6), " ms", 1),
                value(
                    seal.staged_bytes.to_f64().map(|bytes| bytes / 1_048_576.0),
                    " MiB",
                    2,
                ),
                seal.publications_lost.to_string(),
                seal.events_lost.to_string(),
            ]
        })
        .collect()
}

pub const REPORT_HEADINGS: [&str; 10] = [
    "Report ends",
    "Collector CPU",
    "Collector power",
    "Average lateness",
    "Maximum lateness",
    "Slips",
    "Processes observed",
    "Usage refused",
    "Gone",
    "Event-list drops",
];

#[must_use]
pub fn report_rows(reports: &SelfReports) -> Vec<Vec<String>> {
    reports
        .ticks
        .iter()
        .map(|tick| {
            vec![
                crate::chart_axes::clock(tick.second, true),
                value(Some(tick.collector_cpu_percent.into()), "%", 2),
                value(Some(tick.collector_milliwatts.into()), " mW", 2),
                value(Some(tick.late_average_ms.into()), " ms", 1),
                value(Some(tick.late_maximum_ms.into()), " ms", 1),
                tick.slips.to_string(),
                tick.processes_observed.to_string(),
                tick.processes_refused.to_string(),
                tick.processes_gone.to_string(),
                tick.event_drops.to_string(),
            ]
        })
        .collect()
}

pub const MINUTE_HEADINGS: [&str; 6] = [
    "Minute ends",
    "Collector CPU",
    "Persister CPU",
    "Combined CPU",
    "Current footprint",
    "Lifetime maximum",
];

#[must_use]
pub fn minute_rows(reports: &SelfReports) -> Vec<Vec<String>> {
    reports
        .minutes
        .iter()
        .map(|minute| {
            let percent = |field| minute_value(field, minute).map(f64::from);
            vec![
                crate::chart_axes::clock(minute.second, true),
                value(percent(Field::MinuteCollector), "%", 3),
                value(percent(Field::MinutePersister), "%", 3),
                value(percent(Field::MinuteCombined), "%", 3),
                value(percent(Field::Footprint), " MiB", 2),
                value(percent(Field::FootprintMaximum), " MiB", 2),
            ]
        })
        .collect()
}

/// The prototype's `selfValue`: fixed digits and the unit, or Unavailable.
fn value(value: Option<f64>, unit: &str, digits: usize) -> String {
    match value.and_then(|value| value.to_f32()) {
        Some(value) => format!("{}{unit}", fixed_text(value, digits)),
        None => "Unavailable".into(),
    }
}

/// Minutes and seconds since `started`, as the prototype writes a run's elapsed time.
#[must_use]
pub fn elapsed(started: u64, now: u64) -> String {
    let seconds = now.saturating_sub(started);
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
#[path = "self_monitoring_tests.rs"]
mod tests;
