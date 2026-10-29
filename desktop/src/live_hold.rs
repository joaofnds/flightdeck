//! A second of the live Mac the page holds. The page keeps the readings and the charts it showed
//! when the second was held, which keep the window they were drawn in, and shows the held second
//! over them from a reading of its own until the user lets go.

use gpui::Context;

use super::LiveDashboard;
use crate::live_hover::{HeldSecond, Origin, Preview, Target};
use crate::live_source::{Request, Update};

/// The held second's reading, and where the charts were drawn when it was held.
pub(crate) struct Hold {
    /// The last second of the window the charts were drawn in when the second was held, which
    /// no window passes while the hold lasts.
    pub(crate) last: u64,
    /// The second the range is laid against while the hold lasts: the held second when the
    /// hold began or a range was picked, which a later click or step inside the window leaves
    /// as it was, so neither moves the window.
    pub(crate) anchor_second: u64,
    /// The held second's readings, which the page shows wherever no hover shows another's.
    pub(crate) reading: Preview,
}

impl LiveDashboard {
    /// Holds `target` over the readings and the charts the page shows, whose window ends at
    /// `last`. The page forgets the request it had out, whose reply would move the charts, and
    /// takes the readings of `target` from the hover of it, or from what it holds. A second held
    /// while another is keeps the window that one showed.
    pub(super) fn hold_target(&mut self, target: Target, last: u64, cx: &mut Context<Self>) {
        assert!(target.second <= last);
        let anchor_second = self
            .hold
            .as_ref()
            .map_or(target.second, |hold| hold.anchor_second);
        // The page stops following the store's View once its request names the second.
        let in_view = self.store_view_held(&target);
        self.inspect_target_request(target.clone());
        self.generation = 0;
        self.reply_pending = false;
        let hovered = self.preview.take();
        let fresh = self.hold_resident(target, in_view);
        let (reading, read) = match hovered {
            Some(hovered) => {
                self.hover_ended(&hovered);
                hold_adopt(hovered, fresh, &self.request)
            }
            None => (fresh, false),
        };

        self.hold = Some(Hold {
            last,
            anchor_second,
            reading,
        });
        if !read {
            self.hold_first_read();
        }
        self.rebuild();
        cx.notify();
    }

    /// The last second of the window the charts are drawn in, which a hold keeps, where the page
    /// shows readings of the live Mac, its own or, before the worker's first reading, the store's
    /// View's.
    pub(super) fn hold_last(&self) -> Option<u64> {
        if self.worker.mode() != crate::live_source_mode::SourceMode::Live {
            return None;
        }

        Some(self.range_anchor()?.context_last)
    }

    /// Stops the reader following the live Mac with the hold's first read: the page at the
    /// hold's last second where the page has no readings yet, its App histories are pending or
    /// the held second names no reading, since that read brings them, or where the reader
    /// rests, which reads a second's own read as a newly opened page, and otherwise the held
    /// second's own. A rested reader follows nothing, so a hold the View completes sends it
    /// nothing.
    fn hold_first_read(&mut self) {
        let Some(hold) = &self.hold else {
            unreachable!("the hold's first read follows the hold");
        };
        let pending = self
            .snapshot
            .as_ref()
            .is_none_or(|snapshot| snapshot.histories_pending);
        let rested = self.reader_rested();

        if rested && hold.reading.complete {
            return;
        }
        if pending || rested || hold.reading.target.identity.is_none() {
            self.request_reading();
        } else {
            self.hold_read();
        }
    }

    /// Asks the reader for the held second's readings.
    fn hold_read(&mut self) {
        let Some(hold) = &mut self.hold else {
            unreachable!("only a hold reads its second");
        };

        match self.worker.inspect(hold.reading.request.clone()) {
            Ok(generation) => hold.reading.generation = Some(generation),
            Err(error) => {
                hold.reading.failure = Some(error);
                hold.reading.complete = true;
            }
        }
    }

    /// The held second's reading as the page's readings hold it, and as `in_view`, the store's
    /// View, holds it.
    fn hold_resident(&self, target: Target, in_view: Option<HeldSecond>) -> Preview {
        // Before the worker's first reading the page draws only the store's View, or what it kept
        // of one it let go of, so the held second's reading holds only the View's record of it
        // until that reading lands.
        let unread = hold_resident_unread();
        let snapshot = self.snapshot.as_deref().unwrap_or(&unread);

        let mut reading = self.resident_preview(snapshot, target, in_view);
        reading.source = crate::live_chart_view::SELECTION_SOURCE.into();
        reading.origin = Origin::Keyboard;
        reading
    }

    /// Reads the held second where its reading lacks what the page shows and no read of the
    /// page or of a hover's second is out, either of which a read of its own would cancel.
    pub(super) fn hold_tick(&mut self) {
        if self.preview.is_some() {
            return;
        }
        if self.reply_pending {
            return;
        }
        let Some(hold) = &mut self.hold else {
            return;
        };
        if hold.reading.complete {
            return;
        }
        if hold.reading.generation.is_some() {
            return;
        }
        if self
            .snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.histories_pending)
        {
            return;
        }

        if hold.reading.target.identity.is_none() {
            hold.reading.complete = true;
            return;
        }
        self.hold_read();
    }

    /// Whether `update` answers the hold's read of its second.
    pub(super) fn hold_answered(&self, update: &Update) -> bool {
        self.hold
            .as_ref()
            .is_some_and(|hold| hold.reading.generation == Some(update.generation()))
    }

    /// Takes the reader's answer to the hold's read of its second, and answers that the page
    /// changed.
    pub(super) fn hold_receive(&mut self, update: Update) -> bool {
        let Some(hold) = &mut self.hold else {
            unreachable!("only a hold takes the reply to its read");
        };

        crate::live_hover::controller::preview_take_reply(&mut hold.reading, update);
        self.rebuild();

        true
    }

    /// Shows the held second again from the page's new readings, which replaced those it was
    /// held over, and names its reading from them where a hold made before the page had
    /// readings names none, since a reading of no name matches none they hold. A read of it
    /// follows where they lack what the page shows of it.
    pub(super) fn hold_resolve(&mut self) {
        let Some(hold) = &self.hold else {
            return;
        };

        let mut target = hold.reading.target.clone();
        if target.identity.is_none() {
            target = self.inspection_target(target.second);
            assert!(target.second <= hold.last);
            self.request.second = Some(target.second);
            self.request.identity.clone_from(&target.identity);
        }
        let in_view = self.store_view_held(&target);
        let reading = self.hold_resident(target, in_view);
        if let Some(hold) = &mut self.hold {
            hold.reading = reading;
        }
        self.rebuild();
    }

    /// Finds the held second again in the store's View as the page just opened draws it, since a
    /// second held on a page that draws nothing from the View holds no record of it, and
    /// Overview shows that record at once while the page's own readings are read. The reply to
    /// that read builds the rest of the hold's reading again.
    pub(super) fn switch_route_hold(&mut self) {
        let Some(hold) = &self.hold else {
            return;
        };

        let in_view = self.store_view_held(&hold.reading.target);
        if let Some(hold) = &mut self.hold {
            hold.reading.held = in_view;
        }
    }

    /// Makes `target` the request's reading, as a link that names it does. While a second is
    /// held, the page holds `target` in its place over the readings and the charts it shows,
    /// from the hover of it where one is shown, and the page's next read brings what that reading
    /// lacks.
    pub(crate) fn link_target(&mut self, target: &Target) {
        self.request.second = Some(target.second);
        self.request.identity.clone_from(&target.identity);
        let Some(hold) = &self.hold else {
            return;
        };
        if hold.reading.target == *target {
            return;
        }

        assert!(target.second <= hold.last);
        let in_view = self.store_view_held(target);
        let fresh = self.hold_resident(target.clone(), in_view);
        let reading = match self.preview.take() {
            Some(hovered) => {
                self.hover_ended(&hovered);
                hold_adopt(hovered, fresh, &self.request).0
            }
            None => fresh,
        };
        if let Some(hold) = &mut self.hold {
            hold.reading = reading;
        }
    }

    /// The request that reads the page: while a second is held, the page whose context ends at
    /// the hold's last second, naming no reading, so no read of the page lays the window around
    /// the held second or past where the charts were drawn.
    pub(super) fn page_request(&self) -> Request {
        let Some(hold) = &self.hold else {
            return self.request.clone();
        };

        // The reader ends a selected second's context this far past it, or at the newest second
        // where that comes first, which stays past the hold's last second as the Mac runs on.
        let second = hold.last.saturating_sub(viewer::CONTEXT_AFTER_SECONDS);
        Request {
            second: Some(second),
            identity: None,
            ..self.request.clone()
        }
    }

    /// The reading the page shows of a second its readings do not show: a hover's, or else the
    /// held second's.
    pub(crate) fn shown_preview(&self) -> Option<&Preview> {
        self.preview
            .as_ref()
            .or(self.hold.as_ref().map(|hold| &hold.reading))
    }

    /// Whether the page holds a second of the live Mac.
    pub(crate) fn holds_second(&self) -> bool {
        self.hold.is_some()
    }

    /// Lets go of the held second and of the store's View the hold kept parked, keeping a record
    /// of that View's newest second for the page to draw from until the store's next View. Every
    /// way out of a hold that stays on the live Mac lets go here, since a View the page keeps
    /// pins the buffers the Store needs to catch up.
    pub(super) fn hold_let_go(&mut self) {
        if self.hold.take().is_none() {
            return;
        }

        self.store_view_let_go();
    }

    /// Lets go of the held second and of every View the page kept, as opening a recording does,
    /// since that page draws nothing from the store.
    pub(crate) fn hold_drop(&mut self) {
        self.hold = None;
        self.store_view_drop();
    }

    /// The generation of the hold's read of its second, while one is out or was answered.
    #[cfg(test)]
    pub(crate) fn held_read_generation(&self) -> Option<u64> {
        self.hold.as_ref()?.reading.generation
    }

    /// Whether the reader still reads the held second, which a read of any other would cancel.
    pub(crate) fn hold_reading(&self) -> bool {
        self.hold
            .as_ref()
            .is_some_and(|hold| hold.reading.generation.is_some() && !hold.reading.complete)
    }
}

/// The held second's reading: the hover's of it, where the hover reads what a read of the held
/// second reads, and otherwise `fresh`. It keeps `fresh`'s record of the second in the View the
/// page draws, since a View that landed while the pointer stayed still leaves the hover's out of
/// date. Answers whether the hover asked the reader for it, which stopped the reader following
/// the live Mac.
fn hold_adopt(hovered: Preview, fresh: Preview, request: &Request) -> (Preview, bool) {
    if hovered.target != fresh.target {
        return (fresh, false);
    }
    if hovered.request != fresh.request {
        return (fresh, false);
    }

    let read = hovered.generation.is_some();
    let reading = Preview {
        source: fresh.source,
        origin: fresh.origin,
        held: fresh.held,
        base_request: request.clone(),
        layout: None,
        header_height: None,
        page_reply_kept: false,
        hovered_at: None,
        ..hovered
    };
    (reading, read)
}

/// The page's readings before the worker's first reading, which hold none.
fn hold_resident_unread() -> crate::live_source::Snapshot {
    crate::live_source::Snapshot {
        generation: 0,
        catalog: serde_json::Value::Null,
        selection: crate::live_source::Presentation::Legacy(serde_json::Value::Null),
        pages: std::collections::BTreeMap::new(),
        apps: None,
        detail: None,
        history: None,
        cpu_detail: None,
        cpu_history: None,
        resident_history: None,
        app_attribution_history: None,
        overview_app_histories: None,
        process_history: None,
        rankings: None,
        app_ranks: None,
        app_parts: None,
        histories_pending: false,
        read_ms: 0.0,
    }
}
