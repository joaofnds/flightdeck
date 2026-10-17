use persistence::{Error, Result};
use serde_json::{Value, json};

use crate::data::Table;
use crate::session::Observation;

pub(crate) const CPUS_MAX: usize = 1024;
pub(crate) type Counters = Vec<[Option<u64>; 4]>;

#[derive(Clone)]
pub(crate) struct Identity {
    pub run: String,
    pub boot: String,
    pub observation: Observation,
}

pub(crate) struct CounterSample {
    pub identity: Identity,
    pub counters: Counters,
}

pub(crate) struct Interval<'a> {
    pub current: &'a CounterSample,
    pub previous: Option<&'a CounterSample>,
}

#[derive(Clone, Copy)]
struct BusyInterval<'a> {
    current: &'a [Option<u64>; 4],
    previous: &'a [Option<u64>; 4],
}

const _: () = {
    assert!(size_of::<BusyInterval<'static>>() <= 16);
    assert!(size_of::<Option<f64>>() == 16);
    assert!(size_of::<Option<u64>>() == 16);
};

impl Identity {
    pub fn read(value: &Value, observation: &Observation, output: &mut Option<Self>) -> Result<()> {
        assert!(output.is_none());

        let run = value["run"]
            .as_str()
            .ok_or(Error::Bound("CPU run identity"))?;
        let boot = value["boot"]
            .as_str()
            .ok_or(Error::Bound("CPU boot identity"))?;
        if run.len() > 64 {
            return Err(Error::Bound("CPU run identity bytes"));
        }

        if boot.len() > 64 {
            return Err(Error::Bound("CPU boot identity bytes"));
        }

        *output = Some(Self {
            run: run.to_owned(),
            boot: boot.to_owned(),
            observation: observation.clone(),
        });

        Ok(())
    }

    pub fn json(&self) -> Value {
        json!({"run":self.run,"boot":self.boot,
            "publication":self.observation.publication,"run_tick":self.observation.run_tick,
            "wall_ns":self.observation.wall})
    }

    pub fn same(&self, other: &Self) -> bool {
        self.run == other.run
            && self.boot == other.boot
            && self.observation.publication == other.observation.publication
            && self.observation.run_tick == other.observation.run_tick
            && self.observation.wall == other.observation.wall
    }
}

pub(crate) fn counters(table: &Table, publication: u64, output: &mut Counters) -> Result<()> {
    assert!(output.is_empty());
    if table.rows.len() > CPUS_MAX {
        return Err(Error::Bound("logical CPU count"));
    }

    let mut ordered = vec![None; table.rows.len()];
    for row in &table.rows {
        if table.integer(row, "publication")? != Some(publication) {
            return Err(Error::Bound("CPU detail publication"));
        }

        let cpu = table
            .integer(row, "cpu")?
            .ok_or(Error::Bound("missing logical CPU index"))?;
        let cpu = u32::try_from(cpu).map_err(|_| Error::Bound("logical CPU index"))?;
        let cpu = usize::try_from(cpu).map_err(|_| Error::Bound("logical CPU index"))?;
        let slot = ordered
            .get_mut(cpu)
            .ok_or(Error::Bound("incomplete logical CPU domain"))?;
        if slot.is_some() {
            return Err(Error::Bound("duplicate logical CPU index"));
        }

        let mut values = [None; 4];
        for (index, name) in ["user_ticks", "system_ticks", "nice_ticks", "idle_ticks"]
            .into_iter()
            .enumerate()
        {
            values[index] = table.integer(row, name)?;
        }
        *slot = Some(values);
    }

    output.reserve_exact(ordered.len());
    for slot in ordered {
        output.push(slot.ok_or(Error::Bound("incomplete logical CPU domain"))?);
    }

    Ok(())
}

pub(crate) fn derive(
    interval: &Interval<'_>,
    output: &mut Vec<Option<f64>>,
) -> Result<Option<&'static str>> {
    assert!(output.is_empty());
    assert!(interval.current.counters.len() <= CPUS_MAX);

    let unavailable = derive_unavailable(interval);
    output.reserve_exact(interval.current.counters.len());
    for (cpu, current) in interval.current.counters.iter().enumerate() {
        let busy = if unavailable.is_none() {
            let previous = interval
                .previous
                .ok_or(Error::Bound("CPU interval predecessor"))?;
            derive_busy(BusyInterval {
                current,
                previous: &previous.counters[cpu],
            })?
        } else {
            None
        };
        output.push(busy);
    }

    Ok(unavailable)
}

fn derive_unavailable(interval: &Interval<'_>) -> Option<&'static str> {
    let Some(previous) = interval.previous else {
        return Some("Previous CPU reading unavailable");
    };

    if interval.current.identity.run != previous.identity.run {
        return Some("CPU readings belong to different runs");
    }

    if interval.current.identity.boot != previous.identity.boot {
        return Some("CPU readings belong to different boots");
    }

    if !crate::metrics::compatible(
        &previous.identity.observation,
        &interval.current.identity.observation,
    ) {
        return Some("Requires consecutive compatible CPU readings");
    }

    if interval.current.counters.is_empty() {
        return Some("CPU readings unavailable");
    }

    if interval.current.counters.len() != previous.counters.len() {
        return Some("Logical CPU domain changed");
    }

    None
}

fn derive_busy(interval: BusyInterval<'_>) -> Result<Option<f64>> {
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

    let percent = crate::metrics::number(busy) / crate::metrics::number(total) * 100.0;
    assert!(percent.is_finite());
    assert!((0.0..=100.0).contains(&percent));

    Ok(Some(percent))
}
