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
        store_view::set_store_active(window.is_visible() && dashboard.follows_live_mac(), cx);
        // macOS reports only changes, so a window it hides from the start would read its page
        // until it first showed and hid again.
        if !window.is_visible() {
            if dashboard.presence == Presence::Showing {
                dashboard.presence = Presence::Hidden;
            } else {
                dashboard.presence = Presence::Opening;
            }
        }

        cx.observe_window_visibility(window, |dashboard, visibility, _, cx| {
            #[cfg(feature = "automation")]
            if visibility.is_visible() {
                let version = dashboard.resume_store_version();
                dashboard
                    .resume_metrics
                    .begin(&crate::live_resume_metrics::Checkpoint {
                        generation: dashboard.generation,
                        store_version: version,
                        requests: dashboard.worker.requests_submitted(),
                        fresh: crate::live_resume_metrics::Fresh::Reading,
                    });
            }
            store_view::set_store_active(
                visibility.is_visible() && dashboard.follows_live_mac(),
                cx,
            );
            if !visibility.is_visible() {
                dashboard.store_view = None;
            }
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

/// What a reading reads on a page whose first reading the worker is still reading.
pub(crate) const LOADING: &str = "Loading";

/// The open picker after its control is pressed: the picker's choices, or none if they
/// showed already.
pub fn toggle(open: Option<Picker>, pressed: Picker) -> Option<Picker> {
    if open == Some(pressed) {
        None
    } else {
        Some(pressed)
    }
}

/// Spreads `view` across `panel` where it is drawn against the clock, and tells it whether its
/// readings still move on with the newest second.
fn zoom_panel(panel: &mut Rc<Panel>, view: crate::chart_axes::TimeWindow, sliding: bool) {
    let Some(window) = panel.window else {
        return;
    };

    if window != view {
        crate::time_range::zoom(Rc::make_mut(panel), view);
    }
    if panel.sliding != sliding {
        Rc::make_mut(panel).sliding = sliding;
    }
}

pub struct LiveDashboard {
    #[cfg(feature = "automation")]
    pub(crate) resume_metrics: crate::live_resume_metrics::ResumeMetrics,
    pub worker: Worker,
    pub request: Request,
    pub snapshot: Option<Box<Snapshot>>,
    pending_snapshot: Option<Box<Snapshot>>,
    /// The store's newest View the window took, which a hidden window lets go of.
    store_view: Option<std::sync::Arc<viewer::store::View>>,
    store_panels: store_view::StorePanels,
    /// Room to choose the Apps the charts by App drawn from `store_view` draw on lines of their
    /// own.
    leaders: viewer::store::Leaders,
    /// The last live reading of each page the reader has left, shown again at once when the
    /// page is opened while the worker reads a fresh one. At most one for each route.
    visited: std::collections::BTreeMap<String, Box<crate::live_acceptance::Presentation>>,
    pub panels: Vec<Rc<Panel>>,
    pub generation: u64,
    /// The worker has not yet answered the last request, so the app polls it quickly.
    pub(crate) reply_pending: bool,
    presence: Presence,
    store_pending: bool,
    pub error: Option<String>,
    pub rows: Vec<Row>,
    pub expanded: Option<(String, usize)>,
    pub executables: Vec<crate::live_executables::Executable>,
    pub inventory: Vec<(String, String)>,
    pub hidden: Vec<String>,
    pub(crate) sections: crate::live_sections::Sections,
    pub hardware_state: Rc<crate::hardware_motion::HardwareState>,
    /// Settings overlays the resource page without changing its query route.
    pub settings: crate::live_settings::Settings,
    pub started: Instant,
    pub seconds: Option<u32>,
    pub updates: u32,
    pub renders: u64,
    pub(crate) plots: crate::live_plot::Plots,
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
    /// Whether the selected second is pinned, which the end of a hover then shows again.
    pub(crate) pinning: Pinning,
    /// The second of the live Mac the page holds over the readings and the charts it showed
    /// when it was held, while it holds one.
    pub(crate) hold: Option<hold::Hold>,
}

pub(crate) struct WindowState {
    request: Request,
    snapshot: Option<Box<Snapshot>>,
    range: crate::time_range::Range,
    pinning: Pinning,
    hidden: Vec<String>,
}

impl WindowState {
    pub(crate) fn reader_stopped(&mut self) {
        if self.request.second.is_some() {
            if let Some(snapshot) = &mut self.snapshot {
                snapshot.histories_pending = false;
            }
        }
    }
}

/// Whether macOS presents the window, and whether its reader has caught up with that, since the
/// reader rests while the window is hidden and reads no page nobody sees, and while the page
/// draws Overview from the store's View, which brings each second it shows.
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
    /// Shown, and the reader rests while the page draws Overview from the store's View, until
    /// a reply to the page's next request arrives.
    Rested,
}

impl Presence {
    /// The presence once macOS reports whether it presents the window.
    fn reported(self, visible: bool) -> Self {
        if visible {
            match self {
                Presence::Opening | Presence::Hiding => Presence::Shown,
                Presence::Hidden => Presence::Showing,
                Presence::Shown | Presence::Showing | Presence::Rested => self,
            }
        } else {
            match self {
                Presence::Shown | Presence::Rested => Presence::Hiding,
                Presence::Showing => Presence::Hidden,
                Presence::Opening | Presence::Hiding | Presence::Hidden => self,
            }
        }
    }
}

/// A drag across one chart: the seconds under the pointer when it went down and where it is now,
/// and where in the window it went down.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Drag {
    pub(crate) chart: String,
    pub(crate) from: u64,
    pub(crate) to: u64,
    pub(crate) pressed_at: gpui::Point<gpui::Pixels>,
}

/// Whether a click or Return holds the selected second, which every chart then marks.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Pinning {
    /// The selected second is the live or paused one.
    Unpinned,
    /// A click on a chart, or Return on a second a key stepped to, pinned the selected second,
    /// which every chart marks and the end of a hover shows again.
    Pinned,
}

pub struct ChartHover {
    pub second: u64,
}

pub enum Row {
    Summary,
    SelfPart(crate::live_diagnostics::SelfPart),
    OverviewStatus,
    /// A component's temperature chart beside the card of its reading at the selected time.
    Temperature {
        /// The component, which names the row.
        area: &'static str,
        /// The summary key of the sensor the collector records for the component, if any.
        recorded: Option<&'static str>,
        /// The card's label for the reading.
        label: &'static str,
        panel: Option<Rc<Panel>>,
    },
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
    #[cfg(feature = "automation")]
    pub(crate) fn resume_store_version(&self) -> Option<u64> {
        self.followed_store_view().map(|view| view.version)
    }

    pub fn new(worker: Worker, route: String, seconds: Option<u32>) -> Self {
        let mut dashboard = Self::new_idle(worker, Self::new_request(route), seconds);
        dashboard.submit();
        dashboard
    }

    fn new_idle(worker: Worker, request: Request, seconds: Option<u32>) -> Self {
        let mut dashboard = Self {
            #[cfg(feature = "automation")]
            resume_metrics: crate::live_resume_metrics::ResumeMetrics::default(),
            worker,
            request,
            snapshot: None,
            pending_snapshot: None,
            visited: std::collections::BTreeMap::new(),
            panels: Vec::with_capacity(8),
            generation: 0,
            reply_pending: false,
            presence: Presence::Shown,
            store_pending: false,
            store_view: None,
            store_panels: store_view::StorePanels::default(),
            leaders: viewer::store::Leaders::new(),
            error: None,
            rows: Vec::with_capacity(4096),
            expanded: None,
            executables: Vec::with_capacity(512),
            inventory: Vec::with_capacity(16),
            hidden: Vec::with_capacity(64),
            sections: crate::live_sections::Sections::default(),
            hardware_state: Rc::new(crate::hardware_motion::HardwareState::new(
                crate::hardware_motion::reduce_motion,
            )),
            settings: crate::live_settings::Settings::default(),
            started: Instant::now(),
            seconds,
            updates: 0,
            renders: 0,
            plots: crate::live_plot::Plots::new(),
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
            pinning: Pinning::Unpinned,
            hold: None,
        };
        if dashboard.request.route == "computer" {
            dashboard.inventory = crate::live_inventory::rows(None);
        }
        dashboard.rebuild();
        dashboard
    }

    pub(crate) fn window_state(&mut self) -> WindowState {
        WindowState {
            request: self.request.clone(),
            snapshot: self.snapshot.take(),
            range: self.range,
            pinning: self.pinning,
            hidden: self.hidden.clone(),
        }
    }

    pub(crate) fn restore(worker: Worker, state: WindowState, cx: &Context<Self>) -> Self {
        let mut dashboard = Self::new_idle(worker, state.request, None);
        dashboard.range = state.range;
        dashboard.pinning = state.pinning;
        dashboard.hidden = state.hidden;
        if let Some(snapshot) = state.snapshot {
            dashboard.generation = snapshot.generation;
            dashboard.accept(snapshot);
        }
        if !dashboard.pinned_reading_complete() {
            if dashboard.can_resume_from_store(cx) {
                dashboard.presence = Presence::Showing;
            } else {
                dashboard.request_restoration();
            }
        }

        dashboard
    }

    fn pinned_reading_complete(&self) -> bool {
        let Some(second) = self.request.second else {
            return false;
        };
        let Some(snapshot) = &self.snapshot else {
            return false;
        };
        if snapshot.histories_pending {
            return false;
        }
        if snapshot.selection["second"].as_u64() != Some(second) {
            return false;
        }
        if let Some(identity) = &self.request.identity {
            return crate::live_source::identity_validate(
                identity,
                &snapshot.selection["selected"],
            )
            .is_ok();
        }

        true
    }

    fn uses_live_store(&self) -> bool {
        if self.request.route != "overview" {
            return false;
        }

        self.follows_live_mac()
    }

    /// Whether the window follows the live Mac's newest second, on whichever page it shows. The
    /// store ingests while it does, so Overview opened again has no minutes to catch up from.
    fn follows_live_mac(&self) -> bool {
        if self.worker.mode() != crate::live_source_mode::SourceMode::Live {
            return false;
        }

        self.request.second.is_none()
    }

    fn can_resume_from_store(&self, cx: &Context<Self>) -> bool {
        if !self.uses_live_store() {
            return false;
        }
        if !store_view::store_installed(cx) {
            return false;
        }

        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| !snapshot.histories_pending)
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
        if self.settings.refresh_notice(cx) {
            cx.notify();
        }
        if self
            .hardware_state
            .poll_animation(self.settings.animate_fans)
        {
            cx.notify();
        }
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
        if self.presence == Presence::Hidden {
            self.store_pending = false;
        } else {
            self.preview_tick(cx);
            self.hold_tick();
            // A held second keeps the View its charts were drawn from.
            if self.hold.is_none() {
                if let Some(view) = store_view::take_store_view(cx) {
                    if self.take_view(view) {
                        cx.notify();
                    }
                }
            }
            self.tick_rest();
            self.tick_store_pending(cx);
        }
        let status = self.status();
        if self.last_status != status {
            self.last_status = status;
            cx.notify();
        }
        self.tick_deadline(cx);
    }

    fn tick_store_pending(&mut self, cx: &Context<Self>) {
        self.store_pending = false;
        if self.request.route != "overview" {
            return;
        }

        if self.native.source != crate::live_source_mode::SourceMode::Live {
            return;
        }

        if self.request.second.is_some() {
            return;
        }

        self.store_pending = store_view::store_pending(cx);
    }

    fn tick_deadline(&mut self, cx: &mut Context<Self>) {
        if let Some(seconds) = self.seconds {
            if self.started.elapsed() >= Duration::from_secs(u64::from(seconds)) {
                self.seconds = None;
                println!(
                    "{}",
                    serde_json::json!({"mode":"live", "route":self.request.route,
                    "elapsed_seconds":self.started.elapsed().as_secs_f64(),
                    "updates":self.updates,"renders":self.renders,
                    "hardware_animation":self.hardware_state.animation_statistics(),
                    "plot_renders":self.plots.renders(),"plots_held":self.plots.held(),
                    "curve_builds":self.plots.curve_builds(),
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
        let visible = matches!(
            self.presence,
            Presence::Shown | Presence::Showing | Presence::Rested
        );
        if !self.follows_live_mac() && self.hold.is_none() {
            self.store_view = None;
        }
        store_view::set_store_active(visible && self.follows_live_mac(), cx);
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
                self.presence = Presence::Shown;
                self.tick_shown(cx);
            }
            Presence::Opening | Presence::Shown | Presence::Hidden | Presence::Rested => {}
        }
    }

    /// Rests the reader of a window macOS hides, as a closed window's reader rests. A reader of
    /// a recording opened read-only reads nothing once its page is read, so it is left alone.
    /// The live reader leaves its session unchanged until the window asks for a page again.
    fn tick_hidden(&mut self) {
        self.store_view = None;

        match self.worker.hand_over() {
            Ok(Some(resting)) => {
                self.worker = resting;
                self.reply_pending = false;
            }
            Ok(None) => {}
            Err(error) => self.error = Some(error),
        }
    }

    /// Rests the reader while the page draws Overview from the store's View, which brings each
    /// second the page shows.
    fn tick_rest(&mut self) {
        // Each rest cancels what the resting reader does, and a hidden window's reader rests.
        if self.presence != Presence::Shown {
            return;
        }
        // Resting would drop the read the page waits on, its first readings among them.
        if self.reply_pending {
            return;
        }
        // A hover may wait on a read of a second the View does not hold.
        if self.preview.is_some() {
            return;
        }
        // Resting would drop the read of a held second whose top Apps the View lacks.
        if self.hold_reading() {
            return;
        }
        // The App histories the page's first readings left pending come in the reader's next
        // read, which a resting reader drops.
        if let Some(snapshot) = &self.snapshot {
            if snapshot.histories_pending {
                return;
            }
        }
        if self.followed_store_view().is_none() {
            return;
        }

        match self.worker.hand_over() {
            Ok(Some(resting)) => {
                self.worker = resting;
                self.presence = Presence::Rested;
            }
            Ok(None) => {}
            Err(error) => self.error = Some(error),
        }
    }

    /// Whether the page rested its reader while it draws Overview from the store's View.
    pub(crate) fn reader_rested(&self) -> bool {
        self.presence == Presence::Rested
    }

    /// Draws the page shown again from the store's newest View, released while the window hid,
    /// and rests the reader while the View brings each second the page shows. Otherwise asks the
    /// rested reader for the page again, which it reads as newly opened, readings first.
    fn tick_shown(&mut self, cx: &mut Context<Self>) {
        if let Some(view) = store_view::take_store_view(cx) {
            if self.take_view(view) {
                cx.notify();
            }
        }
        self.tick_rest();
        if self.presence == Presence::Rested {
            return;
        }
        if self.reply_pending {
            return;
        }

        if self.pinned_reading_complete() {
            return;
        }

        if self.can_resume_from_store(cx) {
            if store_view::store_pending(cx) {
                self.presence = Presence::Showing;
                return;
            }
        }

        if self.worker.mode() == crate::live_source_mode::SourceMode::Live {
            self.request_restoration();
        }
    }

    fn request_restoration(&mut self) {
        if self.request.second.is_none() {
            self.request_reading();
            return;
        }

        match self.worker.restore(self.page_request()) {
            Ok(generation) => {
                self.generation = generation;
                self.reply_pending = true;
            }
            Err(error) => self.error = Some(error),
        }
    }

    /// Takes the worker's answer to the page or to a hover, and answers whether the page changed.
    pub(crate) fn take_update(&mut self, update: Update) -> bool {
        if self.hold_answered(&update) {
            return self.hold_receive(update);
        }
        if self.preview.is_some() {
            return self.preview_receive(update);
        }

        self.receive(update)
    }

    pub(crate) fn receive(&mut self, update: Update) -> bool {
        // The worker numbers its commands from 1, so a generation of 0 says the page has no
        // request out whose reply it would take.
        if self.generation == 0 {
            return false;
        }

        match update {
            Update::Ready(snapshot) => {
                if snapshot.generation != self.generation {
                    return false;
                }

                self.reply_pending = false;
                self.accept(snapshot);
                self.hold_resolve();
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
        if self.presence == Presence::Rested {
            self.presence = Presence::Shown;
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
        if self.request.route == "computer" {
            let summary = self
                .snapshot
                .as_ref()
                .map(|snapshot| &snapshot.selection["summary"]);
            self.inventory = crate::live_inventory::rows(summary);
        }
        self.error = None;
        self.draw_store_view();
        self.zoom_panels();
        self.refresh_chart_inspector();
        self.received_at = SystemTime::now();
        self.pending_snapshot = None;
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
        self.submit_request();
        self.install_loading_panels();
        self.rebuild();
    }

    /// Gives a page with no readings to show the charts its readings will fill, empty, so each
    /// row takes its final size before the first reading arrives and that reading moves nothing.
    fn install_loading_panels(&mut self) {
        assert!(self.snapshot.is_none());
        if !self.placeholders.is_empty() {
            return;
        }

        match crate::live_projection::empty_panels(&self.request.route) {
            Ok(panels) => self.panels = panels,
            Err(error) => self.error = Some(error),
        }
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
        if self.hold_reading() {
            return true;
        }
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
        if self.store_pending {
            return true;
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
        if self.settings.open {
            self.settings.open = false;
            if self.request.route == route {
                cx.notify();
                return;
            }
        }

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
        self.settings.open = false;
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
            self.inventory = crate::live_inventory::rows(None);
        }
        self.rebuild();
        self.submit();
        if self.request.second.is_none() {
            if let Some(mut kept) = self.visited.remove(route) {
                kept.request = self.request.clone();
                self.accept_presentation(kept);
            } else {
                // The store kept its View current while another page showed, so an Overview
                // with no page of its own kept draws that View at once.
                if self.draw_store_view() {
                    self.zoom_panels();
                    self.rebuild();
                }
            }
        }
    }

    pub fn pause(&mut self, cx: &mut Context<Self>) {
        let (second, identity) = self.pause_target();
        // Pause holds the newest second without pinning it, and the latest reading lets go of
        // a pin.
        self.pinning = Pinning::Unpinned;
        self.request.second = second;
        self.request.identity = identity;
        self.request.offset = 0;
        self.request.apps_offset = 0;
        self.request.members_offset = 0;
        self.request.cpu_offset = 0;
        self.select(cx);
    }

    /// The second and the reading Pause asks for: none where the page holds a second, so it
    /// follows the newest again, and otherwise the second its reading shows.
    pub(crate) fn pause_target(&self) -> (Option<u64>, Option<serde_json::Value>) {
        if self.request.second.is_some() {
            return (None, None);
        }
        // The page shows the View's newest second, which the worker may not have read yet.
        if let Some(second) = self.store_view_last_second() {
            let identity = self
                .store_view_target(second)
                .and_then(|target| target.identity);
            return (Some(second), identity);
        }
        let Some(snapshot) = self.context_snapshot() else {
            return (None, self.request.identity.clone());
        };

        let identity = &snapshot.selection["selected"];
        let identity = if identity.is_null() {
            None
        } else {
            Some(identity.clone())
        };
        (snapshot.selection["second"].as_u64(), identity)
    }

    pub fn inspect(&mut self, second: u64, cx: &mut Context<Self>) {
        let target = self.inspection_target(second);
        self.inspect_target(target, cx);
    }

    /// The reading of `second` the page's readings name, or the second alone where they name
    /// none of it.
    fn inspection_target(&self, second: u64) -> crate::live_hover::Target {
        let second = self.context_snapshot().map_or(second, |snapshot| {
            crate::live_projection::inspection_second(&snapshot.selection, second)
        });
        let identity = self.context_snapshot().and_then(|snapshot| {
            crate::live_projection::inspection_identity(&snapshot.selection, second)
        });

        crate::live_hover::Target { second, identity }
    }

    /// Selects the reading of `target`, from the first row of each of the page's tables.
    fn inspect_target(&mut self, target: crate::live_hover::Target, cx: &mut Context<Self>) {
        self.inspect_target_request(target);
        self.select(cx);
    }

    /// Makes `target` the request's reading, from the first row of each of the page's tables.
    fn inspect_target_request(&mut self, target: crate::live_hover::Target) {
        self.request.second = Some(target.second);
        self.request.identity = target.identity;
        self.request.offset = 0;
        self.request.apps_offset = 0;
        self.request.members_offset = 0;
        self.request.cpu_offset = 0;
    }

    /// Selects `second` and holds every page to it, so the end of a hover shows its readings
    /// again until the latest reading or the range's reset lets it go.
    pub(crate) fn pin_second(&mut self, second: u64, cx: &mut Context<Self>) {
        let target = self.inspection_target(second);
        self.pin_target(target, cx);
    }

    /// Selects the reading of `target` and holds every page to it, as `pin_second` does. On the
    /// live Mac the page holds it over the readings and the charts it shows, which keep the
    /// window they were drawn in, and asks the reader for no window around it. A saved
    /// recording's page selects it, which lays the window around it.
    pub(crate) fn pin_target(&mut self, target: crate::live_hover::Target, cx: &mut Context<Self>) {
        self.pinning = Pinning::Pinned;
        if self.worker.mode() == crate::live_source_mode::SourceMode::Live
            && self.snapshot.is_some()
        {
            if let Some(anchor) = self.range_anchor() {
                self.hold_target(target, anchor.context_last, cx);
                return;
            }
        }
        self.inspect_target(target, cx);
    }

    /// The pinned second, while one is pinned.
    pub(crate) fn pinned_second(&self) -> Option<u64> {
        if self.pinning == Pinning::Unpinned {
            return None;
        }

        assert!(self.request.second.is_some());
        self.request.second
    }

    /// Asks for the request's second and keeps the page's readings shown until the reply, which
    /// takes the reader hundreds of milliseconds. Where those readings hold the second, the page
    /// shows it from them at once, as an arrow step does. A page with nothing shown asks as any
    /// other request does.
    pub(crate) fn select(&mut self, cx: &mut Context<Self>) {
        self.hold = None;
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
        if self.hold.is_some() {
            return Some(self.range.held_window(anchor));
        }

        Some(self.range.window(anchor))
    }

    /// What the range is laid against: the selected second, and the last second the charts hold.
    pub(crate) fn range_anchor(&self) -> Option<crate::time_range::Anchor> {
        if let Some(hold) = &self.hold {
            return Some(crate::time_range::Anchor {
                selected: hold.reading.target.second,
                context_last: hold.last,
            });
        }
        if let Some(last) = self.store_view_last_second() {
            return Some(crate::time_range::Anchor {
                selected: last,
                context_last: last,
            });
        }

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
        let sliding = self.panels_slide();
        for panel in &mut self.panels {
            zoom_panel(panel, view, sliding);
        }
    }

    /// Whether the charts' readings still move on with the newest second, as they do while the
    /// page follows it over a range that slides.
    fn panels_slide(&self) -> bool {
        if self.request.second.is_some() {
            return false;
        }

        self.range.slides()
    }

    /// Shows `range`, which a menu choice or a drag picked, without asking for another reading.
    pub(crate) fn set_range(&mut self, range: crate::time_range::Range, cx: &mut Context<Self>) {
        self.range = range;
        self.drag = None;
        self.store_view_range_picked();
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
        self.hold = None;
        let held = self.request.second.take().is_some();
        if held {
            self.request.identity = None;
        }
        self.pinning = Pinning::Unpinned;
        self.set_range(crate::time_range::Range::History, cx);
        if held {
            self.request_reading();
        }
    }

    /// Asks for the second now shown, as Pause does, so the view stops following without
    /// blanking.
    fn hold_second(&mut self) {
        if self.request.second.is_some() {
            return;
        }
        let (second, identity) = self.pause_target();
        if second.is_none() {
            return;
        }

        self.request.second = second;
        self.request.identity = identity;
        self.request_reading();
    }

    /// Asks the worker for the request as it stands, leaving the reading shown until the reply.
    pub(crate) fn request_reading(&mut self) {
        if let Some(preview) = &mut self.preview {
            preview.page_reply_kept = false;
        }
        match self.worker.request(self.page_request()) {
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

    /// What a reading the page lacks reads: that it is still being read on a newly opened
    /// page, and otherwise `unavailable`.
    pub(crate) fn absent_reading<'a>(&self, unavailable: &'a str) -> &'a str {
        if self.is_opening() {
            return LOADING;
        }
        unavailable
    }

    /// The text a reading shows: `reading`, or that it is still being read on a newly opened
    /// page, which has no readings yet.
    pub(crate) fn shown_reading(&self, reading: String) -> String {
        if self.is_opening() {
            return LOADING.into();
        }
        reading
    }

    /// What a history card reads where the snapshot holds no such history: that it is still
    /// being read on a newly opened page, that the page's App histories are still being read,
    /// and otherwise `unavailable`.
    pub(crate) fn history_absent(&self, unavailable: &str) -> String {
        if self.is_opening() {
            return LOADING.into();
        }
        if let Some(snapshot) = &self.snapshot {
            if snapshot.histories_pending {
                return "Reading App history".into();
            }
        }
        unavailable.into()
    }

    pub(crate) fn status_at(&self, now: SystemTime) -> String {
        // Computer shows the machine facts of the latest reading, with no time selection.
        if self.request.route == "computer" {
            return "System snapshot".into();
        }
        let Some(snapshot) = &self.snapshot else {
            // At launch the store's View draws the page before the worker's first reading.
            if let Some(newest) = self.store_view_last_second() {
                return self.status_at_newest(newest, now);
            }
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
        // The store's View the page draws may hold seconds the worker has not read yet.
        let newest = self
            .store_view_last_second()
            .or_else(|| snapshot.catalog["last_second"].as_u64());
        let Some(newest) = newest else {
            return "No live readings".into();
        };

        self.status_at_newest(newest, now)
    }

    /// Whether `newest`, the newest second the page shows, keeps up with the clock at `now`.
    fn status_at_newest(&self, newest: u64, now: SystemTime) -> String {
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

#[path = "live_hold.rs"]
mod hold;
#[path = "live_store_view.rs"]
mod store_view;

pub use store_view::install_store;
pub(crate) use store_view::set_store_active;
#[cfg(feature = "automation")]
pub(crate) use store_view::store_activity;

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
    fn resource_view(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.install_section_focus(cx);
        self.reading_viewport(window.viewport_size());
        self.install_cpu_focus(window, cx);
        let inspection_focus = self.install_inspection_focus(window, cx);
        let body = self.inspection_body(&inspection_focus, cx);

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .child(self.header(cx))
            .child(self.native_notice(cx))
            .children(self.error_notice())
            .child(body)
    }

    fn render_shell(
        &self,
        root_focus: &gpui::FocusHandle,
        body: gpui::AnyElement,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let hardware_state = Rc::clone(&self.hardware_state);

        div()
            .id("flightdeck-live")
            .track_focus(root_focus)
            .on_action(|_: &crate::native_workflows::CloseWindow, window, _| {
                window.remove_window();
            })
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
                    .child(body),
            )
            .children(self.chart_inspector_overlay(cx))
            .child(
                gpui::canvas(
                    |_, _, _| (),
                    move |_, (), _, _| hardware_state.finish_frame(),
                )
                .absolute()
                .top_0()
                .left_0()
                .size(px(0.0)),
            )
            .into_any_element()
    }

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
        #[cfg(feature = "automation")]
        self.measure_resume_frame(window, cx);
        if self.rendered_source != Some(self.native.source) {
            window.set_window_title(self.native.source.title());
            self.rendered_source = Some(self.native.source);
        }

        let root_focus = self.install_root_focus(window, cx);
        self.hardware_state.begin_frame(
            self.settings.animate_fans
                && !self.settings.open
                && self.request.route == "overview"
                && !self.native.time_menu_open
                && self.chart_inspector.is_none()
                && self.chart_hover.is_none(),
        );
        self.plots.begin_frame();
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

        let body = if self.settings.open {
            self.settings_view(cx).into_any_element()
        } else {
            self.resource_view(window, cx).into_any_element()
        };

        self.render_shell(&root_focus, body, cx)
    }
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    #[test]
    fn a_resting_page_keeps_fast_polling_until_store_catch_up_finishes() {
        // Goal: a resumed page takes the fresh View without a quarter-second polling delay.
        // Method: settle the legacy reply and leave only the store's catch-up outstanding.
        let mut page = dashboard();
        page.reply_pending = false;
        page.presence = Presence::Rested;
        page.snapshot.as_mut().unwrap().histories_pending = false;
        page.store_pending = true;

        assert!(page.awaiting_reply());
    }

    #[test]
    fn a_hidden_page_never_fast_polls_its_pending_store() {
        // Goal: pending store work cannot wake a hidden page repeatedly.
        // Method: leave only store catch-up outstanding and inspect the hidden polling policy.
        let mut page = dashboard();
        page.reply_pending = false;
        page.presence = Presence::Hidden;
        page.snapshot.as_mut().unwrap().histories_pending = false;
        page.store_pending = true;

        assert!(!page.awaiting_reply());
    }

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
            selection: crate::live_source::Presentation::Legacy(
                json!({"second":1000,"context_last_second":1000,"selected":{"publication":1},
                "summary":{"memory_total_bytes":4096},"trends":[]}),
            ),
            pages: (BTreeMap::from([(
                "ticks".into(),
                json!({"offset":0,"total":1,"rows":[["1"]],
                    "columns":["publication"]}),
            )]))
            .into_iter()
            .map(|(key, value)| (key, crate::live_source::Presentation::Legacy(value)))
            .collect(),
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
    fn a_page_opened_for_the_first_time_says_it_is_loading_until_its_reading_arrives() {
        // Goal: the first frame of a page the worker is still reading lays out its rows and
        // charts and shows no "Unavailable", which would flash for the few milliseconds the read
        // takes. Method: open a page with no kept reading, then fail its read.
        let mut dashboard = dashboard();

        dashboard.switch_route("cpu");
        assert!(!dashboard.reading_rows().is_empty());
        assert!(!dashboard.panels.is_empty());
        assert_eq!(dashboard.history_absent("App history unavailable"), LOADING);
        let failed = dashboard.generation;
        dashboard.receive(Update::Failed {
            generation: failed,
            message: "Recording read failed".into(),
        });

        assert_eq!(
            dashboard.history_absent("App history unavailable"),
            "App history unavailable"
        );
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
        missing.selection = crate::live_source::Presentation::Legacy(
            json!({"second":1001,"context_last_second":1001,"selected":null,
            "summary":null,"trends":[]}),
        );
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
        next.selection.legacy_mut().unwrap()["selected"]["publication"] = json!(2);
        next.pages.get_mut("ticks").unwrap().legacy_mut().unwrap()["rows"] = json!([["2"]]);

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
        next.selection.legacy_mut().unwrap()["selected"]["publication"] = json!(2);

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
