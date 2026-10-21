use std::rc::Rc;

use num_traits::ToPrimitive;
use serde_json::Value;

use crate::data::{Line, Panel};
use crate::live_hover::Target;
use crate::live_resident_history::{COLORS, Observation};

#[path = "live_app_machine_reference.rs"]
mod machine_reference;
#[path = "live_app_attribution_validation.rs"]
mod validation;
#[path = "live_app_attribution_view.rs"]
pub(crate) mod view;

use machine_reference::MachineReference;

pub enum AppAttributionHistory {
    Ready(History),
    Unavailable(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Cpu,
    Power,
    Disk,
}

pub struct History {
    metric: Metric,
    points: Vec<Point>,
    cohort: Vec<String>,
    positions: Vec<f32>,
    window: crate::chart_axes::TimeWindow,
    selected_index: usize,
    retention_limited: bool,
}

struct Point {
    observation: Observation,
    segment: u64,
    elapsed_ns: Option<u64>,
    source_available: bool,
    membership_complete: Option<bool>,
    ungrouped_members: Option<u32>,
    usage_refused: Option<u32>,
    usage_gone: Option<u32>,
    apps: Vec<Option<Reading>>,
    other: Option<Other>,
    reference: MachineReference,
}

#[derive(Clone, Copy)]
struct Reading {
    rate: Option<f64>,
    measure: Option<Measure>,
    membership_complete: bool,
}

#[derive(Clone, Copy)]
struct Measure {
    known: u64,
    measured_members: u32,
    unavailable_members: u32,
    overflowed: bool,
}

struct Other {
    app_count: u32,
    reading: Reading,
}

impl Metric {
    pub fn panel_id(self) -> &'static str {
        match self {
            Self::Cpu => "live-cpu-apps",
            Self::Power => "live-power-apps",
            Self::Disk => "live-disk-apps",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Cpu => "CPU by app",
            Self::Power => "Process power by app",
            Self::Disk => "Disk I/O by app",
        }
    }

    fn unit(self) -> &'static str {
        match self {
            Self::Cpu => "%",
            Self::Power => "W",
            Self::Disk => "MB/s",
        }
    }

    pub fn from_panel(id: &str) -> Option<Self> {
        match id {
            "live-cpu-apps" => Some(Self::Cpu),
            "live-power-apps" => Some(Self::Power),
            "live-disk-apps" => Some(Self::Disk),
            _ => None,
        }
    }

    pub fn from_route(route: &str) -> Option<Self> {
        match route {
            "cpu" => Some(Self::Cpu),
            "energy" => Some(Self::Power),
            "storage" => Some(Self::Disk),
            _ => None,
        }
    }

    fn divisor(self) -> f64 {
        match self {
            Self::Cpu | Self::Power => 1.0,
            Self::Disk => 1_000_000.0,
        }
    }

    fn rate_text(self, rate: f64) -> String {
        let Some(value) = (rate / self.divisor()).to_f32() else {
            return "Unavailable".to_owned();
        };

        crate::live_projection::quantity_text(value, self.unit())
    }

    pub fn source(self) -> viewer::AppAttributionMetric {
        match self {
            Self::Cpu => viewer::AppAttributionMetric::Cpu,
            Self::Power => viewer::AppAttributionMetric::Power,
            Self::Disk => viewer::AppAttributionMetric::Disk,
        }
    }
}

impl AppAttributionHistory {
    pub fn prepare(metric: Metric, value: &Value, selection: &Value) -> Self {
        match History::decode(metric, value, selection) {
            Ok(history) => Self::Ready(history),
            Err(error) => Self::Unavailable(error),
        }
    }

    pub fn panel(&self) -> Option<Rc<Panel>> {
        match self {
            Self::Ready(history) => Some(Rc::new(history.panel())),
            Self::Unavailable(_) => None,
        }
    }

    pub fn index(&self, target: Option<&Target>) -> Option<usize> {
        let Self::Ready(history) = self else {
            return None;
        };
        let Some(target) = target else {
            return Some(history.selected_index);
        };

        history
            .points
            .iter()
            .position(|point| point.observation.matches(target))
    }

    pub fn amount(&self, line: usize, index: Option<usize>) -> Option<f64> {
        let Self::Ready(history) = self else {
            return None;
        };

        let point = history.points.get(index?)?;
        if history.is_reference(line) {
            return point.reference.rate();
        }

        point.reading(line)?.rate
    }

    #[cfg(test)]
    pub fn captured_amount(&self, line: usize, index: Option<usize>) -> Option<u64> {
        let Self::Ready(history) = self else {
            return None;
        };
        let reading = history.points.get(index?)?.reading(line)?;
        reading.rate?;

        Some(reading.measure?.known)
    }

    pub fn reading(&self, line: usize, index: Option<usize>) -> String {
        let Self::Ready(history) = self else {
            return "Unavailable".into();
        };
        if history.is_reference(line) {
            return history.reference_reading(index);
        }
        let Some(reading) = index
            .and_then(|index| history.points.get(index))
            .and_then(|point| point.reading(line))
        else {
            return "Unavailable".into();
        };
        let Some(rate) = reading.rate else {
            return reading.unavailable();
        };
        let text = history.metric.rate_text(rate);
        let Some(measure) = reading.measure else {
            return text;
        };
        if !reading.complete() {
            return format!(
                "{text} known subtotal · {} measured · {} unavailable",
                measure.measured_members, measure.unavailable_members
            );
        }

        text
    }

    pub fn is_reference(&self, line: usize) -> bool {
        match self {
            Self::Ready(history) => history.is_reference(line),
            Self::Unavailable(_) => false,
        }
    }

    pub fn reference_reading(&self, index: Option<usize>) -> Option<String> {
        let Self::Ready(history) = self else {
            return None;
        };
        let name = history.reference_label()?;

        Some(format!("{name}: {}", history.reference_reading(index)))
    }

    pub fn coverage(&self, index: Option<usize>) -> String {
        let history = match self {
            Self::Ready(history) => history,
            Self::Unavailable(error) => return format!("App rate history unavailable: {error}"),
        };
        let Some(point) = index.and_then(|index| history.points.get(index)) else {
            return "App rate history unavailable at this observation".into();
        };
        if !point.source_available {
            return "Captured App source unavailable at this observation".into();
        }
        let membership = if point.complete() {
            "complete"
        } else {
            "partial"
        };
        let other = point.other.as_ref().map_or(0, |other| other.app_count);
        let retention = if history.retention_limited {
            " · retained context is limited"
        } else {
            ""
        };

        format!("Membership {membership} · {other} Other Apps{retention}")
    }

    pub fn headline(&self, index: Option<usize>) -> String {
        match self.total(index) {
            Some((rate, true)) => format!("{rate} captured"),
            Some((rate, false)) => format!("{rate} known subtotal"),
            None => "Unavailable".into(),
        }
    }

    /// The App rates added up at one observation, without how much of them was captured.
    pub fn total_rate(&self, index: Option<usize>) -> String {
        self.total(index)
            .map_or_else(|| "Unavailable".into(), |(rate, _)| rate)
    }

    /// The added-up rate as text, and whether every member of it was captured.
    fn total(&self, index: Option<usize>) -> Option<(String, bool)> {
        let Self::Ready(history) = self else {
            return None;
        };
        let point = index.and_then(|index| history.points.get(index))?;
        let mut known = 0.0;
        let mut measured = false;
        let mut complete = point.complete();
        for line in 0..=history.cohort.len() {
            let reading = point.reading(line);
            if let Some(rate) = reading.and_then(|reading| reading.rate) {
                known += rate;
                measured = true;
                if let Some(reading) = reading {
                    complete &= reading.complete();
                }
            } else {
                complete = false;
            }
        }

        measured.then(|| (history.metric.rate_text(known), complete))
    }
}

impl History {
    fn panel(&self) -> Panel {
        let mut lines = Vec::with_capacity(self.cohort.len() + 2);
        for (index, name) in std::iter::once("Other")
            .chain(self.cohort.iter().map(String::as_str))
            .enumerate()
        {
            lines.push(Line {
                name: name.strip_suffix(".app").unwrap_or(name).into(),
                color: COLORS[index].into(),
                values: self
                    .points
                    .iter()
                    .map(|point| point.value(index, self.metric))
                    .collect(),
                stack: None,
                negative: Some(false),
                dashed: Some(false),
                axis: None,
                follows: None,
                unit: Some(self.metric.unit().into()),
                fill: None,
            });
        }

        self.panel_reference_line(&mut lines);

        Panel {
            id: self.metric.panel_id().into(),
            title: self.metric.title().into(),
            unit: self.metric.unit().into(),
            kind: "line".into(),
            lines,
            minimum: Some(0.0),
            maximum: None,
            stack: Some(true),
            fill: Some(1.0),
            caption: None,
            help: Some(self.help().into()),
            source: Some("Captured process intervals".into()),
            quantities: None,
            states: None,
            table_headings: None,
            table_rows: None,
            positions: Some(self.positions.clone()),
            segments: Some(self.points.iter().map(|point| point.segment).collect()),
            selected_index: Some(self.selected_index),
            window: Some(self.window),
            detail_first: None,
            sliding: false,
        }
    }

    fn help(&self) -> &'static str {
        match self.metric {
            Metric::Cpu => {
                "Captured process CPU time per interval. 100% equals one core; \
                Apps can exceed 100%. Eight leaders use the complete context's mean rate. \
                Other sums remaining captured Apps. Missing intervals remain gaps; known \
                subtotals exclude unavailable process members. The dashed All CPU reference sums \
                recorded per-core busy percentages; it is excluded from App totals."
            }
            Metric::Power => {
                "Captured process energy per interval, in watts. This is separate \
                from system power and GPU power. Eight leaders use the complete context's mean \
                rate. Other sums remaining captured Apps. Missing intervals remain gaps; \
                known subtotals exclude unavailable process members."
            }
            Metric::Disk => {
                "Captured process read-plus-write bytes per interval, displayed in MB/s. \
                Exact interval bytes stay separate from the rate. This is separate from \
                device-driver throughput. Eight leaders use the complete context's mean rate; \
                Other sums remaining captured Apps. Known subtotals include only members with \
                both directions measured. Missing Apps and unavailable intervals remain gaps. \
                The dashed All disk I/O reference sums measured whole-device read and write \
                rates; incomplete device coverage is unavailable. References stay outside \
                App totals."
            }
        }
    }
}

impl Point {
    fn complete(&self) -> bool {
        [
            self.source_available,
            self.membership_complete == Some(true),
            self.ungrouped_members == Some(0),
            self.usage_refused == Some(0),
            self.usage_gone == Some(0),
        ]
        .iter()
        .all(|complete| *complete)
    }

    fn reading(&self, line: usize) -> Option<Reading> {
        if line == 0 {
            return Some(self.other.as_ref()?.reading);
        }

        self.apps.get(line - 1).copied().flatten()
    }

    fn value(&self, line: usize, metric: Metric) -> Option<f32> {
        self.observation.wall_ns?;
        (self.reading(line)?.rate? / metric.divisor()).to_f32()
    }
}

impl Reading {
    fn complete(self) -> bool {
        if !self.membership_complete {
            return false;
        }

        self.measure.is_some_and(|measure| {
            if measure.overflowed {
                return false;
            }
            measure.unavailable_members == 0
        })
    }

    fn unavailable(self) -> String {
        match self.measure {
            Some(measure) if measure.overflowed => "Unavailable · counter sum overflow".into(),
            Some(measure) => format!("Unavailable · {} unavailable", measure.unavailable_members),
            None => "Unavailable".into(),
        }
    }
}

#[cfg(test)]
#[path = "live_app_attribution_tests.rs"]
pub(crate) mod tests;
