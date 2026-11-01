use std::cmp::Ordering;

use gpui::{AnyElement, Context, Focusable, IntoElement, SharedString, div, prelude::*, px, rgb};
use num_traits::ToPrimitive;
use serde_json::Value;

use crate::charts::{BLUE, GREEN, MUTED, ORANGE, PURPLE, YELLOW};
use crate::live_dashboard::{LiveDashboard, Picker, toggle};
use crate::live_source::{AppKey, PAGE_APPS_MAX};
use crate::live_views::chrome::select;
use crate::live_views::{summary, text};

/// The prototype's network cyan.
const CYAN: u32 = 0x00_ff_ff;
const SELECTED_APP_INDEX: usize = PAGE_APPS_MAX;
/// The most Apps the App picker lists, the first in its order of those its search names, so the
/// list stays short enough to read at a glance.
pub const APP_CHOICES_MAX: usize = 10;
const JSON_BYTES_MAX: usize = 32 * 1024;
/// The width of each of `AppColumn::ALL`, in points: the App's name as wide as the process
/// group table's, then the rest, each wide enough for its heading and the arrow that shows the
/// Apps are sorted by it.
const APP_TABLE_WIDTHS: [f32; AppColumn::ALL.len()] = [
    crate::live_panel_table::NAME_WIDTH,
    92.0,
    80.0,
    92.0,
    98.0,
    80.0,
    86.0,
    92.0,
];
/// The Disk I/O column's position among an App's reading columns, those after its name and
/// its member count, as `reading_selector` counts them.
const DISK_READING_INDEX: usize = 5;
const _: () = {
    assert!(JSON_BYTES_MAX > 2 * 4095);
    assert!(matches!(
        AppColumn::ALL[DISK_READING_INDEX + 2],
        AppColumn::Disk
    ));
};

impl LiveDashboard {
    pub fn app_selector(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut selector = div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(self.app_selector_controls(cx));
        if self.app_paged() {
            selector = selector.child(self.app_paging(cx));
        }
        selector
    }

    fn app_selector_controls(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let label = match self.current_app() {
            Some(app) => app_name(app),
            None => {
                if self.request.app.is_some() {
                    "Selected App is not on this page".into()
                } else {
                    "Select an App".into()
                }
            }
        };
        let context = self.app_selector_context();
        let selector = format!("app-context:{context}");

        div()
            .flex()
            .items_center()
            .h(px(44.0))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .w(px(340.0))
                    .flex_shrink_0()
                    .text_size(px(13.0))
                    .child("App")
                    .child(
                        select(&label, 300.0)
                            .debug_selector(|| "app-select".to_owned())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.picker = toggle(this.picker, Picker::App);
                                // A person opens the picker to find an App, so its search
                                // starts empty, over the first ten, and takes their keystrokes
                                // at once.
                                if this.picker == Some(Picker::App) {
                                    let Some(search) = &this.app_list.search else {
                                        unreachable!("create_dashboard_with sets the App search");
                                    };
                                    search.update(cx, |search, cx| search.clear(window, cx));
                                    window.focus(&search.focus_handle(cx), cx);
                                }
                                this.rebuild();
                                cx.notify();
                            })),
                    ),
            )
            .child(
                div()
                    .ml(px(12.0))
                    .min_w_0()
                    .truncate()
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .debug_selector(move || selector.clone())
                    .child(context),
            )
    }

    /// The prototype's line beside the App picker: how many process groups the selected
    /// second shows, and that second.
    fn app_selector_context(&self) -> String {
        let Some(snapshot) = self.reading_snapshot() else {
            return "Waiting for readings".into();
        };
        let Some(second) = snapshot.selection["second"].as_u64() else {
            return "Waiting for readings".into();
        };

        let groups = self.reading_executables().len();
        if groups == 0 {
            return "App history retained; process groups not captured at this instant.".into();
        }
        format!(
            "{groups} process groups at {}",
            crate::live_presentation::clock(second, true)
        )
    }

    /// The prototype's control under the process group table, whose note says how processes
    /// group and how much of the App this second's readings cover.
    pub fn process_grouping_about(&self) -> impl IntoElement {
        let help = self.process_grouping_help();
        let label = "About process grouping and snapshot coverage";

        div()
            .id("process-grouping-about")
            .debug_selector(|| label.into())
            .role(gpui::Role::Button)
            .aria_label(label)
            .focusable()
            .tab_stop(true)
            .flex()
            .items_center()
            .h(px(44.0))
            .px(px(8.0))
            .text_size(px(11.0))
            .text_color(rgb(MUTED))
            .cursor_pointer()
            .child(label)
            .tooltip(move |_, cx| {
                cx.new(|_| crate::live_chart_view::ChartHelp(help.clone()))
                    .into()
            })
    }

    fn process_grouping_help(&self) -> String {
        let mut parts = Vec::with_capacity(3);
        parts.push(
            "Process groups gather the selected App's processes by executable name, so a row \
            may stand for several processes and is not a PID. * marks a reading some of the \
            group's processes lack."
                .to_owned(),
        );
        let snapshot = self.reading_snapshot();
        if let Some(app) = self.current_app() {
            let loaded = snapshot
                .and_then(|snapshot| snapshot.detail.as_deref())
                .and_then(|detail| detail["processes"].as_array())
                .map_or(0, Vec::len);
            parts.push(format!(
                "At this second the App has {} live of {} members, and its membership is {}. \
                The table covers the {loaded} processes loaded.",
                text(&app["live_members"]),
                text(&app["members"]),
                if app["membership_complete"].as_bool() == Some(true) {
                    "complete"
                } else {
                    "uncertain"
                },
            ));
        }
        if let Some(page) = snapshot.and_then(|snapshot| snapshot.apps.as_deref()) {
            if page["ungrouped_members"]
                .as_u64()
                .is_some_and(|count| count > 0)
            {
                parts.push(format!(
                    "{} processes have unavailable App identity.",
                    text(&page["ungrouped_members"])
                ));
            }
        }
        parts.join(" ")
    }

    pub fn app_choice_indices(&self) -> Vec<usize> {
        let Some(snapshot) = self.reading_snapshot() else {
            return Vec::new();
        };

        let apps = snapshot
            .apps
            .as_ref()
            .and_then(|page| page["apps"].as_array());
        let mut choices: Vec<&Value> = Vec::with_capacity(PAGE_APPS_MAX + 1);
        if let Some(apps) = apps {
            choices.extend(apps.iter().take(PAGE_APPS_MAX));
        }
        let listed_count = choices.len();
        if let Some(app) = self.current_app() {
            if !choices.iter().any(|choice| choice["key"] == app["key"]) {
                choices.push(app);
            }
        }
        assert!(choices.len() <= PAGE_APPS_MAX + 1);

        app_order(&choices, self.app_list.order, &self.app_list.search_text)
            .into_iter()
            .take(APP_CHOICES_MAX)
            .map(|position| {
                if position < listed_count {
                    position
                } else {
                    SELECTED_APP_INDEX
                }
            })
            .collect()
    }

    /// The picker's row of the App at `index`, which closes the table's panel when `last`.
    pub fn app_row(&self, index: usize, last: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some(app) = self.app_at(index) else {
            return unavailable("App row unavailable");
        };
        let key = match app_key(app) {
            Ok(key) => key,
            Err(error) => return unavailable(&error),
        };
        let selected = self.request.app.as_ref() == Some(&key);
        let texts = AppColumn::ALL.map(|column| column.cell(app));
        let disk_selector = format!(
            "app-disk:{}:{}:{}",
            key.kind,
            key.bytes_hex,
            texts[DISK_READING_INDEX + 2]
        );

        crate::live_panel_table::row(format!("app-choice-{index}"), last, &self.app_list.scroll)
            .accessibility_id(format!("flightdeck.app.{}.{}", key.kind, key.bytes_hex))
            .role(gpui::Role::Button)
            .aria_label(format!(
                "{}; {} members; CPU {}",
                text(&app["display_name"]),
                text(&app["members"]),
                value_rate(&app["cpu_percent_one_core"], "%")
            ))
            .aria_selected(selected)
            .on_click(cx.listener(move |this, _, _, cx| {
                this.request.app = Some(key.clone());
                this.request.executable = None;
                this.request.members_offset = 0;
                this.picker = None;
                this.expanded = None;
                this.submit();
                this.rebuild();
                cx.notify();
            }))
            .child(crate::live_panel_table::cells(
                texts,
                &APP_TABLE_WIDTHS,
                reading_selector(DISK_READING_INDEX, &disk_selector),
                crate::live_panel_table::NameCell::Button { pressed: selected },
            ))
            .into_any_element()
    }

    pub(crate) fn current_app(&self) -> Option<&Value> {
        let snapshot = self.reading_snapshot()?;
        Some(&selected_detail(snapshot.detail.as_deref(), self.request.app.as_ref())?["app"])
    }

    /// The App picker's search field over its headings, each heading a button that sorts the
    /// Apps by its column, the heading of the column they are sorted by carrying an arrow. The
    /// headings close the table's panel when `last`.
    pub fn app_list_columns(&self, last: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let mut row = crate::live_panel_table::cells_row(&APP_TABLE_WIDTHS);
        for (index, (column, width)) in AppColumn::ALL.into_iter().zip(APP_TABLE_WIDTHS).enumerate()
        {
            let selector = format!("app-column:apps:{}", column.heading());
            let cell = crate::live_panel_table::column(index, width)
                .debug_selector(move || selector.clone());
            row = row.child(self.app_list_columns_button(cell, column, cx));
        }

        div()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(44.0))
                    .text_size(px(11.0))
                    .children(self.app_list.search.clone()),
            )
            .child(
                crate::live_panel_table::header("app-columns-apps", last, &self.app_list.scroll)
                    .child(row),
            )
    }

    /// Lists only the Apps the search field's new text names, once the text has changed, since
    /// the field also tells of a moved cursor or selection.
    pub(crate) fn app_search_changed(
        &mut self,
        search: &gpui::Entity<crate::native_input::TextInput>,
        cx: &mut Context<Self>,
    ) {
        let text = &search.read(cx).text.content;
        if *text == self.app_list.search_text {
            return;
        }

        self.app_list.search_text.clone_from(text);
        self.rebuild();
        cx.notify();
    }

    fn app_list_columns_button(
        &self,
        cell: gpui::Div,
        column: AppColumn,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let heading = column.heading();
        let label = if self.app_list.order.column != column {
            heading.to_owned()
        } else if self.app_list.order.descending {
            format!("{heading} \u{2193}")
        } else {
            format!("{heading} \u{2191}")
        };

        cell.id(SharedString::from(format!("app-heading-{heading}")))
            .role(gpui::Role::Button)
            .aria_label(format!("Sort the Apps by {heading}"))
            .focusable()
            .tab_stop(true)
            .focus(crate::charts::focus_ring)
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.app_list.order = this.app_list.order.clicked(column);
                this.rebuild();
                cx.notify();
            }))
            .child(label)
    }

    fn app_at(&self, index: usize) -> Option<&Value> {
        if index == SELECTED_APP_INDEX {
            return self.current_app();
        }
        self.reading_snapshot()?.apps.as_ref()?["apps"].get(index)
    }
}

/// An App's name as the prototype shows it, without the bundle's `.app` extension.
pub(crate) fn app_name(app: &Value) -> String {
    let name = text(&app["display_name"]);
    match name.strip_suffix(".app") {
        Some(stem) => stem.to_owned(),
        None => name,
    }
}

pub fn selected_detail<'a>(
    detail: Option<&'a Value>,
    requested: Option<&AppKey>,
) -> Option<&'a Value> {
    let detail = detail?;
    let app = &detail["app"];
    if app.is_null() {
        return None;
    }
    if let Some(key) = requested {
        if app["key"]["kind"].as_str() != Some(key.kind.as_str()) {
            return None;
        }
        if app["key"]["bytes_hex"].as_str() != Some(key.bytes_hex.as_str()) {
            return None;
        }
    }
    Some(detail)
}

/// The prototype's six readings of the selected App at the selected second. Network has no
/// per-App source.
pub(crate) fn app_cards(app: Option<&Value>) -> Vec<summary::Card> {
    let reading = |read: fn(&Value) -> String| app.map_or_else(|| "Unavailable".into(), read);
    let card = |label: &str, reading, note: &str, color| summary::Card {
        label: label.into(),
        reading,
        note: note.into(),
        color,
    };
    vec![
        card(
            "CPU",
            reading(|app| value_rate(&app["cpu_percent_one_core"], "%")),
            "100% = one core",
            BLUE,
        ),
        card(
            "Resident memory",
            reading(|app| measure_memory(&app["resident_bytes"])),
            "Resident; includes shared pages",
            GREEN,
        ),
        card("Network", "Unavailable".into(), "Selected app", CYAN),
        card(
            "Disk I/O",
            reading(measure_disk_rate),
            "Selected app",
            YELLOW,
        ),
        card(
            "GPU time",
            reading(|app| value_rate(&app["gpu_percent"], "%")),
            "Selected app",
            PURPLE,
        ),
        card(
            "Process power",
            reading(|app| value_quantity(&app["energy_watts"], "W")),
            "Selected app",
            ORANGE,
        ),
    ]
}

/// The selector of a reading's cell: its own text, the count's prefixed so a PID reads apart
/// from a reading, or `disk` for the Disk I/O column, so a test can find that column's reading
/// in any row.
pub fn reading_selector(disk_index: usize, disk: &str) -> impl Fn(usize, &str) -> String {
    move |index, text| {
        if index == 1 {
            return format!("count:{text}");
        }
        if index == disk_index + 2 {
            return disk.to_owned();
        }
        text.to_owned()
    }
}

#[cfg(test)]
#[path = "live_app_hover_input_tests.rs"]
mod hover_input_tests;

#[cfg(test)]
#[path = "live_app_order_tests.rs"]
mod order_tests;

pub fn memory(value: Option<u64>) -> String {
    let Some(value) = value else {
        return "Unavailable".into();
    };

    match value
        .to_f64()
        .and_then(|value| (value / 1_073_741_824.0).to_f32())
    {
        Some(value) => crate::live_projection::quantity_text(value, "GiB"),
        None => "Unavailable".into(),
    }
}

fn app_key(app: &Value) -> Result<AppKey, String> {
    crate::live_source::app_key_from_json(app)
}

fn value_rate(value: &Value, unit: &str) -> String {
    if value.is_null() {
        return "Unavailable".into();
    }
    match value.as_f64() {
        Some(value) => event_rate(Some(value), unit),
        None => "Invalid reading".into(),
    }
}

/// A reading in the unit the charts scale it to, so the App's power reads in milliwatts below
/// one watt, as its chart legend does.
fn value_quantity(value: &Value, unit: &str) -> String {
    if value.is_null() {
        return "Unavailable".into();
    }
    match value.as_f64() {
        Some(value) => event_rate(Some(value), unit),
        None => "Invalid reading".into(),
    }
}

pub fn event_rate(value: Option<f64>, unit: &str) -> String {
    value.and_then(|value| value.to_f32()).map_or_else(
        || "Unavailable".into(),
        |value| crate::live_projection::quantity_text(value, unit),
    )
}

pub fn disk_rate(value: Option<f64>) -> String {
    let Some(value) = value else {
        return "Unavailable".into();
    };

    if !value.is_finite() {
        return "Unavailable".into();
    }

    if value < 0.0 {
        return "Invalid reading".into();
    }

    match (value / 1_000_000.0).to_f32() {
        Some(value) => crate::live_projection::quantity_text(value, "MB/s"),
        None => "Unavailable".into(),
    }
}

fn value_disk_rate(value: &Value) -> String {
    if value.is_null() {
        return "Unavailable".into();
    }

    match value.as_f64() {
        Some(value) => disk_rate(Some(value)),
        None => "Invalid reading".into(),
    }
}

fn measure_disk_rate(app: &Value) -> String {
    let measure = &app["disk_io_bytes"];
    let reading = match measure_value(measure) {
        Ok(Some(_)) => value_disk_rate(&app["disk_io_bps"]),
        Ok(None) => "Unavailable".into(),
        Err(error) => return error,
    };
    let Some(unavailable) = measure["unavailable_members"].as_u64() else {
        return "Missing unavailable coverage".into();
    };

    if unavailable > 0 {
        return format!("{reading} *");
    }

    reading
}

/// A column of the Apps list, by which the list can be sorted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppColumn {
    Name,
    Members,
    Cpu,
    Resident,
    Footprint,
    Power,
    Energy,
    Disk,
}

impl AppColumn {
    /// The App table's columns, in the order of their cells.
    const ALL: [Self; 8] = [
        Self::Name,
        Self::Members,
        Self::Cpu,
        Self::Resident,
        Self::Footprint,
        Self::Power,
        Self::Energy,
        Self::Disk,
    ];

    fn heading(self) -> &'static str {
        match self {
            Self::Name => "App",
            Self::Members => "Members",
            Self::Cpu => "CPU %",
            Self::Resident => "Resident",
            Self::Footprint => "Footprint",
            Self::Power => "Power",
            Self::Energy => "Energy",
            Self::Disk => "Disk I/O",
        }
    }

    /// The text of `app`'s cell in the column.
    fn cell(self, app: &Value) -> String {
        match self {
            Self::Name => app_name(app),
            Self::Members => text(&app["members"]),
            Self::Cpu => value_rate(&app["cpu_percent_one_core"], "%"),
            Self::Resident => measure_memory(&app["resident_bytes"]),
            Self::Footprint => measure_memory(&app["footprint_bytes"]),
            Self::Power => value_rate(&app["energy_watts"], "W"),
            Self::Energy => value_rate(&app["energy_joules"], "J"),
            Self::Disk => measure_disk_rate(app),
        }
    }

    /// The reading of `app` its cell in the column shows, or None where the cell shows none.
    fn reading(self, app: &Value) -> Option<f64> {
        match self {
            Self::Name => None,
            Self::Members => app["members"].as_f64(),
            Self::Cpu => app["cpu_percent_one_core"].as_f64(),
            Self::Resident => app_order_measure(&app["resident_bytes"]),
            Self::Footprint => app_order_measure(&app["footprint_bytes"]),
            Self::Power => app["energy_watts"].as_f64(),
            Self::Energy => app["energy_joules"].as_f64(),
            Self::Disk => match app_order_measure(&app["disk_io_bytes"]) {
                Some(_) => app["disk_io_bps"].as_f64(),
                None => None,
            },
        }
    }
}

/// The App picker's search field, which keeps only the Apps whose name contains its text.
pub const APP_SEARCH_FIELD: crate::native_input::Field = crate::native_input::Field {
    id: "app-search",
    accessibility_id: "flightdeck.app-search",
    label: "Search Apps by name",
    placeholder: "Part of an App's name",
    hint: "Search Apps",
    width: 240.0,
};

/// The App picker's order and search, which its headings and search field set.
#[derive(Default)]
pub struct AppList {
    /// The order a click on a heading sets.
    pub order: AppOrder,
    /// The field a person types part of an App's name into, to list only the Apps it names.
    pub search: Option<gpui::Entity<crate::native_input::TextInput>>,
    /// The search the list was last built for, the field's text since its last change, which
    /// the list's rows are built from without access to the field.
    pub search_text: String,
    /// How far the list's heading and rows are scrolled sideways. The process groups' table
    /// keeps its own, since a narrower table sharing it would hold the wider one back.
    pub scroll: gpui::ScrollHandle,
}

/// The order the Apps list shows its Apps in: by a column, the least or the most first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppOrder {
    pub column: AppColumn,
    pub descending: bool,
}

impl Default for AppOrder {
    fn default() -> Self {
        Self {
            column: AppColumn::Cpu,
            descending: true,
        }
    }
}

impl AppOrder {
    /// The order after a click on `column`'s heading. The column sorted by turns around, and
    /// another column starts with names from A and readings from the most, which is what a
    /// person looking for the busiest App wants first.
    pub fn clicked(self, column: AppColumn) -> Self {
        if column == self.column {
            return Self {
                column,
                descending: !self.descending,
            };
        }

        Self {
            column,
            descending: column != AppColumn::Name,
        }
    }
}

/// The positions in `apps` of the Apps whose name, as the list shows it without ".app",
/// contains `search`, in `order`. Names compare in lowercase. An App with no reading in the
/// column sorts last either way, and Apps that tie sort by name, then by position.
pub fn app_order(apps: &[&Value], order: AppOrder, search: &str) -> Vec<usize> {
    assert!(apps.len() <= PAGE_APPS_MAX + 1);

    let search = search.to_lowercase();
    let mut keys = Vec::with_capacity(apps.len());
    for (position, app) in apps.iter().enumerate() {
        let name = app_name(app).to_lowercase();
        if name.contains(&search) {
            keys.push(AppOrderKey {
                reading: order.column.reading(app),
                name,
                position,
            });
        }
    }
    keys.sort_by(|first, second| app_order_compare(first, second, order));

    keys.into_iter().map(|key| key.position).collect()
}

struct AppOrderKey {
    reading: Option<f64>,
    name: String,
    position: usize,
}

/// A measured sum's known value, or None where no member was measured, the sum overflowed or
/// the sum is malformed, each of which its cell already shows.
fn app_order_measure(value: &Value) -> Option<f64> {
    match measure_value(value) {
        Ok(Some(known)) => known.to_f64(),
        Ok(None) | Err(_) => None,
    }
}

fn app_order_compare(first: &AppOrderKey, second: &AppOrderKey, order: AppOrder) -> Ordering {
    let by_reading = match (first.reading, second.reading) {
        (Some(first), Some(second)) => app_order_directed(first.total_cmp(&second), order),
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    };
    let by_name = if order.column == AppColumn::Name {
        app_order_directed(first.name.cmp(&second.name), order)
    } else {
        first.name.cmp(&second.name)
    };

    by_reading
        .then(by_name)
        .then(first.position.cmp(&second.position))
}

fn app_order_directed(ordering: Ordering, order: AppOrder) -> Ordering {
    if order.descending {
        ordering.reverse()
    } else {
        ordering
    }
}

fn measure_memory(value: &Value) -> String {
    let reading = match measure_value(value) {
        Ok(value) => memory(value),
        Err(error) => error,
    };
    if value["unavailable_members"]
        .as_u64()
        .is_some_and(|count| count > 0)
    {
        format!("{reading} *")
    } else {
        reading
    }
}

fn measure_value(value: &Value) -> Result<Option<u64>, String> {
    let measured = value["measured_members"]
        .as_u64()
        .ok_or("Missing measured coverage")?;
    let overflowed = value["overflowed"]
        .as_bool()
        .ok_or("Missing overflow state")?;
    if overflowed {
        return Ok(None);
    }
    if measured == 0 {
        return Ok(None);
    }
    decimal(&value["known"])
}

fn decimal(value: &Value) -> Result<Option<u64>, String> {
    match value {
        Value::Null => Ok(None),
        Value::String(text) => text
            .parse::<u64>()
            .map(Some)
            .map_err(|error| error.to_string()),
        _ => Err("Invalid exact counter".into()),
    }
}

fn unavailable(message: &str) -> AnyElement {
    div()
        .text_size(px(11.0))
        .text_color(rgb(MUTED))
        .child(message.to_owned())
        .into_any_element()
}

#[cfg(test)]
mod selection_tests {
    use super::*;
    use serde_json::json;

    crate::live_source::app_key_tests::captured_app_key_tests!(app_key);

    #[test]
    fn changing_apps_hides_previous_details_until_the_requested_app_arrives() {
        // A pending or failed B read must never expose retained A processes and histories as B.
        let previous = json!({"app":{"key":{"kind":"bundle","bytes_hex":"41"}},
            "processes":[{"pid":123}]});
        let requested = AppKey {
            kind: "bundle".into(),
            bytes_hex: "42".into(),
        };
        assert!(selected_detail(Some(&previous), Some(&requested)).is_none());
        assert!(selected_detail(None, Some(&requested)).is_none());

        let replacement = json!({"app":{"key":{"kind":"bundle","bytes_hex":"42"}},
            "processes":[{"pid":456}]});
        let detail = selected_detail(Some(&replacement), Some(&requested)).unwrap();
        assert_eq!(detail["processes"][0]["pid"], 456);
    }

    #[test]
    fn app_identity_includes_the_key_kind() {
        // Equal raw bytes from a different identity domain are a different App.
        let detail = json!({"app":{"key":{"kind":"bundle","bytes_hex":"41"}}});
        let requested = AppKey {
            kind: "executable".into(),
            bytes_hex: "41".into(),
        };
        assert!(selected_detail(Some(&detail), Some(&requested)).is_none());
    }
}
