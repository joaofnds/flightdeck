use gpui::{AnyElement, Context, IntoElement, div, prelude::*, px};
use std::sync::Arc;

use crate::live_app_attribution_history::{AppAttributionHistory, Metric};
use crate::live_chart_view::AbsentChart;
use crate::live_dashboard::LiveDashboard;
use crate::live_source::Snapshot;

pub struct OverviewAppHistories {
    pub cpu: Arc<AppAttributionHistory>,
    pub power: Arc<AppAttributionHistory>,
    pub disk: Arc<AppAttributionHistory>,
}

impl OverviewAppHistories {
    /// The Overview's history of `metric`. The Overview loads no GPU history, so its GPU slot
    /// names the GPU page, which draws it.
    pub fn history(&self, metric: Metric) -> Option<&AppAttributionHistory> {
        match metric {
            Metric::Cpu => Some(self.cpu.as_ref()),
            Metric::Power => Some(self.power.as_ref()),
            Metric::Disk => Some(self.disk.as_ref()),
            Metric::Gpu => None,
        }
    }
}

impl Snapshot {
    pub(crate) fn attribution_history(&self, metric: Metric) -> Option<&AppAttributionHistory> {
        if let Some(histories) = &self.overview_app_histories {
            return histories.history(metric);
        }

        self.app_attribution_history.as_deref()
    }
}

impl LiveDashboard {
    pub(crate) fn overview_app_histories(
        &self,
        slots: &[usize],
        cx: &mut Context<Self>,
    ) -> AnyElement {
        assert!(!slots.is_empty());
        assert!(slots.len() <= 2);
        let mut cards = div()
            .debug_selector(|| "overview-app-history-cards".into())
            .flex()
            .gap(px(8.0));
        for slot in slots {
            let (id, title) = overview_app_history_slot(*slot);
            cards = cards.child(
                div()
                    .debug_selector(move || format!("overview-app-history:{id}"))
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .child(self.overview_app_histories_card(id, title, cx)),
            );
        }

        cards.into_any_element()
    }

    fn overview_app_histories_card(
        &self,
        id: &'static str,
        title: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(panel) = self.panels.iter().take(24).find(|panel| panel.id == id) {
            return self.live_chart(panel, cx).into_any_element();
        }

        let reading = if id == NETWORK_APPS_ID {
            "Not recorded per app".to_owned()
        } else {
            if id == Metric::Gpu.panel_id() {
                "Drawn on the GPU page".to_owned()
            } else {
                self.overview_app_histories_unavailable(id)
            }
        };
        crate::live_chart_view::absent_chart(&AbsentChart { id, title }, reading)
            .debug_selector(move || format!("overview-app-history-unavailable:{id}"))
            .into_any_element()
    }

    fn overview_app_histories_unavailable(&self, id: &str) -> String {
        if let Some(snapshot) = self.snapshot.as_deref() {
            if let Some(metric) = Metric::from_panel(id) {
                if let Some(history) = snapshot.attribution_history(metric) {
                    return history.coverage(history.index(self.reading_target()));
                }
            }
            if id == crate::live_resident_history::PANEL_ID {
                if let Some(history) = &snapshot.resident_history {
                    return history.coverage(history.index(self.reading_target()));
                }
            }
        }

        self.history_absent("App history unavailable at this observation")
    }
}

/// The prototype's six histories in its order, each as the panel that draws it and the title
/// it carries.
pub(crate) const OVERVIEW_APP_HISTORY_SLOTS: usize = 6;

pub(crate) fn overview_app_history_slot(slot: usize) -> (&'static str, &'static str) {
    match slot {
        0 => (Metric::Cpu.panel_id(), Metric::Cpu.title()),
        1 => (crate::live_resident_history::PANEL_ID, "Memory by app"),
        2 => (NETWORK_APPS_ID, "Network by app"),
        3 => (Metric::Disk.panel_id(), "Disk by app"),
        4 => (Metric::Gpu.panel_id(), "GPU by app"),
        5 => (Metric::Power.panel_id(), "Energy by app"),
        _ => unreachable!("Overview App history has six fixed resource slots"),
    }
}

/// The title the Overview gives the panel `id`, which the page that owns the panel may name
/// otherwise.
pub(crate) fn overview_app_history_title(id: &str) -> Option<&'static str> {
    (0..OVERVIEW_APP_HISTORY_SLOTS)
        .map(overview_app_history_slot)
        .find(|(slot, _)| *slot == id)
        .map(|(_, title)| title)
}

/// The collector does not record network traffic per app, so no panel carries this id.
const NETWORK_APPS_ID: &str = "live-network-apps";
