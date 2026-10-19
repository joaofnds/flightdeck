use std::rc::Rc;

use num_traits::ToPrimitive;
use serde_json::Value;

use crate::data::{Line, Panel};
use crate::live_hover::Target;
use crate::live_resident_history::{COLORS, Observation};

#[path = "live_app_attribution_validation.rs"]
mod validation;
#[path = "live_app_attribution_view.rs"]
mod view;

pub enum AppAttributionHistory {
    Ready(History),
    Unavailable(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Metric {
    Cpu,
    Power,
}

pub struct History {
    metric: Metric,
    points: Vec<Point>,
    cohort: Vec<String>,
    positions: Vec<f32>,
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
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Cpu => "CPU by App",
            Self::Power => "Process power by App",
        }
    }

    fn unit(self) -> &'static str {
        match self {
            Self::Cpu => "%",
            Self::Power => "W",
        }
    }

    pub fn from_panel(id: &str) -> Option<Self> {
        match id {
            "live-cpu-apps" => Some(Self::Cpu),
            "live-power-apps" => Some(Self::Power),
            _ => None,
        }
    }

    pub fn from_route(route: &str) -> Option<Self> {
        match route {
            "cpu" => Some(Self::Cpu),
            "energy" => Some(Self::Power),
            _ => None,
        }
    }

    pub fn source(self) -> viewer::AppAttributionMetric {
        match self {
            Self::Cpu => viewer::AppAttributionMetric::Cpu,
            Self::Power => viewer::AppAttributionMetric::Power,
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

        history.points.get(index?)?.reading(line)?.rate
    }

    pub fn reading(&self, line: usize, index: Option<usize>) -> String {
        let Self::Ready(history) = self else {
            return "Unavailable".into();
        };
        let Some(reading) = index
            .and_then(|index| history.points.get(index))
            .and_then(|point| point.reading(line))
        else {
            return "Unavailable".into();
        };
        let Some(rate) = reading.rate else {
            return reading.unavailable();
        };
        let text = format!("{rate:.2} {}", history.metric.unit());
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
        let Self::Ready(history) = self else {
            return "Unavailable".into();
        };
        let Some(point) = index.and_then(|index| history.points.get(index)) else {
            return "Unavailable".into();
        };
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
        if !measured {
            return "Unavailable".into();
        }
        let coverage = if complete {
            "captured"
        } else {
            "known subtotal"
        };

        format!("{known:.2} {} {coverage}", history.metric.unit())
    }
}

impl History {
    fn panel(&self) -> Panel {
        let mut lines = Vec::with_capacity(self.cohort.len() + 1);
        for (index, name) in std::iter::once("Other")
            .chain(self.cohort.iter().map(String::as_str))
            .enumerate()
        {
            lines.push(Line {
                name: name.into(),
                color: COLORS[index].into(),
                values: self.points.iter().map(|point| point.value(index)).collect(),
                stack: None,
                negative: Some(false),
                dashed: Some(false),
                axis: None,
                unit: Some(self.metric.unit().into()),
                fill: None,
            });
        }

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
        }
    }

    fn help(&self) -> &'static str {
        match self.metric {
            Metric::Cpu => {
                "Captured process CPU time per interval. 100% equals one core; \
                Apps can exceed 100%. Eight leaders use the complete context's mean rate. \
                Other sums remaining captured Apps. Missing intervals remain gaps; known \
                subtotals exclude unavailable process members."
            }
            Metric::Power => {
                "Captured process energy per interval, in watts. This is separate \
                from system power and GPU power. Eight leaders use the complete context's mean \
                rate. Other sums remaining captured Apps. Missing intervals remain gaps; \
                known subtotals exclude unavailable process members."
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

    fn value(&self, line: usize) -> Option<f32> {
        self.observation.wall_ns?;
        self.reading(line)?.rate?.to_f32()
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
