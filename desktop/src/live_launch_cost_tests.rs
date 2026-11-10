//! The cost cell: what following the Mac costs this process on every page and on a
//! single chart, in CPU time and energy, on W10's reference input. It opens the same frozen copy
//! in the same test window and plays the same clock as the latency cells beside it, and reads
//! this process's own counters through the collector's reader of every process's, so the Store,
//! the Polars threads and the window's work count, and the freezer, which plays the collector's
//! part in a process of its own, does not. It runs under the latency cells' protocol:
//!
//! ```text
//! FLIGHTDECK_REFERENCE_RECORDING=<W10's hour> FLIGHTDECK_REFERENCE_FREEZER=<resume_bench>
//! FLIGHTDECK_REFERENCE_OUTPUT=<directory> POLARS_MAX_THREADS=6 POLARS_ASYNC_THREAD_COUNT=1
//! cargo test --release --manifest-path desktop/Cargo.toml --features automation \
//!     --bin flightdeck cost_tests -- --ignored
//! ```
//!
//! A page's figure is the cost of a wall second of following the Mac, a second revealed at its
//! start and the page's tick and the display's frames played to its end, as the app follows a
//! recording that publishes a second each second. A page leaves part of a second's work, such as
//! its histories, to run after the page shows the second, so a shorter span would leave that
//! work to the next and a faster pace would cut it short. The figures leave out what the latency
//! cells leave out: the GPU's drawing, the work the app's tick adds to the page's own, and the
//! collector and persister, which are other processes.
use std::io::Write;
use std::rc::Rc;
use std::sync::PoisonError;
use std::time::{Duration, Instant};

use collector::clock::Timebase;
use collector::processes::Usage;
use gpui::{
    Context, Entity, IntoElement, Pixels, Render, VisualTestContext, Window, WindowOptions,
    WindowVisibility, div, prelude::*, px,
};
use num_traits::ToPrimitive;
use serde_json::{Value, json};

use super::{CELLS, INPUT_WAIT, Opening, Protocol, REPLAY_REVEALS, ReferenceHarness};
use crate::data::Panel;
use crate::live_chart_view::{ChartLook, PLOT_HEIGHT};
use crate::live_dashboard::ROUTES;
use crate::live_plot::{Plots, Shown};

/// Five pairs of runs, the pages in their order in the first run of a pair and in reverse in
/// the second, so the spread over the pairs is the variation a cut on one build must beat.
const PAIRS: usize = 5;
const RUNS: usize = 2 * PAIRS;
/// The wall seconds each page follows in a run after an untimed first, which takes the work its
/// opening left, and the publications the chart follows.
const FOLLOWED: usize = 5;
const _: () = assert!(ROUTES.len() * (FOLLOWED + 1) + FOLLOWED <= REPLAY_REVEALS);
/// A wall second, the pace at which the recording publishes.
const WALL_SECOND: Duration = Duration::from_secs(1);
/// The page whose first chart is the single chart.
const CHART_ROUTE: &str = "cpu";
/// How many times over the chart draws its publications, enough for the energy counter.
const CHART_LAPS: usize = 50;

#[test]
#[ignore = "opt-in cost on W10's reference input; needs FLIGHTDECK_REFERENCE_*"]
fn following_the_mac_costs_every_page_and_a_single_chart() {
    // Goal: the cost table, the CPU time and energy this process spends following the Mac on
    // each page and on a single chart, with the spread of five alternating pairs of runs.
    // Method: each run opens the copy and warms every page, then on each page in turn follows
    // the Mac an untimed wall second and then `FOLLOWED` more, reading the process's counters
    // around each. Then it takes `FOLLOWED` publications on CPU & Processes, keeping its first
    // chart as each drew it, hides the window so the Store parks, and draws that chart alone in
    // a window of its own `CHART_LAPS` times over, as its page draws it, reading the counters
    // around all of it.
    let _cell = CELLS.lock().unwrap_or_else(PoisonError::into_inner);
    let protocol = Protocol::read();
    let mut lines = std::fs::File::create_new(protocol.output.join("cost.jsonl")).unwrap();
    let mut costs = Vec::with_capacity(RUNS * (ROUTES.len() * FOLLOWED + 1));

    for run in 0..RUNS {
        let protocol = protocol.clone();
        let run_costs = gpui::run_test_once(
            0,
            Box::new(move |dispatcher| {
                let counters = Counters::new();
                let opening = Opening::Replay { cell: "cost" };
                let mut harness = ReferenceHarness::setup(&protocol, dispatcher, opening);
                harness.warm();
                let mut routes = ROUTES.map(|(route, _)| route);
                if run % 2 == 1 {
                    routes.reverse();
                }
                let mut costs = Vec::with_capacity(ROUTES.len() * FOLLOWED + 1);

                for route in routes {
                    harness.open_page(route);
                    follow_a_wall_second(&mut harness);
                    for _ in 0..FOLLOWED {
                        let before = counters.read();
                        follow_a_wall_second(&mut harness);
                        costs.push(counters.cost_since(&before, run, route, "second", 1));
                    }
                }
                costs.push(single_chart_cost(&mut harness, &counters, run));

                harness.teardown();
                costs
            }),
        );
        for cost in &run_costs {
            writeln!(lines, "{cost}").unwrap();
        }
        costs.extend(run_costs);
    }

    let summary = summarize(&costs);
    std::fs::write(
        protocol.output.join("cost-summary.json"),
        format!("{summary:#}\n"),
    )
    .unwrap();
    println!("{summary:#}");
    for target in summary["targets"].as_array().unwrap() {
        assert!(
            0.0 < target["cpu_ms"]["median"].as_f64().unwrap(),
            "{target}"
        );
    }
}

/// Reveals the next second and plays the app's clock to the end of a wall second, with the page
/// settled on that second.
fn follow_a_wall_second(harness: &mut ReferenceHarness) {
    let started = Instant::now();
    harness.take_next_second();
    let waited = harness.run_until(INPUT_WAIT, |_| WALL_SECOND <= started.elapsed());
    assert!(waited.is_some(), "the clock never reached the second's end");
    harness.settle(INPUT_WAIT);
}

/// Takes `FOLLOWED` publications on `CHART_ROUTE`, keeping its first chart as the page drew each,
/// then hides the page's window and draws that chart alone for each, answering the cost of a
/// publication's drawing.
fn single_chart_cost(harness: &mut ReferenceHarness, counters: &Counters, run: usize) -> Value {
    let drawn = single_chart_cost_publications(harness);
    let laps = drawn.iter().cycle().take(CHART_LAPS * FOLLOWED);
    let rebuilt = 1 + laps
        .clone()
        .zip(laps.skip(1))
        .filter(|(drawn, next)| !Rc::ptr_eq(&drawn.0, &next.0))
        .count();
    let rebuilt = u64::try_from(rebuilt).unwrap();
    let width = single_chart_width(harness, &drawn);
    single_chart_cost_park(harness);

    let window = harness
        .window
        .cx
        .update(|cx| {
            cx.open_window(WindowOptions::default(), |_, cx| {
                cx.new(|_| ChartAlone {
                    plots: Plots::new(),
                    width,
                    drawn: None,
                })
            })
        })
        .unwrap();
    let chart: Entity<ChartAlone> = window.root(&mut harness.window.cx).unwrap();
    let mut chart_window = VisualTestContext::from_window(window.into(), &harness.window.cx);

    // A frame of one chart takes too little time for the energy counter, which a span of a
    // millisecond leaves at 0, so the publications are drawn `CHART_LAPS` times over in one
    // span, and the span's cost is shared among them.
    let before = counters.read();
    for _ in 0..CHART_LAPS {
        for (panel, look) in &drawn {
            chart.update(&mut chart_window, |chart, cx| {
                chart.drawn = Some((Rc::clone(panel), look.clone()));
                cx.notify();
            });
            chart_window.update(Window::simulate_next_frame);
            chart_window.run_until_parked();
        }
    }
    let drawings = u32::try_from(CHART_LAPS * FOLLOWED).unwrap();
    let cost = counters.cost_since(&before, run, "chart", "publication", drawings);

    let renders = chart.read_with(&chart_window, |chart, _| chart.plots.renders());
    assert_eq!(renders, rebuilt, "a new panel drew no new plot");
    chart_window.update(|window, _| window.remove_window());
    chart_window.run_until_parked();
    cost
}

/// Opens `CHART_ROUTE` and takes `FOLLOWED` publications, answering its first chart's panel and
/// look as the page drew each. The page builds a chart's panel again only when the chart
/// changes, so a publication may leave it as it was, and its plot then draws nothing.
fn single_chart_cost_publications(harness: &mut ReferenceHarness) -> Vec<(Rc<Panel>, ChartLook)> {
    harness.open_page(CHART_ROUTE);
    let mut drawn = Vec::with_capacity(FOLLOWED);
    for _ in 0..FOLLOWED {
        harness.take_next_second();
        drawn.push(
            harness
                .dashboard
                .read_with(&harness.window, |dashboard, _| {
                    let panel = dashboard
                        .panels
                        .iter()
                        .find(|panel| panel.kind != "timeline")
                        .expect("CPU & Processes draws a chart");
                    (Rc::clone(panel), dashboard.chart_look(panel))
                }),
        );
    }
    drawn
}

/// How wide the page laid out the plot of the chart `drawn` holds.
fn single_chart_width(harness: &mut ReferenceHarness, drawn: &[(Rc<Panel>, ChartLook)]) -> Pixels {
    let (panel, _) = drawn.last().expect("the page drew the chart");
    let selector: &'static str = Box::leak(format!("{}-plot", panel.id).into_boxed_str());
    harness
        .window
        .debug_bounds(selector)
        .expect("the page laid the chart out")
        .size
        .width
}

/// Hides the page's window and waits until the Store parks, so only the chart draws.
fn single_chart_cost_park(harness: &mut ReferenceHarness) {
    harness
        .window
        .simulate_visibility_change(WindowVisibility::Hidden);
    let parked = harness.run_until(INPUT_WAIT, |harness| {
        harness
            .window
            .update(|_, cx| crate::live_dashboard::store_activity(cx)["paused"] == true)
    });
    assert!(parked.is_some(), "the Store never parked");
}

/// A window that holds one chart's plot, as its page draws it, as wide as the page laid it out.
struct ChartAlone {
    plots: Plots,
    width: Pixels,
    drawn: Option<(Rc<Panel>, ChartLook)>,
}

impl Render for ChartAlone {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.plots.begin_frame();
        let Some((panel, look)) = &self.drawn else {
            return div();
        };
        let shown = Shown::new(
            Rc::clone(panel),
            look.inspection,
            Rc::clone(&look.hidden),
            look.time,
        );
        div()
            .w(self.width)
            .h(px(PLOT_HEIGHT))
            .child(self.plots.plot(shown, cx).0)
    }
}

/// This process's own counters, which `proc_pid_rusage` gives for any process.
struct Counters {
    timebase: Timebase,
    pid: i32,
}

/// What a span of the cell cost, by the counters read at its start.
struct Reading {
    usage: Usage,
    at: Instant,
}

impl Counters {
    fn new() -> Self {
        Self {
            timebase: Timebase::read().unwrap(),
            pid: collector::processes::own_pid().unwrap(),
        }
    }

    fn read(&self) -> Reading {
        let usage = collector::processes::read_usage(self.timebase, self.pid)
            .unwrap()
            .unwrap();
        Reading {
            usage,
            at: Instant::now(),
        }
    }

    /// The cost since `before` of each of the `spans` of `target`'s `kind` in `run`, which ran
    /// one after another, as one line of the cell.
    fn cost_since(
        &self,
        before: &Reading,
        run: usize,
        target: &str,
        kind: &str,
        spans: u32,
    ) -> Value {
        assert!(0 < spans);
        let after = self.read();
        let cpu_ns = (after.usage.user_ns + after.usage.system_ns)
            - (before.usage.user_ns + before.usage.system_ns);
        let per_span = |total: f64| total / f64::from(spans);
        json!({
            "run": run, "target": target, "kind": kind, "spans": spans,
            "wall_ms": per_span((after.at - before.at).as_secs_f64() * 1000.0),
            "cpu_ms": per_span(nanos_to_milli(cpu_ns)),
            "energy_mj": per_span(nanos_to_milli(after.usage.energy_nj - before.usage.energy_nj)),
            "idle_wakeups": per_span(count(after.usage.idle_wakeups - before.usage.idle_wakeups)),
            "interrupt_wakeups": per_span(count(
                after.usage.interrupt_wakeups - before.usage.interrupt_wakeups,
            )),
        })
    }
}

/// `nanos` billionths as thousandths, which a figure of the cell is in.
fn nanos_to_milli(nanos: u64) -> f64 {
    count(nanos) / 1e6
}

/// A counter's `value`, exact below 2^53, which no span of the cell reaches.
fn count(value: u64) -> f64 {
    assert!(value < 1 << 53);
    value.to_f64().unwrap()
}

/// Each target's and kind's mean cost in every run, and the median, least and most of those
/// means, the spread over the runs being the variation a cut must beat.
fn summarize(costs: &[Value]) -> Value {
    let mut keys: Vec<(String, String)> = costs
        .iter()
        .map(|cost| {
            (
                cost["target"].as_str().unwrap().to_owned(),
                cost["kind"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    keys.sort();
    keys.dedup();

    let targets = keys
        .iter()
        .map(|(target, kind)| {
            let spans: Vec<&Value> = costs
                .iter()
                .filter(|cost| cost["target"] == target.as_str() && cost["kind"] == kind.as_str())
                .collect();
            let figures = ["wall_ms", "cpu_ms", "energy_mj"].map(|figure| {
                let mut means: Vec<f64> = (0..RUNS)
                    .map(|run| {
                        let run_spans: Vec<f64> = spans
                            .iter()
                            .filter(|cost| cost["run"] == run)
                            .map(|cost| cost[figure].as_f64().unwrap())
                            .collect();
                        assert!(!run_spans.is_empty(), "{target} {kind} in run {run}");
                        let count = u32::try_from(run_spans.len()).unwrap();
                        run_spans.iter().sum::<f64>() / f64::from(count)
                    })
                    .collect();
                means.sort_by(f64::total_cmp);
                // An even count's median is the mean of its two middle means.
                let median = f64::midpoint(means[RUNS / 2 - 1], means[RUNS / 2]);
                (
                    figure,
                    json!({"median": median, "least": means[0], "most": means[RUNS - 1]}),
                )
            });
            let mut target_summary = json!({"target": target, "kind": kind, "spans": spans.len()});
            for (figure, summary) in figures {
                target_summary[figure] = summary;
            }
            target_summary
        })
        .collect::<Vec<Value>>();

    json!({
        "runs": RUNS, "followed": FOLLOWED,
        "scope": "this process in a test window; leaves out the GPU's drawing, the app's tick \
                  beyond the page's own, and the collector and persister",
        "targets": targets,
    })
}
