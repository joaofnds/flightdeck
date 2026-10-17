use std::collections::BTreeMap;

use persistence::{Error, Result};
use serde_json::{Value, json};

use crate::data::Table;
use crate::session::Observation;

pub(crate) const CPUS_MAX: usize = 1024;
const RESPONSE_BYTES_MAX: usize = 128 * 1024;
const _: () = {
    assert!(CPUS_MAX > 0);
    assert!(RESPONSE_BYTES_MAX < 8 * 1024 * 1024);
};

pub(crate) struct Sample {
    pub identity: Value,
    pub observation: Observation,
    pub cpus: Table,
}

type Counters = BTreeMap<u32, [Option<u64>; 4]>;

#[derive(Clone, Copy)]
struct Interval<'a> {
    current: &'a [Option<u64>; 4],
    previous: &'a [Option<u64>; 4],
}

const _: () = assert!(size_of::<Interval<'static>>() <= 16);

pub(crate) fn project(current: &Sample, previous: Option<&Sample>) -> Result<Value> {
    let after = project_counters(current)?;
    let before = previous.map(project_counters).transpose()?;
    let unavailable = project_unavailable(current, previous, &after, before.as_ref());
    let mut cpus = Vec::with_capacity(after.len());
    for (cpu, counters) in &after {
        let busy = if unavailable.is_none() {
            before
                .as_ref()
                .and_then(|values| values.get(cpu))
                .map(|old| {
                    project_busy(Interval {
                        current: counters,
                        previous: old,
                    })
                })
                .transpose()?
                .flatten()
        } else {
            None
        };
        cpus.push(json!({"cpu":cpu,"busy_percent":busy}));
    }

    let value = json!({"observation":current.identity,
        "previous_observation":previous.map(|sample| &sample.identity),
        "interval_available":unavailable.is_none(),"unavailable_reason":unavailable,
        "cpus":cpus});
    if serde_json::to_vec(&value)
        .map_err(std::io::Error::other)?
        .len()
        > RESPONSE_BYTES_MAX
    {
        return Err(Error::Bound("CPU detail bytes"));
    }

    Ok(value)
}

fn project_counters(sample: &Sample) -> Result<Counters> {
    if sample.cpus.rows.len() > CPUS_MAX {
        return Err(Error::Bound("logical CPU count"));
    }

    let mut counters = Counters::new();
    for row in &sample.cpus.rows {
        if sample.cpus.integer(row, "publication")? != Some(sample.observation.publication) {
            return Err(Error::Bound("CPU detail publication"));
        }

        let cpu = sample
            .cpus
            .integer(row, "cpu")?
            .ok_or(Error::Bound("missing logical CPU index"))?;
        let cpu = u32::try_from(cpu).map_err(|_| Error::Bound("logical CPU index"))?;
        let values = ["user_ticks", "system_ticks", "nice_ticks", "idle_ticks"]
            .map(|name| sample.cpus.integer(row, name));
        let mut parsed = [None; 4];
        for (index, value) in values.into_iter().enumerate() {
            parsed[index] = value?;
        }
        if counters.insert(cpu, parsed).is_some() {
            return Err(Error::Bound("duplicate logical CPU index"));
        }
    }

    for (expected, cpu) in counters.keys().enumerate() {
        if usize::try_from(*cpu).map_err(|_| Error::Bound("logical CPU index"))? != expected {
            return Err(Error::Bound("incomplete logical CPU domain"));
        }
    }

    Ok(counters)
}

fn project_unavailable(
    current: &Sample,
    previous: Option<&Sample>,
    after: &Counters,
    before: Option<&Counters>,
) -> Option<&'static str> {
    let Some(previous) = previous else {
        return Some("Previous CPU reading unavailable");
    };

    if current.identity["run"] != previous.identity["run"] {
        return Some("CPU readings belong to different runs");
    }

    if current.identity["boot"] != previous.identity["boot"] {
        return Some("CPU readings belong to different boots");
    }

    if !crate::metrics::compatible(&previous.observation, &current.observation) {
        return Some("Requires consecutive compatible CPU readings");
    }

    if after.is_empty() {
        return Some("CPU readings unavailable");
    }

    if before.is_none_or(|before| !after.keys().eq(before.keys())) {
        return Some("Logical CPU domain changed");
    }

    None
}

fn project_busy(interval: Interval<'_>) -> Result<Option<f64>> {
    let mut total = 0_u64;
    let mut busy = 0_u64;
    for index in 0..4 {
        let (Some(now), Some(old)) = (interval.current[index], interval.previous[index]) else {
            return Ok(None);
        };

        let Some(delta) = now.checked_sub(old) else {
            return Ok(None);
        };

        total = total
            .checked_add(delta)
            .ok_or(Error::Bound("CPU counter total"))?;
        if index < 3 {
            busy = busy
                .checked_add(delta)
                .ok_or(Error::Bound("CPU busy total"))?;
        }
    }

    if total == 0 {
        return Ok(None);
    }

    let percent = number(busy) / number(total) * 100.0;
    assert!(percent.is_finite());
    assert!((0.0..=100.0).contains(&percent));

    Ok(Some(percent))
}

fn number(value: u64) -> f64 {
    let bytes = value.to_le_bytes();
    let low = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    let high = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    f64::from(high) * 4_294_967_296.0 + f64::from(low)
}

#[cfg(test)]
mod tests;
