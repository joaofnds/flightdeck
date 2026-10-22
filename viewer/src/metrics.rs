use std::collections::{BTreeMap, BTreeSet};

use persistence::{Error, Result};
use serde_json::{Value, json};

use crate::data::Table;
use crate::session::{OBSERVATIONS_MAX, Observation, Session, partition};

type ObservationKey = ([u64; 2], [u64; 2], u64);
pub(crate) const TABLE_NAMES: [&str; 7] = [
    "ticks",
    "memory",
    "cpus",
    "interfaces",
    "gpu_states",
    "fans",
    core_frequency::CORE_LEVEL_STATES,
];
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

pub(crate) type Tables = BTreeMap<(usize, &'static str), BTreeMap<u64, Table>>;

/// The cells of the tables one context may hold while it makes its points.
pub(crate) const CONTEXT_CELLS_MAX: usize = 2_000_000;

/// The points of `observations`, the readings of one window. `tables` keeps the tables of their
/// window and of the windows that may hold their predecessors, and drops the rest, since the
/// first reading of the next window takes its rates from the last reading of this one.
pub(crate) fn trends(
    session: &Session,
    observations: &[Observation],
    tables: &mut Tables,
) -> Result<Vec<Value>> {
    let all = trends_read(session, observations, tables)?;
    let mut points = Vec::with_capacity(observations.len());
    let mut segment = 0_u64;
    let mut prior: Option<&Observation> = None;
    for observation in observations {
        let previous = all.get(&key_previous(session, observation));
        if !prior.is_some_and(|value| {
            if session.windows[value.window].identity.run
                != session.windows[observation.window].identity.run
            {
                return false;
            }
            compatible(value, observation)
        }) {
            segment += 1;
        }
        points.push(trends_point(
            session,
            tables,
            observation,
            previous,
            segment,
        )?);
        prior = Some(observation);
    }
    Ok(points)
}

/// The point of `observation` with the identity of its run and boot.
pub(crate) fn trends_point(
    session: &Session,
    tables: &Tables,
    observation: &Observation,
    previous: Option<&Observation>,
    segment: u64,
) -> Result<Value> {
    let mut value = point(tables, observation, previous, segment)?;
    let identity = observation.json(session);
    value["run"] = identity["run"].clone();
    value["boot"] = identity["boot"].clone();

    Ok(value)
}

/// Reads into `tables` the tables of `observations` and of their predecessors, after dropping
/// those of every other window, and returns the observations with their predecessors.
fn trends_read(
    session: &Session,
    observations: &[Observation],
    tables: &mut Tables,
) -> Result<BTreeMap<ObservationKey, Observation>> {
    let mut all: BTreeMap<_, _> = observations
        .iter()
        .map(|value| (key(session, value, value.publication), value.clone()))
        .collect();
    let mut windows = BTreeSet::new();
    for observation in observations {
        assert_eq!(observation.window, observations[0].window);
        windows.insert(observation.window);
        if !all.contains_key(&key_previous(session, observation)) {
            windows.extend(session.predecessor_windows(observation));
        }
    }
    tables.retain(|(window, _), _| windows.contains(window));

    let mut cell_count = trends_cell_count(tables);
    for observation in observations {
        if !all.contains_key(&key_previous(session, observation)) {
            if let Some(previous) =
                trends_predecessor(session, observation, tables, &mut cell_count)?
            {
                all.insert(key(session, &previous, previous.publication), previous);
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
        for name in TABLE_NAMES {
            trends_table(session, tables, window, name, &mut cell_count)?;
        }
    }
    assert_eq!(cell_count, trends_cell_count(tables));

    Ok(all)
}

/// The reading before `observation` in its run, from the ticks of the first window that holds
/// it with a wall clock.
fn trends_predecessor(
    session: &Session,
    observation: &Observation,
    tables: &mut Tables,
    cell_count: &mut usize,
) -> Result<Option<Observation>> {
    let Some(publication) = observation.publication.checked_sub(1) else {
        return Ok(None);
    };

    for window in session.predecessor_windows(observation) {
        let ticks = trends_table(session, tables, window, "ticks", cell_count)?;
        if let Some(table) = ticks.get(&publication) {
            for row in &table.rows {
                if table.integer(row, "wall_ns")?.is_some() {
                    return Ok(Some(Observation::from_row(table, row, window)?));
                }
            }
        }
    }

    Ok(None)
}

/// The rows of table `name` of `window` by publication, read from the window unless `tables`
/// keeps them. `cell_count` counts the cells `tables` keeps, which stay under a bound.
fn trends_table<'tables>(
    session: &Session,
    tables: &'tables mut Tables,
    window: usize,
    name: &'static str,
    cell_count: &mut usize,
) -> Result<&'tables BTreeMap<u64, Table>> {
    match tables.entry((window, name)) {
        std::collections::btree_map::Entry::Occupied(entry) => Ok(entry.into_mut()),
        std::collections::btree_map::Entry::Vacant(entry) => {
            Ok(entry.insert(table_read(session, window, name, cell_count)?))
        }
    }
}

/// The rows of table `name` of `window` by publication, read from the window. `cell_count`
/// counts the cells a context holds, which stay under `CONTEXT_CELLS_MAX`.
pub(crate) fn table_read(
    session: &Session,
    window: usize,
    name: &'static str,
    cell_count: &mut usize,
) -> Result<BTreeMap<u64, Table>> {
    let table = session.table(window, name)?;
    if name == "fans" {
        crate::live_fan_history::source(&table)?;
    }
    *cell_count += table.rows.len() * table.columns.len();
    if *cell_count > CONTEXT_CELLS_MAX {
        return Err(Error::Bound("context cells"));
    }

    partition(&table)
}

/// The count of the cells of the tables `tables` keeps.
pub(crate) fn trends_cell_count(tables: &Tables) -> usize {
    let mut cell_count = 0;
    for publications in tables.values() {
        for table in publications.values() {
            cell_count += table.rows.len() * table.columns.len();
        }
    }

    cell_count
}

fn key(session: &Session, observation: &Observation, publication: u64) -> ObservationKey {
    let run = session.windows[observation.window].identity.run;
    (run.run.0, run.boot.0, publication)
}

/// The key of the reading before `observation` in its run.
fn key_previous(session: &Session, observation: &Observation) -> ObservationKey {
    key(
        session,
        observation,
        observation.publication.saturating_sub(1),
    )
}

fn get<'a>(tables: &'a Tables, observation: &Observation, name: &'static str) -> Result<&'a Table> {
    tables
        .get(&(observation.window, name))
        .and_then(|table| table.get(&observation.publication))
        .ok_or(Error::Bound("missing required recorded table rows"))
}

/// The temperature of a die. A window that captured none holds neither column, which leaves the
/// reading unavailable the way a sensor that gave none does.
fn celsius(ticks: &Table, tick: &[Option<String>], name: &str) -> Result<Option<f64>> {
    if ticks.columns.iter().any(|column| column == name) {
        ticks.number(tick, name)
    } else {
        Ok(None)
    }
}

pub(crate) fn point(
    tables: &Tables,
    current: &Observation,
    previous: Option<&Observation>,
    segment: u64,
) -> Result<Value> {
    let ticks = get(tables, current, "ticks")?;
    let tick = only(ticks)?;
    let fans = tables
        .get(&(current.window, "fans"))
        .ok_or(Error::Bound("missing required recorded fan source"))?
        .get(&current.publication);
    let fan_rpm = crate::live_fan_history::capture(ticks, fans, current.publication)?;
    let memory = get(tables, current, "memory")?;
    let row = only(memory)?;
    let mut value = json!({"wall_ns": current.wall, "publication": current.publication,
        "run_tick": current.run_tick, "continuous_ns": current.continuous,
        "segment": segment, "cpu_percent": null, "cpu_percent_one_core": null,
        "cpu_level_percent": null, "cpu_level_ghz": null,
        "gpu_active_percent": null, "gpu_watts": null,
        "power_watts": ticks.number(tick, "power_watts")?,
        "soc_celsius": ticks.number(tick, "soc_celsius")?,
        "cpu_celsius": celsius(ticks, tick, "cpu_celsius")?,
        "gpu_celsius": celsius(ticks, tick, "gpu_celsius")?, "fan_rpm": fan_rpm,
        "interval_unavailable": "Requires consecutive compatible counter samples"});
    value["battery"] = battery::point(ticks, tick, current.continuous)?;
    value["storage"] = storage::point(tables, current, previous)?;
    let system = system::SystemTick {
        ticks,
        tick,
        wall_ns: current.wall,
    };
    system::point(&system, &mut value)?;
    value["logical_cpu_count"] =
        json!(optional(tables, current, "cpus").map(|cpus| cpus.rows.len()));
    for (name, column) in [
        ("memory_total_bytes", "total_bytes"),
        ("memory_free_bytes", "free_bytes"),
        ("memory_active_bytes", "active_bytes"),
        ("memory_inactive_bytes", "inactive_bytes"),
        ("memory_speculative_bytes", "speculative_bytes"),
        ("memory_wired_bytes", "wired_bytes"),
        ("memory_compressed_bytes", "compressed_bytes"),
        ("memory_anonymous_bytes", "anonymous_bytes"),
        ("memory_file_backed_bytes", "file_backed_bytes"),
        ("memory_purgeable_bytes", "purgeable_bytes"),
        ("swap_total_bytes", "swap_total_bytes"),
        ("swap_used_bytes", "swap_used_bytes"),
        ("pressure", "pressure"),
    ] {
        value[name] = json!(memory.integer(row, column)?);
    }
    for (name, _column) in MEMORY_RATES.into_iter().chain(TRAFFIC_RATES) {
        value[name] = Value::Null;
    }
    if let Some(previous) = previous {
        if compatible(previous, current) {
            point_interval(tables, current, previous, &mut value)?;
        }
    }
    Ok(value)
}

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
    tables: &Tables,
    current: &Observation,
    previous: &Observation,
    value: &mut Value,
) -> Result<()> {
    let cpus = get(tables, current, "cpus")?;
    let old_cpus = get(tables, previous, "cpus")?;
    value["cpu_percent"] = json!(cpu_percent(cpus, old_cpus)?);
    let mut current_counters = Vec::new();
    let mut previous_counters = Vec::new();
    crate::cpu_history_sample::counters(cpus, current.publication, &mut current_counters)?;
    crate::cpu_history_sample::counters(old_cpus, previous.publication, &mut previous_counters)?;
    let counters = crate::cpu_history_sample::CounterInterval {
        current: &current_counters,
        previous: &previous_counters,
    };
    value["cpu_percent_one_core"] = json!(crate::cpu_history_sample::sum_busy(counters)?);
    let ticks = get(tables, current, "ticks")?;
    if let Some(levels) = system::levels(ticks, only(ticks)?)? {
        value["cpu_level_percent"] =
            json!(crate::cpu_history_sample::level_busy(counters, levels)?);
        let states = optional(tables, current, core_frequency::CORE_LEVEL_STATES);
        let old_states = optional(tables, previous, core_frequency::CORE_LEVEL_STATES);
        if let (Some(states), Some(old_states)) = (states, old_states) {
            // A recording without core states reads an empty table live and none from
            // history, so the field stays null unless some level measured a clock.
            let ghz = core_frequency::level_ghz(states, old_states)?;
            if ghz.iter().any(Option::is_some) {
                value["cpu_level_ghz"] = json!(ghz);
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
    for (name, column) in MEMORY_RATES {
        let delta = counter_delta(memory, old_memory, &[], &[column])?;
        value[name] = json!(per_second(delta, elapsed));
    }
    point_gpu(tables, current, previous, elapsed, value)?;
    point_traffic(tables, current, previous, elapsed, value)
}

fn point_gpu(
    tables: &Tables,
    current: &Observation,
    previous: &Observation,
    elapsed: u64,
    value: &mut Value,
) -> Result<()> {
    let ticks = get(tables, current, "ticks")?;
    let tick = only(ticks)?;
    let old_ticks = get(tables, previous, "ticks")?;
    let old_tick = only(old_ticks)?;
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
        value["gpu_watts"] = json!(
            now.checked_sub(before)
                .map(|delta| number(delta) / number(elapsed))
        );
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

    let current_states = optional(tables, current, "gpu_states");
    let previous_states = optional(tables, previous, "gpu_states");
    if let (Some(current_states), Some(previous_states)) = (current_states, previous_states) {
        if point_gpu_complete(current_states, count)? {
            if point_gpu_complete(previous_states, count)? {
                let delta =
                    counter_delta(current_states, previous_states, &["state"], &["active_ns"])?;
                value["gpu_active_percent"] =
                    json!(delta.map(|active| number(active) * 100.0 / number(elapsed)));
            }
        }
    }
    Ok(())
}

fn point_gpu_complete(table: &Table, count: u64) -> Result<bool> {
    if u64::try_from(table.rows.len()).map_err(|_| Error::Bound("GPU state row count"))? != count {
        return Ok(false);
    }

    let mut states = BTreeSet::new();
    for row in &table.rows {
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
    tables: &'a Tables,
    observation: &Observation,
    name: &'static str,
) -> Option<&'a Table> {
    tables
        .get(&(observation.window, name))
        .and_then(|rows| rows.get(&observation.publication))
}

fn point_traffic(
    tables: &Tables,
    current: &Observation,
    previous: &Observation,
    elapsed: u64,
    value: &mut Value,
) -> Result<()> {
    let interfaces = optional(tables, current, "interfaces");
    let old_interfaces = optional(tables, previous, "interfaces");
    if interfaces.is_none() {
        if old_interfaces.is_none() {
            if tables.contains_key(&(current.window, "interfaces")) {
                if tables.contains_key(&(previous.window, "interfaces")) {
                    for (name, _column) in TRAFFIC_RATES {
                        value[name] = json!(0.0);
                    }
                }
            }
        }
    }
    if let (Some(interfaces), Some(old_interfaces)) = (interfaces, old_interfaces) {
        for (name, column) in TRAFFIC_RATES {
            let delta = counter_delta(
                interfaces,
                old_interfaces,
                &["interface", "name"],
                &[column],
            )?;
            value[name] = json!(per_second(delta, elapsed));
        }
    }
    Ok(())
}

fn per_second(delta: Option<u64>, elapsed: u64) -> Option<f64> {
    assert!(elapsed > 0);
    delta.map(|delta| number(delta) * 1e9 / number(elapsed))
}

fn only(table: &Table) -> Result<&[Option<String>]> {
    if table.rows.len() != 1 {
        return Err(Error::Bound("required single row"));
    }
    Ok(&table.rows[0])
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

fn cpu_percent(current: &Table, previous: &Table) -> Result<Option<f64>> {
    let busy = counter_delta(
        current,
        previous,
        &["cpu"],
        &["user_ticks", "system_ticks", "nice_ticks"],
    )?;
    let idle = counter_delta(current, previous, &["cpu"], &["idle_ticks"])?;
    match (busy, idle) {
        (Some(busy), Some(idle)) => {
            let total = busy.checked_add(idle).ok_or(Error::Bound("CPU sum"))?;
            if total == 0 {
                return Ok(None);
            }
            Ok(Some(number(busy) * 100.0 / number(total)))
        }
        _ => Ok(None),
    }
}

fn counter_delta(
    current: &Table,
    previous: &Table,
    keys: &[&str],
    columns: &[&str],
) -> Result<Option<u64>> {
    if current.rows.len() != previous.rows.len() {
        return Ok(None);
    }
    let keyed = |table: &Table| -> Result<BTreeMap<Vec<Option<String>>, usize>> {
        let mut keyed = BTreeMap::new();
        for (index, row) in table.rows.iter().enumerate() {
            let key = keys
                .iter()
                .map(|name| table.field(row, name).map(|v| v.map(str::to_owned)))
                .collect::<Result<Vec<_>>>()?;
            if key.iter().any(Option::is_none) {
                return Err(Error::Bound("missing component key"));
            }
            if keyed.insert(key, index).is_some() {
                return Err(Error::Bound("duplicate component"));
            }
        }
        Ok(keyed)
    };
    let before = keyed(previous)?;
    let after = keyed(current)?;
    let mut sum = 0_u64;
    for (key, index) in after {
        let Some(before_index) = before.get(&key) else {
            return Ok(None);
        };
        for column in columns {
            let Some(now) = current.integer(&current.rows[index], column)? else {
                return Ok(None);
            };
            let Some(old) = previous.integer(&previous.rows[*before_index], column)? else {
                return Ok(None);
            };
            let Some(delta) = now.checked_sub(old) else {
                return Ok(None);
            };
            sum = sum.checked_add(delta).ok_or(Error::Bound("counter sum"))?;
        }
    }
    Ok(Some(sum))
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

mod battery;
pub(crate) mod core_frequency;
mod storage;
mod system;
