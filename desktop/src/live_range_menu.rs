use gpui::{AnyElement, Context, Div, IntoElement, div, prelude::*, px, rgb};

use crate::charts::{self, BLUE, MUTED, TEXT};
use crate::live_dashboard::LiveDashboard;
use crate::time_range::Range;

const SELECTED: u32 = 0x0057_94f2;
const RAISED: u32 = 0x002c_2f35;
/// The quick ranges the loaded history can show, in minutes.
const QUICK_MINUTES: [u64; 3] = [5, 10, 15];
/// The ranges the history is too short to show yet.
const LONGER_RANGES: [&str; 2] = ["Last 30 minutes", "Last 1 hour"];

impl LiveDashboard {
    /// What the picker reads: the stretch the charts show.
    pub(crate) fn range_label(&self) -> String {
        self.range.label(self.selected_second())
    }

    /// The buttons beside the picker: back to the last 15 minutes while zoomed, and to the
    /// latest reading while paused.
    pub(crate) fn range_buttons(&self, latest: AnyElement, cx: &mut Context<Self>) -> Div {
        let zoomed = self.range != Range::History;
        div()
            .flex()
            .items_center()
            .gap(px(4.0))
            .children(zoomed.then(|| Self::reset_button(cx)))
            .child(latest)
    }

    fn reset_button(cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("time-reset")
            .debug_selector(|| "time-reset".into())
            .role(gpui::Role::Button)
            .aria_label("Back to the last 15 minutes")
            .flex()
            .items_center()
            .min_h(px(44.0))
            .px(px(12.0))
            .rounded(px(5.0))
            .bg(rgb(charts::PANEL))
            .border_1()
            .border_color(charts::alpha(TEXT, 0.12))
            .cursor_pointer()
            .focusable()
            .tab_stop(true)
            .hover(|style| style.bg(rgb(RAISED)))
            .on_click(cx.listener(|this, _, _, cx| this.reset_range(cx)))
            .child("Last 15 min")
    }

    /// The quick ranges, the ones the history is too short for, and a custom number of minutes.
    pub(crate) fn range_choices(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let mut choices = div().flex().flex_col().gap(px(4.0));
        for minutes in QUICK_MINUTES {
            choices = choices.child(self.quick_range(minutes, cx));
        }

        choices
            .children(LONGER_RANGES.map(unavailable_range))
            .child(self.custom_range(cx))
    }

    fn quick_range(&self, minutes: u64, cx: &mut Context<Self>) -> impl IntoElement {
        let range = Range::minutes(minutes);
        let chosen = self.range == range;
        div()
            .id(gpui::SharedString::from(format!("range-{minutes}")))
            .debug_selector(move || format!("time-range-{minutes}m"))
            .role(gpui::Role::Button)
            .aria_label(range.label(0))
            .flex()
            .items_center()
            .justify_between()
            .min_h(px(44.0))
            .p(px(8.0))
            .rounded(px(5.0))
            .cursor_pointer()
            .hover(|style| style.bg(rgb(RAISED)))
            .when(chosen, |item| {
                item.bg(charts::alpha(SELECTED, 0.15)).text_color(rgb(BLUE))
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.native.time_menu_open = false;
                this.set_range(range, cx);
            }))
            .child(range.label(0))
            .children(chosen.then_some("✓"))
    }

    fn custom_range(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let minutes = self.range.whole_minutes(self.selected_second());
        div()
            .flex()
            .items_center()
            .justify_between()
            .min_h(px(44.0))
            .p(px(8.0))
            .child("Custom")
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.0))
                    .child(Self::step_button(
                        "time-custom-less",
                        "−",
                        minutes.saturating_sub(1),
                        cx,
                    ))
                    .child(
                        div()
                            .min_w(px(56.0))
                            .text_center()
                            .child(format!("{minutes} min")),
                    )
                    .child(Self::step_button("time-custom-more", "+", minutes + 1, cx)),
            )
    }

    fn step_button(
        label: &'static str,
        glyph: &'static str,
        minutes: u64,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .id(label)
            .debug_selector(|| label.into())
            .role(gpui::Role::Button)
            .aria_label(label)
            .flex()
            .items_center()
            .justify_center()
            .size(px(32.0))
            .rounded(px(5.0))
            .bg(rgb(RAISED))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.set_range(Range::minutes(minutes.max(1)), cx);
            }))
            .child(glyph)
    }

    fn selected_second(&self) -> u64 {
        self.snapshot
            .as_deref()
            .and_then(|snapshot| snapshot.selection["second"].as_u64())
            .unwrap_or(0)
    }
}

fn unavailable_range(label: &'static str) -> impl IntoElement {
    div()
        .flex()
        .items_center()
        .justify_between()
        .min_h(px(44.0))
        .p(px(8.0))
        .opacity(0.4)
        .child(label)
        .child(
            div()
                .text_size(px(11.0))
                .text_color(rgb(MUTED))
                .child("Needs a longer history"),
        )
}
