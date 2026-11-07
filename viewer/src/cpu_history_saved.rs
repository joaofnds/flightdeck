use super::{ContextRange, NS, OBSERVATIONS_MAX, Observation, Session};
use crate::cpu_history::{self, Cache, Entry};
use crate::cpu_history_sample::{self, CounterSample, Identity, Interval};
use crate::recording::CpuRow;
use persistence::{Error, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const BATCH_OBSERVATIONS_MAX: usize = 64;
const BATCH_PUBLICATIONS_MAX: usize = BATCH_OBSERVATIONS_MAX * 2;

impl Session {
    pub fn cpu_history(&mut self, offset: u32) -> Result<Value> {
        let operation = self.operation.begin(self.operation.epoch())?;
        operation.run(|| {
            let selected = self
                .selected
                .as_ref()
                .ok_or(Error::Bound("no selected observation"))?
                .json(&self.windows);
            let context_last_second = self
                .context_last_second
                .ok_or(Error::Bound("no selected context"))?;
            self.cpu_history_restore(ContextRange::ending_at(context_last_second)?)?;
            cpu_history::project(&cpu_history::Request {
                selected: &selected,
                context_last_second,
                context: &self.selected_context,
                saved: self.cpu_history_cache.as_ref(),
                retained: None,
                offset,
            })
        })
    }

    pub(crate) fn cpu_history_restore(&mut self, range: ContextRange) -> Result<()> {
        let end_known = self
            .last
            .map(|second| {
                second
                    .checked_add(1)
                    .and_then(|end| end.checked_mul(NS))
                    .ok_or(Error::Bound("CPU history end"))
            })
            .transpose()?;
        if self
            .cpu_history_cache
            .as_ref()
            .is_some_and(|cache| cache.covers(range, end_known))
        {
            return Ok(());
        }

        let operation = self.operation.begin(self.operation.epoch())?;
        operation.run(|| {
            let observations = self.observations(range)?;
            let mut cache = Cache {
                range,
                entries: Vec::with_capacity(observations.len()),
            };
            self.cpu_history_entries(&observations, &mut cache.entries)?;
            if cache.bytes() > cpu_history::CACHE_BYTES_MAX {
                return Err(Error::Bound("CPU history cache bytes"));
            }

            #[cfg(test)]
            if let Some(barrier) = self.before_cpu_history_install.take() {
                barrier();
            }

            self.operation.check()?;
            self.cpu_history_cache = Some(cache);

            Ok(())
        })
    }

    fn cpu_history_entries(
        &self,
        observations: &[Observation],
        output: &mut Vec<Entry>,
    ) -> Result<()> {
        assert!(observations.len() <= OBSERVATIONS_MAX);
        assert!(output.is_empty());

        let mut windows = BTreeMap::<usize, Vec<&Observation>>::new();
        for observation in observations {
            windows
                .entry(observation.window)
                .or_default()
                .push(observation);
        }

        for observations in windows.values() {
            for batch in observations.chunks(BATCH_OBSERVATIONS_MAX) {
                self.operation.check()?;
                self.cpu_history_batch(batch, output)?;
            }
        }

        Ok(())
    }

    fn cpu_history_batch(
        &self,
        observations: &[&Observation],
        output: &mut Vec<Entry>,
    ) -> Result<()> {
        let mut wanted = BTreeMap::<usize, BTreeSet<u64>>::new();
        for observation in observations {
            wanted
                .entry(observation.window)
                .or_default()
                .insert(observation.publication);
            if let Some(publication) = observation.publication.checked_sub(1) {
                if let Some(window) = self.cpu_history_predecessor_window(observation)? {
                    wanted.entry(window).or_default().insert(publication);
                }
            }
        }
        if wanted.len() > 2 {
            return Err(Error::Bound("CPU history batch windows"));
        }

        let mut samples = BTreeMap::new();
        for (window, publications) in &wanted {
            self.cpu_history_samples(*window, publications, &mut samples)?;
        }

        for observation in observations {
            let current = samples
                .get(&(observation.window, observation.publication))
                .ok_or(Error::Bound("CPU history current reading"))?;
            let previous = observation
                .publication
                .checked_sub(1)
                .and_then(|publication| {
                    samples
                        .values()
                        .find(|sample| sample.identity.observation.publication == publication)
                });
            let mut entry = None;
            Entry::derive(&Interval { current, previous }, &mut entry)?;
            output.push(entry.ok_or(Error::Bound("CPU history interval output"))?);
        }

        Ok(())
    }

    fn cpu_history_predecessor_window(&self, current: &Observation) -> Result<Option<usize>> {
        let Some(publication) = current.publication.checked_sub(1) else {
            return Ok(None);
        };

        let identity = self.windows[current.window].identity.run;
        let mut found = None;
        for (window, manifest) in self.windows.iter().enumerate() {
            if manifest.identity.run == identity {
                if (manifest.publication_first..manifest.publication_end).contains(&publication) {
                    if found.replace(window).is_some() {
                        return Err(Error::Bound("duplicate CPU predecessor commitment"));
                    }
                }
            }
        }

        Ok(found)
    }

    fn cpu_history_samples(
        &self,
        window: usize,
        publications: &BTreeSet<u64>,
        output: &mut BTreeMap<(usize, u64), CounterSample>,
    ) -> Result<()> {
        assert!(publications.len() <= BATCH_PUBLICATIONS_MAX);

        let minute = self.reader().minute_cpus(window)?;
        for reading in crate::store::readings(&minute) {
            self.operation.check()?;
            if publications.contains(&reading.tick.publication) {
                if let Some(observation) = Observation::of_tick(reading.tick, window) {
                    self.cpu_history_sample(&observation, reading.cpus, output)?;
                }
            }
        }

        Ok(())
    }

    fn cpu_history_sample(
        &self,
        observation: &Observation,
        cpus: &[CpuRow],
        output: &mut BTreeMap<(usize, u64), CounterSample>,
    ) -> Result<()> {
        let mut identity = None;
        Identity::read(&observation.json(&self.windows), observation, &mut identity)?;
        let mut counters = Vec::new();
        cpu_history_sample::counters_rows(cpus, &mut counters)?;

        let sample = CounterSample {
            identity: identity.ok_or(Error::Bound("CPU history sample identity"))?,
            counters,
        };
        if output
            .insert((observation.window, observation.publication), sample)
            .is_some()
        {
            return Err(Error::Bound("duplicate CPU history tick"));
        }

        Ok(())
    }
}
