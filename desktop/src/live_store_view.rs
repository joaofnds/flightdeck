//! The page drawn from the store's newest View, which the window takes each tick. While they
//! follow the newest second, Overview, Memory, Network and Storage draw their machine charts and
//! readouts from it. Overview also draws its CPU, memory, energy and disk charts by App, its top
//! Apps and its hardware figure from it, Memory its chart of resident memory by App and its top
//! Apps, and Storage its chart of disk bytes by App and its top Apps. The CPU, Energy and GPU
//! pages draw only their chart by App from it while they follow the newest second, and a chosen
//! second, a hover's reading and every other chart draw from the worker's readings.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, Global};
use num_traits::ToPrimitive;
use serde_json::{Value, json};
use viewer::store::{Cohorts, Leaders, MachineSecond, Second, Store, View, WallSeconds};

use super::LiveDashboard;
use crate::data::Panel;
use crate::hardware::Readings;
use crate::live_app_attribution_history::{AppAttributionHistory, Metric};
use crate::live_hover::{HeldSecond, Target};
use crate::live_inventory::PerformanceLevel;
use crate::live_overview_app_histories::OverviewAppHistories;
use crate::live_projection::held_projection;
use crate::live_rankings::{Rankings, from_sampled, from_selected, stamp_identity};
use crate::live_resident_history::ResidentHistory;
use crate::live_source::AppChartSource;
use crate::live_source_mode::SourceMode;
use crate::live_thermal_pressure::thermal_pressure_level_text;
use crate::live_views::summary;
use crate::live_views::utilization::{Shares, UtilizationReadings, utilizations};

#[cfg(test)]
#[path = "live_store_view_tests.rs"]
mod tests;

const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;
/// The charts by App of a rate the pages draw from the store's View.
const APP_METRICS: [Metric; 4] = [Metric::Cpu, Metric::Power, Metric::Disk, Metric::Gpu];

/// The live buffers kept across window hides; ingestion parks until the window shows again.
struct LiveStore(Store);

impl Global for LiveStore {}

pub fn install_store(store: Store, cx: &mut App) {
    cx.set_global(LiveStore(store));
}

#[cfg(feature = "automation")]
pub(crate) fn store_activity(cx: &App) -> Value {
    cx.try_global::<LiveStore>().map_or(Value::Null, |store| {
        json!({"active":store.0.is_active(), "paused":store.0.is_paused(),
            "pending":store.0.pending()})
    })
}

/// Parks ingestion while hidden, or reconciles the recording when the window shows again.
pub(crate) fn set_store_active(active: bool, cx: &App) {
    if let Some(store) = cx.try_global::<LiveStore>() {
        store.0.set_active(active);
    }
}

#[cfg(test)]
pub(crate) fn store_active(cx: &App) -> bool {
    cx.global::<LiveStore>().0.is_active()
}

/// Takes the installed store out of the app, which stops it once the caller drops it.
#[cfg(all(test, feature = "automation"))]
pub(crate) fn uninstall_store(cx: &mut App) -> Store {
    cx.remove_global::<LiveStore>().0
}

/// Wakes the page through `waker` each time the app's store hands it a View, and answers
/// whether the app runs a store to do so.
pub(super) fn wake_store_with(waker: viewer::store::Waker, cx: &App) -> bool {
    let Some(store) = cx.try_global::<LiveStore>() else {
        return false;
    };

    store.0.wake_with(waker);
    true
}

pub(super) fn store_pending(cx: &App) -> bool {
    cx.try_global::<LiveStore>()
        .is_some_and(|store| store.0.pending())
}

/// The handle a new reader of the app's Store queries it through, where the app runs one.
pub(crate) fn store_query_handle(cx: &App) -> Option<viewer::store::QueryHandle> {
    Some(cx.try_global::<LiveStore>()?.0.query_handle())
}

pub(super) fn store_installed(cx: &App) -> bool {
    cx.has_global::<LiveStore>()
}

/// When the page looks in the store's mailbox for a View.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) enum MailboxWatch {
    /// At every tick, where nothing wakes the page when the store leaves a View.
    EveryTick,
    /// At each tick from the store's wake until a look takes a View. A look can find none while
    /// the store catches up, which leaves its View with no further wake.
    FromWake { woken: bool },
}

impl MailboxWatch {
    pub(super) fn looks(self) -> bool {
        match self {
            Self::EveryTick => true,
            Self::FromWake { woken } => woken,
        }
    }

    /// The watch once a wake says the store may have left a View.
    pub(super) fn after_wake(self) -> Self {
        match self {
            Self::EveryTick => Self::EveryTick,
            Self::FromWake { .. } => Self::FromWake { woken: true },
        }
    }

    pub(super) fn after_view(self) -> Self {
        match self {
            Self::EveryTick => Self::EveryTick,
            Self::FromWake { .. } => Self::FromWake { woken: false },
        }
    }
}

/// The View the store handed over since the last take, or none where the app runs no store, as
/// for a recording opened read-only.
pub(super) fn take_store_view(cx: &App) -> Option<Arc<View>> {
    let store = cx.try_global::<LiveStore>()?;

    let view = store.0.take();
    #[cfg(all(test, feature = "automation"))]
    MAILBOX_LOOKS.with(|looks| looks.set(looks.get().after(view.is_some())));
    view
}

/// How many of `seconds`, the View's live span oldest first, the charts draw a second apart:
/// those of the newest `CHARTED_SECONDS` seconds, since the older seconds the span holds stand
/// before the charts' window.
pub(super) fn tick_deadline_scalar_points_charted(seconds: &[Arc<MachineSecond>]) -> usize {
    let Some(newest) = seconds.last() else {
        return 0;
    };

    let first =
        (newest.wall_ns / NANOSECONDS_PER_SECOND).saturating_sub(viewer::CHARTED_SECONDS - 1);
    let older = seconds.partition_point(|second| second.wall_ns / NANOSECONDS_PER_SECOND < first);

    seconds.len() - older
}

#[cfg(all(test, feature = "automation"))]
thread_local! {
    /// What the page on this thread has asked the store's mailbox for, which a test reads.
    pub(crate) static MAILBOX_LOOKS: std::cell::Cell<MailboxLooks> =
        const { std::cell::Cell::new(MailboxLooks { looks: 0, views: 0 }) };
}

/// How often the page looked in the store's mailbox, and how many of those looks took a View.
#[cfg(all(test, feature = "automation"))]
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MailboxLooks {
    pub(crate) looks: u64,
    pub(crate) views: u64,
}

#[cfg(all(test, feature = "automation"))]
impl MailboxLooks {
    fn after(self, took: bool) -> Self {
        Self {
            looks: self.looks + 1,
            views: self.views + u64::from(took),
        }
    }
}

/// The App charts the page drew from its last View, which the charts of a newer View take the
/// points of the seconds both Views hold from.
struct OlderAppCharts {
    histories: Option<OverviewAppHistories>,
    resident: Option<ResidentHistory>,
}

/// The charts the page drew from a View, as the range zoomed them, so a reading that arrives
/// before the next View splices the same charts in again and their plots draw nothing.
#[derive(Default)]
pub(super) struct StorePanels {
    /// The version of the View they were drawn from, or none before the first.
    version: Option<u64>,
    /// The page whose machine charts they are, with its charts by App under that page's titles,
    /// or none where they are only the charts by App a page that draws no machine chart from the
    /// View draws.
    machine: Option<&'static str>,
    /// The wall seconds of the range the charts by App chose the Apps they draw over, or none
    /// before the first View.
    chosen_over: Option<WallSeconds>,
    panels: Vec<Rc<Panel>>,
    /// The wall second of each point of `panels`, in the order of their positions.
    seconds: Vec<u64>,
    /// The charts by App the View draws, of resident memory and of `APP_METRICS`, leaving out
    /// each it holds too little for.
    apps: Vec<Rc<Panel>>,
    /// The histories of CPU time, power and disk bytes that `apps` draw, which say why a chart
    /// is left out, or none before the first View.
    histories: Option<OverviewAppHistories>,
    /// The history of GPU time that `apps` draw on a page other than Overview, or none on
    /// Overview and before the first View.
    gpu: Option<AppAttributionHistory>,
    /// The history of resident memory `apps` draw, or none before the first View.
    resident: Option<ResidentHistory>,
    /// The top Apps of the View's newest second, or none before the first View.
    rankings: Option<Rankings>,
    /// Why the last View the page took could not be drawn, reported once.
    failure: Option<String>,
    /// What the page kept of the View they were drawn from when it let go of it, or none while
    /// it holds a View.
    remnant: Option<Remnant>,
    /// How many times the page built charts from a View, which a timed run reports.
    builds: u64,
}

/// The newest second of a View the page let go of, which the page shows, with the charts it
/// drew from that View, until the store's next View.
struct Remnant {
    last: u64,
    newest: MachineSecond,
}

impl StorePanels {
    /// How many times the page built charts from a View.
    pub(super) fn builds(&self) -> u64 {
        self.builds
    }
}

impl LiveDashboard {
    /// Takes `view`, the store's newest, and draws from it the charts the page draws from the
    /// store, the machine charts of Overview, Memory, Network and Storage among them. Answers
    /// whether the page changed.
    pub(crate) fn take_view(&mut self, view: Arc<View>) -> bool {
        self.store_view_place(view);
        // The hover holds the charts where they were until it ends.
        if self.store_view_hover_shown() {
            return false;
        }
        if !self.draw_store_view() {
            return false;
        }

        self.zoom_panels();
        self.refresh_chart_inspector();
        self.rebuild();
        true
    }

    /// Puts `view`, the store's newest, in place of the View the page holds, drawing nothing, so
    /// the page's next draw draws from it.
    pub(super) fn store_view_place(&mut self, view: Arc<View>) {
        take_view_report(self.store_view.as_deref(), &view);
        self.store_view = Some(view);
        self.store_panels.remnant = None;
    }

    /// Puts the page's machine charts drawn from the store's View in place of its own, on a page
    /// `store_view_draws_machine` names, where the page draws them from it, drawing them once for
    /// each View. Answers whether it put them in place.
    pub(super) fn draw_store_view(&mut self) -> bool {
        if !store_view_draws_machine(&self.request.route) {
            return self.draw_store_view_apps();
        }
        let Some(version) = self.drawn_store_view().map(|view| view.version) else {
            return self.draw_store_view_remnant();
        };
        let Some(window) = self.view_window() else {
            unreachable!("the range lies against the newest second of the drawn View");
        };
        let chosen_over = WallSeconds {
            first: window.first,
            last: window.first + window.span - 1,
        };

        let drawn = &self.store_panels;
        if drawn.version != Some(version)
            || drawn.chosen_over != Some(chosen_over)
            || drawn.machine != store_panels_machine(&self.request.route)
        {
            let Some(view) = self.store_view.clone() else {
                unreachable!("the drawn View is the View the page holds");
            };
            match self.draw_store_view_panels(&view, chosen_over) {
                Ok(panels) => self.store_view_built(panels),
                Err(error) => {
                    self.draw_store_view_failed(error);
                    return false;
                }
            }
        }
        draw_store_view_splice(&mut self.panels, &self.store_panels.panels);
        let route = self.request.route.clone();
        draw_store_view_splice_apps(&route, &mut self.panels, &self.store_panels.apps);
        true
    }

    /// Puts the page's chart by App drawn from the store's View in place of the worker's, on a
    /// page that draws no machine chart from the View but draws a chart by App from it, drawing
    /// the charts once for each View while no hover holds the page still. Answers whether it put
    /// the chart in place.
    fn draw_store_view_apps(&mut self) -> bool {
        let Some(version) = self.drawable_store_view().map(|view| view.version) else {
            if self.draw_store_view_remnant() {
                return true;
            }
            return self.draw_store_view_apps_left();
        };
        let Some(window) = self.view_window() else {
            unreachable!("a page other than Overview draws from the View once the worker read it");
        };
        let chosen_over = WallSeconds {
            first: window.first,
            last: window.first + window.span - 1,
        };

        let drawn = &self.store_panels;
        if self.preview.is_some() {
            // A hover holds the charts where the pointer found them until it ends, so a reply
            // of the page's own that it takes gets back the charts drawn before it.
            if drawn.machine.is_some() || drawn.version.is_none() {
                return false;
            }
        } else if drawn.version != Some(version)
            || drawn.chosen_over != Some(chosen_over)
            || drawn.machine.is_some()
        {
            // The App charts of the last View give the new ones the points both Views hold.
            let older = OlderAppCharts {
                histories: self.store_panels.histories.take(),
                resident: self.store_panels.resident.take(),
            };
            let older_gpu = self.store_panels.gpu.take();
            let Some(view) = self.store_view.clone() else {
                unreachable!("taking the App charts leaves the drawn View as it was");
            };
            let cohorts = draw_store_view_cohorts(&view, chosen_over, &mut self.leaders);
            let mut panels = draw_store_view_panels_apps(&view, older, &cohorts, chosen_over);
            // Every page that draws no machine chart from the View draws the GPU chart, so such a
            // page opened from another finds its chart among those drawn from the same View.
            let gpu = match older_gpu {
                Some(older) => {
                    AppAttributionHistory::from_view_after(Metric::Gpu, &view, &cohorts, older)
                }
                None => AppAttributionHistory::from_view(Metric::Gpu, &view, &cohorts),
            };
            panels.apps.extend(gpu.panel());
            panels.gpu = Some(gpu);
            for panel in &mut panels.apps {
                super::zoom_panel(panel, window);
            }
            self.store_view_built(panels);
        }
        let route = self.request.route.clone();
        draw_store_view_splice_apps(&route, &mut self.panels, &self.store_panels.apps);
        true
    }

    /// Puts the charts the page drew from the View it let go of in place of the worker's, where
    /// it draws from them until the store's next View. Answers whether it put them in place.
    fn draw_store_view_remnant(&mut self) -> bool {
        if self.kept_remnant().is_none() {
            return false;
        }

        draw_store_view_splice(&mut self.panels, &self.store_panels.panels);
        let route = self.request.route.clone();
        draw_store_view_splice_apps(&route, &mut self.panels, &self.store_panels.apps);
        true
    }

    /// Puts the charts by App the page last drew from a View in place of the worker's, where the
    /// page's reading left its chart by App to the View and the page holds neither a View nor
    /// what it kept of one, as after the window let go of the View. Answers whether it put them
    /// in place.
    fn draw_store_view_apps_left(&mut self) -> bool {
        if !self.store_panels_apps_left() {
            return false;
        }

        let route = self.request.route.clone();
        draw_store_view_splice_apps(&route, &mut self.panels, &self.store_panels.apps);
        true
    }

    /// Draws the page's charts from the store's View again for a range the reader picked, so the
    /// charts by App draw the Apps that led over it and the machine charts of a page drawn from
    /// the View show the range, but while a hover holds the charts where they were, which draws
    /// them once it ends.
    pub(crate) fn store_view_range_picked(&mut self) {
        if self.store_view_hover_shown() {
            return;
        }
        if !self.draw_store_view() {
            return;
        }

        self.refresh_chart_inspector();
    }

    /// Draws the page's machine charts from the store's newest View, which a hover that ended
    /// held them back from, where the page draws them from it.
    pub(crate) fn store_view_resume(&mut self) {
        if !self.draw_store_view() {
            return;
        }

        self.zoom_panels();
        self.refresh_chart_inspector();
    }

    /// Lets go of the View a hold kept, whose buffers the store then makes no new ones beside.
    /// The page keeps the charts it drew from the View and the readings of its newest second,
    /// and shows them until the store's next View, since the worker's readings may be older and
    /// draw the charts by App over fifteen minutes at most. It keeps the Apps it chose those
    /// charts from, since the next View shares the stretches they add up.
    pub(super) fn store_view_let_go(&mut self) {
        let Some(view) = self.store_view.take() else {
            return;
        };

        self.store_panels.remnant = view.seconds.last().map(|newest| Remnant {
            last: newest.wall_ns / NANOSECONDS_PER_SECOND,
            newest: MachineSecond::clone(newest),
        });
    }

    /// Lets go of the store's View, of what the page kept of one it let go of, and of the Apps
    /// the page kept to choose its charts by App from it, so the page draws nothing from the store
    /// and keeps none of its Apps alive until it takes another View.
    pub(super) fn store_view_drop(&mut self) {
        self.store_view = None;
        self.store_panels.remnant = None;
        self.leaders.forget();
    }

    /// Lets go of the store's View as the window hides, but of none a hold keeps, since the
    /// window shown again shows the held page.
    pub(super) fn store_view_hide(&mut self) {
        if self.hold.is_some() {
            return;
        }

        self.store_view_drop();
    }

    /// The charts the page drew from the store's View, for a window opened again in place of this
    /// one, which shows them for a reading that left its chart by App to the View. A reading that
    /// brought its chart needs none of them, so the closed window holds none, and it keeps
    /// nothing of a View the page let go of, since it takes the store's newest View.
    pub(super) fn store_view_window_state(&mut self) -> StorePanels {
        if !self.store_panels_apps_left() {
            return StorePanels::default();
        }

        let mut panels = std::mem::take(&mut self.store_panels);
        panels.remnant = None;
        panels
    }

    /// Puts `panels`, built from the View the page holds, in place of the charts it drew before.
    fn store_view_built(&mut self, panels: StorePanels) {
        // Charts just built carry no count of their own, which this would drop.
        assert_eq!(panels.builds, 0);
        let builds = self.store_panels.builds + 1;
        self.store_panels = StorePanels { builds, ..panels };
    }

    /// Lets go of the View the page could not draw, and of the Apps the page kept to choose its
    /// charts by App from it, so the page shows the worker's readings whole and its error stays
    /// the worker's, and reports `error` once each time it changes. A reading that left its points
    /// to the View holds none to show, so its rested reader reads the page again.
    fn draw_store_view_failed(&mut self, error: String) {
        if self.store_panels.failure.as_ref() != Some(&error) {
            eprintln!("Flightdeck could not draw the store's View: {error}");
        }
        self.store_panels.failure = Some(error);
        self.store_view_drop();

        if self.points_left_to_view() && !self.reply_pending {
            self.request_reading();
        }
    }

    /// The charts of `view`, as the range zooms them, with its charts by App and the top Apps of
    /// its newest second on a page that draws a chart by App from the View. The charts by App
    /// draw on lines of their own the Apps the page chooses for each over the wall seconds
    /// `chosen_over`, and take the points of the seconds they share with the charts by App of
    /// the page's last View.
    fn draw_store_view_panels(
        &mut self,
        view: &View,
        chosen_over: WallSeconds,
    ) -> Result<StorePanels, String> {
        let Some(route) = store_panels_machine(&self.request.route) else {
            unreachable!("only a page drawn from the View draws its machine charts from it");
        };
        let mut drawn = if store_view_draws_apps(route) {
            // The App charts of the last View give the new ones the points both Views hold.
            let older = OlderAppCharts {
                histories: self.store_panels.histories.take(),
                resident: self.store_panels.resident.take(),
            };
            let cohorts = draw_store_view_cohorts(view, chosen_over, &mut self.leaders);
            let mut drawn = draw_store_view_panels_apps(view, older, &cohorts, chosen_over);
            if route == "overview" {
                crate::live_acceptance::prepare_overview_titles(&mut drawn.apps)?;
            }
            drawn.rankings = Some(draw_store_view_rankings(view));
            drawn
        } else {
            // A page that shows no App of the View, as Network, chooses none, builds no chart or
            // ranking of them and keeps none alive.
            assert!(!crate::live_source::route_ranks_apps(route));
            self.leaders.forget();
            StorePanels {
                version: Some(view.version),
                chosen_over: Some(chosen_over),
                ..StorePanels::default()
            }
        };
        let levels = draw_store_view_panels_levels(view);
        let mut panels = crate::live_projection::view_panels(route, view, levels.as_ref())?;

        if let Some(window) = self.view_window() {
            for panel in panels.iter_mut().chain(&mut drawn.apps) {
                super::zoom_panel(panel, window);
            }
        }
        let seconds = crate::live_projection::overview_seconds(view)?;
        // A hover reads the range's end from the charts' last point.
        let last = view
            .seconds
            .last()
            .map(|second| second.wall_ns / NANOSECONDS_PER_SECOND);
        assert_eq!(seconds.last().copied(), last);
        Ok(StorePanels {
            machine: Some(route),
            panels,
            seconds,
            ..drawn
        })
    }

    /// The View a page `store_view_draws_machine` names draws its machine charts and readouts
    /// from, and Overview its hardware figure: the one `followed_store_view` names, while no hover
    /// the worker's readings show is shown.
    fn drawn_store_view(&self) -> Option<&View> {
        // A hover the worker's readings show holds the page still, as it holds those readings.
        if self.worker_hover_shown() {
            return None;
        }

        self.followed_store_view()
    }

    /// Whether a hover the worker's readings show is shown.
    fn worker_hover_shown(&self) -> bool {
        self.preview
            .as_ref()
            .is_some_and(|preview| preview.held.is_none())
    }

    /// What the page kept of the View it let go of, while it draws its machine charts from
    /// that in place of a View, and its readouts and hardware figure while no hover is shown.
    fn drawn_remnant(&self) -> Option<&Remnant> {
        if !store_view_draws_machine(&self.request.route) {
            return None;
        }

        self.kept_remnant()
    }

    /// What the page kept of the View it let go of, while it draws the charts it drew from that
    /// View, machine charts or charts by App, in place of a View: where the page draws charts
    /// from the store, on the kind of page the charts were drawn for, whatever hover is shown,
    /// since a hover holds the charts still.
    fn kept_remnant(&self) -> Option<&Remnant> {
        let remnant = self.store_panels.remnant.as_ref()?;
        assert!(self.store_view.is_none());
        if !self.draws_from_store_view() {
            return None;
        }
        if self.store_panels.machine != store_panels_machine(&self.request.route) {
            return None;
        }

        Some(remnant)
    }

    /// The store's newest View, while a page `store_view_draws_machine` names shows the live Mac
    /// following the newest second, or is opening before the worker's first reading, whatever
    /// hover is shown. The View is no older than the worker's reading, unless that reading left
    /// the page's charts by App to the View.
    pub(super) fn followed_store_view(&self) -> Option<&View> {
        if !store_view_draws_machine(&self.request.route) {
            return None;
        }

        self.drawable_store_view()
    }

    /// Whether the page shows the charts it drew from the store, which `store_panels` holds, its
    /// machine charts or its charts by App or both: from the View `drawn_store_view` names on a
    /// page drawn from the View, from the View it follows on another page, whose charts a hover
    /// holds still, or from what it kept of a View it let go of.
    fn store_panels_shown(&self) -> bool {
        // A page keeps the charts drawn for a page whose machine charts differ from its own until
        // it draws its own, which a hover holds back.
        let machine = store_panels_machine(&self.request.route);
        if self.store_panels.machine != machine {
            return false;
        }
        if self.kept_remnant().is_some() {
            return true;
        }
        if machine.is_some() {
            return self.drawn_store_view().is_some();
        }

        self.drawable_store_view().is_some()
    }

    /// The store's newest View, on any page that draws charts from it, its machine charts or a
    /// chart by App, which `followed_store_view` gives on a page drawn from the View. It is none
    /// while it is older than the worker's reading, unless that reading left the page's charts
    /// by App to it. A page other than Overview draws from it once the worker read the page.
    fn drawable_store_view(&self) -> Option<&View> {
        if !self.draws_from_store_view() {
            return None;
        }

        let view = self.store_view.as_deref()?;
        let view_last = view.seconds.last()?.wall_ns / NANOSECONDS_PER_SECOND;
        // At launch the worker's first reading comes seconds after the store's first View, which
        // draws into the empty charts the page opened with. A page whose read failed, or that
        // keeps the room of the readings it showed until the worker answers, holds no charts.
        let Some(snapshot) = &self.snapshot else {
            if !self.is_opening() {
                return None;
            }
            if !self.placeholders.is_empty() {
                return None;
            }
            return Some(view);
        };
        let readings_last = crate::live_projection::context_last_second(&snapshot.selection)?;
        // A store that falls behind the worker draws nothing where the worker's reading brought
        // the charts by App, so the charts never hold less than the worker read. A reading that
        // left them to the View brought none, so the View goes on drawing them until the page's
        // next reading, which `app_chart_source` then leaves to bring them.
        if view_last < readings_last {
            if snapshot.app_chart_source == AppChartSource::Reading {
                return None;
            }
        }
        Some(view)
    }

    /// Whether the newest second of the store's View the page holds is older than its reading.
    fn store_view_trails(&self) -> bool {
        let Some(view) = self.store_view.as_deref() else {
            return false;
        };
        let Some(snapshot) = &self.snapshot else {
            return false;
        };
        let Some(view_newest) = view.seconds.last() else {
            return false;
        };
        let Some(readings_last) = crate::live_projection::context_last_second(&snapshot.selection)
        else {
            return false;
        };

        view_newest.wall_ns / NANOSECONDS_PER_SECOND < readings_last
    }

    /// Whether the page draws charts from the store: a page that draws its machine charts or a
    /// chart by App from it, once the worker read it where it is not Overview, on the live Mac,
    /// following the newest second or holding one.
    fn draws_from_store_view(&self) -> bool {
        let route = self.request.route.as_str();
        if !store_view_draws_machine(route) && !store_view_draws_apps(route) {
            return false;
        }
        if route != "overview" && self.snapshot.is_none() {
            return false;
        }
        // The store follows the live Mac, so a window that opened a saved recording draws only
        // what its worker reads of it.
        if self.native.source != SourceMode::Live {
            return false;
        }

        // A held second keeps the View the page drew its charts from when it was held.
        self.request.second.is_none() || self.hold.is_some()
    }

    /// What draws the page's chart by App: the store's View, where a page other than Overview
    /// shows the charts it drew from one, and otherwise the worker's reading, which then brings
    /// the history of that chart. Overview's readings bring its charts by App as they always did.
    /// Network, which draws no chart by App, answers the View while it shows its machine charts
    /// drawn from one, and its reading then brings no App history it would not bring anyway.
    pub(crate) fn app_chart_source(&self) -> AppChartSource {
        if self.request.route == "overview" {
            return AppChartSource::Reading;
        }
        // A page that has drawn no chart from a View has none to show in place of the reading's.
        if self.store_panels.version.is_none() {
            return AppChartSource::Reading;
        }
        // A View older than the page's reading gives way to the next reading that brings the
        // chart, as it gave way to every reading before readings left the chart to it.
        if self.store_view_trails() {
            return AppChartSource::Reading;
        }

        if self.store_panels_shown() {
            AppChartSource::View
        } else {
            AppChartSource::Reading
        }
    }

    /// Whether a hover is shown on the page drawn from the store's View, which holds the charts
    /// drawn from it where they were, so the hovered second stays under the pointer as the
    /// worker's readings hold the page still under a hover.
    fn store_view_hover_shown(&self) -> bool {
        let Some(preview) = &self.preview else {
            return false;
        };

        preview.held.is_some()
    }

    /// Whether the page's reading left its chart by App to the store's View, so the page shows
    /// the charts by App it last drew from a View, and reads their histories, in place of the
    /// reading's, whether or not it still holds a View or what it kept of one.
    fn store_panels_apps_left(&self) -> bool {
        let Some(snapshot) = &self.snapshot else {
            return false;
        };
        if snapshot.app_chart_source == AppChartSource::Reading {
            return false;
        }
        // The page says the View draws its chart only where it drew one for its own kind of
        // page, and Overview's readings always bring their charts by App.
        assert!(self.request.route != "overview");
        assert!(self.store_panels.version.is_some());
        // A page kept while it drew from a View can open again after another kind of page drew
        // its own.
        if self.store_panels.machine != store_panels_machine(&self.request.route) {
            return false;
        }

        true
    }

    /// Whether the page reads its charts by App from those it drew from the store: where it
    /// shows them, and where its reading left its chart by App to the View.
    fn store_panels_apps_read(&self) -> bool {
        if self.store_panels_shown() {
            return true;
        }

        self.store_panels_apps_left()
    }

    /// Whether `panel` is a chart the page drew from the store's View.
    pub(crate) fn store_view_draws(&self, panel: &Panel) -> bool {
        let drawn = &self.store_panels;
        if store_view_draws_app_chart(&self.request.route, &panel.id) {
            if !self.store_panels_apps_read() {
                return false;
            }
            return drawn.apps.iter().any(|drawn| drawn.id == panel.id);
        }
        if self.drawn_store_view().is_none() && self.drawn_remnant().is_none() {
            return false;
        }

        drawn.panels.iter().any(|drawn| drawn.id == panel.id)
    }

    /// The index of the point the page's chart `panel`, drawn from the View, shows: the hovered
    /// second's, or the newest second's while no hover is shown.
    pub(crate) fn store_view_index(&self, panel: &Panel) -> Option<usize> {
        let Some(target) = self.reading_target() else {
            return panel.selected_index;
        };

        if let Some(metric) = Metric::from_panel(&panel.id) {
            return self.attribution_history(metric)?.index(Some(target));
        }
        if panel.id == crate::live_resident_history::PANEL_ID {
            return self.resident_history()?.index(Some(target));
        }
        let seconds = &self.store_panels.seconds;
        seconds.iter().rposition(|second| *second == target.second)
    }

    /// The history of `metric` the page reads: the one the page drew from the store's View,
    /// where it draws one, and otherwise the one the worker read.
    pub(crate) fn attribution_history(&self, metric: Metric) -> Option<&AppAttributionHistory> {
        if self.store_panels_apps_read() {
            if let Some(histories) = &self.store_panels.histories {
                return match metric {
                    Metric::Gpu => self.store_panels.gpu.as_ref(),
                    Metric::Cpu | Metric::Power | Metric::Disk => histories.history(metric),
                };
            }
        }

        self.snapshot.as_ref()?.attribution_history(metric)
    }

    /// The history of resident memory by App the page reads: the one the page drew from the
    /// store's View, where it draws one, and otherwise the one the worker read.
    pub(crate) fn resident_history(&self) -> Option<&ResidentHistory> {
        if self.store_panels_apps_read() {
            if let Some(resident) = &self.store_panels.resident {
                return Some(resident);
            }
        }

        self.snapshot
            .as_ref()?
            .resident_history
            .as_deref()
            .map(|value| &**value)
    }

    /// The second the page's readings show: the hovered or held one, or the newest, of the View
    /// the page draws, where it draws one, and otherwise the second the worker's reading, or a
    /// hover's, selected.
    pub(crate) fn shown_second(&self) -> Option<u64> {
        if let Some(second) = self.store_view_last_second() {
            if let Some(preview) = self.shown_preview() {
                return Some(preview.target.second);
            }
            return Some(second);
        }

        let snapshot = self.reading_snapshot()?;
        snapshot.selection["second"].as_u64()
    }

    /// The newest second of the View the page draws, where it draws one, which the range ends
    /// at since the worker's readings may not hold it yet. Under a hover, the newest second of
    /// the View the charts were drawn from.
    pub(crate) fn store_view_last_second(&self) -> Option<u64> {
        let Some(view) = self.drawn_store_view() else {
            return self.drawn_remnant().map(|remnant| remnant.last);
        };
        if self.store_view_hover_shown() {
            return self.store_panels.seconds.last().copied();
        }

        Some(view.seconds.last()?.wall_ns / NANOSECONDS_PER_SECOND)
    }

    /// The top Apps of the second the View the page draws shows, and that second: the one it
    /// held for the shown hover or the held second, or its newest while neither is shown. None
    /// where the View holds no Apps of the second shown, and where the page draws from the
    /// worker's readings.
    pub(crate) fn store_view_rankings(&self) -> Option<(&Rankings, u64)> {
        let second = self.store_view_last_second()?;
        if let Some(preview) = self.shown_preview() {
            let held = preview.held.as_ref()?;
            return Some((held.rankings.as_ref()?, preview.target.second));
        }

        Some((self.store_panels.rankings.as_ref()?, second))
    }

    /// The readings of the second the View the page draws shows, keyed as the worker's summary of
    /// a second keys them: its complete reading's own summary, which the worker's reading of the
    /// second is, and what the second itself holds where it has no complete reading.
    pub(crate) fn store_view_summary(&self) -> Option<Value> {
        let second = self.store_view_shown()?;
        let Some(complete) = second.complete() else {
            return Some(store_view_summary_of(second));
        };

        Some(complete.summary(second))
    }

    /// The hardware figure's readings of the second the View the page draws shows.
    pub(crate) fn store_view_hardware(&self) -> Option<Readings> {
        Some(store_view_hardware_of(self.store_view_shown()?))
    }

    /// The utilization panel's readings of the second the View the page draws shows.
    pub(crate) fn store_view_utilization(&self) -> Option<UtilizationReadings> {
        Some(store_view_utilization_of(self.store_view_shown()?))
    }

    /// The second the View the page draws shows: the one it held for the shown hover or the held
    /// second, or its newest while neither is shown. None in a gap, and where the page draws from
    /// the worker's readings.
    fn store_view_shown(&self) -> Option<&MachineSecond> {
        let Some(view) = self.drawn_store_view() else {
            // The page kept only the newest second of the View it let go of, so a hover shows the
            // reader's readings of its second over the charts drawn from that View.
            if self.shown_preview().is_some() {
                return None;
            }
            return self.drawn_remnant().map(|remnant| &remnant.newest);
        };
        let Some(preview) = self.shown_preview() else {
            return view.seconds.last().map(AsRef::as_ref);
        };
        let Some(held) = &preview.held else {
            unreachable!(
                "a second shown over the View the page draws holds the View's record of it"
            );
        };

        held.second.as_deref()
    }

    /// The wall second of each second the store's View holds of its live span, oldest first, or
    /// none where the page draws no chart from the View.
    pub(crate) fn store_view_wall_seconds(&self) -> Vec<u64> {
        let Some(view) = self.drawable_store_view() else {
            return Vec::new();
        };

        view.seconds
            .iter()
            .map(|second| second.wall_ns / NANOSECONDS_PER_SECOND)
            .collect()
    }

    /// The second whose point the charts of the store's View draw for a hover of `second`, or
    /// none where they draw no point for it or the page draws no chart from the View.
    pub(crate) fn store_view_point_second(&self, second: u64) -> Option<u64> {
        let view = self.drawable_store_view()?;

        match crate::live_projection::overview_point_second(view, second) {
            Ok(shown) => shown,
            Err(error) => {
                eprintln!("Hover of the store's View: {error}");
                None
            }
        }
    }

    /// The hover of `second` on a page that draws charts from the store's View, naming the tick
    /// the View holds of it, so the charts by App find that tick as they find any they draw. A
    /// second of the older tail the View sampled no tick of names the tick of the stretch that
    /// starts there, which the charts by App draw at it. None where the View holds no identity
    /// of `second`, as in a gap, and where the page draws no chart from the View.
    pub(crate) fn store_view_target(&self, second: u64) -> Option<Target> {
        let view = self.drawable_store_view()?;
        let at_second = |held: &Arc<MachineSecond>| held.wall_ns / NANOSECONDS_PER_SECOND == second;

        assert_eq!(view.apps.len(), view.seconds.len());
        let observation = match view.seconds.iter().rposition(at_second) {
            Some(index) => view.apps.second(index).observation()?,
            None => *view
                .sampled
                .iter()
                .rfind(|sample| sample.machine.wall_ns / NANOSECONDS_PER_SECOND == second)
                .map(|sample| &sample.observation)
                .or_else(|| store_view_stretch_observation(view, second))?,
        };
        Some(Target {
            second,
            identity: Some(stamp_identity(&observation.stamp)),
        })
    }

    /// The second `target` names as the View the page draws holds it, whatever hover is shown,
    /// or none where the page draws from the worker's readings.
    pub(crate) fn store_view_held(&self, target: &Target) -> Option<HeldSecond> {
        let view = self.followed_store_view()?;
        assert_eq!(view.apps.len(), view.seconds.len());
        // From the newest, so a recent target ends the search early.
        let recent = view
            .seconds
            .iter()
            .zip(view.apps.seconds())
            .rfind(|(second, apps)| {
                store_view_matches(target, second, apps.observation().as_ref())
            });
        if let Some((second, apps)) = recent {
            return Some(HeldSecond {
                second: Some(Arc::clone(second)),
                rankings: Some(store_view_ranks(apps)),
            });
        }
        let earlier = view
            .earlier
            .iter()
            .enumerate()
            .rposition(|(index, second)| {
                let wall_second = second.wall_ns / NANOSECONDS_PER_SECOND;
                let observation = view
                    .sampled
                    .get(index)
                    .map(|sample| &sample.observation)
                    .or_else(|| store_view_stretch_observation(view, wall_second));

                store_view_matches(target, second, observation)
            });
        let rankings = earlier.and_then(|index| {
            let sample = view.sampled.get(index)?;
            assert_eq!(sample.machine.publication, view.earlier[index].publication);
            assert_eq!(sample.machine.wall_ns, view.earlier[index].wall_ns);
            Some(from_sampled(sample))
        });
        Some(HeldSecond {
            second: earlier.map(|index| Arc::clone(&view.earlier[index])),
            rankings,
        })
    }
}

fn store_view_matches(
    target: &Target,
    second: &MachineSecond,
    observation: Option<&viewer::live_app_response::Observation>,
) -> bool {
    if second.wall_ns / NANOSECONDS_PER_SECOND != target.second {
        return false;
    }
    let Some(identity) = &target.identity else {
        return true;
    };
    let Some(observation) = observation else {
        return false;
    };
    crate::live_source::identity_validate(identity, &stamp_identity(&observation.stamp)).is_ok()
}

/// The tick that places the stretch of `view` that starts at `second` on the charts by App, or
/// none where the first tick the store took in of no stretch of `view` is at `second`.
fn store_view_stretch_observation(
    view: &View,
    second: u64,
) -> Option<&viewer::live_app_response::Observation> {
    // The store's stretches run oldest first. A stretch found out of order names another
    // second, so it names no tick here.
    let stretch = viewer::store::stretch_of_wall_second(second);
    let index = view
        .stretches
        .binary_search_by_key(&stretch, |held| held.stretch)
        .ok()?;
    let observation = view.stretches[index].observation.as_ref()?;
    let wall_ns = observation.wall_ns?;

    (wall_ns / NANOSECONDS_PER_SECOND == second).then_some(observation)
}

/// The Apps each of Overview's App charts draws on a line of its own over `window` of `view`,
/// those that led there, or none while the store is still loading the Apps of `view`.
fn draw_store_view_cohorts(view: &View, window: WallSeconds, leaders: &mut Leaders) -> Cohorts {
    let mut cohorts = Cohorts::with_room();
    if !view.apps_loaded {
        return cohorts;
    }

    leaders.choose(&view.readings(), window, &mut cohorts);
    cohorts
}

/// The charts by App of `view`, each drawing on lines of their own the Apps `cohorts` chose for
/// it.
fn draw_store_view_panels_histories(
    view: &View,
    cohorts: &Cohorts,
    older: Option<OverviewAppHistories>,
) -> OverviewAppHistories {
    let Some(older) = older else {
        let history = |metric| {
            Arc::new(crate::live_source::Presentation::Legacy(
                AppAttributionHistory::from_view(metric, view, cohorts),
            ))
        };
        return OverviewAppHistories {
            cpu: history(Metric::Cpu),
            power: history(Metric::Power),
            disk: history(Metric::Disk),
        };
    };

    // The page holds the only reference to the charts it drew.
    let history = |metric, older| {
        use crate::live_source::Presentation;
        let history = match Arc::into_inner(older) {
            Some(Presentation::Legacy(older)) => Presentation::Legacy(
                AppAttributionHistory::from_view_after(metric, view, cohorts, older),
            ),
            Some(Presentation::Shared(older)) => match Arc::into_inner(older) {
                Some(older) => Presentation::Legacy(AppAttributionHistory::from_view_after(
                    metric, view, cohorts, older,
                )),
                None => {
                    Presentation::Legacy(AppAttributionHistory::from_view(metric, view, cohorts))
                }
            },
            Some(Presentation::Store(older)) => {
                Presentation::Store(older.map(|older| {
                    AppAttributionHistory::from_view_after(metric, view, cohorts, older)
                }))
            }
            Some(Presentation::Canonical(older)) => {
                Presentation::Canonical(older.map(|older| {
                    AppAttributionHistory::from_view_after(metric, view, cohorts, older)
                }))
            }
            None => Presentation::Legacy(AppAttributionHistory::from_view(metric, view, cohorts)),
        };

        Arc::new(history)
    };
    OverviewAppHistories {
        cpu: history(Metric::Cpu, older.cpu),
        power: history(Metric::Power, older.power),
        disk: history(Metric::Disk, older.disk),
    }
}

/// The top Apps of the newest second of `view`, ranked by the rule the worker ranks a second's
/// Apps by.
fn draw_store_view_rankings(view: &View) -> Rankings {
    assert_eq!(view.apps.len(), view.seconds.len());
    let Some(apps) = view.apps.newest() else {
        unreachable!("a View the page draws holds a second");
    };

    store_view_ranks(apps)
}

/// The top Apps of `apps`, ranked by the rule the worker ranks a second's Apps by.
fn store_view_ranks(apps: Second<'_>) -> Rankings {
    from_selected(apps.ranks().map_err(|missing| missing.to_string()))
}

/// The readings of `second` that the readouts of a page drawn from the View read, keyed as the
/// worker's summary keys them.
fn store_view_summary_of(second: &MachineSecond) -> Value {
    let memory = second.memory.as_ref();
    let pressure = memory.map(|memory| held_projection::pressure_level(memory.pressure));

    // The second's record keeps app memory with the purgeable part already taken out, and holds
    // neither anonymous, purgeable nor file-backed memory, so those and the cached memory they
    // make stay unknown. It keeps the bytes the network moved but no packets or errors, and the
    // bytes the disks read and wrote but no operations, errors or service times, so those stay
    // unknown too.
    json!({
        "memory_app_bytes": memory.and_then(|memory| memory.app_bytes),
        "memory_wired_bytes": memory.map(|memory| memory.wired_bytes),
        "memory_compressed_bytes": memory.map(|memory| memory.compressed_bytes),
        "memory_total_bytes": memory.map(|memory| memory.total_bytes),
        "swap_used_bytes": memory.map(|memory| memory.swap_used_bytes),
        "swap_total_bytes": memory.map(|memory| memory.swap_total_bytes),
        "cpu_celsius": second.cpu_celsius,
        "cpu_percent": second.cpu_percent,
        "gpu_celsius": second.gpu_celsius,
        "gpu_active_percent": second.gpu_active_percent,
        "pressure": pressure,
        "power_watts": second.power_watts,
        "storage": {"read_bps": second.disk_read_bps, "write_bps": second.disk_written_bps},
        "network_rx_bps": second.network.map(|traffic| traffic.received_bps),
        "network_tx_bps": second.network.map(|traffic| traffic.sent_bps),
        "soc_celsius": second.soc_celsius,
        "fan_rpm": second.fan_rpm,
        "battery": second.battery.as_ref().map(|battery| json!({
            "charge_percent": battery.charge_percent, "power_watts": battery.power_watts})),
    })
}

/// The hardware figure's readings of `second`, as the figure reads the worker's summary of it.
fn store_view_hardware_of(second: &MachineSecond) -> Readings {
    let memory = second.memory.as_ref();
    let in_use = memory.and_then(|memory| {
        let app = memory.app_bytes?;
        summary::memory_in_use_bytes_of(app, memory.wired_bytes, memory.compressed_bytes)
    });
    let total = memory.map(|memory| memory.total_bytes);
    let battery = second.battery.as_ref();
    let narrow = |value: Option<f64>| value.and_then(|value| value.to_f32());
    let gibibytes = |bytes: Option<u64>| Some(bytes?.to_f32()? / 1_073_741_824.0);

    Readings {
        temperatures: [second.cpu_celsius, second.gpu_celsius, None],
        speeds: second.fan_rpm,
        utilization: [
            narrow(second.cpu_percent),
            narrow(second.gpu_active_percent),
        ],
        memory: [gibibytes(in_use), gibibytes(total)],
        battery: [
            narrow(battery.and_then(|battery| battery.charge_percent)),
            narrow(battery.and_then(|battery| battery.power_watts)),
        ],
        thermal: thermal_pressure_level_text(second.thermal_pressure.map(u64::from)).into(),
    }
}

/// The utilization panel's readings of `second`, as the panel reads the worker's summary of it.
fn store_view_utilization_of(second: &MachineSecond) -> UtilizationReadings {
    let memory = second.memory.as_ref();
    let shares = Shares {
        levels: performance_levels_of(second),
        level_percent: second.cpu_level_percent,
        gpu_percent: second.gpu_active_percent,
        memory_used_bytes: memory.and_then(|memory| {
            let app = memory.app_bytes?;
            summary::memory_in_use_bytes_of(app, memory.wired_bytes, memory.compressed_bytes)
        }),
        memory_total_bytes: memory.map(|memory| memory.total_bytes),
        swap_used_bytes: memory.map(|memory| memory.swap_used_bytes),
        swap_total_bytes: memory.map(|memory| memory.swap_total_bytes),
    };
    let pressure = memory.map(|memory| u64::from(held_projection::pressure_level(memory.pressure)));

    UtilizationReadings {
        utilizations: utilizations(&shares),
        pressure: crate::live_views::pressure_level_text(pressure),
        thermal: thermal_pressure_level_text(second.thermal_pressure.map(u64::from)),
    }
}

/// The CPU's performance levels as the newest second of `view` names them.
fn draw_store_view_panels_levels(view: &View) -> Option<[PerformanceLevel; 2]> {
    performance_levels_of(view.seconds.last()?)
}

/// The CPU's performance levels as `second` names them.
fn performance_levels_of(second: &MachineSecond) -> Option<[PerformanceLevel; 2]> {
    let [fastest, slower] = second.performance_levels.as_ref()?;
    let level = |level: &collector::system::PerformanceLevel| {
        PerformanceLevel::named(level.name.as_str(), level.logical_cpus)
    };

    Some([level(fastest), level(slower)])
}

/// Puts each of `drawn` in place of the page's chart of the same id among `panels`.
fn draw_store_view_splice(panels: &mut [Rc<Panel>], drawn: &[Rc<Panel>]) {
    for panel in drawn {
        let place = panels.iter_mut().find(|place| place.id == panel.id);
        // The page's readings draw Overview's charts from the same list as the View.
        let Some(place) = place else {
            unreachable!("Overview lacks the chart {}", panel.id);
        };
        *place = Rc::clone(panel);
    }
}

/// Puts those of `apps`, the charts by App the View draws, that the page `route` shows in place of
/// the page's own, so a chart the View leaves out is left out and its card says why.
fn draw_store_view_splice_apps(route: &str, panels: &mut Vec<Rc<Panel>>, apps: &[Rc<Panel>]) {
    let shown = |panel: &&Rc<Panel>| store_view_draws_app_chart(route, &panel.id);

    panels.retain(|panel| !shown(&panel));
    panels.extend(apps.iter().filter(shown).map(Rc::clone));
}

/// Whether the page `route` draws its machine charts and readouts from the store's View, and its
/// top Apps where it shows any, while it follows the live Mac's newest second, so its reader
/// rests. Overview, Memory, Network and Storage do. Every gate that lets a page draw from the
/// View, rest its reader on it and resume from it asks this, so a page joins them by being named
/// here.
pub(crate) fn store_view_draws_machine(route: &str) -> bool {
    store_panels_machine(route).is_some()
}

/// The page whose machine charts the charts drawn for the page `route` are, which is `route`
/// where it draws machine charts from the store's View, and none where it draws only charts by
/// App from it. This is the one list of the pages drawn from the View.
fn store_panels_machine(route: &str) -> Option<&'static str> {
    match route {
        "overview" => Some("overview"),
        "memory" => Some("memory"),
        "network" => Some("network"),
        "storage" => Some("storage"),
        _ => None,
    }
}

/// Whether the page `route` draws its chart `id` by App from the store's View: Overview its
/// charts by App of resident memory and of `APP_METRICS` but GPU time, and the Memory, CPU,
/// Energy, Storage and GPU pages the one of them each shows.
fn store_view_draws_app_chart(route: &str, id: &str) -> bool {
    if id == crate::live_resident_history::PANEL_ID {
        return matches!(route, "overview" | "memory");
    }
    let Some(metric) = Metric::from_panel(id) else {
        return false;
    };
    if !APP_METRICS.contains(&metric) {
        return false;
    }
    if route == "overview" {
        // Overview's GPU slot names the GPU page, which draws the chart.
        return metric != Metric::Gpu;
    }

    Metric::from_route(route) == Some(metric)
}

/// Whether the page `route` draws any chart by App from the store's View.
fn store_view_draws_apps(route: &str) -> bool {
    let rates = APP_METRICS.iter().map(|metric| metric.panel_id());
    let mut ids = rates.chain([crate::live_resident_history::PANEL_ID]);

    ids.any(|id| store_view_draws_app_chart(route, id))
}

/// The charts by App of `view`, which draw on lines of their own the Apps `cohorts` chose for
/// each over the wall seconds `chosen_over` and take the points of the seconds they share with
/// `older`, under the titles a page other than Overview gives them.
fn draw_store_view_panels_apps(
    view: &View,
    older: OlderAppCharts,
    cohorts: &Cohorts,
    chosen_over: WallSeconds,
) -> StorePanels {
    let histories = draw_store_view_panels_histories(view, cohorts, older.histories);
    let resident = match older.resident {
        Some(older) => ResidentHistory::from_view_after(view, cohorts, older),
        None => ResidentHistory::from_view(view, cohorts),
    };
    let rates = APP_METRICS
        .iter()
        .filter_map(|metric| histories.history(*metric)?.panel());
    let apps = resident.panel().into_iter().chain(rates).collect();

    StorePanels {
        version: Some(view.version),
        chosen_over: Some(chosen_over),
        apps,
        histories: Some(histories),
        resident: Some(resident),
        ..StorePanels::default()
    }
}

/// Reports why the store reads less than it should, once each time the reason changes.
fn take_view_report(previous: Option<&View>, view: &View) {
    let failure = previous.and_then(|previous| previous.failure);
    if view.failure != failure {
        if let Some(failure) = view.failure {
            eprintln!("Flightdeck store failed to read the recording: {failure:?}");
        }
    }

    let left_out = previous.and_then(|previous| previous.left_out);
    if view.left_out != left_out {
        if let Some(left_out) = view.left_out {
            eprintln!("Flightdeck store left out part of the loaded hour: {left_out:?}");
        }
    }
}
