use std::rc::Rc;

use gpui::{
    AnyElement, Context, IntoElement, ListAlignment, ListOffset, ListState, MouseButton, Render,
    SharedString, Window, div, list, prelude::*, px, rgb,
};

use crate::charts::{BLUE, GRID, Inspection, MUTED, ORANGE, PANEL, TEXT};
use crate::data::{Capture, Catalog, Panel, reading};
use crate::performance::Performance;

pub struct Dashboard {
    pub capture: Rc<Capture>,
    pub catalog: Rc<Catalog>,
    pub route: String,
    pub inspection: Inspection,
    pub app: String,
    pub process: Option<String>,
    pub hidden: Rc<Vec<String>>,
    pub performance: Performance,
    pub animate: bool,
    pub diagnostic_selected: usize,
    pub hardware_state: Rc<crate::hardware_motion::HardwareState>,
    pub replay: Option<crate::replay::Replay>,
    pub replay_timer: Option<crate::live_timer::Timer>,
    rows: Vec<Row>,
    list: ListState,
    help: Option<String>,
}

enum Row {
    Summary,
    Heading(String),
    Panels(Vec<Rc<Panel>>),
    Computer,
    AppPicker,
    Hardware,
}

impl Dashboard {
    pub fn new(
        capture: Capture,
        catalog: Catalog,
        performance: Performance,
        replay: Option<crate::replay::Replay>,
    ) -> Self {
        let route = performance.options.page.clone();
        let animate = performance.options.animate;
        let mut dashboard = Self {
            capture: Rc::new(capture),
            catalog: Rc::new(catalog),
            route,
            inspection: Inspection {
                lower: 90,
                selected: 120,
                upper: 120,
            },
            app: "Brave Browser".into(),
            process: None,
            hidden: Rc::new(Vec::new()),
            performance,
            animate,
            diagnostic_selected: 29,
            hardware_state: Rc::new(crate::hardware_motion::HardwareState::new()),
            replay,
            replay_timer: None,
            rows: Vec::with_capacity(128),
            list: ListState::new(0, ListAlignment::Top, px(100.0)),
            help: None,
        };
        if dashboard.performance.options.range_minutes == 60 {
            dashboard.inspection.lower = 0;
        }
        dashboard.rebuild_rows();
        if dashboard.performance.options.hardware {
            if let Some(index) = dashboard
                .rows
                .iter()
                .position(|row| matches!(row, Row::Hardware))
            {
                dashboard.list.scroll_to(ListOffset {
                    item_ix: index,
                    offset_in_item: px(0.0),
                });
            }
        }
        dashboard
    }

    pub fn rebuild_rows(&mut self) {
        self.rows.clear();
        if self.route == "computer" {
            self.rows.push(Row::Computer);
        } else {
            if self.route != "self" {
                self.rows.push(Row::Summary);
            }
            if self.route == "apps" {
                self.rows.push(Row::AppPicker);
                if !self.catalog.app_pages.contains_key(&self.app) {
                    self.rows.push(Row::Heading(
                        "Process histories unavailable in this capture. App totals follow.".into(),
                    ));
                }
            }
            let panels = if self.route == "self" {
                self.catalog.self_panels.clone()
            } else {
                if self.route == "apps" {
                    self.catalog.app_panels(&self.capture, &self.app)
                } else {
                    if self.inspection.lower == 0 {
                        self.catalog
                            .hour_pages
                            .get(&self.route)
                            .cloned()
                            .unwrap_or_default()
                    } else {
                        self.catalog
                            .pages
                            .get(&self.route)
                            .cloned()
                            .unwrap_or_default()
                    }
                }
            };
            let mut pending = Vec::with_capacity(3);
            for panel in panels.iter().take(crate::data::PANEL_LIMIT) {
                match panel.kind.as_str() {
                    "section" | "fold" => {
                        flush(&mut pending, &mut self.rows);
                        self.rows.push(Row::Heading(panel.title.clone()));
                    }
                    "hardware" => {
                        flush(&mut pending, &mut self.rows);
                        self.rows.push(Row::Hardware);
                    }
                    _ => {
                        pending.push(Rc::clone(panel));
                        let columns = if matches!(self.route.as_str(), "overview" | "cpu" | "gpu") {
                            3
                        } else {
                            2
                        };
                        if pending.len() == columns {
                            flush(&mut pending, &mut self.rows);
                        }
                    }
                }
            }
            flush(&mut pending, &mut self.rows);
        }
        self.list.reset(self.rows.len());
    }

    fn navigate(&mut self, route: &str, cx: &mut Context<Self>) {
        self.route = route.into();
        self.rebuild_rows();
        cx.notify();
    }

    fn navigation(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut sidebar = div()
            .flex()
            .flex_col()
            .w(px(164.0))
            .flex_shrink_0()
            .h_full()
            .p(px(12.0))
            .gap(px(3.0))
            .border_r_1()
            .border_color(rgb(GRID))
            .child(
                div()
                    .py(px(12.0))
                    .px(px(10.0))
                    .text_color(rgb(MUTED))
                    .text_size(px(11.0))
                    .child("SYSTEM MONITOR"),
            );
        for (index, [route, title, _]) in self.catalog.navigation.iter().enumerate() {
            let destination = route.clone();
            if matches!(index, 2 | 9) {
                sidebar = sidebar.child(div().h(px(1.0)).my(px(10.0)).bg(rgb(GRID)));
            }
            sidebar = sidebar.child(
                div()
                    .id(SharedString::from(route.clone()))
                    .px(px(12.0))
                    .py(px(11.0))
                    .rounded(px(5.0))
                    .cursor_pointer()
                    .focusable()
                    .focus(|style| style.bg(rgb(0x32_46_61)))
                    .bg(rgb(if self.route == *route {
                        0x1c_2b_40
                    } else {
                        0x11_12_17
                    }))
                    .text_color(rgb(if self.route == *route { BLUE } else { TEXT }))
                    .hover(|style| style.bg(rgb(0x2c_2f_35)))
                    .on_click(cx.listener(move |this, _, _, cx| this.navigate(&destination, cx)))
                    .child(title.clone()),
            );
        }
        sidebar.child(div().flex_1()).child(
            div()
                .p(px(10.0))
                .text_size(px(10.0))
                .text_color(rgb(MUTED))
                .child("Rust + GPUI / Metal")
                .child(
                    div()
                        .text_color(rgb(ORANGE))
                        .child("Captured data · not live"),
                ),
        )
    }

    fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let metadata = self
            .catalog
            .navigation
            .iter()
            .find(|entry| entry[0] == self.route);
        let mut header = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(16.0))
            .px(px(24.0))
            .py(px(16.0))
            .border_b_1()
            .border_color(rgb(GRID))
            .child(
                div()
                    .child(
                        div()
                            .text_size(px(23.0))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(
                                metadata
                                    .map_or("Flightdeck", |entry| entry[1].as_str())
                                    .to_owned(),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(12.0))
                            .text_color(rgb(MUTED))
                            .child(metadata.map_or("", |entry| entry[2].as_str()).to_owned()),
                    ),
            );
        let mut controls = div().flex().items_center().gap(px(6.0)).text_size(px(12.0));
        for (label, count) in [("15 min", 30), ("1 hour", 120)] {
            controls =
                controls.child(control(label).on_click(cx.listener(move |this, _, _, cx| {
                    this.inspection.lower = 120 - count;
                    this.inspection.selected = 120;
                    this.rebuild_rows();
                    cx.notify();
                })));
        }
        controls = controls
            .child(control("‹").on_click(cx.listener(|this, _, _, cx| {
                this.inspection.selected = this
                    .inspection
                    .selected
                    .saturating_sub(1)
                    .max(this.inspection.lower);
                cx.notify();
            })))
            .child(
                div()
                    .w(px(116.0))
                    .text_center()
                    .child(self.capture.time(self.inspection.selected)),
            )
            .child(control("›").on_click(cx.listener(|this, _, _, cx| {
                this.inspection.selected = (this.inspection.selected + 1).min(120);
                cx.notify();
            })));
        header = header.child(controls);
        header
    }

    fn row(&mut self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        assert!(index < self.rows.len());
        let content = match &self.rows[index] {
            Row::Summary => self.summary(cx).into_any_element(),
            Row::Heading(title) => div()
                .text_size(px(15.0))
                .py(px(8.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title.clone())
                .into_any_element(),
            Row::Panels(panels) => {
                let mut row = div().flex().gap(px(8.0)).w_full();
                for (column, panel) in panels.iter().enumerate() {
                    let mut cell = div().flex().min_w_0().flex_basis(px(0.0));
                    cell.style().flex_grow = Some(self.row_panel_weight(panels, column));
                    row = row.child(cell.child(self.panel(panel, cx)));
                }
                row.into_any_element()
            }
            Row::Computer => self.computer().into_any_element(),
            Row::AppPicker => self.app_picker(cx).into_any_element(),
            Row::Hardware => self.hardware(cx).into_any_element(),
        };
        div()
            .w_full()
            .px(px(24.0))
            .pb(px(8.0))
            .child(content)
            .into_any_element()
    }

    fn row_panel_weight(&self, panels: &[Rc<Panel>], column: usize) -> f32 {
        if self.route == "cpu" {
            if panels.first().is_some_and(|panel| panel.id == "cpu") {
                return [1.35, 0.85, 1.6][column];
            }
        }
        if panels.len() == 2 {
            if panels[1].kind == "ranking" {
                if column == 0 {
                    return 2.0;
                }
            }
        }
        1.0
    }

    fn panel(&self, panel: &Rc<Panel>, cx: &mut Context<Self>) -> AnyElement {
        let mut body = div()
            .flex_1()
            .min_w_0()
            .overflow_hidden()
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(GRID))
            .rounded(px(12.0))
            .px(px(12.0))
            .py(px(8.0));
        let help = panel
            .help
            .as_ref()
            .and_then(|key| self.catalog.help.get(key))
            .map_or_else(|| panel.title.clone(), |paragraphs| paragraphs.join("\n\n"));
        body = body.child(
            div()
                .flex()
                .justify_between()
                .items_center()
                .h(px(36.0))
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(panel.title.clone()),
                )
                .child(control("ⓘ").on_click(cx.listener(move |this, _, _, cx| {
                    this.help = Some(help.clone());
                    cx.notify();
                }))),
        );
        match panel.kind.as_str() {
            "quantities" => body = body.child(self.quantities(panel)),
            "ranking" => body = body.child(self.ranking(panel, cx)),
            "process-table" => body = body.child(self.process_table(cx)),
            "chart" | "self-chart" | "timeline" | "heatmap" | "core-bars" | "pressure" => {
                body = body.child(self.chart(panel, cx));
            }
            _ => body = body.child(Self::static_panel(panel)),
        }
        if let Some(caption) = &panel.caption {
            body = body.child(
                div()
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .mt(px(6.0))
                    .child(caption.clone()),
            );
        }
        body.into_any_element()
    }

    fn quantities(&self, panel: &Panel) -> impl IntoElement {
        let mut result = div().min_h(px(140.0));
        if let Some(quantities) = &panel.quantities {
            for quantity in quantities.iter().take(32) {
                if quantity.len() < 2 {
                    continue;
                }
                let label = &quantity[0];
                let key = &quantity[1];
                result = result.child(
                    div()
                        .flex()
                        .justify_between()
                        .gap(px(8.0))
                        .py(px(8.0))
                        .border_b_1()
                        .border_color(rgb(GRID))
                        .child(label.clone())
                        .child(self.capture.quantity(key, self.inspection.selected)),
                );
            }
        }
        result
    }

    fn ranking(&self, panel: &Panel, cx: &mut Context<Self>) -> impl IntoElement {
        let mut result = div().min_h(px(220.0));
        let mut rows: Vec<_> = panel
            .source
            .as_ref()
            .and_then(|key| self.capture.series.get(key))
            .into_iter()
            .flatten()
            .filter_map(|row| Some((row.app.as_ref()?, row.values[self.inspection.selected]?)))
            .collect();
        rows.sort_by(|left, right| right.1.total_cmp(&left.1));
        let scale = match panel.source.as_deref() {
            Some("appMemory") => 1.0 / 1_073_741_824.0,
            Some("appDisk" | "appNetwork") => 1e-6,
            _ => 1.0,
        };
        for (app, value) in rows.into_iter().take(8) {
            let selected_app = app.clone();
            result = result.child(
                div()
                    .id(SharedString::from(format!("{}-app-{app}", panel.id)))
                    .flex()
                    .justify_between()
                    .gap(px(8.0))
                    .py(px(7.0))
                    .cursor_pointer()
                    .focusable()
                    .focus(|style| style.bg(rgb(0x32_46_61)))
                    .hover(|style| style.bg(rgb(GRID)))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.app.clone_from(&selected_app);
                        this.process = None;
                        this.navigate("apps", cx);
                    }))
                    .child(div().overflow_hidden().child(app.clone()))
                    .child(
                        div()
                            .text_color(rgb(BLUE))
                            .child(reading(Some(value * scale), &panel.unit)),
                    ),
            );
        }
        result
    }

    fn app_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = div().flex().flex_wrap().gap(px(6.0)).py(px(8.0));
        for app in self.capture.process_apps.iter().take(32) {
            let app = app.clone();
            row = row.child(control(&app).on_click(cx.listener(move |this, _, _, cx| {
                this.app.clone_from(&app);
                this.process = None;
                this.rebuild_rows();
                cx.notify();
            })));
        }
        row.child(
            div()
                .w_full()
                .py(px(8.0))
                .text_color(rgb(MUTED))
                .child(format!("Inspecting {} · captured processes", self.app)),
        )
    }

    fn process_table(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut table = div().min_h(px(220.0)).text_size(px(12.0));
        if let Some(rows) = self.capture.series.get("proc1") {
            for row in rows
                .iter()
                .filter(|row| row.app.as_deref() == Some(&self.app))
                .take(32)
            {
                if let Some(process) = row.labels.get("process") {
                    let process_name = process.clone();
                    table = table.child(
                        div()
                            .id(SharedString::from(process.clone()))
                            .flex()
                            .justify_between()
                            .py(px(8.0))
                            .border_b_1()
                            .border_color(rgb(GRID))
                            .cursor_pointer()
                            .focusable()
                            .focus(|style| style.bg(rgb(0x32_46_61)))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.process = Some(process_name.clone());
                                cx.notify();
                            }))
                            .child(process.clone())
                            .child(reading(row.values[self.inspection.selected], "% CPU")),
                    );
                }
            }
        }
        table
    }

    fn static_panel(panel: &Panel) -> impl IntoElement {
        let mut content = div().min_h(px(140.0)).text_color(rgb(MUTED));
        if let Some(headings) = &panel.table_headings {
            content = content.child(headings.join("     "));
        }
        if let Some(rows) = &panel.table_rows {
            for row in rows.iter().take(32) {
                content = content.child(div().py(px(5.0)).child(row.join("     ")));
            }
        }
        content.child(panel.caption.clone().unwrap_or_else(|| {
            "This diagnostic presentation is outside this rendering prototype.".into()
        }))
    }

    fn render_main(&self, body: impl IntoElement, cx: &mut Context<Self>) -> AnyElement {
        let mut main = div()
            .relative()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(self.header(cx))
            .child(body);
        if let Some(help) = &self.help {
            main = main.child(
                div()
                    .absolute()
                    .top(px(80.0))
                    .right(px(30.0))
                    .w(px(480.0))
                    .p(px(24.0))
                    .bg(rgb(0x2c_2f_35))
                    .rounded(px(12.0))
                    .border_1()
                    .border_color(rgb(BLUE))
                    .child(help.clone())
                    .child(control("Close").on_click(cx.listener(|this, _, _, cx| {
                        this.help = None;
                        cx.notify();
                    }))),
            );
        }
        main.into_any_element()
    }

    fn render_chrome() -> impl IntoElement {
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
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child("Captured prototype v30 · Rust / GPUI"),
            )
    }

    fn render_smoke(&mut self, window: &mut Window) {
        if self.performance.frames == 1 {
            window.on_next_frame(|window, _| {
                eprintln!(
                    "first-frame viewport={:?} scale={}",
                    window.viewport_size(),
                    window.scale_factor()
                );
            });
        }
        if self.performance.options.smoke() {
            let index = self.performance.frames / 20 % self.catalog.navigation.len();
            let route = self.catalog.navigation[index][0].clone();
            if self.route != route {
                self.route = route;
                self.rebuild_rows();
                let name = self.route.clone();
                let count = self.rows.len();
                window.on_next_frame(move |_, _| {
                    eprintln!("smoke route={name} rows={count} frame-completed");
                });
            }
            if self.performance.frames % 20 == 10 {
                if !self.rows.is_empty() {
                    self.list.scroll_to_reveal_item(self.rows.len() - 1);
                }
            }
        }
    }

    fn computer(&self) -> impl IntoElement {
        let mut panel = div()
            .max_w(px(760.0))
            .mt(px(16.0))
            .p(px(20.0))
            .rounded(px(12.0))
            .bg(rgb(PANEL))
            .child(
                div()
                    .text_size(px(18.0))
                    .pb(px(12.0))
                    .child("MacBook Pro · captured hardware"),
            );
        for spec in &self.catalog.computer {
            panel = panel.child(
                div()
                    .flex()
                    .justify_between()
                    .py(px(12.0))
                    .border_b_1()
                    .border_color(rgb(GRID))
                    .child(div().text_color(rgb(MUTED)).child(spec.name.clone()))
                    .child(spec.value.clone()),
            );
        }
        panel
    }
}

impl Dashboard {
    pub(crate) fn replay_apply(
        &mut self,
        action: &crate::replay::Action,
    ) -> Result<serde_json::Value, String> {
        self.replay_validate(action)?;
        let mut rebuild = self.route != action.route;
        self.route.clone_from(&action.route);
        if let Some(minutes) = action.minutes {
            let lower = if minutes == 60 { 0 } else { 90 };
            rebuild |= self.inspection.lower != lower;
            self.inspection.lower = lower;
            self.inspection.selected = 120;
            self.performance.options.range_minutes = minutes;
        }
        if let Some(app) = &action.app {
            rebuild |= self.app != *app;
            self.app.clone_from(app);
            self.process = None;
        }
        if let Some(process) = &action.process {
            self.process = if process == "*" {
                None
            } else {
                Some(process.clone())
            };
        }
        if let Some(sample) = action.sample {
            self.inspection.selected = usize::from(sample);
        }
        if rebuild {
            self.rebuild_rows();
        }
        if let Some(scroll) = action.scroll {
            self.replay_scroll(scroll)?;
        }
        let mut effective = serde_json::json!({"route":self.route,
            "sample":self.inspection.selected,"lower":self.inspection.lower,
            "upper":self.inspection.upper,"app":self.app,
            "process":self.process.as_deref().unwrap_or("*")});
        if let Some(scroll) = action.scroll {
            effective["scroll_requested"] = serde_json::json!(match scroll {
                crate::replay::Scroll::Top => "top",
                crate::replay::Scroll::Hardware => "hardware",
            });
        }
        Ok(effective)
    }

    fn replay_validate(&self, action: &crate::replay::Action) -> Result<(), String> {
        if !self
            .catalog
            .navigation
            .iter()
            .take(11)
            .any(|entry| entry[0] == action.route)
        {
            return Err(format!("Replay route is not captured: {}", action.route));
        }
        let app = match &action.app {
            Some(app) => app.as_str(),
            None => self.app.as_str(),
        };
        if action.app.is_some() {
            let captured = self
                .capture
                .series
                .values()
                .take(128)
                .flat_map(|rows| rows.iter().take(2048))
                .any(|row| row.app.as_deref() == Some(app));
            if !captured {
                return Err(format!("Replay App is not captured: {app}"));
            }
        }
        if let Some(process) = &action.process {
            if process != "*" {
                let captured = self.capture.series.get("proc1").is_some_and(|rows| {
                    rows.iter().take(2048).any(|row| {
                        if row.app.as_deref() != Some(app) {
                            return false;
                        }
                        row.labels.get("process") == Some(process)
                    })
                });
                if !captured {
                    return Err(format!("Replay process is not captured: {process}"));
                }
            }
        }
        Ok(())
    }

    fn replay_scroll(&self, scroll: crate::replay::Scroll) -> Result<(), String> {
        let index = match scroll {
            crate::replay::Scroll::Top => 0,
            crate::replay::Scroll::Hardware => self
                .rows
                .iter()
                .take(128)
                .position(|row| matches!(row, Row::Hardware))
                .ok_or("Captured hardware row missing")?,
        };
        self.list.scroll_to(ListOffset {
            item_ix: index,
            offset_in_item: px(0.0),
        });
        Ok(())
    }
}

impl Render for Dashboard {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.hardware_state.begin_frame();
        self.performance.frame(window, cx);
        self.render_smoke(window);
        if self.performance.options.scrub {
            self.inspection.selected = self.inspection.lower
                + self.performance.frames % (self.inspection.upper - self.inspection.lower + 1);
        }
        let view = cx.entity();
        let body = list(self.list.clone(), move |index, _, cx| {
            view.update(cx, |this, cx| this.row(index, cx))
        })
        .flex_1()
        .w_full();
        let main = self.render_main(body, cx);
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(0x11_12_17))
            .text_color(rgb(TEXT))
            .font_family(".AppleSystemUIFont")
            .text_size(px(13.0))
            .on_key_down(keyboard_navigation)
            .child(Self::render_chrome())
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.navigation(cx))
                    .child(main),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, _, _, cx| {
                    this.help = None;
                    cx.notify();
                }),
            )
    }
}

fn flush(pending: &mut Vec<Rc<Panel>>, rows: &mut Vec<Row>) {
    if !pending.is_empty() {
        rows.push(Row::Panels(std::mem::take(pending)));
    }
}

pub fn control(label: &str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(label.to_owned()))
        .px(px(9.0))
        .py(px(7.0))
        .rounded(px(5.0))
        .bg(rgb(0x22_25_2b))
        .cursor_pointer()
        .focusable()
        .focus(|style| style.bg(rgb(0x32_46_61)))
        .hover(|style| style.bg(rgb(0x2c_2f_35)))
        .child(label.to_owned())
}

pub fn keyboard_navigation(event: &gpui::KeyDownEvent, window: &mut Window, cx: &mut gpui::App) {
    if event.keystroke.key != "tab" {
        return;
    }
    let modifiers = event.keystroke.modifiers;
    if modifiers.control {
        return;
    }
    if modifiers.alt {
        return;
    }
    if modifiers.platform {
        return;
    }
    if modifiers.function {
        return;
    }
    if modifiers.shift {
        window.focus_prev();
    } else {
        window.focus_next();
    }
    window.prevent_default();
    cx.stop_propagation();
}

pub fn keyboard_step(event: &gpui::KeyDownEvent) -> Option<i64> {
    if event.keystroke.modifiers.modified() {
        return None;
    }
    match event.keystroke.key.as_str() {
        "left" => Some(-1),
        "right" => Some(1),
        _ => None,
    }
}
