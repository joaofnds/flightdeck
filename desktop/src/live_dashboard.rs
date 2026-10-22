use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::charts::TEXT;
use crate::data::Panel;
use crate::live_hover::Origin;
use crate::live_source::{Request, Snapshot, Update, Worker};
use gpui::{
    Context, ListAlignment, ListOffset, ListState, Render, Window, div, list, prelude::*, px, rgb,
};

pub const ROUTES: [(&str, &str); 11] = [
    ("overview", "Overview"),
    ("cpu", "CPU & Processes"),
    ("apps", "Apps"),
    ("memory", "Memory"),
    ("gpu", "GPU"),
    ("storage", "Storage"),
    ("energy", "Energy"),
    ("network", "Network"),
    ("battery", "Battery"),
    ("computer", "Computer"),
    ("self", "Self-monitoring"),
];

/// The sidebar's groups, in the approved prototype's order.
pub const NAVIGATION: [&[&str]; 3] = [
    &["overview", "cpu"],
    &[
        "apps", "gpu", "memory", "energy", "storage", "network", "battery",
    ],
    &["computer", "self"],
];

/// Tells the dashboard whether macOS presents its window, which its next tick acts on.
pub(crate) fn observe_visibility(
    dashboard: &gpui::Entity<LiveDashboard>,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    dashboard.update(cx, |dashboard, cx| {
        // macOS reports only changes, so a window it hides from the start would read its page
        // until it first showed and hid again.
        if !window.is_visible() {
            dashboard.presence = Presence::Opening;
        }

        cx.observe_window_visibility(window, |dashboard, visibility, _, cx| {
            dashboard.presence = dashboard.presence.reported(visibility.is_visible());
            cx.notify();
        })
        .detach();
    });
}

/// A picker on the Apps page whose choices show below it. One shows at a time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Picker {
    App,
    ProcessGroup,
}

/// The open picker after its control is pressed: the picker's choices, or none if they
/// showed already.
pub fn toggle(open: Option<Picker>, pressed: Picker) -> Option<Picker> {
    if open == Some(pressed) {
        None
    } else {
        Some(pressed)
    }
}

pub struct LiveDashboard {
    pub worker: Worker,
    pub request: Request,
    pub snapshot: Option<Box<Snapshot>>,
    pending_snapshot: Option<Box<Snapshot>>,
    /// The last live reading of each page the reader has left, shown again at once when the
    /// page is opened while the worker reads a fresh one. At most one for each route.
    visited: std::collections::BTreeMap<String, Box<crate::live_acceptance::Presentation>>,
    pub panels: Vec<Rc<Panel>>,
    pub generation: u64,
    /// The worker has not yet answered the last request, so the app polls it quickly.
    reply_pending: bool,
    presence: Presence,
    pub error: Option<String>,
    pub rows: Vec<Row>,
    pub expanded: Option<(String, usize)>,
    pub executables: Vec<crate::live_executables::Executable>,
    pub inventory: Vec<(String, String)>,
    pub hidden: Vec<String>,
    pub(crate) sections: crate::live_sections::Sections,
    pub hardware_state: Rc<crate::hardware_motion::HardwareState>,
    pub started: Instant,
    pub seconds: Option<u32>,
    pub updates: u32,
    pub renders: u64,
    pub animate: bool,
    pub reveal_hardware: bool,
    pub chart_hover: Option<ChartHover>,
    /// Where the pointer stands over a plot, in window coordinates, which anchors the chart
    /// tooltip beside it. None while no plot is under the pointer.
    pub(crate) chart_pointer: Option<gpui::Point<gpui::Pixels>>,
    pub(crate) pointer: crate::live_hover::controller::PointerPlace,
    pub(crate) chart_inspector: Option<crate::live_chart_view::inspector::ChartInspector>,
    pub preview: Option<crate::live_hover::Preview>,
    /// When the shown live reading arrived. A preview holds the live view, so while one is
    /// shown the source status is judged at this moment instead of now.
    received_at: SystemTime,
    pub(crate) preview_submissions: u64,
    pub(crate) hover_timings: crate::live_hover::timings::HoverTimings,
    pub(crate) row_heights:
        Rc<std::cell::RefCell<std::collections::BTreeMap<String, gpui::Pixels>>>,
    /// How wide each chart's card was last laid out, by panel id, which sizes its buckets.
    pub(crate) chart_widths:
        Rc<std::cell::RefCell<std::collections::BTreeMap<String, gpui::Pixels>>>,
    /// The key and room of each row the page shows as a placeholder while a request replaces its
    /// readings, empty otherwise.
    pub(crate) placeholders: Vec<(String, gpui::Pixels)>,
    pub(crate) header_height: Rc<std::cell::Cell<gpui::Pixels>>,
    pub(crate) viewport_size: gpui::Size<gpui::Pixels>,
    /// The Apps page's picker whose choices are open, if any.
    pub picker: Option<Picker>,
    pub(crate) app_table_scroll: gpui::ScrollHandle,
    pub cpu_inspection: Option<crate::live_cpu_history::CPUInspection>,
    pub cpu_history_focus: Option<gpui::FocusHandle>,
    pub(crate) root_focus: Option<gpui::FocusHandle>,
    pub native: crate::native_workflows::NativeControls,
    pub(crate) inspection_focus: Option<gpui::FocusHandle>,
    pub(crate) list: ListState,
    pub(crate) wheel_hold: Rc<crate::live_wheel::WheelHold>,
    last_status: String,
    rendered_source: Option<crate::live_source_mode::SourceMode>,
    /// The reports of the collector this app started, as of the last tick that changed them.
    pub(crate) self_reports: Rc<crate::self_reports::SelfReports>,
    /// The second of the ten-second report the Self charts inspect, or none for the newest.
    pub(crate) self_inspected: Option<u64>,
    /// The stretch of time the charts show.
    pub(crate) range: crate::time_range::Range,
    /// The selection a drag across a chart is making.
    pub(crate) drag: Option<Drag>,
}

/// Whether macOS presents the window, and whether its reader has caught up with that, since the
/// reader rests while the window is hidden and reads no page nobody sees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Presence {
    /// Not yet shown, and the reader reads the page until it answers. macOS reports a new window
    /// hidden until it puts it on the screen, so a window still hidden by then is one it hides.
    Opening,
    /// Shown, and the reader reads the page.
    Shown,
    /// Hidden, and the reader still reads the page until the next tick rests it.
    Hiding,
    /// Hidden, and the reader rests.
    Hidden,
    /// Shown again, and the reader rests until the next tick asks it for the page.
    Showing,
}

impl Presence {
    /// The presence once macOS reports whether it presents the window.
    fn reported(self, visible: bool) -> Self {
        if visible {
            match self {
                Presence::Opening | Presence::Hiding => Presence::Shown,
                Presence::Hidden => Presence::Showing,
                Presence::Shown | Presence::Showing => self,
            }
        } else {
            match self {
                Presence::Shown => Presence::Hiding,
                Presence::Showing => Presence::Hidden,
                Presence::Opening | Presence::Hiding | Presence::Hidden => self,
            }
        }
    }
}

/// A drag across one chart: the seconds under the pointer when it went down and where it is now.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Drag {
    pub(crate) chart: String,
    pub(crate) from: u64,
    pub(crate) to: u64,
}

pub struct ChartHover {
    pub second: u64,
}

pub enum Row {
    Summary,
    SelfPart(crate::live_diagnostics::SelfPart),
    OverviewStatus,
    /// A component's temperature chart beside the card of its reading at the selected time.
    Temperature(&'static str, &'static str, Option<Rc<Panel>>),
    /// The Battery page's charging timeline beside its details card.
    BatteryState(Option<Rc<Panel>>),
    OverviewRankings(Vec<usize>),
    OverviewAppHistories(Vec<usize>),
    /// The Overview's utilization panel beside the load, power and heat chart.
    OverviewAnalysis(Option<Rc<Panel>>),
    MemoryAttribution(Option<Rc<Panel>>),
    /// The Memory page's captured quantities, folded under the All memory quantities details.
    MemoryQuantities,
    AppAttribution(
        crate::live_app_attribution_history::Metric,
        Option<Rc<Panel>>,
    ),
    /// The GPU page's chart of GPU time by App beside its ranking, from the app's samples.
    GpuApps(Option<Rc<Panel>>),
    /// The Network page's chart of network traffic by App beside its ranking.
    NetworkApps(Option<Rc<Panel>>),
    AppSelector,
    /// The Apps page's process group control above the process histories.
    ProcessGroupBar,
    /// The Apps page's note on process grouping and coverage, under the group table.
    ProcessGroupingAbout,
    AppColumns(&'static str),
    CPUTop(Option<Rc<Panel>>),
    CPUActivity,
    CPUHistory,
    /// The CPU page's system context: how long this Mac has run since it booted, and the
    /// system's open file entries.
    SystemContext(Option<Rc<Panel>>),
    Charts {
        panels: Vec<Rc<Panel>>,
        columns: usize,
    },
    /// Charts folded under a details panel, which draws one bordered panel around them.
    ContextCharts {
        panels: Vec<Rc<Panel>>,
        columns: usize,
    },
    /// The Energy page's fan speed beside the thermal state, then the area maxima beside
    /// the readings at the selected time.
    EnergyCooling {
        fans: Option<Rc<Panel>>,
        thermal: Option<Rc<Panel>>,
        maxima: Option<Rc<Panel>>,
    },
    Heading(String),
    Disclosure(crate::live_sections::Section),
    Hardware,
    App(usize),
    Executable(usize),
    Inventory,
    Spacer(gpui::Pixels),
    /// The room a row took, under the row's own key, kept while a request replaces the
    /// readings it showed so the page does not move before the reply arrives.
    Placeholder(String, gpui::Pixels),
}

impl LiveDashboard {
    pub fn new(worker: Worker, route: String, seconds: Option<u32>) -> Self {
        let mut dashboard = Self {
            worker,
            request: Self::new_request(route),
            snapshot: None,
            pending_snapshot: None,
            visited: std::collections::BTreeMap::new(),
            panels: Vec::with_capacity(8),
            generation: 0,
            reply_pending: false,
            presence: Presence::Shown,
            error: None,
            rows: Vec::with_capacity(4096),
            expanded: None,
            executables: Vec::with_capacity(512),
            inventory: Vec::with_capacity(16),
            hidden: Vec::with_capacity(64),
            sections: crate::live_sections::Sections::default(),
            hardware_state: Rc::new(crate::hardware_motion::HardwareState::new()),
            started: Instant::now(),
            seconds,
            updates: 0,
            renders: 0,
            animate: false,
            reveal_hardware: false,
            chart_hover: None,
            chart_pointer: None,
            pointer: crate::live_hover::controller::PointerPlace::InsideWindow,
            chart_inspector: None,
            preview: None,
            received_at: SystemTime::now(),
            preview_submissions: 0,
            hover_timings: crate::live_hover::timings::HoverTimings::new(),
            row_heights: Rc::new(std::cell::RefCell::new(std::collections::BTreeMap::new())),
            chart_widths: Rc::new(std::cell::RefCell::new(std::collections::BTreeMap::new())),
            placeholders: Vec::new(),
            header_height: Rc::new(std::cell::Cell::new(px(0.0))),
            viewport_size: gpui::size(px(0.0), px(0.0)),
            picker: None,
            app_table_scroll: gpui::ScrollHandle::new(),
            cpu_inspection: None,
            cpu_history_focus: None,
            root_focus: None,
            native: crate::native_workflows::NativeControls::default(),
            inspection_focus: None,
            list: ListState::new(0, ListAlignment::Top, px(100.0)),
            wheel_hold: Rc::default(),
            last_status: String::new(),
            rendered_source: None,
            self_reports: Rc::default(),
            self_inspected: None,
            range: crate::time_range::Range::default(),
            drag: None,
        };
        if dashboard.request.route == "computer" {
            dashboard.inventory = crate::live_inventory::rows();
        }
        dashboard.rebuild();
        dashboard.submit();
        dashboard
    }

    fn new_request(route: String) -> Request {
        Request {
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
            earlier: crate::live_source::Earlier::Load,
            executable: None,
        }
    }

    pub fn tick(&mut self, cx: &mut Context<Self>) {
        self.native_tick(cx);
        match self.worker.take() {
            Ok(Some(update)) => {
                if self.take_update(update) {
                    cx.notify();
                }
            }
            Ok(None) => {}
            Err(error) => {
                if let Some(preview) = &mut self.preview {
                    preview.failure = Some(error);
                    preview.complete = true;
                    self.hover_timed();
                } else {
                    self.reply_pending = false;
                    self.clear_presentation();
                    self.error = Some(error);
                }
                cx.notify();
            }
        }
        self.tick_presence(cx);
        // A hover's read would wake the resting reader of a hidden window.
        if self.presence != Presence::Hidden {
            self.preview_tick(cx);
        }
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
                    "hovers":self.hover_timings.report(),
                    "error":self.error})
                );
                cx.quit();
            }
        }
    }

    /// Rests the reader of a window macOS hides, or asks for the page of a window it shows again.
    fn tick_presence(&mut self, cx: &mut Context<Self>) {
        if self.presence == Presence::Opening {
            if !self.reply_pending {
                self.presence = Presence::Hiding;
            }
        }
        match self.presence {
            Presence::Hiding => {
                self.end_reading_preview(cx);
                self.tick_hidden();
                self.presence = Presence::Hidden;
            }
            Presence::Showing => {
                self.tick_shown();
                self.presence = Presence::Shown;
            }
            Presence::Opening | Presence::Shown | Presence::Hidden => {}
        }
    }

    /// Rests the reader of a window macOS hides, as a closed window's reader rests. A reader of
    /// a recording opened read-only reads nothing once its page is read, so it is left alone.
    fn tick_hidden(&mut self) {
        match self.worker.hand_over() {
            Ok(Some(resting)) => {
                self.worker = resting;
                self.reply_pending = false;
            }
            Ok(None) => {}
            Err(error) => self.error = Some(error),
        }
    }

    /// Asks the rested reader for the page again, which it reads as newly opened, readings
    /// first.
    fn tick_shown(&mut self) {
        if self.worker.mode() == crate::live_source_mode::SourceMode::Live {
            self.request_reading();
        }
    }

    /// Takes the worker's answer to the page or to a hover, and answers whether the page changed.
    pub(crate) fn take_update(&mut self, update: Update) -> bool {
        if self.preview.is_some() {
            return self.preview_receive(update);
        }

        self.receive(update)
    }

    pub(crate) fn receive(&mut self, update: Update) -> bool {
        match update {
            Update::Ready(snapshot) => {
                if snapshot.generation != self.generation {
                    return false;
                }

                self.reply_pending = false;
                self.accept(snapshot);
            }
            Update::Failed {
                generation,
                message,
            } => {
                if generation != self.generation {
                    return false;
                }

                self.reply_pending = false;
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
        self.executables = presentation.executables;
        self.snapshot = Some(presentation.snapshot);
        self.zoom_panels();
        self.refresh_chart_inspector();
        self.received_at = SystemTime::now();
        self.pending_snapshot = None;
        self.error = None;
        self.updates += 1;
        self.placeholders.clear();

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
        // A page whose readings are shown keeps its rows' room until the reply replaces them.
        let placeholders = if self.snapshot.is_some() {
            self.placeholder_rows()
        } else {
            Vec::new()
        };
        self.clear_readings();
        self.placeholders = placeholders;
        self.rebuild();
        self.submit_request();
    }

    /// Asks for the request in place of whatever the page asked before, so no reply to an
    /// earlier request is taken if this one cannot be sent.
    fn submit_request(&mut self) {
        self.error = None;
        self.generation = 0;
        self.request_reading();
    }

    /// Whether the worker still reads the second the page selected, whose reply the page waits
    /// on with the readings it shows. A read of any other second would cancel it.
    pub(crate) fn selection_reading(&self) -> bool {
        if !self.reply_pending {
            return false;
        }

        self.request.second.is_some()
    }

    /// Whether the page has no reading to show yet and the worker is still reading it, which
    /// takes a few milliseconds and is not a failure.
    pub(crate) fn is_opening(&self) -> bool {
        let opening = self.reply_pending && self.error.is_none();

        opening
            && self.snapshot.is_none()
            && !matches!(self.request.route.as_str(), "self" | "computer")
    }

    /// Whether the worker still owes an answer to the last request, the page's or a hover's,
    /// or the App histories the page's first readings left pending, which it reads at once.
    pub(crate) fn awaiting_reply(&self) -> bool {
        if self.reply_pending {
            return true;
        }

        // A window hidden or shown again rests its reader or asks for its page at the next
        // tick, which the quick poll brings within a frame rather than at the quarter-second
        // tick.
        if matches!(self.presence, Presence::Hiding | Presence::Showing) {
            return true;
        }

        // A hidden window's reader rests, so the App histories or the hover its page waits on
        // come only once the window shows again.
        if self.presence == Presence::Hidden {
            return false;
        }

        if let Some(snapshot) = &self.snapshot {
            if snapshot.histories_pending {
                return true;
            }
        }

        self.preview_awaiting_reply()
    }

    pub(crate) fn context_snapshot(&self) -> Option<&Snapshot> {
        self.snapshot
            .as_deref()
            .or(self.pending_snapshot.as_deref())
    }

    fn clear_presentation(&mut self) {
        self.placeholders.clear();
        self.clear_readings();
        self.rebuild();
    }

    fn clear_readings(&mut self) {
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
    }

    pub fn navigate(&mut self, route: &str, cx: &mut Context<Self>) {
        self.switch_route(route);
        cx.notify();
    }

    /// Keeps the live reading of the page being left, so a page opened again shows it at once.
    fn keep_page(&mut self) {
        if self.request.second.is_some() || self.preview.is_some() {
            return;
        }
        let Some(snapshot) = self.snapshot.take() else {
            return;
        };
        let presentation = Box::new(crate::live_acceptance::Presentation {
            request: self.request.clone(),
            snapshot,
            panels: std::mem::take(&mut self.panels),
            executables: std::mem::take(&mut self.executables),
        });
        self.visited
            .insert(self.request.route.clone(), presentation);
        assert!(self.visited.len() <= ROUTES.len());
    }

    pub(crate) fn switch_route(&mut self, route: &str) {
        if self.request.route != route {
            self.sections.reset();
        }
        self.keep_page();
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
        self.reset_section_rows();
        if route == "computer" {
            self.inventory = crate::live_inventory::rows();
        }
        self.rebuild();
        self.submit();
        if self.request.second.is_none() {
            if let Some(mut kept) = self.visited.remove(route) {
                kept.request = self.request.clone();
                self.accept_presentation(kept);
            }
        }
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
        self.select(cx);
    }

    pub fn inspect(&mut self, second: u64, cx: &mut Context<Self>) {
        let second = self.context_snapshot().map_or(second, |snapshot| {
            crate::live_projection::inspection_second(&snapshot.selection, second)
        });
        self.request.second = Some(second);
        self.request.identity = self.context_snapshot().and_then(|snapshot| {
            crate::live_projection::inspection_identity(&snapshot.selection, second)
        });
        self.request.offset = 0;
        self.request.apps_offset = 0;
        self.request.members_offset = 0;
        self.request.cpu_offset = 0;
        self.select(cx);
    }

    /// Asks for the request's second and keeps the page's readings shown until the reply, which
    /// takes the reader hundreds of milliseconds. Where those readings hold the second, the page
    /// shows it from them at once, as an arrow step does. A page with nothing shown asks as any
    /// other request does.
    pub(crate) fn select(&mut self, cx: &mut Context<Self>) {
        if self.snapshot.is_none() {
            self.submit();
            cx.notify();
            return;
        }

        self.native.alternative_page = 0;
        self.submit_request();
        if let Some(replaced) = self.preview.take() {
            self.hover_ended(&replaced);
        }
        if !self.show_held_selection(cx) {
            self.rebuild();
            cx.notify();
        }
    }

    /// Shows the selected second from the readings the page holds, as an arrow step does, where
    /// they hold it and do not already show it, and answers whether it shows it.
    pub(crate) fn show_held_selection(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(second) = self.request.second else {
            return false;
        };
        let Some(target) = self.held_target(second) else {
            return false;
        };
        if let Some(snapshot) = &self.snapshot {
            if target.matches(&snapshot.selection["selected"]) {
                return false;
            }
        }

        let source = crate::live_chart_view::SELECTION_SOURCE.into();
        self.set_reading_target(source, target, Origin::Keyboard, cx)
    }

    /// The seconds the charts show, which is the whole history unless a range narrows it.
    pub(crate) fn view_window(&self) -> Option<crate::chart_axes::TimeWindow> {
        let anchor = self.range_anchor()?;

        Some(self.range.window(anchor))
    }

    /// What the range is laid against: the selected second, and the last second the charts hold.
    pub(crate) fn range_anchor(&self) -> Option<crate::time_range::Anchor> {
        let selection = &self.snapshot.as_deref()?.selection;

        Some(crate::time_range::Anchor {
            selected: selection["second"].as_u64()?,
            context_last: crate::live_projection::context_last_second(selection)?,
        })
    }

    /// Spreads the view across every chart that is drawn against the clock, and tells each
    /// whether its readings still move on with the newest second.
    fn zoom_panels(&mut self) {
        let Some(view) = self.view_window() else {
            return;
        };
        let sliding = self.request.second.is_none() && self.range.slides();
        for panel in &mut self.panels {
            let Some(window) = panel.window else {
                continue;
            };
            if window != view {
                crate::time_range::zoom(Rc::make_mut(panel), view);
            }
            if panel.sliding != sliding {
                Rc::make_mut(panel).sliding = sliding;
            }
        }
    }

    /// Shows `range`, which a menu choice or a drag picked, without asking for another reading.
    pub(crate) fn set_range(&mut self, range: crate::time_range::Range, cx: &mut Context<Self>) {
        self.range = range;
        self.drag = None;
        self.zoom_panels();
        self.rebuild();
        cx.notify();
    }

    /// Shows a stretch the reader selected, and holds the view at the newest second so the
    /// stretch stays where it was drawn.
    pub(crate) fn select_range(&mut self, range: crate::time_range::Range, cx: &mut Context<Self>) {
        self.hold_second();
        self.set_range(range, cx);
    }

    /// Goes back to the whole history, following the newest second.
    pub(crate) fn reset_range(&mut self, cx: &mut Context<Self>) {
        let held = self.request.second.take().is_some();
        if held {
            self.request.identity = None;
        }
        self.set_range(crate::time_range::Range::History, cx);
        if held {
            self.request_reading();
        }
    }

    /// Asks for the second now shown, so the view stops following without blanking.
    fn hold_second(&mut self) {
        if self.request.second.is_some() {
            return;
        }
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };
        let identity = snapshot.selection["selected"].clone();
        self.request.second = snapshot.selection["second"].as_u64();
        self.request.identity = if identity.is_null() {
            None
        } else {
            Some(identity)
        };
        self.request_reading();
    }

    /// Asks the worker for the request as it stands, leaving the reading shown until the reply.
    pub(crate) fn request_reading(&mut self) {
        if let Some(preview) = &mut self.preview {
            preview.page_reply_kept = false;
        }
        match self.worker.request(self.request.clone()) {
            Ok(generation) => {
                self.generation = generation;
                self.reply_pending = true;
            }
            Err(error) => self.error = Some(error),
        }
    }

    pub fn status(&self) -> String {
        self.status_at(SystemTime::now())
    }

    /// What a history card reads where the snapshot holds no such history: that it is still
    /// being read on a newly opened page, and otherwise `unavailable`.
    pub(crate) fn history_absent(&self, unavailable: &str) -> String {
        if let Some(snapshot) = &self.snapshot {
            if snapshot.histories_pending {
                return "Reading App history".into();
            }
        }
        unavailable.into()
    }

    pub(crate) fn status_at(&self, now: SystemTime) -> String {
        // Computer reads this Mac's inventory each time it opens, whatever the recording.
        if self.request.route == "computer" {
            return "System snapshot".into();
        }
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
        let Some(newest) = snapshot.catalog["last_second"].as_u64() else {
            return "No live readings".into();
        };
        let judged = if self.preview.is_some() {
            self.received_at
        } else {
            now
        };
        match judged.duration_since(UNIX_EPOCH) {
            Ok(now) => {
                let observed = Duration::from_secs(newest);
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

    fn inspection_body(
        &mut self,
        focus: &gpui::FocusHandle,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
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
            .aria_description(
                "Left and right inspect adjacent seconds, Enter selects the one shown and \
                 Escape returns to the selection",
            )
            .track_focus(focus)
            .tab_stop(false)
            .on_key_down(cx.listener(Self::inspection_keyboard))
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(self.wheel_hold(cx))
            .child(body)
    }
}

#[cfg(test)]
#[path = "live_input_tests.rs"]
pub(crate) mod input_tests;

impl LiveDashboard {
    /// The error that stopped the latest refresh, above the page it left in place.
    fn error_notice(&self) -> Option<gpui::Div> {
        self.error.as_ref().map(|error| {
            div()
                .px(px(24.0))
                .py(px(8.0))
                .text_color(rgb(crate::charts::ORANGE))
                .child(error.clone())
        })
    }
}

impl LiveDashboard {
    /// The window's own focus, taken at the first frame so a key pressed before any control
    /// has focus still reaches the window's key handler.
    fn install_root_focus(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::FocusHandle {
        if let Some(focus) = &self.root_focus {
            return focus.clone();
        }
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        self.root_focus = Some(focus.clone());

        focus
    }
}

impl Render for LiveDashboard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.rendered_source != Some(self.native.source) {
            window.set_window_title(self.native.source.title());
            self.rendered_source = Some(self.native.source);
        }

        self.install_section_focus(cx);
        self.reading_viewport(window.viewport_size());
        self.install_cpu_focus(window, cx);
        let inspection_focus = self.install_inspection_focus(window, cx);
        let root_focus = self.install_root_focus(window, cx);
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

        let body = self.inspection_body(&inspection_focus, cx);

        div()
            .id("flightdeck-live")
            .track_focus(&root_focus)
            .tab_stop(false)
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
            .line_height(gpui::relative(1.5))
            .on_key_down(crate::dashboard::keyboard_navigation)
            .on_mouse_move(cx.listener(|this, _: &gpui::MouseMoveEvent, _, _| {
                this.pointer = crate::live_hover::controller::PointerPlace::InsideWindow;
            }))
            .on_mouse_exit(cx.listener(|this, _: &gpui::MouseExitEvent, _, cx| {
                this.pointer_left_window(cx);
            }))
            .child(self.brandbar())
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
                            .children(self.error_notice())
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
        let mut dashboard =
            LiveDashboard::new(Worker::start(root).unwrap(), "overview".into(), None);
        dashboard.snapshot = Some(Box::new(Snapshot {
            generation: dashboard.generation,
            catalog: json!({"first_second":1000,"last_second":1002}),
            selection: json!({"second":1000,"context_last_second":1000,"selected":{"publication":1},
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
            histories_pending: false,
            process_history: None,
            rankings: None,
            app_ranks: None,
            app_parts: None,
            read_ms: 0.0,
        }));
        dashboard.expanded = Some(("ticks".into(), 0));
        dashboard.rebuild();
        dashboard
    }

    #[test]
    fn a_request_awaits_its_reply_until_the_worker_answers() {
        // Goal: the app polls the worker quickly exactly while an answer is owed. Method:
        // submit a request, then deliver the worker's answer to it.
        let mut dashboard = dashboard();

        dashboard.submit();
        assert!(dashboard.awaiting_reply());
        let answered = dashboard.generation;
        dashboard.receive(Update::Failed {
            generation: answered,
            message: "Recording read failed".into(),
        });

        assert!(!dashboard.awaiting_reply());
    }

    #[test]
    fn choosing_an_executable_awaits_the_readings_it_asks_for() {
        // Goal: the app polls the worker quickly for the readings a chosen executable asks
        // for, so its process charts appear as soon as they are read rather than at the next
        // tick a quarter second later. Method: choose an executable on a page whose readings
        // are shown and answered.
        let mut dashboard = dashboard();
        let mut shown = dashboard.snapshot.take().unwrap();
        dashboard.submit();
        shown.generation = dashboard.generation;
        assert!(dashboard.receive(Update::Ready(shown)));
        assert!(!dashboard.awaiting_reply());

        dashboard.select_executable(Some("hex:61".into()));

        assert!(dashboard.awaiting_reply());
    }

    #[test]
    fn a_page_awaits_the_app_histories_its_first_readings_leave_pending() {
        // Goal: the app polls the worker quickly while a newly opened page's App histories are
        // read, which the worker starts at once, so its charts appear as soon as they are read
        // rather than at the next tick a quarter second later. Method: receive the answer to
        // the page's request, its first readings with the histories still pending.
        let mut dashboard = dashboard();
        let mut first = dashboard.snapshot.take().unwrap();
        dashboard.submit();
        first.generation = dashboard.generation;
        first.histories_pending = true;

        assert!(dashboard.receive(Update::Ready(first)));

        assert!(dashboard.awaiting_reply());
    }

    #[test]
    fn a_page_awaits_nothing_once_its_readings_bring_the_app_histories() {
        // Goal: the quick poll stops once the page holds every reading it asked for. Method:
        // receive the answer to the page's request with its histories read.
        let mut dashboard = dashboard();
        let mut complete = dashboard.snapshot.take().unwrap();
        dashboard.submit();
        complete.generation = dashboard.generation;
        complete.histories_pending = false;

        assert!(dashboard.receive(Update::Ready(complete)));

        assert!(!dashboard.awaiting_reply());
    }

    #[test]
    fn a_page_opened_for_the_first_time_draws_nothing_until_its_reading_arrives() {
        // Goal: the first frame of a page the worker is still reading shows no "Unavailable",
        // which would flash for the few milliseconds the read takes. Method: open a page with
        // no kept reading, then fail its read.
        let mut dashboard = dashboard();

        dashboard.switch_route("cpu");
        assert!(dashboard.reading_rows().is_empty());
        let failed = dashboard.generation;
        dashboard.receive(Update::Failed {
            generation: failed,
            message: "Recording read failed".into(),
        });

        assert!(!dashboard.reading_rows().is_empty());
    }

    #[test]
    fn a_page_left_and_opened_again_shows_its_last_reading_at_once() {
        // Goal: switching pages never blanks a page the reader has already seen, since every
        // reading is local and the app must feel instant. Method: leave a page that has a
        // reading for one that has none, then return, with no reply from the worker between.
        let mut dashboard = dashboard();

        dashboard.switch_route("cpu");
        assert!(dashboard.snapshot.is_none());
        dashboard.switch_route("overview");

        let shown = dashboard.snapshot.as_ref().unwrap();
        assert_eq!(shown.selection["second"], 1000);
        assert_eq!(dashboard.request.route, "overview");
    }

    #[test]
    fn a_page_opened_while_paused_does_not_show_a_reading_of_another_second() {
        // Goal: a reading kept from the live view is never shown as the paused second's.
        // Method: leave a live page, pause on another second, then return.
        let mut dashboard = dashboard();
        dashboard.switch_route("cpu");

        dashboard.request.second = Some(1001);
        dashboard.switch_route("overview");

        assert!(dashboard.snapshot.is_none());
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
        missing.selection = json!({"second":1001,"context_last_second":1001,"selected":null,
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
