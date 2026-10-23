use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, Context, Entity, IntoElement, Pixels, Render, StyleRefinement, Window,
    prelude::*,
};

use crate::chart_axes::TimeWindow;
use crate::charts::{self, HeldCurves, Inspection};
use crate::data::Panel;

/// The most charts whose plots a page holds, well above the charts of every page together.
const PLOTS_MAX: usize = 4096;

/// What a chart's plot shows. A plot that shows the same as in the last frame keeps the
/// primitives GPUI drew for it then, so a frame that changes nothing a chart shows, such as a
/// frame of the fan animation, draws no chart again.
pub(crate) struct Shown {
    panel: Rc<Panel>,
    chart_width: Option<f32>,
    inspection: Inspection,
    hidden: Rc<Vec<String>>,
    time: Option<TimeWindow>,
}

impl Shown {
    pub(crate) fn new(
        panel: Rc<Panel>,
        inspection: Inspection,
        hidden: Rc<Vec<String>>,
        time: Option<TimeWindow>,
    ) -> Self {
        Self {
            chart_width: panel.chart_width.get(),
            inspection: charts::plot_inspection(&panel, inspection),
            panel,
            hidden,
            time,
        }
    }

    /// Whether the plot of `self` looks as the plot of `other` does. Panels compare by address:
    /// the page builds a new panel for new readings, and `Rc::make_mut` copies a panel a plot
    /// holds rather than changing it in place. The width of the card is the panel's one value
    /// that changes in place, so each holds the width it was made with.
    fn same(&self, other: &Self) -> bool {
        if !Rc::ptr_eq(&self.panel, &other.panel) {
            return false;
        }

        let looks = (self.chart_width, self.inspection, &self.hidden, self.time);
        looks
            == (
                other.chart_width,
                other.inspection,
                &other.hidden,
                other.time,
            )
    }
}

/// One chart's plot, which GPUI draws again only when its view is new or its bounds change.
/// The plot that replaces it keeps its curves, so a chart drawn again for a slide of its window
/// moves the curves it drew rather than tessellating them again.
pub(crate) struct Plot {
    shown: Shown,
    measured: Rc<Cell<Bounds<Pixels>>>,
    curves: Rc<HeldCurves>,
    renders: Rc<Cell<u64>>,
}

impl Render for Plot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        assert_eq!(self.shown.panel.chart_width.get(), self.shown.chart_width);
        self.renders.set(self.renders.get() + 1);

        charts::plot_without_cursor(
            Rc::clone(&self.shown.panel),
            self.shown.inspection,
            Rc::clone(&self.shown.hidden),
            Rc::clone(&self.measured),
            self.shown.time,
            Some(Rc::clone(&self.curves)),
        )
    }
}

/// The plots of the charts the page drew in the last frame, by panel id. A plot holds its
/// panel's readings, so the page lets go of a plot no frame drew.
pub(crate) struct Plots {
    views: RefCell<BTreeMap<String, Held>>,
    renders: Rc<Cell<u64>>,
    curve_builds: Rc<Cell<u64>>,
}

struct Held {
    view: Entity<Plot>,
    drawn: bool,
}

impl Plots {
    pub(crate) fn new() -> Self {
        Self {
            views: RefCell::new(BTreeMap::new()),
            renders: Rc::new(Cell::new(0)),
            curve_builds: Rc::new(Cell::new(0)),
        }
    }

    /// How many times a plot has rendered, which the page's reports give.
    pub(crate) fn renders(&self) -> u64 {
        self.renders.get()
    }

    /// How many curves the plots have tessellated, which the page's reports give.
    pub(crate) fn curve_builds(&self) -> u64 {
        self.curve_builds.get()
    }

    /// How many plots the page holds.
    pub(crate) fn held(&self) -> usize {
        self.views.borrow().len()
    }

    /// Lets go of the plots the last frame did not draw, before the page draws the next.
    pub(crate) fn begin_frame(&self) {
        let mut views = self.views.borrow_mut();
        views.retain(|_, held| held.drawn);
        for held in views.values_mut() {
            held.drawn = false;
        }
    }

    /// The plot that shows `shown`, and where it was laid out. The plot is the last frame's
    /// when it showed the same, and a new view otherwise, since a view notified while the
    /// window draws is drawn again only in the next frame.
    pub(crate) fn plot(
        &self,
        shown: Shown,
        cx: &mut App,
    ) -> (AnyElement, Rc<Cell<Bounds<Pixels>>>) {
        let mut views = self.views.borrow_mut();
        let (measured, curves) = if let Some(held) = views.get_mut(&shown.panel.id) {
            held.drawn = true;
            let plot = held.view.read(cx);
            if plot.shown.same(&shown) {
                return (plot_held(held.view.clone()), Rc::clone(&plot.measured));
            }

            (Rc::clone(&plot.measured), Rc::clone(&plot.curves))
        } else {
            if views.len() >= PLOTS_MAX {
                views.clear();
            }

            let curves = HeldCurves::new(Rc::clone(&self.curve_builds));
            (Rc::new(Cell::new(Bounds::default())), Rc::new(curves))
        };

        let id = shown.panel.id.clone();
        let plot = Plot {
            shown,
            measured: Rc::clone(&measured),
            curves,
            renders: Rc::clone(&self.renders),
        };
        let view = cx.new(|_| plot);
        let held = Held {
            view: view.clone(),
            drawn: true,
        };
        views.insert(id, held);

        (plot_held(view), measured)
    }
}

fn plot_held(view: Entity<Plot>) -> AnyElement {
    view.cached(StyleRefinement::default().size_full())
        .into_any_element()
}
