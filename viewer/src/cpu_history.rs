use std::collections::{BTreeMap, VecDeque};

use persistence::{Error, Result};
use serde_json::{Value, json};

use crate::cpu_history_sample::{self, CounterSample, Identity, Interval};
use crate::data::Table;
use crate::session::{ContextRange, OBSERVATIONS_MAX, Observation};

pub(crate) const PAGE_CPUS: usize = 32;
pub(crate) const RESPONSE_BYTES_MAX: usize = 7 * 1024 * 1024;
pub(crate) const CACHE_BYTES_MAX: usize = 80 * 1024 * 1024;
const LIVE_BYTES_MAX: usize = 64 * 1024 * 1024;
const SOURCES_MAX: usize = 2;
const NS: u64 = 1_000_000_000;

pub(crate) struct Cache {
    pub range: ContextRange,
    pub entries: Vec<Entry>,
}

#[derive(Clone)]
pub(crate) struct Entry {
    pub identity: Identity,
    pub previous: Option<Identity>,
    pub interval_available: bool,
    pub values: Vec<Option<f64>>,
}

pub(crate) struct History {
    entries: VecDeque<Entry>,
    previous: Vec<CounterSample>,
    heap_bytes: usize,
}

pub(crate) struct Request<'a> {
    pub selected: &'a Value,
    pub context_last_second: u64,
    pub context: &'a [Identity],
    pub live: Option<&'a History>,
    pub saved: Option<&'a Cache>,
    pub offset: u32,
}

#[derive(Clone, Copy, Default)]
struct Merged<'a> {
    live: Option<&'a Entry>,
    saved: Option<&'a Entry>,
}

type Key<'a> = (&'a str, &'a str, u64);
type Index<'a> = BTreeMap<Key<'a>, Merged<'a>>;

const _: () = {
    assert!(size_of::<Merged<'static>>() <= 16);
    assert!(PAGE_CPUS * OBSERVATIONS_MAX <= 131_072);
    assert!(RESPONSE_BYTES_MAX < 8 * 1024 * 1024);
    assert!(CACHE_BYTES_MAX > OBSERVATIONS_MAX * 1024 * size_of::<Option<f64>>());
    assert!(
        size_of::<Cache>()
            + OBSERVATIONS_MAX * (size_of::<Entry>() + 256 + 1024 * size_of::<Option<f64>>())
            < CACHE_BYTES_MAX
    );
};

impl Entry {
    pub fn derive(interval: &Interval<'_>, output: &mut Option<Self>) -> Result<()> {
        assert!(output.is_none());

        let mut values = Vec::new();
        let unavailable = cpu_history_sample::derive(interval, &mut values)?;
        *output = Some(Self {
            identity: interval.current.identity.clone(),
            previous: interval.previous.map(|previous| previous.identity.clone()),
            interval_available: unavailable.is_none(),
            values,
        });

        Ok(())
    }

    fn heap_bytes(&self) -> usize {
        self.identity.run.capacity()
            + self.identity.boot.capacity()
            + self.previous.as_ref().map_or(0, |previous| {
                previous.run.capacity() + previous.boot.capacity()
            })
            + self.values.capacity() * size_of::<Option<f64>>()
    }
}

impl History {
    pub fn init(output: &mut Option<Self>) {
        assert!(output.is_none());

        *output = Some(Self {
            entries: VecDeque::with_capacity(OBSERVATIONS_MAX),
            previous: Vec::with_capacity(SOURCES_MAX),
            heap_bytes: 0,
        });
    }

    pub fn capture(&mut self, identity: &Identity, table: &Table) -> Result<()> {
        let mut counters = Vec::new();
        cpu_history_sample::counters(table, identity.observation.publication, &mut counters)?;
        let current = CounterSample {
            identity: identity.clone(),
            counters,
        };
        let previous = self.previous.iter().position(|sample| {
            sample.identity.run == identity.run && sample.identity.boot == identity.boot
        });
        if let Some(index) = previous {
            if self.previous[index].identity.observation.publication
                == identity.observation.publication
            {
                if !self.previous[index].identity.same(identity) {
                    return Err(Error::Bound("changed retained CPU identity"));
                }

                if self.previous[index].counters != current.counters {
                    return Err(Error::Bound("changed retained CPU counters"));
                }

                return Ok(());
            }
        }

        let mut entry = None;
        Entry::derive(
            &Interval {
                current: &current,
                previous: previous.map(|index| &self.previous[index]),
            },
            &mut entry,
        )?;
        let entry = entry.ok_or(Error::Bound("CPU interval output"))?;
        if self.entries.len() == OBSERVATIONS_MAX {
            self.capture_remove()?;
        }

        self.heap_bytes += entry.heap_bytes();
        self.entries.push_back(entry);
        if let Some(index) = previous {
            self.previous[index] = current;
        } else {
            if self.previous.len() == SOURCES_MAX {
                self.previous.remove(0);
            }
            self.previous.push(current);
        }
        let first_ns = (identity.observation.wall / NS).saturating_sub(899) * NS;

        self.capture_evict(first_ns)
    }

    fn capture_evict(&mut self, first_ns: u64) -> Result<()> {
        for _attempt in 0..=OBSERVATIONS_MAX {
            let expired = self
                .entries
                .front()
                .is_some_and(|entry| entry.identity.observation.wall < first_ns);
            if !expired {
                if self.bytes() <= LIVE_BYTES_MAX {
                    return Ok(());
                }
            }

            self.capture_remove()?;
        }

        Err(Error::Bound("CPU retention eviction"))
    }

    fn capture_remove(&mut self) -> Result<()> {
        let removed = self
            .entries
            .pop_front()
            .ok_or(Error::Bound("CPU retention budget"))?;

        self.heap_bytes -= removed.heap_bytes();

        Ok(())
    }

    pub fn bytes(&self) -> usize {
        size_of::<Self>()
            + self.entries.capacity() * size_of::<Entry>()
            + self.heap_bytes
            + self.previous.capacity() * size_of::<CounterSample>()
            + self
                .previous
                .iter()
                .map(|sample| {
                    sample.identity.run.capacity()
                        + sample.identity.boot.capacity()
                        + sample.counters.capacity() * size_of::<[Option<u64>; 4]>()
                })
                .sum::<usize>()
    }
}

impl Cache {
    pub fn covers(&self, range: ContextRange, end_known: Option<u64>) -> bool {
        if range.first_ns < self.range.first_ns {
            return false;
        }

        end_known.is_none_or(|end| range.end_ns.min(end) <= self.range.end_ns)
    }

    pub fn bytes(&self) -> usize {
        size_of::<Self>()
            + self.entries.capacity() * size_of::<Entry>()
            + self.entries.iter().map(Entry::heap_bytes).sum::<usize>()
    }
}

/// The identities of the charted `points` that lie in `range`, which leaves out the lead-in the
/// charts draw before it.
pub(crate) fn context(
    points: &[Value],
    range: ContextRange,
    output: &mut Vec<Identity>,
) -> Result<()> {
    assert!(output.is_empty());
    let held = range.held_points(points);
    if held.len() > OBSERVATIONS_MAX {
        return Err(Error::Bound("CPU context observations"));
    }

    output.reserve_exact(held.len());
    for point in held {
        let required = |name| {
            point[name]
                .as_u64()
                .ok_or(Error::Bound("CPU context identity"))
        };
        let observation = Observation {
            window: 0,
            publication: required("publication")?,
            run_tick: required("run_tick")?,
            wall: required("wall_ns")?,
            continuous: point["continuous_ns"].as_u64(),
        };
        let mut identity = None;
        Identity::read(point, &observation, &mut identity)?;
        output.push(identity.ok_or(Error::Bound("CPU context identity output"))?);
    }

    Ok(())
}

pub(crate) fn project(request: &Request<'_>) -> Result<Value> {
    let range = ContextRange::ending_at(request.context_last_second)?;
    let mut index = Index::new();
    project_index(request, &mut index)?;
    let mut merged = Vec::with_capacity(request.context.len());
    project_context(request, &index, range, &mut merged)?;
    let selected = project_selected(request)?;
    let total = merged[selected]
        .primary()
        .map_or(0, |entry| entry.values.len());
    let offset = usize::try_from(request.offset).map_err(|_| Error::Bound("CPU history offset"))?;
    project_offset(offset, total)?;

    let observations = request
        .context
        .iter()
        .zip(&merged)
        .map(|(identity, entry)| {
            let primary = entry.primary();
            json!({"observation":identity.json(),
            "previous_observation":primary.and_then(|entry| {
                entry.previous.as_ref().map(Identity::json)
            }),
            "interval_available":primary.is_some_and(|entry| entry.interval_available),
            "cpu_count":primary.map_or(0, |entry| entry.values.len())})
        })
        .collect::<Vec<_>>();
    let mut cpus = Vec::with_capacity(PAGE_CPUS.min(total));
    for cpu in offset..(offset + PAGE_CPUS).min(total) {
        let values = merged
            .iter()
            .map(|entry| entry.value(cpu))
            .collect::<Vec<_>>();
        cpus.push(json!({"cpu":cpu,"busy_percent":values}));
    }
    let value = json!({"observation":request.selected,"context_seconds":900,
        "first_ns":range.first_ns,"end_ns":range.end_ns,"offset":request.offset,"total":total,
        "retention_limited":merged.iter().any(|entry| entry.primary().is_none()),
        "observations":observations,"cpus":cpus});
    project_bytes(&value)?;

    Ok(value)
}

pub(crate) fn project_bytes(value: &Value) -> Result<()> {
    if serde_json::to_vec(value)
        .map_err(std::io::Error::other)?
        .len()
        > RESPONSE_BYTES_MAX
    {
        return Err(Error::Bound("CPU history response bytes"));
    }

    Ok(())
}

fn project_offset(offset: usize, total: usize) -> Result<()> {
    if !offset.is_multiple_of(PAGE_CPUS) {
        return Err(Error::Bound("CPU history offset"));
    }

    if total == 0 {
        if offset == 0 {
            return Ok(());
        }
    } else {
        if offset < total {
            return Ok(());
        }
    }

    Err(Error::Bound("CPU history page past end"))
}

fn project_selected(request: &Request<'_>) -> Result<usize> {
    let run = request.selected["run"]
        .as_str()
        .ok_or(Error::Bound("CPU selected run"))?;
    let boot = request.selected["boot"]
        .as_str()
        .ok_or(Error::Bound("CPU selected boot"))?;
    let publication = request.selected["publication"]
        .as_u64()
        .ok_or(Error::Bound("CPU selected publication"))?;
    let selected = request
        .context
        .iter()
        .position(|identity| {
            identity.run == run
                && identity.boot == boot
                && identity.observation.publication == publication
        })
        .ok_or(Error::Bound("CPU history selected context"))?;
    let identity = &request.context[selected];
    if Some(identity.observation.run_tick) != request.selected["run_tick"].as_u64() {
        return Err(Error::Bound("CPU selected run tick"));
    }

    if Some(identity.observation.wall) != request.selected["wall_ns"].as_u64() {
        return Err(Error::Bound("CPU selected clock"));
    }

    Ok(selected)
}

fn project_index<'a>(request: &'a Request<'a>, output: &mut Index<'a>) -> Result<()> {
    if let Some(saved) = request.saved {
        if saved.entries.len() > OBSERVATIONS_MAX {
            return Err(Error::Bound("CPU history saved count"));
        }

        for entry in &saved.entries {
            let slot = output.entry(key(&entry.identity)).or_default();
            if slot.saved.replace(entry).is_some() {
                return Err(Error::Bound("duplicate saved CPU observation"));
            }
        }
    }
    if let Some(live) = request.live {
        for entry in &live.entries {
            let slot = output.entry(key(&entry.identity)).or_default();
            if slot.live.replace(entry).is_some() {
                return Err(Error::Bound("duplicate live CPU observation"));
            }
        }
    }

    Ok(())
}

fn project_context<'a>(
    request: &Request<'a>,
    index: &Index<'a>,
    range: ContextRange,
    output: &mut Vec<Merged<'a>>,
) -> Result<()> {
    assert!(output.is_empty());
    if request.context.len() > OBSERVATIONS_MAX {
        return Err(Error::Bound("CPU history observations"));
    }

    let mut previous = None;
    for identity in request.context {
        if !(range.first_ns..range.end_ns).contains(&identity.observation.wall) {
            return Err(Error::Bound("CPU history observation domain"));
        }

        let order = (identity.observation.wall, key(identity));
        if previous.is_some_and(|previous| previous >= order) {
            return Err(Error::Bound("CPU history observation order"));
        }

        previous = Some(order);
        let merged = index.get(&key(identity)).copied().unwrap_or_default();
        merged.validate(identity)?;
        output.push(merged);
    }

    Ok(())
}

fn key(identity: &Identity) -> Key<'_> {
    (
        &identity.run,
        &identity.boot,
        identity.observation.publication,
    )
}

impl<'a> Merged<'a> {
    fn primary(self) -> Option<&'a Entry> {
        if self.live.is_some_and(|entry| entry.interval_available) {
            return self.live;
        }

        if self.saved.is_some_and(|entry| entry.interval_available) {
            return self.saved;
        }

        self.live.or(self.saved)
    }

    fn validate(self, identity: &Identity) -> Result<()> {
        for entry in [self.live, self.saved].into_iter().flatten() {
            if !entry.identity.same(identity) {
                return Err(Error::Bound("changed CPU history observation"));
            }

            if entry.values.len() > cpu_history_sample::CPUS_MAX {
                return Err(Error::Bound("logical CPU count"));
            }
        }
        if let (Some(live), Some(saved)) = (self.live, self.saved) {
            if live.values.len() != saved.values.len() {
                return Err(Error::Bound("changed CPU history domain"));
            }

            if live.interval_available {
                if saved.interval_available {
                    let same = live
                        .previous
                        .as_ref()
                        .zip(saved.previous.as_ref())
                        .is_some_and(|(left, right)| left.same(right));
                    if !same {
                        return Err(Error::Bound("changed CPU history predecessor"));
                    }
                }
            }
        }

        Ok(())
    }

    fn value(self, cpu: usize) -> Option<f64> {
        let primary = self.primary()?;
        if !primary.interval_available {
            return None;
        }

        let value = primary.values.get(cpu).copied().flatten();
        if value.is_some() {
            return value;
        }

        self.saved
            .filter(|entry| entry.interval_available)
            .and_then(|entry| entry.values.get(cpu))
            .copied()
            .flatten()
    }
}

#[cfg(test)]
#[path = "cpu_history_tests.rs"]
mod tests;
