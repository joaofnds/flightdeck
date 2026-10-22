use std::rc::Rc;

use gpui::{AnyElement, Context, IntoElement, div, prelude::*, px};

use crate::data::Panel;
use crate::live_chart_view::AbsentChart;
use crate::live_dashboard::LiveDashboard;

impl LiveDashboard {
    pub(crate) fn memory_attribution(
        &self,
        panel: Option<&Rc<Panel>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let chart = if let Some(panel) = panel {
            self.live_chart(panel, cx).into_any_element()
        } else {
            let reading = self
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.resident_history.as_ref())
                .map_or_else(
                    || self.history_absent("Resident App history unavailable at this observation"),
                    |history| history.coverage(None),
                );
            crate::live_chart_view::absent_chart(
                &AbsentChart {
                    id: crate::live_resident_history::PANEL_ID,
                    title: crate::live_resident_history::TITLE,
                },
                reading,
            )
            .debug_selector(|| "memory-attribution-unavailable".into())
            .into_any_element()
        };

        div()
            .flex()
            .gap(px(8.0))
            .when(self.reading_chart_columns() == 1, gpui::Styled::flex_col)
            .child(crate::live_app_attribution_history::view::attribution_chart(self, chart))
            .child(
                div()
                    .debug_selector(|| "memory-attribution-ranking".into())
                    .flex()
                    .flex_basis(px(0.0))
                    .flex_grow(1.0)
                    .min_w_0()
                    .child(self.app_rankings(&[1], cx)),
            )
            .into_any_element()
    }
}
