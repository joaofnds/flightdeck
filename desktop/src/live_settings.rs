use gpui::{Context, IntoElement, Toggled, div, prelude::*, px, rgb};

use crate::charts::{self, BLUE, LINE, MUTED, ORANGE, PANEL, TEXT};
use crate::live_dashboard::LiveDashboard;

pub(crate) const ANIMATE_FANS_DEFAULT: bool = false;

pub struct Settings {
    pub open: bool,
    pub animate_fans: bool,
    pub notice: Option<String>,
    pub(crate) animation_focus: Option<gpui::FocusHandle>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            open: false,
            animate_fans: ANIMATE_FANS_DEFAULT,
            notice: None,
            animation_focus: None,
        }
    }
}

impl Settings {
    pub fn refresh_notice(&mut self, cx: &gpui::App) -> bool {
        if self.notice.is_none() {
            return false;
        }

        let notice = crate::live_launch::animation_save_notice(cx);
        let changed = self.notice != notice;
        self.notice = notice;
        changed
    }
}

impl LiveDashboard {
    /// Opens app settings without sending a resource request to the reader.
    pub fn open_settings(&mut self, cx: &mut Context<Self>) {
        self.settings.open = true;
        self.native.time_menu_open = false;
        self.picker = None;
        self.chart_inspector = None;
        self.drag = None;
        cx.notify();
    }

    /// Applies the choice now, even if its save fails, and makes that failure visible.
    pub fn toggle_animate_fans(&mut self, cx: &mut Context<Self>) {
        self.set_fan_animation(!self.settings.animate_fans, cx);
    }

    fn restore_fan_animation(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        self.set_fan_animation(ANIMATE_FANS_DEFAULT, cx);
        if let Some(focus) = &self.settings.animation_focus {
            window.focus(focus, cx);
        }
    }

    fn set_fan_animation(&mut self, enabled: bool, cx: &mut Context<Self>) {
        self.settings.animate_fans = enabled;
        self.settings.notice = crate::live_launch::set_animate_fans(enabled, cx).err();
        cx.notify();
    }

    pub fn settings_view(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.settings.animation_focus.is_none() {
            self.settings.animation_focus = Some(cx.focus_handle().tab_stop(true));
        }

        div()
            .id("settings-page")
            .debug_selector(|| "settings-page".into())
            .accessibility_id("flightdeck.settings")
            .role(gpui::Role::Group)
            .aria_label("Settings")
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .overflow_y_scroll()
            .px(px(16.0))
            .py(px(24.0))
            .gap(px(32.0))
            .child(settings_header())
            .child(self.settings_view_appearance(cx))
    }

    fn settings_view_appearance(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w_full()
            .max_w(px(760.0))
            .flex()
            .flex_col()
            .gap(px(12.0))
            .child(
                div()
                    .text_size(px(16.0))
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Appearance"),
            )
            .child(
                div()
                    .rounded(px(7.0))
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(charts::alpha(TEXT, 0.12))
                    .child(self.settings_view_appearance_row(cx)),
            )
            .child(settings_motion_status())
            .children(
                self.settings
                    .notice
                    .as_deref()
                    .map(settings_view_appearance_notice),
            )
    }

    fn settings_view_appearance_row(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .p(px(12.0))
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(16.0))
            .child(
                div()
                    .flex_grow_1()
                    .flex_basis(px(240.0))
                    .min_w(px(0.0))
                    .max_w_full()
                    .child(self.settings_view_appearance_row_label(cx))
                    .child(
                        div()
                            .debug_selector(|| "animate-fans-description".into())
                            .mt(px(4.0))
                            .text_size(px(13.0))
                            .line_height(px(20.0))
                            .text_color(rgb(MUTED))
                            .child(
                                "Show rotating fans and outgoing airflow on Overview \
                                (uses more resources)",
                            ),
                    ),
            )
            .child(self.settings_animation_toggle(cx))
    }

    fn settings_view_appearance_row_label(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .items_center()
            .min_h(px(24.0))
            .gap(px(4.0))
            .child(
                div()
                    .min_w_0()
                    .debug_selector(|| "animate-fans-label".into())
                    .font_weight(gpui::FontWeight::SEMIBOLD)
                    .child("Animate fans"),
            )
            .when(
                self.settings.animate_fans != ANIMATE_FANS_DEFAULT,
                |label| label.child(Self::settings_view_appearance_row_label_reset(cx)),
            )
    }

    fn settings_animation_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = self.settings.animate_fans;
        let state = animation_value(enabled);
        let Some(focus) = &self.settings.animation_focus else {
            unreachable!("Settings installs the switch focus before building its controls");
        };

        div()
            .id("animate-fans")
            .debug_selector(|| "animate-fans".into())
            .accessibility_id("flightdeck.settings.animate-fans")
            .role(gpui::Role::Switch)
            .aria_label("Animate fans")
            .aria_toggled(if enabled {
                Toggled::True
            } else {
                Toggled::False
            })
            .aria_value(state)
            .track_focus(focus)
            .tab_stop(true)
            .focus(crate::charts::focus_ring)
            .cursor_pointer()
            .min_h(px(44.0))
            .px(px(8.0))
            .rounded(px(5.0))
            .flex()
            .flex_shrink_0()
            .items_center()
            .gap(px(8.0))
            .hover(|style| style.bg(charts::alpha(TEXT, 0.06)))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_animate_fans(cx)))
            .child(
                div()
                    .w(px(24.0))
                    .debug_selector(move || format!("animate-fans:{state}"))
                    .text_color(rgb(MUTED))
                    .child(state),
            )
            .child(settings_animation_toggle_switch(enabled))
    }

    fn settings_view_appearance_row_label_reset(cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("restore-animate-fans")
            .size(px(24.0))
            .flex_shrink_0()
            .debug_selector(|| "restore-animate-fans".into())
            .role(gpui::Role::Button)
            .accessibility_id("flightdeck.settings.animate-fans.restore")
            .aria_label(format!(
                "Reset Animate fans to default: {}",
                animation_value(ANIMATE_FANS_DEFAULT)
            ))
            .focusable()
            .tab_stop(true)
            .focus(crate::charts::focus_ring)
            .rounded(px(5.0))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .hover(|style| style.bg(charts::alpha(TEXT, 0.06)))
            .tooltip(|_, cx| cx.new(|_| FanDefaultHelp).into())
            .on_click(cx.listener(|this, _, window, cx| this.restore_fan_animation(window, cx)))
            .child(settings_view_appearance_row_label_reset_icon())
    }
}

struct FanDefaultHelp;

impl gpui::Render for FanDefaultHelp {
    fn render(&mut self, _: &mut gpui::Window, _: &mut Context<Self>) -> impl IntoElement {
        let text = "Reset to default";
        div()
            .child(text)
            .debug_selector(move || text.into())
            .px(px(8.0))
            .py(px(4.0))
            .rounded(px(5.0))
            .bg(rgb(PANEL))
            .border_1()
            .border_color(rgb(LINE))
            .text_size(px(12.0))
            .text_color(rgb(TEXT))
    }
}

fn settings_view_appearance_row_label_reset_icon() -> impl IntoElement {
    gpui::canvas(
        |_, _, _| (),
        |bounds, (), window, _| {
            let unit = bounds.size.width / 24.0;
            let at = |x: f32, y: f32| {
                gpui::point(bounds.origin.x + unit * x, bounds.origin.y + unit * y)
            };

            let mut path = gpui::PathBuilder::stroke(px(1.5));
            path.move_to(at(9.0, 14.0));
            path.line_to(at(4.0, 9.0));
            path.line_to(at(9.0, 4.0));
            path.move_to(at(4.0, 9.0));
            path.line_to(at(14.0, 9.0));
            path.cubic_bezier_to(at(14.0, 21.0), at(22.0, 9.0), at(22.0, 21.0));
            path.line_to(at(12.0, 21.0));

            match path.build() {
                Ok(path) => window.paint_path(path, rgb(TEXT)),
                Err(error) => eprintln!("Settings reset icon: {error}"),
            }
        },
    )
    .size(px(16.0))
}

fn animation_value(enabled: bool) -> &'static str {
    if enabled { "On" } else { "Off" }
}

fn settings_animation_toggle_switch(enabled: bool) -> impl IntoElement {
    div()
        .w(px(40.0))
        .h(px(24.0))
        .p(px(3.0))
        .rounded(px(12.0))
        .bg(rgb(if enabled { BLUE } else { LINE }))
        .border_1()
        .border_color(charts::alpha(TEXT, 0.24))
        .flex()
        .items_center()
        .when(enabled, gpui::Div::justify_end)
        .child(div().size(px(16.0)).rounded_full().bg(rgb(TEXT)))
}

fn settings_view_appearance_notice(notice: &str) -> impl IntoElement {
    div()
        .id("settings-notice")
        .debug_selector(|| "settings-notice".into())
        .accessibility_id("flightdeck.settings.notice")
        .role(gpui::Role::Status)
        .aria_label(notice.to_owned())
        .text_size(px(13.0))
        .text_color(rgb(ORANGE))
        .child(notice.to_owned())
}

fn settings_header() -> impl IntoElement {
    div()
        .child(
            div()
                .id("settings-heading")
                .role(gpui::Role::Heading)
                .aria_level(1)
                .text_size(px(23.0))
                .line_height(px(30.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("Settings"),
        )
        .child(
            div()
                .mt(px(3.0))
                .text_color(rgb(MUTED))
                .child("Appearance and behavior."),
        )
}

fn settings_motion_status() -> impl IntoElement {
    let status = if crate::hardware_motion::reduce_motion() {
        "Reduce Motion is on in macOS, so fans and airflow stay still."
    } else {
        "Animation pauses when Flightdeck is inactive and follows Reduce Motion in macOS."
    };

    div()
        .debug_selector(|| "settings-reduce-motion".into())
        .text_size(px(12.0))
        .line_height(px(18.0))
        .text_color(rgb(MUTED))
        .child(status)
}
