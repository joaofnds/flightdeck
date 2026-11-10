//! How long a CPU burst takes to reach Overview's CPU line, from the publication of its first
//! reading to the first frame whose line passes 70%, timed with nothing on screen. A GPUI test
//! window opens a recording the test writes as a collector would, a steady 10% for the whole
//! range and then bursts of 100%, a publication a second on the test's own clock, and plays
//! the app's timers and a 120 Hz display as the latency cells do. Each figure leaves out the
//! frame's wait and the GPU's work, as theirs do. Opt-in, since it takes minutes and times the
//! release build the app ships:
//!
//! ```text
//! POLARS_MAX_THREADS=6 POLARS_ASYNC_THREAD_COUNT=1 cargo test --release --features automation \
//!     --bin flightdeck burst_tests -- --ignored --nocapture
//! ```
use std::path::PathBuf;
use std::sync::PoisonError;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use collector::machine::{CPUTicks, Memory};
use collector::recording::{self, RegisteredRun, Registry, private};
use collector::ring::{Dropped, ObservationTimes, Place, Readings, Shape, Writer};
use gpui::{Entity, TestAppContext, VisualTestContext, px, size};
use serde_json::json;

use super::{AppClock, INPUT_WAIT, OPEN_WAIT, WINDOW_HEIGHT, WINDOW_WIDTH};
use crate::charts::newest_drawn;
use crate::live_dashboard::LiveDashboard;

/// The chart and line a burst is read from, Overview's CPU.
const PANEL: &str = "live-cpu";
const LINE: &str = "All cores";
/// The ranges a burst is timed at, in minutes.
const RANGE_MINUTES: [u64; 2] = [5, 15];
const BURSTS: u32 = 5;
const STEADY_PERCENT: u32 = 10;
const BURST_PERCENT: u32 = 100;
/// The share of the chart a burst's line must pass.
const PASS: f32 = 70.0;
const PUBLICATION_INTERVAL: Duration = Duration::from_secs(1);
/// The publications of a burst before it counts as never reaching the line.
const BURST_PUBLICATIONS_MAX: u32 = 20;
/// The steady publications after a burst's line falls back below `PASS`, before the next burst,
/// more than any drawing's newest point stands for.
const SETTLE_PUBLICATIONS: u32 = 8;
/// Room in the ring for the steady range the window opens on, which the Store reads at its start.
/// Later publications may overwrite it once the Store has read them, as a collector's do.
const SLOTS: u64 = 1024;
const _: () = assert!(RANGE_MINUTES[1] * 60 < SLOTS);
const CONTINUOUS_FIRST_NS: u64 = 100_000_000_000;
const AWAKE_FIRST_NS: u64 = 80_000_000_000;

#[test]
#[ignore = "opt-in latency of a fabricated burst; takes minutes in a release build"]
fn a_cpu_burst_reaches_the_overview_line() {
    // Goal: the time from a CPU burst's first reading to the first frame whose CPU line passes
    // 70%, at the 5 and 15 minute ranges. Method: steady 10% for the range, then five bursts of
    // 100% a second, each starting a fifth of the page's tick later than the last, timing each
    // from its first publication to the frame whose newest point first passes 70%, and letting
    // the line settle back at 10% between them.
    let _cell = super::CELLS.lock().unwrap_or_else(PoisonError::into_inner);
    super::assert_timed_as_shipped();

    for minutes in RANGE_MINUTES {
        gpui::run_test_once(
            0,
            Box::new(move |dispatcher| {
                let mut harness = BurstHarness::setup(dispatcher, minutes);
                let before = harness.asked();
                for burst in 0..BURSTS {
                    let phase = crate::live_timer::TICK_INTERVAL * burst / BURSTS;
                    let passed = harness.burst(phase);
                    let line = json!({"minutes": minutes, "burst": burst,
                        "phase_ms": phase.as_millis(), "passed_ms": passed.as_secs_f64() * 1e3});
                    println!("{line}");
                    assert!(
                        passed < crate::live_timer::TICK_INTERVAL,
                        "a burst took longer than the page's tick to reach the line: {line}"
                    );
                    harness.settle_steady();
                }
                let after = harness.asked();
                let line = json!({"minutes": minutes,
                    "publications": after.publications - before.publications,
                    "mailbox_looks": after.looks.looks - before.looks.looks,
                    "views": after.looks.views - before.looks.views,
                    "requests": after.requests - before.requests});
                println!("{line}");
                harness.teardown();
            }),
        );
    }
}

/// A recording a burst is written to, open live in a test window the size of the latency
/// cells'. The collector comes last, so a failed run drops the window before the ring.
struct BurstHarness {
    window: VisualTestContext,
    dashboard: Entity<LiveDashboard>,
    clock: AppClock,
    /// When the next publication is due on the test's clock.
    due: Instant,
    collector: Collector,
}

impl BurstHarness {
    fn setup(dispatcher: gpui::TestDispatcher, minutes: u64) -> Self {
        // The range timed before this one ran on this thread and left its last point behind.
        newest_drawn::forget();
        let collector = Collector::start(minutes * 60);
        let text = std::sync::Arc::new(gpui_apple::AppleTextSystem::new());
        let mut app = TestAppContext::build_with_text_system(dispatcher, Some("burst"), text);
        let (dashboard, mut window) = super::open(&collector.root, &mut app);
        let clock = AppClock::start(&dashboard, &mut window);
        let mut harness = Self {
            window,
            dashboard,
            clock,
            due: Instant::now(),
            collector,
        };

        harness
            .window
            .simulate_resize(size(px(WINDOW_WIDTH), px(WINDOW_HEIGHT)));
        harness
            .dashboard
            .update(&mut harness.window, |dashboard, cx| {
                dashboard.set_range(crate::time_range::Range::minutes(minutes), cx);
                dashboard.navigate("overview", cx);
            });
        let drawn = harness.run_until(OPEN_WAIT, |_| newest().is_some_and(|value| value < PASS));
        assert!(drawn.is_some(), "the line never drew the steady reading");
        harness.due = Instant::now();
        harness
    }

    fn teardown(self) {
        let Self {
            mut window,
            dashboard,
            collector,
            ..
        } = self;
        crate::live_dashboard::input_tests::close_dashboard_window(&mut window, &dashboard);
        drop(window.cx.update(crate::live_dashboard::uninstall_store));
        window.cx.run_until_parked();
        window.cx.quit();
        drop(collector);
    }

    /// Starts a burst `phase` after the next publication is due and publishes 100% a second until
    /// a frame's line passes `PASS`, and answers how long that took from the first publication.
    fn burst(&mut self, phase: Duration) -> Duration {
        self.due += phase;
        let first = self.publish(BURST_PERCENT);

        for _ in 0..BURST_PUBLICATIONS_MAX {
            let due = self.due;
            let passed = self.run_until(INPUT_WAIT, |_| {
                newest().is_some_and(|value| PASS < value) || due <= Instant::now()
            });
            assert!(passed.is_some(), "the page stopped playing");
            if newest().is_some_and(|value| PASS < value) {
                return passed.unwrap().duration_since(first);
            }
            self.publish(BURST_PERCENT);
        }
        panic!("a burst never reached the line in {BURST_PUBLICATIONS_MAX} publications");
    }

    /// Publishes the steady reading until the line falls back below `PASS`, then
    /// `SETTLE_PUBLICATIONS` more.
    fn settle_steady(&mut self) {
        let mut published = 0;
        while newest().is_some_and(|value| PASS < value) {
            assert!(
                published < BURST_PUBLICATIONS_MAX,
                "the line never fell back"
            );
            self.publish(STEADY_PERCENT);
            published += 1;
        }
        for _ in 0..SETTLE_PUBLICATIONS {
            self.publish(STEADY_PERCENT);
        }
    }

    /// What the page has asked the store for so far, and the publications it could ask for.
    fn asked(&self) -> Asked {
        Asked {
            publications: self.collector.tick,
            looks: crate::live_dashboard::MAILBOX_LOOKS.with(std::cell::Cell::get),
            requests: self
                .dashboard
                .read_with(&self.window, |dashboard, _| dashboard.generation),
        }
    }

    /// Plays the app until the next publication is due, publishes `percent` and answers when it
    /// did.
    fn publish(&mut self, percent: u32) -> Instant {
        let due = self.due;
        let reached = self.run_until(INPUT_WAIT, |_| due <= Instant::now());
        assert!(reached.is_some(), "a publication never came due");

        self.collector.publish(percent);
        let published = Instant::now();
        self.due = due + PUBLICATION_INTERVAL;
        published
    }

    /// Plays the app's timers, its wake and the display until `done` holds, and answers when it
    /// first held, or none past `bound`.
    fn run_until(
        &mut self,
        bound: Duration,
        mut done: impl FnMut(&mut Self) -> bool,
    ) -> Option<Instant> {
        let started = Instant::now();
        for _ in 0..super::STEPS_MAX {
            if done(self) {
                return Some(Instant::now());
            }
            if bound < started.elapsed() {
                return None;
            }
            let next = self.clock.step(&self.dashboard, &mut self.window);
            self.clock.wait_until(next);
        }
        panic!("a wait took {} steps", super::STEPS_MAX);
    }
}

/// What the page had asked the store for by some moment.
struct Asked {
    /// The publications the collector had made.
    publications: u64,
    looks: crate::live_dashboard::MailboxLooks,
    /// The requests the page had sent the worker, each of which asks the store.
    requests: u64,
}

/// The value of the newest point Overview's CPU line drew in the last frame that drew it.
fn newest() -> Option<f32> {
    newest_drawn::value(PANEL, LINE)
}

/// A collector run on one CPU, which publishes a reading of a share of that CPU busy at a time.
/// Dropping it removes its ring and its recording.
struct Collector {
    writer: Writer,
    root: PathBuf,
    tick: u64,
    wall_first: u64,
    cpu: CPUTicks,
}

impl Collector {
    /// Creates a recording and publishes `seconds` of the steady reading, the newest at this
    /// second.
    fn start(seconds: u64) -> Self {
        let root = std::env::temp_dir().join(format!(
            "flightdeck-burst-{}",
            recording::identity_text(recording::random_identity().unwrap())
        ));
        let store = persistence::Store::open(&root).unwrap();
        persistence::retention::Period::from_seconds(86_400)
            .unwrap()
            .write(&private::PrivateDirectory::open(&root).unwrap())
            .unwrap();
        let identity = recording::new_run_identity(store.recording()).unwrap();
        drop(store);
        let place = Place {
            lock_path: root.join("burst.lock"),
            name_prefix: format!("/fdburst{}", std::process::id()),
        };
        let writer =
            Writer::create_recording(&place, SHAPE, &identity, CONTINUOUS_FIRST_NS).unwrap();
        register(&root, &identity, writer.name());
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let mut collector = Self {
            writer,
            root,
            tick: 0,
            wall_first: now - seconds,
            cpu: CPUTicks::default(),
        };

        for _ in 0..seconds {
            collector.publish(STEADY_PERCENT);
        }
        collector
    }

    /// Publishes the next second's reading, with `percent` of the CPU busy since the last.
    fn publish(&mut self, percent: u32) {
        assert!(percent <= 100);
        self.cpu.user += percent;
        self.cpu.idle += 100 - percent;
        let memory = Memory {
            total_bytes: 8_589_934_592,
            free_bytes: 4_294_967_296,
            ..Memory::default()
        };
        let times = ObservationTimes {
            wall_ns: (self.wall_first + self.tick) * 1_000_000_000,
            continuous_ns: CONTINUOUS_FIRST_NS + self.tick * 1_000_000_000,
            awake_ns: AWAKE_FIRST_NS + self.tick * 1_000_000_000,
        };
        let readings = Readings {
            run_tick: self.tick,
            cpus: &[self.cpu],
            memory: &memory,
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: Dropped::default(),
        };

        self.writer.publish_observed(&readings, &times);
        self.tick += 1;
    }
}

impl Drop for Collector {
    fn drop(&mut self) {
        let unlinked = collector::ring::unlink_generation(self.writer.name());
        let removed = std::fs::remove_dir_all(&self.root);
        if !std::thread::panicking() {
            unlinked.unwrap();
            removed.unwrap();
        }
    }
}

const SHAPE: Shape = Shape {
    slot_count: SLOTS,
    cpu_capacity: 1,
    interface_capacity: 1,
    core_capacity: 1,
    state_capacity: 1,
    fan_capacity: 1,
    process_capacity: 1,
    coalition_capacity: 1,
    event_capacity: 2,
};

/// Names the run in the recording's registry, as the collector does when it starts.
fn register(root: &std::path::Path, identity: &collector::ring::RunIdentity, ring: &str) {
    let lock = private::FileLock::wait_short(&root.join("registration.lock")).unwrap();
    let mut registry = Registry::read(root).unwrap();
    registry
        .add(
            root,
            RegisteredRun {
                identity: *identity,
                ring_name: ring.to_owned(),
                collector_pid: std::process::id(),
            },
        )
        .unwrap();
    registry.write(root).unwrap();
    drop(lock);
}
