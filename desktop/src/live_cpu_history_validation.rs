use std::collections::BTreeMap;

use super::{CPUHistoryWire, CPUObservation, CPUS_MAX, CPUSample, NS, OBSERVATIONS_MAX, PAGE_CPUS};
use serde_json::Value;

pub(super) fn preflight(value: &Value) -> Result<(), String> {
    let observations = value["observations"]
        .as_array()
        .ok_or("Missing CPU history observations")?;
    if observations.len() > OBSERVATIONS_MAX {
        return Err("CPU history exceeds 4096 observations".into());
    }

    let cpus = value["cpus"].as_array().ok_or("Missing CPU history rows")?;
    if cpus.len() > 32 {
        return Err("CPU history exceeds 32 CPU rows".into());
    }

    for row in cpus {
        let values = row["busy_percent"]
            .as_array()
            .ok_or("Missing CPU history values")?;
        if values.len() != observations.len() {
            return Err("CPU history row length differs from its context".into());
        }
    }

    Ok(())
}

impl CPUHistoryWire {
    pub(super) fn validate(&mut self, selection: &Value) -> Result<(), String> {
        self.observation.validate()?;
        self.observation.matches(&selection["selected"])?;
        self.validate_context(selection)?;
        self.validate_page()?;
        self.validate_observations(selection)?;
        self.validate_intervals()?;
        self.validate_rows()?;
        if let Some(message) = &self.restoration_error {
            if message.is_empty() {
                return Err("CPU history restoration diagnostic is empty".into());
            }

            if message.chars().count() > 512 {
                return Err("CPU history restoration diagnostic exceeds 512 characters".into());
            }
        }

        Ok(())
    }

    fn validate_context(&self, selection: &Value) -> Result<(), String> {
        if self.context_seconds != 900 {
            return Err("CPU history context must contain 900 seconds".into());
        }

        if selection["context_seconds"].as_u64() != Some(900) {
            return Err("CPU history does not match the selected context".into());
        }

        let second = self.observation.wall_ns / NS;
        if selection["second"].as_u64() != Some(second) {
            return Err("CPU history does not match the selected second".into());
        }

        let last = crate::live_projection::context_last_second(selection)
            .ok_or("CPU history selection has no context end")?;
        let end_ns = last
            .checked_add(1)
            .and_then(|end| end.checked_mul(NS))
            .ok_or("CPU history context clock overflow")?;
        if self.first_ns != last.saturating_sub(899) * NS {
            return Err("CPU history has a different context start".into());
        }

        if self.end_ns != end_ns {
            return Err("CPU history has a different context end".into());
        }

        Ok(())
    }

    fn validate_page(&self) -> Result<(), String> {
        if self.total > CPUS_MAX {
            return Err("CPU history domain exceeds 1024".into());
        }

        if !self.offset.is_multiple_of(PAGE_CPUS) {
            return Err("CPU history page is not aligned to 32 CPUs".into());
        }

        if self.total == 0 {
            if self.offset != 0 {
                return Err("Empty CPU history has a nonzero page".into());
            }

            if !self.cpus.is_empty() {
                return Err("Empty CPU history contains rows".into());
            }
        } else {
            if self.offset >= self.total {
                return Err("CPU history page lies beyond its domain".into());
            }

            let count = usize::try_from(PAGE_CPUS.min(self.total - self.offset))
                .map_err(|error| error.to_string())?;
            if self.cpus.len() != count {
                return Err("CPU history page has a different row count".into());
            }
        }

        Ok(())
    }

    fn validate_observations(&mut self, selection: &Value) -> Result<(), String> {
        let trends = selection["trends"]
            .as_array()
            .ok_or("Missing selected CPU context")?;
        // The charted readings reach a lead-in before the context, which the history does not
        // hold.
        let held = trends.partition_point(|trend| {
            trend["wall_ns"]
                .as_u64()
                .is_some_and(|wall_ns| wall_ns < self.first_ns)
        });
        let trends = &trends[held..];
        if self.observations.is_empty() {
            return Err("CPU history context is empty".into());
        }

        if trends.len() != self.observations.len() {
            return Err("CPU history context differs from the selected context".into());
        }

        let mut previous = None;
        let mut positions = BTreeMap::new();
        let mut selected = None;
        for (index, (sample, trend)) in self.observations.iter().zip(trends).enumerate() {
            let point = &sample.observation;
            point.validate()?;
            point.matches(trend)?;
            if !(self.first_ns..self.end_ns).contains(&point.wall_ns) {
                return Err("CPU history observation lies outside the context".into());
            }

            if positions.insert(point.identity(), index).is_some() {
                return Err("CPU history contains duplicate identities".into());
            }

            if previous.is_some_and(|previous| previous >= point.order()) {
                return Err("CPU history observations are not ordered".into());
            }

            previous = Some(point.order());
            if sample.cpu_count > CPUS_MAX {
                return Err("CPU history observation domain exceeds 1024".into());
            }

            if point.identity() == self.observation.identity() {
                if *point != self.observation {
                    return Err("Selected CPU history observation has changed metadata".into());
                }

                if sample.cpu_count != self.total {
                    return Err("Selected CPU history domain differs from the page".into());
                }

                selected = Some(index);
            }
        }
        self.selected_index = selected.ok_or("Selected CPU observation is missing from history")?;

        Ok(())
    }

    fn validate_intervals(&self) -> Result<(), String> {
        let positions = self
            .observations
            .iter()
            .map(|sample| (sample.observation.identity(), sample))
            .collect::<BTreeMap<_, _>>();
        for sample in &self.observations {
            if let Some(previous) = &sample.previous_observation {
                previous.validate()?;
            }
            if sample.interval_available {
                sample.validate_interval()?;
                let previous = sample
                    .previous_observation
                    .as_ref()
                    .ok_or("Available CPU history interval lacks a predecessor")?;
                if let Some(known) = positions.get(&previous.identity()) {
                    if known.cpu_count != sample.cpu_count {
                        return Err("CPU history interval crosses different CPU domains".into());
                    }

                    if known.observation != *previous {
                        return Err("CPU history interval has a changed predecessor".into());
                    }
                }
            }
        }

        Ok(())
    }

    fn validate_rows(&self) -> Result<(), String> {
        for (index, row) in self.cpus.iter().enumerate() {
            let index = u32::try_from(index).map_err(|error| error.to_string())?;
            if row.cpu != self.offset + index {
                return Err("CPU history rows differ from the requested CPU indices".into());
            }

            for (value, sample) in row.busy_percent.iter().zip(&self.observations) {
                if let Some(value) = value {
                    if !value.is_finite() {
                        return Err("CPU history percentage is non-finite".into());
                    }

                    if !(0.0..=100.0).contains(value) {
                        return Err("CPU history percentage lies outside zero to 100".into());
                    }

                    if !sample.interval_available {
                        return Err(
                            "Unavailable CPU history interval contains a measurement".into()
                        );
                    }

                    if row.cpu >= sample.cpu_count {
                        return Err(
                            "CPU history measurement lies outside its observation domain".into(),
                        );
                    }
                }
            }
        }

        Ok(())
    }
}

impl CPUObservation {
    fn validate(&self) -> Result<(), String> {
        for text in [&self.run, &self.boot] {
            if text.len() != 32 {
                return Err("CPU history identity has an invalid length".into());
            }

            if !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err("CPU history identity is not hexadecimal".into());
            }
        }

        Ok(())
    }

    fn matches(&self, value: &Value) -> Result<(), String> {
        if value["run"].as_str() != Some(self.run.as_str()) {
            return Err("CPU history run does not match its context".into());
        }

        if value["boot"].as_str() != Some(self.boot.as_str()) {
            return Err("CPU history boot does not match its context".into());
        }

        if value["publication"].as_u64() != Some(self.publication) {
            return Err("CPU history publication does not match its context".into());
        }

        if value["run_tick"].as_u64() != Some(self.run_tick) {
            return Err("CPU history run tick does not match its context".into());
        }

        if value["wall_ns"].as_u64() != Some(self.wall_ns) {
            return Err("CPU history wall clock does not match its context".into());
        }

        Ok(())
    }

    fn identity(&self) -> (&str, &str, u64) {
        (&self.run, &self.boot, self.publication)
    }

    fn order(&self) -> (u64, (&str, &str, u64)) {
        (self.wall_ns, self.identity())
    }
}

impl CPUSample {
    fn validate_interval(&self) -> Result<(), String> {
        if self.cpu_count == 0 {
            return Err("Available CPU history interval has no CPUs".into());
        }

        let before = self
            .previous_observation
            .as_ref()
            .ok_or("Available CPU history interval lacks a predecessor")?;
        let after = &self.observation;
        if before.run != after.run {
            return Err("CPU history interval crosses runs".into());
        }

        if before.boot != after.boot {
            return Err("CPU history interval crosses boots".into());
        }

        if before.publication.checked_add(1) != Some(after.publication) {
            return Err("CPU history interval has nonconsecutive publications".into());
        }

        if before.run_tick.checked_add(1) != Some(after.run_tick) {
            return Err("CPU history interval has nonconsecutive ticks".into());
        }

        if before.wall_ns >= after.wall_ns {
            return Err("CPU history interval clock does not advance".into());
        }

        if after.wall_ns / NS > (before.wall_ns / NS).saturating_add(1) {
            return Err("CPU history interval crosses an unavailable second".into());
        }

        Ok(())
    }
}
