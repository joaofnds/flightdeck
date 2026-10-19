//! The compact state timeline the prototype draws for memory pressure beside thermal state:
//! a labelled band, the window's first and last minute, and a caption naming the state.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{AnyElement, Bounds, Context, Pixels, SharedString, div, prelude::*, px, rgb};

use super::ChartHelp;
use crate::charts::{self, MUTED, PANEL};
use crate::dashboard::control;
use crate::data::Panel;
use crate::live_dashboard::LiveDashboard;
use crate::live_presentation::clock;

/// The height of the state band, as the prototype's compact timeline draws it.
const BAND_HEIGHT: f32 = 45.0;
/// The prototype narrows the label column on plots under this many pixels wide.
const NARROW_CARD: f32 = 300.0;

impl LiveDashboard {
    pub(crate) fn live_timeline(&self, panel: &Rc<Panel>, cx: &mut Context<Self>) -> AnyElement {
        let index = self.chart_inspection_index(panel);
        let inspection = super::chart_inspection(panel, index);
        let measured = Rc::new(Cell::new(Bounds::default()));
        let plot = charts::plot_without_cursor(
            Rc::clone(panel),
            inspection,
            self.chart_hidden(panel),
            Rc::clone(&measured),
            None,
        );
        let cursor = self.chart_cursor_fraction(panel);
        let band = self.chart_events(
            div()
                .relative()
                .flex_1()
                .min_w_0()
                .h(charts::PLOT_INSETS.top + px(BAND_HEIGHT) + charts::PLOT_INSETS.bottom)
                .child(plot)
                .child(super::chart_cursor(cursor))
                .children(self.chart_tooltip(panel, index, cursor)),
            measured,
            panel,
            cx,
        );
        let gutter = self.timeline_gutter();

        div()
            .debug_selector(|| format!("chart-card:{}", panel.id))
            .flex_1()
            .min_w_0()
            .rounded(px(12.0))
            .border_1()
            .border_color(rgb(charts::LINE))
            .bg(rgb(PANEL))
            .px(px(12.0))
            .pt(px(8.0))
            .pb(px(10.0))
            .child(timeline_heading(panel))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(timeline_label(gutter, timeline_name(panel)))
                    .child(band),
            )
            .child(self.timeline_bounds(gutter))
            .child(self.timeline_caption(panel, index))
            .into_any_element()
    }

    /// The label column's width. The prototype gives it 110 pixels, and 55 on a narrow card.
    pub(crate) fn timeline_gutter(&self) -> Pixels {
        let columns = match self.reading_chart_columns() {
            1 => 1.0,
            2 => 2.0,
            _ => 3.0,
        };
        // The sidebar and the page's side padding take 212 pixels of the window.
        let card = (self.viewport_size.width - px(212.0)) / columns;
        if card < px(NARROW_CARD) {
            px(55.0)
        } else {
            px(110.0)
        }
    }

    pub(crate) fn timeline_bounds(&self, gutter: Pixels) -> gpui::Div {
        let row = div()
            .pl(gutter)
            .h(px(20.0))
            .flex()
            .items_center()
            .justify_between()
            .text_size(px(11.0))
            .text_color(rgb(MUTED));
        let Some(time) = self.chart_time_window() else {
            return row;
        };

        assert!(time.span > 0);
        let last = time.first + (time.span - 1);
        row.child(clock(time.first, false))
            .child(clock(last, false))
    }

    fn timeline_caption(&self, panel: &Panel, index: Option<usize>) -> gpui::Div {
        let state = self.chart_reading(panel, index);
        let time = self
            .reading_snapshot()
            .and_then(|snapshot| snapshot.selection["second"].as_u64())
            .map_or_else(|| "Unavailable".into(), |second| clock(second, true));
        let states = panel
            .states
            .as_ref()
            .map_or_else(String::new, |states| states.join(" / "));
        let selector = format!("chart-caption:{}:{state}", panel.id);

        timeline_caption_text(format!("{time} · {state} · {states}"))
            .debug_selector(move || selector.clone())
    }
}

/// A panel heading as the prototype draws it: the title, then its help control beside it.
pub(crate) fn timeline_heading_text(title: &str, help: String, glyph: &'static str) -> gpui::Div {
    let selector = format!("chart-help:{title}");
    div()
        .flex()
        .items_center()
        .gap(px(4.0))
        .min_h(px(40.0))
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(title.to_owned()),
        )
        .child(
            control(glyph)
                .id(SharedString::from(selector.clone()))
                .debug_selector(move || selector.clone())
                .text_color(rgb(MUTED))
                .tooltip(move |_, cx| cx.new(|_| ChartHelp(help.clone())).into()),
        )
}

fn timeline_heading(panel: &Panel) -> gpui::Div {
    let help = panel
        .help
        .clone()
        .unwrap_or_else(|| "Blank means unavailable, including gaps.".into());
    timeline_heading_text(&panel.title, help, "ⓘ")
}

pub(crate) fn timeline_label(gutter: Pixels, name: &str) -> gpui::Div {
    div()
        .w(gutter)
        .flex_shrink_0()
        .pr(px(8.0))
        .flex()
        .justify_end()
        .text_size(px(11.0))
        .text_color(rgb(MUTED))
        .child(name.to_owned())
}

pub(crate) fn timeline_caption_text(text: String) -> gpui::Div {
    div()
        .mt(px(7.0))
        .text_size(px(11.0))
        .line_height(px(17.0))
        .text_color(rgb(MUTED))
        .child(text)
}

fn timeline_name(panel: &Panel) -> &str {
    match panel.id.as_str() {
        "live-memory-pressure" => "Pressure",
        "live-battery-charging" => "Charging",
        _ => panel
            .lines
            .first()
            .map_or("State", |line| line.name.as_str()),
    }
}
