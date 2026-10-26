use persistence::{Error, Result};
use serde_json::{Value, json};

use crate::cpu_history_sample::{self, CounterSample, Identity, Interval};
use crate::session::Observation;

pub(crate) use crate::cpu_history_sample::CPUS_MAX;
const RESPONSE_BYTES_MAX: usize = 128 * 1024;
const _: () = {
    assert!(CPUS_MAX > 0);
    assert!(RESPONSE_BYTES_MAX < 8 * 1024 * 1024);
};

pub(crate) struct Sample {
    pub identity: Value,
    pub observation: Observation,
    pub cpus: Source,
}

pub(crate) enum Source {
    Saved(crate::data::Table),
    Native(cpu_history_sample::Counters),
}

pub(crate) fn project(current: &Sample, previous: Option<&Sample>) -> Result<Value> {
    let mut after = None;
    let mut before = None;
    project_sample(current, &mut after)?;
    if let Some(previous) = previous {
        project_sample(previous, &mut before)?;
    }

    let mut values = Vec::new();
    let unavailable = cpu_history_sample::derive(
        &Interval {
            current: after.as_ref().ok_or(Error::Bound("CPU sample"))?,
            previous: before.as_ref(),
        },
        &mut values,
    )?;
    let cpus = values
        .iter()
        .enumerate()
        .map(|(cpu, busy)| json!({"cpu":cpu,"busy_percent":busy}))
        .collect::<Vec<_>>();
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

fn project_sample(sample: &Sample, output: &mut Option<CounterSample>) -> Result<()> {
    assert!(output.is_none());

    let mut identity = None;
    Identity::read(&sample.identity, &sample.observation, &mut identity)?;
    let counters = match &sample.cpus {
        Source::Native(counters) => counters.clone(),
        Source::Saved(table) => {
            let mut counters = Vec::new();
            cpu_history_sample::counters(table, sample.observation.publication, &mut counters)?;
            counters
        }
    };
    *output = Some(CounterSample {
        identity: identity.ok_or(Error::Bound("CPU identity"))?,
        counters,
    });

    Ok(())
}

#[cfg(test)]
mod tests;
