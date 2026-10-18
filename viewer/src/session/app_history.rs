use std::collections::{BTreeMap, BTreeSet};

use super::{ContextRange, NS, OBSERVATIONS_MAX, Session, WINDOWS_MAX};
use crate::live_app_history::{Target, TracePoint};
use crate::live_app_response::{KeyKind, Observation};
use crate::live_apps::{Frame, committed::Endpoint};
use collector::ring::{Identity, PROGRAM_PATH_BYTES_MAX, RunIdentity};
use persistence::{Error, Result};
use serde_json::Value;

pub(super) struct Cache {
    recording: [u64; 2],
    kind: KeyKind,
    key: Vec<u8>,
    range: ContextRange,
    points: Vec<TracePoint>,
    pins: Vec<usize>,
}

struct Restore {
    readings: BTreeMap<Key, Reading>,
    sources: BTreeMap<(usize, Source), String>,
    source_bytes: usize,
    plotted: usize,
}

struct Projection<'target> {
    target: &'target Target<'target>,
    points: &'target mut Vec<TracePoint>,
    previous: Option<Box<Frame>>,
    reading: Option<Reading>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    recording: [u64; 2],
    boot: [u64; 2],
    run: [u64; 2],
    publication: u64,
}

#[derive(Clone, Copy)]
struct Reading {
    window: usize,
    observation: Observation,
    refused: u32,
    gone: u32,
    events_first: u64,
    events_end: u64,
    plotted: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Source {
    Ticks,
    Apps,
}

const TICKS_PER_WINDOW_MAX: usize = crate::live_apps::committed::ENDPOINTS_MAX;
const READINGS_MAX: usize = OBSERVATIONS_MAX * 2;
const SOURCES_MAX: usize = READINGS_MAX * 2;
const SOURCE_BYTES_MAX: usize = 16 * 1024 * 1024;
const CACHE_BYTES_MAX: usize = size_of::<Cache>()
    + PROGRAM_PATH_BYTES_MAX
    + OBSERVATIONS_MAX * size_of::<TracePoint>()
    + READINGS_MAX * size_of::<usize>();

const _: () = {
    assert!(CACHE_BYTES_MAX < 2 * 1024 * 1024);
    assert!(READINGS_MAX <= WINDOWS_MAX);
    assert!(TICKS_PER_WINDOW_MAX <= OBSERVATIONS_MAX);
};

impl Session {
    pub(crate) fn with_app_history(
        &self,
        selected: &Observation,
        target: &Target<'_>,
        consume: impl FnOnce(&[TracePoint]) -> Result<Value>,
    ) -> Result<Value> {
        let result = self.operation.attach(&self.connection).and_then(|()| {
            self.operation.run(|| {
                let range = self.app_history_range(selected)?;
                if target.bytes.len() > PROGRAM_PATH_BYTES_MAX {
                    return Err(Error::Bound("committed App history key"));
                }

                if collector::recording::read_identity(&self.root)?.0 != selected.recording {
                    return Err(Error::Bound("committed App history recording"));
                }

                let mut cache = self.app_history_cache.borrow_mut();
                let hit = cache
                    .as_ref()
                    .is_some_and(|cache| cache.covers(&range, selected, target));
                if !hit {
                    *cache = None;
                    let restored = self.app_history_restore(&range, selected, target)?;
                    #[cfg(test)]
                    if let Some(checkpoint) = self.before_app_history_install.borrow_mut().take() {
                        checkpoint();
                    }
                    self.app_history_validate(&restored, selected)?;
                    self.operation.check()?;
                    *cache = Some(restored);
                }

                let cache = cache
                    .as_ref()
                    .ok_or(Error::Bound("committed App history cache"))?;
                self.app_history_validate(cache, selected)?;
                let first = cache.points.partition_point(|point| {
                    point
                        .observation
                        .wall_ns
                        .is_some_and(|wall| wall < range.first_ns)
                });
                let end = cache.points.partition_point(|point| {
                    point
                        .observation
                        .wall_ns
                        .is_some_and(|wall| wall < range.end_ns)
                });
                consume(&cache.points[first..end])
            })
        });
        if result.is_err() {
            *self.app_history_cache.borrow_mut() = None;
        }

        result
    }

    fn app_history_range(&self, selected: &Observation) -> Result<ContextRange> {
        let second = selected
            .wall_ns
            .ok_or(Error::Bound("committed App history clock"))?
            / NS;
        let first_ns = second.saturating_sub(899) * NS;
        let end_ns = second
            .checked_add(1)
            .and_then(|value| value.checked_mul(NS))
            .ok_or(Error::Bound("committed App history end"))?;
        let end_known = self
            .last
            .map(|second| {
                second
                    .checked_add(1)
                    .and_then(|value| value.checked_mul(NS))
                    .ok_or(Error::Bound("committed App history extent"))
            })
            .transpose()?;

        Ok(ContextRange {
            first_ns,
            end_ns: end_known
                .map_or(end_ns, |known| known.min(end_ns))
                .max(first_ns),
        })
    }

    fn app_history_restore(
        &self,
        range: &ContextRange,
        selected: &Observation,
        target: &Target<'_>,
    ) -> Result<Cache> {
        let mut restore = Restore {
            readings: BTreeMap::new(),
            sources: BTreeMap::new(),
            source_bytes: 0,
            plotted: 0,
        };
        self.app_history_gather(range, &mut restore)?;
        self.app_history_predecessors(&mut restore)?;
        let mut points = Vec::with_capacity(restore.plotted);
        self.app_history_points(&mut restore, target, &mut points)?;
        points.sort_by_key(|point| (point.observation.wall_ns, Key::new(&point.observation)));
        let pins: BTreeSet<_> = restore
            .sources
            .keys()
            .map(|(window, _source)| *window)
            .collect();
        if pins.len() > READINGS_MAX {
            return Err(Error::Bound("committed App history pins"));
        }

        let cache = Cache {
            recording: selected.recording,
            kind: target.kind,
            key: target.bytes.to_vec(),
            range: *range,
            points,
            pins: pins.into_iter().collect(),
        };
        if cache.bytes() > CACHE_BYTES_MAX {
            return Err(Error::Bound("committed App history cache bytes"));
        }

        assert!(cache.points.len() <= OBSERVATIONS_MAX);
        assert!(cache.pins.len() <= READINGS_MAX);

        Ok(cache)
    }

    fn app_history_gather(&self, range: &ContextRange, restore: &mut Restore) -> Result<()> {
        assert!(self.windows.len() <= WINDOWS_MAX);
        let predicate = format!("wall_ns>={} AND wall_ns<{}", range.first_ns, range.end_ns);
        for (window, manifest) in self.windows.iter().enumerate() {
            self.operation.check()?;
            if let Some(bounds) = manifest.wall_ns {
                if bounds.max < range.first_ns {
                    continue;
                }
                if bounds.min >= range.end_ns {
                    continue;
                }
            }
            let mut readings = Vec::with_capacity(TICKS_PER_WINDOW_MAX);
            self.app_history_ticks(
                restore,
                window,
                &predicate,
                TICKS_PER_WINDOW_MAX,
                &mut readings,
            )?;
            for mut reading in readings {
                if self.app_history_window(&reading.observation)? != Some(window) {
                    return Err(Error::Bound("committed App history window association"));
                }

                if restore.plotted == OBSERVATIONS_MAX {
                    return Err(Error::Bound("committed App history observations"));
                }

                reading.plotted = true;
                restore.plotted += 1;
                app_history_insert(restore, &reading)?;
            }
        }

        Ok(())
    }

    fn app_history_predecessors(&self, restore: &mut Restore) -> Result<()> {
        let selected: Vec<_> = restore.readings.values().copied().collect();
        assert!(selected.len() <= OBSERVATIONS_MAX);
        for reading in &selected {
            self.operation.check()?;
            let Some(publication) = reading.observation.stamp.publication.checked_sub(1) else {
                continue;
            };
            let mut previous = reading.observation;
            previous.stamp.publication = publication;
            if restore.readings.contains_key(&Key::new(&previous)) {
                continue;
            }
            let Some(window) = self.app_history_window(&previous)? else {
                continue;
            };
            let mut readings = Vec::with_capacity(1);
            self.app_history_ticks(
                restore,
                window,
                &format!("publication={publication}"),
                1,
                &mut readings,
            )?;
            if let Some(reading) = readings.first() {
                app_history_insert(restore, reading)?;
            }
        }

        Ok(())
    }

    fn app_history_window(&self, observation: &Observation) -> Result<Option<usize>> {
        let mut found = None;
        for (window, manifest) in self.windows.iter().enumerate() {
            self.operation.check()?;
            if manifest.identity.run.recording.0 != observation.recording {
                continue;
            }
            if manifest.identity.run.boot.0 != observation.stamp.boot {
                continue;
            }
            if manifest.identity.run.run.0 != observation.stamp.run {
                continue;
            }
            if (manifest.publication_first..manifest.publication_end)
                .contains(&observation.stamp.publication)
            {
                if found.replace(window).is_some() {
                    return Err(Error::Bound("duplicate committed App history commitment"));
                }
            }
        }

        Ok(found)
    }

    fn app_history_ticks(
        &self,
        restore: &mut Restore,
        window: usize,
        predicate: &str,
        maximum: usize,
        output: &mut Vec<Reading>,
    ) -> Result<()> {
        assert!(maximum <= TICKS_PER_WINDOW_MAX);
        assert!(output.is_empty());
        let source = self.app_history_source(restore, window, Source::Ticks)?;
        let limit = maximum + 1;
        let mut statement = self.connection.prepare(&format!(
            "SELECT publication, run_tick, wall_ns, continuous_ns, awake_ns, processes_refused, \
            processes_gone, events_first, events_end FROM {source} \
            WHERE {predicate} ORDER BY publication LIMIT {limit}"
        ))?;
        let mut rows = statement.query([])?;
        for index in 0..=maximum {
            self.operation.check()?;
            let Some(row) = rows.next()? else {
                return Ok(());
            };

            if index == maximum {
                return Err(Error::Bound("committed App history tick rows"));
            }

            output.push(app_history_reading(
                row,
                window,
                &self.windows[window].identity.run,
            )?);
        }

        Err(Error::Bound("committed App history tick rows"))
    }

    fn app_history_source<'restore>(
        &self,
        restore: &'restore mut Restore,
        window: usize,
        kind: Source,
    ) -> Result<&'restore str> {
        let key = (window, kind);
        if !restore.sources.contains_key(&key) {
            if restore.sources.len() == SOURCES_MAX {
                return Err(Error::Bound("committed App history sources"));
            }

            let source = match kind {
                Source::Ticks => self.source(window, "ticks")?,
                Source::Apps => {
                    let processes = self.source(window, "processes")?;
                    let events = self.source(window, "events")?;
                    crate::live_apps::committed::Sources {
                        processes: &processes,
                        events: &events,
                    }
                    .query()
                }
            };
            restore.source_bytes = restore
                .source_bytes
                .checked_add(source.capacity())
                .ok_or(Error::Bound("committed App history source bytes"))?;
            if restore.source_bytes > SOURCE_BYTES_MAX {
                return Err(Error::Bound("committed App history source bytes"));
            }

            restore.sources.insert(key, source);
        }

        restore
            .sources
            .get(&key)
            .map(String::as_str)
            .ok_or(Error::Bound("committed App history source output"))
    }

    fn app_history_points(
        &self,
        restore: &mut Restore,
        target: &Target<'_>,
        output: &mut Vec<TracePoint>,
    ) -> Result<()> {
        let readings: Vec<_> = restore.readings.values().copied().collect();
        assert!(readings.len() <= READINGS_MAX);
        let mut projection = Projection {
            target,
            points: output,
            previous: None,
            reading: None,
        };
        let mut index = 0;
        for _batch in 0..READINGS_MAX {
            self.operation.check()?;
            if index == readings.len() {
                return Ok(());
            }

            let count = readings[index..]
                .iter()
                .take_while(|reading| reading.window == readings[index].window)
                .count();
            let end = index + count;
            assert!(index < end);
            self.app_history_batch(restore, &readings[index..end], &mut projection)?;
            index = end;
        }
        assert_eq!(index, readings.len());

        Ok(())
    }

    fn app_history_batch(
        &self,
        restore: &mut Restore,
        readings: &[Reading],
        projection: &mut Projection<'_>,
    ) -> Result<()> {
        if readings.len() > TICKS_PER_WINDOW_MAX {
            return Err(Error::Bound("committed App history window endpoints"));
        }

        let first = readings
            .first()
            .ok_or(Error::Bound("committed App history empty batch"))?;
        let endpoints: Vec<_> = readings.iter().map(Reading::endpoint).collect();
        let source = self.app_history_source(restore, first.window, Source::Apps)?;
        let mut index = 0;
        Frame::committed_many(
            &endpoints,
            &self.connection,
            source,
            &self.operation,
            |frame| {
                let reading = readings
                    .get(index)
                    .ok_or(Error::Bound("committed App history batch association"))?;
                app_history_capture(projection, reading, frame)?;
                index += 1;
                Ok(())
            },
        )?;
        assert_eq!(index, readings.len());

        Ok(())
    }

    fn app_history_validate(&self, cache: &Cache, selected: &Observation) -> Result<()> {
        self.operation.check()?;
        for window in &cache.pins {
            self.validate_commitment(*window)?;
        }
        if self.app_history_window(selected)?.is_some() {
            let point = cache
                .points
                .iter()
                .find(|point| Key::new(&point.observation) == Key::new(selected))
                .ok_or(Error::Bound(
                    "committed App history selected point unavailable",
                ))?;
            if point.observation != *selected {
                return Err(Error::Bound("committed App history selected identity"));
            }
        }

        self.operation.check()
    }
}

impl Cache {
    fn covers(&self, range: &ContextRange, selected: &Observation, target: &Target<'_>) -> bool {
        if self.recording != selected.recording {
            return false;
        }

        if self.kind != target.kind {
            return false;
        }

        if self.key != target.bytes {
            return false;
        }

        if self.range.first_ns > range.first_ns {
            return false;
        }

        self.range.end_ns >= range.end_ns
    }

    fn bytes(&self) -> usize {
        size_of::<Self>()
            + self.key.capacity()
            + self.points.capacity() * size_of::<TracePoint>()
            + self.pins.capacity() * size_of::<usize>()
    }
}

impl Key {
    fn new(observation: &Observation) -> Self {
        Self {
            recording: observation.recording,
            boot: observation.stamp.boot,
            run: observation.stamp.run,
            publication: observation.stamp.publication,
        }
    }
}

impl Reading {
    fn endpoint(&self) -> Endpoint {
        Endpoint {
            identity: RunIdentity {
                recording: Identity(self.observation.recording),
                boot: Identity(self.observation.stamp.boot),
                run: Identity(self.observation.stamp.run),
            },
            stamp: self.observation.stamp,
            run_tick: self.observation.run_tick,
            wall_ns: self.observation.wall_ns,
            usage_refused: self.refused,
            usage_gone: self.gone,
            events_first: self.events_first,
            events_end: self.events_end,
        }
    }

    fn adjacent(&self, selected: &Self) -> bool {
        if self.observation.recording != selected.observation.recording {
            return false;
        }

        if self.observation.stamp.boot != selected.observation.stamp.boot {
            return false;
        }

        if self.observation.stamp.run != selected.observation.stamp.run {
            return false;
        }

        self.observation.stamp.publication.checked_add(1)
            == Some(selected.observation.stamp.publication)
    }
}

fn app_history_insert(restore: &mut Restore, reading: &Reading) -> Result<()> {
    if restore.readings.len() == READINGS_MAX {
        return Err(Error::Bound("committed App history readings"));
    }

    if restore
        .readings
        .insert(Key::new(&reading.observation), *reading)
        .is_some()
    {
        return Err(Error::Bound("duplicate committed App history tick"));
    }

    Ok(())
}

fn app_history_capture(
    projection: &mut Projection<'_>,
    reading: &Reading,
    current: Box<Frame>,
) -> Result<()> {
    assert_eq!(
        current.stamp().publication,
        reading.observation.stamp.publication
    );
    if reading.plotted {
        let previous = projection.reading.as_ref().and_then(|previous| {
            if previous.adjacent(reading) {
                if super::committed_apps::with_app_sample_predecessor(
                    &previous.endpoint(),
                    Some(&current),
                ) {
                    return projection.previous.as_deref();
                }
            }
            None
        });
        projection
            .points
            .push(TracePoint::capture(&current, previous, projection.target)?);
    }
    projection.previous = Some(current);
    projection.reading = Some(*reading);

    Ok(())
}

fn app_history_reading(row: &duckdb::Row<'_>, window: usize, run: &RunIdentity) -> Result<Reading> {
    Ok(Reading {
        window,
        observation: Observation {
            recording: run.recording.0,
            stamp: crate::apps::Stamp {
                boot: run.boot.0,
                run: run.run.0,
                publication: row
                    .get::<_, Option<u64>>(0)?
                    .ok_or(Error::Bound("committed App history publication"))?,
                continuous_ns: row.get(3)?,
                awake_ns: row.get(4)?,
            },
            run_tick: row
                .get::<_, Option<u64>>(1)?
                .ok_or(Error::Bound("committed App history run tick"))?,
            wall_ns: row.get(2)?,
        },
        refused: row
            .get::<_, Option<u32>>(5)?
            .ok_or(Error::Bound("committed App history refused count"))?,
        gone: row
            .get::<_, Option<u32>>(6)?
            .ok_or(Error::Bound("committed App history gone count"))?,
        events_first: row
            .get::<_, Option<u64>>(7)?
            .ok_or(Error::Bound("committed App history event range"))?,
        events_end: row
            .get::<_, Option<u64>>(8)?
            .ok_or(Error::Bound("committed App history event range"))?,
        plotted: false,
    })
}

#[cfg(test)]
mod tests;
