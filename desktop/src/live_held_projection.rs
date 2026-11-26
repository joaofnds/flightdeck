//! A route's machine charts drawn from the seconds the store holds, which the main thread reads
//! as values rather than as trend points.

use std::rc::Rc;
use std::sync::Arc;

use num_traits::ToPrimitive;
use viewer::store::{
    AppsStretch, CompleteMachine, Extreme, Highest, MachinePlace, MachineSecond, MachineStretch,
    MemoryRate, NetworkRate, PointRate, Second, StorageRate, View,
};

use super::{
    Coordinates, Domain, FAN_MEAN_KEY, LineReach, NANOSECONDS_PER_SECOND, OBSERVATIONS_MAX,
    Observations, Panel, PerformanceLevel, Reading, Specification,
};

#[cfg(test)]
#[path = "live_held_projection_tests.rs"]
mod tests;

/// The machine charts of `route` drawn from `view` over the loaded hour, following its newest
/// second, which the charts select. `levels` names the performance levels Overview's CPU chart
/// draws.
pub fn view_panels(
    route: &str,
    view: &View,
    levels: Option<&[PerformanceLevel; 2]>,
) -> Result<Vec<Rc<Panel>>, String> {
    let Some(newest) = view.seconds.last() else {
        // A route the store draws no charts of fails here too.
        view_panels_specifications(route, levels, &[])?;
        return super::empty_panels(route);
    };

    let domain = view_panels_domain(newest)?;
    let seconds = view_panels_points(view, &domain);
    let specifications = view_panels_specifications(route, levels, &seconds)?;
    let coordinates = view_panels_coordinates(&seconds, &domain)?;
    let stretches = view_panels_stretches(view, &seconds, &domain);
    let points = ViewPanelsPoints {
        seconds: &seconds,
        stretches: &stretches,
    };

    let mut result = Vec::with_capacity(specifications.len());
    for specification in &specifications {
        let panel = super::panels_panel(specification, &domain, &points, &coordinates)?;
        result.push(Rc::new(panel));
    }
    assert!(result.len() <= super::panels_limit(route, false));
    Ok(result)
}

/// The charts of `route` the store draws of `seconds`, which are the charts the trend points of
/// those seconds draw of it.
fn view_panels_specifications(
    route: &str,
    levels: Option<&[PerformanceLevel; 2]>,
    seconds: &[&MachineSecond],
) -> Result<Vec<Specification>, String> {
    match route {
        "overview" => Ok(super::overview(levels)),
        "cpu" => Ok(super::cpu_specifications(levels)),
        "memory" => Ok(super::memory_specifications()),
        "network" => Ok(super::network()),
        "storage" => Ok(super::storage_specifications()),
        "gpu" => Ok(super::gpu_specifications()),
        "energy" => super::energy_specifications(view_panels_fan_channel_count(seconds)),
        _ => Err(format!("The store draws no charts of {route}")),
    }
}

/// How many fan channels the fan chart of `seconds` draws: as many as the longest complete
/// reading of their fans holds, as the trend points of those seconds count them.
fn view_panels_fan_channel_count(seconds: &[&MachineSecond]) -> usize {
    seconds
        .iter()
        .filter_map(|second| second.complete().and_then(CompleteMachine::fan_rpm))
        .map(<[_]>::len)
        .max()
        .unwrap_or(0)
}

/// The wall second of each point the Overview's machine charts of `view` draw, in the order of
/// their positions.
pub fn overview_seconds(view: &View) -> Result<Vec<u64>, String> {
    let Some(newest) = view.seconds.last() else {
        return Ok(Vec::new());
    };

    let domain = view_panels_domain(newest)?;
    let points = view_panels_points(view, &domain);
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

    let domain = view_panels_domain(newest)?;
    let points = view_panels_points(view, &domain);
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
fn view_panels_domain(newest: &MachineSecond) -> Result<Domain, String> {
    let older_stride = Some(viewer::EARLIER_STRIDE_SECONDS);

    Domain::ending_at(newest.wall_ns / NANOSECONDS_PER_SECOND, older_stride)
}

/// The seconds of `view` the charts of `domain` draw, oldest first: of the seconds older than
/// the full-detail ones, the first of each stretch the older tail keeps, and every full-detail
/// second after them.
fn view_panels_points<'a>(view: &'a View, domain: &Domain) -> Vec<&'a MachineSecond> {
    view_panels_indices(view, domain)
        .into_iter()
        .map(|index| view_panels_second(view, index))
        .collect()
}

/// Where each second `view_panels_points` gives sits among the seconds of `view`'s older
/// tail and then its live span.
fn view_panels_indices(view: &View, domain: &Domain) -> Vec<usize> {
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

/// What each of the seconds the charts of `domain` draw stands for: the stretch of a point older
/// than the full-detail seconds, where the store holds it, and only itself after them. The first
/// of those also stands for the seconds before it that lie in no point, which are of its stretch.
fn view_panels_stretches<'view>(
    view: &'view View,
    seconds: &[&MachineSecond],
    domain: &Domain,
) -> Vec<Option<&'view MachineStretch>> {
    assert_eq!(domain.stride, viewer::EARLIER_STRIDE_SECONDS);

    let mut stretches = view.stretches.iter().peekable();
    let mut held = Vec::with_capacity(seconds.len());
    for second in seconds {
        // Each full-detail second asks for the stretch at the join, which only the first finds.
        let wall_second = (second.wall_ns / NANOSECONDS_PER_SECOND).min(domain.recent_first);
        let stretch = overview_stretch(&mut stretches, wall_second);
        held.push(stretch.map(|stretch| &*stretch.machine));
    }

    held
}

/// The stretch `stretches` holds for `wall_second`. They run oldest first, as the points do, so
/// the stretches before it are passed for good.
fn overview_stretch<'view>(
    stretches: &mut std::iter::Peekable<std::slice::Iter<'view, Arc<AppsStretch>>>,
    wall_second: u64,
) -> Option<&'view Arc<AppsStretch>> {
    let stretch = viewer::store::stretch_of_wall_second(wall_second);
    while stretches.next_if(|held| held.stretch < stretch).is_some() {}

    stretches.next_if(|held| held.stretch == stretch)
}

/// The seconds a chart drawn from the store draws, with what each of them stands for.
struct ViewPanelsPoints<'view> {
    seconds: &'view [&'view MachineSecond],
    stretches: &'view [Option<&'view MachineStretch>],
}

impl Observations for ViewPanelsPoints<'_> {
    fn count(&self) -> usize {
        self.seconds.len()
    }

    fn wall_ns(&self, index: usize) -> Result<u64, String> {
        Ok(self.seconds[index].wall_ns)
    }

    fn value(&self, reading: &Reading, index: usize) -> Result<Option<f32>, String> {
        let value = second_value(reading, self.seconds[index])?;
        if let Some(value) = value {
            if !value.is_finite() {
                return Err("Nonfinite live chart quantity".to_owned());
            }
        }

        Ok(value)
    }

    fn reach(&self, reading: &Reading, index: usize) -> Result<Option<LineReach>, String> {
        let Some(stretch) = self.stretches[index] else {
            return Ok(None);
        };

        stretch_reach(reading, stretch)
    }
}

/// The second at `index` among the seconds of `view`'s older tail and then its live span.
fn view_panels_second(view: &View, index: usize) -> &MachineSecond {
    match index.checked_sub(view.earlier.len()) {
        Some(live) => &view.seconds[live],
        None => &view.earlier[index],
    }
}

/// What one point of Overview's App charts of a View shows: the Apps of a second of its live
/// span, or those of a stretch of its older part.
#[derive(Clone, Copy)]
pub enum OverviewApps<'view> {
    Second(Second<'view>),
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
            Some(OverviewApps::Second(apps)) => apps.missing().is_some(),
            Some(OverviewApps::Stretch(stretch)) => !stretch.holds_apps(),
            None => true,
        }
    }
}

/// The points of Overview's App charts of a View, the span they are drawn across, and the first
/// second of its full-detail points, before which they stand a stretch apart.
pub struct OverviewAppPoints<'view> {
    pub window: crate::chart_axes::TimeWindow,
    pub detail_first: u64,
    pub points: Vec<OverviewAppPoint<'view>>,
}

/// The points of Overview's App charts of `view`, which holds a second, following its newest
/// second: one wherever its machine charts draw a point, so an App chart spans the loaded hour as
/// they do.
pub fn overview_app_points(view: &View) -> Result<OverviewAppPoints<'_>, String> {
    let Some(newest) = view.seconds.last() else {
        unreachable!("the App charts draw a View that holds a second");
    };

    let domain = view_panels_domain(newest)?;
    let indices = view_panels_indices(view, &domain);
    let seconds: Vec<&MachineSecond> = indices
        .iter()
        .map(|index| view_panels_second(view, *index))
        .collect();
    let coordinates = view_panels_coordinates(&seconds, &domain)?;
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
        detail_first: coordinates.detail_first,
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

    // The indices rise, so one walk of the live span meets each live second they name.
    assert!(indices.is_sorted_by(|before, after| before < after));

    let mut stretches = view.stretches.iter().peekable();
    let mut live_seconds = view.apps.seconds().enumerate();
    let mut apps = Vec::with_capacity(seconds.len());
    for (second, index) in seconds.iter().zip(indices) {
        let wall_second = second.wall_ns / NANOSECONDS_PER_SECOND;
        if wall_second < domain.recent_first {
            let held = overview_stretch(&mut stretches, wall_second);
            apps.push(held.map(|held| OverviewApps::Stretch(held)));
        } else {
            let live = index.checked_sub(view.earlier.len());
            let second = live.map(|live| {
                let Some((_, second)) = live_seconds.find(|(index, _)| *index == live) else {
                    unreachable!("the live span holds the second an index names");
                };
                second
            });
            apps.push(second.map(OverviewApps::Second));
        }
    }
    apps
}

/// Where each of `points` sits in `domain`, and its segment, numbered from zero in the order
/// the segments first appear, as the trend points number theirs. The newest point is selected.
fn view_panels_coordinates(
    points: &[&MachineSecond],
    domain: &Domain,
) -> Result<Coordinates, String> {
    let mut positions = Vec::with_capacity(points.len());
    let mut segments = Vec::with_capacity(points.len());
    let mut places = Vec::with_capacity(points.len());
    let mut segment_count = 0_u64;
    let mut previous = None;
    for second in points {
        positions.push(domain.position(second.wall_ns)?);
        places.push(MachinePlace {
            boot: second.boot,
            run: second.run,
            publication: second.publication,
            wall_ns: second.wall_ns,
        });
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
        places: places.into(),
        selected_index: points.len().checked_sub(1),
    })
}

/// How high `reading` reached over `stretch`, as `second_value` reads one second. A
/// category has no height between its levels, so it reaches its highest level alone.
fn stretch_reach(reading: &Reading, stretch: &MachineStretch) -> Result<Option<LineReach>, String> {
    let key = reading.reach.unwrap_or(reading.key);
    let reach = match (reading.resource, key, reading.channel) {
        (None, "cpu_percent", None) => stretch_reach_measured(&stretch.cpu_percent),
        (None, "cpu_level_percent", Some(level)) => stretch
            .cpu_level_percent
            .get(level)
            .and_then(|percent| stretch_reach_measured(percent)),
        (None, "cpu_level_ghz", Some(level)) => stretch
            .cpu_level_ghz
            .get(level)
            .and_then(|ghz| stretch_reach_measured(ghz)),
        (None, "load_average", Some(window)) => stretch
            .load_average
            .get(window)
            .and_then(stretch_reach_measured),
        (None, "open_files", None) => stretch_reach_measured(&stretch.open_files),
        (None, "open_files_percent", None) => stretch_reach_measured(&stretch.open_files_percent),
        (None, "cpu_celsius", None) => stretch_reach_measured(&stretch.cpu_celsius),
        (None, "gpu_active_percent", None) => stretch_reach_measured(&stretch.gpu_active_percent),
        (None, "gpu_watts", None) => stretch_reach_measured(&stretch.gpu_watts),
        (None, "gpu_memory_in_use_bytes", None) => {
            stretch_reach_measured(&stretch.gpu_memory_in_use_bytes)
        }
        (None, "gpu_memory_allocated_bytes", None) => {
            stretch_reach_measured(&stretch.gpu_memory_allocated_bytes)
        }
        (None, "gpu_celsius", None) => stretch_reach_measured(&stretch.gpu_celsius),
        // No reading records the GPU's frequency, so its chart draws none. The count of cores
        // is fixed for the machine, so it never moves over a stretch, and the older tail keeps
        // no reach for a number that did not move.
        (None, "gpu_frequency_ghz" | "logical_cpu_count", None) => None,
        (None, "memory_app_bytes", None) => stretch_reach_measured(&stretch.memory_app_bytes),
        (None, "memory_wired_bytes", None) => stretch_reach_measured(&stretch.memory_wired_bytes),
        (None, "memory_compressed_bytes", None) => {
            stretch_reach_measured(&stretch.memory_compressed_bytes)
        }
        (None, "memory_total_bytes", None) => stretch_reach_measured(&stretch.memory_total_bytes),
        (None, "memory_used_bytes", None) => stretch_reach_measured(&stretch.memory_used_bytes),
        (None, "swap_used_bytes", None) => stretch_reach_measured(&stretch.swap_used_bytes),
        (None, "swap_total_bytes", None) => stretch_reach_measured(&stretch.swap_total_bytes),
        (None, key, None) if let Some(rate) = rate_named::<MemoryRate>(key) => {
            stretch_reach_measured(stretch.memory_rate(rate))
        }
        (None, "pageout_bytes_per_second" | "memory_area_celsius", None) => None,
        (None, "network_rx_bps", None) => stretch_reach_measured(&stretch.network_received_bps),
        (None, "network_tx_bps", None) => stretch_reach_measured(&stretch.network_sent_bps),
        (None, key, None) if let Some(rate) = rate_named::<NetworkRate>(key) => {
            stretch_reach_measured(stretch.network_rate(rate))
        }
        // No reading records traffic by App or the storage area's temperature, so their charts
        // draw none.
        (None, "network_apps_bytes_per_second" | "storage_area_celsius", None) => None,
        (Some("storage"), "read_bps", None) => stretch_reach_measured(&stretch.disk_read_bps),
        (Some("storage"), "write_bps", None) => stretch_reach_measured(&stretch.disk_written_bps),
        (Some("storage"), key, None) if let Some(rate) = rate_named::<StorageRate>(key) => {
            stretch_reach_measured(stretch.storage_rate(rate))
        }
        (None, "pressure", None) => stretch_reach_category(stretch.pressure.highest()),
        (None, key, channel) => return stretch_reach_power_and_cooling(key, channel, stretch),
        _ => return Err(format!("The store reduces no reading {key}")),
    };

    Ok(reach)
}

/// How high the power and cooling reading `key`, on `channel`, reached over `stretch`. The
/// Energy page's charts read these, and so do Overview's power, thermal state and correlation
/// charts. `stretch_reach` sends here every key no other arm names, so this is also where a key
/// naming none of the readings the store reduces gets its error.
fn stretch_reach_power_and_cooling(
    key: &str,
    channel: Option<usize>,
    stretch: &MachineStretch,
) -> Result<Option<LineReach>, String> {
    let reach = match (key, channel) {
        ("power_watts", None) => stretch_reach_measured(&stretch.power_watts),
        ("soc_celsius", None) => stretch_reach_measured(&stretch.soc_celsius),
        (FAN_MEAN_KEY, None) => stretch_reach_measured(&stretch.fan_mean_rpm),
        ("fan_rpm", Some(channel)) => stretch
            .fan_rpm
            .get(channel)
            .and_then(stretch_reach_measured),
        // The stacked power chart reaches the system power, which its GPU line names, so the
        // remainder's own line reaches nothing of its own. The collector records no area's
        // hottest sensor, so their chart draws none.
        (
            "power_remainder_watts"
            | "soc_max_celsius"
            | "voltage_regulator_celsius"
            | "display_celsius"
            | "board_celsius"
            | "wireless_celsius"
            | "ambient_celsius",
            None,
        ) => None,
        ("thermal_pressure", None) => stretch_reach_category(stretch.thermal_pressure.highest()),
        _ => return Err(format!("The store reduces no reading {key}")),
    };

    Ok(reach)
}

fn stretch_reach_measured(series: &Highest<MachinePlace>) -> Option<LineReach> {
    let highest = series.highest()?;

    Some(LineReach {
        high: highest.value.to_f32()?,
        peak: highest.place,
    })
}

fn stretch_reach_category(highest: Option<&Extreme<u8, MachinePlace>>) -> Option<LineReach> {
    let highest = highest?;
    let level = f32::from(highest.value);

    Some(LineReach {
        high: level,
        peak: highest.place,
    })
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
        (None, "cpu_level_ghz", Some(level)) => narrow(
            second
                .cpu_level_ghz()
                .and_then(|ghz| ghz.get(level).copied().flatten()),
        ),
        (None, "load_average", Some(window)) => narrow(
            second
                .load_average()
                .and_then(|load| load.get(window).copied()),
        ),
        (None, "logical_cpu_count", None) => second_value_logical_cpu_count(second),
        (None, "open_files", None) => second.open_files().and_then(|files| files.to_f32()),
        (None, "open_files_percent", None) => narrow(second.open_files_percent()),
        (None, "gpu_active_percent", None) => narrow(second.gpu_active_percent),
        (None, "gpu_watts", None) => narrow(second.gpu_watts()),
        (None, "gpu_memory_in_use_bytes", None) => bytes(second.gpu_memory_in_use_bytes()),
        (None, "gpu_memory_allocated_bytes", None) => bytes(second.gpu_memory_allocated_bytes()),
        (None, "gpu_celsius", None) => second.gpu_celsius,
        // The collector does not record the GPU's frequency, so its chart draws no reading
        // wherever it is drawn.
        (None, "gpu_frequency_ghz", None) => None,
        (None, "memory_app_bytes", None) => bytes(memory.and_then(|memory| memory.app_bytes)),
        (None, "memory_wired_bytes", None) => bytes(memory.map(|memory| memory.wired_bytes)),
        (None, "memory_compressed_bytes", None) => {
            bytes(memory.map(|memory| memory.compressed_bytes))
        }
        (None, "memory_total_bytes", None) => bytes(memory.map(|memory| memory.total_bytes)),
        (None, "pressure", None) => memory.map(|memory| f32::from(pressure_level(memory.pressure))),
        (None, "swap_used_bytes", None) => bytes(memory.map(|memory| memory.swap_used_bytes)),
        (None, "swap_total_bytes", None) => bytes(memory.map(|memory| memory.swap_total_bytes)),
        (None, key, None) if let Some(rate) = rate_named::<MemoryRate>(key) => {
            narrow(second.memory_rate(rate))
        }
        // The collector records neither, so their charts draw no reading wherever they are drawn.
        (None, "pageout_bytes_per_second" | "memory_area_celsius", None) => None,
        (None, "network_rx_bps", None) => narrow(network.map(|traffic| traffic.received_bps)),
        (None, "network_tx_bps", None) => narrow(network.map(|traffic| traffic.sent_bps)),
        (None, key, None) if let Some(rate) = rate_named::<NetworkRate>(key) => {
            narrow(second.network_rate(rate))
        }
        // No reading records traffic by App or the storage area's temperature, so their charts
        // draw none wherever they are drawn.
        (None, "network_apps_bytes_per_second" | "storage_area_celsius", None) => None,
        (Some("storage"), "read_bps", None) => narrow(second.disk_read_bps),
        (Some("storage"), "write_bps", None) => narrow(second.disk_written_bps),
        (Some("storage"), key, None) if let Some(rate) = rate_named::<StorageRate>(key) => {
            narrow(second.storage_rate(rate))
        }
        (None, "cpu_celsius", None) => second.cpu_celsius,
        (None, key, channel) => return second_value_power_and_cooling(key, channel, second),
        _ => return Err(format!("The store holds no reading {}", reading.key)),
    };

    Ok(value)
}

/// What the power and cooling reading `key`, on `channel`, reads of `second`, as `second_value`
/// reads it. The Energy page's charts read these, and so do Overview's power, thermal state and
/// correlation charts. `second_value` sends here every key no other arm names, so this is also
/// where a key naming none of the readings the store holds gets its error.
fn second_value_power_and_cooling(
    key: &str,
    channel: Option<usize>,
    second: &MachineSecond,
) -> Result<Option<f32>, String> {
    let value = match (key, channel) {
        ("power_watts", None) => second.power_watts,
        ("power_remainder_watts", None) => {
            super::power_remainder_watts(second.power_watts, narrow(second.gpu_watts()))?
        }
        ("soc_celsius", None) => second.soc_celsius,
        (FAN_MEAN_KEY, None) => second.fan_mean_rpm,
        ("fan_rpm", Some(channel)) => narrow(
            second
                .complete()
                .and_then(CompleteMachine::fan_rpm)
                .and_then(|fans| fans.get(channel).copied().flatten()),
        ),
        ("thermal_pressure", None) => second.thermal_pressure.map(f32::from),
        // The collector records no area's hottest sensor, so their chart draws no reading
        // wherever it is drawn.
        (
            "soc_max_celsius"
            | "voltage_regulator_celsius"
            | "display_celsius"
            | "board_celsius"
            | "wireless_celsius"
            | "ambient_celsius",
            None,
        ) => None,
        _ => return Err(format!("The store holds no reading {key}")),
    };

    Ok(value)
}

/// The count of logical CPUs `second` reads, the per-CPU readings it holds, as the reader
/// counts them, whether the second's complete reading was taken or refused.
fn second_value_logical_cpu_count(second: &MachineSecond) -> Option<f32> {
    second
        .cpu_counters()
        .and_then(|counters| counters.len().to_f32())
}

pub(crate) use viewer::store::pressure_level;

/// The rate of the family `Family` a trend point names `key`, which only a second's complete
/// reading holds: memory's counters, the network's packets and errors, or the disks' operations,
/// errors and service times inside the point's `storage`.
fn rate_named<Family: PointRate>(key: &str) -> Option<Family> {
    Family::RATES.iter().copied().find(|rate| rate.key() == key)
}

/// The nearest `f32` to `value`.
fn narrow(value: Option<f64>) -> Option<f32> {
    value.and_then(|value| value.to_f32())
}

/// The nearest `f32` to a count of bytes.
fn bytes(value: Option<u64>) -> Option<f32> {
    value.and_then(|value| value.to_f32())
}
