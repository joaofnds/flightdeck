use gpui::{Context, IntoElement, div, prelude::*, px, rgb};
use serde_json::Value;

use crate::charts::{GRID, MUTED, ORANGE, PANEL, TEXT};
use crate::dashboard::control;
use crate::live_dashboard::LiveDashboard;
use crate::live_projection::quantity;

pub fn clock(second: u64, seconds: bool) -> String {
    let Some(parts) = local_parts(second) else {
        return "Unavailable".into();
    };
    if seconds {
        format!(
            "{:02}:{:02}:{:02}",
            parts.tm_hour, parts.tm_min, parts.tm_sec
        )
    } else {
        format!("{:02}:{:02}", parts.tm_hour, parts.tm_min)
    }
}

pub fn selected_clock(selection: &Value) -> String {
    let Some(second) = selection["second"].as_u64() else {
        return "Waiting for readings".into();
    };
    let Some(parts) = local_parts(second) else {
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

fn local_parts(second: u64) -> Option<libc::tm> {
    let Ok(timestamp) = libc::time_t::try_from(second) else {
        return None;
    };
    let mut output = std::mem::MaybeUninit::<libc::tm>::uninit();
    // Both pointers are valid for the call; only a successful conversion initializes output.
    let result = unsafe { libc::localtime_r(&raw const timestamp, output.as_mut_ptr()) };
    if result.is_null() {
        return None;
    }
    // localtime_r returned the supplied initialized output pointer.
    Some(unsafe { output.assume_init() })
}

impl LiveDashboard {
    pub fn overview_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let selection = self
            .reading_snapshot()
            .map_or(&Value::Null, |snapshot| &snapshot.selection);
        let summary = crate::live_views::matching_summary(selection);
        let pressure = match summary["pressure"].as_u64() {
            Some(0) => "Normal",
            Some(1) => "Warning",
            Some(2) => "Critical",
            _ => "Unavailable",
        };
        let status = format!(
            "Memory pressure: {pressure}    Swap: {}",
            quantity(summary, "swap_used_bytes", "GiB", 1_073_741_824.0)
        );
        let status = div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(16.0))
            .text_size(px(12.0))
            .text_color(rgb(MUTED))
            .child(div().debug_selector(|| status.clone()).child(status))
            .child(
                control("Inspect processes →").on_click(cx.listener(|this, _, _, cx| {
                    this.navigate("cpu", cx);
                })),
            );
        div()
            .flex()
            .flex_col()
            .gap(px(8.0))
            .child(status)
            .child(overview_context(
                summary,
                self.viewport_size.width < px(640.0),
            ))
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

struct ContextCard {
    key: &'static str,
    title: &'static str,
}

struct ContextReading {
    key: &'static str,
    label: &'static str,
}

fn overview_context(summary: &Value, narrow: bool) -> gpui::Div {
    let thermal = overview_context_card(&ContextCard {
        key: "thermal",
        title: "Thermal state",
    })
    .child(overview_context_reading(
        &ContextReading {
            key: "thermal",
            label: "macOS verdict",
        },
        "Uncaptured".into(),
    ));
    let cooling = overview_context_card(&ContextCard {
        key: "cooling-battery",
        title: "Cooling and battery",
    })
    .child(overview_context_reading(
        &ContextReading {
            key: "soc",
            label: "SoC temperature",
        },
        quantity(summary, "soc_celsius", "°C", 1.0),
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
        quantity(&summary["battery"], "charge_percent", "%", 1.0),
    ));
    div()
        .flex()
        .gap(px(8.0))
        .when(narrow, gpui::Styled::flex_col)
        .child(thermal)
        .child(cooling)
}

fn overview_context_card(specification: &ContextCard) -> gpui::Div {
    let key = specification.key;
    div()
        .debug_selector(move || format!("overview-context:{key}"))
        .flex_1()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(8.0))
        .rounded(px(12.0))
        .border_1()
        .border_color(rgb(GRID))
        .bg(rgb(PANEL))
        .p(px(12.0))
        .text_color(rgb(TEXT))
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child(specification.title),
        )
}

fn overview_context_reading(specification: &ContextReading, reading: String) -> gpui::Div {
    let key = specification.key;
    let selector = format!("overview-context:{key}:{reading}");
    div()
        .debug_selector(move || selector.clone())
        .flex()
        .flex_wrap()
        .justify_between()
        .gap(px(8.0))
        .text_size(px(12.0))
        .child(div().text_color(rgb(MUTED)).child(specification.label))
        .child(reading)
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

    format!("{value:.0} rpm")
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
