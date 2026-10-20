use gpui::{
    AnyElement, Context, Div, IntoElement, MouseButton, PathBuilder, Pixels, Point, canvas,
    deferred, div, point, prelude::*, px, rgb,
};
use std::rc::Rc;

use crate::charts::{self, BLUE, GREEN, MUTED, PANEL, TEXT};
use crate::live_dashboard::{LiveDashboard, NAVIGATION, ROUTES};
use crate::live_source_mode::SourceMode;

/// The window bar's height, which also places the macOS window buttons inside it.
pub const BRAND_HEIGHT: f32 = 48.0;
const SIDEBAR_WIDTH: f32 = 164.0;
const SELECTED: u32 = 0x0057_94f2;
const RAISED: u32 = 0x002c_2f35;
const ICON_SIZE: f32 = 16.0;

/// The prototype's subtitle under each page title.
#[must_use]
pub fn route_subtitle(route: &str) -> &'static str {
    match route {
        "overview" => "System behavior, resource pressure and the apps behind it.",
        "cpu" => "Activity by cluster and core, frequency and scheduling demand.",
        "apps" => "Follow an app across CPU, memory, I/O, GPU time and energy.",
        "gpu" => "Busy time, frequency, memory and measured component power.",
        "memory" => "Separate memory quantity, pressure, compression and paging.",
        "energy" => "Whole-machine power, process energy, cooling and thermal state.",
        "storage" => "Throughput, operations and reported mean I/O time.",
        "network" => "Machine traffic and the applications contributing to it.",
        "battery" => "Charge, power flow, health and cycle count.",
        "computer" => "Hardware and system information for this Mac.",
        "self" => "Recording integrity, collection cost and Flightdeck pipeline diagnostics.",
        _ => "",
    }
}

/// The note at the right of the window bar, which names where the readings come from. The
/// prototype's wording describes a captured file, so it is not used for a live collector.
fn chrome_note(route: &str, source: SourceMode) -> &'static str {
    match (route, source) {
        ("computer", _) => "Live system information",
        ("self", SourceMode::Live) => "Live Flightdeck diagnostics",
        ("self", SourceMode::Recorded) => "Saved Flightdeck diagnostics",
        (_, SourceMode::Live) => "Live data from the collector",
        (_, SourceMode::Recorded) => "Saved recording",
    }
}

/// What the source pill reads. It tells the state of the readings, as `Live` or `Paused`, and
/// the Self-monitoring page names the run it shows where the reader is connected.
fn pill_label(route: &str, status: &str) -> String {
    if route == "self" && status == "Reader connected" {
        return "Flightdeck diagnostic run".to_owned();
    }
    status.to_owned()
}

impl LiveDashboard {
    /// The window bar, which holds the macOS window buttons and moves the window when dragged.
    pub fn brandbar(&self) -> impl IntoElement {
        let source = chrome_note(&self.request.route, self.native.source);

        div()
            .id("window-bar")
            .h(px(BRAND_HEIGHT))
            .flex_shrink_0()
            .pl(px(81.0))
            .pr(px(18.0))
            .flex()
            .items_center()
            .justify_between()
            .border_b_1()
            .border_color(charts::alpha(TEXT, 0.12))
            .on_mouse_down(MouseButton::Left, |event, window, _| {
                if event.click_count == 2 {
                    window.titlebar_double_click();
                } else {
                    window.start_window_move();
                }
            })
            .child(
                div()
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Flightdeck"),
            )
            .child(
                div()
                    .debug_selector(|| format!("source-sidebar:{source}"))
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child(source),
            )
    }

    pub fn navigation(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut sidebar = div()
            .flex()
            .flex_col()
            .w(px(SIDEBAR_WIDTH))
            .flex_shrink_0()
            .h_full()
            .px(px(12.0))
            .py(px(20.0))
            .gap(px(3.0))
            .border_r_1()
            .border_color(charts::alpha(TEXT, 0.12));
        for (index, group) in NAVIGATION.iter().enumerate() {
            if index > 0 {
                sidebar = sidebar.child(div().h(px(1.0)).m(px(12.0)).bg(charts::alpha(TEXT, 0.12)));
            }
            for route in *group {
                sidebar = sidebar.child(self.navigation_item(route, cx));
            }
        }
        sidebar
    }

    fn navigation_item(&self, route: &'static str, cx: &mut Context<Self>) -> impl IntoElement {
        let title = route_title(route);
        let selected = self.request.route == route;

        div()
            .id(route)
            .debug_selector(move || format!("navigation:{route}"))
            .accessibility_id(format!("flightdeck.navigation.{route}"))
            .role(gpui::Role::Button)
            .aria_label(title)
            .aria_selected(selected)
            .flex()
            .items_center()
            .min_h(px(44.0))
            .px(px(12.0))
            .rounded(px(5.0))
            .cursor_pointer()
            .focusable()
            .tab_stop(true)
            .focus(|style| style.bg(rgb(0x0032_4661)))
            .when(selected, |item| {
                item.bg(charts::alpha(SELECTED, 0.15)).text_color(rgb(BLUE))
            })
            .when(!selected, |item| item.hover(|style| style.bg(rgb(RAISED))))
            .on_click(cx.listener(move |this, _, _, cx| this.navigate(route, cx)))
            .child(title)
    }

    pub fn header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let route = self.request.route.as_str();
        let header = div().px(px(24.0)).pt(px(24.0)).pb(px(8.0)).child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(12.0))
                .justify_between()
                .items_start()
                .child(
                    div()
                        .min_w_0()
                        .max_w_full()
                        .child(
                            div()
                                .debug_selector(|| "reading-title".into())
                                .text_size(px(23.0))
                                .line_height(px(30.0))
                                .font_weight(gpui::FontWeight::SEMIBOLD)
                                .child(route_title(route)),
                        )
                        .child(
                            div()
                                .mt(px(3.0))
                                .text_color(rgb(MUTED))
                                .child(route_subtitle(route)),
                        ),
                )
                .child(self.header_controls(cx)),
        );
        self.header_geometry(header)
    }

    fn header_geometry(&self, header: Div) -> impl IntoElement {
        let height = self
            .preview
            .as_ref()
            .and_then(|preview| preview.header_height);
        let measured = Rc::clone(&self.header_height);
        header
            .id("reading-header")
            .relative()
            .when_some(height, |header, height| {
                header.h(height).overflow_y_scroll()
            })
            .child(
                canvas(
                    move |bounds, _, _| measured.set(bounds.size.height),
                    |_, (), _, _| {},
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
    }

    fn header_controls(&self, cx: &mut Context<Self>) -> Div {
        let controls = div()
            .debug_selector(|| "reading-controls".into())
            .relative()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(8.0))
            .min_h(px(44.0))
            .min_w_0()
            .max_w_full()
            .child(self.source_pill());

        // Computer and Self-monitoring show no recorded second, so the prototype gives them
        // no time selection.
        if matches!(self.request.route.as_str(), "computer" | "self") {
            return controls;
        }
        controls.child(self.time_controls(self.latest_button(cx).into_any_element(), cx))
    }

    /// Returns to the latest reading, and is disabled while the charts already follow it.
    fn latest_button(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let paused = self.request.second.is_some();
        let label = self.native.source.following_label(true);
        icon_button("Go to latest reading", Icon::Latest, !paused).when(paused, |button| {
            button
                .debug_selector(move || format!("source-follow:{label}"))
                .on_click(cx.listener(|this, _, _, cx| this.pause(cx)))
        })
    }

    fn source_pill(&self) -> impl IntoElement {
        let status = self.status();
        let following = self.request.second.is_none() && self.snapshot.is_some();

        div()
            .debug_selector(|| format!("source-status:{status}"))
            .flex()
            .items_center()
            .gap(px(8.0))
            .px(px(8.0))
            .text_size(px(11.0))
            .text_color(rgb(MUTED))
            .child(div().size(px(6.0)).rounded_full().bg(rgb(if following {
                GREEN
            } else {
                MUTED
            })))
            .child(pill_label(&self.request.route, &status))
    }

    /// The prototype's range picker and go-to-latest button. The picker opens a menu that also
    /// holds the native inspection and recording controls the prototype has no room for.
    fn time_controls(&self, trailing: AnyElement, cx: &mut Context<Self>) -> impl IntoElement {
        let picker = div()
            .id("time-range")
            .debug_selector(|| "time-range".into())
            .role(gpui::Role::Button)
            .aria_label("Time range and inspection")
            .aria_expanded(self.native.time_menu_open)
            .flex()
            .items_center()
            .gap(px(8.0))
            .min_h(px(44.0))
            .px(px(12.0))
            .rounded(px(5.0))
            .bg(rgb(PANEL))
            .border_1()
            .border_color(charts::alpha(TEXT, 0.12))
            .cursor_pointer()
            .focusable()
            .tab_stop(true)
            .hover(|style| style.bg(rgb(RAISED)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.native.time_menu_open = !this.native.time_menu_open;
                cx.notify();
            }))
            .child(icon(Icon::Calendar))
            .child("Last 15 minutes")
            .child(icon(Icon::Chevron));

        div()
            .relative()
            .flex()
            .items_center()
            .gap(px(4.0))
            .child(picker)
            .child(trailing)
            .children(
                self.native
                    .time_menu_open
                    .then(|| self.time_menu(cx).into_any_element()),
            )
    }

    fn time_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let menu = div()
            .id("time-menu")
            .debug_selector(|| "time-menu".into())
            .occlude()
            .w(px(320.0))
            .p(px(12.0))
            .flex()
            .flex_col()
            .gap(px(8.0))
            .rounded(px(7.0))
            .bg(rgb(PANEL))
            .border_1()
            .border_color(charts::alpha(TEXT, 0.12))
            .shadow_lg()
            .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                this.native.time_menu_open = false;
                cx.notify();
            }))
            .child(menu_heading("Quick ranges"))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.0))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_between()
                            .min_h(px(44.0))
                            .p(px(8.0))
                            .rounded(px(5.0))
                            .bg(charts::alpha(SELECTED, 0.15))
                            .text_color(rgb(BLUE))
                            .child("Last 15 minutes")
                            .child("✓"),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .min_h(px(44.0))
                            .p(px(8.0))
                            .opacity(0.4)
                            .child("Last 1 hour"),
                    ),
            )
            .child(self.time_menu_range())
            .child(
                div()
                    .px(px(8.0))
                    .py(px(4.0))
                    .text_size(px(11.0))
                    .line_height(px(16.5))
                    .text_color(rgb(MUTED))
                    .child(
                        "Ranges end at the latest captured sample. Hover a chart or use its \
                         arrow keys to inspect.",
                    ),
            );

        deferred(div().absolute().top(px(52.0)).right(px(48.0)).child(menu)).with_priority(1)
    }

    fn time_menu_range(&self) -> impl IntoElement {
        let second = self
            .reading_snapshot()
            .and_then(|snapshot| snapshot.selection["second"].as_u64());
        let entry = |label: &'static str, value: String| {
            div()
                .flex()
                .justify_between()
                .gap(px(12.0))
                .child(div().text_color(rgb(MUTED)).child(label))
                .child(value)
        };
        let clock = |second: Option<u64>| {
            second.map_or_else(
                || "Unavailable".into(),
                |second| crate::chart_axes::clock(second, true),
            )
        };
        let date = second.map_or_else(|| "Unavailable".into(), crate::chart_axes::date_with_zone);

        div()
            .flex()
            .flex_col()
            .gap(px(6.0))
            .px(px(8.0))
            .mt(px(4.0))
            .pt(px(12.0))
            .border_t_1()
            .border_color(charts::alpha(TEXT, 0.12))
            .text_size(px(12.0))
            .line_height(px(18.0))
            .child(entry(
                "From",
                clock(second.map(|second| second.saturating_sub(899))),
            ))
            .child(entry("To", clock(second)))
            .child(entry("Date", date))
    }
}

fn route_title(route: &str) -> &'static str {
    ROUTES
        .iter()
        .find(|(candidate, _)| *candidate == route)
        .map_or("Flightdeck", |(_, title)| *title)
}

fn menu_heading(label: &'static str) -> impl IntoElement {
    div()
        .px(px(8.0))
        .pt(px(4.0))
        .text_size(px(12.0))
        .text_color(rgb(MUTED))
        .child(label)
}

#[derive(Clone, Copy)]
enum Icon {
    Calendar,
    Chevron,
    Latest,
}

/// The prototype's select control: the choice made, and the arrow of a menu, in a box of the
/// given width.
pub(crate) fn select(label: &str, width: f32) -> gpui::Stateful<Div> {
    div()
        .id(gpui::SharedString::from(label.to_owned()))
        .role(gpui::Role::Button)
        .aria_label(label.to_owned())
        .flex()
        .items_center()
        .justify_between()
        .w(px(width))
        .h(px(44.0))
        .px(px(12.0))
        .rounded(px(5.0))
        .bg(rgb(RAISED))
        .border_1()
        .border_color(charts::alpha(TEXT, 0.12))
        .text_size(px(13.0))
        .text_color(rgb(TEXT))
        .cursor_pointer()
        .focusable()
        .tab_stop(true)
        .focus(|style| style.bg(rgb(0x32_46_61)))
        .child(div().truncate().child(label.to_owned()))
        .child(icon(Icon::Chevron))
}

fn icon_button(label: &'static str, shape: Icon, disabled: bool) -> gpui::Stateful<Div> {
    div()
        .id(label)
        .role(gpui::Role::Button)
        .aria_label(label)
        .flex()
        .items_center()
        .justify_center()
        .size(px(44.0))
        .rounded(px(5.0))
        .bg(rgb(PANEL))
        .border_1()
        .border_color(charts::alpha(TEXT, 0.12))
        .when(disabled, |button| button.opacity(0.4))
        .when(!disabled, |button| {
            button
                .cursor_pointer()
                .focusable()
                .tab_stop(true)
                .hover(|style| style.bg(rgb(RAISED)))
        })
        .child(icon(shape))
}

/// Draws the prototype's 16-pixel stroked icons, whose paths are given on a 24-unit grid.
fn icon(shape: Icon) -> AnyElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, _| {
            let unit = bounds.size.width / 24.0;
            let at = |x: f32, y: f32| point(bounds.origin.x + unit * x, bounds.origin.y + unit * y);
            let mut path = PathBuilder::stroke(px(1.5));
            match shape {
                Icon::Calendar => icon_calendar(&mut path, at),
                Icon::Chevron => {
                    path.move_to(at(7.0, 10.0));
                    path.line_to(at(12.0, 15.0));
                    path.line_to(at(17.0, 10.0));
                }
                Icon::Latest => icon_latest(&mut path, at, unit),
            }
            match path.build() {
                Ok(path) => window.paint_path(path, rgb(TEXT)),
                Err(error) => eprintln!("icon path: {error}"),
            }
        },
    )
    .size(px(ICON_SIZE))
    .flex_shrink_0()
    .into_any_element()
}

fn icon_calendar(path: &mut PathBuilder, at: impl Fn(f32, f32) -> Point<Pixels>) {
    path.add_polygon(
        &[at(3.0, 5.0), at(21.0, 5.0), at(21.0, 21.0), at(3.0, 21.0)],
        true,
    );
    for (start, end) in [
        ((16.0, 3.0), (16.0, 7.0)),
        ((8.0, 3.0), (8.0, 7.0)),
        ((3.0, 11.0), (21.0, 11.0)),
    ] {
        path.move_to(at(start.0, start.1));
        path.line_to(at(end.0, end.1));
    }
}

/// A counter-clockwise arrow: most of a circle of radius 9 ending in an arrowhead at its start.
fn icon_latest(path: &mut PathBuilder, at: impl Fn(f32, f32) -> Point<Pixels>, unit: Pixels) {
    path.move_to(at(3.0, 11.0));
    path.arc_to(
        gpui::point(unit * 9.0, unit * 9.0),
        px(0.0),
        true,
        true,
        at(5.4, 18.0),
    );
    path.move_to(at(3.0, 4.0));
    path.line_to(at(3.0, 11.0));
    path.line_to(at(10.0, 11.0));
}

#[cfg(test)]
mod tests {
    use super::pill_label;

    #[test]
    fn the_source_pill_reads_the_state_of_the_readings_and_never_a_snapshot_name() {
        // Goal: a live page's pill says Live, as the status does, and not the prototype's
        // "sysmon snapshot". Method: label each healthy status on a page and on Self.
        assert_eq!(pill_label("cpu", "Live"), "Live");
        assert_eq!(pill_label("cpu", "Paused"), "Paused");
        assert_eq!(pill_label("cpu", "Recorded"), "Recorded");
        assert_eq!(pill_label("cpu", "Waiting"), "Waiting");
        assert_eq!(
            pill_label("self", "Reader connected"),
            "Flightdeck diagnostic run"
        );
    }
}
