use std::cell::Cell;
use std::rc::Rc;

use gpui::{Context, DispatchPhase, IntoElement, ListOffset, Pixels, ScrollWheelEvent, Styled, px};

use crate::live_dashboard::LiveDashboard;

/// The height GPUI's list gives a line of a wheel that scrolls by lines.
const LINE_HEIGHT: Pixels = px(20.0);

/// GPUI's list applies each wheel event to the position it last drew. A hover that replaces the
/// page's rows moves the list between two frames, and a wheel event before the next frame would
/// undo that move and read the old position against the new rows, which lands the page a row
/// away and puts another chart under the pointer. The hold keeps the distance of such a wheel
/// until the window has drawn the move, then scrolls by it from where the page is.
#[derive(Default)]
pub(crate) struct WheelHold {
    drawn_item: Cell<usize>,
    drawn_offset: Cell<Pixels>,
    drawn_rebuilds: Cell<u64>,
    rebuilds: Cell<u64>,
    held: Cell<Pixels>,
}

impl WheelHold {
    pub(crate) fn rows_rebuilt(&self) {
        self.rebuilds.set(self.rebuilds.get() + 1);
    }

    /// Records the list position the window draws, and hands back the distance held since the
    /// window last drew.
    fn drawn(&self, top: ListOffset) -> Pixels {
        self.drawn_item.set(top.item_ix);
        self.drawn_offset.set(top.offset_in_item);
        self.drawn_rebuilds.set(self.rebuilds.get());

        self.held.replace(px(0.0))
    }

    /// Whether the list moved or rebuilt its rows since the window last drew it.
    fn moved(&self, top: ListOffset) -> bool {
        if self.drawn_rebuilds.get() != self.rebuilds.get() {
            return true;
        }
        if self.drawn_item.get() != top.item_ix {
            return true;
        }

        self.drawn_offset.get() != top.offset_in_item
    }

    fn hold(&self, distance: Pixels) {
        self.held.set(self.held.get() + distance);
    }
}

impl LiveDashboard {
    /// Covers the page's list, takes the wheel while the list moved since the window drew it,
    /// and scrolls by what it held once the window has drawn again.
    pub(crate) fn wheel_hold(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let hold = Rc::clone(&self.wheel_hold);
        let list = self.list.clone();
        let view = cx.weak_entity();
        gpui::canvas(
            |_, _, _| {},
            move |bounds, (), window, cx| {
                let distance = hold.drawn(list.logical_scroll_top());
                if distance != px(0.0) {
                    let view = view.clone();
                    window.defer(cx, move |_, cx| {
                        match view.update(cx, |this, cx| this.wheel_released(distance, cx)) {
                            Ok(()) => {}
                            Err(error) => eprintln!("Wheel hold: {error}"),
                        }
                    });
                }

                let hold = Rc::clone(&hold);
                let list = list.clone();
                window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture {
                        return;
                    }
                    if !bounds.contains(&event.position) {
                        return;
                    }
                    if !hold.moved(list.logical_scroll_top()) {
                        return;
                    }
                    // A mostly sideways wheel belongs to the tables, which scroll sideways.
                    let delta = event.delta.pixel_delta(LINE_HEIGHT);
                    if delta.x.abs() > delta.y.abs() {
                        return;
                    }

                    hold.hold(-delta.y);
                    cx.stop_propagation();
                    window.refresh();
                });
            },
        )
        .absolute()
        .size_full()
    }

    fn wheel_released(&mut self, distance: Pixels, cx: &mut Context<Self>) {
        self.list.scroll_by(distance);
        self.reading_layout_scrolled();
        cx.notify();
    }
}
