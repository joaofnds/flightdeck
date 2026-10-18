use gpui::{Context, IntoElement, div, prelude::*, px, rgb};
use serde_json::Value;

use crate::charts::{MUTED, ORANGE};
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
        let summary = self
            .reading_snapshot()
            .map_or(&Value::Null, |snapshot| &snapshot.selection["summary"]);
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
        div()
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
            )
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
