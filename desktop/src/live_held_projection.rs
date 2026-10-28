//! The Overview's machine charts drawn from the seconds the store holds, which the main thread
//! reads as values rather than as trend points.

use std::rc::Rc;

use collector::machine::Pressure;
use num_traits::ToPrimitive;
use viewer::store::{AppsSecond, AppsStretch, MachineSecond, View};

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

    let domain = overview_panels_domain(newest)?;
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

/// The wall second of each point the Overview's machine charts of `view` draw, in the order of
/// their positions.
pub fn overview_seconds(view: &View) -> Result<Vec<u64>, String> {
    let Some(newest) = view.seconds.last() else {
        return Ok(Vec::new());
    };

    let domain = overview_panels_domain(newest)?;
    let points = overview_panels_points(view, &domain);
    Ok(points
        .iter()
        .map(|second| second.wall_ns / NANOSECONDS_PER_SECOND)
        .collect())
}

/// The wall second of the point the Overview's machine charts of `view` draw for `second`:
/// `second` itself among the seconds they draw in full detail, and the one point of its stretch
/// before them, since every second of an older stretch shows the reading that stands for it.
/// None where the charts draw no point for `second`, as in a gap.
pub fn overview_point_second(view: &View, second: u64) -> Result<Option<u64>, String> {
    let Some(newest) = view.seconds.last() else {
        return Ok(None);
    };

    let domain = overview_panels_domain(newest)?;
    let points = overview_panels_points(view, &domain);
    Ok(points
        .iter()
        .map(|point| point.wall_ns / NANOSECONDS_PER_SECOND)
        .rfind(|point| overview_point_second_stands_for(&domain, *point, second)))
}

/// Whether the point the charts of `domain` draw at `point` stands for `second`.
fn overview_point_second_stands_for(domain: &Domain, point: u64, second: u64) -> bool {
    if second >= domain.recent_first {
        return point == second;
    }
    if point >= domain.recent_first {
        return false;
    }

    // Both divisions floor, as the stretches of the store and of the older tail do.
    point / domain.stride == second / domain.stride
}

/// The span the Overview's machine charts draw, ending at `newest`. The page always asks for the
/// older tail, so the charts span the loaded hour.
fn overview_panels_domain(newest: &MachineSecond) -> Result<Domain, String> {
    let older_stride = Some(viewer::EARLIER_STRIDE_SECONDS);

    Domain::ending_at(newest.wall_ns / NANOSECONDS_PER_SECOND, older_stride)
}

/// The seconds of `view` the charts of `domain` draw, oldest first: of the seconds older than
/// the full-detail ones, the first of each stretch the older tail keeps, and every full-detail
/// second after them.
fn overview_panels_points<'a>(view: &'a View, domain: &Domain) -> Vec<&'a MachineSecond> {
    overview_panels_indices(view, domain)
        .into_iter()
        .map(|index| overview_panels_second(view, index))
        .collect()
}

/// Where each second `overview_panels_points` gives sits among the seconds of `view`'s older
/// tail and then its live span.
fn overview_panels_indices(view: &View, domain: &Domain) -> Vec<usize> {
    // The older tail ends where the stretch that holds the first full-detail second starts, so
    // that stretch draws only its full-detail seconds. The divisions floor, as the store's and
    // the older tail's stretches do.
    let tail_stretch_end = domain.recent_first / domain.stride;

    let mut indices = Vec::with_capacity(view.earlier.len() + view.seconds.len());
    let mut stretch = None;
    for (index, second) in view.earlier.iter().chain(&view.seconds).enumerate() {
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

        indices.push(index);
    }

    assert!(indices.len() <= OBSERVATIONS_MAX);
    indices
}

/// The second at `index` among the seconds of `view`'s older tail and then its live span.
fn overview_panels_second(view: &View, index: usize) -> &MachineSecond {
    match index.checked_sub(view.earlier.len()) {
        Some(live) => &view.seconds[live],
        None => &view.earlier[index],
    }
}

/// What one point of Overview's App charts of a View shows: the Apps of a second of its live
/// span, or those of a stretch of its older part.
#[derive(Clone, Copy)]
pub enum OverviewApps<'view> {
    Second(&'view AppsSecond),
    Stretch(&'view AppsStretch),
}

/// One point of Overview's App charts of a View, drawn where its machine charts draw one.
pub struct OverviewAppPoint<'view> {
    /// The machine readings the machine charts draw at the point.
    pub machine: &'view MachineSecond,
    /// Where the point sits across the span the charts draw, from 0 to 1.
    pub position: f32,
    /// The segment of the machine charts' point, numbered from zero in the order the segments
    /// first appear.
    pub segment: u64,
    /// The Apps the point shows, or none where the View holds none for it.
    pub apps: Option<OverviewApps<'view>>,
}

impl OverviewAppPoint<'_> {
    /// Whether the point shows less than the whole of what its Apps used or held, because the
    /// recording kept none of them there.
    #[must_use]
    pub fn retention_limited(&self) -> bool {
        match self.apps {
            Some(OverviewApps::Second(apps)) => apps.missing.is_some(),
            Some(OverviewApps::Stretch(stretch)) => !stretch.holds_apps(),
            None => true,
        }
    }
}

/// The points of Overview's App charts of a View, and the span they are drawn across.
pub struct OverviewAppPoints<'view> {
    pub window: crate::chart_axes::TimeWindow,
    pub points: Vec<OverviewAppPoint<'view>>,
}

/// The points of Overview's App charts of `view`, which holds a second, following its newest
/// second: one wherever its machine charts draw a point, so an App chart spans the loaded hour as
/// they do.
pub fn overview_app_points(view: &View) -> Result<OverviewAppPoints<'_>, String> {
    let Some(newest) = view.seconds.last() else {
        unreachable!("the App charts draw a View that holds a second");
    };

    let domain = overview_panels_domain(newest)?;
    let indices = overview_panels_indices(view, &domain);
    let seconds: Vec<&MachineSecond> = indices
        .iter()
        .map(|index| overview_panels_second(view, *index))
        .collect();
    let coordinates = overview_panels_coordinates(&seconds, &domain)?;
    let apps = overview_app_points_apps(view, &seconds, &indices, &domain);

    assert_eq!(apps.len(), seconds.len());
    assert_eq!(coordinates.positions.len(), seconds.len());
    let points = seconds
        .into_iter()
        .zip(coordinates.positions)
        .zip(coordinates.segments)
        .zip(apps)
        .map(|(((machine, position), segment), apps)| OverviewAppPoint {
            machine,
            position,
            segment,
            apps,
        })
        .collect();
    Ok(OverviewAppPoints {
        window: coordinates.window,
        points,
    })
}

/// The Apps each of `seconds`, which `indices` place among the seconds of `view`'s older tail
/// and then its live span, shows on the App charts of `domain`: a full-detail second its own, an
/// older second those of its stretch, and none where the View holds none for it.
fn overview_app_points_apps<'view>(
    view: &'view View,
    seconds: &[&MachineSecond],
    indices: &[usize],
    domain: &Domain,
) -> Vec<Option<OverviewApps<'view>>> {
    assert_eq!(seconds.len(), indices.len());
    assert_eq!(domain.stride, viewer::EARLIER_STRIDE_SECONDS);

    // The store's stretches run oldest first, as the points do.
    let mut stretches = view.stretches.iter().peekable();
    let mut apps = Vec::with_capacity(seconds.len());
    for (second, index) in seconds.iter().zip(indices) {
        let wall_second = second.wall_ns / NANOSECONDS_PER_SECOND;
        if wall_second < domain.recent_first {
            let stretch = viewer::store::stretch_of_wall_second(wall_second);
            while stretches.next_if(|held| held.stretch < stretch).is_some() {}
            let held = stretches.next_if(|held| held.stretch == stretch);
            apps.push(held.map(|held| OverviewApps::Stretch(held)));
        } else {
            let live = index.checked_sub(view.earlier.len());
            apps.push(live.map(|live| OverviewApps::Second(&view.apps[live])));
        }
    }
    apps
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
        (None, "pressure", None) => memory.map(|memory| f32::from(pressure_level(memory.pressure))),
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

/// The level the recording writes for `pressure`, which the memory pressure chart and readout
/// read.
pub(crate) fn pressure_level(pressure: Pressure) -> u8 {
    match pressure {
        Pressure::Normal => 0,
        Pressure::Warning => 1,
        Pressure::Critical => 2,
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
