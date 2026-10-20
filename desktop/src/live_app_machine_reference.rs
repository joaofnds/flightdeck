use std::collections::BTreeMap;

use num_traits::ToPrimitive;
use serde_json::Value;

use super::{History, Metric};
use crate::data::Line;
use crate::live_projection::{identity, integer};

type Key = ([u64; 2], [u64; 2], u64);

#[derive(Clone, Copy)]
pub(super) enum MachineReference {
    Available(f64),
    Unavailable(&'static str),
}

impl MachineReference {
    pub(super) fn rate(self) -> Option<f64> {
        match self {
            Self::Available(rate) => Some(rate),
            Self::Unavailable(_) => None,
        }
    }

    fn decode(metric: Metric, value: &Value) -> Self {
        let rate = match metric {
            Metric::Cpu => decode_rate(&value["cpu_percent_one_core"]),
            Metric::Disk => {
                let storage = &value["storage"];
                match storage["state"].as_str() {
                    Some("complete") => {}
                    Some("partial") => return Self::Unavailable("whole-device scan partial"),
                    Some("exhausted") => return Self::Unavailable("whole-device scan exhausted"),
                    Some("failed") => return Self::Unavailable("whole-device scan failed"),
                    _ => return Self::Unavailable("whole-device source unavailable"),
                }
                decode_rate(&storage["read_bps"])
                    .zip(decode_rate(&storage["write_bps"]))
                    .map(|(read, write)| read + write)
            }
            Metric::Power => unreachable!("Process power has no machine reference"),
        };
        let Some(rate) = rate.filter(|rate| rate.is_finite()) else {
            return Self::Unavailable("machine interval unavailable");
        };

        let Some(display) = (rate / metric.divisor()).to_f32() else {
            return Self::Unavailable("machine rate exceeds chart range");
        };

        if !display.is_finite() {
            return Self::Unavailable("machine rate exceeds chart range");
        }

        Self::Available(rate)
    }
}

impl History {
    pub(super) fn is_reference(&self, line: usize) -> bool {
        if self.reference_name().is_none() {
            return false;
        }

        line == self.cohort.len() + 1
    }

    pub(super) fn reference_name(&self) -> Option<&'static str> {
        match self.metric {
            Metric::Cpu => Some("All CPU"),
            Metric::Disk => Some("All disk I/O"),
            Metric::Power => None,
        }
    }

    pub(super) fn reference_reading(&self, index: Option<usize>) -> String {
        let Some(point) = index.and_then(|index| self.points.get(index)) else {
            return "Unavailable · machine observation unavailable".into();
        };

        match point.reference {
            MachineReference::Unavailable(reason) => format!("Unavailable · {reason}"),
            MachineReference::Available(rate) => {
                let scope = match self.metric {
                    Metric::Cpu => "all recorded logical CPUs",
                    Metric::Disk => "whole-device read + write",
                    Metric::Power => unreachable!("Process power has no machine reference"),
                };
                format!("{} · {scope}", self.metric.rate_text(rate))
            }
        }
    }

    pub(super) fn reference_label(&self) -> Option<String> {
        let name = self.reference_name()?;
        if !self.cohort.iter().any(|app| app == name) {
            return Some(name.into());
        }

        for suffix in 1..=9_u32 {
            let candidate = format!("{name} (machine reference {suffix})");
            if !self.cohort.contains(&candidate) {
                return Some(candidate);
            }
        }
        unreachable!("At most eight App names cannot occupy nine reference labels");
    }

    pub(super) fn panel_reference_line(&self, lines: &mut Vec<Line>) {
        let Some(name) = self.reference_label() else {
            return;
        };

        lines.push(Line {
            name,
            color: "#ccccdc".into(),
            values: self
                .points
                .iter()
                .map(|point| {
                    point.observation.wall_ns?;
                    (point.reference.rate()? / self.metric.divisor()).to_f32()
                })
                .collect(),
            stack: Some(false),
            negative: Some(false),
            dashed: Some(true),
            axis: None,
            follows: None,
            unit: Some(self.metric.unit().into()),
            fill: Some(0.0),
        });
    }

    pub(super) fn attach_reference(&mut self, selection: &Value) {
        if self.reference_name().is_none() {
            return;
        }

        let mut index = BTreeMap::new();
        if let Err(error) = attach_reference_index(selection, &mut index) {
            eprintln!("flightdeck machine comparison: {error}");
            for point in &mut self.points {
                point.reference = MachineReference::Unavailable("machine context invalid");
            }
            return;
        }

        for point in &mut self.points {
            let observation = &point.observation;
            let key = (observation.run, observation.boot, observation.publication);
            if let Some(value) = index.get(&key) {
                if let Some(wall_ns) = observation.wall_ns {
                    if value["wall_ns"].as_u64() == Some(wall_ns) {
                        point.reference = MachineReference::decode(self.metric, value);
                    }
                }
            }
        }
    }
}

fn attach_reference_index<'a>(
    selection: &'a Value,
    index: &mut BTreeMap<Key, &'a Value>,
) -> Result<(), String> {
    assert!(index.is_empty());
    let points = selection["trends"]
        .as_array()
        .ok_or("Missing scalar context")?;
    if points.len() > crate::data::LIVE_SAMPLE_LIMIT {
        return Err("Scalar context exceeds 4096 points".into());
    }

    for point in points {
        let key = (
            identity(&point["run"])?,
            identity(&point["boot"])?,
            integer(&point["publication"])?,
        );
        if index.insert(key, point).is_some() {
            return Err("Duplicate scalar observation".into());
        }
    }

    Ok(())
}

fn decode_rate(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .filter(|rate| rate.is_finite())
        .filter(|rate| *rate >= 0.0)
}

#[cfg(test)]
#[path = "live_app_machine_reference_tests.rs"]
mod tests;
