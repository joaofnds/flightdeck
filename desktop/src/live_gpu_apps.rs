//! GPU time by App, which the collector does not record. The app reads each process's GPU
//! time from the GPU driver once a second while it runs and groups the processes into Apps by
//! the collector's own rule, so time before the app started has no reading.

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use collector::gpu_clients::{GpuClientTime, GpuClients};
use collector::ring::ProcessCreation;
use serde_json::{Value, json};

use crate::live_rankings::{App, Entry, Metric, Ranking};
use crate::live_source::AppKey;

/// The Apps the chart draws on their own, the rest summed as Other, as the other App charts do.
pub const COHORT_MAX: usize = 8;

/// The Apps the ranking keeps at one second, past the few dozen that use the GPU at once.
const RANKED_MAX: usize = 64;

/// An App as the collector groups processes: the outermost bundle a process runs from, or its
/// executable name outside any bundle.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GpuApp {
    pub bundle: bool,
    pub bytes: Vec<u8>,
}

/// Each App's GPU time over one interval between readings, the most first.
#[derive(Clone, Debug, PartialEq)]
pub struct GpuAppSample {
    pub elapsed_ns: u64,
    pub apps: Vec<GpuAppTime>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct GpuAppTime {
    pub app: Rc<GpuApp>,
    pub gpu_ns: u64,
    pub processes: u32,
}

/// One process the last reading saw, kept to measure its GPU time over the next interval.
struct Tracked {
    creation: ProcessCreation,
    gpu_ns: u64,
    app: Option<Rc<GpuApp>>,
}

pub struct GpuAppSampler {
    clients: GpuClients,
    times: Vec<GpuClientTime>,
    tracked: HashMap<i32, Tracked>,
    next: HashMap<i32, Tracked>,
    read_at: Option<Instant>,
    path: Box<[u8; 4096]>,
}

impl GpuApp {
    /// The App's name as the rankings list it, without the bundle's `.app` extension.
    pub fn name(&self) -> String {
        let name = if self.bundle {
            self.bytes
                .rsplit(|byte| *byte == b'/')
                .next()
                .unwrap_or(&self.bytes)
        } else {
            &self.bytes
        };
        let name = String::from_utf8_lossy(name);
        name.strip_suffix(".app").unwrap_or(&name).to_owned()
    }

    pub fn key(&self) -> AppKey {
        let mut bytes_hex = String::with_capacity(self.bytes.len() * 2);
        for byte in &self.bytes {
            for nibble in [byte >> 4, byte & 0x0f] {
                bytes_hex.push(char::from(b"0123456789abcdef"[usize::from(nibble)]));
            }
        }
        AppKey {
            kind: if self.bundle { "bundle" } else { "executable" }.into(),
            bytes_hex,
        }
    }
}

impl GpuAppSampler {
    pub fn new() -> std::io::Result<Self> {
        Ok(Self {
            clients: GpuClients::new()?,
            times: Vec::with_capacity(collector::gpu_clients::CLIENTS_MAX),
            tracked: HashMap::new(),
            next: HashMap::new(),
            read_at: None,
            path: Box::new([0; 4096]),
        })
    }

    /// Reads each process's GPU time and returns each App's share of it since the last read,
    /// or none on the first read. A process seen for the first time, one whose pid now names
    /// another process, and one whose time fell because a client closed, add nothing to this
    /// interval and are measured from this read on.
    pub fn sample(&mut self) -> Result<Option<GpuAppSample>, String> {
        self.clients
            .read(&mut self.times)
            .map_err(|error| format!("GPU clients unreadable: {error}"))?;
        let now = Instant::now();
        let elapsed = self.read_at.map(|at| now.duration_since(at));
        self.read_at = Some(now);

        let mut gains: BTreeMap<Rc<GpuApp>, (u64, u32)> = BTreeMap::new();
        self.next.clear();
        for time in &self.times {
            let Some(creation) = creation(time.pid) else {
                continue;
            };
            let previous = self
                .tracked
                .remove(&time.pid)
                .filter(|tracked| tracked.creation == creation);
            let app = match &previous {
                Some(tracked) => tracked.app.clone(),
                None => app_of(time.pid, &mut self.path).map(Rc::new),
            };
            if let Some((previous, app)) = previous.as_ref().zip(app.as_ref()) {
                if let Some(gain) = time.gpu_ns.checked_sub(previous.gpu_ns) {
                    let total = gains.entry(Rc::clone(app)).or_default();
                    total.0 = total.0.saturating_add(gain);
                    total.1 += 1;
                }
            }
            let tracked = Tracked {
                creation,
                gpu_ns: time.gpu_ns,
                app,
            };
            self.next.insert(time.pid, tracked);
        }
        std::mem::swap(&mut self.tracked, &mut self.next);

        let Some(elapsed) = elapsed else {
            return Ok(None);
        };
        let elapsed_ns = u64::try_from(elapsed.as_nanos()).map_err(|error| error.to_string())?;
        if elapsed_ns == 0 {
            return Ok(None);
        }
        let mut apps: Vec<GpuAppTime> = gains
            .into_iter()
            .map(|(app, (gpu_ns, processes))| GpuAppTime {
                app,
                gpu_ns,
                processes,
            })
            .collect();
        apps.sort_by(|left, right| {
            right
                .gpu_ns
                .cmp(&left.gpu_ns)
                .then_with(|| left.app.cmp(&right.app))
        });
        Ok(Some(GpuAppSample { elapsed_ns, apps }))
    }
}

/// The identity macOS gives the process `pid` now, or none where it is gone or refused.
fn creation(pid: i32) -> Option<ProcessCreation> {
    let mut creation = Err(collector::programs::CreationError::NotRead);
    collector::programs::read_creation(pid, false, &mut creation);
    creation.ok()
}

/// The App of the process `pid`, or none where its program path is unreadable.
fn app_of(pid: i32, path: &mut [u8; 4096]) -> Option<GpuApp> {
    let length = collector::programs::read_path(pid, path).ok()?;
    let mut program = None;
    collector::programs::split_program(&path[..length], &mut program).ok()?;
    let program = program?;
    Some(match program.app_path {
        Some(bundle) => GpuApp {
            bundle: true,
            bytes: bundle.to_vec(),
        },
        None => GpuApp {
            bundle: false,
            bytes: program.executable_name.to_vec(),
        },
    })
}

/// Adds each App's GPU share to the trend points and the selected summary. The chart's
/// leaders are the eight Apps with the most GPU time over the points the app sampled, written
/// as `gpu_app_cohort`, and each sampled point gets `gpu_app_percent`, Other first and then
/// each leader. The summary gets every App of its second as `gpu_apps`, the most first.
pub fn annotate<'samples>(
    selection: &mut Value,
    sample_at: impl Fn(u64) -> Option<&'samples GpuAppSample>,
) {
    let mut totals: BTreeMap<&GpuApp, u64> = BTreeMap::new();
    if let Some(points) = selection.get("trends").and_then(Value::as_array) {
        for point in points.iter().take(crate::live_projection::OBSERVATIONS_MAX) {
            let Some(sample) = point_second(point).and_then(&sample_at) else {
                continue;
            };
            for time in &sample.apps {
                let total = totals.entry(time.app.as_ref()).or_default();
                *total = total.saturating_add(time.gpu_ns);
            }
        }
    }
    let mut leaders: Vec<(&GpuApp, u64)> = totals.into_iter().collect();
    leaders.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(right.0)));
    leaders.truncate(COHORT_MAX);
    let cohort: Vec<GpuApp> = leaders.iter().map(|(app, _)| (*app).clone()).collect();

    if let Some(points) = selection.get_mut("trends").and_then(Value::as_array_mut) {
        for point in points
            .iter_mut()
            .take(crate::live_projection::OBSERVATIONS_MAX)
        {
            if let Some(sample) = point_second(point).and_then(&sample_at) {
                point["gpu_app_percent"] = json!(cohort_percent(sample, &cohort));
            }
        }
    }
    if selection.is_object() {
        let names: Vec<String> = cohort.iter().map(GpuApp::name).collect();
        selection["gpu_app_cohort"] = json!(names);
    }
    if let Some(summary) = selection.get_mut("summary") {
        if summary.is_object() {
            if let Some(sample) = point_second(summary).and_then(&sample_at) {
                summary["gpu_apps"] = summary_apps(sample);
            }
        }
    }
}

/// One App's GPU time over the sample's interval, as a percent of it. An App the driver lists
/// no GPU client for used no GPU time, so it reads zero.
pub fn app_percent(sample: &GpuAppSample, app: &AppKey) -> f64 {
    let mut gpu_ns: u64 = 0;
    for time in &sample.apps {
        if time.app.key() == *app {
            gpu_ns = gpu_ns.saturating_add(time.gpu_ns);
        }
    }
    percent(gpu_ns, sample.elapsed_ns)
}

fn point_second(point: &Value) -> Option<u64> {
    point["wall_ns"].as_u64().map(|wall| wall / 1_000_000_000)
}

/// Other's share and then each leader's, as a percent of the interval.
fn cohort_percent(sample: &GpuAppSample, cohort: &[GpuApp]) -> Vec<f64> {
    let mut gpu_ns = vec![0_u64; cohort.len() + 1];
    for time in &sample.apps {
        let slot = cohort
            .iter()
            .position(|app| app == time.app.as_ref())
            .map_or(0, |index| index + 1);
        gpu_ns[slot] = gpu_ns[slot].saturating_add(time.gpu_ns);
    }
    gpu_ns
        .into_iter()
        .map(|gpu_ns| percent(gpu_ns, sample.elapsed_ns))
        .collect()
}

fn summary_apps(sample: &GpuAppSample) -> Value {
    let apps: Vec<Value> = sample
        .apps
        .iter()
        .take(RANKED_MAX)
        .map(|time| {
            let key = time.app.key();
            json!({
                "name": time.app.name(),
                "kind": key.kind,
                "bytes_hex": key.bytes_hex,
                "gpu_ns": time.gpu_ns,
                "processes": time.processes,
                "percent": percent(time.gpu_ns, sample.elapsed_ns),
            })
        })
        .collect();
    json!(apps)
}

fn percent(gpu_ns: u64, elapsed_ns: u64) -> f64 {
    assert!(elapsed_ns > 0);
    let gpu_ns = num_traits::ToPrimitive::to_f64(&gpu_ns).unwrap_or(f64::MAX);
    let elapsed_ns = num_traits::ToPrimitive::to_f64(&elapsed_ns).unwrap_or(f64::MAX);
    gpu_ns / elapsed_ns * 100.0
}

/// The GPU ranking of the selected second from its summary, or an error naming why it has
/// none: a second before the app started, or one it did not sample.
pub fn ranking(selection: &Value) -> Result<Ranking, String> {
    let apps = selection["summary"]["gpu_apps"]
        .as_array()
        .ok_or("GPU time by App is sampled only while the app runs")?;
    let mut entries = Vec::with_capacity(apps.len().min(RANKED_MAX));
    for app in apps.iter().take(RANKED_MAX) {
        let key = crate::live_source::app_key_from_json(&json!({ "key": app }))?;
        let name = app["name"].as_str().ok_or("GPU App name unavailable")?;
        let amount = app["gpu_ns"].as_u64().ok_or("GPU App time unavailable")?;
        let rate = app["percent"].as_f64().ok_or("GPU App share unavailable")?;
        if rate <= 0.0 {
            continue;
        }
        let processes = app["processes"]
            .as_u64()
            .ok_or("GPU App processes unavailable")?;
        entries.push(Entry {
            app: Arc::new(App {
                key,
                name: name.into(),
            }),
            amount,
            rate: Some(rate),
            measured_members: u32::try_from(processes).map_err(|error| error.to_string())?,
            unavailable_members: 0,
        });
    }
    Ok(Ranking {
        metric: Metric::Gpu,
        entries,
        unavailable: 0,
        overflowed: 0,
        partial: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str) -> Rc<GpuApp> {
        Rc::new(GpuApp {
            bundle: false,
            bytes: name.as_bytes().to_vec(),
        })
    }

    fn sample(apps: &[(&Rc<GpuApp>, u64)]) -> GpuAppSample {
        GpuAppSample {
            elapsed_ns: 1_000_000_000,
            apps: apps
                .iter()
                .map(|(app, gpu_ns)| GpuAppTime {
                    app: Rc::clone(app),
                    gpu_ns: *gpu_ns,
                    processes: 1,
                })
                .collect(),
        }
    }

    #[test]
    fn annotate_ranks_leaders_over_the_window_and_leaves_unsampled_seconds_empty() {
        // Goal: the leaders are the Apps with the most GPU time over the sampled points, each
        // sampled point carries Other and each leader's share, and an unsampled second has
        // none rather than a neighbour's.
        // Method: nine Apps over seconds 100 and 102, the ninth least, with second 101 unsampled.
        let apps: Vec<Rc<GpuApp>> = (0..9).map(|index| app(&format!("app{index}"))).collect();
        let first: Vec<(&Rc<GpuApp>, u64)> = apps
            .iter()
            .zip((1..=9).rev())
            .map(|(app, tenths)| (app, tenths * 10_000_000))
            .collect();
        let samples = [
            (100, sample(&first)),
            (102, sample(&[(&apps[0], 500_000_000)])),
        ];
        let mut selection = json!({
            "trends": [
                {"wall_ns": 100_000_000_000_u64},
                {"wall_ns": 101_000_000_000_u64},
                {"wall_ns": 102_500_000_000_u64},
            ],
            "summary": {"wall_ns": 100_000_000_000_u64},
        });

        annotate(&mut selection, |second| {
            samples
                .iter()
                .find(|(at, _)| *at == second)
                .map(|(_, sample)| sample)
        });

        let cohort: Vec<String> = (0..8).map(|index| format!("app{index}")).collect();
        assert_eq!(selection["gpu_app_cohort"], json!(cohort));
        let first = selection["trends"][0]["gpu_app_percent"]
            .as_array()
            .unwrap();
        assert_eq!(first.len(), 9);
        assert!((first[0].as_f64().unwrap() - 1.0).abs() < 1e-9);
        assert!((first[1].as_f64().unwrap() - 9.0).abs() < 1e-9);
        assert_eq!(selection["trends"][1]["gpu_app_percent"], Value::Null);
        let last = selection["trends"][2]["gpu_app_percent"]
            .as_array()
            .unwrap();
        assert!((last[1].as_f64().unwrap() - 50.0).abs() < 1e-9);
        assert_eq!(last[0], json!(0.0));
        assert_eq!(
            selection["summary"]["gpu_apps"].as_array().unwrap().len(),
            9
        );
    }

    #[test]
    fn ranking_reads_the_summary_apps_or_names_why_it_has_none() {
        // Goal: the ranking lists the selected second's Apps with their share and key, and a
        // second the app did not sample says so instead of ranking nothing as zero.
        // Method: annotate one sampled second, rank it, then rank an unsampled selection.
        let browser = Rc::new(GpuApp {
            bundle: true,
            bytes: b"/Applications/Brave Browser.app".to_vec(),
        });
        let samples = sample(&[(&browser, 250_000_000)]);
        let mut selection = json!({"summary": {"wall_ns": 7_000_000_000_u64}});
        annotate(&mut selection, |_| Some(&samples));

        let ranking = ranking(&selection).unwrap();

        assert_eq!(ranking.entries.len(), 1);
        assert_eq!(ranking.entries[0].app.name, "Brave Browser");
        assert_eq!(ranking.entries[0].app.key.kind, "bundle");
        assert_eq!(ranking.entries[0].rate, Some(25.0));
        assert!(super::ranking(&json!({"summary": {}})).is_err());
    }
}
