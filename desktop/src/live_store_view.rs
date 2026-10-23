//! The page drawn from the store's newest View, which the window takes each tick. Overview draws
//! its machine charts, its CPU, memory, energy and disk charts by App, its readouts and its
//! hardware figure from it while it follows the newest second, and a chosen second, a hover's
//! reading and every other page draw from the worker's readings as before.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, Global};
use num_traits::ToPrimitive;
use serde_json::{Value, json};
use viewer::store::{AppsSecond, MachineSecond, Store, View};

use super::LiveDashboard;
use crate::data::Panel;
use crate::hardware::Readings;
use crate::live_app_attribution_history::{AppAttributionHistory, Metric};
use crate::live_hover::{HeldSecond, Target};
use crate::live_inventory::PerformanceLevel;
use crate::live_overview_app_histories::OverviewAppHistories;
use crate::live_projection::held_projection;
use crate::live_rankings::{Rankings, from_selected, stamp_identity};
use crate::live_resident_history::ResidentHistory;
use crate::live_source_mode::SourceMode;
use crate::live_thermal_pressure::thermal_pressure_level_text;
use crate::live_views::summary;
use crate::live_views::utilization::{Shares, UtilizationReadings, utilizations};

#[cfg(test)]
#[path = "live_store_view_tests.rs"]
mod tests;

const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;
/// The charts by App of a rate Overview draws from the store's View.
const APP_METRICS: [Metric; 3] = [Metric::Cpu, Metric::Power, Metric::Disk];

/// The store the app starts before its window opens and keeps for its life, so a window shown
/// or opened again draws what the store holds at once.
struct LiveStore(Store);

impl Global for LiveStore {}

pub fn install_store(store: Store, cx: &mut App) {
    cx.set_global(LiveStore(store));
}

/// The View the store handed over since the last take, or none where the app runs no store, as
/// for a recording opened read-only.
pub(super) fn take_store_view(cx: &App) -> Option<Arc<View>> {
    let store = cx.try_global::<LiveStore>()?;

    store.0.take()
}

/// The charts the page drew from a View, as the range zoomed them, so a reading that arrives
/// before the next View splices the same charts in again and their plots draw nothing.
#[derive(Default)]
pub(super) struct StorePanels {
    /// The version of the View they were drawn from, or none before the first.
    version: Option<u64>,
    panels: Vec<Rc<Panel>>,
    /// The wall second of each point of `panels`, in the order of their positions.
    seconds: Vec<u64>,
    /// The charts by App the View draws, of resident memory and of `APP_METRICS`, leaving out
    /// each it holds too little for.
    apps: Vec<Rc<Panel>>,
    /// The histories of `APP_METRICS` that `apps` draw, which say why a chart is left out, or
    /// none before the first View.
    histories: Option<OverviewAppHistories>,
    /// The history of resident memory `apps` draw, or none before the first View.
    resident: Option<ResidentHistory>,
    /// The top Apps of the View's newest second, or none before the first View.
    rankings: Option<Rankings>,
    /// Why the last View the page took could not be drawn, reported once.
    failure: Option<String>,
}

impl LiveDashboard {
    /// Takes `view`, the store's newest, and draws Overview's machine charts from it where the
    /// page draws them from the store. Answers whether the page changed.
    pub(crate) fn take_view(&mut self, view: Arc<View>) -> bool {
        take_view_report(self.store_view.as_deref(), &view);
        self.store_view = Some(view);
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
        let Some(view) = self.drawn_store_view() else {
            return false;
        };

        let version = view.version;
        if self.store_panels.version != Some(version) {
            match self.draw_store_view_panels(view) {
                Ok(panels) => self.store_panels = panels,
                Err(error) => {
                    self.draw_store_view_failed(error);
                    return false;
                }
            }
        }
        draw_store_view_splice(&mut self.panels, &self.store_panels.panels);
        draw_store_view_splice_apps(&mut self.panels, &self.store_panels.apps);
        true
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

    /// Lets go of the View the page could not draw, so the page shows the worker's readings
    /// whole and its error stays the worker's, and reports `error` once each time it changes.
    fn draw_store_view_failed(&mut self, error: String) {
        if self.store_panels.failure.as_ref() != Some(&error) {
            eprintln!("Flightdeck could not draw the store's View: {error}");
        }
        self.store_panels.failure = Some(error);
        self.store_view = None;
    }

    /// The charts of `view`, as the range zooms them, and the top Apps of its newest second.
    fn draw_store_view_panels(&self, view: &View) -> Result<StorePanels, String> {
        let levels = draw_store_view_panels_levels(view);
        let mut panels = crate::live_projection::overview_panels(view, levels.as_ref())?;
        let histories = draw_store_view_panels_histories(view);
        let resident = ResidentHistory::from_view(view);
        let rates = APP_METRICS
            .iter()
            .filter_map(|metric| histories.history(*metric)?.panel());
        let mut apps: Vec<Rc<Panel>> = resident.panel().into_iter().chain(rates).collect();
        crate::live_acceptance::prepare_overview_titles(&mut apps)?;

        if let Some(window) = self.view_window() {
            let sliding = self.panels_slide();
            for panel in panels.iter_mut().chain(&mut apps) {
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
            version: Some(view.version),
            panels,
            seconds,
            apps,
            histories: Some(histories),
            resident: Some(resident),
            rankings: Some(draw_store_view_rankings(view)),
            failure: None,
        })
    }

    /// The View the page draws Overview's machine charts, readouts and hardware figure from: the
    /// one `followed_store_view` names, while no hover the worker's readings show is shown.
    fn drawn_store_view(&self) -> Option<&View> {
        // A hover the worker's readings show holds the page still, as it holds those readings.
        if self
            .preview
            .as_ref()
            .is_some_and(|preview| preview.held.is_none())
        {
            return None;
        }

        self.followed_store_view()
    }

    /// The store's newest View, while the page shows the live Mac's Overview following the
    /// newest second and the View is no older than the worker's readings, or the page is opening
    /// before the worker's first reading, whatever hover is shown.
    fn followed_store_view(&self) -> Option<&View> {
        if self.request.route != "overview" {
            return None;
        }
        // The store follows the live Mac, so a window that opened a saved recording draws only
        // what its worker reads of it.
        if self.native.source != SourceMode::Live {
            return None;
        }
        if self.request.second.is_some() {
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
        if self.drawn_store_view().is_none() {
            return false;
        }

        let drawn = &self.store_panels;
        let mut panels = drawn.panels.iter().chain(&drawn.apps);
        panels.any(|drawn| drawn.id == panel.id)
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
        if self.drawn_store_view().is_some() {
            if let Some(histories) = &self.store_panels.histories {
                return histories.history(metric);
            }
        }

        self.snapshot.as_ref()?.attribution_history(metric)
    }

    /// The history of resident memory by App the page reads: the one the page drew from the
    /// store's View, where it draws one, and otherwise the one the worker read.
    pub(crate) fn resident_history(&self) -> Option<&ResidentHistory> {
        if self.drawn_store_view().is_some() {
            if let Some(resident) = &self.store_panels.resident {
                return Some(resident);
            }
        }

        self.snapshot.as_ref()?.resident_history.as_deref()
    }

    /// The second the page's readings show: the hovered one, or the newest, of the View the page
    /// draws, where it draws one, and otherwise the second the worker's reading, or a hover's,
    /// selected.
    pub(crate) fn shown_second(&self) -> Option<u64> {
        if let Some(second) = self.store_view_last_second() {
            if let Some(preview) = &self.preview {
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
    pub(super) fn store_view_last_second(&self) -> Option<u64> {
        let view = self.drawn_store_view()?;
        if self.store_view_hover_shown() {
            return self.store_panels.seconds.last().copied();
        }

        Some(view.seconds.last()?.wall_ns / NANOSECONDS_PER_SECOND)
    }

    /// The top Apps of the second the View the page draws shows, and that second: the one it
    /// held for the shown hover, or its newest while no hover is shown. None where the View holds
    /// no Apps of the hovered second, and where the page draws from the worker's readings.
    pub(crate) fn store_view_rankings(&self) -> Option<(&Rankings, u64)> {
        let second = self.store_view_last_second()?;
        if let Some(preview) = &self.preview {
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

    /// The second the View the page draws shows: the one it held for the shown hover, or its
    /// newest while no hover is shown. None in a gap, and where the page draws from the
    /// worker's readings.
    fn store_view_shown(&self) -> Option<&MachineSecond> {
        let view = self.drawn_store_view()?;
        let hovered = self
            .preview
            .as_ref()
            .and_then(|preview| preview.held.as_ref());

        match hovered {
            Some(held) => held.second.as_deref(),
            None => view.seconds.last().map(AsRef::as_ref),
        }
    }

    /// The hover of `second` on the page drawn from the store's View, naming the tick the View
    /// holds of it, so the charts by App find that tick as they find any they draw. None where
    /// the View holds no Apps of `second`, as in a gap or the older tail, and where the page
    /// draws from the worker's readings.
    pub(crate) fn store_view_target(&self, second: u64) -> Option<Target> {
        let view = self.followed_store_view()?;
        let at_second = |held: &Arc<MachineSecond>| held.wall_ns / NANOSECONDS_PER_SECOND == second;

        assert_eq!(view.apps.len(), view.seconds.len());
        let index = view.seconds.iter().rposition(at_second)?;
        let observation = view.apps[index].observation.as_ref()?;
        Some(Target {
            second,
            identity: Some(stamp_identity(&observation.stamp)),
        })
    }

    /// The second `target` names as the View the page draws holds it, whatever hover is shown,
    /// or none where the page draws from the worker's readings.
    pub(crate) fn store_view_held(&self, target: &Target) -> Option<HeldSecond> {
        let view = self.followed_store_view()?;
        let at_target =
            |second: &&Arc<MachineSecond>| second.wall_ns / NANOSECONDS_PER_SECOND == target.second;

        assert_eq!(view.apps.len(), view.seconds.len());
        if let Some(index) = view.seconds.iter().rposition(|second| at_target(&second)) {
            return Some(HeldSecond {
                second: Some(Arc::clone(&view.seconds[index])),
                rankings: Some(store_view_ranks(&view.apps[index])),
            });
        }
        // The store holds no Apps of the older tail's seconds.
        Some(HeldSecond {
            second: view.earlier.iter().rfind(at_target).map(Arc::clone),
            rankings: None,
        })
    }
}

/// The charts by App of `view`.
fn draw_store_view_panels_histories(view: &View) -> OverviewAppHistories {
    let history = |metric| Arc::new(AppAttributionHistory::from_view(metric, view));

    OverviewAppHistories {
        cpu: history(Metric::Cpu),
        power: history(Metric::Power),
        disk: history(Metric::Disk),
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

/// Puts `apps`, the charts by App the View draws, in place of the page's charts of resident
/// memory and of `APP_METRICS`, so a chart the View leaves out is left out and its card says why.
fn draw_store_view_splice_apps(panels: &mut Vec<Rc<Panel>>, apps: &[Rc<Panel>]) {
    let drawn_by_app = |panel: &Rc<Panel>| {
        if panel.id == crate::live_resident_history::PANEL_ID {
            return true;
        }
        let metric = Metric::from_panel(&panel.id);
        metric.is_some_and(|metric| APP_METRICS.contains(&metric))
    };

    panels.retain(|panel| !drawn_by_app(panel));
    panels.extend(apps.iter().map(Rc::clone));
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
