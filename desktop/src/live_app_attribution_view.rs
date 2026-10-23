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
        self.attribution_history(Metric::from_panel(&panel.id)?)
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
            let reading = self.attribution_history(metric).map_or_else(
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
                    .child(self.app_rankings(&[metric.ranking_index()], cx)),
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
