use gpui::{Context, SharedString};

use super::{Origin, Preview, Target};
use crate::live_dashboard::{ChartHover, LiveDashboard};
use crate::live_source::{Snapshot, Update};

#[cfg(test)]
#[path = "live_hover_input_tests.rs"]
mod input_tests;

impl LiveDashboard {
    pub(crate) fn reading_header(&self, selected: &str) -> gpui::Div {
        use gpui::prelude::*;
        let status = match &self.preview {
            Some(preview) => match &preview.failure {
                Some(error) => format!("Hover reading unavailable: {error}"),
                None => {
                    if preview.complete {
                        "Hover preview".into()
                    } else {
                        "Hover preview · Loading exact details".into()
                    }
                }
            },
            None => self.status(),
        };
        let selected = selected.to_owned();
        gpui::div()
            .flex()
            .flex_wrap()
            .child(
                gpui::div()
                    .debug_selector(|| selected.clone())
                    .child(selected),
            )
            .child(" · ")
            .child(gpui::div().debug_selector(|| status.clone()).child(status))
    }

    pub(crate) fn set_reading_target(
        &mut self,
        source: SharedString,
        target: Target,
        origin: Origin,
        cx: &mut Context<Self>,
    ) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };

        let changed = !self.preview.as_ref().is_some_and(|preview| {
            preview.target == target && preview.base_request == self.request
        });
        let ownership_changed = !self
            .preview
            .as_ref()
            .is_some_and(|preview| preview.source == source && preview.origin == origin);
        if changed {
            let layout = self
                .preview
                .as_ref()
                .and_then(|preview| preview.layout.clone())
                .or_else(|| self.reading_layout(&source));
            let mut preview = Preview::resident(snapshot, &self.request, target);
            preview.layout = layout;
            preview.header_height = self
                .preview
                .as_ref()
                .and_then(|preview| preview.header_height)
                .or_else(|| Some(self.header_height.get()));
            self.preview = Some(preview);
        }
        if let Some(preview) = &mut self.preview {
            preview.source = source;
            preview.origin = origin;
            self.chart_hover = match origin {
                Origin::Pointer => Some(ChartHover {
                    second: preview.target.second,
                }),
                Origin::Keyboard => None,
            };
        }
        if changed {
            self.rebuild();
            cx.notify();
        } else {
            if ownership_changed {
                cx.notify();
            }
        }
    }

    pub(crate) fn clear_reading_target(&mut self, source: &SharedString, cx: &mut Context<Self>) {
        if self.reading_origin() == Some(Origin::Keyboard) {
            return;
        }

        if self.reading_target_source() == Some(source) {
            self.end_reading_preview(cx);
        }
    }

    pub(crate) fn end_reading_preview(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = self.preview.take() else {
            return;
        };

        self.chart_hover = None;
        match self.worker.request(self.request.clone()) {
            Ok(generation) => self.generation = generation,
            Err(error) => self.error = Some(error),
        }
        self.rebuild_anchored(preview.layout);
        cx.notify();
    }

    pub(crate) fn preview_tick(&mut self, cx: &mut Context<Self>) {
        self.preview_tick_at(std::time::Instant::now(), cx);
    }

    pub(crate) fn preview_tick_at(&mut self, now: std::time::Instant, cx: &mut Context<Self>) {
        let Some(preview) = &mut self.preview else {
            return;
        };

        if preview.base_request != self.request {
            self.end_reading_preview(cx);
            return;
        }

        if preview.complete {
            return;
        }

        if preview.generation.is_some() {
            return;
        }

        if preview.target.identity.is_none() {
            preview.complete = true;
            cx.notify();
            return;
        }

        if now < self.preview_ready_at {
            return;
        }

        match self.worker.request(preview.request.clone()) {
            Ok(generation) => {
                preview.generation = Some(generation);
                self.preview_submissions += 1;
                self.preview_ready_at = now + std::time::Duration::from_millis(250);
            }
            Err(error) => {
                preview.failure = Some(error);
                preview.complete = true;
            }
        }
        cx.notify();
    }

    pub(crate) fn install_cpu_focus(&mut self, window: &mut gpui::Window, cx: &mut Context<Self>) {
        if self.cpu_history_focus.is_some() {
            return;
        }

        let focus = cx.focus_handle();
        self.list
            .set_scroll_handler(cx.listener(|_, _, window, cx| {
                cx.defer_in(window, |this, _, _| this.reading_layout_scrolled());
            }));
        cx.on_blur(&focus, window, |this, _, cx| {
            if this.reading_origin() == Some(Origin::Keyboard) {
                if this.reading_target_source().map(SharedString::as_ref)
                    == Some("cpu-history-plot")
                {
                    this.cpu_inspection = None;
                    this.end_reading_preview(cx);
                }
            }
        })
        .detach();
        self.cpu_history_focus = Some(focus);
    }

    pub(crate) fn preview_receive(&mut self, update: Update) -> bool {
        let Some(preview) = &mut self.preview else {
            return false;
        };

        if preview.base_request != self.request {
            return false;
        }

        let generation = match &update {
            Update::Ready(snapshot) => snapshot.generation,
            Update::Failed { generation, .. } => *generation,
        };
        if preview.generation != Some(generation) {
            return false;
        }

        match update {
            Update::Ready(snapshot) => match preview_accept(preview, snapshot) {
                Ok(()) => {}
                Err(error) => preview.failure = Some(error),
            },
            Update::Failed { message, .. } => preview.failure = Some(message),
        }
        preview.complete = true;
        self.rebuild();

        true
    }
}

fn preview_accept(preview: &mut Preview, snapshot: Box<Snapshot>) -> Result<(), String> {
    if snapshot.selection["second"].as_u64() != Some(preview.target.second) {
        return Err("Hover response has a different timestamp".into());
    }

    if !preview.target.matches(&snapshot.selection["selected"]) {
        return Err("Hover response has a different observation".into());
    }

    if snapshot.catalog["path"] != preview.reading.catalog["path"] {
        return Err("Hover response belongs to another recording".into());
    }

    crate::live_acceptance::normalize_offsets(&mut preview.request, &snapshot)?;
    let executables = crate::live_executables::groups(snapshot.detail.as_ref())?;
    preview.executables = executables;
    preview.reading = snapshot;

    Ok(())
}
