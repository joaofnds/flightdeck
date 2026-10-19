use gpui::{AnyElement, Context, IntoElement, div, prelude::*, px, rgb};
use std::sync::Arc;

use crate::charts::{GRID, MUTED, PANEL};
use crate::live_app_attribution_history::{AppAttributionHistory, Metric};
use crate::live_dashboard::LiveDashboard;
use crate::live_source::Snapshot;

pub struct OverviewAppHistories {
    pub cpu: Arc<AppAttributionHistory>,
    pub power: Arc<AppAttributionHistory>,
    pub disk: Arc<AppAttributionHistory>,
}

impl OverviewAppHistories {
    pub fn history(&self, metric: Metric) -> &AppAttributionHistory {
        match metric {
            Metric::Cpu => self.cpu.as_ref(),
            Metric::Power => self.power.as_ref(),
            Metric::Disk => self.disk.as_ref(),
        }
    }
}

impl Snapshot {
    pub(crate) fn attribution_history(&self, metric: Metric) -> Option<&AppAttributionHistory> {
        if let Some(histories) = &self.overview_app_histories {
            return Some(histories.history(metric));
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
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .child(self.overview_app_histories_card(id, title, cx)),
            );
        }

        div()
            .child(cards)
            .when(slots.contains(&3), |row| {
                row.child(
                    div()
                        .debug_selector(|| {
                            "Per-App network and GPU histories are not captured.".into()
                        })
                        .mt(px(8.0))
                        .text_size(px(11.0))
                        .text_color(rgb(MUTED))
                        .child("Per-App network and GPU histories are not captured."),
                )
            })
            .into_any_element()
    }

    fn overview_app_histories_card(
        &self,
        id: &'static str,
        title: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(panel) = self.panels.iter().take(16).find(|panel| panel.id == id) {
            return self.live_chart(panel, cx).into_any_element();
        }

        let reading = self.overview_app_histories_unavailable(id);
        div()
            .debug_selector(move || format!("overview-app-history-unavailable:{id}"))
            .flex_1()
            .min_w_0()
            .p(px(18.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(rgb(GRID))
            .bg(rgb(PANEL))
            .child(title)
            .child(
                div()
                    .mt(px(8.0))
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .debug_selector(|| reading.clone())
                    .child(reading),
            )
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

pub(crate) fn overview_app_history_slot(slot: usize) -> (&'static str, &'static str) {
    match slot {
        0 => (Metric::Cpu.panel_id(), Metric::Cpu.title()),
        1 => (
            crate::live_resident_history::PANEL_ID,
            "Resident memory by App",
        ),
        2 => (Metric::Disk.panel_id(), Metric::Disk.title()),
        3 => (Metric::Power.panel_id(), Metric::Power.title()),
        _ => unreachable!("Overview App history has four fixed resource slots"),
    }
}
