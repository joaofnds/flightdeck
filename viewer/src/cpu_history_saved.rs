use super::{ContextRange, NS, OBSERVATIONS_MAX, Observation, Session, partition};
use crate::cpu_history::{self, Cache, Entry};
use crate::cpu_history_sample::{self, CounterSample, Identity, Interval};
use crate::data::{self, Table};
use persistence::{Error, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const BATCH_OBSERVATIONS_MAX: usize = 64;
const BATCH_PUBLICATIONS_MAX: usize = BATCH_OBSERVATIONS_MAX * 2;

struct CpuQuery<'a> {
    window: usize,
    table: &'a str,
    predicate: &'a str,
    rows_max: usize,
}

impl Session {
    pub fn cpu_history(&mut self, offset: u32) -> Result<Value> {
        let operation = self.operation.begin(self.operation.epoch())?;
        operation.run(|| {
            let selected = self
                .selected
                .as_ref()
                .ok_or(Error::Bound("no selected observation"))?
                .json(self);
            self.cpu_history_restore(cpu_history::range(&selected)?)?;
            cpu_history::project(&cpu_history::Request {
                selected: &selected,
                context: &self.selected_context,
                live: None,
                saved: self.cpu_history_cache.as_ref(),
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

        self.operation.attach(&self.connection)?;
        let operation = self.operation.begin(self.operation.epoch())?;
        operation.run(|| {
            let observations = self.observations(range.first_ns, range.end_ns)?;
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

        let predicate = publications
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let mut ticks = None;
        self.cpu_history_query(
            &CpuQuery {
                window,
                table: "ticks",
                predicate: &predicate,
                rows_max: publications.len(),
            },
            &mut ticks,
        )?;
        let rows_max = publications.len() * cpu_history_sample::CPUS_MAX;
        let mut cpus = None;
        self.cpu_history_query(
            &CpuQuery {
                window,
                table: "cpus",
                predicate: &predicate,
                rows_max,
            },
            &mut cpus,
        )?;
        let ticks = ticks
            .as_ref()
            .ok_or(Error::Bound("CPU history tick output"))?;
        let mut cpus = partition(
            cpus.as_ref()
                .ok_or(Error::Bound("CPU history table output"))?,
        )?;
        for row in &ticks.rows {
            self.operation.check()?;
            if ticks.integer(row, "wall_ns")?.is_none() {
                continue;
            }

            let observation = Observation::from_row(ticks, row, window)?;
            self.cpu_history_sample(&observation, &mut cpus, output)?;
        }

        Ok(())
    }

    fn cpu_history_sample(
        &self,
        observation: &Observation,
        cpus: &mut BTreeMap<u64, Table>,
        output: &mut BTreeMap<(usize, u64), CounterSample>,
    ) -> Result<()> {
        let mut identity = None;
        Identity::read(&observation.json(self), observation, &mut identity)?;
        let mut counters = Vec::new();
        if let Some(table) = cpus.remove(&observation.publication) {
            cpu_history_sample::counters(&table, observation.publication, &mut counters)?;
        }

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

    fn cpu_history_query(&self, query: &CpuQuery<'_>, output: &mut Option<Table>) -> Result<()> {
        assert!(output.is_none());
        assert!(query.rows_max <= BATCH_PUBLICATIONS_MAX * cpu_history_sample::CPUS_MAX);

        let source = self.source(query.window, query.table)?;
        let limit = query.rows_max + 1;
        let predicate = query.predicate;
        *output = Some(data::query(
            &self.connection,
            &format!("SELECT * FROM {source} WHERE publication IN ({predicate}) LIMIT {limit}"),
            query.rows_max,
            &self.operation,
        )?);

        Ok(())
    }
}
