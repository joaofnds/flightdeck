use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use gpui::{
    Context, ListAlignment, ListOffset, ListState, Render, Window, div, list, prelude::*, px, rgb,
};
use serde_json::Value;

use crate::charts::{GRID, MUTED, TEXT};
use crate::data::Panel;
use crate::live_source::{Request, Snapshot, Update, Worker};
use crate::live_timer::Timer;

pub const ROUTES: [(&str, &str); 9] = [
    ("overview", "Overview"),
    ("cpu", "CPU & processes"),
    ("apps", "Apps"),
    ("memory", "Memory"),
    ("gpu", "GPU"),
    ("energy", "Energy"),
    ("network", "Network"),
    ("computer", "Computer"),
    ("raw", "Recorded readings"),
];

pub struct LiveDashboard {
    pub worker: Worker,
    pub request: Request,
    pub snapshot: Option<Box<Snapshot>>,
    pub panels: Vec<Rc<Panel>>,
    pub generation: u64,
    pub error: Option<String>,
    pub rows: Vec<Row>,
    pub expanded: Option<(String, usize)>,
    pub executables: Vec<crate::live_executables::Executable>,
    pub executable: Option<String>,
    pub inventory: Vec<(String, String)>,
    pub hidden: Vec<String>,
    pub timer: Option<Timer>,
    pub hardware_state: Rc<crate::hardware_motion::HardwareState>,
    pub started: Instant,
    pub seconds: Option<u32>,
    pub updates: u32,
    pub renders: u64,
    pub animate: bool,
    pub reveal_hardware: bool,
    pub hover_second: Option<u64>,
    pub app_picker_open: bool,
    list: ListState,
    last_status: String,
}

pub enum Row {
    Summary,
    OverviewStatus,
    AppSelector,
    AppSummary,
    AppColumns(&'static str),
    CPUActivity,
    Charts(Vec<Rc<Panel>>),
    Heading(String),
    Hardware,
    TableHeading(String),
    Table(String, usize),
    App(usize),
    Process(usize),
    Executable(usize),
    Inventory,
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
            },
            snapshot: None,
            panels: Vec::with_capacity(8),
            generation: 0,
            error: None,
            rows: Vec::with_capacity(4096),
            expanded: None,
            executables: Vec::with_capacity(512),
            executable: None,
            inventory: Vec::with_capacity(16),
            hidden: Vec::with_capacity(64),
            timer: None,
            hardware_state: Rc::new(crate::hardware_motion::HardwareState::new()),
            started: Instant::now(),
            seconds,
            updates: 0,
            renders: 0,
            animate: false,
            reveal_hardware: false,
            hover_second: None,
            app_picker_open: false,
            list: ListState::new(0, ListAlignment::Top, px(100.0)),
            last_status: String::new(),
        };
        if dashboard.request.route == "computer" {
            dashboard.inventory = crate::live_inventory::rows();
        }
        dashboard.rebuild();
        dashboard.submit();
        dashboard
    }

    pub fn tick(&mut self, cx: &mut Context<Self>) {
        match self.worker.take() {
            Ok(Some(Update::Ready(snapshot))) => {
                if snapshot.generation == self.generation {
                    self.accept(snapshot);
                    cx.notify();
                }
            }
            Ok(Some(Update::Failed {
                generation,
                message,
            })) => {
                if generation == self.generation {
                    self.error = Some(message);
                    cx.notify();
                }
            }
            Ok(None) => {}
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
        let status = self.status();
        if self.last_status != status {
            self.last_status = status;
            cx.notify();
        }
        if let Some(seconds) = self.seconds {
            if self.started.elapsed() >= Duration::from_secs(u64::from(seconds)) {
                println!(
                    "{}",
                    serde_json::json!({"mode":"live", "route":self.request.route,
                    "elapsed_seconds":self.started.elapsed().as_secs_f64(),
                    "updates":self.updates,"renders":self.renders,
                    "read_ms":self.snapshot.as_ref().map(|snapshot| snapshot.read_ms),
                    "selected":self.snapshot.as_ref().map(|s| &s.selection["selected"]),
                    "error":self.error})
                );
                cx.quit();
            }
        }
    }

    fn accept(&mut self, snapshot: Box<Snapshot>) {
        if let Err(error) = self.accept_offsets(&snapshot) {
            self.error = Some(error);
            return;
        }
        let groups = match crate::live_executables::groups(snapshot.detail.as_ref()) {
            Ok(groups) => groups,
            Err(error) => {
                self.error = Some(error);
                return;
            }
        };
        match crate::live_projection::panels(
            &self.request.route,
            &snapshot.selection,
            snapshot.history.as_ref(),
        ) {
            Ok(panels) => {
                self.panels = panels;
                self.error = None;
            }
            Err(error) => {
                self.error = Some(error);
                return;
            }
        }
        self.updates += 1;
        self.executables = groups;
        self.snapshot = Some(snapshot);
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

    fn accept_offsets(&mut self, snapshot: &Snapshot) -> Result<(), String> {
        let table = if self.request.route == "cpu" {
            "processes"
        } else {
            &self.request.table
        };
        if let Some(page) = snapshot.pages.get(table) {
            self.request.offset = page["offset"].as_u64().ok_or("Missing raw page offset")?;
        }
        if let Some(apps) = &snapshot.apps {
            self.request.apps_offset = u32::try_from(
                apps["page"]["offset"]
                    .as_u64()
                    .ok_or("Missing Apps page offset")?,
            )
            .map_err(|error| error.to_string())?;
        }
        if let Some(detail) = &snapshot.detail {
            if !detail.is_null() {
                if self.request.app.is_none() {
                    self.request.app = Some(crate::live_source::AppKey {
                        kind: detail["app"]["key"]["kind"]
                            .as_str()
                            .ok_or("Missing selected App kind")?
                            .into(),
                        bytes_hex: detail["app"]["key"]["bytes_hex"]
                            .as_str()
                            .ok_or("Missing selected App bytes")?
                            .into(),
                    });
                }
                self.request.members_offset = u32::try_from(
                    detail["page"]["offset"]
                        .as_u64()
                        .ok_or("Missing process page offset")?,
                )
                .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }

    pub fn submit(&mut self) {
        match self.worker.request(self.request.clone()) {
            Ok(generation) => self.generation = generation,
            Err(error) => self.error = Some(error),
        }
    }

    pub fn navigate(&mut self, route: &str, cx: &mut Context<Self>) {
        self.hover_second = None;
        self.request.route = route.into();
        self.request.offset = 0;
        self.request.members_offset = 0;
        self.snapshot = None;
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
            if let Some(snapshot) = &self.snapshot {
                self.request.second = snapshot.selection["second"].as_u64();
                self.request.identity = Some(snapshot.selection["selected"].clone());
            }
        }
        self.request.offset = 0;
        self.request.apps_offset = 0;
        self.request.members_offset = 0;
        self.submit();
        cx.notify();
    }

    pub fn inspect(&mut self, second: u64, cx: &mut Context<Self>) {
        self.request.second = Some(second);
        self.request.identity = self.snapshot.as_ref().and_then(|snapshot| {
            crate::live_projection::inspection_identity(&snapshot.selection, second)
        });
        self.request.offset = 0;
        self.request.apps_offset = 0;
        self.request.members_offset = 0;
        self.submit();
        cx.notify();
    }

    pub fn status(&self) -> String {
        let Some(snapshot) = &self.snapshot else {
            return "No live readings".into();
        };
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
        let previous = self.rows.len();
        self.rows.clear();
        if self.request.route == "computer" {
            self.rows.push(Row::Inventory);
        } else {
            self.rebuild_readings();
        }
        if previous != self.rows.len() {
            self.list.reset(self.rows.len());
        }
    }

    fn rebuild_readings(&mut self) {
        if self.request.route == "apps" {
            self.rebuild_apps();
            return;
        }
        if self.request.route == "memory" {
            self.rows.push(Row::Summary);
        }
        if self.request.route == "cpu" {
            self.rows.push(Row::CPUActivity);
        }
        for panels in self.panels.chunks(2) {
            self.rows.push(Row::Charts(panels.to_vec()));
        }
        if self.request.route == "overview" {
            self.rows.push(Row::OverviewStatus);
            self.rows.push(Row::Hardware);
        }
        if let Some(snapshot) = &self.snapshot {
            for (name, page) in &snapshot.pages {
                if name == "fans" {
                    continue;
                }
                self.rows.push(Row::TableHeading(name.clone()));
                for index in 0..array_len(Some(page), "rows") {
                    self.rows.push(Row::Table(name.clone(), index));
                }
            }
        }
        assert!(self.rows.len() <= 4096);
    }

    fn rebuild_apps(&mut self) {
        self.rows.push(Row::AppSelector);
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        if self.app_picker_open {
            self.rows.push(Row::AppColumns("apps"));
            for index in self.app_choice_indices() {
                self.rows.push(Row::App(index));
            }
        }
        self.rows.push(Row::AppSummary);
        let Some(detail) = crate::live_app_views::selected_detail(
            snapshot.detail.as_ref(),
            self.request.app.as_ref(),
        ) else {
            return;
        };
        if detail.is_null() {
            return;
        }
        self.rows.push(Row::Heading(format!(
            "Inside {} · executable groups",
            crate::live_views::text(&detail["app"]["display_name"])
        )));
        self.rows.push(Row::AppColumns("executables"));
        for index in 0..self.executables.len() {
            self.rows.push(Row::Executable(index));
        }
        self.rows
            .push(Row::Heading("Process instances · selected app".into()));
        self.rows.push(Row::AppColumns("processes"));
        for index in 0..array_len(Some(detail), "processes") {
            if let Some(key) = &self.executable {
                if !crate::live_executables::matches(&detail["processes"][index], key) {
                    continue;
                }
            }
            self.rows.push(Row::Process(index));
        }
        self.rows
            .push(Row::Heading("Selected app · last 15 minutes".into()));
        for panels in self.panels.chunks(2) {
            self.rows.push(Row::Charts(panels.to_vec()));
        }
    }
}

fn array_len(value: Option<&Value>, key: &str) -> usize {
    value
        .and_then(|value| value[key].as_array())
        .map_or(0, |rows| rows.len().min(512))
}

impl Render for LiveDashboard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
        let body = list(
            self.list.clone(),
            cx.processor(|this, index: usize, _, cx| this.row(index, cx)),
        )
        .flex_1()
        .min_h_0();
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(rgb(0x11_12_17))
            .text_color(rgb(TEXT))
            .font_family(".AppleSystemUIFont")
            .text_size(px(13.0))
            .on_key_down(crate::dashboard::keyboard_navigation)
            .child(
                div()
                    .h(px(48.0))
                    .px(px(24.0))
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(rgb(GRID))
                    .child("◉  FLIGHTDECK")
                    .child(div().text_color(rgb(MUTED)).child(self.status())),
            )
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
    }
}
