use std::time::Instant;

use gpui::{Context, SharedString};

use super::timings::{HoverTiming, HoverTimings};
use super::{Origin, Preview, Target};
use crate::live_dashboard::{ChartHover, LiveDashboard, Picker};
use crate::live_source::{Snapshot, Update};

#[cfg(test)]
#[path = "live_hover_input_tests.rs"]
mod input_tests;

/// Where the pointer was last seen. GPUI keeps the last pointer position after the pointer
/// leaves the window, so a hover listener must not trust a hit test until the pointer moves
/// inside the window again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PointerPlace {
    InsideWindow,
    OutsideWindow,
}

impl LiveDashboard {
    /// Shows the readings of the second the pointer hovers, and times the hover from
    /// `hovered_at`, when the app began handling what put the pointer over that second.
    pub(crate) fn hover_reading_target(
        &mut self,
        source: SharedString,
        target: Target,
        hovered_at: Instant,
        cx: &mut Context<Self>,
    ) {
        if !self.set_reading_target(source, target, Origin::Pointer, cx) {
            return;
        }

        if let Some(preview) = &mut self.preview {
            preview.hovered_at = Some(hovered_at);
        }
        self.hover_timed();
    }

    /// Shows the readings of `target`, and answers whether the target changed.
    pub(crate) fn set_reading_target(
        &mut self,
        source: SharedString,
        target: Target,
        origin: Origin,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.snapshot.is_none() {
            return false;
        }

        let changed = !self.preview.as_ref().is_some_and(|preview| {
            preview.target == target && preview.base_request == self.request
        });
        let ownership_changed = !self
            .preview
            .as_ref()
            .is_some_and(|preview| preview.source == source && preview.origin == origin);
        if changed {
            if let Some(replaced) = &self.preview {
                hover_unfinished(&mut self.hover_timings, replaced);
            }
            self.preview_retarget(&source, target);
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
        changed
    }

    /// Ends the hover's timing once the page holds its second's complete readings, or once the
    /// read of them failed.
    pub(crate) fn hover_timed(&mut self) {
        let Some(preview) = &mut self.preview else {
            return;
        };
        if !preview.complete {
            return;
        }
        let Some(hovered_at) = preview.hovered_at.take() else {
            return;
        };

        let complete = if preview.failure.is_some() {
            None
        } else {
            Some(hovered_at.elapsed())
        };
        self.hover_timings.push(HoverTiming {
            route: preview.base_request.route.clone(),
            second: preview.target.second,
            complete,
        });
    }

    pub(crate) fn clear_reading_target(&mut self, source: &SharedString, cx: &mut Context<Self>) {
        if self.reading_origin() == Some(Origin::Keyboard) {
            return;
        }

        if self.reading_target_source() == Some(source) {
            self.end_reading_preview(cx);
        }
    }

    /// Ends a pointer hover when the pointer leaves the window. GPUI keeps the last pointer
    /// position, so until the pointer moves again the next frame would hover the plot under
    /// that stale position.
    pub(crate) fn pointer_left_window(&mut self, cx: &mut Context<Self>) {
        self.pointer = PointerPlace::OutsideWindow;
        if self.reading_origin() == Some(Origin::Pointer) {
            self.end_reading_preview(cx);
        }
    }

    pub(crate) fn end_reading_preview(&mut self, cx: &mut Context<Self>) {
        let Some(preview) = self.preview.take() else {
            return;
        };

        self.hover_ended(&preview);
        // The page holds the reply to its request unless the hover kept that reply from it. A
        // page that holds a second reads nothing, and shows the held second again.
        if preview.page_reply_kept && self.hold.is_none() {
            self.request_reading();
        }
        // The page's readings still show the second it showed before it selected another.
        if self.selection_reading() {
            if self.show_held_selection(cx) {
                return;
            }
        }
        self.rebuild_anchored(preview.layout);
        cx.notify();
    }

    /// Takes the cursor of `preview`, which the page no longer shows, off the charts, records
    /// its hover as ended before the page held its readings where its timing still runs, and
    /// draws the charts from the store's newest View, which the hover held them back from.
    pub(crate) fn hover_ended(&mut self, preview: &Preview) {
        hover_unfinished(&mut self.hover_timings, preview);
        self.chart_hover = None;
        self.store_view_resume();
    }

    /// Whether a hover still waits on a read of its second, whether that read is out or waits
    /// to be sent.
    pub(crate) fn preview_awaiting_reply(&self) -> bool {
        self.preview
            .as_ref()
            .is_some_and(|preview| !preview.complete)
    }

    pub(crate) fn preview_tick(&mut self, cx: &mut Context<Self>) {
        let selection_reading = self.selection_reading();
        let reader_rested = self.reader_rested();
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
            self.hover_timed();
            cx.notify();
            return;
        }

        // A hover's read would cancel the page's read of its App histories, so the hover waits
        // for that read and shows the hovered second from what it brings.
        if let Some(snapshot) = &self.snapshot {
            if snapshot.histories_pending {
                return;
            }
        }

        // A hover's read would cancel the page's read of its selection, whose reply shows the
        // hovered second again from what it brings.
        if selection_reading {
            return;
        }

        // A rested reader would read the hovered second as a newly opened page, so the hover
        // asks for the page first and waits for it.
        if reader_rested {
            if !self.reply_pending {
                self.request_reading();
            }
            return;
        }

        match self.worker.inspect(preview.request.clone()) {
            Ok(generation) => {
                preview.generation = Some(generation);
                self.preview_submissions += 1;
                preview.page_reply_kept = true;
            }
            Err(error) => {
                preview.failure = Some(error);
                preview.complete = true;
            }
        }
        self.hover_timed();
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

        let generation = match &update {
            Update::Ready(snapshot) => snapshot.generation,
            Update::Failed { generation, .. } => *generation,
        };
        if preview.base_request != self.request {
            if generation == self.generation {
                preview.page_reply_kept = true;
            }
            return false;
        }

        if preview.generation != Some(generation) {
            if generation != self.generation {
                return false;
            }
            if self.preview_receive_page(update) {
                return true;
            }
            if let Some(preview) = &mut self.preview {
                preview.page_reply_kept = true;
            }
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
        self.hover_timed();

        true
    }

    /// Takes a reply to the page's request that the page waits on during a hover: its read of
    /// its App histories, which the hover waited for, or the second the page selected, which the
    /// hover read nothing in place of. The hover then shows its second again from what the
    /// reply brings. A keyboard's ends where the reply shows that second itself or holds it no
    /// longer. Every other reading of the page waits for the hover to end, so the charts hold
    /// still under the pointer.
    fn preview_receive_page(&mut self, update: Update) -> bool {
        if !self.preview_waits_on_page() {
            return false;
        }

        if !self.receive(update) {
            return false;
        }

        if let Some(previous) = self.preview.take() {
            if self.preview_receive_page_ends(&previous) {
                self.hover_ended(&previous);
            } else {
                self.preview = Some(previous);
                self.preview_reresolve();
            }
        }
        self.rebuild();
        self.hover_timed();

        true
    }

    /// Whether the hover lets the page take its next reply: the page's App histories, or the
    /// second the page selected.
    fn preview_waits_on_page(&self) -> bool {
        if let Some(committed) = &self.snapshot {
            if committed.histories_pending {
                return true;
            }
        }
        // The hover asked the rested reader for the page before its own read.
        if self.reader_rested() {
            return true;
        }

        self.selection_reading()
    }

    /// Whether the keyboard's `preview` ends with the page's new readings, which show its second
    /// themselves or no longer hold it.
    fn preview_receive_page_ends(&self, preview: &Preview) -> bool {
        if preview.origin != Origin::Keyboard {
            return false;
        }

        let Some(committed) = self.snapshot.as_deref() else {
            return true;
        };
        if preview.target.matches(&committed.selection["selected"]) {
            return true;
        }

        !crate::live_projection::holds_second(&committed.selection, preview.target.second)
    }

    /// Shows the hovered second again from the page's readings, keeping who shows it.
    fn preview_reresolve(&mut self) {
        let Some(previous) = &self.preview else {
            return;
        };

        let source = previous.source.clone();
        let origin = previous.origin;
        let hovered_at = previous.hovered_at;
        self.preview_retarget(&source, previous.target.clone());
        if let Some(preview) = &mut self.preview {
            preview.source = source;
            preview.origin = origin;
            preview.hovered_at = hovered_at;
        }
    }

    /// Resolves the hover at `target` from the committed snapshot, holding the hovered plot
    /// where the hover before it held it.
    fn preview_retarget(&mut self, source: &SharedString, target: Target) {
        let Some(snapshot) = self.snapshot.as_deref() else {
            return;
        };

        let layout = self
            .preview
            .as_ref()
            .and_then(|preview| preview.layout.clone())
            .or_else(|| self.reading_layout(source));
        let header_height = self
            .preview
            .as_ref()
            .and_then(|preview| preview.header_height)
            .or_else(|| Some(self.header_height.get()));
        let page_reply_kept = self
            .preview
            .as_ref()
            .is_some_and(|preview| preview.page_reply_kept);
        let mut preview = Preview::resident(snapshot, &self.request, target);
        preview.held = self.store_view_held(&preview.target);
        // The View holds the hovered second's readings and top Apps, which is all the page shows
        // of it, so the hover reads nothing.
        if let Some(held) = &preview.held {
            if held.rankings.is_some() {
                preview.complete = true;
            }
        }
        preview.page_reply_kept = page_reply_kept;
        if self.picker == Some(Picker::App) {
            preview.list_apps(snapshot);
        }
        preview.layout = layout;
        preview.header_height = header_height;
        self.preview = Some(preview);
    }
}

/// Records the hover of `preview` as ended before the page held its readings, where its timing
/// still runs.
fn hover_unfinished(timings: &mut HoverTimings, preview: &Preview) {
    if preview.hovered_at.is_none() {
        return;
    }

    timings.push(HoverTiming {
        route: preview.base_request.route.clone(),
        second: preview.target.second,
        complete: None,
    });
}

pub(crate) fn preview_accept(
    preview: &mut Preview,
    mut snapshot: Box<Snapshot>,
) -> Result<(), String> {
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
    // Exact details omit context; the committed group remains the inspection source.
    snapshot
        .process_history
        .clone_from(&preview.reading.process_history);
    snapshot
        .resident_history
        .clone_from(&preview.reading.resident_history);
    snapshot
        .overview_app_histories
        .clone_from(&preview.reading.overview_app_histories);
    let executables = crate::live_executables::groups(snapshot.detail.as_deref())?;
    preview.executables = executables;
    preview.reading = snapshot;

    Ok(())
}
