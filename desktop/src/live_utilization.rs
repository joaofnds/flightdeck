use std::rc::Rc;

use gpui::{
    AnyElement, Context, IntoElement, div, linear_color_stop, linear_gradient, prelude::*, px,
    relative, rgb,
};
use num_traits::ToPrimitive;
use serde_json::Value;

use crate::charts::{GREEN, GRID, MUTED, ORANGE, RED, TEXT, YELLOW};
use crate::data::Panel;
use crate::live_dashboard::LiveDashboard;
use crate::live_presentation::overview_context_card;

const HELP: &str = "The share of each resource in use at the selected second. A core \
    level's share averages its own CPUs, the GPU's is its active time, memory's is memory in \
    use against installed memory, and swap's is swap used against the swap macOS set aside. \
    The colours mark 60, 80 and 90 percent. Memory pressure and thermal state are macOS's own \
    verdicts.";

/// The width at which the panel and the chart sit side by side, as the prototype lays them.
const SIDE_BY_SIDE_WIDTH: f32 = 1000.0;

/// One resource's share in use, in percent, or none where it was not measured.
#[derive(Debug, PartialEq)]
pub(crate) struct Utilization {
    pub name: &'static str,
    pub percent: Option<f64>,
}

impl LiveDashboard {
    /// The prototype's analysis row: the utilization panel, then the chart that sets load,
    /// power and heat side by side, stacked where the window is narrow.
    pub(crate) fn overview_analysis(
        &self,
        panel: Option<&Rc<Panel>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let selection = self
            .reading_snapshot()
            .map_or(&Value::Null, |snapshot| &snapshot.selection);
        let panel_utilization = utilization_panel(super::matching_summary(selection));
        let chart = panel.map(|panel| self.live_chart(panel, cx).into_any_element());

        if self.viewport_size.width < px(SIDE_BY_SIDE_WIDTH) {
            return div()
                .flex()
                .flex_col()
                .gap(px(8.0))
                .child(panel_utilization)
                .children(chart)
                .into_any_element();
        }
        div()
            .flex()
            .gap(px(8.0))
            .child(
                panel_utilization
                    .w(relative(1.0 / 3.0))
                    .min_w(px(260.0))
                    .flex_shrink_0(),
            )
            .child(div().flex().flex_1().min_w_0().children(chart))
            .into_any_element()
    }
}

/// The share in use of each resource the prototype lists, slower core level first.
pub(crate) fn utilizations(summary: &Value) -> Vec<Utilization> {
    let levels = crate::live_inventory::performance_levels();
    let level = |index: usize, fallback: &'static str| Utilization {
        name: levels.map_or(fallback, |levels| levels[index].cores.as_str()),
        percent: levels.and_then(|_| summary["cpu_level_percent"][index].as_f64()),
    };
    let memory = super::summary::memory_in_use_bytes(summary);
    vec![
        level(1, "Core level 1"),
        level(0, "Core level 0"),
        Utilization {
            name: "GPU",
            percent: summary["gpu_active_percent"].as_f64(),
        },
        Utilization {
            name: "Memory",
            percent: utilizations_share(memory, summary["memory_total_bytes"].as_u64()),
        },
        Utilization {
            name: "Swap",
            percent: utilizations_share(
                summary["swap_used_bytes"].as_u64(),
                summary["swap_total_bytes"].as_u64(),
            ),
        },
    ]
}

/// The part as a percentage of the whole, or none where either is missing or the whole is
/// empty, since a share of nothing is not zero.
fn utilizations_share(part: Option<u64>, whole: Option<u64>) -> Option<f64> {
    let whole = whole?;
    if whole == 0 {
        return None;
    }
    Some(part?.to_f64()? / whole.to_f64()? * 100.0)
}

fn utilization_panel(summary: &Value) -> gpui::Div {
    overview_context_card("utilization")
        .child(crate::live_chart_view::timeline::timeline_heading_text(
            "Utilization by resource",
            HELP.into(),
            "ⓘ",
        ))
        .children(
            utilizations(summary)
                .into_iter()
                .map(|utilization| utilization_row(&utilization)),
        )
        .child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(12.0))
                .mt(px(12.0))
                .pt(px(12.0))
                .border_t_1()
                .border_color(rgb(GRID))
                .child(utilization_status(
                    "Memory pressure",
                    super::pressure(&summary["pressure"]),
                ))
                .child(utilization_status(
                    "Thermal state",
                    crate::live_system::thermal_pressure_text(summary),
                )),
        )
        .child(
            div()
                .mt(px(10.0))
                .text_size(px(11.0))
                .text_color(rgb(MUTED))
                .child(
                    "Utilization: 0–100%. Display thresholds: 60 / 80 / 90%. Verdicts below \
                     come from macOS.",
                ),
        )
}

fn utilization_row(utilization: &Utilization) -> gpui::Div {
    let reading = utilization
        .percent
        .map_or_else(|| "Unavailable".into(), |percent| format!("{percent:.0}%"));
    let selector = format!("utilization:{}:{reading}", utilization.name);
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .gap(px(8.0))
        .min_h(px(32.0))
        .text_size(px(12.0))
        .child(div().w(px(128.0)).flex_shrink_0().child(utilization.name))
        .child(utilization_track(utilization.percent))
        .child(
            div()
                .min_w(px(45.0))
                .flex()
                .justify_end()
                .text_color(rgb(utilization_color(utilization.percent)))
                .child(reading),
        )
}

/// The track with the bar drawn to the share in use. The bar shows the colours of the whole
/// scale up to its own length, so a bar past 60% turns from green through yellow and orange
/// towards red where it crosses each threshold.
fn utilization_track(percent: Option<f64>) -> gpui::Div {
    let track = div()
        .flex_1()
        .min_w(px(20.0))
        .h(px(22.0))
        .rounded(px(2.0))
        .overflow_hidden()
        .bg(rgb(GRID));
    let Some(filled) = percent.and_then(|percent| percent.clamp(0.0, 100.0).to_f32()) else {
        return track;
    };
    if filled <= 0.0 {
        return track;
    }

    let scale = div()
        .flex()
        .flex_shrink_0()
        .h_full()
        .w(relative(100.0 / filled))
        .child(div().w(relative(0.6)).h_full().bg(rgb(GREEN)))
        .child(utilization_track_segment(GREEN, YELLOW, 0.2))
        .child(utilization_track_segment(YELLOW, ORANGE, 0.1))
        .child(utilization_track_segment(ORANGE, RED, 0.1));
    track.child(
        div()
            .h_full()
            .w(relative(filled / 100.0))
            .overflow_hidden()
            .child(scale),
    )
}

fn utilization_track_segment(from: u32, to: u32, share: f32) -> gpui::Div {
    div().w(relative(share)).h_full().bg(linear_gradient(
        90.0,
        linear_color_stop(rgb(from), 0.0),
        linear_color_stop(rgb(to), 1.0),
    ))
}

/// The colour of a share's number: green below 60%, then yellow, orange from 80% and red
/// from 90%, as the prototype marks them.
fn utilization_color(percent: Option<f64>) -> u32 {
    let Some(percent) = percent else {
        return MUTED;
    };
    if percent >= 90.0 {
        return RED;
    }
    if percent >= 80.0 {
        return ORANGE;
    }
    if percent >= 60.0 {
        return YELLOW;
    }
    GREEN
}

fn utilization_status(label: &'static str, verdict: &'static str) -> gpui::Div {
    let selector = format!("utilization-status:{label}:{verdict}");
    div()
        .debug_selector(move || selector.clone())
        .text_size(px(11.0))
        .text_color(rgb(MUTED))
        .child(label)
        .child(
            div()
                .text_size(px(13.0))
                .text_color(rgb(TEXT))
                .child(verdict),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn utilizations_divide_memory_and_swap_by_their_wholes() {
        // Goal: memory and swap read as shares of installed memory and of swap set aside.
        // Method: a summary with a quarter of memory and half of swap in use.
        let summary = json!({
            "gpu_active_percent": 12.5,
            "memory_anonymous_bytes": 6_u64, "memory_purgeable_bytes": 2_u64,
            "memory_wired_bytes": 3_u64, "memory_compressed_bytes": 1_u64,
            "memory_total_bytes": 32_u64,
            "swap_used_bytes": 4_u64, "swap_total_bytes": 8_u64,
        });

        let utilizations = utilizations(&summary);

        assert_eq!(utilizations.len(), 5);
        assert_eq!(utilizations[2].percent, Some(12.5));
        assert_eq!(utilizations[3].name, "Memory");
        assert_eq!(utilizations[3].percent, Some(25.0));
        assert_eq!(utilizations[4].name, "Swap");
        assert_eq!(utilizations[4].percent, Some(50.0));
    }

    #[test]
    fn utilizations_withhold_a_share_of_nothing_or_of_a_missing_reading() {
        // Goal: no swap set aside and an unread memory component leave the shares unknown.
        // Method: a summary with zero swap and no compressed memory.
        let summary = json!({
            "memory_anonymous_bytes": 6_u64, "memory_purgeable_bytes": 2_u64,
            "memory_wired_bytes": 3_u64, "memory_total_bytes": 32_u64,
            "swap_used_bytes": 0_u64, "swap_total_bytes": 0_u64,
        });

        let utilizations = utilizations(&summary);

        assert_eq!(utilizations[2].percent, None);
        assert_eq!(utilizations[3].percent, None);
        assert_eq!(utilizations[4].percent, None);
    }

    #[test]
    fn utilization_color_marks_each_threshold() {
        // Goal: the number turns at 60, 80 and 90 percent and stays muted when unknown.
        // Method: one reading on each side of every threshold.
        let colors = [
            None,
            Some(59.9),
            Some(60.0),
            Some(80.0),
            Some(89.9),
            Some(90.0),
        ]
        .map(utilization_color);

        assert_eq!(colors, [MUTED, GREEN, YELLOW, ORANGE, ORANGE, RED]);
    }
}
