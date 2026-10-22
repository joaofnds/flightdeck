//! Readings the collector does not record: load average, open files, thermal pressure, the
//! GPU's unified memory and each App's GPU time. The app samples them once a second while it
//! runs and keeps the last 16 minutes, so a second before the app started, or one it missed,
//! has no reading.

use std::collections::VecDeque;
use std::ffi::{CStr, c_char, c_int};

use collector::gpu_memory::{GpuMemory, GpuMemoryReading};
use serde_json::{Value, json};

/// The samples kept, a minute past the 15 the charts show so the window's first second
/// still has one.
pub const SAMPLES_MAX: usize = 16 * 60;

/// One second's readings, each none where macOS refused it.
#[derive(Clone, Debug, PartialEq)]
pub struct SystemSample {
    pub second: u64,
    pub load_average: Option<[f64; 3]>,
    pub logical_cpus: Option<u32>,
    pub open_files: Option<OpenFiles>,
    pub thermal_pressure: Option<u64>,
    pub gpu_memory: Option<GpuMemoryReading>,
    pub gpu_apps: Option<crate::live_gpu_apps::GpuAppSample>,
}

/// The file entries open across the system and the most macOS allows.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OpenFiles {
    pub count: u64,
    pub limit: u64,
}

/// The samples of the last 16 minutes, oldest first, one per second.
pub struct SystemSamples {
    samples: VecDeque<SystemSample>,
    boot_second: Option<u64>,
    thermal: Option<ThermalPressure>,
    gpu: Option<GpuMemory>,
    gpu_apps: Option<crate::live_gpu_apps::GpuAppSampler>,
    error_last: Option<String>,
}

impl SystemSamples {
    pub fn new() -> Self {
        let boot_second = match boot_second() {
            Ok(second) => Some(second),
            Err(error) => {
                eprintln!("flightdeck boot time unreadable: {error}");
                None
            }
        };
        let thermal = match ThermalPressure::register() {
            Ok(thermal) => Some(thermal),
            Err(error) => {
                eprintln!("flightdeck thermal pressure unreadable: {error}");
                None
            }
        };
        let gpu = match GpuMemory::open() {
            Ok(gpu) => Some(gpu),
            Err(error) => {
                eprintln!("flightdeck GPU memory unreadable: {error}");
                None
            }
        };
        let gpu_apps = match crate::live_gpu_apps::GpuAppSampler::new() {
            Ok(sampler) => Some(sampler),
            Err(error) => {
                eprintln!("flightdeck GPU time by App unreadable: {error}");
                None
            }
        };
        let mut samples = Self::with_boot(boot_second, thermal);
        samples.gpu = gpu;
        samples.gpu_apps = gpu_apps;
        samples
    }

    /// Samples that know no boot time and read no thermal pressure, for tests that compare
    /// a recording's selection as the viewer returned it.
    #[cfg(test)]
    pub fn unread() -> Self {
        Self::with_boot(None, None)
    }

    fn with_boot(boot_second: Option<u64>, thermal: Option<ThermalPressure>) -> Self {
        Self {
            samples: VecDeque::with_capacity(SAMPLES_MAX),
            boot_second,
            thermal,
            gpu: None,
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
        let (mut sample, mut errors) =
            sample_read(second, self.thermal.as_ref(), self.gpu.as_ref());
        if let Some(sampler) = &mut self.gpu_apps {
            sample.gpu_apps = sample_read_keep(sampler.sample(), &mut errors).flatten();
        }
        let error = (!errors.is_empty()).then(|| errors.join("; "));
        if error != self.error_last {
            if let Some(error) = &error {
                eprintln!("flightdeck system readings: {error}");
            }
            self.error_last = error;
        }
        self.record(sample);
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

    /// Adds the sampled readings to each trend point and to the selected summary, matched
    /// by the second of their recorded time. A point with no sample of its second gets no
    /// reading, never a neighbour's.
    pub fn annotate(&self, selection: &mut Value) {
        // Reading through `get_mut` leaves a selection without trends or a summary as it
        // was, where indexing would insert them.
        if let Some(points) = selection.get_mut("trends").and_then(Value::as_array_mut) {
            for point in points
                .iter_mut()
                .take(crate::live_projection::OBSERVATIONS_MAX)
            {
                self.annotate_point(point);
            }
        }
        if let Some(summary) = selection.get_mut("summary") {
            if summary.is_object() {
                self.annotate_point(summary);
            }
        }
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

    fn annotate_point(&self, point: &mut Value) {
        let Some(second) = point["wall_ns"].as_u64().map(|wall| wall / 1_000_000_000) else {
            return;
        };
        if let Some(boot) = self.boot_second {
            if boot <= second {
                point["uptime_seconds"] = json!(second - boot);
            }
        }
        let Some(sample) = self.sample_at(second) else {
            return;
        };
        if let Some(load) = sample.load_average {
            point["load_average"] = json!(load);
        }
        if let Some(count) = sample.logical_cpus {
            point["logical_cpu_count"] = json!(count);
        }
        if let Some(files) = sample.open_files {
            point["open_files"] = json!(files.count);
            point["open_files_limit"] = json!(files.limit);
            if let Some(share) = files.share_percent() {
                point["open_files_percent"] = json!(share);
            }
        }
        if let Some(level) = sample.thermal_pressure {
            point["thermal_pressure"] = json!(level);
        }
        if let Some(gpu) = sample.gpu_memory {
            if let Some(bytes) = gpu.in_use_bytes {
                point["gpu_memory_in_use_bytes"] = json!(bytes);
            }
            if let Some(bytes) = gpu.allocated_bytes {
                point["gpu_memory_allocated_bytes"] = json!(bytes);
            }
        }
    }
}

impl OpenFiles {
    /// The share of the limit in use, or none where macOS reports no limit.
    pub fn share_percent(self) -> Option<f64> {
        if self.limit == 0 {
            return None;
        }
        let count = num_traits::ToPrimitive::to_f64(&self.count)?;
        let limit = num_traits::ToPrimitive::to_f64(&self.limit)?;
        Some(count / limit * 100.0)
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

fn sample_read(
    second: u64,
    thermal: Option<&ThermalPressure>,
    gpu: Option<&GpuMemory>,
) -> (SystemSample, Vec<String>) {
    let mut errors = Vec::new();
    let load_average = sample_read_keep(load_average(), &mut errors);
    let logical_cpus = sample_read_keep(logical_cpus(), &mut errors);
    let open_files = sample_read_keep(open_files(), &mut errors);
    let thermal_pressure =
        thermal.and_then(|thermal| sample_read_keep(thermal.level(), &mut errors));
    let gpu_memory = gpu.and_then(GpuMemory::read);
    let sample = SystemSample {
        second,
        load_average,
        logical_cpus,
        open_files,
        thermal_pressure,
        gpu_memory,
        gpu_apps: None,
    };
    (sample, errors)
}

/// The reading, or none with its error kept for the report.
fn sample_read_keep<T>(result: Result<T, String>, errors: &mut Vec<String>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(error) => {
            errors.push(error);
            None
        }
    }
}

fn load_average() -> Result<[f64; 3], String> {
    let mut load = [0.0_f64; 3];
    let count = unsafe { libc::getloadavg(load.as_mut_ptr(), 3) };
    if count != 3 {
        return Err(format!("getloadavg returned {count} of 3 averages"));
    }
    for average in load {
        if !average.is_finite() {
            return Err("getloadavg returned a nonfinite average".into());
        }
        if average < 0.0 {
            return Err("getloadavg returned a negative average".into());
        }
    }
    Ok(load)
}

fn logical_cpus() -> Result<u32, String> {
    let count = sysctl_int(c"hw.logicalcpu")?;
    u32::try_from(count).map_err(|error| format!("hw.logicalcpu {count}: {error}"))
}

fn open_files() -> Result<OpenFiles, String> {
    let count = sysctl_int(c"kern.num_files")?;
    let limit = sysctl_int(c"kern.maxfiles")?;
    Ok(OpenFiles {
        count: u64::try_from(count).map_err(|error| format!("kern.num_files: {error}"))?,
        limit: u64::try_from(limit).map_err(|error| format!("kern.maxfiles: {error}"))?,
    })
}

fn boot_second() -> Result<u64, String> {
    let mut value = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut count = std::mem::size_of::<libc::timeval>();
    sysctl_read(
        c"kern.boottime",
        std::ptr::from_mut(&mut value).cast(),
        &mut count,
    )?;
    if count != std::mem::size_of::<libc::timeval>() {
        return Err(format!("kern.boottime returned {count} bytes"));
    }
    u64::try_from(value.tv_sec).map_err(|error| format!("kern.boottime: {error}"))
}

fn sysctl_int(key: &CStr) -> Result<c_int, String> {
    let mut value: c_int = 0;
    let mut count = std::mem::size_of::<c_int>();
    sysctl_read(key, std::ptr::from_mut(&mut value).cast(), &mut count)?;
    if count != std::mem::size_of::<c_int>() {
        return Err(format!("{} returned {count} bytes", key.to_string_lossy()));
    }
    Ok(value)
}

fn sysctl_read(key: &CStr, buffer: *mut libc::c_void, count: &mut usize) -> Result<(), String> {
    // Each caller supplies initialized storage and its exact byte capacity.
    let status =
        unsafe { libc::sysctlbyname(key.as_ptr(), buffer, count, std::ptr::null_mut(), 0) };
    if status != 0 {
        return Err(format!(
            "{}: {}",
            key.to_string_lossy(),
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// A registration for macOS's thermal pressure notification, whose state is the level.
struct ThermalPressure {
    token: c_int,
}

const NOTIFY_STATUS_OK: u32 = 0;

unsafe extern "C" {
    fn notify_register_check(name: *const c_char, out_token: *mut c_int) -> u32;
    fn notify_get_state(token: c_int, state64: *mut u64) -> u32;
    fn notify_cancel(token: c_int) -> u32;
}

impl ThermalPressure {
    fn register() -> Result<Self, String> {
        let mut token: c_int = 0;
        let name = c"com.apple.system.thermalpressurelevel";
        let status = unsafe { notify_register_check(name.as_ptr(), &raw mut token) };
        if status != NOTIFY_STATUS_OK {
            return Err(format!("notify_register_check returned {status}"));
        }
        Ok(Self { token })
    }

    fn level(&self) -> Result<u64, String> {
        let mut state = 0_u64;
        let status = unsafe { notify_get_state(self.token, &raw mut state) };
        if status != NOTIFY_STATUS_OK {
            return Err(format!("notify_get_state returned {status}"));
        }
        if thermal_pressure_name(state).is_none() {
            return Err(format!("unknown thermal pressure level {state}"));
        }
        Ok(state)
    }
}

impl Drop for ThermalPressure {
    fn drop(&mut self) {
        let status = unsafe { notify_cancel(self.token) };
        if status != NOTIFY_STATUS_OK {
            eprintln!("flightdeck thermal pressure cancel returned {status}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(second: u64) -> SystemSample {
        SystemSample {
            second,
            load_average: Some([2.5, 3.0, 3.5]),
            logical_cpus: Some(18),
            open_files: Some(OpenFiles {
                count: 1_000,
                limit: 4_000,
            }),
            thermal_pressure: Some(0),
            gpu_memory: Some(GpuMemoryReading {
                in_use_bytes: Some(1 << 30),
                allocated_bytes: None,
            }),
            gpu_apps: None,
        }
    }

    #[test]
    fn annotate_matches_each_point_to_its_own_second() {
        // Goal: a point takes the readings sampled in its second, and a second the app did
        // not sample stays without them rather than borrowing a neighbour's.
        // Method: samples at seconds 100 and 102, points at 100, 101 and 102.
        let mut samples = SystemSamples::with_boot(Some(40), None);
        samples.record(sample(100));
        samples.record(sample(102));
        let mut selection = json!({
            "trends": [
                {"wall_ns": 100_500_000_000_u64},
                {"wall_ns": 101_000_000_000_u64},
                {"wall_ns": 102_900_000_000_u64},
            ],
            "summary": {"wall_ns": 102_900_000_000_u64},
        });

        samples.annotate(&mut selection);

        let trends = &selection["trends"];
        assert_eq!(trends[0]["load_average"], json!([2.5, 3.0, 3.5]));
        assert_eq!(trends[0]["open_files_percent"], json!(25.0));
        assert_eq!(trends[1]["load_average"], Value::Null);
        assert_eq!(trends[1]["open_files"], Value::Null);
        assert_eq!(trends[1]["uptime_seconds"], json!(61));
        assert_eq!(trends[2]["logical_cpu_count"], json!(18));
        assert_eq!(selection["summary"]["thermal_pressure"], json!(0));
        assert_eq!(trends[0]["gpu_memory_in_use_bytes"], json!(1_u64 << 30));
        assert_eq!(trends[0]["gpu_memory_allocated_bytes"], Value::Null);
        assert_eq!(trends[1]["gpu_memory_in_use_bytes"], Value::Null);
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
        let mut samples = SystemSamples::with_boot(None, None);
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
    fn annotate_leaves_a_selection_without_readings_unchanged() {
        // Goal: a null selection, as diagnostics reads it, stays null.
        // Method: annotate a null selection with one sample kept.
        let mut samples = SystemSamples::with_boot(Some(40), None);
        samples.record(sample(100));
        let mut selection = Value::Null;

        samples.annotate(&mut selection);

        assert_eq!(selection, Value::Null);
    }

    #[test]
    fn annotate_leaves_uptime_unknown_before_this_boot() {
        // Goal: a second from before this boot has no uptime, since it belongs to another.
        // Method: boot at second 200 and a point at second 100.
        let samples = SystemSamples::with_boot(Some(200), None);
        let mut selection = json!({"trends": [{"wall_ns": 100_000_000_000_u64}]});

        samples.annotate(&mut selection);

        assert_eq!(selection["trends"][0]["uptime_seconds"], Value::Null);
    }

    #[test]
    fn record_keeps_the_newest_samples_within_the_bound() {
        // Goal: the buffer never grows past its bound and drops the oldest second first.
        // Method: record one sample past the bound.
        let mut samples = SystemSamples::with_boot(None, None);
        let total = u64::try_from(SAMPLES_MAX).unwrap() + 1;

        for second in 0..total {
            samples.record(sample(second));
        }

        assert_eq!(samples.samples.len(), SAMPLES_MAX);
        assert_eq!(samples.samples.front().map(|sample| sample.second), Some(1));
    }

    #[test]
    fn open_files_share_needs_a_limit() {
        // Goal: a zero limit leaves the share unknown rather than infinite.
        // Method: one reading with a limit and one without.
        let limited = OpenFiles {
            count: 10,
            limit: 40,
        };
        let unlimited = OpenFiles {
            count: 10,
            limit: 0,
        };

        assert_eq!(limited.share_percent(), Some(25.0));
        assert_eq!(unlimited.share_percent(), None);
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
