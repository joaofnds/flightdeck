//! The page drawn from the store's newest View, which the window takes each tick. Overview draws
//! its machine charts, its CPU, memory, energy and disk charts by App, its readouts and its
//! hardware figure from it while it follows the newest second. The Memory, CPU, Energy, Storage
//! and GPU pages draw their chart by App from it while they follow the newest second, and a
//! chosen second, a hover's reading and every other chart draw from the worker's readings.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, Global};
use num_traits::ToPrimitive;
use serde_json::{Value, json};
use viewer::store::{AppsSecond, Cohorts, Leaders, MachineSecond, Store, View, WallSeconds};

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

pub(super) fn store_pending(cx: &App) -> bool {
    cx.try_global::<LiveStore>()
        .is_some_and(|store| store.0.pending())
}

pub(super) fn store_installed(cx: &App) -> bool {
    cx.has_global::<LiveStore>()
}

/// The View the store handed over since the last take, or none where the app runs no store, as
/// for a recording opened read-only.
pub(super) fn take_store_view(cx: &App) -> Option<Arc<View>> {
    let store = cx.try_global::<LiveStore>()?;

    store.0.take()
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
    /// Whether they are Overview's, its machine charts among them and its titles on the charts
    /// by App, rather than the charts by App another page draws.
    overview: bool,
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
    let_go: Option<LetGo>,
}

/// The newest second of a View the page let go of, which the page shows, with the charts it
/// drew from that View, until the store's next View.
struct LetGo {
    last: u64,
    newest: MachineSecond,
}

impl LiveDashboard {
    /// Takes `view`, the store's newest, and draws Overview's machine charts from it where the
    /// page draws them from the store. Answers whether the page changed.
    pub(crate) fn take_view(&mut self, view: Arc<View>) -> bool {
        take_view_report(self.store_view.as_deref(), &view);
        self.store_view = Some(view);
        self.store_panels.let_go = None;
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

    /// Puts Overview's machine charts drawn from the store's View in place of the page's own,
    /// where the page draws them from it, drawing them once for each View. Answers whether it
    /// put them in place.
    pub(super) fn draw_store_view(&mut self) -> bool {
        if self.request.route != "overview" {
            return self.draw_store_view_apps();
        }
        let Some(version) = self.drawn_store_view().map(|view| view.version) else {
            return self.draw_store_view_let_go();
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
            || !drawn.overview
        {
            // The App charts of the last View give the new ones the points both Views hold.
            let older = OlderAppCharts {
                histories: self.store_panels.histories.take(),
                resident: self.store_panels.resident.take(),
            };
            let Some(view) = self.store_view.clone() else {
                unreachable!("taking the App charts leaves the drawn View as it was");
            };
            let cohorts = draw_store_view_cohorts(&view, chosen_over, &mut self.leaders);
            match self.draw_store_view_panels(&view, older, &cohorts, chosen_over) {
                Ok(panels) => self.store_panels = panels,
                Err(error) => {
                    self.draw_store_view_failed(error);
                    return false;
                }
            }
        }
        draw_store_view_splice(&mut self.panels, &self.store_panels.panels);
        draw_store_view_splice_apps("overview", &mut self.panels, &self.store_panels.apps);
        true
    }

    /// Puts the page's chart by App drawn from the store's View in place of the worker's, on a
    /// page other than Overview that draws one from it, drawing the charts once for each View
    /// while no hover holds the page still. Answers whether it put the chart in place.
    fn draw_store_view_apps(&mut self) -> bool {
        let Some(version) = self.apps_followed_store_view().map(|view| view.version) else {
            return self.draw_store_view_let_go();
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
            if drawn.overview || drawn.version.is_none() {
                return false;
            }
        } else if drawn.version != Some(version)
            || drawn.chosen_over != Some(chosen_over)
            || drawn.overview
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
            // Every page other than Overview draws the GPU chart, so a page opened from another
            // finds its chart among those drawn from the same View.
            let gpu = match older_gpu {
                Some(older) => {
                    AppAttributionHistory::from_view_after(Metric::Gpu, &view, &cohorts, older)
                }
                None => AppAttributionHistory::from_view(Metric::Gpu, &view, &cohorts),
            };
            panels.apps.extend(gpu.panel());
            panels.gpu = Some(gpu);
            let sliding = self.panels_slide();
            for panel in &mut panels.apps {
                super::zoom_panel(panel, window, sliding);
            }
            self.store_panels = panels;
        }
        let route = self.request.route.clone();
        draw_store_view_splice_apps(&route, &mut self.panels, &self.store_panels.apps);
        true
    }

    /// Puts the charts the page drew from the View it let go of in place of the worker's, where
    /// it draws from them until the store's next View. Answers whether it put them in place.
    fn draw_store_view_let_go(&mut self) -> bool {
        if self.apps_drawn_let_go().is_none() {
            return false;
        }

        draw_store_view_splice(&mut self.panels, &self.store_panels.panels);
        let route = self.request.route.clone();
        draw_store_view_splice_apps(&route, &mut self.panels, &self.store_panels.apps);
        true
    }

    /// Draws Overview's charts from the store's View again for a range the reader picked, so the
    /// charts by App draw the Apps that led over it, but while a hover holds the charts where they
    /// were, which draws them once it ends.
    pub(crate) fn store_view_range_picked(&mut self) {
        if self.store_view_hover_shown() {
            return;
        }
        if !self.draw_store_view() {
            return;
        }

        self.refresh_chart_inspector();
    }

    /// Draws Overview's machine charts from the store's newest View, which a hover that ended
    /// held them back from, where the page draws them from it.
    pub(crate) fn store_view_resume(&mut self) {
        if !self.draw_store_view() {
            return;
        }

        self.zoom_panels();
        self.refresh_chart_inspector();
    }

    /// Lets go of the View a hold kept, before the store wakes to catch up, since the catch-up
    /// writes the buffers a View the page holds keeps. The page keeps the charts it drew from
    /// the View and the readings of its newest second, and shows them until the store's next
    /// View, since the worker's readings may be older and draw the charts by App over fifteen
    /// minutes at most.
    pub(super) fn store_view_let_go(&mut self) {
        let Some(view) = self.store_view.take() else {
            return;
        };

        self.store_panels.let_go = view.seconds.last().map(|newest| LetGo {
            last: newest.wall_ns / NANOSECONDS_PER_SECOND,
            newest: MachineSecond::clone(newest),
        });
    }

    /// Lets go of the store's View and of what the page kept of one it let go of, so the page
    /// draws nothing from the store until it takes another View.
    pub(super) fn store_view_drop(&mut self) {
        self.store_view = None;
        self.store_panels.let_go = None;
    }

    /// Lets go of the store's View as the window hides, but of none a hold keeps, since the store
    /// stays parked while a second is held and the window shown again shows the held page.
    pub(super) fn store_view_hide(&mut self) {
        if self.hold.is_some() {
            return;
        }

        self.store_view_drop();
    }

    /// Lets go of the View the page could not draw, so the page shows the worker's readings
    /// whole and its error stays the worker's, and reports `error` once each time it changes.
    fn draw_store_view_failed(&mut self, error: String) {
        if self.store_panels.failure.as_ref() != Some(&error) {
            eprintln!("Flightdeck could not draw the store's View: {error}");
        }
        self.store_panels.failure = Some(error);
        self.store_view = None;
    }

    /// The charts of `view`, as the range zooms them, and the top Apps of its newest second. The
    /// App charts draw on lines of their own the Apps `cohorts` chose for each over the wall
    /// seconds `chosen_over`, and take the points of the seconds they share with `older`.
    fn draw_store_view_panels(
        &self,
        view: &View,
        older: OlderAppCharts,
        cohorts: &Cohorts,
        chosen_over: WallSeconds,
    ) -> Result<StorePanels, String> {
        let levels = draw_store_view_panels_levels(view);
        let mut panels = crate::live_projection::overview_panels(view, levels.as_ref())?;
        let mut drawn = draw_store_view_panels_apps(view, older, cohorts, chosen_over);
        crate::live_acceptance::prepare_overview_titles(&mut drawn.apps)?;

        if let Some(window) = self.view_window() {
            let sliding = self.panels_slide();
            for panel in panels.iter_mut().chain(&mut drawn.apps) {
                super::zoom_panel(panel, window, sliding);
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
            overview: true,
            panels,
            seconds,
            rankings: Some(draw_store_view_rankings(view)),
            ..drawn
        })
    }

    /// The View the page draws Overview's machine charts, readouts and hardware figure from: the
    /// one `followed_store_view` names, while no hover the worker's readings show is shown.
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

    /// What the page kept of the View it let go of, while it draws Overview's machine charts from
    /// that in place of a View, and its readouts and hardware figure while no hover is shown.
    fn drawn_let_go(&self) -> Option<&LetGo> {
        if self.request.route != "overview" {
            return None;
        }

        self.apps_drawn_let_go()
    }

    /// What the page kept of the View it let go of, while it draws its charts by App from that
    /// in place of a View: where the page draws them from the store, on the kind of page the
    /// charts were drawn for, whatever hover is shown, since a hover holds the charts still.
    fn apps_drawn_let_go(&self) -> Option<&LetGo> {
        let let_go = self.store_panels.let_go.as_ref()?;
        assert!(self.store_view.is_none());
        if !self.store_view_follows() {
            return None;
        }
        let overview = self.request.route == "overview";
        if self.store_panels.overview != overview {
            return None;
        }

        Some(let_go)
    }

    /// The store's newest View, while the page shows the live Mac's Overview following the
    /// newest second and the View is no older than the worker's readings, or the page is opening
    /// before the worker's first reading, whatever hover is shown.
    pub(super) fn followed_store_view(&self) -> Option<&View> {
        if self.request.route != "overview" {
            return None;
        }

        self.apps_followed_store_view()
    }

    /// The View the page's charts by App were drawn from: on Overview the one `drawn_store_view`
    /// names, and on another page that draws one from the View the one it follows, whose charts
    /// a hover holds still.
    fn apps_drawn_store_view(&self) -> Option<&View> {
        if self.request.route == "overview" {
            return self.drawn_store_view();
        }

        self.apps_followed_store_view()
    }

    /// The store's newest View, as `followed_store_view` gives it, on any page that draws a chart
    /// by App from it. A page other than Overview draws from it once the worker read the page,
    /// since its other charts are the worker's.
    fn apps_followed_store_view(&self) -> Option<&View> {
        if !self.store_view_follows() {
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
        // A store that falls behind the worker draws nothing, so the charts never hold less
        // than the worker read.
        if view_last < readings_last {
            return None;
        }
        Some(view)
    }

    /// Whether the page draws charts by App from the store: a page that draws one, once the
    /// worker read it where it is not Overview, on the live Mac, following the newest second or
    /// holding one.
    fn store_view_follows(&self) -> bool {
        if !store_view_draws_apps(&self.request.route) {
            return false;
        }
        if self.request.route != "overview" && self.snapshot.is_none() {
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

    /// Whether a hover is shown on the page drawn from the store's View, which holds the charts
    /// drawn from it where they were, so the hovered second stays under the pointer as the
    /// worker's readings hold the page still under a hover.
    fn store_view_hover_shown(&self) -> bool {
        let Some(preview) = &self.preview else {
            return false;
        };

        preview.held.is_some()
    }

    /// Whether `panel` is a chart the page drew from the store's View.
    pub(crate) fn store_view_draws(&self, panel: &Panel) -> bool {
        let drawn = &self.store_panels;
        if store_view_draws_app_chart(&self.request.route, &panel.id) {
            if self.apps_drawn_store_view().is_none() && self.apps_drawn_let_go().is_none() {
                return false;
            }
            return drawn.apps.iter().any(|drawn| drawn.id == panel.id);
        }
        if self.drawn_store_view().is_none() && self.drawn_let_go().is_none() {
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
        if self.apps_drawn_store_view().is_some() || self.apps_drawn_let_go().is_some() {
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
        if self.apps_drawn_store_view().is_some() || self.apps_drawn_let_go().is_some() {
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
            return self.drawn_let_go().map(|let_go| let_go.last);
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

    /// The readings Overview's readouts read of the second the View the page draws shows,
    /// keyed as the worker's summary of a second keys them.
    pub(crate) fn store_view_summary(&self) -> Option<Value> {
        Some(store_view_summary_of(self.store_view_shown()?))
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
            return self.drawn_let_go().map(|let_go| &let_go.newest);
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

    /// The second whose point the charts of the store's View draw for a hover of `second`, or
    /// none where they draw no point for it or the page draws no chart from the View.
    pub(crate) fn store_view_point_second(&self, second: u64) -> Option<u64> {
        let view = self.apps_followed_store_view()?;

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
        let view = self.apps_followed_store_view()?;
        let at_second = |held: &Arc<MachineSecond>| held.wall_ns / NANOSECONDS_PER_SECOND == second;

        assert_eq!(view.apps.len(), view.seconds.len());
        let observation = match view.seconds.iter().rposition(at_second) {
            Some(index) => view.apps[index].observation.as_ref()?,
            None => view
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
        let recent = view
            .seconds
            .iter()
            .zip(&view.apps)
            .rposition(|(second, apps)| {
                store_view_matches(target, second, apps.observation.as_ref())
            });
        if let Some(index) = recent {
            return Some(HeldSecond {
                second: Some(Arc::clone(&view.seconds[index])),
                rankings: Some(store_view_ranks(&view.apps[index])),
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

    let readings = viewer::store::Readings {
        seconds: &view.seconds,
        apps: &view.apps,
        stretches: &view.stretches,
    };
    leaders.choose(&readings, window, &mut cohorts);
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
    let Some(apps) = view.apps.last() else {
        unreachable!("a View the page draws holds a second");
    };

    store_view_ranks(apps)
}

/// The top Apps of `apps`, ranked by the rule the worker ranks a second's Apps by.
fn store_view_ranks(apps: &AppsSecond) -> Rankings {
    from_selected(apps.ranks().map_err(|missing| missing.to_string()))
}

/// The readings of `second` that Overview's readouts read, keyed as the worker's summary keys
/// them.
fn store_view_summary_of(second: &MachineSecond) -> Value {
    let memory = second.memory.as_ref();
    let pressure = memory.map(|memory| held_projection::pressure_level(memory.pressure));

    json!({
        "cpu_celsius": second.cpu_celsius,
        "cpu_percent": second.cpu_percent,
        "gpu_celsius": second.gpu_celsius,
        "gpu_active_percent": second.gpu_active_percent,
        "pressure": pressure,
        "power_watts": second.power_watts,
        "storage": {"read_bps": second.disk_read_bps},
        "network_rx_bps": second.network.map(|traffic| traffic.received_bps),
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
