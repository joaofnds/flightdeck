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
    fresh_cause: FreshCause,
    render_ms: Option<f64>,
}

struct Checkpoint {
    generation: u64,
    store_version: Option<u64>,
    requests: u64,
    fresh_cause: FreshCause,
}

/// What makes the first frame of an epoch fresh.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FreshCause {
    /// Readings newer than the checkpoint's, as after a show or a page opened.
    Reading,
    /// The switch itself, which redraws the readings the page holds, as a picked range does.
    Switch,
    /// The held second's answered reading, as after a pick on a chart of the live Mac.
    Hold,
}

impl ResumeMetrics {
    pub(crate) fn enable(&mut self) -> bool {
        if self.enabled {
            return false;
        }
        self.enabled = true;
        true
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    fn begin(&mut self, checkpoint: &Checkpoint) {
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
            fresh_cause: checkpoint.fresh_cause,
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
            "route":dashboard.route, "hold_failure":dashboard.hold_failure,
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
    /// Why the read of the held second failed, which leaves the frame without its reading.
    hold_failure: Option<String>,
}

impl LiveDashboard {
    /// Starts a measurement epoch whose frame is fresh once the page holds readings newer than
    /// it does now, as after a show or a page opened, and answers the epoch count before it.
    pub(crate) fn resume_begin_reading(&mut self) -> u64 {
        let store_version = self.resume_store_version();
        self.resume_begin(store_version, FreshCause::Reading)
    }

    /// Starts a measurement epoch for a picked range, whose next frame is fresh since it redraws
    /// the readings the page already holds, and answers the epoch count before it.
    pub(crate) fn resume_begin_switch(&mut self) -> u64 {
        self.resume_begin(None, FreshCause::Switch)
    }

    /// Starts a measurement epoch for a second about to be held, whose frame is fresh once the
    /// hold has that second's complete reading or its read failed, and answers the epoch count
    /// before it.
    pub(crate) fn resume_begin_hold(&mut self) -> u64 {
        self.resume_begin(None, FreshCause::Hold)
    }

    fn resume_begin(&mut self, store_version: Option<u64>, fresh_cause: FreshCause) -> u64 {
        let before = self.resume_metrics.count();

        self.resume_metrics.begin(&Checkpoint {
            generation: self.generation,
            store_version,
            requests: self.worker.requests_submitted(),
            fresh_cause,
        });
        before
    }

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
                    hold_failure: dashboard
                        .held_reading()
                        .and_then(|reading| reading.failure.clone()),
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
        let Some(started) = &self.resume_metrics.started else {
            return false;
        };
        if started.fresh_cause == FreshCause::Switch {
            return true;
        }
        if started.fresh_cause == FreshCause::Hold {
            return self.held_reading().is_some_and(|reading| reading.complete);
        }
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
    use gpui::{AppContext, Entity, TestAppContext};

    use super::{Checkpoint, Frame, FreshCause, ResumeMetrics};
    use crate::live_dashboard::LiveDashboard;
    use crate::live_source::{Update, Worker};
    use crate::live_store_fixtures::reading_at;
    use crate::live_store_page_fixtures::readings;

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
            fresh_cause: FreshCause::Reading,
        });
        metrics.started.as_mut().unwrap().pid = pid;

        let result = metrics.finish(&Frame {
            epoch: metrics.count(),
            requests: 1,
            store_version: None,
            second: Some(100),
            route: "overview".into(),
            hold_failure: None,
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
            fresh_cause: FreshCause::Reading,
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
                hold_failure: None,
            })
            .unwrap();

        assert!(metrics.started.is_some());
        assert!(metrics.after(first).is_none());
    }

    #[test]
    fn a_sample_names_why_the_held_seconds_read_failed() {
        // Goal: a hold whose read failed reports the failure with its timing, so a run counts
        // it as a failed input and not as a frame that showed the second.
        // Method: finish the epoch profile-hold begins with a frame whose held reading failed,
        // then read the sample.
        let mut metrics = ResumeMetrics::default();
        assert!(metrics.enable());
        let previous = metrics.count();
        metrics.begin(&Checkpoint {
            generation: 1,
            store_version: None,
            requests: 1,
            fresh_cause: FreshCause::Hold,
        });

        metrics
            .finish(&Frame {
                epoch: metrics.count(),
                requests: 1,
                store_version: None,
                second: Some(100),
                route: "cpu".into(),
                hold_failure: Some("the second's minute is gone".into()),
            })
            .unwrap();

        assert_eq!(
            metrics.after(previous).unwrap()["sample"]["hold_failure"],
            "the second's minute is gone"
        );
    }

    #[test]
    fn a_range_switch_on_a_page_the_worker_draws_is_fresh_at_the_next_frame() {
        // Goal: profile-range measures a switch on every page, which redraws the readings the
        // page holds and asks the worker for nothing. Method: the CPU page following the newest
        // second, the epoch profile-range begins, then the readiness of the next frame.
        let mut dashboard = page("cpu");
        assert!(dashboard.resume_metrics.enable());

        dashboard.resume_begin_switch();

        assert!(dashboard.resume_frame_ready());
    }

    #[test]
    fn a_page_opened_on_a_page_the_worker_draws_waits_for_newer_readings() {
        // Goal: an epoch begun as a page opens or the window shows has its fresh frame only once
        // the worker answers with newer readings, unlike a range switch on the same page.
        // Method: the CPU page following the newest second, the epoch profile-navigate begins,
        // then the readiness of the next frame.
        let mut dashboard = page("cpu");
        assert!(dashboard.resume_metrics.enable());

        dashboard.resume_begin_reading();

        assert!(!dashboard.resume_frame_ready());
    }

    #[gpui::test]
    fn a_held_second_waits_for_its_own_reading(cx: &mut TestAppContext) {
        // Goal: profile-hold's frame is not fresh while the reader still reads the held second,
        // since that frame shows the second without what only its own reading holds.
        // Method: the CPU page following the newest second holds a second its readings name, as
        // a click on its chart does, then the readiness of the next frame.
        let dashboard = cx.new(|_| page("cpu"));

        hold(&dashboard, 9_999, cx);

        assert!(dashboard.read_with(cx, |dashboard, _| dashboard.hold_read_pending()));
        assert!(!dashboard.read_with(cx, |dashboard, _| dashboard.resume_frame_ready()));
    }

    #[gpui::test]
    fn a_held_second_is_fresh_once_the_reader_answers_its_read(cx: &mut TestAppContext) {
        // Goal: profile-hold's frame is fresh once the reader answers the held second's read,
        // failed or not, and the failure stays with the held reading for the sample to name.
        // Method: the CPU page holds a second its readings name, the reader answers the hold's
        // read with a failure, then the readiness of the next frame.
        let dashboard = cx.new(|_| page("cpu"));
        hold(&dashboard, 9_999, cx);

        dashboard.update(cx, |dashboard, _| {
            let generation = dashboard.held_read_generation().unwrap();
            let message = "the second's minute is gone".to_owned();
            assert!(dashboard.take_update(Update::Failed {
                generation,
                message
            }));
        });

        assert!(dashboard.read_with(cx, |dashboard, _| dashboard.resume_frame_ready()));
        assert_eq!(
            dashboard.read_with(cx, |dashboard, _| dashboard.held_reading()?.failure.clone()),
            Some("the second's minute is gone".to_owned())
        );
    }

    /// Begins profile-hold's epoch and holds `second` on `dashboard`, as profile-hold does.
    fn hold(dashboard: &Entity<LiveDashboard>, second: u64, cx: &mut TestAppContext) {
        dashboard.update(cx, |dashboard, cx| {
            assert!(dashboard.resume_metrics.enable());
            dashboard.resume_begin_hold();
            dashboard.pin_second(second, cx);
        });
    }

    /// The page `route` showing the worker's readings, following the newest second.
    fn page(route: &str) -> LiveDashboard {
        let root = std::env::temp_dir().join(format!(
            "flightdeck-missing-resume-metrics-{}",
            std::process::id()
        ));
        let mut dashboard = LiveDashboard::new(Worker::start(root).unwrap(), route.into(), None);
        dashboard.accept(readings(&[reading_at(9_999, 0), reading_at(10_000, 0)]));
        assert!(dashboard.error.is_none());
        dashboard
    }
}
