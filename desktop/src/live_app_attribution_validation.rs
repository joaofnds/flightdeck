use std::collections::BTreeSet;

use num_traits::ToPrimitive;
use serde_json::Value;

use super::{History, Measure, Metric, Other, Point, Reading};
use crate::live_projection::integer;
use crate::live_resident_history::{
    Observation, boolean, count, decode_cohort_labels, optional_boolean, optional_count,
    optional_integer, position, validate_coverage,
};

const POINTS_MAX: usize = crate::data::LIVE_SAMPLE_LIMIT;
const COHORT_MAX: usize = 8;
const _: () = {
    assert!(POINTS_MAX == 4096);
    assert!(COHORT_MAX + 1 == super::COLORS.len());
    assert!(size_of::<Point>() + COHORT_MAX * size_of::<Option<Reading>>() <= 1024);
};

impl History {
    pub(super) fn decode(metric: Metric, value: &Value, selection: &Value) -> Result<Self, String> {
        validate_metric(metric, value)?;
        let selected = Observation::decode(&value["selected"])?;
        crate::live_source::identity_validate(&value["selected"], &selection["selected"])?;
        if selection["selected"].get("stamp").is_some() {
            if selected != Observation::decode(&selection["selected"])? {
                return Err("App rate history has a different selected observation".into());
            }
        }
        let second = integer(&selection["second"])?;
        if selected.wall_ns.map(|wall| wall / 1_000_000_000) != Some(second) {
            return Err("App rate history has a different selected timestamp".into());
        }
        let last = integer(&selection["context_last_second"])?;
        let values = value["points"]
            .as_array()
            .ok_or("Missing App rate points")?;
        if values.is_empty() {
            return Err("App rate history is empty".into());
        }
        if values.len() > POINTS_MAX {
            return Err("App rate history exceeds 4096 points".into());
        }
        let mut history = Self {
            metric,
            points: Vec::with_capacity(values.len()),
            cohort: decode_cohort(&value["cohort"])?,
            positions: Vec::with_capacity(values.len()),
            window: crate::chart_axes::TimeWindow::live(last),
            selected_index: 0,
            retention_limited: boolean(&value["coverage"]["retention_limited"])?,
        };
        history.decode_points(values, &selected, last)?;
        history.attach_reference(selection);
        validate_coverage(&value["coverage"])?;

        Ok(history)
    }

    fn decode_points(
        &mut self,
        values: &[Value],
        selected: &Observation,
        context_last_second: u64,
    ) -> Result<(), String> {
        assert!(values.len() <= POINTS_MAX);
        let mut selected_index = None;
        let mut identities = BTreeSet::new();
        for value in values {
            let point = Point::decode(self.metric, value, self.cohort.len())?;
            if point.observation.recording != selected.recording {
                return Err("App rate observation belongs to another recording".into());
            }
            if !identities.insert((
                point.observation.recording,
                point.observation.boot,
                point.observation.run,
                point.observation.publication,
            )) {
                return Err("Duplicate App rate observation".into());
            }
            if let Some(previous) = self.points.last() {
                if previous.observation >= point.observation {
                    return Err("App rate observations are not ordered".into());
                }
            }
            if &point.observation == selected {
                selected_index = Some(self.points.len());
            }
            self.positions
                .push(position(point.observation.wall_ns, context_last_second)?);
            self.points.push(point);
        }
        self.selected_index = selected_index.ok_or("Selected App rate observation missing")?;

        Ok(())
    }
}

impl Point {
    fn decode(metric: Metric, value: &Value, count: usize) -> Result<Self, String> {
        let apps = value["apps"]
            .as_array()
            .ok_or("Missing App rate measures")?;
        if apps.len() != count {
            return Err("App rate measure count differs from cohort".into());
        }
        let point = Self {
            observation: Observation::decode(&value["observation"])?,
            segment: integer(&value["segment"])?,
            elapsed_ns: optional_integer(&value["elapsed_ns"])?,
            source_available: boolean(&value["source_available"])?,
            membership_complete: optional_boolean(&value["membership_complete"])?,
            ungrouped_members: optional_count(&value["ungrouped_members"])?,
            usage_refused: optional_count(&value["usage_refused"])?,
            usage_gone: optional_count(&value["usage_gone"])?,
            apps: apps
                .iter()
                .map(|app| Reading::decode_optional(metric, app))
                .collect::<Result<_, _>>()?,
            other: Other::decode_optional(metric, &value["other"])?,
            reference: super::MachineReference::Unavailable("machine observation unavailable"),
        };
        point.validate()?;

        Ok(point)
    }

    fn validate(&self) -> Result<(), String> {
        let coverage_present = [
            self.membership_complete.is_some(),
            self.ungrouped_members.is_some(),
            self.usage_refused.is_some(),
            self.usage_gone.is_some(),
            self.other.is_some(),
        ];
        if self.source_available {
            if coverage_present.contains(&false) {
                return Err("Available App rate source lacks coverage".into());
            }
        } else {
            if coverage_present.contains(&true) {
                return Err("Unavailable App rate source contains coverage".into());
            }
            if self.elapsed_ns.is_some() {
                return Err("Unavailable App rate source contains an interval".into());
            }
            if self.apps.iter().any(Option::is_some) {
                return Err("Unavailable App rate source contains readings".into());
            }
        }
        if let Some(other) = &self.other {
            if other.app_count == 0 {
                if other.reading.rate.is_some() {
                    if !self.complete() {
                        return Err("Incomplete App source contains a measured empty Other".into());
                    }
                }
            }
        }
        for reading in self
            .apps
            .iter()
            .flatten()
            .copied()
            .chain(self.other.as_ref().map(|other| other.reading))
        {
            if reading.rate.is_some() {
                match self.elapsed_ns {
                    Some(elapsed) if elapsed > 0 => {}
                    _ => return Err("App rate requires a positive captured interval".into()),
                }
            }
        }

        Ok(())
    }
}

impl Reading {
    fn decode_optional(metric: Metric, value: &Value) -> Result<Option<Self>, String> {
        if value.is_null() {
            return Ok(None);
        }
        count(&value["members"])?;

        let reading = Self::decode(metric, value)?;
        reading.validate_measured()?;

        Ok(Some(reading))
    }

    fn decode(metric: Metric, value: &Value) -> Result<Self, String> {
        let (measure, rate) = match metric {
            Metric::Cpu => (&value["cpu_ns"], &value["cpu_percent_one_core"]),
            Metric::Power => (&value["energy_nj"], &value["energy_watts"]),
            Metric::Disk => (&value["disk_io_bytes"], &value["disk_io_bps"]),
        };
        let reading = Self {
            rate: decode_rate(rate)?,
            measure: Measure::decode_optional(measure)?,
            membership_complete: boolean(&value["membership_complete"])?,
        };
        if reading.rate.is_some() {
            let measure = reading.measure.ok_or("App rate lacks a captured measure")?;
            if measure.overflowed {
                return Err("Overflowed App measure contains a rate".into());
            }
        }

        Ok(reading)
    }

    fn validate_measured(self) -> Result<(), String> {
        if self.rate.is_some() {
            if self
                .measure
                .is_some_and(|measure| measure.measured_members == 0)
            {
                return Err("Unmeasured App contains a rate".into());
            }
        }

        Ok(())
    }
}

impl Measure {
    fn decode_optional(value: &Value) -> Result<Option<Self>, String> {
        if value.is_null() {
            return Ok(None);
        }

        Ok(Some(Self {
            known: integer(&value["known"])?,
            measured_members: count(&value["measured_members"])?,
            unavailable_members: count(&value["unavailable_members"])?,
            overflowed: boolean(&value["overflowed"])?,
        }))
    }
}

impl Other {
    fn decode_optional(metric: Metric, value: &Value) -> Result<Option<Self>, String> {
        if value.is_null() {
            return Ok(None);
        }

        let other = Self {
            app_count: count(&value["app_count"])?,
            reading: Reading::decode(metric, value)?,
        };

        if other.app_count == 0 {
            if let Some(rate) = other.reading.rate {
                let measure = other.reading.measure.ok_or("Empty Other lacks a measure")?;
                if rate != 0.0 {
                    return Err("Empty Other contains a nonzero rate".into());
                }
                if measure.known != 0 {
                    return Err("Empty Other contains a nonzero counter".into());
                }
                if measure.unavailable_members != 0 {
                    return Err("Empty Other contains unavailable members".into());
                }
            }
        } else {
            other.reading.validate_measured()?;
        }

        Ok(Some(other))
    }
}

fn decode_rate(value: &Value) -> Result<Option<f64>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let rate = value.as_f64().ok_or("Invalid App rate")?;
    if !rate.is_finite() {
        return Err("Nonfinite App rate".into());
    }
    if rate < 0.0 {
        return Err("Negative App rate".into());
    }
    let display = rate.to_f32().ok_or("App rate exceeds chart precision")?;
    if !display.is_finite() {
        return Err("App rate exceeds chart range".into());
    }

    Ok(Some(rate))
}

fn validate_metric(metric: Metric, value: &Value) -> Result<(), String> {
    let expected = match metric {
        Metric::Cpu => "cpu",
        Metric::Power => "power",
        Metric::Disk => "disk",
    };
    if value["metric"].as_str() != Some(expected) {
        return Err("App rate history has a different metric".into());
    }
    if integer(&value["context_seconds"])? != 900 {
        return Err("App rate context must contain 900 seconds".into());
    }

    Ok(())
}

fn decode_cohort(value: &Value) -> Result<Vec<String>, String> {
    let entries = value.as_array().ok_or("Missing App rate cohort")?;
    if entries.len() > COHORT_MAX {
        return Err("App rate cohort exceeds eight leaders".into());
    }
    let mut names = Vec::with_capacity(entries.len());
    let mut keys = BTreeSet::new();
    for entry in entries {
        let key = crate::live_source::app_key_from_json(entry)?;
        let bundle = key.kind == "bundle";
        if !keys.insert((key.kind, key.bytes_hex)) {
            return Err("Duplicate App rate cohort key".into());
        }
        let display = entry["key"]["display"]
            .as_str()
            .ok_or("Missing App rate name")?;
        if display.len() > 16_384 {
            return Err("App rate display exceeds its bound".into());
        }
        let name = if bundle {
            display
                .rsplit('/')
                .next()
                .ok_or("Missing App rate basename")?
        } else {
            display
        };
        names.push(name.into());
    }
    decode_cohort_labels(entries, &mut names)?;

    Ok(names)
}
