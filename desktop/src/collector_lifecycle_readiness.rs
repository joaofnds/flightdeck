use std::time::{Duration, Instant};

use super::lifecycle::{CollectorSession, STARTUP_LIMIT};

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const POLL_STEPS_MAX: u128 = STARTUP_LIMIT
    .as_millis()
    .div_ceil(POLL_INTERVAL.as_millis())
    + 1;

pub(super) fn wait(collector: &mut CollectorSession) -> Result<(), String> {
    let started = Instant::now();
    let mut first = None;
    let mut latest = None;
    let mut observations = 0_u32;
    for _step in 0..POLL_STEPS_MAX {
        collector.tick();
        if let Some(failure) = collector.failure() {
            return Err(failure.to_owned());
        }

        if let Some(publication) = collector.observed_publication() {
            if first.is_none() {
                first = Some(publication);
            }
            if latest != Some(publication) {
                observations += 1;
                latest = Some(publication);
            }
        }
        if started.elapsed() >= STARTUP_LIMIT {
            return Err("Owned collector readiness exceeded 20 seconds".into());
        }

        if collector.ready() {
            eprintln!(
                "{}",
                serde_json::json!({"event":"owned_collector_ready",
                "elapsed_seconds":collector.elapsed().as_secs_f64(),
                "readiness_elapsed_seconds":started.elapsed().as_secs_f64(),
                "collector_pid":collector.owned_pid(), "first_publication":first,
                "latest_publication":latest, "observations":observations,
                "readiness_source":"registered_ring_headers"})
            );
            return Ok(());
        }

        std::thread::sleep(POLL_INTERVAL);
    }

    Err("Owned collector readiness exhausted its bounded steps".into())
}
