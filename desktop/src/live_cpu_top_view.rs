use std::rc::Rc;

use gpui::{Context, IntoElement, div, prelude::*, px};

use crate::data::Panel;
use crate::live_dashboard::LiveDashboard;

impl LiveDashboard {
    pub(super) fn cpu_top(
        &self,
        panel: Option<&Rc<Panel>>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let wide = self.viewport_size.width >= px(1100.0);
        div()
            .debug_selector(|| "cpu-top".into())
            .flex()
            .gap(px(8.0))
            .when(!wide, gpui::Div::flex_col)
            .child(
                cpu_top_part("cpu-activity-history-panel", 1.35, wide).child(match panel {
                    Some(panel) => self.live_chart(panel, cx).into_any_element(),
                    None => div()
                        .child("CPU activity history unavailable.")
                        .into_any_element(),
                }),
            )
            .child(cpu_top_part("cpu-selected-part", 0.85, wide).child(self.cpu_activity()))
            .child(cpu_top_part("cpu-heatmap-part", 1.6, wide).child(self.cpu_history_panel(cx)))
    }
}

fn cpu_top_part(selector: &'static str, ratio: f32, wide: bool) -> gpui::Div {
    div()
        .debug_selector(move || selector.into())
        .flex()
        .flex_col()
        .min_w_0()
        .when(wide, |part| part.flex_basis(px(0.0)).flex_grow(ratio))
}
