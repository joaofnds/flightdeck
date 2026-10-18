use gpui::{AnyElement, IntoElement, SharedString, div, prelude::*, px, rgb};
use serde_json::Value;

use crate::charts::{GRID, MUTED, PANEL};
use crate::live_dashboard::LiveDashboard;

#[path = "live_resource_data.rs"]
mod data;

impl LiveDashboard {
    pub fn resource_summary(&self) -> AnyElement {
        let selection = self.resource_selection();
        let summary = data::summary(&self.request.route, selection);
        let mut content = div().flex().flex_col().gap(px(10.0));
        if let Some(notice) = summary.notice {
            content = content.child(
                div()
                    .text_color(rgb(MUTED))
                    .text_size(px(12.0))
                    .child(notice),
            );
        }
        content = content.child(fields(summary.fields));
        if self.request.route == "battery" {
            content = content.child(div().text_color(rgb(MUTED)).text_size(px(11.0)).child(
                "Battery flow is positive into the battery and negative while discharging. \
                 Health can exceed 100%. Adapter rating is capacity, separate from measured flow.",
            ));
        }
        content.into_any_element()
    }

    pub fn resource_device_count(&self) -> usize {
        data::devices(self.resource_selection()).len()
    }

    pub fn resource_device_layout_key(&self, index: usize) -> String {
        data::device_key(self.resource_selection(), index)
    }

    pub fn resource_device(&self, index: usize) -> AnyElement {
        let selection = self.resource_selection();
        let Some(device) = data::devices(selection).get(index) else {
            return div()
                .child("Physical disk unavailable at this observation")
                .into_any_element();
        };

        let title = data::device_name(device);
        div()
            .id(SharedString::from(data::device_key(selection, index)))
            .accessibility_id(format!("flightdeck.storage.device.{index}"))
            .role(gpui::Role::Group)
            .aria_label(format!("Physical disk {title}"))
            .p(px(16.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(rgb(GRID))
            .bg(rgb(PANEL))
            .child(
                div()
                    .debug_selector(|| format!("resource-device:{title}"))
                    .text_size(px(16.0))
                    .child(title),
            )
            .child(fields(data::device_fields(device)))
            .into_any_element()
    }

    fn resource_selection(&self) -> &Value {
        self.reading_snapshot()
            .map_or(&Value::Null, |snapshot| &snapshot.selection)
    }
}

fn fields(readings: Vec<data::Field>) -> impl IntoElement {
    div()
        .flex()
        .flex_wrap()
        .gap(px(10.0))
        .children(readings.into_iter().map(|field| {
            let selector = format!("resource-field:{}:{}", field.name, field.reading);

            div()
                .flex_1()
                .min_w(px(145.0))
                .p(px(14.0))
                .rounded(px(7.0))
                .border_1()
                .border_color(rgb(GRID))
                .bg(rgb(PANEL))
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(rgb(MUTED))
                        .child(field.name),
                )
                .child(
                    div()
                        .debug_selector(|| selector.clone())
                        .mt(px(6.0))
                        .text_size(px(20.0))
                        .child(field.reading),
                )
        }))
}
