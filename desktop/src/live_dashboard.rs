use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::charts::{GRID, MUTED, TEXT};
use crate::data::Panel;
use crate::live_source::{Request, Snapshot, Update, Worker};
use gpui::{
    Context, ListAlignment, ListOffset, ListState, Render, Window, div, list, prelude::*, px, rgb,
};

pub const ROUTES: [(&str, &str); 12] = [
    ("overview", "Overview"),
    ("cpu", "CPU"),
    ("apps", "Apps"),
    ("memory", "Memory"),
    ("gpu", "GPU"),
    ("storage", "Storage"),
    ("energy", "Energy"),
    ("network", "Network"),
    ("battery", "Battery"),
    ("computer", "Computer"),
    ("self", "Self-monitoring"),
    ("raw", "Recorded readings"),
];

pub struct LiveDashboard {
    pub worker: Worker,
    pub request: Request,
    pub snapshot: Option<Box<Snapshot>>,
    pending_snapshot: Option<Box<Snapshot>>,
    pub panels: Vec<Rc<Panel>>,
    pub generation: u64,
    pub error: Option<String>,
    pub rows: Vec<Row>,
    pub expanded: Option<(String, usize)>,
    pub executables: Vec<crate::live_executables::Executable>,
    pub inventory: Vec<(String, String)>,
    pub hidden: Vec<String>,
    pub hardware_state: Rc<crate::hardware_motion::HardwareState>,
    pub started: Instant,
    pub seconds: Option<u32>,
    pub updates: u32,
    pub renders: u64,
    pub animate: bool,
    pub reveal_hardware: bool,
    pub chart_hover: Option<ChartHover>,
    pub(crate) chart_inspector: Option<crate::live_chart_view::inspector::ChartInspector>,
    pub preview: Option<crate::live_hover::Preview>,
    pub(crate) preview_ready_at: Instant,
    pub(crate) preview_submissions: u64,
    pub(crate) row_heights:
        Rc<std::cell::RefCell<std::collections::BTreeMap<String, gpui::Pixels>>>,
    pub(crate) header_height: Rc<std::cell::Cell<gpui::Pixels>>,
    pub(crate) viewport_size: gpui::Size<gpui::Pixels>,
    pub app_picker_open: bool,
    pub(crate) app_table_scroll: gpui::ScrollHandle,
    pub cpu_inspection: Option<crate::live_cpu_history::CPUInspection>,
    pub cpu_history_focus: Option<gpui::FocusHandle>,
    pub native: crate::native_workflows::NativeControls,
    pub(crate) inspection_focus: Option<gpui::FocusHandle>,
    pub(crate) list: ListState,
    last_status: String,
    rendered_source: Option<crate::live_source_mode::SourceMode>,
}

pub struct ChartHover {
    pub second: u64,
}

pub enum Row {
    Summary,
    ResourceSummary,
    StorageDevice(usize, String),
    Diagnostics,
    OverviewStatus,
    Rankings(Vec<usize>),
    OverviewAppHistories(Vec<usize>),
    MemoryAttribution(Option<Rc<Panel>>),
    AppAttribution(
        crate::live_app_attribution_history::Metric,
        Option<Rc<Panel>>,
    ),
    AppSelector,
    AppSummary,
    AppColumns(&'static str),
    CPUTop(Option<Rc<Panel>>),
    CPUActivity,
    CPUHistory,
    Charts(Vec<Rc<Panel>>),
    Heading(String),
    Hardware,
    TableHeading(String),
    Table(String, usize),
    App(usize),
    Process(usize),
    Executable(usize),
    Inventory,
    Spacer(gpui::Pixels),
}

impl LiveDashboard {
    pub fn new(worker: Worker, route: String, seconds: Option<u32>) -> Self {
        let mut dashboard = Self {
            worker,
            request: Request {
                route,
                second: None,
                identity: None,
                table: "ticks".into(),
                offset: 0,
                app: None,
                apps_offset: 0,
                members_offset: 0,
                cpu_offset: 0,
                load_resident_history: true,
                load_app_attribution_history: true,
                load_process_history: true,
                executable: None,
            },
            snapshot: None,
            pending_snapshot: None,
            panels: Vec::with_capacity(8),
            generation: 0,
            error: None,
            rows: Vec::with_capacity(4096),
            expanded: None,
            executables: Vec::with_capacity(512),
            inventory: Vec::with_capacity(16),
            hidden: Vec::with_capacity(64),
            hardware_state: Rc::new(crate::hardware_motion::HardwareState::new()),
            started: Instant::now(),
            seconds,
            updates: 0,
            renders: 0,
            animate: false,
            reveal_hardware: false,
            chart_hover: None,
            chart_inspector: None,
            preview: None,
            preview_ready_at: Instant::now(),
            preview_submissions: 0,
            row_heights: Rc::new(std::cell::RefCell::new(std::collections::BTreeMap::new())),
            header_height: Rc::new(std::cell::Cell::new(px(0.0))),
            viewport_size: gpui::size(px(0.0), px(0.0)),
            app_picker_open: false,
            app_table_scroll: gpui::ScrollHandle::new(),
            cpu_inspection: None,
            cpu_history_focus: None,
            native: crate::native_workflows::NativeControls::default(),
            inspection_focus: None,
            list: ListState::new(0, ListAlignment::Top, px(100.0)),
            last_status: String::new(),
            rendered_source: None,
        };
        if dashboard.request.route == "computer" {
            dashboard.inventory = crate::live_inventory::rows();
        }
        dashboard.rebuild();
        dashboard.submit();
        dashboard
    }

    pub fn tick(&mut self, cx: &mut Context<Self>) {
        self.native_tick(cx);
        match self.worker.take() {
            Ok(Some(update)) => {
                let changed = if self.preview.is_some() {
                    self.preview_receive(update)
                } else {
                    self.receive(update)
                };
                if changed {
                    cx.notify();
                }
            }
            Ok(None) => {}
            Err(error) => {
                if let Some(preview) = &mut self.preview {
                    preview.failure = Some(error);
                    preview.complete = true;
                } else {
                    self.clear_presentation();
                    self.error = Some(error);
                }
                cx.notify();
            }
        }
        self.preview_tick(cx);
        let status = self.status();
        if self.last_status != status {
            self.last_status = status;
            cx.notify();
        }
        if let Some(seconds) = self.seconds {
            if self.started.elapsed() >= Duration::from_secs(u64::from(seconds)) {
                self.seconds = None;
                println!(
                    "{}",
                    serde_json::json!({"mode":"live", "route":self.request.route,
                    "elapsed_seconds":self.started.elapsed().as_secs_f64(),
                    "updates":self.updates,"renders":self.renders,
                    "read_ms":self.snapshot.as_ref().map(|snapshot| snapshot.read_ms),
                    "tail_cache_bytes":self.snapshot.as_ref()
                        .map(|snapshot| &snapshot.catalog["tail_cache_bytes"]),
                    "tail_ticks":self.snapshot.as_ref().map(|s| &s.catalog["tail_ticks"]),
                    "scalar_points":self.snapshot.as_ref()
                        .and_then(|s| s.selection["trends"].as_array()).map(Vec::len),
                    "cpu_history":self.snapshot.as_ref()
                        .and_then(|snapshot| snapshot.cpu_history.as_ref())
                        .map(|history| serde_json::json!({"rows":history.cpus().len(),
                            "total":history.total(),"observations":history.observation_count()})),
                    "selected":self.snapshot.as_ref().map(|s| &s.selection["selected"]),
                    "error":self.error})
                );
                crate::live_launch::request_quit(cx);
            }
        }
    }

    fn receive(&mut self, update: Update) -> bool {
        match update {
            Update::Ready(snapshot) => {
                if snapshot.generation != self.generation {
                    return false;
                }

                self.accept(snapshot);
            }
            Update::Failed {
                generation,
                message,
            } => {
                if generation != self.generation {
                    return false;
                }

                self.clear_presentation();
                self.error = Some(message);
            }
        }

        true
    }

    pub(crate) fn accept(&mut self, snapshot: Box<Snapshot>) {
        match crate::live_acceptance::Presentation::prepare(&self.request, snapshot) {
            Ok(presentation) => self.accept_presentation(presentation),
            Err(error) => {
                self.clear_presentation();
                self.error = Some(error);
            }
        }
    }

    pub(crate) fn accept_presentation(
        &mut self,
        presentation: Box<crate::live_acceptance::Presentation>,
    ) {
        if presentation.selection_changed(self.context_snapshot()) {
            self.expanded = None;
            self.native.alternative_page = 0;
            self.cpu_inspection = None;
        }

        self.request = presentation.request;
        self.panels = presentation.panels;
        self.refresh_chart_inspector();
        self.executables = presentation.executables;
        self.snapshot = Some(presentation.snapshot);
        self.pending_snapshot = None;
        self.error = None;
        self.updates += 1;

        self.rebuild();
        if self.reveal_hardware {
            if let Some(index) = self
                .rows
                .iter()
                .position(|row| matches!(row, Row::Hardware))
            {
                self.list.scroll_to(ListOffset {
                    item_ix: index,
                    offset_in_item: px(0.0),
                });
            }
            self.reveal_hardware = false;
        }
    }

    pub fn submit(&mut self) {
        self.native.alternative_page = 0;
        self.clear_presentation();
        self.error = None;
        self.generation = 0;
        match self.worker.request(self.request.clone()) {
            Ok(generation) => self.generation = generation,
            Err(error) => self.error = Some(error),
        }
    }

    pub(crate) fn context_snapshot(&self) -> Option<&Snapshot> {
        self.snapshot
            .as_deref()
            .or(self.pending_snapshot.as_deref())
    }

    fn clear_presentation(&mut self) {
        if let Some(snapshot) = self.snapshot.take() {
            self.pending_snapshot = Some(snapshot);
        }
        self.panels.clear();
        self.executables.clear();
        self.expanded = None;
        self.chart_hover = None;
        self.chart_inspector = None;
        self.preview = None;
        self.cpu_inspection = None;
        self.rebuild();
    }

    pub fn navigate(&mut self, route: &str, cx: &mut Context<Self>) {
        if !matches!(self.request.route.as_str(), "computer" | "self") {
            self.native.diagnostic_selected = self
                .context_snapshot()
                .map_or(serde_json::Value::Null, |snapshot| {
                    snapshot.selection["selected"].clone()
                });
        }

        self.chart_hover = None;
        self.chart_inspector = None;
        self.preview = None;
        self.request.route = route.into();
        self.request.offset = 0;
        self.request.members_offset = 0;
        self.request.cpu_offset = 0;
        self.snapshot = None;
        self.pending_snapshot = None;
        self.expanded = None;
        self.panels.clear();
        self.rows.clear();
        self.list.reset(0);
        if route == "computer" {
            self.inventory = crate::live_inventory::rows();
        }
        self.rebuild();
        self.submit();
        cx.notify();
    }

    pub fn pause(&mut self, cx: &mut Context<Self>) {
        if self.request.second.is_some() {
            self.request.second = None;
            self.request.identity = None;
        } else {
            if let Some(snapshot) = self.context_snapshot() {
                let second = snapshot.selection["second"].as_u64();
                let identity = snapshot.selection["selected"].clone();
                self.request.second = second;
                self.request.identity = if identity.is_null() {
                    None
                } else {
                    Some(identity)
                };
            }
        }
        self.request.offset = 0;
        self.request.apps_offset = 0;
        self.request.members_offset = 0;
        self.request.cpu_offset = 0;
        self.submit();
        cx.notify();
    }

    pub fn inspect(&mut self, second: u64, cx: &mut Context<Self>) {
        self.request.second = Some(second);
        self.request.identity = self.context_snapshot().and_then(|snapshot| {
            crate::live_projection::inspection_identity(&snapshot.selection, second)
        });
        self.request.offset = 0;
        self.request.apps_offset = 0;
        self.request.members_offset = 0;
        self.request.cpu_offset = 0;
        self.submit();
        cx.notify();
    }

    pub fn status(&self) -> String {
        let Some(snapshot) = &self.snapshot else {
            if self.error.is_some() {
                return "Reading unavailable".into();
            }

            if self.pending_snapshot.is_some() {
                return "Reading selection".into();
            }

            return "No live readings".into();
        };
        if self.native.source == crate::live_source_mode::SourceMode::Recorded {
            return "Recorded".into();
        }

        if self.request.route == "self" {
            return "Reader connected".into();
        }

        if snapshot.catalog["collector_completed"].as_bool() == Some(true) {
            return "Collector stopped".into();
        }
        if self.request.second.is_some() {
            return "Paused".into();
        }
        let Some(wall) = snapshot.selection["selected"]["wall_ns"].as_u64() else {
            return "No live readings".into();
        };
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(now) => {
                let observed = Duration::from_nanos(wall);
                if observed > now + Duration::from_secs(3) {
                    return "Clock changed".into();
                }
                if now > observed + Duration::from_secs(3) {
                    return "Waiting".into();
                }
                "Live".into()
            }
            Err(error) => format!("Clock unavailable: {error}"),
        }
    }

    pub fn rebuild(&mut self) {
        let layout = self
            .preview
            .as_ref()
            .and_then(|preview| preview.layout.clone());
        self.rebuild_anchored(layout);
    }

    fn inspection_body(&mut self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let focus = self
            .inspection_focus
            .get_or_insert_with(|| cx.focus_handle())
            .clone();
        let body = list(
            self.list.clone(),
            cx.processor(|this, index: usize, _, cx| this.row(index, cx)),
        )
        .flex_1()
        .min_h_0();

        div()
            .id("flightdeck-inspection")
            .role(gpui::Role::Group)
            .aria_label("History inspection")
            .aria_description("Left and right inspect adjacent seconds while readings load")
            .track_focus(&focus)
            .tab_stop(false)
            .focus(|style| style.bg(rgb(0x18_20_2b)))
            .on_key_down(cx.listener(Self::inspection_keyboard))
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(body)
    }

    fn render_brandbar(&self) -> impl IntoElement {
        let status = self.status();

        div()
            .h(px(48.0))
            .px(px(24.0))
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(rgb(GRID))
            .child("◉  FLIGHTDECK")
            .child(
                div()
                    .debug_selector(|| format!("source-status:{status}"))
                    .text_color(rgb(MUTED))
                    .child(status),
            )
    }
}

#[cfg(test)]
#[path = "live_input_tests.rs"]
mod input_tests;

impl Render for LiveDashboard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.rendered_source != Some(self.native.source) {
            window.set_window_title(self.native.source.title());
            self.rendered_source = Some(self.native.source);
        }

        self.reading_viewport(window.viewport_size());
        self.install_cpu_focus(window, cx);
        self.hardware_state.begin_frame();
        self.renders += 1;
        if self.renders == 1 {
            window.on_next_frame(|window, _| {
                eprintln!(
                    "live first-frame viewport={:?} scale={}",
                    window.viewport_size(),
                    window.scale_factor()
                );
            });
        }

        let body = self.inspection_body(cx);

        div()
            .id("flightdeck-live")
            .role(gpui::Role::Application)
            .accessibility_id("flightdeck.live")
            .aria_label("Flightdeck system monitor")
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x11_12_17))
            .text_color(rgb(TEXT))
            .font_family(".AppleSystemUIFont")
            .text_size(px(13.0))
            .on_key_down(crate::dashboard::keyboard_navigation)
            .child(self.render_brandbar())
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.navigation(cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(self.header(cx))
                            .child(self.native_notice(cx))
                            .children(self.error.as_ref().map(|error| {
                                div()
                                    .px(px(24.0))
                                    .py(px(8.0))
                                    .text_color(rgb(crate::charts::ORANGE))
                                    .child(error.clone())
                            }))
                            .child(body),
                    ),
            )
            .children(self.chart_inspector_overlay(cx))
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn dashboard() -> LiveDashboard {
        let root = std::env::temp_dir().join(format!(
            "flightdeck-missing-selection-{}",
            std::process::id()
        ));
        let mut dashboard = LiveDashboard::new(Worker::start(root).unwrap(), "raw".into(), None);
        dashboard.snapshot = Some(Box::new(Snapshot {
            generation: dashboard.generation,
            catalog: json!({"first_second":1000,"last_second":1002}),
            selection: json!({"second":1000,"selected":{"publication":1},
                "summary":{"memory_total_bytes":4096},"trends":[]}),
            pages: BTreeMap::from([(
                "ticks".into(),
                json!({"offset":0,"total":1,"rows":[["1"]],
                    "columns":["publication"]}),
            )]),
            apps: None,
            detail: None,
            history: None,
            cpu_detail: None,
            cpu_history: None,
            resident_history: None,
            app_attribution_history: None,
            overview_app_histories: None,
            process_history: None,
            rankings: None,
            read_ms: 0.0,
        }));
        dashboard.expanded = Some(("ticks".into(), 0));
        dashboard.rebuild();
        dashboard
    }

    #[test]
    fn changing_the_second_hides_the_previous_reading_before_the_worker_returns() {
        // Goal: selecting B cannot present or copy A while the new read is pending.
        // Method: submit a missing second through a dashboard retaining an expanded A row.
        let mut dashboard = dashboard();
        dashboard.request.second = Some(1001);

        dashboard.submit();

        assert!(dashboard.snapshot.is_none());
        assert!(dashboard.expanded.is_none());
        assert!(dashboard.rows.is_empty());
        assert!(
            crate::native_workflows::selected_copy(
                dashboard.snapshot.as_deref(),
                &dashboard.request,
                dashboard.expanded.as_ref()
            )
            .is_err()
        );
    }

    #[test]
    fn a_failed_selection_keeps_the_previous_reading_hidden() {
        // Goal: a failed B read cannot restore A's presented data.
        // Method: deliver a current-generation worker failure after selecting B.
        let mut dashboard = dashboard();
        dashboard.request.second = Some(1001);
        dashboard.submit();
        let failed = dashboard.generation;

        assert!(dashboard.receive(Update::Failed {
            generation: failed,
            message: "Recording read failed".into(),
        }));

        assert!(dashboard.snapshot.is_none());
        assert!(dashboard.rows.is_empty());
        assert_eq!(dashboard.error.as_deref(), Some("Recording read failed"));
        assert_eq!(dashboard.status(), "Reading unavailable");
    }

    #[test]
    fn superseded_results_cannot_repopulate_a_pending_selection() {
        // Goal: a late A completion cannot replace requested B.
        // Method: submit B and deliver the retained A snapshot through result handling.
        let mut dashboard = dashboard();
        let old = dashboard.snapshot.take().unwrap();
        dashboard.request.second = Some(1002);
        dashboard.submit();

        let accepted = dashboard.receive(Update::Ready(old));

        assert!(!accepted);
        assert!(dashboard.snapshot.is_none());
    }

    #[test]
    fn a_rejected_request_cannot_accept_a_previous_worker_result() {
        // Goal: local validation failure cannot reopen an old generation's presentation.
        // Method: reject an incomplete identity, then deliver the preceding valid result.
        let mut dashboard = dashboard();
        let old = dashboard.snapshot.take().unwrap();
        dashboard.request.identity = Some(json!({"publication":1}));
        dashboard.submit();
        assert!(dashboard.error.is_some());

        let accepted = dashboard.receive(Update::Ready(old));

        assert!(!accepted);
        assert!(dashboard.snapshot.is_none());
    }

    #[test]
    fn a_missing_second_replaces_the_pending_reading_with_explicit_absence() {
        // Goal: a successful gap selection is distinct from pending work and old readings.
        // Method: receive a current-generation empty observation after selecting a gap.
        let mut dashboard = dashboard();
        let mut missing = dashboard.snapshot.take().unwrap();
        dashboard.request.second = Some(1001);
        dashboard.submit();
        missing.generation = dashboard.generation;
        missing.selection = json!({"second":1001,"selected":null,
            "summary":null,"trends":[]});
        missing.pages.clear();

        assert!(dashboard.receive(Update::Ready(missing)));

        let selection = &dashboard.snapshot.as_ref().unwrap().selection;
        assert_eq!(selection["second"], 1001);
        assert!(selection["selected"].is_null());
        assert!(dashboard.pending_snapshot.is_none());
        assert!(dashboard.error.is_none());
    }

    #[test]
    fn queued_inputs_keep_context_without_exposing_its_previous_reading() {
        // Goal: rapid keyboard input can use old bounds without presenting stale data.
        // Method: supersede an accepted snapshot twice before a worker result arrives.
        let mut dashboard = dashboard();
        dashboard.request.second = Some(1001);
        dashboard.submit();
        dashboard.request.second = Some(1002);

        dashboard.submit();

        assert!(dashboard.snapshot.is_none());
        assert_eq!(
            dashboard.context_snapshot().unwrap().selection["second"],
            1000
        );
        assert_eq!(dashboard.request.second, Some(1002));
    }

    #[test]
    fn an_advancing_observation_clears_expanded_row_details() {
        // Goal: a reused row index cannot silently disclose another process after a live update.
        // Method: accept a same-second sibling publication with a different row at index zero.
        let mut next = dashboard().snapshot.take().unwrap();
        let mut dashboard = dashboard();
        next.selection["selected"]["publication"] = json!(2);
        next.pages.get_mut("ticks").unwrap()["rows"] = json!([["2"]]);

        dashboard.accept(next);

        assert!(dashboard.expanded.is_none());
        assert_eq!(
            dashboard.snapshot.as_ref().unwrap().selection["selected"]["publication"],
            2
        );
    }

    #[test]
    fn rereading_the_same_observation_preserves_expanded_details() {
        // Goal: automatic polling of an immutable paused observation keeps its disclosure open.
        // Method: accept another result for the same identity and unchanged table row.
        let next = dashboard().snapshot.take().unwrap();
        let mut dashboard = dashboard();

        dashboard.accept(next);

        assert_eq!(dashboard.expanded, Some(("ticks".into(), 0)));
    }

    #[test]
    fn an_advancing_observation_resets_the_alternatives_page() {
        // Goal: a live update cannot carry a page from a different set of alternatives.
        // Method: accept a sibling publication after opening a later alternatives page.
        let mut next = dashboard().snapshot.take().unwrap();
        let mut dashboard = dashboard();
        dashboard.native.alternative_page = 5;
        next.selection["selected"]["publication"] = json!(2);

        dashboard.accept(next);

        assert_eq!(dashboard.native.alternative_page, 0);
    }

    #[test]
    fn rereading_the_same_observation_preserves_the_alternatives_page() {
        // Goal: polling an immutable observation does not force its selector back to page one.
        // Method: accept the same identity after opening a later alternatives page.
        let next = dashboard().snapshot.take().unwrap();
        let mut dashboard = dashboard();
        dashboard.native.alternative_page = 5;

        dashboard.accept(next);

        assert_eq!(dashboard.native.alternative_page, 5);
    }
}
