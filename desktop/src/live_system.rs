//! Each App's GPU time, which the window still samples once a second while it runs and
//! keeps for the last 16 minutes, so a second before the app started, or one it missed, has
//! no reading. Every other reading of the Mac comes from the collector through the viewer.

use std::collections::VecDeque;

use serde_json::{Value, json};

/// The samples kept, a minute past the 15 the charts show so the window's first second
/// still has one.
pub const SAMPLES_MAX: usize = 16 * 60;

/// One second's GPU time by App, none where macOS refused it.
#[derive(Clone, Debug, PartialEq)]
pub struct SystemSample {
    pub second: u64,
    pub gpu_apps: Option<crate::live_gpu_apps::GpuAppSample>,
}

/// The samples of the last 16 minutes, oldest first, one per second.
pub struct SystemSamples {
    samples: VecDeque<SystemSample>,
    gpu_apps: Option<crate::live_gpu_apps::GpuAppSampler>,
    error_last: Option<String>,
}

impl SystemSamples {
    pub fn new() -> Self {
        let mut samples = Self::unread();
        samples.gpu_apps = match crate::live_gpu_apps::GpuAppSampler::new() {
            Ok(gpu_apps) => Some(gpu_apps),
            Err(error) => {
                eprintln!("flightdeck GPU time by App unreadable: {error}");
                None
            }
        };
        samples
    }

    /// Samples that read nothing, for tests that compare a recording's selection as the
    /// viewer returned it.
    pub fn unread() -> Self {
        Self {
            samples: VecDeque::with_capacity(SAMPLES_MAX),
            gpu_apps: None,
            error_last: None,
        }
    }

    /// Samples this second unless it already has a sample.
    pub fn sample(&mut self, second: u64) {
        if self
            .samples
            .back()
            .is_some_and(|last| last.second >= second)
        {
            return;
        }
        let Some(sampler) = &mut self.gpu_apps else {
            return;
        };
        let (gpu_apps, error) = match sampler.sample() {
            Ok(gpu_apps) => (gpu_apps, None),
            Err(error) => (None, Some(error)),
        };
        if error != self.error_last {
            if let Some(error) = &error {
                eprintln!("flightdeck GPU time by App: {error}");
            }
            self.error_last = error;
        }
        self.record(SystemSample { second, gpu_apps });
    }

    /// Keeps a sample, dropping the oldest past the bound. Seconds only move forward.
    pub fn record(&mut self, sample: SystemSample) {
        if let Some(last) = self.samples.back() {
            assert!(last.second < sample.second);
        }
        if self.samples.len() == SAMPLES_MAX {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
        assert!(self.samples.len() <= SAMPLES_MAX);
    }

    /// Adds the sampled GPU time by App to the selection, matched by the second of each
    /// point's recorded time. A point with no sample of its second gets no reading, never a
    /// neighbour's.
    pub fn annotate(&self, selection: &mut Value) {
        crate::live_gpu_apps::annotate(selection, |second| {
            self.sample_at(second)
                .and_then(|sample| sample.gpu_apps.as_ref())
        });
    }

    /// Adds the selected App's GPU time to each point of its history as `gpu_percent`,
    /// matched by the second of the point's observation. A point with no GPU sample of its
    /// second gets no reading, never a neighbour's.
    pub fn annotate_app(&self, history: &mut Value, app: &crate::live_source::AppKey) {
        let Some(points) = history.get_mut("points").and_then(Value::as_array_mut) else {
            return;
        };
        for point in points
            .iter_mut()
            .take(crate::live_projection::OBSERVATIONS_MAX)
        {
            let Ok(wall_ns) = crate::live_projection::integer(&point["observation"]["wall_ns"])
            else {
                continue;
            };
            let sample = self
                .sample_at(wall_ns / 1_000_000_000)
                .and_then(|sample| sample.gpu_apps.as_ref());
            if let Some(sample) = sample {
                point["gpu_percent"] = json!(crate::live_gpu_apps::app_percent(sample, app));
            }
        }
    }

    fn sample_at(&self, second: u64) -> Option<&SystemSample> {
        let index = self
            .samples
            .binary_search_by_key(&second, |sample| sample.second)
            .ok()?;
        self.samples.get(index)
    }
}

/// The thermal pressure sampled at a reading, by macOS's name, or "Unavailable".
#[must_use]
pub fn thermal_pressure_text(point: &Value) -> &'static str {
    point["thermal_pressure"]
        .as_u64()
        .and_then(thermal_pressure_name)
        .unwrap_or("Unavailable")
}

/// macOS's name for a thermal pressure level, as `OSThermalNotification.h` lists them.
#[must_use]
pub fn thermal_pressure_name(level: u64) -> Option<&'static str> {
    match level {
        0 => Some("Nominal"),
        1 => Some("Moderate"),
        2 => Some("Heavy"),
        3 => Some("Trapping"),
        4 => Some("Sleeping"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(second: u64) -> SystemSample {
        SystemSample {
            second,
            gpu_apps: None,
        }
    }

    #[test]
    fn annotate_app_reads_the_selected_apps_gpu_time_in_each_points_second() {
        // Goal: an App history point takes the selected App's GPU share of its own second,
        // reads zero where the App used no GPU, and stays without a reading where no GPU
        // sample exists.
        // Method: GPU samples at seconds 100 and 102, the App present only in the first, and
        // history points at 100, 101 and 102.
        let selected = std::rc::Rc::new(crate::live_gpu_apps::GpuApp {
            bundle: false,
            bytes: b"A".to_vec(),
        });
        let other = std::rc::Rc::new(crate::live_gpu_apps::GpuApp {
            bundle: false,
            bytes: b"B".to_vec(),
        });
        let time = |app: &std::rc::Rc<crate::live_gpu_apps::GpuApp>, gpu_ns: u64| {
            crate::live_gpu_apps::GpuAppTime {
                app: std::rc::Rc::clone(app),
                gpu_ns,
                processes: 1,
            }
        };
        let mut samples = SystemSamples::unread();
        let mut first = sample(100);
        first.gpu_apps = Some(crate::live_gpu_apps::GpuAppSample {
            elapsed_ns: 1_000_000_000,
            apps: vec![time(&selected, 250_000_000), time(&other, 500_000_000)],
        });
        samples.record(first);
        let mut last = sample(102);
        last.gpu_apps = Some(crate::live_gpu_apps::GpuAppSample {
            elapsed_ns: 1_000_000_000,
            apps: vec![time(&other, 500_000_000)],
        });
        samples.record(last);
        let mut history = json!({"points": [
            {"observation": {"wall_ns": "100500000000"}},
            {"observation": {"wall_ns": "101000000000"}},
            {"observation": {"wall_ns": "102900000000"}},
        ]});
        let key = crate::live_source::AppKey {
            kind: "executable".into(),
            bytes_hex: "41".into(),
        };

        samples.annotate_app(&mut history, &key);

        let points = &history["points"];
        assert_eq!(points[0]["gpu_percent"], json!(25.0));
        assert_eq!(points[1]["gpu_percent"], Value::Null);
        assert_eq!(points[2]["gpu_percent"], json!(0.0));
    }

    #[test]
    fn record_keeps_the_newest_samples_within_the_bound() {
        // Goal: the buffer never grows past its bound and drops the oldest second first.
        // Method: record one sample past the bound.
        let mut samples = SystemSamples::unread();
        let total = u64::try_from(SAMPLES_MAX).unwrap() + 1;

        for second in 0..total {
            samples.record(sample(second));
        }

        assert_eq!(samples.samples.len(), SAMPLES_MAX);
        assert_eq!(samples.samples.front().map(|sample| sample.second), Some(1));
    }

    #[test]
    fn thermal_pressure_names_follow_the_header() {
        // Goal: each level macOS defines reads by its name and an unknown one by none.
        // Method: every defined level and the first past them.
        let names = [0, 1, 2, 3, 4, 5].map(thermal_pressure_name);

        assert_eq!(
            names,
            [
                Some("Nominal"),
                Some("Moderate"),
                Some("Heavy"),
                Some("Trapping"),
                Some("Sleeping"),
                None
            ]
        );
    }
}
