use std::collections::btree_map::Entry;
use std::collections::{BTreeMap, BTreeSet};

use persistence::{Error, Result};
use serde_json::{Value, json};

use crate::cpu_history_sample::{CapturedInterval, ObservedCounters, ObservedInterval};
use crate::metric_view::{Sources, View};
use crate::recording::MachineMinute;
use crate::session::{OBSERVATIONS_MAX, Observation, Reader};
use crate::store::{Reading, reading_at};

type ObservationKey = ([u64; 2], [u64; 2], u64);
const MEMORY_RATES: [(&str, &str); 6] = [
    ("memory_pageins_per_second", "pageins"),
    ("memory_pageouts_per_second", "pageouts"),
    ("memory_compressions_per_second", "compressions"),
    ("memory_decompressions_per_second", "decompressions"),
    ("swap_in_bytes_per_second", "swapped_in_bytes"),
    ("swap_out_bytes_per_second", "swapped_out_bytes"),
];
const TRAFFIC_RATES: [(&str, &str); 6] = [
    ("network_rx_bps", "received_bytes"),
    ("network_tx_bps", "sent_bytes"),
    ("network_rx_packets_per_second", "received_packets"),
    ("network_tx_packets_per_second", "sent_packets"),
    ("network_rx_errors_per_second", "receive_errors"),
    ("network_tx_errors_per_second", "send_errors"),
];

/// The decoded minutes one reader holds while it makes its points, by window.
pub(crate) type Minutes = BTreeMap<usize, MachineMinute>;

/// The bytes of decoded minutes one reader may hold, which bounds a context's memory by its
/// readers. A reader holds two minutes at most.
pub(crate) const READER_MINUTES_BYTES_MAX: usize = 64 * 1024 * 1024;

/// The points of `observations`, the readings of one window. `minutes` keeps their minute and
/// those that may hold their predecessors, and drops the rest, since the first reading of the next
/// window takes its rates from the last reading of this one.
pub(crate) fn trends(
    reader: &Reader<'_>,
    observations: &[Observation],
    minutes: &mut Minutes,
) -> Result<Vec<Value>> {
    let all = trends_read(reader, observations, minutes)?;
    let mut points = Vec::with_capacity(observations.len());
    let mut segment = 0_u64;
    let mut prior: Option<&Observation> = None;
    for observation in observations {
        let previous = all.get(&key_previous(reader, observation));
        if !prior.is_some_and(|value| {
            if reader.windows[value.window].identity.run
                != reader.windows[observation.window].identity.run
            {
                return false;
            }
            compatible(value, observation)
        }) {
            segment += 1;
        }
        let current = trends_reading(minutes, observation)?;
        let before = previous
            .map(|previous| trends_reading(minutes, previous))
            .transpose()?;
        points.push(trends_point(
            reader,
            &Sources {
                current: &current,
                previous: before.as_ref(),
            },
            observation,
            previous,
            segment,
        )?);
        prior = Some(observation);
    }
    Ok(points)
}

/// The reading of `observation` in the minute `minutes` keeps of its window.
fn trends_reading<'minutes>(
    minutes: &'minutes Minutes,
    observation: &Observation,
) -> Result<Reading<'minutes>> {
    minutes
        .get(&observation.window)
        .and_then(|minute| reading_at(minute, observation.publication))
        .ok_or(Error::Bound("missing required recorded table rows"))
}

/// The point of `observation` with the identity of its run and boot.
pub(crate) fn trends_point(
    reader: &Reader<'_>,
    sources: &Sources<'_>,
    observation: &Observation,
    previous: Option<&Observation>,
    segment: u64,
) -> Result<Value> {
    persistence::work::ingestion::record_publication(
        reader.windows[observation.window].identity.run,
        observation.publication,
        persistence::work::ingestion::Kind::MachineReduction,
    );
    let mut value = point_sources(sources, observation, previous, segment)?;
    let identity = observation.json(reader.windows);
    value["run"] = identity["run"].clone();
    value["boot"] = identity["boot"].clone();

    Ok(value)
}

/// Decodes into `minutes` the minutes of `observations` and of their predecessors, after dropping
/// those of every other window, and returns the observations with their predecessors.
fn trends_read(
    reader: &Reader<'_>,
    observations: &[Observation],
    minutes: &mut Minutes,
) -> Result<BTreeMap<ObservationKey, Observation>> {
    let mut all: BTreeMap<_, _> = observations
        .iter()
        .map(|value| (key(reader, value, value.publication), value.clone()))
        .collect();
    let mut windows = BTreeSet::new();
    for observation in observations {
        assert_eq!(observation.window, observations[0].window);
        windows.insert(observation.window);
        if !all.contains_key(&key_previous(reader, observation)) {
            windows.extend(reader.predecessor_windows(observation));
        }
    }
    minutes.retain(|window, _| windows.contains(window));

    for observation in observations {
        if !all.contains_key(&key_previous(reader, observation)) {
            if let Some(previous) = trends_predecessor(reader, observation, minutes)? {
                all.insert(key(reader, &previous, previous.publication), previous);
            }
        }
    }
    if all.len() > OBSERVATIONS_MAX * 2 {
        return Err(Error::Bound("predecessor count"));
    }
    let read = all
        .values()
        .map(|value| value.window)
        .collect::<BTreeSet<_>>();
    if read.len() > 2 {
        return Err(Error::Bound("context batch windows"));
    }
    for window in read {
        trends_minute(reader, minutes, window)?;
    }

    Ok(all)
}

/// The reading before `observation` in its run, from the first window that holds it with a wall
/// clock.
fn trends_predecessor(
    reader: &Reader<'_>,
    observation: &Observation,
    minutes: &mut Minutes,
) -> Result<Option<Observation>> {
    let _purpose =
        persistence::work::ingestion::purpose(persistence::work::ingestion::Purpose::Predecessor);
    let Some(publication) = observation.publication.checked_sub(1) else {
        return Ok(None);
    };

    for window in reader.predecessor_windows(observation) {
        let minute = trends_minute(reader, minutes, window)?;
        if let Some(reading) = reading_at(minute, publication) {
            if let Some(previous) = Observation::of_tick(reading.tick, window) {
                return Ok(Some(previous));
            }
        }
    }

    Ok(None)
}

/// The minute at `window`, decoded unless `minutes` keeps it.
pub(crate) fn trends_minute<'minutes>(
    reader: &Reader<'_>,
    minutes: &'minutes mut Minutes,
    window: usize,
) -> Result<&'minutes MachineMinute> {
    let held_bytes: usize = minutes.values().map(MachineMinute::heap_bytes).sum();
    match minutes.entry(window) {
        Entry::Occupied(held) => Ok(held.into_mut()),
        Entry::Vacant(place) => {
            let minute = reader.machine_minute(window)?;
            minutes_bytes_check(held_bytes + minute.heap_bytes())?;

            Ok(place.insert(minute))
        }
    }
}

/// Refuses a reader that would hold `bytes` of decoded minutes, more than
/// `READER_MINUTES_BYTES_MAX`.
pub(crate) fn minutes_bytes_check(bytes: usize) -> Result<()> {
    if bytes > READER_MINUTES_BYTES_MAX {
        return Err(Error::Bound("context minute bytes"));
    }

    Ok(())
}

fn key(reader: &Reader<'_>, observation: &Observation, publication: u64) -> ObservationKey {
    let run = reader.windows[observation.window].identity.run;
    (run.run.0, run.boot.0, publication)
}

/// The key of the reading before `observation` in its run.
fn key_previous(reader: &Reader<'_>, observation: &Observation) -> ObservationKey {
    key(
        reader,
        observation,
        observation.publication.saturating_sub(1),
    )
}

fn get<'a>(
    tables: &Sources<'a>,
    observation: &Observation,
    name: &'static str,
) -> Result<View<'a>> {
    tables.get(observation, name)
}

/// The temperature of a die. A window that captured none holds neither column, which leaves the
/// reading unavailable the way a sensor that gave none does.
fn celsius(ticks: &View<'_>, tick: usize, name: &str) -> Result<Option<f64>> {
    if ticks.holds(name) {
        ticks.number(tick, name)
    } else {
        Ok(None)
    }
}

fn point_sources(
    tables: &Sources<'_>,
    current: &Observation,
    previous: Option<&Observation>,
    segment: u64,
) -> Result<Value> {
    Ok(capture_sources(tables, current, previous, None)?.json(current, segment))
}

fn capture_sources(
    tables: &Sources<'_>,
    current: &Observation,
    previous: Option<&Observation>,
    cpu_interval: Option<&mut Option<CapturedInterval>>,
) -> Result<point::Point> {
    let ticks = get(tables, current, "ticks")?;
    let tick = only(&ticks)?;
    let fans = tables.optional(current, "fans")?;
    let fan_rpm =
        crate::live_fan_history::capture_typed(&ticks, fans.as_ref(), current.publication)?;
    let memory = get(tables, current, "memory")?;
    let row = only(&memory)?;
    let mut value = point::Point {
        power_watts: ticks.number(tick, "power_watts")?,
        soc_celsius: ticks.number(tick, "soc_celsius")?,
        cpu_celsius: celsius(&ticks, tick, "cpu_celsius")?,
        gpu_celsius: celsius(&ticks, tick, "gpu_celsius")?,
        fan_rpm,
        battery: battery::capture(&ticks, tick, current.continuous)?,
        storage: storage::capture(tables, current, previous)?,
        system: system::capture(&system::SystemTick {
            ticks: &ticks,
            tick,
            wall_ns: current.wall,
        })?,
        logical_cpu_count: optional(tables, current, "cpus")?
            .map(|cpus| u32::try_from(cpus.len()).map_err(|_| Error::Bound("logical CPU count")))
            .transpose()?,
        ..point::Point::default()
    };
    for (index, (_name, column)) in point::MEMORY_FIELDS.iter().enumerate() {
        value.memory[index] = memory.integer(row, column)?;
    }
    if let Some(previous) = previous {
        if compatible(previous, current) {
            point_interval(tables, current, previous, &mut value, cpu_interval)?;
        }
    }
    Ok(value)
}

#[cfg(test)]
pub(crate) fn public_point(point: &Value) -> Result<Value> {
    let mut value = point.clone();
    value
        .as_object_mut()
        .ok_or(Error::Bound("point shape"))?
        .retain(|name, _value| name != "observation");
    Ok(value)
}

pub(crate) fn context_segments(points: &mut [Value]) -> Result<()> {
    assert!(points.len() <= OBSERVATIONS_MAX);
    let mut segment = 0_u64;
    for index in 0..points.len() {
        let compatible = if let Some(previous) = index.checked_sub(1) {
            if points[previous]["run"] == points[index]["run"] {
                if points[previous]["boot"] == points[index]["boot"] {
                    compatible(
                        &context_segments_observation(&points[previous])?,
                        &context_segments_observation(&points[index])?,
                    )
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };
        if !compatible {
            segment += 1;
        }
        points[index]["segment"] = json!(segment);
    }
    Ok(())
}

fn context_segments_observation(point: &Value) -> Result<Observation> {
    let required = |name| {
        point[name]
            .as_u64()
            .ok_or(Error::Bound("context observation field"))
    };
    Ok(Observation {
        window: 0,
        publication: required("publication")?,
        run_tick: required("run_tick")?,
        wall: required("wall_ns")?,
        continuous: point["continuous_ns"].as_u64(),
    })
}

fn point_interval(
    tables: &Sources<'_>,
    current: &Observation,
    previous: &Observation,
    value: &mut point::Point,
    cpu_interval: Option<&mut Option<CapturedInterval>>,
) -> Result<()> {
    let cpus = get(tables, current, "cpus")?;
    let old_cpus = get(tables, previous, "cpus")?;
    value.cpu_percent = cpu_percent(&cpus, &old_cpus)?;
    let mut current_counters = Vec::new();
    let mut previous_counters = Vec::new();
    crate::cpu_history_sample::counters_view(&cpus, current.publication, &mut current_counters)?;
    crate::cpu_history_sample::counters_view(
        &old_cpus,
        previous.publication,
        &mut previous_counters,
    )?;
    let counters = crate::cpu_history_sample::CounterInterval {
        current: &current_counters,
        previous: &previous_counters,
    };
    if let Some(output) = cpu_interval {
        assert!(output.is_none());
        *output = Some(CapturedInterval::capture(&ObservedInterval {
            current: ObservedCounters {
                observation: current,
                counters: &current_counters,
            },
            previous: Some(ObservedCounters {
                observation: previous,
                counters: &previous_counters,
            }),
        })?);
    }
    value.cpu_percent_one_core = crate::cpu_history_sample::sum_busy(counters)?;
    let ticks = get(tables, current, "ticks")?;
    if let Some(levels) = system::levels(&ticks, only(&ticks)?)? {
        value.cpu_level_percent = Some(crate::cpu_history_sample::level_busy(counters, levels)?);
        let states = optional(tables, current, core_frequency::CORE_LEVEL_STATES)?;
        let old_states = optional(tables, previous, core_frequency::CORE_LEVEL_STATES)?;
        if let (Some(states), Some(old_states)) = (states, old_states) {
            // A recording without core states reads an empty table live and none from
            // history, so the field stays null unless some level measured a clock.
            let ghz = core_frequency::level_ghz_view(&states, &old_states)?;
            if ghz.iter().any(Option::is_some) {
                value.cpu_level_ghz = Some(ghz);
            }
        }
    }
    let elapsed = current
        .continuous
        .and_then(|now| {
            previous
                .continuous
                .and_then(|before| now.checked_sub(before))
        })
        .ok_or(Error::Bound("elapsed clock"))?;
    assert!(elapsed > 0);
    let memory = get(tables, current, "memory")?;
    let old_memory = get(tables, previous, "memory")?;
    for (index, (_name, column)) in MEMORY_RATES.iter().enumerate() {
        let delta = counter_delta(&memory, &old_memory, &[], column)?;
        value.rates[index] = per_second(delta, elapsed);
    }
    point_gpu(tables, current, previous, elapsed, value)?;
    point_traffic(tables, current, previous, elapsed, value)
}

fn point_gpu(
    tables: &Sources<'_>,
    current: &Observation,
    previous: &Observation,
    elapsed: u64,
    value: &mut point::Point,
) -> Result<()> {
    let ticks = get(tables, current, "ticks")?;
    let tick = only(&ticks)?;
    let old_ticks = get(tables, previous, "ticks")?;
    let old_tick = only(&old_ticks)?;
    if ticks.field(tick, "ioreport_read")? != Some("true") {
        return Ok(());
    }

    if old_ticks.field(old_tick, "ioreport_read")? != Some("true") {
        return Ok(());
    }

    if let (Some(now), Some(before)) = (
        ticks.integer(tick, "gpu_energy_nj")?,
        old_ticks.integer(old_tick, "gpu_energy_nj")?,
    ) {
        value.gpu_watts = now
            .checked_sub(before)
            .map(|delta| number(delta) / number(elapsed));
    }

    let Some(count) = ticks.integer(tick, "gpu_state_count")? else {
        return Ok(());
    };

    if count == 0 {
        return Ok(());
    }

    if old_ticks.integer(old_tick, "gpu_state_count")? != Some(count) {
        return Ok(());
    }

    let current_states = optional(tables, current, "gpu_states")?;
    let previous_states = optional(tables, previous, "gpu_states")?;
    if let (Some(current_states), Some(previous_states)) = (current_states, previous_states) {
        if point_gpu_complete(&current_states, count)? {
            if point_gpu_complete(&previous_states, count)? {
                let delta =
                    counter_delta(&current_states, &previous_states, &["state"], "active_ns")?;
                value.gpu_active_percent =
                    delta.map(|active| number(active) * 100.0 / number(elapsed));
            }
        }
    }
    Ok(())
}

fn point_gpu_complete(table: &View<'_>, count: u64) -> Result<bool> {
    if u64::try_from(table.len()).map_err(|_| Error::Bound("GPU state row count"))? != count {
        return Ok(false);
    }

    let mut states = BTreeSet::new();
    for row in table.rows() {
        let Some(state) = table.integer(row, "state")? else {
            return Ok(false);
        };

        if state >= count {
            return Ok(false);
        }

        if !states.insert(state) {
            return Ok(false);
        }

        if table.integer(row, "active_ns")?.is_none() {
            return Ok(false);
        }
    }

    Ok(true)
}

fn optional<'a>(
    tables: &Sources<'a>,
    observation: &Observation,
    name: &'static str,
) -> Result<Option<View<'a>>> {
    tables.optional(observation, name)
}

fn point_traffic(
    tables: &Sources<'_>,
    current: &Observation,
    previous: &Observation,
    elapsed: u64,
    value: &mut point::Point,
) -> Result<()> {
    let interfaces = optional(tables, current, "interfaces")?;
    let old_interfaces = optional(tables, previous, "interfaces")?;
    if interfaces.is_none() {
        if old_interfaces.is_none() {
            for index in 6..12 {
                value.rates[index] = Some(0.0);
            }
        }
    }
    if let (Some(interfaces), Some(old_interfaces)) = (interfaces, old_interfaces) {
        for (index, (_name, column)) in TRAFFIC_RATES.iter().enumerate() {
            let delta =
                counter_delta(&interfaces, &old_interfaces, &["interface", "name"], column)?;
            value.rates[index + 6] = per_second(delta, elapsed);
        }
    }
    Ok(())
}

fn per_second(delta: Option<u64>, elapsed: u64) -> Option<f64> {
    assert!(elapsed > 0);
    delta.map(|delta| {
        reduction::per_second(&reduction::Rate {
            delta,
            elapsed_ns: elapsed,
        })
    })
}

fn only(table: &View<'_>) -> Result<usize> {
    if table.len() != 1 {
        return Err(Error::Bound("required single row"));
    }
    Ok(0)
}

pub(crate) fn compatible(previous: &Observation, current: &Observation) -> bool {
    if previous.publication.checked_add(1) != Some(current.publication) {
        return false;
    }
    if previous.run_tick.checked_add(1) != Some(current.run_tick) {
        return false;
    }
    if previous.wall >= current.wall {
        return false;
    }
    if current.wall / 1_000_000_000 > previous.wall / 1_000_000_000 + 1 {
        return false;
    }
    match (previous.continuous, current.continuous) {
        (Some(before), Some(now)) => now > before,
        _ => false,
    }
}

fn cpu_percent(current: &View<'_>, previous: &View<'_>) -> Result<Option<f64>> {
    let busy = components::sum(&components::Interval {
        current,
        previous,
        keys: &["cpu"],
        columns: &["user_ticks", "system_ticks", "nice_ticks"],
    })?;
    let idle = components::sum(&components::Interval {
        current,
        previous,
        keys: &["cpu"],
        columns: &["idle_ticks"],
    })?;
    match (busy, idle) {
        (Some(busy), Some(idle)) => {
            let total = busy.checked_add(idle).ok_or(Error::Bound("CPU sum"))?;
            Ok(reduction::machine_percent(&reduction::Fraction {
                numerator: busy,
                denominator: total,
            }))
        }
        _ => Ok(None),
    }
}

fn counter_delta(
    current: &View<'_>,
    previous: &View<'_>,
    keys: &[&str],
    column: &str,
) -> Result<Option<u64>> {
    components::sum(&components::Interval {
        current,
        previous,
        keys,
        columns: &[column],
    })
}

pub(crate) fn number(value: u64) -> f64 {
    let bytes = value.to_le_bytes();
    let low = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let high = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod encoding_tests;

pub(crate) mod battery;
pub(crate) mod core_frequency;
pub(crate) mod storage;
mod system;

mod components;
pub(crate) mod reduction;

pub(crate) mod point;
