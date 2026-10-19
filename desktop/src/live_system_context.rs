use gpui::{IntoElement, div, prelude::*, px, rgb};
use serde_json::Value;

use crate::charts::{GRID, LINE, MUTED, PANEL, TEXT};
use crate::live_dashboard::LiveDashboard;

const SECONDS_PER_HOUR: u64 = 60 * 60;
const SECONDS_PER_DAY: u64 = 24 * SECONDS_PER_HOUR;

impl LiveDashboard {
    /// How long this Mac had run since it booted, at the selected second.
    pub(crate) fn system_context(&self) -> impl IntoElement {
        let selection = self
            .reading_snapshot()
            .map_or(&Value::Null, |snapshot| &snapshot.selection);
        let summary = super::matching_summary(selection);
        let uptime = summary["uptime_seconds"]
            .as_u64()
            .map_or_else(|| "Unavailable".into(), uptime_text);
        let selector = format!("system-context:Uptime:{uptime}");
        div()
            .debug_selector(|| "system-context".into())
            .flex()
            .flex_col()
            .px(px(12.0))
            .pt(px(8.0))
            .pb(px(10.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(PANEL))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.0))
                    .min_h(px(32.0))
                    .text_size(px(14.0))
                    .text_color(rgb(TEXT))
                    .child("System context")
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(rgb(MUTED))
                            .child("About"),
                    ),
            )
            .child(
                div()
                    .debug_selector(move || selector.clone())
                    .flex()
                    .justify_between()
                    .items_center()
                    .min_h(px(32.0))
                    .border_b_1()
                    .border_color(rgb(GRID))
                    .text_size(px(12.0))
                    .child(div().text_color(rgb(MUTED)).child("Uptime"))
                    .child(div().text_color(rgb(TEXT)).child(uptime)),
            )
    }
}

/// Uptime as the prototype reads it: minutes within the first hour, hours within the first
/// day, then days, each past the first to one decimal.
fn uptime_text(seconds: u64) -> String {
    if seconds < SECONDS_PER_HOUR {
        return format!("{} min", seconds / 60);
    }
    if seconds < SECONDS_PER_DAY {
        return format!(
            "{}.{} hours",
            seconds / SECONDS_PER_HOUR,
            tenths(seconds, SECONDS_PER_HOUR)
        );
    }
    format!(
        "{}.{} days",
        seconds / SECONDS_PER_DAY,
        tenths(seconds, SECONDS_PER_DAY)
    )
}

/// The first decimal of `seconds` in `unit`s, which floors.
fn tenths(seconds: u64, unit: u64) -> u64 {
    seconds % unit * 10 / unit
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uptime_text_reads_in_minutes_hours_then_days() {
        // Goal: uptime reads in the largest unit it has reached, to one decimal past a minute.
        // Method: one reading inside each unit and one on each boundary.
        let texts = [59, 3_599, 3_600, 5_400, 86_399, 86_400, 207_360].map(uptime_text);

        assert_eq!(
            texts,
            [
                "0 min",
                "59 min",
                "1.0 hours",
                "1.5 hours",
                "23.9 hours",
                "1.0 days",
                "2.4 days"
            ]
        );
    }
}
