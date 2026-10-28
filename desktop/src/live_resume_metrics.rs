//! Opt-in measurements from a window shown or Overview opened to its fresh CPU frame, for the
//! automation build.
use std::time::{Duration, Instant};

use collector::clock::Timebase;
use collector::processes::{Usage, read_usage};
use gpui::{Context, Window};
use persistence::resources::Footprint;
use serde_json::{Value, json};

use crate::live_dashboard::LiveDashboard;

#[derive(Default)]
pub(crate) struct ResumeMetrics {
    enabled: bool,
    started: Option<Started>,
    frame_pending: bool,
    result: Value,
    count: u64,
}

struct Started {
    clock: Instant,
    timebase: Timebase,
    pid: i32,
    usage: Usage,
    generation: u64,
    store_version: Option<u64>,
    requests: u64,
    render_ms: Option<f64>,
}

pub(crate) struct Checkpoint {
    pub(crate) generation: u64,
    pub(crate) store_version: Option<u64>,
    pub(crate) requests: u64,
}

impl ResumeMetrics {
    pub(crate) fn enable(&mut self) -> bool {
        if self.enabled {
            return false;
        }
        self.enabled = true;
        true
    }

    pub(crate) fn begin(&mut self, checkpoint: &Checkpoint) {
        if !self.enabled {
            return;
        }
        match Self::begin_read(checkpoint) {
            Ok(started) => {
                self.started = Some(started);
                self.frame_pending = false;
                self.result = Value::Null;
                self.count += 1;
            }
            Err(error) => eprintln!("Resume measurement start: {error}"),
        }
    }

    fn begin_read(checkpoint: &Checkpoint) -> std::io::Result<Started> {
        let clock = Instant::now();
        let timebase = Timebase::read()?;
        let pid = i32::try_from(std::process::id()).map_err(std::io::Error::other)?;
        let usage = read_usage(timebase, pid)?.map_err(|error| {
            std::io::Error::other(format!("resume process counters unavailable: {error:?}"))
        })?;
        Ok(Started {
            clock,
            timebase,
            pid,
            usage,
            generation: checkpoint.generation,
            store_version: checkpoint.store_version,
            requests: checkpoint.requests,
            render_ms: None,
        })
    }

    pub(crate) fn result(&self) -> Value {
        json!({"count":self.count, "pending":self.started.is_some(), "sample":self.result})
    }

    pub(crate) fn count(&self) -> u64 {
        self.count
    }

    pub(crate) fn after(&self, count: u64) -> Option<Value> {
        if self.count <= count {
            return None;
        }
        if self.started.is_some() {
            return None;
        }
        Some(self.result())
    }

    fn finish(&mut self, dashboard: &Frame) -> std::io::Result<()> {
        if dashboard.epoch != self.count {
            return Ok(());
        }
        let Some(started) = self.started.as_ref() else {
            return Ok(());
        };
        let elapsed = started.clock.elapsed();
        let after = read_usage(started.timebase, started.pid)?
            .map_err(|error| std::io::Error::other(format!("resume counters: {error:?}")))?;
        let footprint = Footprint::read()?;
        self.result = json!({
            "fresh_render_start_ms":started.render_ms,
            "fresh_cpu_frame_upper_ms":elapsed.as_secs_f64() * 1000.0,
            "cpu_ms":Duration::from_nanos(after.user_ns + after.system_ns
                - started.usage.user_ns - started.usage.system_ns).as_secs_f64() * 1000.0,
            "disk_read_bytes":after.disk_read_bytes - started.usage.disk_read_bytes,
            "footprint_bytes":footprint.current_bytes,
            "footprint_peak_bytes":footprint.lifetime_bytes_max,
            "worker_requests":dashboard.requests - started.requests,
            "store_version":dashboard.store_version, "second":dashboard.second,
            "route":dashboard.route,
            "scope":"Visibility or navigation through CPU frame completion; excludes GPU display",
        });
        self.started = None;
        Ok(())
    }
}

struct Frame {
    epoch: u64,
    requests: u64,
    store_version: Option<u64>,
    second: Option<u64>,
    route: String,
}

impl LiveDashboard {
    pub(crate) fn measure_resume_frame(&mut self, window: &Window, cx: &mut Context<Self>) {
        if self.resume_metrics.started.is_none() {
            return;
        }
        if self.resume_metrics.frame_pending {
            return;
        }
        if !self.resume_frame_ready() {
            return;
        }
        let started = self
            .resume_metrics
            .started
            .as_mut()
            .unwrap_or_else(|| unreachable!());
        started.render_ms = Some(started.clock.elapsed().as_secs_f64() * 1000.0);
        self.resume_metrics.frame_pending = true;
        let epoch = self.resume_metrics.count;
        let entity = cx.entity().downgrade();
        window.on_next_frame(move |_, cx| {
            if let Err(error) = entity.update(cx, |dashboard, _| {
                let frame = Frame {
                    epoch,
                    requests: dashboard.worker.requests_submitted(),
                    store_version: dashboard.resume_store_version(),
                    second: dashboard.shown_second(),
                    route: dashboard.request.route.clone(),
                };
                if let Err(error) = dashboard.resume_metrics.finish(&frame) {
                    eprintln!("Resume measurement finish: {error}");
                }
            }) {
                eprintln!("Resume measurement window closed: {error}");
            }
        });
    }

    fn resume_frame_ready(&self) -> bool {
        if self.request.second.is_none() {
            if self.request.route == "overview" {
                let Some(version) = self.resume_store_version() else {
                    return false;
                };
                return self.resume_metrics.started.as_ref().is_some_and(|started| {
                    started
                        .store_version
                        .is_none_or(|previous| version > previous)
                });
            }
        }
        if self.reply_pending {
            return false;
        }
        if self.request.second.is_none() {
            if self
                .resume_metrics
                .started
                .as_ref()
                .is_some_and(|started| self.generation <= started.generation)
            {
                return false;
            }
        }
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| !snapshot.histories_pending)
    }
}

#[cfg(test)]
mod tests {
    use super::{Checkpoint, Frame, ResumeMetrics};

    #[test]
    fn refused_native_counters_do_not_complete_a_successful_measurement() {
        // Goal: a failed counter read cannot return a successful null sample.
        // Method: start against this process, then read an exited owned process at completion.
        let mut child = std::process::Command::new("/usr/bin/true")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        assert!(child.wait().unwrap().success());
        let mut metrics = ResumeMetrics::default();
        assert!(metrics.enable());
        let previous = metrics.count();
        metrics.begin(&Checkpoint {
            generation: 1,
            store_version: None,
            requests: 1,
        });
        metrics.started.as_mut().unwrap().pid = pid;

        let result = metrics.finish(&Frame {
            epoch: metrics.count(),
            requests: 1,
            store_version: None,
            second: Some(100),
            route: "overview".into(),
        });

        assert!(result.is_err());
        assert!(metrics.after(previous).is_none());
    }

    #[test]
    fn a_previous_frames_callback_cannot_finish_the_new_visibility_epoch() {
        // Goal: a hidden frame's delayed callback cannot report the next restore complete.
        // Method: start two real measurement epochs, then finish the first frame after the second.
        let mut metrics = ResumeMetrics::default();
        assert!(metrics.enable());
        let checkpoint = Checkpoint {
            generation: 1,
            store_version: None,
            requests: 1,
        };
        metrics.begin(&checkpoint);
        let first = metrics.count();
        metrics.begin(&checkpoint);
        assert!(metrics.started.is_some());

        metrics
            .finish(&Frame {
                epoch: first,
                requests: 1,
                store_version: None,
                second: Some(100),
                route: "overview".into(),
            })
            .unwrap();

        assert!(metrics.started.is_some());
        assert!(metrics.after(first).is_none());
    }
}
