use std::rc::Rc;

use gpui::{AnyElement, Context, IntoElement, div, prelude::*, px, rgb};

use crate::charts::{GRID, MUTED, PANEL};
use crate::data::Panel;
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
                    || "Resident App history unavailable at this observation".into(),
                    |history| history.coverage(None),
                );
            div()
                .debug_selector(|| "memory-attribution-unavailable".into())
                .flex_1()
                .min_w_0()
                .p(px(18.0))
                .rounded(px(7.0))
                .border_1()
                .border_color(rgb(GRID))
                .bg(rgb(PANEL))
                .child(crate::live_resident_history::TITLE)
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(MUTED))
                        .debug_selector(|| reading.clone())
                        .child(reading),
                )
                .into_any_element()
        };

        div()
            .child(
                div()
                    .flex()
                    .gap(px(8.0))
                    .when(self.reading_chart_columns() == 1, gpui::Styled::flex_col)
                    .child(chart)
                    .child(
                        div()
                            .debug_selector(|| "memory-attribution-ranking".into())
                            .flex_1()
                            .min_w_0()
                            .child(self.app_rankings(&[1], cx)),
                    ),
            )
            .child(self.memory_quantities(cx))
            .into_any_element()
    }
}
