//! Viewer-only bounded projection of registered live rings alongside committed history.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use collector::recording::{Registry, identity_text};
use collector::ring::{Found, FoundEvent, IdentityObservation, RingReader, RunIdentity, Tick};
use duckdb::Connection;
use persistence::{Error, Limits, Losses, NumberedEvent, Result, Window, WindowIdentity};
use serde_json::{Value, json};

use crate::cancellation::Operation;
use crate::data::{self, Table};
use crate::session::{OBSERVATIONS_MAX, Observation, PAGE_ROWS};
use crate::{Cancellation, Session, metrics};

const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;
const CACHE_BYTES_MAX: usize = 128 * 1024 * 1024;
const SOURCES_MAX: usize = 2;
const PENDING_WINDOWS_MAX: usize = OBSERVATIONS_MAX;
const WINDOW_MARKERS_PER_POLL_MAX: usize = 16;
const HISTORY_REFRESH_POLLS_MAX: usize = 256;
const _: () = {
    assert!(OBSERVATIONS_MAX <= 4096);
    assert!(CACHE_BYTES_MAX >= 32 * 1024 * 1024);
    assert!(WINDOW_MARKERS_PER_POLL_MAX > 0);
    assert!(HISTORY_REFRESH_POLLS_MAX >= PENDING_WINDOWS_MAX.div_ceil(WINDOW_MARKERS_PER_POLL_MAX));
};

pub struct LiveSession {
    root: PathBuf,
    history: Option<Session>,
    sources: Vec<Source>,
    observations: VecDeque<LiveObservation>,
    points: VecDeque<Value>,
    connection: Connection,
    operation: Operation,
    cancellation: Arc<Cancellation>,
    selected: Option<Value>,
    inspected: Option<LiveObservation>,
    cache_bytes: usize,
    #[cfg(test)]
    before_projection: Option<Box<dyn FnOnce()>>,
}

struct Source {
    reader: RingReader,
    tick: Tick,
    identities: Vec<IdentityObservation>,
    identity: RunIdentity,
    collector_pid: u32,
    losses: Losses,
    window: Option<u64>,
    pending_windows: BTreeSet<u64>,
    pending_cursor: Option<u64>,
    pending_retention_limited: bool,
    pending_refresh_polls: usize,
}

struct ProjectionEvents {
    events: Vec<(u64, u64, collector::tracker::Event)>,
    identities: Vec<IdentityObservation>,
}

impl Source {
    fn read_tick(&mut self) -> std::io::Result<Found> {
        if self.reader.dictionary_capacity().is_some() {
            self.reader
                .read_identity_into(&mut self.tick, &mut self.identities)
        } else {
            self.reader.read_into(&mut self.tick)
        }
    }
}

struct LiveObservation {
    identity: RunIdentity,
    observation: Observation,
    losses: Losses,
    tables: BTreeMap<&'static str, Table>,
    point: Value,
    bytes: usize,
}

impl LiveObservation {
    fn json(&self) -> Value {
        json!({"run": identity_text(self.identity.run).to_string(),
            "boot": identity_text(self.identity.boot).to_string(),
            "publication": self.observation.publication, "run_tick": self.observation.run_tick,
            "wall_ns": self.observation.wall, "window_losses": null, "reader_losses": {
                "publications": self.losses.publications, "events": self.losses.events,
                "unknown": self.losses.unknown}})
    }
}

impl LiveSession {
    pub fn open(root: &Path) -> Result<Self> {
        Self::open_with(root, Arc::new(Cancellation::default()), 0)
    }

    pub fn open_with(root: &Path, cancellation: Arc<Cancellation>, epoch: u64) -> Result<Self> {
        persistence::query::validate_root(root)?;
        collector::recording::private::verify_directory(root)?;
        collector::recording::read_identity(root)?;
        let operation = cancellation.clone().operation(epoch)?;
        let connection = data::connection()?;
        operation.attach(&connection)?;
        for (table, columns, _) in persistence::schema::TABLES {
            connection.execute_batch(&format!("CREATE TABLE {} ({columns})", table.name()))?;
        }
        let mut session = Self {
            root: root.to_owned(),
            history: None,
            sources: Vec::with_capacity(SOURCES_MAX),
            observations: VecDeque::new(),
            points: VecDeque::new(),
            connection,
            operation,
            cancellation,
            selected: None,
            inspected: None,
            cache_bytes: 0,
            #[cfg(test)]
            before_projection: None,
        };
        session.refresh_history()?;
        session.poll()?;
        Ok(session)
    }

    pub fn begin(&mut self, epoch: u64) -> Result<()> {
        if self.operation.epoch() != epoch {
            self.selected = None;
        }
        self.operation = self.operation.begin(epoch)?;
        self.operation.attach(&self.connection)?;
        if let Some(history) = &mut self.history {
            history.begin(epoch)?;
        }
        Ok(())
    }

    pub fn poll(&mut self) -> Result<bool> {
        let operation = self.operation.begin(self.operation.epoch())?;
        operation.attach(&self.connection)?;
        operation.run(|| self.poll_inner())
    }

    fn poll_inner(&mut self) -> Result<bool> {
        self.operation.check()?;
        self.poll_sources()?;
        let mut changed = false;
        for index in 0..self.sources.len() {
            for _attempt in 0..=OBSERVATIONS_MAX {
                let source = &mut self.sources[index];
                let found = source.read_tick();
                match found? {
                    Found::Nothing => break,
                    Found::Missed { first, end } => {
                        self.sources[index].losses.publications += end - first;
                    }
                    Found::Publication { publication, .. } => {
                        #[cfg(test)]
                        if let Some(barrier) = self.before_projection.take() {
                            barrier();
                        }
                        if self.poll_publication(index, publication)? {
                            changed = true;
                        }
                    }
                }
                self.operation.check()?;
            }
        }
        self.poll_commits()?;
        Ok(changed)
    }

    #[must_use]
    pub fn catalog(&self) -> Value {
        let mut catalog = self.history.as_ref().map_or_else(
            || {
                json!({"path": self.root,
            "first_second": null, "last_second": null, "windows": 0,
            "unknown_clock_ticks": 0,
            "tables": persistence::schema::TABLES.map(|entry| entry.0.name())})
            },
            Session::catalog,
        );
        for point in &self.points {
            let second = point["wall_ns"]
                .as_u64()
                .map(|wall| wall / NANOSECONDS_PER_SECOND);
            assert!(second.is_some());
            let Some(second) = second else { continue };
            let first = catalog["first_second"]
                .as_u64()
                .map_or(second, |bound| bound.min(second));
            let last = catalog["last_second"]
                .as_u64()
                .map_or(second, |bound| bound.max(second));
            catalog["first_second"] = json!(first);
            catalog["last_second"] = json!(last);
        }
        catalog["commit_discovery"] = catalog_commit_discovery(&self.sources);
        catalog["live"] = json!(true);
        catalog["tail_ticks"] = json!(self.observations.len());
        catalog["tail_cache_bytes"] = json!(self.cache_bytes);
        catalog["tail_cache_bytes_max"] = json!(CACHE_BYTES_MAX);
        catalog["latest_observation"] =
            json!(self.points.back().map(|point| &point["observation"]));
        catalog["latest_collector_pid"] = json!(self.latest_collector_pid());
        catalog["collector_completed"] =
            json!(self.sources.iter().all(|source| source.reader.completed()));
        catalog["reader_losses"] = json!(
            self.sources
                .iter()
                .map(|source| json!({
                    "run": identity_text(source.identity.run).to_string(),
                    "boot": identity_text(source.identity.boot).to_string(),
                    "losses": {"publications": source.losses.publications,
                        "events": source.losses.events, "unknown": source.losses.unknown}
                }))
                .collect::<Vec<_>>()
        );
        catalog
    }

    pub fn select(&mut self, second: u64, alternative: usize) -> Result<Value> {
        self.selected = None;
        if let Some(inspected) = self.inspected.take() {
            self.cache_bytes -= inspected.bytes;
        }
        self.operation.check()?;
        second
            .checked_add(1)
            .and_then(|end| end.checked_mul(NANOSECONDS_PER_SECOND))
            .ok_or(Error::Bound("selection time"))?;
        if self.select_refresh_due(second) {
            self.refresh_history()?;
        }
        let mut response = match &mut self.history {
            Some(history) => {
                history.begin(self.operation.epoch())?;
                history.select(second, 0)?
            }
            None => json!({"second": second, "selected": null, "alternatives": [],
                "summary": null, "trends": [], "context_seconds": 900}),
        };
        let alternatives = response["alternatives"]
            .as_array_mut()
            .ok_or(Error::Bound("alternatives shape"))?;
        self.select_alternatives(second, alternatives)?;
        alternatives.sort_by(|left, right| identity_order(left).cmp(&identity_order(right)));
        let selected = if alternatives.is_empty() {
            None
        } else {
            Some(
                alternatives
                    .get(alternative)
                    .ok_or(Error::Bound("observation alternative"))?
                    .clone(),
            )
        };
        self.select_trends(second, &mut response)?;
        self.select_observation(selected, &mut response)?;
        if let Some(selected) = &self.selected {
            response["summary"] = response["trends"]
                .as_array()
                .ok_or(Error::Bound("trends shape"))?
                .iter()
                .find(|point| same(point, selected))
                .ok_or(Error::Bound("selected context point"))?
                .clone();
        }
        Ok(response)
    }

    /// Follows only points observed by this session, without querying committed history.
    pub fn follow_latest(&mut self) -> Result<Value> {
        self.selected = None;
        if let Some(inspected) = self.inspected.take() {
            self.cache_bytes -= inspected.bytes;
        }

        self.operation
            .begin(self.operation.epoch())?
            .run(|| self.follow_latest_inner())
    }

    fn follow_latest_inner(&mut self) -> Result<Value> {
        let point = self
            .points
            .back()
            .ok_or(Error::Bound("no live observation"))?;
        let current = self
            .observations
            .iter()
            .find(|observation| same(&observation.point, point))
            .ok_or(Error::Bound("latest live raw observation unavailable"))?;
        let second = current.observation.wall / NANOSECONDS_PER_SECOND;
        let selected = current.json();
        let summary = metrics::public_point(&current.point)?;

        let mut trends = Vec::with_capacity(self.points.len());
        let mut alternatives = Vec::new();
        for point in &self.points {
            self.operation.check()?;
            let wall_second = point["wall_ns"]
                .as_u64()
                .ok_or(Error::Bound("point clock"))?
                / NANOSECONDS_PER_SECOND;
            if (second.saturating_sub(899)..=second).contains(&wall_second) {
                trends.push(metrics::public_point(point)?);
            }
            if wall_second == second {
                alternatives.push(point["observation"].clone());
            }
        }

        if trends.len() > OBSERVATIONS_MAX {
            return Err(Error::Bound("context observations"));
        }

        trends.sort_by(|left, right| identity_order(left).cmp(&identity_order(right)));
        alternatives.sort_by(|left, right| identity_order(left).cmp(&identity_order(right)));

        self.operation.check()?;
        let response = json!({"second": second, "selected": selected,
            "alternatives": alternatives, "summary": summary, "trends": trends,
            "context_seconds": 900});
        self.selected = Some(selected);

        Ok(response)
    }

    pub fn page(&self, name: &str, offset: u64) -> Result<Value> {
        self.operation.check()?;
        let selected = self
            .selected
            .as_ref()
            .ok_or(Error::Bound("no selected observation"))?;
        if let Some(observation) = self
            .inspected
            .iter()
            .chain(self.observations.iter())
            .find(|observation| same(&observation.json(), selected))
        {
            if !offset.is_multiple_of(PAGE_ROWS) {
                return Err(Error::Bound("page offset"));
            }
            let table = observation
                .tables
                .get(name)
                .ok_or(Error::Bound("table name"))?;
            let start = usize::try_from(offset).map_err(|_| Error::Bound("page offset"))?;
            if start > table.rows.len() {
                return Err(Error::Bound("page past end"));
            }
            let end = (start
                + usize::try_from(PAGE_ROWS).map_err(|_| Error::Bound("page size"))?)
            .min(table.rows.len());
            return Ok(json!({"name": name, "columns": table.columns,
                "rows": table.rows[start..end], "offset": offset, "total": table.rows.len(),
                "observation": selected}));
        }
        self.history
            .as_ref()
            .ok_or(Error::Bound("missing historical selection"))?
            .page(name, offset)
    }

    fn select_refresh_due(&self, second: u64) -> bool {
        self.observations.front().is_none_or(|observation| {
            second < observation.observation.wall / NANOSECONDS_PER_SECOND
        })
    }

    fn refresh_history(&mut self) -> Result<()> {
        if self.root.join("commits").try_exists()? {
            let mut history = Session::open_with(
                &self.root,
                self.cancellation.clone(),
                self.operation.epoch(),
            )?;
            if let Some(selected) = &self.selected {
                let second = selected["wall_ns"]
                    .as_u64()
                    .ok_or(Error::Bound("selected clock"))?
                    / NANOSECONDS_PER_SECOND;
                let response = history.select(second, 0)?;
                let alternatives = response["alternatives"]
                    .as_array()
                    .ok_or(Error::Bound("historical alternatives"))?;
                if let Some(index) = alternatives
                    .iter()
                    .position(|observation| same(observation, selected))
                {
                    history.select(second, index)?;
                }
            }
            refresh_history_pending(&mut self.sources, &history);
            self.history = Some(history);
        }
        Ok(())
    }

    fn poll_commits(&mut self) -> Result<()> {
        let mut committed = false;
        let mut refresh_due = false;
        for source in &mut self.sources {
            if poll_commit_history_due(source) {
                refresh_due = true;
            }
            for window in poll_commit_candidates(source) {
                let identity = WindowIdentity {
                    run: source.identity,
                    window,
                };
                let marker = persistence::layout::WindowKey::from(&identity).marker(&self.root);
                if marker.try_exists()? {
                    committed = true;
                }
            }
        }
        if !committed {
            if !refresh_due {
                return Ok(());
            }
        }

        self.refresh_history()
    }

    fn poll_sources(&mut self) -> Result<()> {
        let recording = collector::recording::read_identity(&self.root)?;
        let mut buffer = [0; 4097];
        let registry = Registry::read_into(&self.root.join("runs"), &mut buffer)?;
        let mut retained = [None; SOURCES_MAX];
        for (index, run) in registry.runs().enumerate() {
            retained[index] = Some(run?.identity);
        }
        self.sources
            .retain(|source| retained.contains(&Some(source.identity)));
        for run in registry.runs() {
            let run = run?;
            if run.identity.recording != recording {
                return Err(Error::Bound("foreign live recording"));
            }

            if self
                .sources
                .iter()
                .any(|source| source.identity == run.identity)
            {
                continue;
            }

            let reader = match RingReader::open_generation(run.ring_name) {
                Ok(reader) => reader,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };

            let Some(identity) = reader.identity() else {
                // Legacy recovery entries have no boot or clock identity to follow live.
                continue;
            };
            if identity != run.identity {
                return Err(Error::Bound("changed live identity"));
            }

            if self.sources.len() == SOURCES_MAX {
                return Err(Error::Bound("live sources"));
            }

            if reader.mapped_size_bytes() > 1024 * 1024 * 1024 {
                return Err(Error::Bound("live mapping size"));
            }

            self.poll_sources_insert(reader, run.identity, run.collector_pid)?;
        }

        Ok(())
    }

    fn poll_sources_insert(
        &mut self,
        reader: RingReader,
        identity: RunIdentity,
        collector_pid: u32,
    ) -> Result<()> {
        let tick = persistence::tick_buffer(&reader.shape())?;
        let identities = if reader.dictionary_capacity().is_some() {
            Vec::with_capacity(tick.processes.capacity())
        } else {
            Vec::new()
        };

        self.sources.push(Source {
            reader,
            tick,
            identities,
            identity,
            collector_pid,
            losses: Losses::default(),
            window: None,
            pending_windows: BTreeSet::new(),
            pending_cursor: None,
            pending_retention_limited: false,
            pending_refresh_polls: 0,
        });

        Ok(())
    }

    fn latest_collector_pid(&self) -> Option<u32> {
        let current = self.observations.back()?;

        self.sources
            .iter()
            .find(|source| source.identity == current.identity)
            .map(|source| source.collector_pid)
    }

    fn poll_publication(&mut self, index: usize, publication: u64) -> Result<bool> {
        let losses = self.sources[index].losses;
        let projected = self
            .poll_observation(index, publication)
            .and_then(|observation| {
                if let Some(observation) = observation {
                    self.poll_store(observation)?;
                    return Ok(true);
                }
                Ok(false)
            });
        match projected {
            Ok(changed) => Ok(changed),
            Err(error) => {
                self.sources[index].reader.seek_publication(publication)?;
                self.sources[index].losses = losses;
                Err(error)
            }
        }
    }

    fn poll_observation(
        &mut self,
        index: usize,
        publication: u64,
    ) -> Result<Option<LiveObservation>> {
        let source = &mut self.sources[index];
        if let Some(times) = source.tick.times {
            poll_observation_window(source, self.history.as_ref(), times.continuous_ns)?;
        }
        project_observation(source, publication, &self.connection, &self.operation)
    }

    fn poll_store(&mut self, mut current: LiveObservation) -> Result<()> {
        if current.bytes > CACHE_BYTES_MAX {
            return Err(Error::Bound("live observation bytes"));
        }
        let previous = self
            .observations
            .iter()
            .rev()
            .find(|observation| observation.identity == current.identity);
        let mut tables = metrics::Tables::new();
        for name in metrics::TABLE_NAMES {
            let mut publications = BTreeMap::new();
            if let Some(previous) = previous {
                publications.insert(
                    previous.observation.publication,
                    previous.tables[name].clone(),
                );
            }
            publications.insert(
                current.observation.publication,
                current.tables[name].clone(),
            );
            tables.insert((0, name), publications);
        }
        let segment = self.poll_segment(&current)?;
        current.point = metrics::point(
            &tables,
            &current.observation,
            previous.map(|observation| &observation.observation),
            segment,
        )?;
        current.point["run"] = json!(identity_text(current.identity.run).to_string());
        current.point["boot"] = json!(identity_text(current.identity.boot).to_string());
        current.point["observation"] = current.json();
        let first = current
            .observation
            .wall
            .saturating_sub(899 * NANOSECONDS_PER_SECOND);
        self.poll_points(&current.point, first)?;
        self.cache_bytes += current.bytes;
        self.observations.push_back(current);
        self.poll_evict(first)
    }

    fn poll_evict(&mut self, first: u64) -> Result<()> {
        for _attempt in 0..=OBSERVATIONS_MAX {
            let expired = self
                .observations
                .front()
                .is_some_and(|observation| observation.observation.wall < first);
            let count = self.observations.len() + usize::from(self.inspected.is_some());
            if count <= OBSERVATIONS_MAX {
                if self.cache_bytes <= CACHE_BYTES_MAX {
                    if !expired {
                        return Ok(());
                    }
                }
            }
            let removed = self
                .observations
                .pop_front()
                .ok_or(Error::Bound("live cache"))?;
            self.cache_bytes -= removed.bytes;
        }
        Err(Error::Bound("live eviction count"))
    }

    fn poll_points(&mut self, point: &Value, first: u64) -> Result<()> {
        self.points.push_back(point.clone());
        for _attempt in 0..=OBSERVATIONS_MAX {
            let expired = match self.points.front() {
                Some(point) => {
                    point["wall_ns"]
                        .as_u64()
                        .ok_or(Error::Bound("point clock"))?
                        < first
                }
                None => return Ok(()),
            };
            if self.points.len() <= OBSERVATIONS_MAX {
                if !expired {
                    return Ok(());
                }
            }
            self.points.pop_front().ok_or(Error::Bound("point cache"))?;
        }
        Err(Error::Bound("point eviction count"))
    }

    fn poll_segment(&self, current: &LiveObservation) -> Result<u64> {
        Ok(match self.observations.back() {
            Some(previous) => {
                let old = previous.point["segment"]
                    .as_u64()
                    .ok_or(Error::Bound("segment"))?;
                if previous.identity == current.identity {
                    if metrics::compatible(&previous.observation, &current.observation) {
                        old
                    } else {
                        old + 1
                    }
                } else {
                    old + 1
                }
            }
            None => 1,
        })
    }

    fn select_alternatives(&self, second: u64, alternatives: &mut Vec<Value>) -> Result<()> {
        for observation in &self.observations {
            if observation.observation.wall / NANOSECONDS_PER_SECOND == second {
                select_alternative(alternatives, observation.json());
            }
        }
        for point in &self.points {
            if point["wall_ns"]
                .as_u64()
                .map(|wall| wall / NANOSECONDS_PER_SECOND)
                == Some(second)
            {
                if let Some(source) = self
                    .sources
                    .iter()
                    .find(|source| source_matches(source, point))
                {
                    let publication = point["publication"]
                        .as_u64()
                        .ok_or(Error::Bound("point publication"))?;
                    let newest = source.reader.publication_newest()?;
                    let oldest = newest
                        .saturating_sub(source.reader.shape().slot_count - 1)
                        .max(source.reader.publication_first());
                    if publication >= oldest {
                        if publication <= newest {
                            select_alternative(alternatives, point["observation"].clone());
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn select_rehydrate(&self, selected: &Value) -> Result<Option<LiveObservation>> {
        if self.history.as_ref().is_some_and(|history| {
            history.windows.iter().any(|manifest| {
                if !identity_matches(manifest.identity.run, selected) {
                    return false;
                }
                let publication = selected["publication"].as_u64();
                publication.is_some_and(|publication| {
                    publication >= manifest.publication_first
                        && publication < manifest.publication_end
                })
            })
        }) {
            return Ok(None);
        }

        let Some(point) = self.points.iter().find(|point| same(point, selected)) else {
            return Ok(None);
        };

        let Some(source) = self
            .sources
            .iter()
            .find(|source| source_matches(source, selected))
        else {
            return Ok(None);
        };

        let mut independent = None;
        rehydrate_source(source, selected, &mut independent)?;
        let Some(mut independent) = independent else {
            unreachable!("inspection source initialized before projection");
        };

        let publication = selected["publication"]
            .as_u64()
            .ok_or(Error::Bound("selected publication"))?;
        let mut current = project_observation(
            &mut independent,
            publication,
            &self.connection,
            &self.operation,
        )?
        .ok_or(Error::Bound("unsealed observation clock"))?;
        if current.bytes > CACHE_BYTES_MAX {
            return Err(Error::Bound("live observation bytes"));
        }

        current.point = point.clone();

        Ok(Some(current))
    }

    fn select_trends(&self, second: u64, response: &mut Value) -> Result<()> {
        let points = response["trends"]
            .as_array_mut()
            .ok_or(Error::Bound("trends shape"))?;
        for point in &self.points {
            let current = point["wall_ns"]
                .as_u64()
                .ok_or(Error::Bound("point clock"))?
                / NANOSECONDS_PER_SECOND;
            if (second.saturating_sub(899)..=second).contains(&current) {
                if !points.iter().any(|existing| same(existing, point)) {
                    points.push(metrics::public_point(point)?);
                }
            }
        }
        if points.len() > OBSERVATIONS_MAX {
            return Err(Error::Bound("context observations"));
        }
        points.sort_by(|left, right| identity_order(left).cmp(&identity_order(right)));
        select_segments(points)
    }

    fn select_observation(
        &mut self,
        mut selected: Option<Value>,
        response: &mut Value,
    ) -> Result<()> {
        let already_selected = selected
            .as_ref()
            .is_some_and(|chosen| same(&response["selected"], chosen));
        if !already_selected {
            if let Some(selected) = &selected {
                if let Some(observation) = self
                    .observations
                    .iter()
                    .find(|observation| same(&observation.json(), selected))
                {
                    response["summary"] = observation.point.clone();
                } else {
                    if let Some(inspected) = self.select_rehydrate(selected)? {
                        response["summary"] = inspected.point.clone();
                        self.cache_bytes += inspected.bytes;
                        self.inspected = Some(inspected);
                        self.poll_evict(0)?;
                    } else {
                        self.select_history(selected, response)?;
                    }
                }
            } else {
                response["summary"] = Value::Null;
            }
        }
        if let Some(inspected) = &self.inspected {
            let actual = inspected.json();
            let alternatives = response["alternatives"]
                .as_array_mut()
                .ok_or(Error::Bound("alternatives shape"))?;
            if let Some(alternative) = alternatives.iter_mut().find(|value| same(value, &actual)) {
                *alternative = actual.clone();
            }
            selected = Some(actual);
        }
        response["selected"] = json!(selected);
        self.selected = selected;
        Ok(())
    }

    fn select_history(&mut self, selected: &Value, response: &mut Value) -> Result<()> {
        let history = self
            .history
            .as_mut()
            .ok_or(Error::Bound("missing history"))?;
        let second = response["second"]
            .as_u64()
            .ok_or(Error::Bound("selection second"))?;
        let historical = history.select(second, 0)?;
        let alternatives = historical["alternatives"]
            .as_array()
            .ok_or(Error::Bound("historical alternatives"))?;
        let index = alternatives
            .iter()
            .position(|observation| same(observation, selected))
            .ok_or(Error::Bound("historical observation"))?;
        response["summary"] = history.select(second, index)?["summary"].clone();
        Ok(())
    }
}

fn select_alternative(alternatives: &mut Vec<Value>, identity: Value) {
    if !alternatives
        .iter()
        .any(|observation| same(observation, &identity))
    {
        alternatives.push(identity);
    }
}

fn source_matches(source: &Source, observation: &Value) -> bool {
    identity_matches(source.identity, observation)
}

fn identity_matches(identity: RunIdentity, observation: &Value) -> bool {
    observation["run"] == identity_text(identity.run).to_string()
        && observation["boot"] == identity_text(identity.boot).to_string()
}

fn rehydrate_source(
    source: &Source,
    selected: &Value,
    output: &mut Option<Box<Source>>,
) -> Result<()> {
    assert!(output.is_none());

    let reader = RingReader::open_generation(source.reader.name())?;
    if reader.identity() != Some(source.identity) {
        return Err(Error::Bound("changed inspection source identity"));
    }

    let tick = persistence::tick_buffer(&reader.shape())?;
    let identities = if reader.dictionary_capacity().is_some() {
        Vec::with_capacity(tick.processes.capacity())
    } else {
        Vec::new()
    };
    let losses = &selected["reader_losses"];
    let independent = output.insert(Box::new(Source {
        reader,
        tick,
        identities,
        identity: source.identity,
        collector_pid: source.collector_pid,
        losses: Losses {
            publications: losses["publications"]
                .as_u64()
                .ok_or(Error::Bound("reader publication losses"))?,
            events: losses["events"]
                .as_u64()
                .ok_or(Error::Bound("reader event losses"))?,
            unknown: losses["unknown"]
                .as_bool()
                .ok_or(Error::Bound("reader unknown losses"))?,
        },
        window: None,
        pending_windows: BTreeSet::new(),
        pending_cursor: None,
        pending_retention_limited: false,
        pending_refresh_polls: 0,
    }));

    rehydrate_source_read(independent, selected)
}

fn rehydrate_source_read(source: &mut Source, selected: &Value) -> Result<()> {
    let publication = selected["publication"]
        .as_u64()
        .ok_or(Error::Bound("selected publication"))?;

    source.reader.seek_publication(publication)?;
    match source.read_tick()? {
        Found::Publication {
            publication: found, ..
        } if found == publication => {}
        _ => return Err(Error::Bound("unsealed publication no longer retained")),
    }

    if Some(source.tick.run_tick) != selected["run_tick"].as_u64() {
        return Err(Error::Bound("changed inspection tick identity"));
    }

    if source.tick.times.map(|times| times.wall_ns) != selected["wall_ns"].as_u64() {
        return Err(Error::Bound("changed inspection wall identity"));
    }

    Ok(())
}

fn project_observation(
    source: &mut Source,
    publication: u64,
    connection: &Connection,
    operation: &Operation,
) -> Result<Option<LiveObservation>> {
    let Some(times) = source.tick.times else {
        source.losses.unknown = true;
        return Ok(None);
    };

    operation.check()?;

    let mut window = Window::new(
        WindowIdentity {
            run: source.identity,
            window: 0,
        },
        Limits::from_shape(&source.reader.shape())?,
    );
    project_observation_stage(source, publication, &times, &mut window)?;

    let tables = project_observation_tables(connection, operation, &window)?;
    let bytes = tables.values().map(table_bytes).sum();
    let observation = Observation {
        window: 0,
        publication,
        run_tick: source.tick.run_tick,
        wall: times.wall_ns,
        continuous: Some(times.continuous_ns),
    };

    Ok(Some(LiveObservation {
        identity: source.identity,
        observation,
        losses: source.losses,
        tables,
        point: Value::Null,
        bytes,
    }))
}

fn project_observation_stage(
    source: &mut Source,
    publication: u64,
    times: &collector::ring::ObservationTimes,
    window: &mut Window,
) -> Result<()> {
    let count = source
        .tick
        .events
        .end
        .checked_sub(source.tick.events.start)
        .ok_or(Error::Bound("live event range"))?;
    let capacity = usize::try_from(count)
        .map_err(|_| Error::Bound("live event range"))?
        .min(collector::tracker::EVENTS_PER_TICK_MAX);

    let mut events = ProjectionEvents {
        events: Vec::with_capacity(capacity),
        identities: if source.reader.dictionary_capacity().is_some() {
            Vec::with_capacity(capacity)
        } else {
            Vec::new()
        },
    };

    project_observation_stage_events(source, &mut events)?;

    let numbered: Vec<_> = events
        .events
        .iter()
        .map(|(number, tick, event)| NumberedEvent {
            number: *number,
            tick: *tick,
            event,
        })
        .collect();
    let observation = persistence::Observation {
        publication,
        wall_ns: Some(times.wall_ns),
        continuous_ns: Some(times.continuous_ns),
        awake_ns: Some(times.awake_ns),
    };

    if source.reader.dictionary_capacity().is_some() {
        window.push_tick_with_identity(
            &observation,
            &source.tick,
            &numbered,
            &persistence::IdentitySource {
                reader: &source.reader,
                processes: &source.identities,
                events: &events.identities,
            },
        )?;
    } else {
        window.push_tick(&observation, &source.tick, &numbered)?;
    }

    Ok(())
}

fn refresh_history_pending(sources: &mut [Source], history: &Session) {
    for source in sources {
        let before_count = source.pending_windows.len();
        let mut removed_count = 0;
        for manifest in &history.windows {
            if manifest.identity.run == source.identity {
                if source.pending_windows.remove(&manifest.identity.window) {
                    removed_count += 1;
                }
            }
        }
        assert_eq!(source.pending_windows.len() + removed_count, before_count);
    }
}

fn catalog_commit_discovery(sources: &[Source]) -> Value {
    json!(
        sources
            .iter()
            .map(|source| json!({
                "run": identity_text(source.identity.run).to_string(),
                "boot": identity_text(source.identity.boot).to_string(),
                "pending_windows": source.pending_windows.len(),
                "tracked_windows_max": PENDING_WINDOWS_MAX,
                "markers_per_poll_max": WINDOW_MARKERS_PER_POLL_MAX,
                "seconds_before_tail_refresh_history": true,
                "history_refresh_polls_max": source.pending_retention_limited
                    .then_some(HISTORY_REFRESH_POLLS_MAX)
            }))
            .collect::<Vec<_>>()
    )
}

fn poll_commit_history_due(source: &mut Source) -> bool {
    assert!(source.pending_refresh_polls < HISTORY_REFRESH_POLLS_MAX);
    if !source.pending_retention_limited {
        return false;
    }
    source.pending_refresh_polls += 1;
    if source.pending_refresh_polls == HISTORY_REFRESH_POLLS_MAX {
        source.pending_refresh_polls = 0;
        return true;
    }
    false
}

fn poll_commit_candidates(source: &mut Source) -> Vec<u64> {
    assert!(source.pending_windows.len() <= PENDING_WINDOWS_MAX);
    let count = source
        .pending_windows
        .len()
        .min(WINDOW_MARKERS_PER_POLL_MAX);
    let mut candidates = Vec::with_capacity(count);
    if let Some(cursor) = source.pending_cursor {
        candidates.extend(
            source
                .pending_windows
                .range((
                    std::ops::Bound::Excluded(cursor),
                    std::ops::Bound::Unbounded,
                ))
                .take(count)
                .copied(),
        );
    }
    let remaining = count - candidates.len();
    candidates.extend(source.pending_windows.iter().take(remaining).copied());
    source.pending_cursor = candidates.last().copied();
    assert!(candidates.len() <= WINDOW_MARKERS_PER_POLL_MAX);
    candidates
}

fn poll_observation_window(
    source: &mut Source,
    history: Option<&Session>,
    continuous: u64,
) -> Result<()> {
    let origin = source
        .reader
        .run_start_continuous_ns()
        .ok_or(Error::Bound("live source origin"))?;
    let window = continuous
        .checked_sub(origin)
        .ok_or(Error::Bound("continuous time order"))?
        / (60 * NANOSECONDS_PER_SECOND);
    if source.window == Some(window) {
        return Ok(());
    }

    source.window = Some(window);
    let identity = WindowIdentity {
        run: source.identity,
        window,
    };
    let known = history.is_some_and(|session| {
        session
            .windows
            .iter()
            .any(|manifest| manifest.identity == identity)
    });
    if !known {
        source.pending_windows.insert(window);
        if source.pending_windows.len() > PENDING_WINDOWS_MAX {
            let removed = source
                .pending_windows
                .pop_first()
                .ok_or(Error::Bound("pending window eviction"))?;
            assert!(removed <= window);
            // Selections before the tail refresh immediately; polling refreshes the catalog
            // every 256 polls after overflow, so an evicted delayed commit is not forgotten.
            source.pending_retention_limited = true;
        }
    }
    assert!(source.pending_windows.len() <= PENDING_WINDOWS_MAX);
    Ok(())
}

fn project_observation_stage_events(
    source: &mut Source,
    output: &mut ProjectionEvents,
) -> Result<()> {
    if source.tick.events.is_empty() {
        return Ok(());
    }

    source.reader.seek_event(source.tick.events.start)?;

    for _attempt in 0..=collector::tracker::EVENTS_PER_TICK_MAX {
        if source.reader.next_event() >= source.tick.events.end {
            return Ok(());
        }

        let mut identity = IdentityObservation::EMPTY;
        let found = if source.reader.dictionary_capacity().is_some() {
            source.reader.read_event_identity(&mut identity)?
        } else {
            source.reader.read_event()?
        };

        match found {
            FoundEvent::Nothing => {
                source.losses.unknown = true;

                return Ok(());
            }
            FoundEvent::Missed { first, end } => {
                source.losses.events += end.min(source.tick.events.end) - first;
            }
            FoundEvent::Event {
                number,
                tick,
                event,
            } => {
                output.events.push((number, tick, event));

                if source.reader.dictionary_capacity().is_some() {
                    output.identities.push(identity);
                }
            }
        }
    }

    Err(Error::Bound("live event count"))
}

fn table_bytes(table: &Table) -> usize {
    table.columns.capacity() * size_of::<String>()
        + table.columns.iter().map(String::capacity).sum::<usize>()
        + table.rows.capacity() * size_of::<Vec<Option<String>>>()
        + table
            .rows
            .iter()
            .map(|row| {
                row.capacity() * size_of::<Option<String>>()
                    + row
                        .iter()
                        .map(|value| value.as_ref().map_or(0, String::capacity))
                        .sum::<usize>()
            })
            .sum::<usize>()
}

fn same(left: &Value, right: &Value) -> bool {
    left["run"] == right["run"]
        && left["boot"] == right["boot"]
        && left["publication"] == right["publication"]
}

fn identity_order(value: &Value) -> (Option<u64>, Option<&str>, Option<&str>, Option<u64>) {
    (
        value["wall_ns"].as_u64(),
        value["run"].as_str(),
        value["boot"].as_str(),
        value["publication"].as_u64(),
    )
}

fn project_observation_tables(
    connection: &Connection,
    operation: &Operation,
    window: &Window,
) -> Result<BTreeMap<&'static str, Table>> {
    let mut tables = BTreeMap::new();
    for (table, _, order) in persistence::schema::TABLES {
        let name = table.name();

        if matches!(
            table,
            persistence::schema::Table::Processes | persistence::schema::Table::Events
        ) {
            let columns = persistence::table_columns(table, window);
            connection.execute_batch(&format!(
                "DROP TABLE IF EXISTS {name}; CREATE TABLE {name} ({columns})"
            ))?;
        } else {
            connection.execute_batch(&format!("DELETE FROM {name}"))?;
        }

        persistence::append(connection, table, window)?;
        let source = persistence::query::table_source(
            connection,
            table,
            persistence::query::TableSource::Staged,
        )?;

        tables.insert(
            name,
            data::query(
                connection,
                &format!("SELECT * FROM {source} ORDER BY {order}"),
                data::ROWS_MAX,
                operation,
            )?,
        );
    }

    Ok(tables)
}

fn select_segments(points: &mut [Value]) -> Result<()> {
    assert!(points.len() <= OBSERVATIONS_MAX);
    let mut segment = 0_u64;
    for index in 0..points.len() {
        let compatible = if let Some(previous) = index.checked_sub(1) {
            if points[previous]["run"] == points[index]["run"] {
                if points[previous]["boot"] == points[index]["boot"] {
                    metrics::compatible(
                        &point_observation(&points[previous])?,
                        &point_observation(&points[index])?,
                    )
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };
        if !compatible {
            segment += 1;
        }
        points[index]["segment"] = json!(segment);
    }
    Ok(())
}

fn point_observation(point: &Value) -> Result<Observation> {
    let required = |name| {
        point[name]
            .as_u64()
            .ok_or(Error::Bound("context observation field"))
    };
    Ok(Observation {
        window: 0,
        publication: required("publication")?,
        run_tick: required("run_tick")?,
        wall: required("wall_ns")?,
        continuous: point["continuous_ns"].as_u64(),
    })
}

#[cfg(test)]
#[path = "live_fixture.rs"]
mod identity_fixture;

#[cfg(test)]
mod tests {
    use super::*;
    use collector::recording::{Registration, private};
    use collector::ring::{Dropped, ObservationTimes, Place, Readings, Shape, Writer};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct RingFixture {
        root: PathBuf,
        writer: Writer,
    }

    impl RingFixture {
        fn new() -> Self {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("flightdeck-retry-{}-{serial}", std::process::id()));
            private::create_private_dir(&root).unwrap();
            let place = Place {
                lock_path: root.join("ring.lock"),
                name_prefix: format!("/fdretry{}x{serial}", std::process::id()),
            };
            let shape = Shape {
                slot_count: 2,
                cpu_capacity: 1,
                interface_capacity: 1,
                core_capacity: 1,
                state_capacity: 1,
                fan_capacity: 1,
                process_capacity: 1,
                coalition_capacity: 1,
                event_capacity: 8,
            };
            let (writer, registered) = Registration::start(&root, &place, shape, 0)
                .unwrap()
                .into_parts();
            registered.unwrap();
            Self { root, writer }
        }

        fn publish(&mut self, tick: u64) {
            let memory = collector::machine::Memory::default();
            let cpus = [collector::machine::CPUTicks {
                user: 1,
                system: 2,
                nice: 3,
                idle: 4,
            }];
            let events =
                std::array::from_fn::<_, 8, _>(|_| collector::tracker::Event::Exec { pid: 1 });
            self.writer.publish_observed(
                &Readings {
                    run_tick: tick,
                    cpus: &cpus,
                    memory: &memory,
                    interfaces: &[],
                    ioreport: None,
                    smc: None,
                    processes: &[],
                    processes_refused: 0,
                    processes_gone: 0,
                    coalitions: &[],
                    events: &events,
                    dropped: Dropped::default(),
                },
                &ObservationTimes {
                    wall_ns: (1000 + tick) * NANOSECONDS_PER_SECOND,
                    continuous_ns: tick * NANOSECONDS_PER_SECOND,
                    awake_ns: tick * NANOSECONDS_PER_SECOND,
                },
            );
        }
    }

    impl Drop for RingFixture {
        fn drop(&mut self) {
            collector::ring::unlink_generation(self.writer.name()).unwrap();
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn retries_a_canceled_projection_without_losing_the_tick_or_counting_losses_twice() {
        // Goal: supersession retries an advanced cursor and preserves genuine source loss.
        // Method: cancel at the cursor/projection barrier after overwriting ticks and events.
        let mut fixture = RingFixture::new();
        let control = Arc::new(Cancellation::default());
        let mut session = LiveSession::open_with(&fixture.root, control.clone(), 0).unwrap();
        for tick in 1..=4 {
            fixture.publish(tick);
        }
        let cancel = control.clone();
        session.before_projection = Some(Box::new(move || cancel.cancel().unwrap()));

        assert!(session.poll().is_err());
        session.begin(control.epoch()).unwrap();
        assert!(session.poll().unwrap());
        let selection = session.select(1003, 0).unwrap();

        assert_eq!(selection["selected"]["publication"], 3);
        assert_eq!(selection["selected"]["reader_losses"]["publications"], 2);
        assert_eq!(selection["selected"]["reader_losses"]["events"], 8);
        assert_eq!(
            session.select(1004, 0).unwrap()["selected"]["publication"],
            4
        );
    }
    #[test]
    fn rehydration_keeps_poll_cursors_and_source_losses_separate() {
        // Goal: inspecting an evicted tick cannot advance the source or charge source losses.
        // Method: expire its events, evict its raw rows, and re-read its still-retained tick.
        let mut fixture = RingFixture::new();
        fixture.publish(1);
        let mut session = LiveSession::open(&fixture.root).unwrap();
        session.observations.clear();
        session.cache_bytes = 0;
        fixture.publish(2);
        assert!(session.poll().unwrap());
        let cursor = session.sources[0].reader.next_publication();
        let before_losses = session.catalog()["reader_losses"].clone();
        let selection = session.select(1001, 0).unwrap();
        assert_eq!(selection["selected"]["publication"], 1);
        assert_eq!(selection["selected"]["reader_losses"]["events"], 8);
        assert_eq!(
            session.page("events", 0).unwrap()["observation"],
            selection["selected"]
        );
        assert_eq!(session.sources[0].reader.next_publication(), cursor);
        assert_eq!(session.catalog()["reader_losses"], before_losses);
        assert_eq!(session.catalog()["latest_observation"]["publication"], 2);
        assert!(!session.poll().unwrap());
        assert!(session.cache_bytes <= CACHE_BYTES_MAX);
    }

    #[test]
    fn following_requires_the_latest_raw_rows_instead_of_rehydrating_or_falling_back() {
        // Goal: retained scalar points cannot silently substitute for missing latest raw rows.
        // Method: remove the test's raw cache while keeping its real point and retained ring tick.
        let mut fixture = RingFixture::new();
        fixture.publish(1);
        let mut session = LiveSession::open(&fixture.root).unwrap();
        session.select(1001, 0).unwrap();
        session.observations.clear();
        session.cache_bytes = 0;
        let cursor = session.sources[0].reader.next_publication();

        let error = session.follow_latest().unwrap_err();

        assert!(
            error
                .to_string()
                .contains("latest live raw observation unavailable")
        );
        assert!(session.selected.is_none());
        assert!(session.page("memory", 0).is_err());
        assert_eq!(cursor, session.sources[0].reader.next_publication());
        assert_eq!(session.catalog()["latest_observation"]["publication"], 1);
    }

    #[test]
    fn following_releases_inspection_bytes_without_changing_poll_state() {
        // Goal: following and table paging share latest identity with consistent cache accounting.
        // Method: retain an older inspected observation, follow, and compare cursor/loss state.
        let mut fixture = RingFixture::new();
        fixture.publish(1);
        fixture.publish(2);
        let mut session = LiveSession::open(&fixture.root).unwrap();
        session.select(1001, 0).unwrap();
        let inspected = session.observations.pop_front().unwrap();
        let expected_bytes = session.cache_bytes - inspected.bytes;
        session.inspected = Some(inspected);
        let cursor = session.sources[0].reader.next_publication();
        let losses = session.catalog()["reader_losses"].clone();

        let response = session.follow_latest().unwrap();

        assert_eq!(response["selected"]["publication"], 2);
        assert!(session.inspected.is_none());
        assert_eq!(expected_bytes, session.cache_bytes);
        assert_eq!(
            session.page("memory", 0).unwrap()["observation"],
            response["selected"]
        );
        assert_eq!(cursor, session.sources[0].reader.next_publication());
        assert_eq!(session.catalog()["reader_losses"], losses);
        assert!(!session.poll().unwrap());
    }

    #[test]
    fn rotates_pending_markers_with_a_fixed_budget_without_starvation() {
        // Goal: a stopped persister cannot multiply per-poll filesystem work.
        // Method: visit 4,096 pending keys in exactly 256 distinct batches of 16.
        let fixture = RingFixture::new();
        let mut session = LiveSession::open(&fixture.root).unwrap();
        let source = &mut session.sources[0];
        source.pending_windows = (0..u64::try_from(PENDING_WINDOWS_MAX).unwrap()).collect();
        let mut visited = BTreeSet::new();
        for _poll in 0..HISTORY_REFRESH_POLLS_MAX {
            let candidates = poll_commit_candidates(source);
            assert_eq!(candidates.len(), WINDOW_MARKERS_PER_POLL_MAX);
            for window in candidates {
                assert!(visited.insert(window));
            }
        }
        assert_eq!(visited, source.pending_windows);
        assert_eq!(poll_commit_candidates(source)[0], 0);
    }

    #[test]
    fn overflows_pending_retention_without_stopping_and_refreshes_old_history() {
        // Goal: a long persister outage neither fails following nor forgets old commits.
        // Method: overflow the fixed set and verify selection and bounded poll refreshes.
        let mut fixture = RingFixture::new();
        fixture.publish(1);
        let mut session = LiveSession::open(&fixture.root).unwrap();
        let source = &session.sources[0];
        let identity = WindowIdentity {
            run: source.identity,
            window: 0,
        };
        let mut window = Window::new(
            identity,
            Limits::from_shape(&source.reader.shape()).unwrap(),
        );
        window
            .push_tick(
                &persistence::Observation {
                    publication: 1,
                    wall_ns: Some(1001 * NANOSECONDS_PER_SECOND),
                    continuous_ns: Some(NANOSECONDS_PER_SECOND),
                    awake_ns: Some(NANOSECONDS_PER_SECOND),
                },
                &source.tick,
                &[],
            )
            .unwrap();
        session.sources[0].pending_windows =
            (0..u64::try_from(PENDING_WINDOWS_MAX).unwrap()).collect();
        fixture.publish(u64::try_from(PENDING_WINDOWS_MAX).unwrap() * 60);
        assert!(session.poll().unwrap());
        assert_eq!(
            session.sources[0].pending_windows.len(),
            PENDING_WINDOWS_MAX
        );
        assert!(!session.sources[0].pending_windows.contains(&0));
        assert!(session.select_refresh_due(0));
        assert_eq!(
            session.catalog()["commit_discovery"][0]["history_refresh_polls_max"],
            256
        );
        let source = &mut session.sources[0];
        source.pending_refresh_polls = 0;
        for _poll in 1..HISTORY_REFRESH_POLLS_MAX {
            assert!(!poll_commit_history_due(source));
        }
        assert!(poll_commit_history_due(source));
        assert!(!poll_commit_history_due(source));
        seal_window(&fixture.root, &window);
        session.sources[0].pending_refresh_polls = HISTORY_REFRESH_POLLS_MAX - 1;
        assert_eq!(session.catalog()["windows"], 0);
        assert!(!session.poll().unwrap());
        assert_eq!(session.catalog()["windows"], 1);
        session.sources[0].pending_windows.insert(0);
        let history = session.history.as_ref().unwrap();
        refresh_history_pending(&mut session.sources, history);
        assert!(!session.sources[0].pending_windows.contains(&0));
    }

    fn seal_window(root: &Path, window: &Window) {
        let store = persistence::Store::open(root).unwrap();
        let persistence::Begin::Pending(pending) = store.begin(&window.identity).unwrap() else {
            panic!("fixture window already committed");
        };
        let tables = persistence::Encoder.encode(window, pending.path()).unwrap();
        let publications = window.publication_range();
        let events = window.event_range();
        store
            .commit(
                pending,
                persistence::Manifest {
                    identity: window.identity,
                    publication_first: publications.start,
                    publication_end: publications.end,
                    events_first: events.start,
                    events_end: events.end,
                    wall_ns: None,
                    continuous_ns: None,
                    awake_ns: None,
                    losses: Losses::default(),
                    data_directory: String::new(),
                    tables,
                },
            )
            .unwrap();
    }
    #[test]
    fn retries_captured_identity_after_cancellation_and_hands_the_same_pages_to_history() {
        // Goal: canceled v4 decoding cannot lose a publication, event identity or seal handoff.
        // Method: cancel after the reader advances, retry, then compare live and sealed rows.
        let mut fixture = identity_fixture::Fixture::new();
        let control = Arc::new(Cancellation::default());
        let mut session = LiveSession::open_with(&fixture.root, control.clone(), 0).unwrap();
        fixture.publish(1000);
        let cancel = control.clone();
        session.before_projection = Some(Box::new(move || cancel.cancel().unwrap()));

        assert!(session.poll().is_err());
        session.begin(control.epoch()).unwrap();
        assert!(session.poll().unwrap());
        session.select(1000, 0).unwrap();
        let processes = session.page("processes", 0).unwrap();
        let events = session.page("events", 0).unwrap();
        fixture.seal();
        session.poll().unwrap();

        assert_eq!(processes["rows"][0][26], r"refused\xff");
        assert_eq!(events["rows"][0][25], "700");
        assert_eq!(events["rows"][0][27], r"departed\xfd");
        assert_eq!(session.sources[0].reader.next_publication(), 2);
        assert_eq!(session.sources[0].losses.publications, 0);
        assert_eq!(session.sources[0].losses.events, 0);
        let mut saved = Session::open(&fixture.root).unwrap();
        saved.select(1000, 0).unwrap();
        assert_eq!(
            saved.page("processes", 0).unwrap()["rows"],
            processes["rows"]
        );
        assert_eq!(saved.page("events", 0).unwrap()["rows"], events["rows"]);
    }

    #[test]
    fn retries_identity_projection_after_interrupted_table_replacement() {
        // Goal: a retry recreates tables dropped before a canceled schema replacement.
        // Method: reproduce that intermediate database state before polling a real v4 tick.
        let mut fixture = identity_fixture::Fixture::new();
        let mut session = LiveSession::open(&fixture.root).unwrap();
        session
            .connection
            .execute_batch("DROP TABLE processes; DROP TABLE events")
            .unwrap();
        fixture.publish(1000);

        assert!(session.poll().unwrap());
        session.select(1000, 0).unwrap();

        assert_eq!(
            session.page("processes", 0).unwrap()["rows"][0][26],
            r"refused\xff"
        );
        assert_eq!(session.page("events", 0).unwrap()["rows"][0][25], "700");
    }

    #[test]
    fn bounds_repeated_identity_projection_and_records_its_small_fixture_cost() {
        // Goal: repeated identity projection keeps bounded cache and exact latest metadata.
        // Method: project 100 two-process/one-event ticks and observe this test process.
        let mut fixture = identity_fixture::Fixture::new();
        let mut session = LiveSession::open(&fixture.root).unwrap();
        let timebase = collector::clock::Timebase::read().unwrap();
        let pid = i32::try_from(std::process::id()).unwrap();
        let before = collector::processes::read_usage(timebase, pid)
            .unwrap()
            .unwrap();
        let started = std::time::Instant::now();

        for second in 1000..1100 {
            fixture.publish(second);
            assert!(session.poll().unwrap());
        }
        let elapsed_ns = started.elapsed().as_nanos();
        let after = collector::processes::read_usage(timebase, pid)
            .unwrap()
            .unwrap();
        let footprint = persistence::resources::Footprint::read().unwrap();
        session.select(1099, 0).unwrap();

        assert_eq!(session.catalog()["tail_ticks"], 100);
        assert!(session.cache_bytes <= CACHE_BYTES_MAX);
        assert_eq!(
            session.page("processes", 0).unwrap()["rows"][0][26],
            r"refused\xff"
        );
        assert_eq!(session.page("events", 0).unwrap()["rows"][0][25], "700");
        println!(
            "{}",
            json!({"fixture_publications":100,"process_rows_per_tick":2,
            "event_rows_per_tick":1,"elapsed_ns":elapsed_ns,
            "process_cpu_ns":after.user_ns+after.system_ns-before.user_ns-before.system_ns,
            "physical_footprint_bytes":footprint.current_bytes,
            "process_lifetime_footprint_max_bytes":footprint.lifetime_bytes_max,
            "retained_table_bytes":session.cache_bytes,
            "source_identity_buffer_bytes":session.sources[0].identities.capacity()
                *size_of::<IdentityObservation>()})
        );
    }
}
