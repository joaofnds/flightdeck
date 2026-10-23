//! The Overview's machine charts drawn from the seconds the store holds, which the main thread
//! reads as values rather than as trend points.

use std::rc::Rc;

use collector::machine::Pressure;
use num_traits::ToPrimitive;
use viewer::store::{MachineSecond, View};

use super::{
    Coordinates, Domain, FAN_MEAN_KEY, NANOSECONDS_PER_SECOND, OBSERVATIONS_MAX,
    OVERVIEW_PANELS_MAX, Observations, Panel, PerformanceLevel, Reading,
};

#[cfg(test)]
#[path = "live_held_projection_tests.rs"]
mod tests;

/// The Overview's machine charts of `view`, following its newest second, which the charts
/// select. `levels` names the performance levels the CPU chart draws.
pub fn overview_panels(
    view: &View,
    levels: Option<&[PerformanceLevel; 2]>,
) -> Result<Vec<Rc<Panel>>, String> {
    let Some(newest) = view.seconds.last() else {
        return super::empty_panels("overview");
    };

    // The page always asks for the older tail, so the charts span the loaded hour.
    let older_stride = Some(viewer::EARLIER_STRIDE_SECONDS);
    let domain = Domain::ending_at(newest.wall_ns / NANOSECONDS_PER_SECOND, older_stride)?;
    let points = overview_panels_points(view, &domain);
    let coordinates = overview_panels_coordinates(&points, &domain)?;
    let specifications = super::overview(levels);

    let mut result = Vec::with_capacity(specifications.len());
    for specification in &specifications {
        let panel = super::panels_panel(specification, &domain, points.as_slice(), &coordinates)?;
        result.push(Rc::new(panel));
    }
    assert!(result.len() <= OVERVIEW_PANELS_MAX);
    Ok(result)
}

/// The seconds of `view` the charts of `domain` draw, oldest first: of the seconds older than
/// the full-detail ones, the first of each stretch the older tail keeps, and every full-detail
/// second after them.
fn overview_panels_points<'a>(view: &'a View, domain: &Domain) -> Vec<&'a MachineSecond> {
    // The older tail ends where the stretch that holds the first full-detail second starts, so
    // that stretch draws only its full-detail seconds. The divisions floor, as the store's and
    // the older tail's stretches do.
    let tail_stretch_end = domain.recent_first / domain.stride;

    let mut points = Vec::with_capacity(view.earlier.len() + view.seconds.len());
    let mut stretch = None;
    for second in view.earlier.iter().chain(&view.seconds) {
        let wall_second = second.wall_ns / NANOSECONDS_PER_SECOND;
        if !(domain.first..domain.end).contains(&wall_second) {
            continue;
        }
        if wall_second < domain.recent_first {
            let current = wall_second / domain.stride;
            if current >= tail_stretch_end {
                continue;
            }
            if stretch == Some(current) {
                continue;
            }
            stretch = Some(current);
        }

        points.push(second.as_ref());
    }

    assert!(points.len() <= OBSERVATIONS_MAX);
    points
}

/// Where each of `points` sits in `domain`, and its segment, numbered from zero in the order
/// the segments first appear, as the trend points number theirs. The newest point is selected.
fn overview_panels_coordinates(
    points: &[&MachineSecond],
    domain: &Domain,
) -> Result<Coordinates, String> {
    let mut positions = Vec::with_capacity(points.len());
    let mut segments = Vec::with_capacity(points.len());
    let mut segment_count = 0_u64;
    let mut previous = None;
    for second in points {
        positions.push(domain.position(second.wall_ns)?);
        if previous != Some(second.segment) {
            segment_count += 1;
            previous = Some(second.segment);
        }
        segments.push(segment_count - 1);
    }

    Ok(Coordinates {
        window: crate::chart_axes::TimeWindow {
            first: domain.first,
            span: u64::from(domain.span),
        },
        detail_first: domain.recent_first,
        positions,
        segments,
        selected_index: points.len().checked_sub(1),
    })
}

impl Observations for [&MachineSecond] {
    fn count(&self) -> usize {
        self.len()
    }

    fn wall_ns(&self, index: usize) -> Result<u64, String> {
        Ok(self[index].wall_ns)
    }

    fn value(&self, reading: &Reading, index: usize) -> Result<Option<f32>, String> {
        let value = second_value(reading, self[index])?;
        if let Some(value) = value {
            if !value.is_finite() {
                return Err("Nonfinite live chart quantity".to_owned());
            }
        }

        Ok(value)
    }
}

/// What `reading` reads of `second`, as it reads the trend point of that second.
fn second_value(reading: &Reading, second: &MachineSecond) -> Result<Option<f32>, String> {
    let memory = second.memory.as_ref();
    let network = second.network.as_ref();
    let value = match (reading.resource, reading.key, reading.channel) {
        (None, "cpu_percent", None) => narrow(second.cpu_percent),
        (None, "cpu_level_percent", Some(level)) => {
            narrow(second.cpu_level_percent.get(level).copied().flatten())
        }
        (None, "gpu_active_percent", None) => narrow(second.gpu_active_percent),
        (None, "memory_app_bytes", None) => bytes(memory.and_then(|memory| memory.app_bytes)),
        (None, "memory_wired_bytes", None) => bytes(memory.map(|memory| memory.wired_bytes)),
        (None, "memory_compressed_bytes", None) => {
            bytes(memory.map(|memory| memory.compressed_bytes))
        }
        (None, "memory_total_bytes", None) => bytes(memory.map(|memory| memory.total_bytes)),
        (None, "pressure", None) => memory.map(|memory| pressure_level(memory.pressure)),
        (None, "network_rx_bps", None) => narrow(network.map(|traffic| traffic.received_bps)),
        (None, "network_tx_bps", None) => narrow(network.map(|traffic| traffic.sent_bps)),
        (Some("storage"), "read_bps", None) => narrow(second.disk_read_bps),
        (Some("storage"), "write_bps", None) => narrow(second.disk_written_bps),
        (None, "power_watts", None) => second.power_watts,
        (None, "soc_celsius", None) => second.soc_celsius,
        (None, FAN_MEAN_KEY, None) => second.fan_mean_rpm,
        (None, "thermal_pressure", None) => second.thermal_pressure.map(f32::from),
        _ => return Err(format!("The store holds no reading {}", reading.key)),
    };

    Ok(value)
}

/// The level the recording writes for `pressure`, which the memory pressure chart reads.
fn pressure_level(pressure: Pressure) -> f32 {
    match pressure {
        Pressure::Normal => 0.0,
        Pressure::Warning => 1.0,
        Pressure::Critical => 2.0,
    }
}

/// The nearest `f32` to `value`.
fn narrow(value: Option<f64>) -> Option<f32> {
    value.and_then(|value| value.to_f32())
}

/// The nearest `f32` to a count of bytes.
fn bytes(value: Option<u64>) -> Option<f32> {
    value.and_then(|value| value.to_f32())
}
