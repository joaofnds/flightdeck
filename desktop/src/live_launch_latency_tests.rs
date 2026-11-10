//! P00's latency cells on W10's reference input, timed with nothing on screen and no wait on
//! the recording's own clock. A GPUI test window, laying out text with macOS's own text system,
//! opens a frozen copy of the hour through the app's live backend, as `flightdeck --recording
//! COPY --attach-live` opens it. The test plays the app's two timers and a 120 Hz display at
//! their own intervals, and reveals the copy's seconds one at a time through the freezer, so a
//! cell takes as long as the app's work and no longer. P00 runs hardware latency thresholds only
//! under its declared protocol, so each cell is opt-in:
//!
//! ```text
//! FLIGHTDECK_REFERENCE_RECORDING=<W10's hour> FLIGHTDECK_REFERENCE_FREEZER=<resume_bench>
//! FLIGHTDECK_REFERENCE_OUTPUT=<directory> POLARS_MAX_THREADS=6 POLARS_ASYNC_THREAD_COUNT=1
//! cargo test --release --features automation --bin flightdeck latency_tests -- --ignored
//! ```
//!
//! A figure leaves out the wait for the frame the app draws in, at most one display frame, and
//! the GPU's work on that frame, since a test window draws as soon as the page changes. The
//! inputs follow the app sessions, the earlier runs that timed these cells in the app itself on
//! screen, so the figures of the two compare.
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gpui::{
    Entity, Modifiers, TestAppContext, VisualTestContext, WindowOptions, WindowVisibility, point,
    px, size,
};
use serde_json::{Value, json};

use super::collection::QUICK_POLL_INTERVAL;
use crate::live_dashboard::LiveDashboard;
use crate::live_timer::TICK_INTERVAL;

/// P00's target for a warm show of the window and a retained hover, at p95.
const TARGET_MS: f64 = 100.0;
/// The first second the replayed cells keep hidden, as the app sessions did, which leaves 117
/// of the hour's seconds to reveal.
const REPLAY_FIRST: u64 = 3480;
/// The seconds a return past the ring finds hidden, more than the ring keeps. Hiding 180 leaves
/// the copy's newest reading at the end of a sealed minute, three minutes before the hour's last,
/// so the window opens on readings the Store can read. The app sessions hid 150, which left
/// visible 30 seconds past a sealed minute whose ring slots the frozen copy had already
/// overwritten with the hour's end, a state no live recording shows, and no page asked for after
/// the opening was ever answered.
const RETURN_GAP: u64 = 180;
const CYCLES: usize = 100;
const RETURNS: usize = 10;
/// The pointer's places on each hovered chart, 4 points apart along the row the app sessions
/// swept, at its height.
const HOVER_FIRST_X: f32 = 260.0;
const HOVER_STEP_X: f32 = 4.0;
const HOVER_Y: f32 = 430.0;
const HOVERS_PER_ROUTE: usize = 100;
/// Pointer moves to each revealed second, as the app sessions moved the pointer every 0.15 s
/// while the freezer revealed a second each second.
const HOVERS_PER_REVEAL: usize = 7;
const HOVER_ROUTES: [&str; 3] = ["overview", "cpu", "memory"];
/// The window's size in the app sessions.
const WINDOW_WIDTH: f32 = 1280.0;
const WINDOW_HEIGHT: f32 = 800.0;
/// A 120 Hz display's frame interval.
const FRAME_INTERVAL: Duration = Duration::from_nanos(8_333_333);
/// How long a page may take to open over the hour, which the Store reads whole first.
const OPEN_WAIT: Duration = Duration::from_secs(120);
/// How long one input may take before it counts as lost.
const INPUT_WAIT: Duration = Duration::from_secs(10);
/// A bound on the timer steps of one wait, past the most a wait can take at one step a
/// millisecond.
const STEPS_MAX: u32 = 1_000_000;
const FREEZER_REPLY_BYTES_MAX: u64 = 4096;

/// Lets one cell run at a time, since two at once would time each other's work.
static CELLS: Mutex<()> = Mutex::new(());

#[test]
#[ignore = "opt-in latency on W10's reference input; needs FLIGHTDECK_REFERENCE_*"]
fn a_window_shown_again_draws_the_second_revealed_while_hidden_within_the_target() {
    // Goal: P00's warm resume, Overview's window hidden and shown again, draws the newer second
    // within 100 ms at p95. Method: replay the hour from its last two minutes, then a hundred
    // times hide the window, wait for the Store to park, reveal the next second and show the
    // window, timing each show to its fresh frame with the app's own measurement.
    let _cell = CELLS.lock().unwrap();
    let protocol = Protocol::read();
    gpui::run_test_once(
        0,
        Box::new(move |dispatcher| {
            let opening = Opening::Replay { cell: "cycle" };
            let mut harness = ReferenceHarness::setup(&protocol, dispatcher, opening);
            harness.warm();
            let mut samples = Samples::new(&protocol, "cycle");

            for _ in 0..CYCLES {
                let shown = harness.show_after_revealing_while_hidden("replay-next");
                samples.push_resume(shown.as_ref());
            }

            let summary = samples.finish();
            harness.teardown();
            assert_eq!(summary["failed"], 0);
            assert!(
                summary["p95_ms"].as_f64().unwrap() <= TARGET_MS,
                "{summary}"
            );
        }),
    );
}

#[test]
#[ignore = "opt-in latency on W10's reference input; needs FLIGHTDECK_REFERENCE_*"]
fn a_hover_of_a_retained_second_completes_within_the_target() {
    // Goal: P00's retained hover, the pointer moved across a chart of the hour on Overview, CPU
    // and Memory, gives each hovered second's complete readings within 100 ms at p95.
    // Method: replay the hour from its last two minutes, move the pointer a hundred times across
    // each route's chart, revealing a second every seventh move, and time each hover to the
    // hovered second's complete readings with the app's own measurement. Each move waits for
    // the one before it to finish, where the app sessions moved every 0.15 s, which differs
    // only for a hover slower than that.
    let _cell = CELLS.lock().unwrap();
    let protocol = Protocol::read();
    gpui::run_test_once(
        0,
        Box::new(move |dispatcher| {
            let opening = Opening::Replay { cell: "hover" };
            let mut harness = ReferenceHarness::setup(&protocol, dispatcher, opening);
            harness.warm();
            let mut samples = Samples::new(&protocol, "hover");

            for route in HOVER_ROUTES {
                harness.open_page(route);
                for index in 0..HOVERS_PER_ROUTE {
                    if index % HOVERS_PER_REVEAL == 0 {
                        harness.freezer.command("replay-next");
                    }
                    match harness.hover(hover_x(index)) {
                        Some(timings) => timings
                            .iter()
                            .for_each(|timing| samples.push_hover(Some(timing))),
                        None => samples.push_hover(None),
                    }
                }
                harness.leave_charts();
            }

            let summary = samples.finish();
            harness.teardown();
            assert_eq!(summary["failed"], 0);
            assert!(
                summary["p95_ms"].as_f64().unwrap() <= TARGET_MS,
                "{summary}"
            );
        }),
    );
}

#[test]
#[ignore = "opt-in latency on W10's reference input; needs FLIGHTDECK_REFERENCE_*"]
fn a_return_past_the_ring_draws_the_newest_second() {
    // Goal: the frame side of a return past the ring, Memory's window shown again after the
    // recording went on past what the ring keeps, timed to the frame that draws the newest
    // second and held to no target. Method: ten times, open a frozen copy with its newest
    // `RETURN_GAP` seconds hidden, warm every route, hide the window on Memory, wait for the
    // Store to park, reveal the hidden seconds and show the window.
    let _cell = CELLS.lock().unwrap();
    let protocol = Protocol::read();
    let mut samples = Samples::new(&protocol, "return");

    for round in 0..RETURNS {
        let protocol = protocol.clone();
        let shown = gpui::run_test_once(
            0,
            Box::new(move |dispatcher| {
                let opening = Opening::Gap { round };
                let mut harness = ReferenceHarness::setup(&protocol, dispatcher, opening);
                harness.warm();
                harness.open_page("memory");

                let shown = harness.show_after_revealing_while_hidden("advance");
                harness.teardown();
                shown
            }),
        );
        samples.push_resume(shown.as_ref());
    }

    let summary = samples.finish();
    assert_eq!(summary["failed"], 0);
}

/// The x of the pointer's `index`th place on a chart.
fn hover_x(index: usize) -> f32 {
    let index = u16::try_from(index).unwrap();
    HOVER_FIRST_X + HOVER_STEP_X * f32::from(index)
}

/// The declared protocol: W10's hour, the freezer that holds a copy of it still, where the
/// samples go, and the app's own Polars threads, in a release build.
#[derive(Clone)]
struct Protocol {
    recording: PathBuf,
    freezer: PathBuf,
    output: PathBuf,
}

impl Protocol {
    fn read() -> Self {
        let profile = if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        };
        assert_eq!(
            profile, "release",
            "time a release build, which the app ships"
        );
        for (option, count) in crate::APP_THREAD_COUNTS.environment() {
            let value = std::env::var(option).ok();
            assert_eq!(value.as_deref(), Some(count.as_str()), "{option}");
        }
        let output = PathBuf::from(std::env::var_os("FLIGHTDECK_REFERENCE_OUTPUT").unwrap());
        assert!(output.is_dir());

        Self {
            recording: PathBuf::from(std::env::var_os("FLIGHTDECK_REFERENCE_RECORDING").unwrap()),
            freezer: PathBuf::from(std::env::var_os("FLIGHTDECK_REFERENCE_FREEZER").unwrap()),
            output,
        }
    }
}

/// What the frozen copy shows when the window opens.
#[derive(Clone, Copy)]
enum Opening {
    /// The hour up to `REPLAY_FIRST`, with the rest to reveal a second at a time, for `cell`.
    Replay { cell: &'static str },
    /// The hour without its newest `RETURN_GAP` seconds and the minutes sealed after them, to
    /// reveal at once, for the `round`th return.
    Gap { round: usize },
}

impl Opening {
    fn copy_name(self) -> String {
        match self {
            Opening::Replay { cell } => format!("{cell}-copy"),
            Opening::Gap { round } => format!("return-copy-{round}"),
        }
    }

    fn command(self) -> String {
        match self {
            Opening::Replay { .. } => format!("replay-reset {REPLAY_FIRST}"),
            Opening::Gap { .. } => format!("checkpoint {RETURN_GAP}"),
        }
    }
}

/// The freezer's process, which holds a copy of the hour still and reveals its seconds when
/// told. Ending it removes the rings it copied, and dropping it ends it and removes the copy.
struct Freezer {
    child: Child,
    commands: ChildStdin,
    replies: BufReader<ChildStdout>,
    copy: PathBuf,
}

impl Freezer {
    fn start(protocol: &Protocol, copy: PathBuf) -> Self {
        assert!(!copy.exists(), "{}", copy.display());
        let mut child = Command::new(&protocol.freezer)
            .arg("--freeze")
            .arg(&protocol.recording)
            .arg("--directory")
            .arg(&copy)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let commands = child.stdin.take().unwrap();
        let replies = BufReader::new(child.stdout.take().unwrap());
        let mut freezer = Self {
            child,
            commands,
            replies,
            copy,
        };

        let captured = freezer.reply();
        assert_eq!(captured["root"].as_str(), freezer.copy.to_str());
        freezer
    }

    /// Sends `line` and asserts the freezer carried it out.
    fn command(&mut self, line: &str) {
        writeln!(self.commands, "{line}").unwrap();
        assert_eq!(self.reply(), json!({"ok": true}), "{line}");
    }

    fn reply(&mut self) -> Value {
        let mut line = String::new();
        let read = (&mut self.replies)
            .take(FREEZER_REPLY_BYTES_MAX)
            .read_line(&mut line)
            .unwrap();
        assert!(0 < read);
        serde_json::from_str(&line).unwrap()
    }
}

impl Drop for Freezer {
    /// Runs on a failed cell too, so no copy outlives its cell. A freezer that did not end
    /// cleanly may not have removed its rings, so its copy stays, since the copy's registry
    /// names the rings left to unlink.
    fn drop(&mut self) {
        let ended = writeln!(self.commands, "quit").and_then(|()| self.child.wait());
        let removed = match ended {
            Ok(status) if status.success() => {
                std::fs::remove_dir_all(&self.copy).map_err(|error| error.to_string())
            }
            Ok(status) => Err(format!("the freezer ended with {status}")),
            Err(error) => Err(error.to_string()),
        };

        if let Err(error) = removed {
            if std::thread::panicking() {
                eprintln!("{}: {error}", self.copy.display());
            } else {
                panic!("{}: {error}", self.copy.display());
            }
        }
    }
}

/// A frozen copy of W10's hour open live in a test window the size the app sessions used. The
/// freezer comes last, so a failed cell drops the window before it removes the copy.
struct ReferenceHarness {
    window: VisualTestContext,
    dashboard: Entity<LiveDashboard>,
    clock: AppClock,
    freezer: Freezer,
}

impl ReferenceHarness {
    fn setup(protocol: &Protocol, dispatcher: gpui::TestDispatcher, opening: Opening) -> Self {
        let mut freezer = Freezer::start(protocol, protocol.output.join(opening.copy_name()));
        freezer.command(&opening.command());
        let text = Arc::new(gpui_apple::AppleTextSystem::new());
        let mut app = TestAppContext::build_with_text_system(dispatcher, Some("latency"), text);
        let (dashboard, window) = open(&freezer.copy, &mut app);
        let mut harness = Self {
            window,
            dashboard,
            clock: AppClock::new(),
            freezer,
        };

        harness
            .window
            .simulate_resize(size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)));
        harness.settle(OPEN_WAIT);
        harness
    }

    fn teardown(self) {
        let Self {
            mut window,
            dashboard,
            freezer,
            ..
        } = self;
        crate::live_dashboard::input_tests::close_dashboard_window(&mut window, &dashboard);
        drop(window.cx.update(crate::live_dashboard::uninstall_store));
        window.cx.run_until_parked();
        window.cx.quit();
        drop(freezer);
    }

    /// Opens every page and sweeps Overview's charts with the pointer, as the app sessions did
    /// before they timed anything, so each page's first reading is behind it.
    fn warm(&mut self) {
        for (route, _) in crate::live_dashboard::ROUTES {
            self.open_page(route);
        }
        self.open_page("overview");
        for index in 0..HOVERS_PER_ROUTE {
            assert!(
                self.hover(hover_x(index)).is_some(),
                "a warming hover was lost"
            );
        }
        self.leave_charts();
    }

    fn open_page(&mut self, route: &str) {
        self.dashboard.update(&mut self.window, |dashboard, cx| {
            dashboard.navigate(route, cx);
        });
        self.settle(INPUT_WAIT);
    }

    /// Moves the pointer to `x` on the charts' row, plays the app's clock until the worker owes
    /// the page nothing, and answers the app's timings of the hovers the move ended or made.
    /// A move between two charts hovers nothing and answers none, as a fifth of the app
    /// sessions' moves did. A page still waiting past `INPUT_WAIT` answers no list at all.
    fn hover(&mut self, x: f32) -> Option<Vec<Value>> {
        let before = self.hovers()["count"].as_u64().unwrap();

        self.window
            .simulate_mouse_move(point(px(x), px(HOVER_Y)), None, Modifiers::none());

        self.run_until(INPUT_WAIT, |harness| {
            !awaiting_reply(&harness.dashboard, &harness.window)
        })?;
        let hovers = self.hovers();
        let added = usize::try_from(hovers["count"].as_u64().unwrap() - before).unwrap();
        let latest = hovers["latest"].as_array().unwrap();
        assert!(added <= latest.len(), "{added} hovers outran the report");
        Some(latest[latest.len() - added..].to_vec())
    }

    fn leave_charts(&mut self) {
        self.window
            .simulate_mouse_move(point(px(40.0), px(40.0)), None, Modifiers::none());
        self.settle(INPUT_WAIT);
    }

    /// Hides the window, waits for the Store to park, has the freezer `reveal` what the
    /// recording took in meanwhile and shows the window, and answers the app's measurement of
    /// the show to its fresh frame, or none where that frame never came.
    fn show_after_revealing_while_hidden(&mut self, reveal: &str) -> Option<Value> {
        self.window
            .simulate_visibility_change(WindowVisibility::Hidden);
        let parked = self.run_until(INPUT_WAIT, |harness| {
            harness
                .window
                .update(|_, cx| crate::live_dashboard::store_activity(cx)["paused"] == true)
        });
        assert!(parked.is_some(), "the Store never parked");
        self.freezer.command(reveal);
        let before = self.dashboard.read_with(&self.window, |dashboard, _| {
            dashboard.resume_metrics.count()
        });

        self.window
            .simulate_visibility_change(WindowVisibility::Visible);
        self.window.update(|window, _| window.activate_window());

        let mut sample = None;
        self.run_until(INPUT_WAIT, |harness| {
            sample = harness
                .dashboard
                .read_with(&harness.window, |dashboard, _| {
                    dashboard.resume_metrics.after(before)
                });
            sample.is_some()
        });
        self.settle(INPUT_WAIT);
        sample
    }

    /// Plays the app's clock until the page has the reading of its latest request and the
    /// worker owes it nothing. Self-monitoring shows no second of the recording, so the page's
    /// shown second is no test of it.
    #[track_caller]
    fn settle(&mut self, bound: Duration) {
        let settled = self.run_until(bound, |harness| {
            harness
                .dashboard
                .read_with(&harness.window, |dashboard, _| {
                    assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
                    let answered = dashboard
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.generation == dashboard.generation);
                    answered && !dashboard.awaiting_reply()
                })
        });
        assert!(settled.is_some(), "the page never settled: {}", self.page());
    }

    /// What the page shows and waits on, for a failure to name.
    fn page(&mut self) -> Value {
        let store = self
            .window
            .update(|_, cx| crate::live_dashboard::store_activity(cx));
        self.dashboard.read_with(&self.window, |dashboard, _| {
            json!({
                "route": dashboard.request.route, "second": dashboard.request.second,
                "generation": dashboard.generation,
                "snapshot_generation": dashboard.snapshot.as_ref()
                    .map(|snapshot| snapshot.generation),
                "shown_second": dashboard.shown_second(),
                "awaiting_reply": dashboard.awaiting_reply(),
                "reply_pending": dashboard.reply_pending,
                "histories_pending": dashboard.snapshot.as_ref()
                    .map(|snapshot| snapshot.histories_pending),
                "store": store,
            })
        })
    }

    fn hovers(&self) -> Value {
        self.dashboard.read_with(&self.window, |dashboard, _| {
            dashboard.hover_timings.report()
        })
    }

    /// Plays the app's timers and the display until `done` holds, and answers how long that
    /// took, or none past `bound`.
    fn run_until(
        &mut self,
        bound: Duration,
        mut done: impl FnMut(&mut Self) -> bool,
    ) -> Option<Duration> {
        let started = Instant::now();
        for _ in 0..STEPS_MAX {
            if done(self) {
                return Some(started.elapsed());
            }
            if bound < started.elapsed() {
                return None;
            }
            let next = self.clock.step(&self.dashboard, &mut self.window);
            std::thread::sleep(next.saturating_duration_since(Instant::now()));
        }
        panic!("a wait took {STEPS_MAX} steps");
    }
}

/// Opens the frozen copy at `copy` through the app's live backend, in a window that measures
/// its shows, as `--control`'s profile-resume makes it.
fn open(copy: &Path, app: &mut TestAppContext) -> (Entity<LiveDashboard>, VisualTestContext) {
    let mut backend = None;
    super::Backend::start(
        copy,
        crate::live_source_mode::SourceMode::Live,
        &mut backend,
    )
    .unwrap();
    let super::Backend { worker, store } = backend.unwrap();
    app.update(|cx| crate::live_dashboard::install_store(store.unwrap(), cx));

    let window = app.update(|cx| {
        cx.open_window(WindowOptions::default(), |window, cx| {
            let page = super::preferences::page(None, cx);
            let range = super::preferences::range(None, cx);
            let dashboard = crate::native_workflows::create_dashboard(worker, page, None, cx);
            dashboard.update(cx, |dashboard, cx| dashboard.set_range(range, cx));
            crate::live_dashboard::observe_visibility(&dashboard, window, cx);
            assert!(dashboard.update(cx, |dashboard, _| dashboard.resume_metrics.enable()));
            dashboard
        })
        .unwrap()
    });
    let dashboard = window.root(app).unwrap();
    (
        dashboard,
        VisualTestContext::from_window(window.into(), app),
    )
}

/// The app's two timers and the display, which a test window has none of: the page's
/// quarter-second tick, the quick poll that runs while the worker owes an answer, and a 120 Hz
/// display's frames, each at its own interval on the wall clock.
/// Each field is when that one fires next.
struct AppClock {
    tick: Instant,
    quick_poll: Option<Instant>,
    frame: Instant,
}

impl AppClock {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            tick: now + TICK_INTERVAL,
            quick_poll: None,
            frame: now + FRAME_INTERVAL,
        }
    }

    /// Fires what is due, as the app's run loop would, and answers when the next thing is due.
    fn step(
        &mut self,
        dashboard: &Entity<LiveDashboard>,
        window: &mut VisualTestContext,
    ) -> Instant {
        let now = Instant::now();
        // The quick poll starts when the worker comes to owe an answer, as the page's observer
        // starts it, and stops at the first of its ticks that finds none owed.
        if self.quick_poll.is_none() && awaiting_reply(dashboard, window) {
            self.quick_poll = Some(now + QUICK_POLL_INTERVAL);
        }
        if self.quick_poll.is_some_and(|due| due <= now) {
            dashboard.update(window, LiveDashboard::tick);
            self.quick_poll = awaiting_reply(dashboard, window).then(|| now + QUICK_POLL_INTERVAL);
        }
        if self.tick <= now {
            dashboard.update(window, LiveDashboard::tick);
            self.tick = next_after(self.tick, TICK_INTERVAL, now);
        }
        if self.frame <= now {
            window.update(gpui::Window::simulate_next_frame);
            self.frame = next_after(self.frame, FRAME_INTERVAL, now);
        }
        window.run_until_parked();

        let next = self.tick.min(self.frame);
        self.quick_poll
            .map_or(next, |quick_poll| next.min(quick_poll))
    }
}

fn awaiting_reply(dashboard: &Entity<LiveDashboard>, window: &VisualTestContext) -> bool {
    dashboard.read_with(window, |dashboard, _| dashboard.awaiting_reply())
}

/// The first time after `now` a timer that fired at `fired` fires again, skipping what it
/// missed, as a run loop's timer does.
fn next_after(fired: Instant, interval: Duration, now: Instant) -> Instant {
    let missed = now.duration_since(fired).as_nanos() / interval.as_nanos();
    let missed = u32::try_from(missed).unwrap();
    fired + interval * (missed + 1)
}

/// One cell's samples, written to `<output>/<cell>.jsonl` as they come.
struct Samples {
    cell: &'static str,
    file: std::fs::File,
    summary: PathBuf,
    times_ms: Vec<f64>,
    failed: u64,
}

impl Samples {
    fn new(protocol: &Protocol, cell: &'static str) -> Self {
        let file = std::fs::File::create_new(protocol.output.join(format!("{cell}.jsonl")));
        Self {
            cell,
            file: file.unwrap(),
            summary: protocol.output.join(format!("{cell}-summary.json")),
            times_ms: Vec::with_capacity(HOVER_ROUTES.len() * HOVERS_PER_ROUTE),
            failed: 0,
        }
    }

    /// Takes the app's measurement of a show, failed where none came or the held reading's read
    /// failed.
    fn push_resume(&mut self, measurement: Option<&Value>) {
        let time_ms = measurement.and_then(|measurement| {
            let sample = &measurement["sample"];
            if sample["hold_failure"].is_null() {
                sample["fresh_cpu_frame_upper_ms"].as_f64()
            } else {
                None
            }
        });
        self.push(time_ms, measurement.unwrap_or(&Value::Null));
    }

    /// Takes the app's timing of a hover, failed where its readings never came.
    fn push_hover(&mut self, timing: Option<&Value>) {
        let time_ms = timing.and_then(|timing| timing["complete_ms"].as_f64());
        self.push(time_ms, timing.unwrap_or(&Value::Null));
    }

    fn push(&mut self, time_ms: Option<f64>, line: &Value) {
        writeln!(self.file, "{line}").unwrap();
        match time_ms {
            Some(time_ms) => self.times_ms.push(time_ms),
            None => self.failed += 1,
        }
    }

    /// Writes and answers the cell's p50, nearest-rank p95 and max, with its input count and
    /// failures.
    fn finish(mut self) -> Value {
        assert!(!self.times_ms.is_empty());
        self.times_ms.sort_by(f64::total_cmp);
        // The nearest rank of a percentile is the count times its share, rounded up.
        let nearest_rank = |percent: usize| {
            let rank = (percent * self.times_ms.len()).div_ceil(100);
            self.times_ms[rank.max(1) - 1]
        };
        let timed = u64::try_from(self.times_ms.len()).unwrap();
        let summary = json!({
            "cell": self.cell, "inputs": timed + self.failed,
            "failed": self.failed, "p50_ms": nearest_rank(50), "p95_ms": nearest_rank(95),
            "max_ms": self.times_ms[self.times_ms.len() - 1],
            "scope": "the app's work in a test window; leaves out the frame's wait and GPU work",
        });
        std::fs::write(&self.summary, format!("{summary}\n")).unwrap();
        println!("{summary}");
        summary
    }
}
