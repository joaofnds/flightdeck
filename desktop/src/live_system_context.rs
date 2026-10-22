use std::rc::Rc;

use gpui::{Context, IntoElement, div, prelude::*};
use serde_json::Value;

use crate::data::Panel;
use crate::live_dashboard::LiveDashboard;
use crate::live_presentation::quantity_panel;

const SECONDS_PER_HOUR: u64 = 60 * 60;
const SECONDS_PER_DAY: u64 = 24 * SECONDS_PER_HOUR;

impl LiveDashboard {
    /// How long this Mac had run since it booted, at the selected second, above the system's
    /// open file entries, which the prototype folds into the same panel.
    pub(crate) fn system_context(
        &self,
        panel: Option<&Rc<Panel>>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let selection = self
            .reading_snapshot()
            .map_or(&Value::Null, |snapshot| &snapshot.selection);
        let summary = super::matching_summary(selection);
        let uptime = self.shown_reading(
            summary["uptime_seconds"]
                .as_u64()
                .map_or_else(|| "Unavailable".into(), uptime_text),
        );

        div()
            .flex()
            .flex_col()
            .child(quantity_panel(
                "system-context",
                "System context",
                "How long this Mac has run since it booted, at the selected time.",
                vec![("uptime", "Uptime", uptime)],
            ))
            .children(panel.map(|panel| div().min_w_0().flex().child(self.live_chart(panel, cx))))
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
