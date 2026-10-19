use std::io::Write;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::{probe_request, workflows};
use crate::live_source::{Snapshot, Update, Worker, identity_signature, identity_validate};

#[derive(Default)]
struct Observations {
    latest: Value,
    updates: u32,
    failures: u32,
    distinct: u32,
    previous: Option<(String, String, u64)>,
    first_at: Option<Instant>,
    latest_at: Option<Instant>,
    longest_gap: Duration,
    read_ms_max: f64,
    first_error: Option<&'static str>,
    latest_error: Option<&'static str>,
}

struct ReportTime {
    started: Instant,
    now: Instant,
}

const SECONDS_MAX: u32 = 1800;
const POLL_INTERVAL: Duration = Duration::from_millis(250);
const REPORT_INTERVAL: Duration = Duration::from_secs(10);
const STEPS_MAX: u32 = SECONDS_MAX * 4;
const SETTLE_INTERVAL: Duration = Duration::from_secs(5);
const _: () = {
    assert!(SECONDS_MAX > 900);
    assert!(STEPS_MAX == 7200);
};

pub(super) fn run(root: PathBuf, seconds: u32) -> Result<(), String> {
    if !(5..=SECONDS_MAX).contains(&seconds) {
        return Err("Live soak duration must be between 5 and 1800 seconds".into());
    }
    let started = Instant::now();
    let mut worker = Worker::start(root).map_err(|error| error.to_string())?;
    let result = run_updates(&mut worker, started, seconds);
    worker.stop();
    drop(worker);
    run_emit(&json!({"mode": "live_soak", "event": "worker_dropped",
        "elapsed_seconds": started.elapsed().as_secs_f64(), "success": result.is_ok(),
        "worker_stop_wait_seconds_max": 2,
        "settle_seconds": SETTLE_INTERVAL.as_secs()}))?;
    thread::sleep(SETTLE_INTERVAL);
    run_emit(&json!({"mode": "live_soak", "event": "settled",
        "elapsed_seconds": started.elapsed().as_secs_f64()}))?;

    result
}

fn run_updates(worker: &mut Worker, started: Instant, seconds: u32) -> Result<(), String> {
    let deadline = started + Duration::from_secs(u64::from(seconds));
    let generation = worker.request(probe_request("overview"))?;
    let mut next_report = started;
    let mut observations = Observations::default();
    run_emit(
        &json!({"mode": "live_soak", "event": "started", "seconds": seconds,
        "generation": generation}),
    )?;
    for _step in 0..STEPS_MAX {
        if Instant::now() >= deadline {
            break;
        }
        match worker.take()? {
            Some(Update::Ready(snapshot)) => {
                if snapshot.generation != generation {
                    return Err("Live soak received a superseded generation".into());
                }
                observations.observe(&snapshot, Instant::now())?;
                drop(snapshot);
            }
            Some(Update::Failed {
                generation: failed,
                message,
            }) => {
                if failed != generation {
                    return Err("Live soak received a superseded failure".into());
                }
                observations.failed(&message);
                eprintln!("flightdeck live soak generation {failed}: {message:.512}");
            }
            None => {}
        }
        if Instant::now() >= next_report {
            run_emit(&observations.report(
                &ReportTime {
                    started,
                    now: Instant::now(),
                },
                "progress",
            ))?;
            next_report = Instant::now() + REPORT_INTERVAL;
        }
        thread::sleep(POLL_INTERVAL.min(deadline.saturating_duration_since(Instant::now())));
    }
    run_emit(&observations.report(
        &ReportTime {
            started,
            now: Instant::now(),
        },
        "finished",
    ))?;
    if observations.updates == 0 {
        return Err("Live soak received no real recording observations".into());
    }
    if seconds >= 10 {
        if observations.distinct < 2 {
            return Err("Live soak did not observe an advancing publication".into());
        }
    }

    Ok(())
}

fn run_snapshot(snapshot: &Snapshot) -> Result<Value, String> {
    let fans = snapshot
        .pages
        .get("fans")
        .ok_or("Missing live soak fan page")?;
    identity_validate(&fans["observation"], &snapshot.selection["selected"])?;
    if !snapshot.read_ms.is_finite() {
        return Err("Live soak received a non-finite read duration".into());
    }
    if snapshot.read_ms < 0.0 {
        return Err("Live soak received a negative read duration".into());
    }
    for field in ["tail_ticks", "tail_cache_bytes", "tail_cache_bytes_max"] {
        if snapshot.catalog[field].as_u64().is_none() {
            return Err(format!("Live soak catalog is missing {field}"));
        }
    }
    let coverage = workflows::history_coverage(&snapshot.selection, "trends", false)?;
    let rankings = super::rankings_report(snapshot)?;
    let selected = &snapshot.selection["selected"];
    Ok(
        json!({"read_ms": snapshot.read_ms, "selected": selected, "rankings": rankings,
        "selected_run_tick": selected["run_tick"], "scalar_history": coverage,
        "tail_ticks": snapshot.catalog["tail_ticks"],
        "tail_cache_bytes": snapshot.catalog["tail_cache_bytes"],
        "tail_cache_bytes_max": snapshot.catalog["tail_cache_bytes_max"],
        "collector_completed": snapshot.catalog["collector_completed"]}),
    )
}

impl Observations {
    fn observe(&mut self, snapshot: &Snapshot, now: Instant) -> Result<(), String> {
        self.latest = run_snapshot(snapshot)?;
        let identity = identity_signature(&snapshot.selection["selected"])?;
        if self.previous.as_ref() != Some(&identity) {
            if let Some(previous) = &self.previous {
                if previous.0 == identity.0 {
                    if previous.1 == identity.1 {
                        if identity.2 < previous.2 {
                            return Err("Live soak followed a decreasing publication".into());
                        }
                    }
                }
            }
            if let Some(latest) = self.latest_at {
                self.longest_gap = self.longest_gap.max(now.duration_since(latest));
            }
            if self.first_at.is_none() {
                self.first_at = Some(now);
            }
            self.latest_at = Some(now);
            self.distinct += 1;
            self.previous = Some(identity);
        }
        self.updates += 1;
        self.read_ms_max = self.read_ms_max.max(snapshot.read_ms);

        Ok(())
    }

    fn failed(&mut self, message: &str) {
        let category = run_error_category(message);
        if self.first_error.is_none() {
            self.first_error = Some(category);
        }
        self.latest_error = Some(category);
        self.failures += 1;
    }

    fn report(&self, time: &ReportTime, event: &str) -> Value {
        let age = self.latest_at.map(|latest| time.now.duration_since(latest));
        let initial_wait = self
            .first_at
            .unwrap_or(time.now)
            .duration_since(time.started);
        let gap = self
            .longest_gap
            .max(age.unwrap_or(Duration::ZERO))
            .max(initial_wait);
        json!({"mode": "live_soak", "event": event,
            "elapsed_seconds": time.now.duration_since(time.started).as_secs_f64(),
            "updates": self.updates, "distinct_observations": self.distinct,
            "advancing_observations": self.distinct > 1,
            "last_observation_age_seconds": age.map(|age| age.as_secs_f64()),
            "initial_observation_wait_seconds": initial_wait.as_secs_f64(),
            "longest_without_new_observation_seconds": gap.as_secs_f64(),
            "read_ms_max": if self.updates > 0 { Some(self.read_ms_max) } else { None },
            "failures": self.failures,
            "first_error_category": self.first_error, "last_error_category": self.latest_error,
            "latest": self.latest})
    }
}

fn run_error_category(message: &str) -> &'static str {
    if message.contains("cancel") {
        "cancelled"
    } else {
        if message.contains("Permission denied") {
            "permission_denied"
        } else {
            if message.contains("No such file") {
                "recording_unavailable"
            } else {
                if message.contains("bound") {
                    "bound_refused"
                } else {
                    "reader_failure"
                }
            }
        }
    }
}

fn run_emit(report: &Value) -> Result<(), String> {
    let mut output = std::io::stdout().lock();
    writeln!(output, "{report}").map_err(|error| error.to_string())?;
    output.flush().map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::{Duration, Instant};

    use serde_json::{Value, json};

    use super::{Observations, ReportTime};
    use crate::live_source::Snapshot;

    #[test]
    fn a_metadata_refresh_preserves_the_last_distinct_observation_age() {
        // Equal publication identities refresh metadata while the arrival age keeps increasing.
        let started = Instant::now();
        let mut observations = Observations::default();
        let mut refresh = snapshot(&observation(10));
        refresh.selection["selected"]["reader_losses"] = json!({"publications": 3});
        refresh.catalog["tail_ticks"] = json!(2);

        observations
            .observe(
                &snapshot(&observation(10)),
                started + Duration::from_secs(2),
            )
            .unwrap();
        observations
            .observe(&refresh, started + Duration::from_secs(8))
            .unwrap();
        let report = observations.report(
            &ReportTime {
                started,
                now: started + Duration::from_secs(10),
            },
            "finished",
        );

        assert_eq!(report["updates"], 2);
        assert_eq!(report["distinct_observations"], 1);
        assert_eq!(report["advancing_observations"], false);
        assert_eq!(report["last_observation_age_seconds"], 8.0);
        assert_eq!(report["longest_without_new_observation_seconds"], 8.0);
        assert_eq!(report["latest"]["tail_ticks"], 2);
    }

    #[test]
    fn an_increasing_publication_advances_the_observation_clock() {
        // A later publication becomes a new observation and resets its reported age.
        let started = Instant::now();
        let mut observations = Observations::default();

        observations
            .observe(
                &snapshot(&observation(10)),
                started + Duration::from_secs(2),
            )
            .unwrap();
        observations
            .observe(
                &snapshot(&observation(11)),
                started + Duration::from_secs(5),
            )
            .unwrap();
        let report = observations.report(
            &ReportTime {
                started,
                now: started + Duration::from_secs(7),
            },
            "finished",
        );

        assert_eq!(report["updates"], 2);
        assert_eq!(report["distinct_observations"], 2);
        assert_eq!(report["advancing_observations"], true);
        assert_eq!(report["last_observation_age_seconds"], 2.0);
        assert_eq!(report["initial_observation_wait_seconds"], 2.0);
        assert_eq!(report["longest_without_new_observation_seconds"], 3.0);
    }

    #[test]
    fn a_decreasing_publication_in_the_same_run_is_rejected() {
        // The same run and boot cannot follow publication twenty with publication nineteen.
        let started = Instant::now();
        let mut observations = Observations::default();
        observations
            .observe(&snapshot(&observation(20)), started)
            .unwrap();

        let result = observations.observe(
            &snapshot(&observation(19)),
            started + Duration::from_secs(1),
        );

        assert!(result.is_err());
    }

    #[test]
    fn a_new_run_can_restart_its_publication_counter() {
        // A new collector run has its own publication sequence even within the same boot.
        let started = Instant::now();
        let mut observations = Observations::default();
        let mut restarted = observation(1);
        restarted["run"] = json!("00000000000000050000000000000006");
        restarted["wall_ns"] = json!(21_000_000_000_u64);

        observations
            .observe(&snapshot(&observation(20)), started)
            .unwrap();
        observations
            .observe(&snapshot(&restarted), started + Duration::from_secs(2))
            .unwrap();
        let report = observations.report(
            &ReportTime {
                started,
                now: started + Duration::from_secs(3),
            },
            "finished",
        );

        assert_eq!(report["distinct_observations"], 2);
        assert_eq!(report["advancing_observations"], true);
        assert_eq!(report["last_observation_age_seconds"], 1.0);
        assert_eq!(report["latest"]["selected"]["run"], restarted["run"]);
    }

    fn observation(publication: u64) -> Value {
        json!({"run": "00000000000000010000000000000002",
            "boot": "00000000000000030000000000000004", "publication": publication,
            "run_tick": publication, "wall_ns": publication * 1_000_000_000})
    }

    fn snapshot(selected: &Value) -> Snapshot {
        let rankings = crate::live_rankings::read(selected, |_| {
            Ok(json!({
                "selected": selected, "apps": [], "membership_complete": true,
                "ungrouped_members": 0, "app_count": 0,
                "page": {"offset": 0, "limit": 512, "total": 0, "next_offset": null}
            }))
        });
        Snapshot {
            generation: 1,
            catalog: json!({"tail_ticks": 1, "tail_cache_bytes": 1024,
                "tail_cache_bytes_max": 134_217_728, "collector_completed": false}),
            selection: json!({"selected": selected, "trends": [selected]}),
            pages: BTreeMap::from([("fans".to_owned(), json!({"observation": selected}))]),
            apps: None,
            detail: None,
            history: None,
            cpu_detail: None,
            cpu_history: None,
            resident_history: None,
            app_attribution_history: None,
            process_history: None,
            rankings: Some(std::sync::Arc::new(rankings)),
            read_ms: 5.0,
        }
    }
}
