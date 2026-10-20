use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[cfg(test)]
use crate::live_app_history::Target;

use super::{ContextRange, NS, OBSERVATIONS_MAX, Session, WINDOWS_MAX};
use crate::live_app_history::{Selection, TracePoint};
use crate::live_app_response::{KeyKind, Observation};
use crate::live_apps::{Frame, committed::Endpoint};
use collector::ring::{Identity, PROGRAM_PATH_BYTES_MAX, RunIdentity};
use persistence::{Error, Result};
use serde_json::Value;

mod attribution;
pub(crate) use attribution::AttributionReplay;
mod process_groups;
pub(crate) use process_groups::Replay as ProcessGroupsReplay;
mod resident;
mod sealed_ranks;
pub use sealed_ranks::SealedRanks;

pub(super) struct Cache {
    recording: [u64; 2],
    kind: KeyKind,
    key: Vec<u8>,
    executable: Option<Vec<u8>>,
    range: ContextRange,
    points: Vec<TracePoint>,
    pins: Vec<usize>,
}

struct Restore {
    readings: BTreeMap<Key, Reading>,
    sources: BTreeMap<(usize, Source), SourceQuery>,
    source_bytes: usize,
    plotted: usize,
}

struct Projection<'target> {
    target: &'target dyn Selection,
    points: &'target mut Vec<TracePoint>,
    previous: Option<Box<Frame>>,
    reading: Option<Reading>,
}

struct SourceQuery {
    sql: String,
    files: Vec<SourceFile>,
}

struct SourceFile {
    path: PathBuf,
    alias: String,
}

impl SourceQuery {
    fn refresh(&self, connection: &persistence::engine::Engine) -> Result<()> {
        assert!(self.files.len() <= 2);
        for file in &self.files {
            if connection.parquet_source(&file.path)? != file.alias {
                return Err(Error::Bound("changed committed App history source"));
            }
        }

        Ok(())
    }

    fn bytes(&self) -> usize {
        size_of::<Self>()
            + self.sql.capacity()
            + self.files.capacity() * size_of::<SourceFile>()
            + self
                .files
                .iter()
                .map(|file| file.path.capacity() + file.alias.capacity())
                .sum::<usize>()
    }
}

/// The tick rows of each sealed window the App histories read. A sealed window's rows never
/// change and each second's history reads the same windows again, so the Session keeps them.
pub(super) struct Ticks {
    windows: BTreeMap<usize, Vec<Reading>>,
}

impl Ticks {
    pub(super) fn new() -> Self {
        Self {
            windows: BTreeMap::new(),
        }
    }
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
const TICKS_WINDOWS_MAX: usize = 64;
const CACHE_BYTES_MAX: usize = size_of::<Cache>()
    + 2 * PROGRAM_PATH_BYTES_MAX
    + OBSERVATIONS_MAX * size_of::<TracePoint>()
    + READINGS_MAX * size_of::<usize>();

const _: () = {
    assert!(CACHE_BYTES_MAX < 2 * 1024 * 1024);
    assert!(2 * CACHE_BYTES_MAX < 4 * 1024 * 1024);
    assert!(READINGS_MAX <= WINDOWS_MAX);
    assert!(TICKS_PER_WINDOW_MAX <= OBSERVATIONS_MAX);
    assert!(TICKS_WINDOWS_MAX * TICKS_PER_WINDOW_MAX * size_of::<Reading>() < 1024 * 1024);
};

impl Session {
    fn app_history_target_cache(
        &self,
        target: &dyn Selection,
    ) -> &std::cell::RefCell<Option<Cache>> {
        if target.executable().is_some() {
            &self.app_process_history_cache
        } else {
            &self.app_history_cache
        }
    }

    pub(crate) fn with_app_history(
        &self,
        selected: &Observation,
        target: &dyn Selection,
        consume: impl FnOnce(&[TracePoint]) -> Result<Value>,
    ) -> Result<Value> {
        let result = self.operation.attach(&self.connection).and_then(|()| {
            self.operation.run(|| {
                let range = self.app_history_range(selected)?;
                if target.app().bytes.len() > PROGRAM_PATH_BYTES_MAX {
                    return Err(Error::Bound("committed App history key"));
                }

                if collector::recording::read_identity(&self.root)?.0 != selected.recording {
                    return Err(Error::Bound("committed App history recording"));
                }

                let mut cache = self.app_history_target_cache(target).borrow_mut();
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
            *self.app_history_target_cache(target).borrow_mut() = None;
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
        target: &dyn Selection,
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
            kind: target.app().kind,
            key: target.app().bytes.to_vec(),
            executable: target.executable().map(<[u8]>::to_vec),
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
        self.saved_history_context_preparations.set(
            self.saved_history_context_preparations
                .get()
                .saturating_add(1),
        );
        assert!(self.windows.len() <= WINDOWS_MAX);
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
                TICKS_PER_WINDOW_MAX,
                &mut readings,
                |reading| {
                    reading
                        .observation
                        .wall_ns
                        .is_some_and(|wall| (range.first_ns..range.end_ns).contains(&wall))
                },
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
            self.app_history_ticks(restore, window, 1, &mut readings, |reading| {
                reading.observation.stamp.publication == publication
            })?;
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
        maximum: usize,
        output: &mut Vec<Reading>,
        keep: impl Fn(&Reading) -> bool,
    ) -> Result<()> {
        assert!(maximum <= TICKS_PER_WINDOW_MAX);
        assert!(output.is_empty());
        self.app_history_ticks_cache(restore, window)?;
        let ticks = self.app_history_ticks.borrow();
        let readings = ticks
            .windows
            .get(&window)
            .ok_or(Error::Bound("committed App history ticks"))?;
        for reading in readings {
            self.operation.check()?;
            if keep(reading) {
                if output.len() == maximum {
                    return Err(Error::Bound("committed App history tick rows"));
                }
                output.push(*reading);
            }
        }

        Ok(())
    }

    fn app_history_ticks_cache(&self, restore: &mut Restore, window: usize) -> Result<()> {
        // Registering the source pins its file and checks the window is still committed.
        let source = self.app_history_source(restore, window, Source::Ticks)?;
        if self
            .app_history_ticks
            .borrow()
            .windows
            .contains_key(&window)
        {
            return Ok(());
        }

        #[cfg(test)]
        self.tick_queries.set(self.tick_queries.get() + 1);
        let readings = app_history_readings(
            &self.connection,
            &self.operation,
            source,
            window,
            &self.windows[window].identity.run,
        )?;

        let mut ticks = self.app_history_ticks.borrow_mut();
        if ticks.windows.len() == TICKS_WINDOWS_MAX {
            ticks.windows.clear();
        }
        ticks.windows.insert(window, readings);
        Ok(())
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

            let source = self.app_history_source_capture(window, kind)?;

            restore.source_bytes = restore
                .source_bytes
                .checked_add(source.bytes())
                .ok_or(Error::Bound("committed App history source bytes"))?;
            if restore.source_bytes > SOURCE_BYTES_MAX {
                return Err(Error::Bound("committed App history source bytes"));
            }

            restore.sources.insert(key, source);
        }

        let source = restore
            .sources
            .get(&key)
            .ok_or(Error::Bound("committed App history source output"))?;

        // A long history can retain SQL after the engine's bounded scan cache evicts its aliases.
        source.refresh(&self.connection)?;

        Ok(&source.sql)
    }

    fn app_history_source_capture(&self, window: usize, kind: Source) -> Result<SourceQuery> {
        let sql = match kind {
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

        let names: &[&str] = match kind {
            Source::Ticks => &["ticks"],
            Source::Apps => &["processes", "events"],
        };

        let mut files = Vec::with_capacity(names.len());
        for name in names {
            let path = self
                .root
                .join(&self.windows[window].data_directory)
                .join(format!("{name}.parquet"));
            let alias = self.connection.parquet_source(&path)?;

            files.push(SourceFile { path, alias });
        }

        Ok(SourceQuery { sql, files })
    }

    fn app_history_points(
        &self,
        restore: &mut Restore,
        target: &dyn Selection,
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
    fn covers(&self, range: &ContextRange, selected: &Observation, target: &dyn Selection) -> bool {
        if self.recording != selected.recording {
            return false;
        }

        if self.kind != target.app().kind {
            return false;
        }

        if self.key != target.app().bytes {
            return false;
        }

        if self.executable.as_deref() != target.executable() {
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
            + self.executable.as_ref().map_or(0, Vec::capacity)
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
        let previous = sample_predecessor(
            projection.reading.as_ref(),
            projection.previous.as_deref(),
            reading,
            &current,
        );
        projection
            .points
            .push(TracePoint::capture(&current, previous, projection.target)?);
    }
    projection.previous = Some(current);
    projection.reading = Some(*reading);

    Ok(())
}

/// The frame a sample of `reading` takes its rates from: the frame of the reading just before
/// it, when that is the observation before it in its run and the collector took both on
/// consecutive ticks.
fn sample_predecessor<'frame>(
    previous: Option<&Reading>,
    previous_frame: Option<&'frame Frame>,
    reading: &Reading,
    current: &Frame,
) -> Option<&'frame Frame> {
    let previous = previous?;
    if previous.adjacent(reading) {
        if super::committed_apps::with_app_sample_predecessor(&previous.endpoint(), Some(current)) {
            return previous_frame;
        }
    }

    None
}

/// The readings of a sealed window's ticks, which the relation `source` holds, in publication
/// order.
fn app_history_readings(
    connection: &persistence::engine::Engine,
    operation: &crate::cancellation::Operation,
    source: &str,
    window: usize,
    run: &RunIdentity,
) -> Result<Vec<Reading>> {
    let limit = TICKS_PER_WINDOW_MAX + 1;
    let frame = connection.query(&format!(
        "SELECT publication, run_tick, wall_ns, continuous_ns, awake_ns, processes_refused, \
        processes_gone, events_first, events_end FROM {source} \
        ORDER BY publication LIMIT {limit}"
    ))?;
    if frame.height() > TICKS_PER_WINDOW_MAX {
        return Err(Error::Bound("committed App history tick rows"));
    }

    let mut readings = Vec::with_capacity(frame.height());
    for index in 0..frame.height() {
        operation.check()?;
        let row = persistence::engine::Row::new(&frame, index)?;
        readings.push(app_history_reading(&row, window, run)?);
    }

    Ok(readings)
}

fn app_history_reading(
    row: &persistence::engine::Row<'_>,
    window: usize,
    run: &RunIdentity,
) -> Result<Reading> {
    Ok(Reading {
        window,
        observation: Observation {
            recording: run.recording.0,
            stamp: crate::apps::Stamp {
                boot: run.boot.0,
                run: run.run.0,
                publication: row
                    .get::<Option<u64>>(0)?
                    .ok_or(Error::Bound("committed App history publication"))?,
                continuous_ns: row.get(3)?,
                awake_ns: row.get(4)?,
            },
            run_tick: row
                .get::<Option<u64>>(1)?
                .ok_or(Error::Bound("committed App history run tick"))?,
            wall_ns: row.get(2)?,
        },
        refused: row
            .get::<Option<u32>>(5)?
            .ok_or(Error::Bound("committed App history refused count"))?,
        gone: row
            .get::<Option<u32>>(6)?
            .ok_or(Error::Bound("committed App history gone count"))?,
        events_first: row
            .get::<Option<u64>>(7)?
            .ok_or(Error::Bound("committed App history event range"))?,
        events_end: row
            .get::<Option<u64>>(8)?
            .ok_or(Error::Bound("committed App history event range"))?,
        plotted: false,
    })
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod source_cache_tests {
    use super::*;
    use crate::live::identity_fixture::Fixture;

    #[test]
    fn rebinds_retained_app_sql_after_unrelated_sources_evict_its_native_relations() {
        // Goal: a long history can reuse normalized SQL after the bounded engine cache turns.
        // Method: evict a real App source pair, then reuse its Restore entry without normalizing.
        let mut fixture = Fixture::new();
        fixture.publish_with_system_rows(1001);
        fixture.publish_with_system_rows(1002);
        fixture.seal();
        let session = Session::open(&fixture.root).unwrap();
        let mut restore = Restore {
            readings: BTreeMap::new(),
            sources: BTreeMap::new(),
            source_bytes: 0,
            plotted: 0,
        };
        let original = session
            .app_history_source(&mut restore, 0, Source::Apps)
            .unwrap()
            .to_owned();
        let sql = format!("SELECT * FROM {original} ORDER BY publication,pid");
        let expected = session.connection.query(&sql).unwrap();
        let reads = session.source_reads.get();
        let path = fixture
            .root
            .join(&session.windows[0].data_directory)
            .join("processes.parquet");
        for index in 0..persistence::engine::PARQUET_SOURCES_MAX {
            let copied = fixture.root.join(format!("query-source-{index}.parquet"));
            std::fs::copy(&path, &copied).unwrap();
            session.connection.parquet_source(&copied).unwrap();
        }
        assert!(session.connection.query(&sql).is_err());

        let rebound = session
            .app_history_source(&mut restore, 0, Source::Apps)
            .unwrap();
        let restored = session
            .connection
            .query(&format!("SELECT * FROM {rebound} ORDER BY publication,pid"))
            .unwrap();

        assert_eq!(rebound, original);
        assert_eq!(session.source_reads.get(), reads);
        assert_eq!(restored, expected);
    }
}
