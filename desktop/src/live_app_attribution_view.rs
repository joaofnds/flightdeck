use std::rc::Rc;

use gpui::{AnyElement, Context, IntoElement, div, prelude::*, px};

use super::Metric;
use crate::data::Panel;
use crate::live_chart_view::AbsentChart;
use crate::live_dashboard::LiveDashboard;

impl LiveDashboard {
    pub(crate) fn chart_attribution_history(
        &self,
        panel: &Panel,
    ) -> Option<&super::AppAttributionHistory> {
        let metric = Metric::from_panel(&panel.id)?;

        self.snapshot.as_ref()?.attribution_history(metric)
    }

    /// GPU time by App beside its ranking, laid out as the recorded App charts are. Before the
    /// app has sampled two seconds there is no chart, and the card says why.
    pub(crate) fn gpu_apps(&self, panel: Option<&Rc<Panel>>, cx: &mut Context<Self>) -> AnyElement {
        let chart = match panel {
            Some(panel) => self.live_chart(panel, cx).into_any_element(),
            None => crate::live_chart_view::absent_chart(
                &AbsentChart {
                    id: crate::live_projection::GPU_APPS_ID,
                    title: "GPU time by app",
                },
                self.history_absent("GPU time by App is sampled only while the app runs"),
            )
            .debug_selector(|| "attribution-unavailable:live-gpu-apps".into())
            .into_any_element(),
        };

        div()
            .debug_selector(|| "attribution-row:live-gpu-apps".into())
            .flex()
            .gap(px(8.0))
            .when(self.reading_chart_columns() == 1, gpui::Styled::flex_col)
            .child(attribution_chart(self, chart))
            .child(
                div()
                    .debug_selector(|| "attribution-ranking:live-gpu-apps".into())
                    .flex()
                    .flex_basis(px(0.0))
                    .flex_grow(1.0)
                    .min_w_0()
                    .child(self.app_rankings(&[4], cx)),
            )
            .into_any_element()
    }

    /// Network traffic by App beside its ranking. The collector records no traffic per App, so
    /// the chart has no readings and the ranking says so.
    pub(crate) fn network_apps(
        &self,
        panel: Option<&Rc<Panel>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .debug_selector(|| "attribution-row:live-network-apps".into())
            .flex()
            .gap(px(8.0))
            .when(self.reading_chart_columns() == 1, gpui::Styled::flex_col)
            .child(attribution_chart(
                self,
                div()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .children(panel.map(|panel| self.live_chart(panel, cx)))
                    .into_any_element(),
            ))
            .child(
                div()
                    .debug_selector(|| "attribution-ranking:live-network-apps".into())
                    .flex()
                    .flex_basis(px(0.0))
                    .flex_grow(1.0)
                    .min_w_0()
                    .child(crate::live_rankings::view::network_ranking()),
            )
            .into_any_element()
    }

    pub(crate) fn app_attribution(
        &self,
        metric: Metric,
        panel: Option<&Rc<Panel>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chart = if let Some(panel) = panel {
            self.live_chart(panel, cx).into_any_element()
        } else {
            let reading = self
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.attribution_history(metric))
                .map_or_else(
                    || self.history_absent("App rate history unavailable at this observation"),
                    |history| history.coverage(None),
                );
            let chart = AbsentChart {
                id: metric.panel_id(),
                title: metric.title(),
            };
            crate::live_chart_view::absent_chart(&chart, reading)
                .debug_selector(|| format!("attribution-unavailable:{}", metric.panel_id()))
                .into_any_element()
        };
        let ranking = match metric {
            Metric::Cpu => 0,
            Metric::Power => 2,
            Metric::Disk => 3,
        };

        div()
            .debug_selector(|| format!("attribution-row:{}", metric.panel_id()))
            .flex()
            .gap(px(8.0))
            .when(self.reading_chart_columns() == 1, gpui::Styled::flex_col)
            .child(attribution_chart(self, chart))
            .child(
                div()
                    .debug_selector(|| format!("attribution-ranking:{}", metric.panel_id()))
                    .flex()
                    .flex_basis(px(0.0))
                    .flex_grow(1.0)
                    .min_w_0()
                    .child(self.app_rankings(&[ranking], cx)),
            )
            .into_any_element()
    }
}

/// The chart of an attribution row, twice as wide as its ranking beside it.
pub(crate) fn attribution_chart(dashboard: &LiveDashboard, chart: AnyElement) -> gpui::Div {
    div()
        .flex()
        .flex_basis(px(0.0))
        .flex_grow(if dashboard.reading_chart_columns() == 1 {
            1.0
        } else {
            2.0
        })
        .min_w_0()
        .child(chart)
}
