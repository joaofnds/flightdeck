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

pub(crate) struct Presentation<'a> {
    pub identity: &'a Value,
    pub previous: Option<&'a Value>,
    pub unavailable: Option<&'static str>,
    pub values: &'a [Option<f64>],
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
    project_values(&Presentation {
        identity: &current.identity,
        previous: previous.map(|sample| &sample.identity),
        unavailable,
        values: &values,
    })
}

pub(crate) fn project_values(presentation: &Presentation<'_>) -> Result<Value> {
    let cpus = presentation
        .values
        .iter()
        .enumerate()
        .map(|(cpu, busy)| json!({"cpu":cpu,"busy_percent":busy}))
        .collect::<Vec<_>>();
    let value = json!({"observation":presentation.identity,
        "previous_observation":presentation.previous,
        "interval_available":presentation.unavailable.is_none(),
        "unavailable_reason":presentation.unavailable,
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
