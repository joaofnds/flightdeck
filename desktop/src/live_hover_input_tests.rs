use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use gpui::{Entity, Modifiers, TestAppContext, VisualTestContext, WindowOptions};
use serde_json::{Value, json};

use crate::live_dashboard::LiveDashboard;
use crate::live_source::{Snapshot, Update, Worker};

const NS: u64 = 1_000_000_000;
const GIB: u64 = 1_073_741_824;

#[gpui::test]
fn pressure_accessibility_uses_the_exact_effective_sibling_and_gap(cx: &mut TestAppContext) {
    // The pressure trace collapses siblings; its current metadata must share the exact headline.
    let mut harness = HoverHarness::setup(cx, Route::Memory, Mode::Follow);
    harness.driver.inspect_same_second_memory_sibling();

    assert!(
        harness
            .driver
            .text_is_visible("chart-caption:live-memory-pressure:Critical")
    );
    let (second, reading) = harness.driver.pressure_accessibility_reading();
    assert_eq!(second, Some(1000));
    assert!(reading.ends_with("Critical"));

    harness.driver.hover_second(997);
    let (second, reading) = harness.driver.pressure_accessibility_reading();
    assert_eq!(second, Some(997));
    assert!(reading.ends_with("Unavailable"));
    harness.teardown();
}

#[gpui::test]
fn historical_memory_inspection_updates_every_quantity_and_the_header_mode(
    cx: &mut TestAppContext,
) {
    // Read all quantity rows from the composed Memory page after a production plot hover.
    let mut harness = HoverHarness::setup(cx, Route::Memory, Mode::Follow);
    assert!(
        harness
            .driver
            .text_is_visible("overview-context:memory-free:24.0 GiB")
    );

    harness.driver.hover(Epoch::Historical);

    harness.driver.assert_historical_memory_visible();
    let copied = harness.driver.copy_json();
    assert_eq!(copied["second"], 999);
    assert_eq!(copied["observation"]["publication"], 2);
    assert_eq!(copied["summary"]["memory_active_bytes"], GIB);
    harness.teardown();
}

#[gpui::test]
fn null_memory_quantities_remain_unavailable_at_an_existing_historical_observation(
    cx: &mut TestAppContext,
) {
    // Keep the historical identity while removing one value that the accepted point possesses.
    let mut harness = HoverHarness::setup(cx, Route::Memory, Mode::Follow);
    harness.driver.remove_historical_free_memory();

    harness.driver.hover(Epoch::Historical);

    assert!(
        harness
            .driver
            .text_is_visible("overview-context:memory-free:Unavailable")
    );
    assert!(
        harness
            .driver
            .text_is_visible("overview-context:memory-wired:2.0 GiB")
    );
    assert!(
        !harness
            .driver
            .text_is_visible("overview-context:memory-free:24.0 GiB")
    );
    harness.teardown();
}

#[gpui::test]
fn empty_memory_seconds_clear_readings_and_copy_an_explicit_gap(cx: &mut TestAppContext) {
    // A missing second must not inherit the accepted selection, even with a focused Copy button.
    let mut harness = HoverHarness::setup(cx, Route::Memory, Mode::Follow);

    harness.driver.hover_second(997);

    assert!(
        harness
            .driver
            .text_is_visible("overview-context:memory-free:Unavailable")
    );
    assert!(
        harness
            .driver
            .text_is_visible("overview-context:memory-pressure:Unavailable")
    );
    let copied = harness.driver.copy_json();
    assert_eq!(copied["second"], 997);
    assert!(copied["observation"].is_null());
    assert!(copied["summary"].is_null());
    harness.teardown();
}

#[gpui::test]
fn copy_uses_the_hover_identity_without_the_accepted_expanded_row(cx: &mut TestAppContext) {
    // Seed a private accepted row, then activate the production Copy control from the keyboard.
    let mut harness = HoverHarness::setup(cx, Route::Cpu, Mode::Follow);
    harness.driver.expand_accepted_row();

    harness.driver.hover(Epoch::Historical);
    let copied = harness.driver.copy_json();

    assert_eq!(copied["second"], 999);
    assert_eq!(copied["observation"], Epoch::Historical.identity());
    assert_eq!(copied["summary"]["cpu_percent"], 13);
    assert!(copied.get("values").is_none());
    assert!(copied.get("table").is_none());
    assert!(!copied.to_string().contains("accepted-only-row"));
    harness.teardown();
}

#[gpui::test]
fn superseding_a_target_immediately_rejects_the_obsolete_ready_response(cx: &mut TestAppContext) {
    // Deliver A after the pointer has chosen B, before B has dispatched any lookup.
    let mut harness = HoverHarness::setup(cx, Route::Cpu, Mode::Follow);
    harness.driver.hover(Epoch::Historical);
    let obsolete = harness.driver.begin_lookup();

    harness.driver.hover(Epoch::Older);
    harness.driver.deliver(obsolete, Epoch::Historical);

    assert!(harness.driver.text_is_visible("27%"));
    assert!(!harness.driver.text_is_visible("13%"));
    let copied = harness.driver.copy_json();
    assert_eq!(copied["observation"], Epoch::Older.identity());
    assert!(copied["summary"]["response_marker"].is_null());
    harness.teardown();
}

#[gpui::test]
fn pointer_exit_restores_following_readings_and_rejects_a_late_response(cx: &mut TestAppContext) {
    // The old response arrives only after the pointer leaves the entire plot ownership region.
    let mut harness = HoverHarness::setup(cx, Route::Cpu, Mode::Follow);
    harness.driver.hover(Epoch::Historical);
    let obsolete = harness.driver.begin_lookup();

    harness.driver.leave_plot();
    harness.driver.deliver(obsolete, Epoch::Historical);

    assert!(harness.driver.text_is_visible("87%"));
    let copied = harness.driver.copy_json();
    assert_eq!(copied["observation"], Epoch::Accepted.observation());
    assert_eq!(harness.driver.committed_mode(), Mode::Follow);
    harness.teardown();
}

#[gpui::test]
fn leaving_the_window_ends_the_hover_after_the_next_frame(cx: &mut TestAppContext) {
    // Goal: a pointer that leaves the window stops inspecting, though GPUI keeps its last
    // position. Method: hover the plot, leave the window, repaint, and look for the preview.
    let mut harness = HoverHarness::setup(cx, Route::Cpu, Mode::Follow);
    harness.driver.hover(Epoch::Historical);

    harness.driver.leave_window();

    assert_eq!(harness.driver.committed_mode(), Mode::Follow);
    harness.teardown();
}

#[gpui::test]
fn the_source_status_follows_the_newest_reading_while_an_old_second_is_shown(
    cx: &mut TestAppContext,
) {
    // Goal: showing an older second leaves the source status on the newest reading.
    // Method: make only the newest reading current and read the status of the old selection.
    let mut harness = HoverHarness::setup(cx, Route::Cpu, Mode::Follow);

    harness.driver.make_newest_reading_current();

    assert_eq!(harness.driver.status(), "Live");
    harness.teardown();
}

#[gpui::test]
fn the_source_status_stays_live_while_a_hover_holds_the_view(cx: &mut TestAppContext) {
    // Goal: a hover that holds the live view does not report a stalled collector.
    // Method: judge the status ten seconds later, first without a hover and then during one.
    let mut harness = HoverHarness::setup(cx, Route::Cpu, Mode::Follow);
    harness.driver.make_newest_reading_current();
    let later = std::time::SystemTime::now() + std::time::Duration::from_secs(10);
    assert_eq!(harness.driver.status_at(later), "Waiting");

    harness.driver.hover(Epoch::Historical);

    assert_eq!(harness.driver.status_at(later), "Live");
    harness.teardown();
}

#[gpui::test]
fn pointer_exit_restores_the_paused_selection_after_a_late_failure(cx: &mut TestAppContext) {
    // A failed hover cannot unpause or overwrite the committed paused observation.
    let mut harness = HoverHarness::setup(cx, Route::Cpu, Mode::Paused);
    harness.driver.hover(Epoch::Historical);
    let obsolete = harness.driver.begin_lookup();

    harness.driver.leave_plot();
    harness.driver.fail(obsolete, "obsolete hover failure");

    assert!(harness.driver.text_is_visible("87%"));
    let copied = harness.driver.copy_json();
    assert_eq!(copied["observation"], Epoch::Accepted.observation());
    assert_eq!(harness.driver.committed_mode(), Mode::Paused);
    harness.teardown();
}

#[gpui::test]
fn a_response_for_the_wrong_exact_observation_keeps_resident_values_and_explains_the_failure(
    cx: &mut TestAppContext,
) {
    // Keep A's generation but return another publication to isolate identity acceptance.
    let mut harness = HoverHarness::setup(cx, Route::Cpu, Mode::Follow);
    harness.driver.hover(Epoch::Historical);
    let lookup = harness.driver.begin_lookup();

    harness.driver.deliver_wrong_observation(lookup);

    assert!(harness.driver.text_is_visible("13%"));
    let copied = harness.driver.copy_json();
    assert_eq!(copied["observation"], Epoch::Historical.identity());
    assert!(copied["summary"]["response_marker"].is_null());
    harness.teardown();
}

#[gpui::test]
fn repeated_moves_within_one_timestamp_preserve_the_first_pending_lookup(cx: &mut TestAppContext) {
    // The first response remains acceptable after many real moves and scheduled ticks in one bin.
    let mut harness = HoverHarness::setup(cx, Route::Cpu, Mode::Follow);
    harness.driver.hover(Epoch::Historical);
    let first = harness.driver.begin_lookup();

    harness.driver.move_within_historical_second();
    harness.driver.tick_after(Duration::from_millis(250));
    harness.driver.deliver(first, Epoch::Historical);

    let copied = harness.driver.copy_json();
    assert_eq!(copied["summary"]["response_marker"], "historical-response");
    assert_eq!(copied["observation"], Epoch::Historical.observation());
    harness.teardown();
}

#[gpui::test]
fn superseded_lookups_wait_for_the_next_quarter_second_before_new_details_can_appear(
    cx: &mut TestAppContext,
) {
    // An early response is deliverable only if production actually submitted a premature request.
    let mut harness = HoverHarness::setup(cx, Route::Cpu, Mode::Follow);
    harness.driver.hover(Epoch::Historical);
    harness.driver.begin_lookup();
    harness.driver.hover(Epoch::Older);

    harness.driver.tick_after(Duration::from_millis(249));
    harness.driver.deliver_if_requested(Epoch::Older);

    let before = harness.driver.copy_json();
    assert!(before["summary"]["response_marker"].is_null());
    harness.driver.tick_after(Duration::from_millis(1));
    harness.driver.deliver_if_requested(Epoch::Older);
    let after = harness.driver.copy_json();
    assert_eq!(after["summary"]["response_marker"], "older-response");
    assert_eq!(after["observation"], Epoch::Older.observation());
    harness.teardown();
}

struct HoverHarness {
    driver: HoverDriver,
}

impl HoverHarness {
    fn setup(cx: &mut TestAppContext, route: Route, mode: Mode) -> Self {
        let worker = Worker::start(std::env::temp_dir().join(format!(
            "flightdeck-page-hover-contract-{}",
            std::process::id()
        )))
        .unwrap();
        let window = cx.update(|cx| {
            cx.open_window(WindowOptions::default(), |_, cx| {
                crate::native_workflows::create_dashboard(worker, route.name().into(), None, cx)
            })
            .unwrap()
        });
        let view = window.root(cx).unwrap();
        let context = VisualTestContext::from_window(window.into(), cx);
        let mut harness = Self {
            driver: HoverDriver {
                view,
                context,
                route,
                now: Instant::now(),
            },
        };
        harness.driver.seed_accepted(mode);
        harness
    }

    fn teardown(self) {
        drop(self);
    }
}

impl Drop for HoverHarness {
    fn drop(&mut self) {
        self.driver.context.update(|window, cx| {
            self.driver
                .view
                .update(cx, |dashboard, _| dashboard.worker.stop());
            window.remove_window();
        });
        self.driver.context.run_until_parked();
    }
}

struct HoverDriver {
    view: Entity<LiveDashboard>,
    context: VisualTestContext,
    route: Route,
    now: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Follow,
    Paused,
}

#[derive(Clone, Copy)]
enum Route {
    Memory,
    Cpu,
}

impl Route {
    fn name(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Cpu => "cpu",
        }
    }

    fn plot(self) -> &'static str {
        match self {
            Self::Memory => "live-memory-plot",
            Self::Cpu => "live-cpu-plot",
        }
    }
}

#[derive(Clone, Copy)]
struct Lookup {
    generation: u64,
}

impl HoverDriver {
    fn inspect_same_second_memory_sibling(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = Epoch::Accepted.snapshot(dashboard.generation, false);
            snapshot.selection["trends"][1]["wall_ns"] = json!(1000 * NS);
            snapshot.pages.clear();
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            let target =
                crate::live_hover::Target::exact(1000, &Epoch::Historical.identity()).unwrap();
            dashboard.set_reading_target(
                "keyboard-pressure".into(),
                target,
                crate::live_hover::Origin::Keyboard,
                cx,
            );
        });
        self.context.run_until_parked();
    }

    fn pressure_accessibility_reading(&self) -> (Option<u64>, String) {
        self.view.read_with(&self.context, |dashboard, _| {
            let panel = dashboard
                .panels
                .iter()
                .find(|panel| panel.id == "live-memory-pressure")
                .unwrap();
            dashboard.chart_accessibility_reading(panel)
        })
    }

    fn seed_accepted(&mut self, mode: Mode) {
        self.view.update(&mut self.context, |dashboard, cx| {
            if mode == Mode::Paused {
                dashboard.request.second = Some(Epoch::Accepted.second());
                dashboard.request.identity = Some(Epoch::Accepted.identity());
            }
            let mut snapshot = Epoch::Accepted.snapshot(dashboard.generation, false);
            if matches!(self.route, Route::Memory) {
                snapshot.pages.clear();
            }
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(gpui::px(1536.0), gpui::px(2600.0)));
        self.context.run_until_parked();
        self.now = Instant::now();
    }

    fn remove_historical_free_memory(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = Epoch::Accepted.snapshot(dashboard.generation, false);
            snapshot.selection["trends"][1]["memory_free_bytes"] = Value::Null;
            snapshot.pages.clear();
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn expand_accepted_row(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.expanded = Some(("processes".into(), 0));
            dashboard.rebuild();
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn hover(&mut self, epoch: Epoch) {
        self.hover_second(epoch.second());
    }

    fn hover_second(&mut self, second: u64) {
        let offset = u16::try_from(second - 101).unwrap();
        let bounds = self.context.debug_bounds(self.route.plot()).unwrap();
        let plot = crate::charts::plot_bounds(bounds);
        self.hover_position(gpui::point(
            plot.left() + plot.size.width * ((f32::from(offset) + 0.5) / 900.0),
            plot.center().y,
        ));
    }

    fn move_within_historical_second(&mut self) {
        let bounds = self.context.debug_bounds(self.route.plot()).unwrap();
        let plot = crate::charts::plot_bounds(bounds);
        for index in 0..32_u16 {
            let offset = 898.05 + f32::from(index) / 32.0 * 0.9;
            self.hover_position(gpui::point(
                plot.left() + plot.size.width * (offset / 900.0),
                plot.center().y,
            ));
        }
    }

    fn hover_position(&mut self, position: gpui::Point<gpui::Pixels>) {
        self.context
            .simulate_mouse_move(position, None, Modifiers::none());
    }

    fn leave_plot(&mut self) {
        let bounds = self.context.debug_bounds(self.route.plot()).unwrap();
        self.hover_position(gpui::point(gpui::px(20.0), bounds.center().y));
    }

    fn leave_window(&mut self) {
        let bounds = self.context.debug_bounds(self.route.plot()).unwrap();
        self.context.simulate_event(gpui::MouseExitEvent {
            position: bounds.center(),
            pressed_button: None,
            modifiers: Modifiers::none(),
        });
        self.context.update(|window, _| window.refresh());
        self.context.run_until_parked();
    }

    fn make_newest_reading_current(&mut self) {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.view.update(&mut self.context, |dashboard, _| {
            let snapshot = dashboard.snapshot.as_mut().unwrap();
            snapshot.catalog["last_second"] = json!(now);
        });
    }

    fn status(&self) -> String {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.status())
    }

    fn status_at(&self, now: std::time::SystemTime) -> String {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.status_at(now))
    }

    /// The prototype has no copy control, so the copy is the model's own: the text the control
    /// copied while it existed.
    fn copy_json(&mut self) -> Value {
        let text = self
            .view
            .read_with(&self.context, |dashboard, _| {
                crate::native_workflows::selected_copy(
                    dashboard.reading_snapshot(),
                    dashboard.reading_request(),
                    None,
                )
            })
            .unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn begin_lookup(&mut self) -> Lookup {
        self.tick_after(Duration::ZERO);
        let generation = self.view.read_with(&self.context, |dashboard, _| {
            dashboard.preview.as_ref().unwrap().generation.unwrap()
        });
        Lookup { generation }
    }

    fn tick_after(&mut self, elapsed: Duration) {
        self.now += elapsed;
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick_at(self.now, cx);
        });
        self.context.run_until_parked();
    }

    fn deliver(&mut self, lookup: Lookup, epoch: Epoch) {
        self.deliver_update(Update::Ready(epoch.snapshot(lookup.generation, true)));
    }

    fn deliver_wrong_observation(&mut self, lookup: Lookup) {
        let mut snapshot = Epoch::Historical.snapshot(lookup.generation, true);
        snapshot.selection["selected"] = Epoch::Older.observation();
        self.deliver_update(Update::Ready(snapshot));
    }

    fn fail(&mut self, lookup: Lookup, message: &str) {
        self.deliver_update(Update::Failed {
            generation: lookup.generation,
            message: message.into(),
        });
    }

    fn deliver_if_requested(&mut self, epoch: Epoch) {
        let pending = self.view.read_with(&self.context, |dashboard, _| {
            dashboard
                .preview
                .as_ref()
                .and_then(|preview| preview.generation)
        });
        if let Some(generation) = pending {
            self.deliver(Lookup { generation }, epoch);
        }
    }

    fn deliver_update(&mut self, update: Update) {
        self.view.update(&mut self.context, |dashboard, cx| {
            if dashboard.preview_receive(update) {
                cx.notify();
            }
        });
        self.context.run_until_parked();
    }

    fn text_is_visible(&mut self, text: &'static str) -> bool {
        self.context.debug_bounds(text).is_some()
    }

    fn committed_mode(&self) -> Mode {
        self.view.read_with(&self.context, |dashboard, _| {
            if dashboard.request.second.is_some() {
                Mode::Paused
            } else {
                Mode::Follow
            }
        })
    }

    fn assert_historical_memory_visible(&mut self) {
        assert!(self.text_is_visible("overview-context:memory-app:Unavailable"));
        assert!(self.text_is_visible("overview-context:memory-wired:2.0 GiB"));
        assert!(self.text_is_visible("overview-context:memory-compressed:3.0 GiB"));
        assert!(self.text_is_visible("overview-context:memory-cached:17.0 GiB"));
        assert!(self.text_is_visible("overview-context:memory-free:4.0 GiB"));
        assert!(self.text_is_visible("overview-context:memory-pressure:Critical"));
    }
}

#[derive(Clone, Copy)]
enum Epoch {
    Older,
    Historical,
    Accepted,
}

impl Epoch {
    fn second(self) -> u64 {
        match self {
            Self::Older => 998,
            Self::Historical => 999,
            Self::Accepted => 1000,
        }
    }

    fn publication(self) -> u64 {
        match self {
            Self::Older => 1,
            Self::Historical => 2,
            Self::Accepted => 3,
        }
    }

    fn identity(self) -> Value {
        json!({"run":"00000000000000010000000000000002",
            "boot":"00000000000000030000000000000004", "publication":self.publication()})
    }

    fn observation(self) -> Value {
        let mut observation = self.identity();
        observation["wall_ns"] = json!(self.second() * NS);
        observation["run_tick"] = json!(self.publication());
        observation
    }

    fn point(self) -> Value {
        let (cpu, first) = match self {
            Self::Older => (27, 41_u64),
            Self::Historical => (13, 1),
            Self::Accepted => (87, 21),
        };
        let mut point = self.observation();
        point["segment"] = json!(1);
        point["cpu_percent"] = json!(cpu);
        point["memory_total_bytes"] = json!(match self {
            Self::Historical => 16 * GIB,
            Self::Older => 32 * GIB,
            Self::Accepted => 64 * GIB,
        });
        for (offset, field) in [
            "memory_active_bytes",
            "memory_wired_bytes",
            "memory_compressed_bytes",
            "memory_free_bytes",
            "memory_inactive_bytes",
            "memory_speculative_bytes",
            "memory_anonymous_bytes",
            "memory_file_backed_bytes",
            "memory_purgeable_bytes",
            "swap_used_bytes",
            "swap_total_bytes",
        ]
        .iter()
        .enumerate()
        {
            let offset = u64::try_from(offset).unwrap();
            point[*field] = json!((first + offset) * GIB);
        }
        point["pressure"] = json!(match self {
            Self::Historical => 2,
            _ => 0,
        });
        point
    }

    fn snapshot(self, generation: u64, response: bool) -> Box<Snapshot> {
        let mut summary = self.point();
        if response {
            summary["response_marker"] = json!(match self {
                Self::Older => "older-response",
                Self::Historical => "historical-response",
                Self::Accepted => "accepted-response",
            });
        }
        let points = [Self::Older, Self::Historical, Self::Accepted]
            .into_iter()
            .filter(|epoch| epoch.second() <= self.second())
            .map(Self::point)
            .collect::<Vec<_>>();
        Box::new(Snapshot {
            generation,
            catalog: json!({"path":"/fixture/whole-page-hover",
                "first_second":998,"last_second":1000}),
            selection: json!({"second":self.second(),"selected":self.observation(),
                "summary":summary,"context_seconds":900,
                "trends":points}),
            pages: BTreeMap::from([(
                "processes".into(),
                json!({
                "observation":self.observation(),"columns":["name"],
                "rows":[["accepted-only-row"]],"total":1,"offset":0,"limit":512}),
            )]),
            apps: None,
            detail: None,
            history: None,
            cpu_detail: None,
            cpu_history: None,
            resident_history: None,
            app_attribution_history: None,
            overview_app_histories: None,
            histories_pending: false,
            process_history: None,
            rankings: None,
            read_ms: 0.0,
        })
    }
}
