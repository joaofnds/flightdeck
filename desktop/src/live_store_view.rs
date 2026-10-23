//! The page drawn from the store's newest View, which the window takes each tick. Overview draws
//! its machine charts, readouts and hardware figure from it while it follows the newest second,
//! and a chosen second, a hover's reading and every other page draw from the worker's readings
//! as before.

use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, Global};
use num_traits::ToPrimitive;
use serde_json::{Value, json};
use viewer::store::{MachineSecond, Store, View};

use super::LiveDashboard;
use crate::data::Panel;
use crate::hardware::Readings;
use crate::live_inventory::PerformanceLevel;
use crate::live_projection::held_projection;
use crate::live_thermal_pressure::thermal_pressure_level_text;
use crate::live_views::summary;

#[cfg(test)]
#[path = "live_store_view_tests.rs"]
mod tests;

const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;

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

/// The machine charts the page drew from a View, as the range zoomed them, so a reading that
/// arrives before the next View splices the same charts in again and their plots draw nothing.
#[derive(Default)]
pub(super) struct StorePanels {
    /// The version of the View they were drawn from, or none before the first.
    version: Option<u64>,
    panels: Vec<Rc<Panel>>,
}

impl LiveDashboard {
    /// Takes `view`, the store's newest, and draws Overview's machine charts from it where the
    /// page draws them from the store. Answers whether the page changed.
    pub(crate) fn take_view(&mut self, view: Arc<View>) -> bool {
        take_view_report(self.store_view.as_deref(), &view);
        self.store_view = Some(view);
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
                Ok(panels) => {
                    self.store_panels = StorePanels {
                        version: Some(version),
                        panels,
                    };
                }
                Err(error) => {
                    self.error = Some(error);
                    return false;
                }
            }
        }
        draw_store_view_splice(&mut self.panels, &self.store_panels.panels);
        true
    }

    /// The machine charts of `view`, as the range zooms them.
    fn draw_store_view_panels(&self, view: &View) -> Result<Vec<Rc<Panel>>, String> {
        let levels = draw_store_view_panels_levels(view);
        let mut panels = crate::live_projection::overview_panels(view, levels.as_ref())?;

        if let Some(window) = self.view_window() {
            let sliding = self.panels_slide();
            for panel in &mut panels {
                super::zoom_panel(panel, window, sliding);
            }
        }
        Ok(panels)
    }

    /// The View the page draws Overview's machine charts, readouts and hardware figure from: the
    /// store's newest, while the page shows Overview's readings following the newest second, no
    /// hover's reading is shown, and the View is no older than the readings.
    fn drawn_store_view(&self) -> Option<&View> {
        if self.request.route != "overview" {
            return None;
        }
        if self.request.second.is_some() {
            return None;
        }
        // A hover holds the page still, as it holds the worker's readings.
        if self.preview.is_some() {
            return None;
        }

        let selection = &self.snapshot.as_ref()?.selection;
        let readings_last = crate::live_projection::context_last_second(selection)?;
        let view = self.store_view.as_deref()?;
        let view_last = view.seconds.last()?.wall_ns / NANOSECONDS_PER_SECOND;
        // A store that falls behind the worker draws nothing, so the charts never hold less
        // than the worker read.
        if view_last < readings_last {
            return None;
        }
        Some(view)
    }

    /// The newest second of the View the page draws, where it draws one, which the range ends
    /// at since the worker's readings may not hold it yet.
    pub(super) fn store_view_last_second(&self) -> Option<u64> {
        let view = self.drawn_store_view()?;

        Some(view.seconds.last()?.wall_ns / NANOSECONDS_PER_SECOND)
    }

    /// The readings Overview's readouts read of the newest second of the View the page draws,
    /// where it draws one, keyed as the worker's summary of a second keys them.
    pub(crate) fn store_view_summary(&self) -> Option<Value> {
        let view = self.drawn_store_view()?;

        Some(store_view_summary_of(view.seconds.last()?))
    }

    /// The hardware figure's readings of the newest second of the View the page draws, where it
    /// draws one.
    pub(crate) fn store_view_hardware(&self) -> Option<Readings> {
        let view = self.drawn_store_view()?;

        Some(store_view_hardware_of(view.seconds.last()?))
    }
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

/// The CPU's performance levels as the newest second of `view` names them.
fn draw_store_view_panels_levels(view: &View) -> Option<[PerformanceLevel; 2]> {
    let [fastest, slower] = view.seconds.last()?.performance_levels.as_ref()?;
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
