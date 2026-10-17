use std::collections::BTreeMap;

use persistence::{Error, Result};
use serde_json::{Value, json};

use crate::data::Table;
use crate::session::{OBSERVATIONS_MAX, Observation, Session, partition};

type ObservationKey = ([u64; 2], [u64; 2], u64);
type Tables = BTreeMap<(usize, &'static str), BTreeMap<u64, Table>>;

pub(crate) fn trends(session: &Session, observations: &[Observation]) -> Result<Vec<Value>> {
    let mut all: BTreeMap<_, _> = observations
        .iter()
        .map(|value| (key(session, value, value.publication), value.clone()))
        .collect();
    for observation in observations {
        if !all.contains_key(&key(
            session,
            observation,
            observation.publication.saturating_sub(1),
        )) {
            if let Some(previous) = session.predecessor(observation)? {
                all.insert(key(session, &previous, previous.publication), previous);
            }
        }
    }
    if all.len() > OBSERVATIONS_MAX * 2 {
        return Err(Error::Bound("predecessor count"));
    }
    let mut tables = Tables::new();
    let mut cells = 0;
    for observation in all.values() {
        for name in ["ticks", "memory", "cpus", "interfaces"] {
            if let std::collections::btree_map::Entry::Vacant(entry) =
                tables.entry((observation.window, name))
            {
                let table = session.table(observation.window, name)?;
                cells += table.rows.len() * table.columns.len();
                if cells > 2_000_000 {
                    return Err(Error::Bound("context cells"));
                }
                entry.insert(partition(&table)?);
            }
        }
    }
    let mut points = Vec::with_capacity(observations.len());
    let mut segment = 0_u64;
    let mut prior: Option<&Observation> = None;
    for observation in observations {
        let previous = all.get(&key(
            session,
            observation,
            observation.publication.saturating_sub(1),
        ));
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
        let mut value = point(&tables, observation, previous, segment)?;
        let identity = observation.json(session);
        value["run"] = identity["run"].clone();
        value["boot"] = identity["boot"].clone();
        points.push(value);
        prior = Some(observation);
    }
    Ok(points)
}

fn key(session: &Session, observation: &Observation, publication: u64) -> ObservationKey {
    let run = session.windows[observation.window].identity.run;
    (run.run.0, run.boot.0, publication)
}

fn get<'a>(tables: &'a Tables, observation: &Observation, name: &'static str) -> Result<&'a Table> {
    tables
        .get(&(observation.window, name))
        .and_then(|table| table.get(&observation.publication))
        .ok_or(Error::Bound("missing required recorded table rows"))
}

fn point(
    tables: &Tables,
    current: &Observation,
    previous: Option<&Observation>,
    segment: u64,
) -> Result<Value> {
    let ticks = get(tables, current, "ticks")?;
    let tick = only(ticks)?;
    let memory = get(tables, current, "memory")?;
    let row = only(memory)?;
    let mut value = json!({"wall_ns": current.wall, "publication": current.publication,
        "segment": segment, "cpu_percent": null, "network_rx_bps": null,
        "network_tx_bps": null, "gpu_watts": null,
        "power_watts": ticks.number(tick, "power_watts")?,
        "soc_celsius": ticks.number(tick, "soc_celsius")?,
        "memory_total_bytes": memory.integer(row, "total_bytes")?,
        "memory_active_bytes": memory.integer(row, "active_bytes")?,
        "memory_wired_bytes": memory.integer(row, "wired_bytes")?,
        "memory_compressed_bytes": memory.integer(row, "compressed_bytes")?,
        "swap_used_bytes": memory.integer(row, "swap_used_bytes")?,
        "pressure": memory.integer(row, "pressure")?,
        "interval_unavailable": "Requires consecutive compatible counter samples"});
    if let Some(previous) = previous {
        if compatible(previous, current) {
            let cpus = get(tables, current, "cpus")?;
            let old_cpus = get(tables, previous, "cpus")?;
            value["cpu_percent"] = json!(cpu_percent(cpus, old_cpus)?);
            let elapsed = current
                .continuous
                .and_then(|now| {
                    previous
                        .continuous
                        .and_then(|before| now.checked_sub(before))
                })
                .ok_or(Error::Bound("elapsed clock"))?;
            let previous_ticks = get(tables, previous, "ticks")?;
            if let (Some(now), Some(before)) = (
                ticks.integer(tick, "gpu_energy_nj")?,
                previous_ticks.integer(only(previous_ticks)?, "gpu_energy_nj")?,
            ) {
                value["gpu_watts"] = json!(
                    now.checked_sub(before)
                        .map(|delta| number(delta) / number(elapsed))
                );
            }
            traffic(tables, current, previous, elapsed, &mut value)?;
        }
    }
    Ok(value)
}

fn traffic(
    tables: &Tables,
    current: &Observation,
    previous: &Observation,
    elapsed: u64,
    value: &mut Value,
) -> Result<()> {
    let interfaces = tables
        .get(&(current.window, "interfaces"))
        .and_then(|rows| rows.get(&current.publication));
    let old_interfaces = tables
        .get(&(previous.window, "interfaces"))
        .and_then(|rows| rows.get(&previous.publication));
    if let (Some(interfaces), Some(old_interfaces)) = (interfaces, old_interfaces) {
        let rx = counter_delta(
            interfaces,
            old_interfaces,
            &["interface", "name"],
            &["received_bytes"],
        )?;
        let tx = counter_delta(
            interfaces,
            old_interfaces,
            &["interface", "name"],
            &["sent_bytes"],
        )?;
        value["network_rx_bps"] = json!(rx.map(|v| number(v) * 1e9 / number(elapsed)));
        value["network_tx_bps"] = json!(tx.map(|v| number(v) * 1e9 / number(elapsed)));
    }
    Ok(())
}

fn only(table: &Table) -> Result<&[Option<String>]> {
    if table.rows.len() != 1 {
        return Err(Error::Bound("required single row"));
    }
    Ok(&table.rows[0])
}

fn compatible(previous: &Observation, current: &Observation) -> bool {
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

fn number(value: u64) -> f64 {
    let bytes = value.to_le_bytes();
    let low = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let high = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}
