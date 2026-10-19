use gpui::{Context, IntoElement, div, prelude::*, px, rgb};
use num_traits::ToPrimitive;
use serde_json::Value;

use crate::charts::{GRID, MUTED, ORANGE, PANEL, TEXT};
use crate::live_chart_view::timeline::{
    self, timeline_caption_text, timeline_heading_text, timeline_label,
};
use crate::live_dashboard::LiveDashboard;
use crate::live_projection::{hardware_reading, quantity};

pub fn clock(second: u64, seconds: bool) -> String {
    crate::chart_axes::clock(second, seconds)
}

pub fn selected_clock(selection: &Value) -> String {
    let Some(second) = selection["second"].as_u64() else {
        return "Waiting for readings".into();
    };
    let Some(parts) = crate::chart_axes::local_parts(second) else {
        return "Time unavailable".into();
    };
    format!(
        "{:02}/{:02}/{} · {}",
        parts.tm_mday,
        parts.tm_mon + 1,
        i64::from(parts.tm_year) + 1900,
        clock(second, true)
    )
}

impl LiveDashboard {
    /// The prototype's row of three compact cards: memory pressure, thermal state, and
    /// cooling and battery. They share the chart grid's columns.
    pub fn overview_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selection = self
            .reading_snapshot()
            .map_or(&Value::Null, |snapshot| &snapshot.selection);
        let summary = crate::live_views::matching_summary(selection);
        let pressure = self
            .panels
            .iter()
            .take(64)
            .find(|panel| panel.id == "live-memory-pressure")
            .map(|panel| self.live_timeline(panel, timeline::BAND_COMPACT, cx));

        div()
            .grid()
            .grid_cols(match self.reading_chart_columns() {
                1 => 1,
                2 => 2,
                _ => 3,
            })
            .gap(px(8.0))
            .child(div().flex().min_w_0().children(pressure))
            .child(self.overview_thermal())
            .child(overview_cooling(summary))
    }

    /// Thermal state as the compact timeline will draw it. The collector records no thermal
    /// state yet, so the band says so instead of showing a state nobody measured.
    fn overview_thermal(&self) -> gpui::Div {
        let gutter = self.timeline_gutter();
        let second = self
            .reading_snapshot()
            .and_then(|snapshot| snapshot.selection["second"].as_u64());
        let time = second.map_or_else(|| "Unavailable".into(), |second| clock(second, true));
        let band = div()
            .debug_selector(|| "overview-context:thermal:Uncaptured".into())
            .flex_1()
            .min_w_0()
            .h(px(45.0))
            .my(px(6.0))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(2.0))
            .border_1()
            .border_color(rgb(GRID))
            .text_size(px(11.0))
            .text_color(rgb(MUTED))
            .child("Uncaptured");

        overview_context_card("thermal")
            .child(timeline_heading_text(
                "Thermal state",
                THERMAL_HELP.into(),
                "ⓘ",
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(timeline_label(gutter, "Thermal"))
                    .child(band),
            )
            .child(self.timeline_bounds(gutter))
            .child(timeline_caption_text(format!(
                "{time} · Uncaptured · Nominal / Fair / Serious / Critical"
            )))
    }

    /// The prototype's battery state row: the compact charging timeline and a details card
    /// with health, cycle count and the adapter's rating.
    pub fn battery_state(
        &self,
        panel: Option<&std::rc::Rc<crate::data::Panel>>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let selection = self
            .reading_snapshot()
            .map_or(&Value::Null, |snapshot| &snapshot.selection);
        let battery = &crate::live_views::matching_summary(selection)["battery"];
        let details = overview_context_card("battery-details")
            .child(timeline_heading_text(
                "Battery details",
                BATTERY_HELP.into(),
                "About",
            ))
            .child(overview_context_reading(
                &ContextReading {
                    key: "health",
                    label: "Health",
                },
                hardware_reading(battery, "health_percent", "%", 1),
            ))
            .child(overview_context_reading(
                &ContextReading {
                    key: "cycles",
                    label: "Cycles",
                },
                hardware_reading(battery, "cycle_count", "", 0),
            ))
            .child(overview_context_reading(
                &ContextReading {
                    key: "adapter",
                    label: "Adapter rating",
                },
                quantity(battery, "adapter_watts", "W", 1.0),
            ));

        div()
            .grid()
            .grid_cols(match self.reading_chart_columns() {
                1 => 1,
                _ => 2,
            })
            .gap(px(8.0))
            .child(
                div().flex().min_w_0().children(
                    panel.map(|panel| self.live_timeline(panel, timeline::BAND_COMPACT, cx)),
                ),
            )
            .child(details)
            .into_any_element()
    }

    pub fn missing_reading(&self) -> impl IntoElement {
        let missing = self
            .reading_snapshot()
            .is_some_and(|snapshot| snapshot.selection["selected"].is_null());
        div().h(px(16.0)).children(missing.then(|| {
            div()
                .debug_selector(|| "missing-observation".into())
                .text_color(rgb(ORANGE))
                .child("No recorded observation at this second")
        }))
    }
}

const THERMAL_HELP: &str = "macOS's own verdict on heat, from nominal through fair and \
    serious to critical. This collector does not record it yet, so no state is shown.";

const BATTERY_HELP: &str = "Health compares the full-charge capacity with the design \
    capacity. The adapter rating is the charger's limit, not a measurement of the power drawn.";

const COOLING_HELP: &str = "The SoC temperature, each fan channel's speed and the battery's \
    charge at the selected second, as the collector read them from the SMC and the battery.";

struct ContextReading {
    key: &'static str,
    label: &'static str,
}

fn overview_cooling(summary: &Value) -> gpui::Div {
    overview_context_card("cooling-battery")
        .child(timeline_heading_text(
            "Cooling and battery",
            COOLING_HELP.into(),
            "About",
        ))
        .child(overview_context_reading(
            &ContextReading {
                key: "soc",
                label: "SoC temperature",
            },
            hardware_reading(summary, "soc_celsius", " °C", 1),
        ))
        .child(overview_context_reading(
            &ContextReading {
                key: "fan0",
                label: "Fan channel 0",
            },
            overview_context_fan(summary, 0),
        ))
        .child(overview_context_reading(
            &ContextReading {
                key: "fan1",
                label: "Fan channel 1",
            },
            overview_context_fan(summary, 1),
        ))
        .child(overview_context_reading(
            &ContextReading {
                key: "charge",
                label: "Battery charge",
            },
            hardware_reading(&summary["battery"], "charge_percent", "%", 1),
        ))
}

pub(crate) fn overview_context_card(key: &'static str) -> gpui::Div {
    div()
        .debug_selector(move || format!("overview-context:{key}"))
        .min_w_0()
        .flex()
        .flex_col()
        .rounded(px(12.0))
        .border_1()
        .border_color(rgb(crate::charts::LINE))
        .bg(rgb(PANEL))
        .px(px(12.0))
        .pt(px(8.0))
        .pb(px(10.0))
        .text_color(rgb(TEXT))
}

fn overview_context_reading(specification: &ContextReading, reading: String) -> gpui::Div {
    let key = specification.key;
    let selector = format!("overview-context:{key}:{reading}");
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .justify_between()
        .gap(px(12.0))
        .py(px(8.0))
        .mb(px(2.0))
        .border_b_1()
        .border_color(rgb(GRID))
        .text_size(px(13.0))
        .child(div().text_color(rgb(MUTED)).child(specification.label))
        .child(div().font_weight(gpui::FontWeight::SEMIBOLD).child(reading))
}

fn overview_context_fan(summary: &Value, channel: usize) -> String {
    assert!(channel < 2);
    let Some(value) = summary["fan_rpm"].get(channel).and_then(Value::as_f64) else {
        return "Unavailable".into();
    };

    if !value.is_finite() {
        return "Unavailable".into();
    }

    if value < 0.0 {
        return "Unavailable".into();
    }

    match value.to_f32() {
        Some(value) => format!("{} rpm", crate::live_projection::fixed_text(value, 0)),
        None => "Unavailable".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_clock_is_fixed_width_and_refuses_unrepresentable_time() {
        // Local timezone varies, but valid clock labels retain their HH:MM[:SS] shape.
        let short = clock(1_700_000_000, false);
        let long = clock(1_700_000_000, true);
        assert_eq!(short.len(), 5);
        assert_eq!(long.len(), 8);
        assert_eq!(&short[2..3], ":");
        assert_eq!(&long[5..6], ":");
        assert_eq!(clock(u64::MAX, true), "Unavailable");
    }
}
