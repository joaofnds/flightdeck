use serde::Deserialize;
use serde_json::{Value, json};

use crate::live_cpu::CPUActivity;

#[derive(Debug)]
pub struct CPUHistory {
    wire: Box<CPUHistoryWire>,
}

#[derive(Debug, Deserialize)]
struct CPUHistoryWire {
    observation: CPUObservation,
    first_ns: u64,
    end_ns: u64,
    offset: u32,
    total: u32,
    restoration_error: Option<String>,
    observations: Vec<CPUSample>,
    cpus: Vec<CPUHistoryRow>,
    context_seconds: u64,
    #[serde(skip)]
    intervals: Vec<CPUInterval>,
    #[serde(skip)]
    selected_index: usize,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct CPUObservation {
    run: String,
    boot: String,
    publication: u64,
    run_tick: u64,
    wall_ns: u64,
}

#[derive(Debug, Deserialize)]
struct CPUSample {
    observation: CPUObservation,
    previous_observation: Option<CPUObservation>,
    interval_available: bool,
    cpu_count: u32,
}

#[derive(Debug, Deserialize)]
pub(crate) struct CPUHistoryRow {
    cpu: u32,
    busy_percent: Vec<Option<f64>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CPUInspection {
    Gap,
    Sample {
        row: usize,
        observation_index: usize,
    },
}

#[derive(Debug)]
struct CPUInterval {
    first_ns: u64,
    end_ns: u64,
    observation_index: usize,
}

pub const PAGE_CPUS: u32 = 32;
const CPUS_MAX: u32 = 1024;
const OBSERVATIONS_MAX: usize = 4096;
const NS: u64 = 1_000_000_000;
const _: () = {
    assert!(PAGE_CPUS < CPUS_MAX);
    assert!(OBSERVATIONS_MAX * 32 == 131_072);
    assert!(size_of::<CPUHistory>() <= 16);
};

impl CPUHistory {
    pub fn decode(value: Value, selection: &Value) -> Result<Self, String> {
        validation::preflight(&value)?;
        let mut wire: Box<CPUHistoryWire> =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        wire.validate(selection)?;
        wire.project_intervals();

        Ok(Self { wire })
    }

    pub(crate) fn offset(&self) -> u32 {
        self.wire.offset
    }

    pub(crate) fn total(&self) -> u32 {
        self.wire.total
    }

    pub(crate) fn cpus(&self) -> &[CPUHistoryRow] {
        &self.wire.cpus
    }

    pub(crate) fn observation_count(&self) -> usize {
        self.wire.observations.len()
    }

    pub fn inspect(&self, row: usize, fraction: f64) -> Option<CPUInspection> {
        if row >= self.wire.cpus.len() {
            return None;
        }

        if !fraction.is_finite() {
            return None;
        }

        if !(0.0..1.0).contains(&fraction) {
            return None;
        }

        self.wire.intervals.iter().rev().find_map(|interval| {
            if fraction >= self.fraction(interval.first_ns) {
                if fraction < self.fraction(interval.end_ns) {
                    return Some(CPUInspection::Sample {
                        row,
                        observation_index: interval.observation_index,
                    });
                }
            }

            None
        })
    }

    pub fn selected(&self) -> Option<CPUInspection> {
        if self.wire.cpus.is_empty() {
            return None;
        }

        Some(CPUInspection::Sample {
            row: 0,
            observation_index: self.wire.selected_index,
        })
    }

    /// The cores of the observation `identity` names, as the panel of the selected second reads
    /// them, so a hover shows its second without waiting on the worker. Nothing when this page
    /// lacks some of the observation's logical CPUs or the history holds no reading of it.
    pub fn activity(&self, identity: &Value) -> Result<Option<CPUActivity>, String> {
        let total = usize::try_from(self.wire.total).map_err(|error| error.to_string())?;
        if self.wire.cpus.len() != total {
            return Ok(None);
        }

        let Some(CPUInspection::Sample {
            observation_index, ..
        }) = self.matching_inspection(identity, 0)
        else {
            return Ok(None);
        };

        let sample = &self.wire.observations[observation_index];
        if sample.cpu_count != self.wire.total {
            return Ok(None);
        }

        let cpus = self
            .wire
            .cpus
            .iter()
            .map(|row| json!({"cpu":row.cpu,"busy_percent":row.busy_percent[observation_index]}))
            .collect::<Vec<_>>();
        let observation = activity_observation(&sample.observation);
        let payload = json!({"observation":observation,
            "previous_observation":sample.previous_observation.as_ref().map(activity_observation),
            "interval_available":sample.interval_available,"unavailable_reason":null,
            "cpus":cpus});

        CPUActivity::decode(payload, &observation).map(Some)
    }

    pub fn value(&self, inspection: CPUInspection) -> Option<f64> {
        let CPUInspection::Sample {
            row,
            observation_index,
        } = inspection
        else {
            return None;
        };

        self.wire
            .cpus
            .get(row)?
            .busy_percent
            .get(observation_index)
            .copied()
            .flatten()
    }

    pub fn target(&self, inspection: CPUInspection) -> Option<(u64, Value)> {
        let CPUInspection::Sample {
            row,
            observation_index,
        } = inspection
        else {
            return None;
        };

        self.wire.cpus.get(row)?;
        let point = &self.wire.observations.get(observation_index)?.observation;

        Some((
            point.wall_ns / NS,
            serde_json::json!({
            "run":point.run,"boot":point.boot,"publication":point.publication}),
        ))
    }

    pub(crate) fn matching_inspection(
        &self,
        identity: &Value,
        row: usize,
    ) -> Option<CPUInspection> {
        self.wire.cpus.get(row)?;
        let Ok((run, boot, publication)) = crate::live_source::identity_signature(identity) else {
            return None;
        };

        let observation_index = self.wire.observations.iter().position(|sample| {
            (
                sample.observation.run.as_str(),
                sample.observation.boot.as_str(),
                sample.observation.publication,
            ) == (run.as_str(), boot.as_str(), publication)
        })?;

        Some(CPUInspection::Sample {
            row,
            observation_index,
        })
    }

    pub fn fraction(&self, wall_ns: u64) -> f64 {
        let offset = wall_ns.clamp(self.wire.first_ns, self.wire.end_ns) - self.wire.first_ns;
        let seconds = u32::try_from(offset / NS);
        let nanos = u32::try_from(offset % NS);
        let span = u32::try_from((self.wire.end_ns - self.wire.first_ns) / NS);
        let (Ok(seconds), Ok(nanos), Ok(span)) = (seconds, nanos, span) else {
            unreachable!("Validated CPU history context fits 900 seconds");
        };

        (f64::from(seconds) + f64::from(nanos) / 1_000_000_000.0) / f64::from(span)
    }

    pub fn step(&self, current: Option<CPUInspection>, step: CPUStep) -> Option<CPUInspection> {
        assert!((-1..=1).contains(&step.rows));
        assert!((-1..=1).contains(&step.observations));
        if self.wire.cpus.is_empty() {
            return None;
        }

        let current = current
            .filter(|value| *value != CPUInspection::Gap)
            .or_else(|| self.selected())?;
        let CPUInspection::Sample {
            row,
            observation_index,
        } = current
        else {
            return None;
        };

        Some(CPUInspection::Sample {
            row: row
                .saturating_add_signed(isize::from(step.rows))
                .min(self.wire.cpus.len() - 1),
            observation_index: observation_index
                .saturating_add_signed(isize::from(step.observations))
                .min(self.wire.observations.len() - 1),
        })
    }
}

#[derive(Clone, Copy)]
pub struct CPUStep {
    pub observations: i8,
    pub rows: i8,
}

impl CPUHistoryWire {
    fn project_intervals(&mut self) {
        self.intervals = Vec::with_capacity(self.observations.len());
        for (index, sample) in self.observations.iter().enumerate() {
            if sample.interval_available {
                if let Some(previous) = &sample.previous_observation {
                    let first_ns = self.first_ns.max(previous.wall_ns);
                    let end_ns = self.end_ns.min(sample.observation.wall_ns);
                    if first_ns < end_ns {
                        intervals_remove_occluded(&mut self.intervals, first_ns);
                        self.intervals.push(CPUInterval {
                            first_ns,
                            end_ns,
                            observation_index: index,
                        });
                    }
                }
            }
        }
        assert!(self.intervals.len() <= OBSERVATIONS_MAX);
    }
}

impl CPUHistoryRow {
    pub(crate) fn cpu(&self) -> u32 {
        self.cpu
    }

    pub(crate) fn busy_percent(&self) -> &[Option<f64>] {
        &self.busy_percent
    }
}

fn intervals_remove_occluded(intervals: &mut Vec<CPUInterval>, first_ns: u64) {
    let count = intervals.len();
    for _attempt in 0..count {
        let Some(previous) = intervals.last() else {
            break;
        };
        if previous.end_ns <= first_ns {
            break;
        }
        let previous_first = previous.first_ns;
        let previous_index = previous.observation_index;
        intervals.pop();
        if previous_first < first_ns {
            intervals.push(CPUInterval {
                first_ns: previous_first,
                end_ns: first_ns,
                observation_index: previous_index,
            });
            break;
        }
    }
}

fn activity_observation(observation: &CPUObservation) -> Value {
    json!({"run":observation.run,"boot":observation.boot,"publication":observation.publication,
        "run_tick":observation.run_tick,"wall_ns":observation.wall_ns})
}

#[path = "live_cpu_history_validation.rs"]
mod validation;

#[path = "live_cpu_history_view.rs"]
pub(crate) mod view;

#[cfg(test)]
#[path = "live_cpu_history_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "live_cpu_history_input_tests.rs"]
mod input_tests;

#[cfg(test)]
#[path = "live_cpu_hover_input_tests.rs"]
mod hover_input_tests;
