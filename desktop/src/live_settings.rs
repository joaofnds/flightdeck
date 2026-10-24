use gpui::{Context, IntoElement, Toggled, div, prelude::*, px, rgb};

use crate::charts::{self, BLUE, MUTED, ORANGE, PANEL, TEXT};
use crate::live_dashboard::LiveDashboard;

#[derive(Default)]
pub struct Settings {
    pub open: bool,
    pub animate_fans: bool,
    pub notice: Option<String>,
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
        self.settings.animate_fans = !self.settings.animate_fans;
        self.settings.notice =
            crate::live_launch::set_animate_fans(self.settings.animate_fans, cx).err();
        cx.notify();
    }

    pub fn settings_view(&self, cx: &mut Context<Self>) -> impl IntoElement {
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
            .p(px(24.0))
            .gap(px(24.0))
            .child(settings_header())
            .child(
                div()
                    .max_w(px(640.0))
                    .p(px(16.0))
                    .rounded(px(7.0))
                    .bg(rgb(PANEL))
                    .border_1()
                    .border_color(charts::alpha(TEXT, 0.12))
                    .flex()
                    .flex_col()
                    .gap(px(12.0))
                    .child(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("Appearance"),
                    )
                    .child(self.settings_animation_toggle(cx))
                    .child(div().text_color(rgb(MUTED)).child(
                        "Animate the fans and airflow on Overview. Leave this off to save \
                             resources. Animation pauses when it is not visible and respects \
                             Reduce Motion in macOS.",
                    ))
                    .child(settings_motion_status())
                    .children(self.settings.notice.as_ref().map(|notice| {
                        div()
                            .id("settings-notice")
                            .debug_selector(|| "settings-notice".into())
                            .accessibility_id("flightdeck.settings.notice")
                            .role(gpui::Role::Status)
                            .aria_label(notice.clone())
                            .text_color(rgb(ORANGE))
                            .child(notice.clone())
                    })),
            )
    }

    fn settings_animation_toggle(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let enabled = self.settings.animate_fans;
        let state = if enabled { "On" } else { "Off" };

        div()
            .id("animate-fans")
            .debug_selector(|| "animate-fans".into())
            .accessibility_id("flightdeck.settings.animate-fans")
            .role(gpui::Role::CheckBox)
            .aria_label("Animate fans")
            .aria_toggled(if enabled {
                Toggled::True
            } else {
                Toggled::False
            })
            .aria_value(state)
            .focusable()
            .tab_stop(true)
            .focus(crate::charts::focus_ring)
            .cursor_pointer()
            .min_h(px(44.0))
            .px(px(12.0))
            .py(px(8.0))
            .rounded(px(5.0))
            .flex()
            .items_center()
            .gap(px(12.0))
            .hover(|style| style.bg(rgb(0x2c_2f35)))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_animate_fans(cx)))
            .child(
                div()
                    .size(px(20.0))
                    .flex_shrink_0()
                    .rounded(px(3.0))
                    .border_1()
                    .border_color(rgb(if enabled { BLUE } else { MUTED }))
                    .when(enabled, |mark| mark.bg(charts::alpha(BLUE, 0.15)))
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if enabled { "✓" } else { "" }),
            )
            .child(div().flex_1().child("Animate fans"))
            .child(
                div()
                    .debug_selector(move || format!("animate-fans:{state}"))
                    .text_color(rgb(MUTED))
                    .child(state),
            )
    }
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
                .child("Appearance and animation."),
        )
}

fn settings_motion_status() -> impl IntoElement {
    let status = if crate::hardware_motion::reduce_motion() {
        "Reduce Motion is on in macOS. Fans and airflow stay still."
    } else {
        "Reduce Motion is off in macOS."
    };

    div()
        .debug_selector(|| "settings-reduce-motion".into())
        .text_color(rgb(MUTED))
        .child(status)
}
