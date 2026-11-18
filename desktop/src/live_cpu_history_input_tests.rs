use std::sync::Arc;

use gpui::{Entity, Modifiers, MouseButton, TestAppContext, VisualTestContext, WindowOptions};
use serde_json::{Value, json};

use super::{CPUHistory, NS};
use crate::live_dashboard::LiveDashboard;
use crate::live_source::{Snapshot, Worker};

#[gpui::test]
fn keyboard_history_inspection_selects_the_exact_same_second_observation(cx: &mut TestAppContext) {
    // Goal: Return selects the inspected publication rather than its same-second sibling.
    // Method: dispatch Left and Return through the production CPU history element tree.
    let mut harness = CPUHistoryHarness::setup(cx);
    harness.driver.focus_plot();

    harness.driver.press("left");
    assert_eq!(harness.driver.inspected_publication(), Some(2));
    harness.driver.press("enter");

    assert_eq!(
        harness.driver.requested_selection(),
        (
            Some(1001),
            Some(json!({
        "run":"00000000000000010000000000000002",
        "boot":"00000000000000030000000000000004","publication":2}))
        )
    );
    harness.teardown();
}

#[gpui::test]
fn keyboard_history_selection_keeps_the_pages_readings_shown(cx: &mut TestAppContext) {
    // Goal: Return on the CPU history selects the inspected second without blanking the page
    // while the reader reads that second.
    // Method: step left on the focused history, press Return, then read the page's readings
    // before any reply.
    let mut harness = CPUHistoryHarness::setup(cx);
    harness.driver.focus_plot();
    harness.driver.press("left");

    harness.driver.press("enter");

    assert!(!harness.driver.readings_are_hidden());
    harness.teardown();
}

#[gpui::test]
fn clicking_a_history_interval_pins_the_observation_under_the_pointer(cx: &mut TestAppContext) {
    // Goal: a click on the CPU history pins the exact observation the pointer inspects, as
    // Return does, and not merely its second.
    // Method: hover the interval drawn at 1000.9, which ends at an observation of second 1001
    // other than publication 3, the later observation of that second. Read the observation it
    // inspects, click it, then read the request and the pinned second.
    let mut harness = CPUHistoryHarness::setup(cx);
    let interval = 1000 * NS + NS * 9 / 10;
    harness.driver.hover_at(interval);
    let inspected = harness.driver.inspected_publication();
    assert_ne!(inspected, Some(3));

    harness.driver.click_at(interval);

    let (second, identity) = harness.driver.requested_selection();
    assert_eq!(second, Some(1001));
    assert_eq!(
        identity.and_then(|identity| identity["publication"].as_u64()),
        inspected
    );
    assert_eq!(harness.driver.pinned_second(), Some(1001));
    assert!(!harness.driver.readings_are_hidden());
    harness.teardown();
}

#[gpui::test]
fn a_maximum_held_above_the_history_stays_shown_while_the_pointer_rests(cx: &mut TestAppContext) {
    // Goal: holding a maximum closes the dialog above the CPU history, which puts its cells under
    // a pointer that has not moved. The first frame reports that as a hover, and it must not take
    // the page from the second the user just picked.
    // Method: a window whose dialog sets the maximum's control above the cells, the maximum held
    // with the pointer left on it, then whether the history inspects anything.
    let mut harness = CPUHistoryHarness::setup(cx);
    harness.driver.seed_history_with_cpu_readings();
    let name = "chart-maximum:live-cpu:All cores";
    harness
        .driver
        .context
        .simulate_resize(gpui::size(gpui::px(1_000.0), gpui::px(2_200.0)));
    harness.driver.context.run_until_parked();
    let data = harness.driver.context.debug_bounds("chart-data:live-cpu");
    harness
        .driver
        .context
        .simulate_click(data.unwrap().center(), Modifiers::none());
    harness.driver.context.run_until_parked();
    let control = harness.driver.context.debug_bounds(name).unwrap().center();
    let cells = harness.driver.context.debug_bounds("cpu-history-cells");
    assert!(cells.unwrap().contains(&control));

    harness
        .driver
        .context
        .simulate_click(control, Modifiers::none());
    harness.driver.context.run_until_parked();

    assert!(harness.driver.pinned_second().is_some());
    assert!(!harness.driver.history_is_inspected());
    harness.teardown();
}

#[gpui::test]
fn hovering_the_history_while_a_second_is_pinned_inspects_the_hovered_interval(
    cx: &mut TestAppContext,
) {
    // Goal: a hover of the CPU history inspects the interval under it while a second is
    // pinned, as it does live. Method: click the interval drawn at 1000.9, which ends at an
    // observation other than publication 3, hover the one drawn at 1001.3, which ends at
    // publication 3, and read the observation it inspects.
    let mut harness = CPUHistoryHarness::setup(cx);
    harness.driver.click_at(1000 * NS + NS * 9 / 10);

    harness.driver.hover_at(1001 * NS + NS * 3 / 10);

    assert_eq!(harness.driver.inspected_publication(), Some(3));
    harness.teardown();
}

#[gpui::test]
fn hovering_a_zoomed_history_inspects_the_interval_under_the_pointer(cx: &mut TestAppContext) {
    // Goal: once a range narrows the view, the pointer inspects the interval drawn under it
    // and not the one at the same fraction of the whole history.
    // Method: view only second 1001, hover a fifth across the cells, and read the publication.
    let mut harness = CPUHistoryHarness::setup(cx);
    harness.driver.view_only_second(1001);

    harness.driver.hover_cells(0.2);

    assert_eq!(harness.driver.inspected_publication(), Some(3));
    harness.teardown();
}

#[gpui::test]
fn dragging_across_the_history_selects_that_stretch_of_time(cx: &mut TestAppContext) {
    // Goal: a drag over the cells selects the stretch of time it covers, like a drag over a
    // line chart. Method: press a quarter across the cells, release at three quarters of the
    // 900 seconds the history spans, then read the range.
    let mut harness = CPUHistoryHarness::setup(cx);

    harness.driver.drag_cells(0.25, 0.75);

    assert_eq!(
        harness.driver.range(),
        crate::time_range::Range::Between {
            first: 327,
            last: 777
        }
    );
    harness.teardown();
}

#[gpui::test]
fn pressing_on_the_cells_leaves_the_plot_unfocused(cx: &mut TestAppContext) {
    // Goal: a press that starts a drag does not focus the plot, since a focused plot draws
    // its focus ring as a solid fill over the cells. Method: press on the cells and ask the
    // plot's focus handle whether the window focuses it.
    let mut harness = CPUHistoryHarness::setup(cx);

    harness.driver.press_cells(0.5);

    assert!(!harness.driver.plot_is_focused());
    harness.teardown();
}

#[gpui::test]
fn a_hover_on_the_history_of_a_page_opened_again_before_its_reading_shows_its_observation(
    cx: &mut TestAppContext,
) {
    // Goal: the CPU history names each observation it draws, so a hover on it shows the hovered
    // observation on a page opened again before its next reading, as before the page was left,
    // although the reading the page kept holds none of its charts' points. Method: the CPU page
    // following the live Mac over the history, Overview, the CPU page again, a hover over the
    // interval drawn at 1000.9, then the second shown and the observation inspected.
    let mut harness = CPUHistoryHarness::setup_following(cx);
    harness.driver.open_cpu_again();

    harness.driver.hover_at(1000 * NS + NS * 9 / 10);

    assert_eq!(harness.driver.shown_reading_second(), Some(1001));
    assert_eq!(harness.driver.inspected_publication(), Some(2));
    harness.teardown();
}

#[gpui::test]
fn a_click_on_the_history_of_a_page_opened_again_before_its_reading_pins_its_observation(
    cx: &mut TestAppContext,
) {
    // Goal: a click on the CPU history of a page opened again before its next reading pins the
    // observation under the pointer, as it does before the page is left. Method: the CPU page
    // following the live Mac over the history, Overview, the CPU page again, a click on the
    // interval drawn at 1000.9, then the pinned second and the observation the page asks for.
    let mut harness = CPUHistoryHarness::setup_following(cx);
    harness.driver.open_cpu_again();

    harness.driver.click_at(1000 * NS + NS * 9 / 10);

    assert_eq!(harness.driver.pinned_second(), Some(1001));
    let (_, identity) = harness.driver.requested_selection();
    assert_eq!(identity.unwrap()["publication"], json!(2));
    harness.teardown();
}

#[gpui::test]
fn a_hover_on_the_history_of_a_page_opened_again_before_its_reading_reads_its_observation(
    cx: &mut TestAppContext,
) {
    // Goal: the reading a page kept when it was left lost every other observation's summary
    // with its charts' points, so a hover on the CPU history of a page opened again before its
    // next reading asks the reader for the hovered observation, although the kept ranks and CPU
    // history hold the rest of its readings. Method: the CPU page following the live Mac over
    // the history with the ranks of each observation it draws, Overview, the CPU page again, a
    // hover over the interval drawn at 1000.9 and a tick of the hover, then the observation the
    // reader was asked for.
    let mut harness = CPUHistoryHarness::setup_following(cx);
    harness.driver.keep_window_ranks();
    harness.driver.open_cpu_again();

    harness.driver.hover_at(1000 * NS + NS * 9 / 10);
    harness.driver.tick_hover();

    let asked = harness.driver.reader_asked_identity();
    assert_eq!(
        asked.map(|identity| identity["publication"].clone()),
        Some(json!(2))
    );
    harness.teardown();
}

#[gpui::test]
fn a_hover_on_the_history_of_a_page_opened_again_shows_the_reading_the_reader_answers(
    cx: &mut TestAppContext,
) {
    // Goal: the reader's answer to a hover on the CPU history of a page opened again before its
    // next reading shows the hovered observation's reading in the CPU readouts. Method: the CPU
    // page following the live Mac over the history with the ranks of each observation it draws,
    // Overview, the CPU page again, a hover over the interval drawn at 1000.9 and a tick of the
    // hover, the reader's answer, then the readouts against that observation's reading.
    let mut harness = CPUHistoryHarness::setup_following(cx);
    harness.driver.keep_window_ranks();
    harness.driver.open_cpu_again();
    harness.driver.hover_at(1000 * NS + NS * 9 / 10);
    harness.driver.tick_hover();

    harness.driver.answer_hover(hovered_observation_selection());

    let expected = crate::live_views::summary::cards("cpu", &hovered_observation_selection());
    assert_eq!(
        harness.driver.readout_texts(),
        crate::live_store_page_fixtures::texts(&expected)
    );
    harness.teardown();
}

#[gpui::test]
fn the_cells_of_a_history_take_one_place_in_the_draw_order(cx: &mut TestAppContext) {
    // Goal: GPUI gives the cells of the CPU history one place in its draw order, rather than
    // searching for a place for each of the thousands a history holds.
    // Method: draw two minutes of readings of one colour in a plot of fifteen minutes. A cell is
    // then narrower than a pixel, and GPUI moves the edges of some neighbours onto the same
    // pixels, so those cover one another and would take different places if each took its own.
    // Check that some do, then read the draw order of every cell.
    let mut harness = CPUHistoryHarness::setup(cx);

    harness.driver.seed_two_minutes_of_history();

    let cells = harness.driver.painted(half_busy_color());
    assert!(
        some_overlap(&cells),
        "No two cells cover one another, so their draw orders show nothing."
    );
    let orders = cells.iter().map(|cell| cell.order);
    assert_eq!(orders.clone().min(), orders.max());
    harness.teardown();
}

#[gpui::test]
fn the_cells_of_a_history_draw_over_their_row_and_under_the_cursor(cx: &mut TestAppContext) {
    // Goal: a cell covers the ground of its row and the cursor covers the cells.
    // Method: draw two minutes of readings, then compare the draw orders of the row's ground,
    // of the cells and of the cursor's edge wherever two of them overlap.
    let mut harness = CPUHistoryHarness::setup(cx);

    harness.driver.seed_two_minutes_of_history();

    let row = harness.driver.painted(gpui::rgb(crate::charts::GRID));
    let cells = harness.driver.painted(half_busy_color());
    let cursor_edge = harness.driver.painted(crate::charts::alpha(
        super::view::CURSOR_EDGE,
        super::view::CURSOR_EDGE_OPACITY,
    ));
    assert_eq!(row.len(), 1);
    assert_eq!(cursor_edge.len(), 1);
    assert_draws_over(&cells, &row);
    assert_draws_over(&cursor_edge, &cells);
    harness.teardown();
}

#[gpui::test]
fn the_cells_of_each_cpu_stand_in_its_own_row(cx: &mut TestAppContext) {
    // Goal: the cells of each logical CPU are drawn in that CPU's row and in no other.
    // Method: draw two minutes of two CPUs, the first half busy and the second fully, so their
    // cells differ in colour, then compare where the cells of each colour stand with the
    // grounds of the two rows, the upper row's first.
    let mut harness = CPUHistoryHarness::setup(cx);

    harness.driver.seed_two_minutes_of_two_cpus();

    let rows = harness.driver.painted(gpui::rgb(crate::charts::GRID));
    let upper = harness.driver.painted(half_busy_color());
    let lower = harness
        .driver
        .painted(crate::charts::utilization_color(100.0));
    assert_eq!(rows.len(), 2);
    assert!(rows[0].bounds.top() < rows[1].bounds.top());
    assert_stand_in(&upper, &rows[0]);
    assert_stand_in(&lower, &rows[1]);
    harness.teardown();
}

/// The colour of a cell of `seed_two_minutes_of_history`.
fn half_busy_color() -> gpui::Rgba {
    crate::charts::utilization_color(HALF_BUSY_PERCENT)
}

/// Whether any two of `quads` cover a part of one another.
fn some_overlap(quads: &[gpui::Quad]) -> bool {
    quads.iter().enumerate().any(|(index, quad)| {
        quads[index + 1..]
            .iter()
            .any(|later| quad.bounds.intersects(&later.bounds))
    })
}

/// Every quad of `above` comes later in the draw order than each quad of `below` it overlaps,
/// and at least one pair overlaps.
#[track_caller]
fn assert_draws_over(above: &[gpui::Quad], below: &[gpui::Quad]) {
    let mut overlap_count = 0;
    for upper in above {
        for lower in below {
            if upper.bounds.intersects(&lower.bounds) {
                overlap_count += 1;
                assert!(upper.order > lower.order, "{upper:?} over {lower:?}");
            }
        }
    }
    assert!(
        overlap_count > 0,
        "No quad of the one overlaps a quad of the other."
    );
}

/// There are `cells`, and each stands between the top and the bottom of `row`.
#[track_caller]
fn assert_stand_in(cells: &[gpui::Quad], row: &gpui::Quad) {
    assert!(!cells.is_empty());
    for cell in cells {
        assert_eq!(cell.bounds.top(), row.bounds.top(), "{cell:?} in {row:?}");
        assert_eq!(
            cell.bounds.bottom(),
            row.bounds.bottom(),
            "{cell:?} in {row:?}"
        );
    }
}

/// The reader's reading of publication 2, the observation drawn at 1000.9, whose CPU was 13%
/// busy.
fn hovered_observation_selection() -> Value {
    let (payload, _) = super::tests::fixture();
    let observation = payload["observations"][1]["observation"].clone();
    let mut summary = observation.clone();
    summary["cpu_percent"] = json!(13.0);

    json!({"second":1001,"context_last_second":1001,"selected":observation,
        "summary":summary,"trends":[]})
}

/// The observations `seed_two_minutes_of_history` draws, one a second.
const TWO_MINUTES_OBSERVATION_COUNT: u64 = 120;
/// How busy the first logical CPU is in every interval between them.
const HALF_BUSY_PERCENT: f64 = 50.0;

struct CPUHistoryHarness {
    driver: CPUHistoryDriver,
}

impl CPUHistoryHarness {
    fn setup(cx: &mut TestAppContext) -> Self {
        let worker = Worker::start(std::env::temp_dir().join(format!(
            "flightdeck-cpu-input-contract-{}",
            std::process::id()
        )))
        .unwrap();
        let window = cx.update(|cx| {
            cx.open_window(WindowOptions::default(), |_, cx| {
                crate::native_workflows::create_dashboard(worker, "cpu".into(), None, cx)
            })
            .unwrap()
        });
        let view = window.root(cx).unwrap();
        let context = VisualTestContext::from_window(window.into(), cx);
        let mut harness = Self {
            driver: CPUHistoryDriver { view, context },
        };
        harness.driver.seed_history();

        harness
    }

    /// Opens the CPU page following the live Mac over the CPU history.
    fn setup_following(cx: &mut TestAppContext) -> Self {
        let mut harness = Self::setup(cx);
        harness
            .driver
            .view
            .update(&mut harness.driver.context, |dashboard, _| {
                dashboard.request.second = None;
                dashboard.request.identity = None;
            });
        harness
            .driver
            .accept_history(CPUHistoryDriver::seed_history_fixture());

        harness
    }

    fn teardown(self) {
        drop(self);
    }
}

impl Drop for CPUHistoryHarness {
    fn drop(&mut self) {
        crate::live_dashboard::input_tests::close_dashboard_window(
            &mut self.driver.context,
            &self.driver.view,
        );
    }
}

struct CPUHistoryDriver {
    view: Entity<LiveDashboard>,
    context: VisualTestContext,
}

struct CPUHistoryFixture {
    payload: Value,
    selection: Value,
}

impl CPUHistoryDriver {
    fn seed_history(&mut self) {
        let fixture = Self::seed_history_fixture();
        self.install_history(fixture);
    }

    /// `seed_history`, with the CPU reading at each second of the chart above the history.
    fn seed_history_with_cpu_readings(&mut self) {
        let mut fixture = Self::seed_history_fixture();
        for trend in fixture.selection["trends"].as_array_mut().unwrap() {
            trend["cpu_percent"] = json!(50.0);
        }
        self.install_history(fixture);
    }

    fn seed_history_fixture() -> CPUHistoryFixture {
        let (mut payload, mut selection) = super::tests::fixture();
        payload["observation"]["publication"] = json!(3);
        payload["observation"]["run_tick"] = json!(3);
        payload["observation"]["wall_ns"] = json!(1001 * NS + NS * 2 / 5);
        let selected = payload["observation"].clone();
        let previous = payload["observations"][1]["observation"].clone();
        payload["observations"].as_array_mut().unwrap().push(json!({
            "observation":selected,"previous_observation":previous,
            "interval_available":true,"cpu_count":1}));
        payload["cpus"][0]["busy_percent"]
            .as_array_mut()
            .unwrap()
            .push(json!(50.0));
        selection["selected"] = selected.clone();
        selection["trends"].as_array_mut().unwrap().push(selected);

        CPUHistoryFixture { payload, selection }
    }

    fn install_history(&mut self, fixture: CPUHistoryFixture) {
        let selected = fixture.selection["selected"].clone();
        self.view.update(&mut self.context, |dashboard, _| {
            dashboard.request.second = Some(1001);
            dashboard.request.identity = Some(selected);
        });
        self.accept_history(fixture);
    }

    /// Hands the page the reader's answer to its request, the readings of `fixture`.
    fn accept_history(&mut self, fixture: CPUHistoryFixture) {
        let history = Arc::new(CPUHistory::decode(fixture.payload, &fixture.selection).unwrap());
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.accept(Box::new(Snapshot {
                generation: dashboard.generation,
                catalog: json!({"first_second":1000,"last_second":1001}),
                selection: crate::live_source::Presentation::Legacy(fixture.selection),
                apps: None,
                detail: None,
                history: None,
                cpu_detail: None,
                cpu_history: Some(std::sync::Arc::new(
                    crate::live_source::Presentation::Shared(history),
                )),
                resident_history: None,
                app_attribution_history: None,
                overview_app_histories: None,
                histories_pending: false,
                points_dropped: false,
                app_chart_source: crate::live_source::AppChartSource::Reading,
                process_history: None,
                rankings: None,
                app_ranks: None,
                app_parts: None,
                read_ms: 0.0,
            }));
            cx.notify();
        });
        let error = self
            .view
            .read_with(&self.context, |dashboard, _| dashboard.error.clone());
        assert!(error.is_none(), "{error:?}");
        self.context.run_until_parked();
    }

    /// `seed_history`, with an observation at each of the 120 seconds up to 1001, every interval
    /// between them 50% busy.
    fn seed_two_minutes_of_history(&mut self) {
        self.install_history(Self::two_minutes_fixture());
    }

    /// `seed_two_minutes_of_history`, with a second logical CPU that is 100% busy in every
    /// interval.
    fn seed_two_minutes_of_two_cpus(&mut self) {
        let mut fixture = Self::two_minutes_fixture();
        let busy_percent: Vec<Value> = fixture.payload["cpus"][0]["busy_percent"]
            .as_array()
            .unwrap()
            .iter()
            .map(|reading| reading.as_f64().map_or(Value::Null, |_| json!(100.0)))
            .collect();
        fixture.payload["cpus"]
            .as_array_mut()
            .unwrap()
            .push(json!({"cpu":1,"busy_percent":busy_percent}));
        fixture.payload["total"] = json!(2);
        for observation in fixture.payload["observations"].as_array_mut().unwrap() {
            observation["cpu_count"] = json!(2);
        }
        self.install_history(fixture);
    }

    fn two_minutes_fixture() -> CPUHistoryFixture {
        let (mut payload, mut selection) = super::tests::fixture();
        let mut observations = Vec::new();
        let mut trends = Vec::new();
        let mut busy_percent = Vec::new();
        let mut previous = Value::Null;
        for publication in 1..=TWO_MINUTES_OBSERVATION_COUNT {
            let mut observation = payload["observation"].clone();
            observation["publication"] = json!(publication);
            observation["run_tick"] = json!(publication);
            observation["wall_ns"] =
                json!((1001 + publication - TWO_MINUTES_OBSERVATION_COUNT) * NS);
            observations.push(json!({
                "observation":observation,"previous_observation":previous,
                "interval_available":publication > 1,"cpu_count":1}));
            busy_percent.push(if publication > 1 {
                json!(HALF_BUSY_PERCENT)
            } else {
                Value::Null
            });
            trends.push(observation.clone());
            previous = observation;
        }
        payload["observation"] = previous.clone();
        payload["observations"] = json!(observations);
        payload["cpus"][0]["busy_percent"] = json!(busy_percent);
        selection["selected"] = previous;
        selection["trends"] = json!(trends);

        CPUHistoryFixture { payload, selection }
    }

    /// Each quad of `color` that the frame paints over the history's cells.
    fn painted(&mut self, color: impl Into<gpui::Background>) -> Vec<gpui::Quad> {
        let cells = self.context.debug_bounds("cpu-history-cells").unwrap();
        let color = color.into();
        self.context.update(|window, _| {
            let cells = cells.scale(window.scale_factor());
            window
                .painted_quads()
                .into_iter()
                .filter(|quad| quad.background == color)
                .filter(|quad| quad.bounds.intersects(&cells))
                .collect()
        })
    }

    fn focus_plot(&mut self) {
        let bounds = self.context.debug_bounds("cpu-history-plot").unwrap();
        self.context
            .simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::none());
        self.context
            .simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::none());
    }

    fn press(&mut self, keys: &str) {
        self.context.simulate_keystrokes(keys);
    }

    fn hover_at(&mut self, wall_ns: u64) {
        let position = self.position_at(wall_ns);
        self.context
            .simulate_mouse_move(position, None, Modifiers::none());
        self.context.run_until_parked();
    }

    fn click_at(&mut self, wall_ns: u64) {
        let position = self.position_at(wall_ns);
        self.context.simulate_click(position, Modifiers::none());
    }

    fn pinned_second(&self) -> Option<u64> {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.pinned_second())
    }

    /// Where the cells draw `wall_ns`, at their middle row.
    fn position_at(&mut self, wall_ns: u64) -> gpui::Point<gpui::Pixels> {
        let bounds = self.context.debug_bounds("cpu-history-cells").unwrap();
        let fraction = self.view.read_with(&self.context, |dashboard, _| {
            dashboard
                .snapshot
                .as_ref()
                .unwrap()
                .cpu_history
                .as_ref()
                .unwrap()
                .fraction(wall_ns)
        });
        let fraction = num_traits::ToPrimitive::to_f32(&fraction).unwrap();
        gpui::point(
            bounds.origin.x + bounds.size.width * fraction,
            bounds.center().y,
        )
    }

    fn drag_cells(&mut self, from: f32, to: f32) {
        let bounds = self.context.debug_bounds("cpu-history-cells").unwrap();
        let at = |fraction: f32| {
            gpui::point(
                bounds.origin.x + bounds.size.width * fraction,
                bounds.center().y,
            )
        };
        self.context
            .simulate_mouse_down(at(from), MouseButton::Left, Modifiers::none());
        self.context
            .simulate_mouse_move(at(to), Some(MouseButton::Left), Modifiers::none());
        self.context
            .simulate_mouse_up(at(to), MouseButton::Left, Modifiers::none());
        self.context.run_until_parked();
    }

    fn press_cells(&mut self, fraction: f32) {
        let bounds = self.context.debug_bounds("cpu-history-cells").unwrap();
        let position = gpui::point(
            bounds.origin.x + bounds.size.width * fraction,
            bounds.center().y,
        );
        self.context
            .simulate_mouse_down(position, MouseButton::Left, Modifiers::none());
        self.context.run_until_parked();
    }

    fn plot_is_focused(&mut self) -> bool {
        let focus = self.view.read_with(&self.context, |dashboard, _| {
            dashboard.cpu_history_focus.clone().unwrap()
        });

        self.context.update(|window, _| focus.is_focused(window))
    }

    fn range(&self) -> crate::time_range::Range {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.range)
    }

    fn view_only_second(&mut self, second: u64) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.range = crate::time_range::Range::Between {
                first: second,
                last: second,
            };
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn hover_cells(&mut self, fraction: f32) {
        let bounds = self.context.debug_bounds("cpu-history-cells").unwrap();
        let position = gpui::point(
            bounds.origin.x + bounds.size.width * fraction,
            bounds.center().y,
        );
        self.context
            .simulate_mouse_move(position, None, Modifiers::none());
        self.context.run_until_parked();
    }

    fn inspected_publication(&self) -> Option<u64> {
        self.view.read_with(&self.context, |dashboard, _| {
            let history = dashboard.snapshot.as_ref()?.cpu_history.as_ref()?;
            let inspection = dashboard.cpu_inspection.or_else(|| history.selected())?;
            history.target(inspection)?.1["publication"].as_u64()
        })
    }

    fn history_is_inspected(&self) -> bool {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.cpu_inspection.is_some()
        })
    }

    fn requested_selection(&self) -> (Option<u64>, Option<Value>) {
        self.view.read_with(&self.context, |dashboard, _| {
            (dashboard.request.second, dashboard.request.identity.clone())
        })
    }

    /// Leaves the CPU page for Overview and opens the CPU page again, whose next reading is
    /// still to come.
    fn open_cpu_again(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.navigate("overview", cx);
            dashboard.navigate("cpu", cx);
        });
        self.context.run_until_parked();
    }

    /// Keeps the ranks of each observation of a sealed recording's window beside the page's
    /// readings, whose observations share the run and boot of the history's.
    fn keep_window_ranks(&mut self) {
        let ranks = crate::live_source::tests::rankings::kept_window_ranks();
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.snapshot.as_mut().unwrap().app_ranks = Some(ranks);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn tick_hover(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick(cx);
        });
        self.context.run_until_parked();
    }

    /// The observation the window's last command asked the reader for.
    fn reader_asked_identity(&self) -> Option<Value> {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.worker.sent_request()?.identity
        })
    }

    /// Hands the hover the reader's answer to its read, a reading whose selection is
    /// `selection`.
    fn answer_hover(&mut self, selection: Value) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let generation = dashboard.preview.as_ref().unwrap().generation.unwrap();
            let reply = Box::new(Snapshot {
                generation,
                catalog: json!({"first_second":1000,"last_second":1001}),
                selection: crate::live_source::Presentation::Legacy(selection),
                apps: None,
                detail: None,
                history: None,
                cpu_detail: None,
                cpu_history: None,
                resident_history: None,
                app_attribution_history: None,
                overview_app_histories: None,
                histories_pending: false,
                points_dropped: false,
                app_chart_source: crate::live_source::AppChartSource::Reading,
                process_history: None,
                rankings: None,
                app_ranks: None,
                app_parts: None,
                read_ms: 0.0,
            });
            assert!(dashboard.take_update(crate::live_source::Update::Ready(reply)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    /// The label, reading, note and colour of each of the readouts the page shows.
    fn readout_texts(&self) -> Vec<(String, String, String, u32)> {
        self.view.read_with(&self.context, |dashboard, _| {
            crate::live_store_page_fixtures::texts(&dashboard.summary_cards())
        })
    }

    /// The second of the reading a hover or a step shows, if one shows.
    fn shown_reading_second(&self) -> Option<u64> {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.reading_target().map(|target| target.second)
        })
    }

    fn readings_are_hidden(&self) -> bool {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.snapshot.is_none())
    }
}
