use std::rc::Rc;

use gpui::{AnyElement, Context, IntoElement, div, prelude::*, px, rgb};

use super::Metric;
use crate::charts::{GRID, MUTED, PANEL};
use crate::data::Panel;
use crate::live_dashboard::LiveDashboard;

impl LiveDashboard {
    pub(crate) fn chart_attribution_history(
        &self,
        panel: &Panel,
    ) -> Option<&super::AppAttributionHistory> {
        let metric = Metric::from_panel(&panel.id)?;

        self.snapshot.as_ref()?.attribution_history(metric)
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
                    || "App rate history unavailable at this observation".into(),
                    |history| history.coverage(None),
                );
            div()
                .debug_selector(|| format!("attribution-unavailable:{}", metric.panel_id()))
                .flex_1()
                .min_w_0()
                .p(px(18.0))
                .rounded(px(7.0))
                .border_1()
                .border_color(rgb(GRID))
                .bg(rgb(PANEL))
                .child(metric.title())
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(MUTED))
                        .debug_selector(|| reading.clone())
                        .child(reading),
                )
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
            .child(chart)
            .child(
                div()
                    .debug_selector(|| format!("attribution-ranking:{}", metric.panel_id()))
                    .flex_1()
                    .min_w_0()
                    .child(self.app_rankings(&[ranking], cx)),
            )
            .into_any_element()
    }
}
