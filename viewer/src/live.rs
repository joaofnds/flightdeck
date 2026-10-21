//! Viewer-only bounded projection of registered live rings alongside committed history.
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use collector::recording::{Registry, identity_text};
use collector::ring::{Found, FoundEvent, IdentityObservation, RingReader, RunIdentity, Tick};
use persistence::engine::Engine;
use persistence::{Error, Limits, Losses, NumberedEvent, Result, Window, WindowIdentity};
use serde_json::{Value, json};

use crate::cancellation::Operation;
use crate::data::{self, Table};
use crate::live_app_history::{GroupTarget, History, Selection, Target};
use crate::live_app_response::{Detail, KeyKind, Page, Sample};
use crate::live_apps::{Frame, Publication, PublicationEvents};
use crate::session::{
    CHARTED_SECONDS, ContextRange, ContextSelection, DETAIL_SECONDS, EARLIER_STRIDE_SECONDS,
    LEAD_IN_SECONDS, LOADED_SECONDS, OBSERVATIONS_MAX, Observation, PAGE_ROWS, Tail, Verified,
};
use crate::{Cancellation, Session, metrics};

#[path = "app_resident_history.rs"]
pub(crate) mod resident_history;

#[path = "app_attribution_history.rs"]
pub(crate) mod attribution_history;

#[path = "live_app_parts.rs"]
pub(crate) mod app_parts;
#[path = "live_app_ranks.rs"]
pub(crate) mod app_ranks;
#[path = "live_storage_merge.rs"]
mod storage_merge;
#[path = "live_window.rs"]
pub(crate) mod window;

#[path = "live_process_groups.rs"]
mod process_groups;

const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;
const CACHE_BYTES_MAX: usize = 128 * 1024 * 1024;
const SOURCES_MAX: usize = 2;
const CONTEXT_BYTES_MAX: usize = 8 * 1024 * 1024;
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
    history_error: Option<String>,
    restored: Option<ScalarContext>,
    earlier: Option<EarlierCache>,
    sources: Vec<Source>,
    observations: VecDeque<LiveObservation>,
    points: VecDeque<Value>,
    app_history: History,
    connection: Engine,
    operation: Operation,
    cancellation: Arc<Cancellation>,
    selected: Option<Value>,
    selected_context: Vec<crate::cpu_history_sample::Identity>,
    /// The last second of the context the last selection read, which its histories span.
    context_last_second: Option<u64>,
    cpu_history: Option<crate::cpu_history::History>,
    inspected: Option<LiveObservation>,
    cache_bytes: usize,
    app_ranks: Arc<app_ranks::WindowRanks>,
    parts_app: Option<app_parts::PartsApp>,
    app_parts: Arc<app_parts::WindowParts>,
    /// Whether the selection follows the latest live second, so a poll ranks it and keeps its
    /// part. A selected earlier second keeps its window's, which a poll would drop.
    follows_live: bool,
    #[cfg(test)]
    before_projection: Option<Box<dyn FnOnce()>>,
    #[cfg(test)]
    before_restore: Option<Box<dyn FnOnce()>>,
}

struct ScalarContext {
    range: ContextRange,
    /// The end of the sealed history the points were read from. A minute sealed later begins
    /// at or after it, so a later follow reads only the history past it.
    through_ns: u64,
    points: Vec<Value>,
}

/// The older tail, read once for every stretch of seconds the full-detail context moves through.
struct EarlierCache {
    limit_second: u64,
    windows: usize,
    tail: Tail,
}

type PointIndex<'a> = BTreeMap<(Option<&'a str>, Option<&'a str>, Option<u64>), usize>;

struct PointMerge<'a> {
    live: &'a mut Value,
    committed: &'a Value,
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
    app_frame: Box<Frame>,
    app_sample: Option<Box<Sample>>,
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
        let connection = data::connection();
        operation.attach(&connection)?;
        let mut session = Self {
            root: root.to_owned(),
            history: None,
            history_error: None,
            restored: None,
            earlier: None,
            sources: Vec::with_capacity(SOURCES_MAX),
            observations: VecDeque::new(),
            points: VecDeque::new(),
            app_history: History::new(),
            connection,
            operation,
            cancellation,
            selected: None,
            selected_context: Vec::new(),
            context_last_second: None,
            cpu_history: None,
            inspected: None,
            cache_bytes: 0,
            app_ranks: Arc::default(),
            parts_app: None,
            app_parts: Arc::default(),
            follows_live: true,
            #[cfg(test)]
            before_projection: None,
            #[cfg(test)]
            before_restore: None,
        };
        crate::cpu_history::History::init(&mut session.cpu_history);
        if let Err(error) = session.refresh_history() {
            session.history_failure(error)?;
        }

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

        if let Err(error) = self.poll_commits() {
            self.history_failure(error)?;
        }

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
        catalog["history_error"] = json!(self.history_error);
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
        self.follows_live = false;
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
        let selection = ContextSelection::of(second, self.newest_second());
        let saved = match &mut self.history {
            Some(history) => {
                history.begin(self.operation.epoch())?;
                Some(history.select_context(selection, 0))
            }
            None => None,
        };
        let mut response = match saved {
            Some(Ok(response)) => response,
            // A saved minute that fails its read leaves the readings the ring holds, as an
            // unreadable history does at open.
            Some(Err(error)) => {
                self.history_failure(error)?;
                select_unsaved(selection)
            }
            None => select_unsaved(selection),
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
        self.select_trends(selection, &mut response)?;
        self.select_observation(selection, selected, &mut response)?;
        if let Some(selected) = &self.selected {
            response["summary"] = response["trends"]
                .as_array()
                .ok_or(Error::Bound("trends shape"))?
                .iter()
                .find(|point| same(point, selected))
                .ok_or(Error::Bound("selected context point"))?
                .clone();
        }
        let mut context = Vec::new();
        crate::cpu_history::context(
            response["trends"]
                .as_array()
                .ok_or(Error::Bound("CPU selected context"))?,
            &mut context,
        )?;
        self.selected_context = context;
        self.context_last_second = Some(selection.context_last_second());

        Ok(response)
    }

    /// Selects the observation `identity` names at `second`, for a read of that second alone, as
    /// a hover's is. It builds no context, which costs `select` hundreds of milliseconds on a
    /// full window, and leaves the context the last `select` read, so only reads of the selected
    /// second may follow it, never a history. The summary carries no segment, which only a
    /// context numbers.
    pub fn inspect(&mut self, second: u64, identity: &Value) -> Result<Value> {
        self.selected = None;
        self.follows_live = false;
        if let Some(inspected) = self.inspected.take() {
            self.cache_bytes -= inspected.bytes;
        }
        self.operation.check()?;

        let response = self.inspect_selected(second, identity)?;
        self.selected = Some(response["selected"].clone());
        Ok(response)
    }

    /// The observation `identity` names at `second`, with its summary, from the newest live
    /// readings, then the ring, then the saved minutes, which are opened again once where the
    /// session's copy of them is older than the observation.
    fn inspect_selected(&mut self, second: u64, identity: &Value) -> Result<Value> {
        if let Some(response) = self.inspect_live(second, identity)? {
            return Ok(response);
        }

        if let Some(response) = self.inspect_ring(second, identity)? {
            return Ok(response);
        }

        if let Some(response) = self.inspect_history(second, identity)? {
            return Ok(response);
        }
        self.refresh_history()?;
        self.inspect_history(second, identity)?
            .ok_or(Error::Bound("inspected observation"))
    }

    /// The observation `identity` names at `second`, with its summary, read again from the ring
    /// that still holds it. A hover names an observation by its run, boot and publication
    /// alone, so the ring is read with the observation its point keeps, which holds the losses
    /// the reader had met when it read it.
    fn inspect_ring(&mut self, second: u64, identity: &Value) -> Result<Option<Value>> {
        let Some(point) = self.points.iter().find(|point| same(point, identity)) else {
            return Ok(None);
        };
        let Some(inspected) = self.select_rehydrate(&point["observation"])? else {
            return Ok(None);
        };

        let response = json!({"second": second, "selected": inspected.json(),
            "summary": inspect_summary(&inspected.point)?});
        if response["selected"]["wall_ns"]
            .as_u64()
            .map(|wall| wall / NANOSECONDS_PER_SECOND)
            != Some(second)
        {
            return Err(Error::Bound("inspected observation second"));
        }
        self.cache_bytes += inspected.bytes;
        self.inspected = Some(inspected);
        self.poll_evict(0)?;
        Ok(Some(response))
    }

    /// The live observation `identity` names at `second`, with its summary. The first
    /// observation of its run the session holds was read with no predecessor, so its interval
    /// readings come from the saved minute that keeps it, as a selection's do.
    fn inspect_live(&mut self, second: u64, identity: &Value) -> Result<Option<Value>> {
        let Some(index) = self
            .observations
            .iter()
            .enumerate()
            .filter(|(_, observation)| {
                observation.observation.wall / NANOSECONDS_PER_SECOND == second
            })
            .find(|(_, observation)| same(&observation.json(), identity))
            .map(|(index, _)| index)
        else {
            return Ok(None);
        };

        let observation = &self.observations[index];
        let selected = observation.json();
        let mut summary = inspect_summary(&observation.point)?;
        let predecessor_held = self
            .observations
            .iter()
            .take(index)
            .any(|earlier| earlier.identity == observation.identity);

        if !predecessor_held {
            if let Some(saved) = self.inspect_history(second, identity)? {
                follow_merge_point(PointMerge {
                    live: &mut summary,
                    committed: &saved["summary"],
                })?;
            }
        }
        Ok(Some(
            json!({"second": second, "selected": selected, "summary": summary}),
        ))
    }

    fn inspect_history(&mut self, second: u64, identity: &Value) -> Result<Option<Value>> {
        let Some(history) = &mut self.history else {
            return Ok(None);
        };

        history.begin(self.operation.epoch())?;
        history.inspect(second, |candidate| same(candidate, identity))
    }

    /// The saved readings older than the full-detail context that ends at
    /// `context_last_second`, back to the lead-in before the loaded hour, one in every stretch of
    /// seconds. Read again only when the context moves into another stretch or a minute is
    /// sealed, so following the newest second costs one read in every `EARLIER_STRIDE_SECONDS`.
    pub fn earlier_context(&mut self, context_last_second: u64) -> Result<Value> {
        let limit_second = earlier_limit_second(context_last_second);
        if !self.earlier_current(limit_second) {
            let tail = match &mut self.history {
                Some(history) => {
                    history.begin(self.operation.epoch())?;
                    history.earlier_tail(
                        limit_second
                            .saturating_sub(LOADED_SECONDS + LEAD_IN_SECONDS - CHARTED_SECONDS),
                        limit_second,
                    )?
                }
                None => Tail { points: Vec::new() },
            };
            self.earlier = Some(EarlierCache {
                limit_second,
                windows: self.earlier_windows(),
                tail,
            });
        }

        self.earlier_context_kept(context_last_second)?
            .ok_or(Error::Bound("older tail"))
    }

    /// The older tail `earlier_context` gives for `context_last_second`, when the tail it read
    /// last still answers it, and `None` when answering would read saved history.
    pub fn earlier_context_kept(&self, context_last_second: u64) -> Result<Option<Value>> {
        if !self.earlier_current(earlier_limit_second(context_last_second)) {
            return Ok(None);
        }

        let first_second = context_last_second.saturating_sub(LOADED_SECONDS + LEAD_IN_SECONDS - 1);
        let cache = self.earlier.as_ref().ok_or(Error::Bound("older tail"))?;
        let points: Vec<&Value> = cache
            .tail
            .points
            .iter()
            .filter(|point| {
                point["wall_ns"]
                    .as_u64()
                    .is_some_and(|wall| wall / NANOSECONDS_PER_SECOND >= first_second)
            })
            .collect();
        context_bytes_of(&points)?;

        Ok(Some(json!({"earlier_trends": points,
            "earlier_stride_seconds": EARLIER_STRIDE_SECONDS})))
    }

    /// Whether the older tail read last ends at `limit_second` and saw every sealed minute.
    fn earlier_current(&self, limit_second: u64) -> bool {
        let windows = self.earlier_windows();
        self.earlier
            .as_ref()
            .is_some_and(|cache| cache.limit_second == limit_second && cache.windows == windows)
    }

    fn earlier_windows(&self) -> usize {
        self.history
            .as_ref()
            .map_or(0, |history| history.windows.len())
    }

    /// Restores committed context once, then follows live points without repeating its queries.
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
        let range = ContextRange::charted_ending_at(second)?;

        if let Err(error) = self.follow_restore(range) {
            self.history_failure(error)?;
        }

        let mut trends = Vec::new();
        self.follow_trends(range, &mut trends)?;
        let summary = trends
            .iter()
            .find(|point| same(point, &selected))
            .ok_or(Error::Bound("selected context point"))?
            .clone();

        let mut alternatives = self
            .points
            .iter()
            .filter(|point| {
                point["wall_ns"]
                    .as_u64()
                    .map(|wall| wall / NANOSECONDS_PER_SECOND)
                    == Some(second)
            })
            .map(|point| point["observation"].clone())
            .collect::<Vec<_>>();
        alternatives.sort_by(|left, right| identity_order(left).cmp(&identity_order(right)));
        let mut context = Vec::new();
        let held = ContextRange::ending_at(second)?.held_points(&trends);
        crate::cpu_history::context(held, &mut context)?;
        self.operation.check()?;

        let response = json!({"second": second, "context_last_second": second,
            "selected": selected, "alternatives": alternatives, "summary": summary,
            "trends": trends, "context_seconds": 900, "context_error": self.history_error});
        self.selected = Some(selected);
        self.selected_context = context;
        self.context_last_second = Some(second);
        self.follows_live = true;

        Ok(response)
    }

    fn follow_restore(&mut self, range: ContextRange) -> Result<()> {
        let Some(history) = &self.history else {
            return Ok(());
        };

        let history_end = history.catalog()["last_second"]
            .as_u64()
            .map(|second| {
                second
                    .checked_add(1)
                    .and_then(|value| value.checked_mul(NANOSECONDS_PER_SECOND))
                    .ok_or(Error::Bound("history context end"))
            })
            .transpose()?;

        let mut kept = Vec::new();
        let mut first_ns = range.first_ns;
        if self.history_error.is_none() {
            if let Some(restored) = &self.restored {
                if range.first_ns >= restored.range.first_ns {
                    if history_end.is_none_or(|end| range.end_ns.min(end) <= restored.through_ns) {
                        return Ok(());
                    }

                    first_ns = restored.through_ns.max(range.first_ns);
                    follow_restore_kept(&restored.points, range.first_ns, first_ns, &mut kept)?;
                }
            }
        }
        assert!(first_ns < range.end_ns);

        let mut points = Vec::new();
        history.context(
            ContextRange {
                first_ns,
                end_ns: range.end_ns,
            },
            &mut points,
        )?;
        kept.append(&mut points);
        context_bytes(&kept)?;
        #[cfg(test)]
        if let Some(barrier) = self.before_restore.take() {
            barrier();
        }

        self.operation.check()?;
        let through_ns = history_end.map_or(range.first_ns, |end| end.min(range.end_ns));
        self.restored = Some(ScalarContext {
            range,
            through_ns,
            points: kept,
        });
        self.history_error = None;

        Ok(())
    }

    fn follow_trends(&self, range: ContextRange, output: &mut Vec<Value>) -> Result<()> {
        assert!(output.is_empty());

        let mut positions = PointIndex::new();
        self.follow_restored_points(range, output, &mut positions)?;
        for point in &self.points {
            self.operation.check()?;
            if !context_contains(range, point)? {
                continue;
            }

            let mut live = metrics::public_point(point)?;
            let key = (
                point["run"].as_str(),
                point["boot"].as_str(),
                point["publication"].as_u64(),
            );
            if let Some(index) = positions.get(&key) {
                follow_merge_point(PointMerge {
                    live: &mut live,
                    committed: &output[*index],
                })?;
                output[*index] = live;
            } else {
                if output.len() == OBSERVATIONS_MAX {
                    return Err(Error::Bound("context observations"));
                }

                positions.insert(key, output.len());
                output.push(live);
            }
        }

        output.sort_by(|left, right| identity_order(left).cmp(&identity_order(right)));
        metrics::context_segments(output)?;

        context_bytes(output)
    }

    fn follow_restored_points<'a>(
        &'a self,
        range: ContextRange,
        output: &mut Vec<Value>,
        positions: &mut PointIndex<'a>,
    ) -> Result<()> {
        let Some(restored) = &self.restored else {
            return Ok(());
        };

        for point in &restored.points {
            if context_contains(range, point)? {
                let key = (
                    point["run"].as_str(),
                    point["boot"].as_str(),
                    point["publication"].as_u64(),
                );
                if positions.insert(key, output.len()).is_some() {
                    return Err(Error::Bound("duplicate committed context point"));
                }

                output.push(point.clone());
            }
        }

        Ok(())
    }

    pub fn cpu_detail(&self) -> Result<Value> {
        self.operation.run(|| self.cpu_detail_inner())
    }

    pub fn cpu_history(&mut self, offset: u32) -> Result<Value> {
        let operation = self.operation.begin(self.operation.epoch())?;
        operation.run(|| self.cpu_history_inner(offset))
    }

    fn cpu_history_inner(&mut self, offset: u32) -> Result<Value> {
        let selected = self
            .selected
            .as_ref()
            .ok_or(Error::Bound("no selected observation"))?
            .clone();
        let context_last_second = self
            .context_last_second
            .ok_or(Error::Bound("no selected context"))?;
        let range = ContextRange::ending_at(context_last_second)?;
        let mut restoration_error = None;
        if let Some(history) = &mut self.history {
            if let Err(error) = history.cpu_history_restore(range) {
                self.operation.check()?;
                if let Error::Io(problem) = &error {
                    if problem.kind() == std::io::ErrorKind::Interrupted {
                        return Err(error);
                    }
                }
                restoration_error = Some(error.to_string().chars().take(512).collect::<String>());
            }
        }

        let mut response = crate::cpu_history::project(&crate::cpu_history::Request {
            selected: &selected,
            context_last_second,
            context: &self.selected_context,
            live: self.cpu_history.as_ref(),
            saved: self
                .history
                .as_ref()
                .and_then(|history| history.cpu_history_cache.as_ref()),
            offset,
        })?;
        if let Some(message) = restoration_error {
            response["restoration_error"] = json!(message);
            crate::cpu_history::project_bytes(&response)?;
        }

        Ok(response)
    }

    fn cpu_detail_inner(&self) -> Result<Value> {
        let selected = self
            .selected
            .as_ref()
            .ok_or(Error::Bound("no selected observation"))?;
        let mut current_slot = None;
        let mut previous_slot = None;
        self.cpu_detail_cached(selected, &mut current_slot)?;
        if current_slot.is_none() {
            let history = self
                .history
                .as_ref()
                .ok_or(Error::Bound("missing historical selection"))?;
            let publication = selected["publication"]
                .as_u64()
                .ok_or(Error::Bound("selected publication"))?;
            history.cpu_detail_publication(selected, publication, &mut current_slot)?;
        }

        let current = current_slot
            .as_ref()
            .ok_or(Error::Bound("missing selected CPU reading"))?;
        if current.identity["run_tick"] != selected["run_tick"] {
            return Err(Error::Bound("changed CPU selection tick"));
        }

        if current.identity["wall_ns"] != selected["wall_ns"] {
            return Err(Error::Bound("changed CPU selection wall"));
        }

        if let Some(publication) = current.observation.publication.checked_sub(1) {
            let mut identity = current.identity.clone();
            identity["publication"] = json!(publication);
            self.cpu_detail_cached(&identity, &mut previous_slot)?;
            if previous_slot.is_none() {
                if let Some(history) = &self.history {
                    history.cpu_detail_publication(&identity, publication, &mut previous_slot)?;
                }
            }
        }

        crate::cpu_detail::project(current, previous_slot.as_ref())
    }

    fn cpu_detail_cached(
        &self,
        identity: &Value,
        output: &mut Option<crate::cpu_detail::Sample>,
    ) -> Result<()> {
        assert!(output.is_none());

        let Some(observation) = self
            .inspected
            .iter()
            .chain(self.observations.iter())
            .find(|observation| same(&observation.json(), identity))
        else {
            return Ok(());
        };

        let cpus = observation
            .tables
            .get("cpus")
            .ok_or(Error::Bound("missing CPU table"))?;
        if cpus.rows.len() > crate::cpu_detail::CPUS_MAX {
            return Err(Error::Bound("logical CPU count"));
        }

        *output = Some(crate::cpu_detail::Sample {
            identity: observation.json(),
            observation: observation.observation.clone(),
            cpus: cpus.clone(),
        });

        Ok(())
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

    pub fn apps_page(&self, offset: u32) -> Result<Value> {
        self.operation.run(|| {
            self.with_selected_apps(|sample| {
                sample.apps_page(Page {
                    offset,
                    limit: crate::live_app_response::PAGE_ROWS_MAX,
                })
            })
        })
    }

    pub fn app_history(&self, kind: KeyKind, bytes: &[u8]) -> Result<Value> {
        self.app_history_selected(&Target { kind, bytes })
    }

    /// History for one captured executable-name byte sequence within an exact App identity.
    pub fn app_process_history(
        &self,
        kind: KeyKind,
        app_bytes: &[u8],
        executable_name_bytes: &[u8],
    ) -> Result<Value> {
        let target = GroupTarget {
            app: Target {
                kind,
                bytes: app_bytes,
            },
            executable: executable_name_bytes,
        };
        target.validate()?;

        let mut response = self.app_history_selected(&target)?;
        target.identify(&mut response)?;
        self.app_history.process_coverage(&mut response);
        let response = crate::live_app_response::bounded(response)?;
        self.operation.check()?;

        Ok(response)
    }

    fn app_history_selected(&self, target: &dyn Selection) -> Result<Value> {
        self.operation.run(|| {
            let selected = self.selected_in_context()?;
            if let Some(history) = &self.history {
                let mut restored = false;
                let result = history.with_app_history(&selected, target, |saved| {
                    restored = true;
                    self.app_history
                        .response_merged(&crate::live_app_history::Merge {
                            selected: &selected,
                            target,
                            saved,
                            context: &self.selected_context,
                            operation: &self.operation,
                        })
                });
                match result {
                    Err(error) if !restored => {
                        if target.executable().is_some() {
                            Err(error)
                        } else {
                            self.app_history_degraded(&selected, target, error)
                        }
                    }
                    result => result,
                }
            } else {
                if target.executable().is_some() {
                    self.app_history
                        .response_merged(&crate::live_app_history::Merge {
                            selected: &selected,
                            target,
                            saved: &[],
                            context: &self.selected_context,
                            operation: &self.operation,
                        })
                } else {
                    self.app_history
                        .response(&selected, target, &self.operation)
                }
            }
        })
    }

    fn app_history_degraded(
        &self,
        selected: &crate::live_app_response::Selected,
        target: &dyn Selection,
        error: Error,
    ) -> Result<Value> {
        self.operation.check()?;
        match &error {
            Error::Bound(_) => return Err(error),
            Error::Io(problem) if problem.kind() == std::io::ErrorKind::Interrupted => {
                return Err(error);
            }
            Error::Io(_) | Error::Engine(_) => {}
        }
        let mut response = self
            .app_history
            .response_merged(&crate::live_app_history::Merge {
                selected,
                target,
                saved: &[],
                context: &self.selected_context,
                operation: &self.operation,
            })?;
        response["restoration_error"] =
            json!(error.to_string().chars().take(512).collect::<String>());
        let response = crate::live_app_response::bounded(response)?;
        self.operation.check()?;

        Ok(response)
    }

    pub fn app_detail(&self, kind: KeyKind, bytes: &[u8], offset: u32) -> Result<Value> {
        self.operation.run(|| {
            self.with_selected_apps(|sample| {
                let index = sample.apps().iter().position(|app| {
                    if app.key.kind == kind {
                        app.key.bytes == bytes
                    } else {
                        false
                    }
                });
                let Some(index) = index else {
                    return Ok(Value::Null);
                };

                sample.app_detail(&Detail {
                    selected: &sample.selected,
                    app_index: u32::try_from(index).map_err(|_| Error::Bound("live App index"))?,
                    page: Page {
                        offset,
                        limit: crate::live_app_response::PAGE_ROWS_MAX,
                    },
                })
            })
        })
    }

    /// The selected observation, with the last second of the context the selection read, which
    /// its histories span.
    fn selected_in_context(&self) -> Result<crate::live_app_response::Selected> {
        let mut observation = None;
        self.with_selected_apps(|sample| {
            observation = Some(sample.selected);
            Ok(Value::Null)
        })?;
        let observation = observation.ok_or(Error::Bound("history selection"))?;
        let context_last_second = self
            .context_last_second
            .ok_or(Error::Bound("history selection context"))?;

        crate::live_app_response::Selected::new(observation, context_last_second)
    }

    fn with_selected_apps(&self, consume: impl FnOnce(&Sample) -> Result<Value>) -> Result<Value> {
        let selected = self
            .selected
            .as_ref()
            .ok_or(Error::Bound("no selected observation"))?;
        let current = self
            .inspected
            .iter()
            .chain(self.observations.iter())
            .find(|observation| same(&observation.json(), selected));

        if let Some(current) = current {
            let sample = current
                .app_sample
                .as_deref()
                .ok_or(Error::Bound("live App aggregate unavailable"))?;

            consume(sample)
        } else {
            self.history
                .as_ref()
                .ok_or(Error::Bound("live App raw observation unavailable"))?
                .with_app_sample(selected, consume)
        }
    }

    /// The newest second the session holds, saved or still in the ring.
    fn newest_second(&self) -> Option<u64> {
        let saved = self.history.as_ref().and_then(Session::last_second);
        let mut newest = saved;
        for point in &self.points {
            let second = point["wall_ns"]
                .as_u64()
                .map(|wall| wall / NANOSECONDS_PER_SECOND);
            assert!(second.is_some());
            newest = newest.max(second);
        }

        newest
    }

    fn select_refresh_due(&self, second: u64) -> bool {
        self.observations.front().is_none_or(|observation| {
            second < observation.observation.wall / NANOSECONDS_PER_SECOND
        })
    }

    fn refresh_history(&mut self) -> Result<()> {
        if self.root.join("commits").try_exists()? {
            // The windows the previous `Session` verified on a read need only their markers read
            // again.
            let verified = self
                .history
                .as_ref()
                .map_or_else(Verified::default, Session::verified_windows);
            let mut history = Session::reopen(
                &self.root,
                self.cancellation.clone(),
                self.operation.epoch(),
                &verified,
            )?;

            if let Some(previous) = &self.history {
                if previous.windows == history.windows {
                    refresh_history_pending(&mut self.sources, previous);
                    return Ok(());
                }
            }

            if let Some(selected) = &self.selected {
                history.restore_selection(selected)?;
            }

            refresh_history_pending(&mut self.sources, &history);
            if let Some(restored) = &self.restored {
                // A recovered minute can land before the restored context's end, and only a
                // full restoration reads it.
                let kept = self.history.as_ref().is_some_and(|previous| {
                    refresh_history_before(&previous.windows, restored.through_ns)
                        == refresh_history_before(&history.windows, restored.through_ns)
                });
                if !kept {
                    self.restored = None;
                }
            }
            self.history = Some(history);
        }

        Ok(())
    }

    fn history_failure(&mut self, error: Error) -> Result<()> {
        self.operation.check()?;
        if let Error::Io(problem) = &error {
            if problem.kind() == std::io::ErrorKind::Interrupted {
                return Err(error);
            }
        }

        let message = error.to_string().chars().take(512).collect::<String>();
        eprintln!("flightdeck history: {message}");
        self.history_error = Some(message);

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
        let previous = self
            .observations
            .iter()
            .rev()
            .find(|observation| observation.identity == current.identity);
        Sample::capture(
            &current.app_frame,
            previous.map(|observation| observation.app_frame.as_ref()),
            &mut current.app_sample,
        )?;
        current.bytes += current
            .app_sample
            .as_ref()
            .unwrap_or_else(|| unreachable!("successful App capture initializes its output"))
            .bytes();
        if current.bytes > CACHE_BYTES_MAX {
            return Err(Error::Bound("live observation bytes"));
        }

        let tables = poll_store_tables(previous, &current);
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
        let mut app_point = None;
        self.app_history.capture(
            current
                .app_sample
                .as_ref()
                .unwrap_or_else(|| unreachable!()),
            segment,
            &mut app_point,
        );
        let second = current.observation.wall / NANOSECONDS_PER_SECOND;
        let first = second.saturating_sub(899) * NANOSECONDS_PER_SECOND;
        // The App history keeps the lead-in before the context as well, which its charts draw.
        let charted_first = second.saturating_sub(CHARTED_SECONDS - 1) * NANOSECONDS_PER_SECOND;
        self.poll_points(&current.point, first)?;
        self.cache_bytes += current.bytes;
        self.observations.push_back(current);
        self.poll_evict(first)?;
        self.poll_cpu_history()?;
        if self.follows_live {
            self.poll_ranks(first)?;
            self.poll_parts(first)?;
        }
        self.app_history
            .push(app_point.unwrap_or_else(|| unreachable!()), charted_first);

        Ok(())
    }

    fn poll_cpu_history(&mut self) -> Result<()> {
        let current = self
            .observations
            .back()
            .ok_or(Error::Bound("CPU live current"))?;
        let mut identity = None;
        crate::cpu_history_sample::Identity::read(
            &current.point,
            &current.observation,
            &mut identity,
        )?;
        self.cpu_history
            .as_mut()
            .ok_or(Error::Bound("CPU live retention"))?
            .capture(
                &identity.ok_or(Error::Bound("CPU live identity"))?,
                current
                    .tables
                    .get("cpus")
                    .ok_or(Error::Bound("missing CPU table"))?,
            )?;

        Ok(())
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
        self.select_rehydrate_apps(&mut current, &mut independent)?;
        if current.bytes > CACHE_BYTES_MAX {
            return Err(Error::Bound("live observation bytes"));
        }

        current.point = point.clone();

        Ok(Some(current))
    }

    fn select_rehydrate_apps(
        &self,
        current: &mut LiveObservation,
        source: &mut Source,
    ) -> Result<()> {
        let previous = self.points.iter().rev().find(|point| {
            let selected = &point["observation"];
            source_matches(source, selected)
                && selected["publication"]
                    .as_u64()
                    .is_some_and(|publication| publication < current.observation.publication)
        });
        let mut previous_frame = None;
        if let Some(previous) = previous {
            if rehydrate_source_try_read(source, &previous["observation"])? {
                let publication = previous["observation"]["publication"]
                    .as_u64()
                    .ok_or(Error::Bound("previous App publication"))?;

                Frame::capture(
                    &Publication {
                        reader: &source.reader,
                        tick: &source.tick,
                        identities: &source.identities,
                        publication,
                    },
                    &mut previous_frame,
                )?;
                if let Some(frame) = previous_frame.as_deref_mut() {
                    project_observation_capture_events(source, publication, frame, |_, _, _| {
                        Ok(())
                    })?;
                }
            }
        }

        Sample::capture(
            &current.app_frame,
            previous_frame.as_deref(),
            &mut current.app_sample,
        )?;
        current.bytes += current
            .app_sample
            .as_ref()
            .unwrap_or_else(|| unreachable!("successful App capture initializes its output"))
            .bytes();

        Ok(())
    }

    fn select_trends(&self, selection: ContextSelection, response: &mut Value) -> Result<()> {
        let last = selection.context_last_second();
        let points = response["trends"]
            .as_array_mut()
            .ok_or(Error::Bound("trends shape"))?;

        for point in &self.points {
            let current = point["wall_ns"]
                .as_u64()
                .ok_or(Error::Bound("point clock"))?
                / NANOSECONDS_PER_SECOND;

            if (last.saturating_sub(DETAIL_SECONDS - 1)..=last).contains(&current) {
                let mut live = metrics::public_point(point)?;
                if let Some(index) = points.iter().position(|existing| same(existing, point)) {
                    follow_merge_point(PointMerge {
                        live: &mut live,
                        committed: &points[index],
                    })?;
                    points[index] = live;
                } else {
                    if points.len() == OBSERVATIONS_MAX {
                        return Err(Error::Bound("context observations"));
                    }

                    points.push(live);
                }
            }
        }

        points.sort_by(|left, right| identity_order(left).cmp(&identity_order(right)));
        metrics::context_segments(points)?;

        context_bytes(points)
    }

    fn select_observation(
        &mut self,
        selection: ContextSelection,
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
                        self.select_history(selection, selected, response)?;
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

    fn select_history(
        &mut self,
        selection: ContextSelection,
        selected: &Value,
        response: &mut Value,
    ) -> Result<()> {
        let history = self
            .history
            .as_mut()
            .ok_or(Error::Bound("missing history"))?;
        let historical = history.select_context(selection, 0)?;
        let alternatives = historical["alternatives"]
            .as_array()
            .ok_or(Error::Bound("historical alternatives"))?;
        let index = alternatives
            .iter()
            .position(|observation| same(observation, selected))
            .ok_or(Error::Bound("historical observation"))?;
        response["summary"] = history.select_context(selection, index)?["summary"].clone();
        Ok(())
    }
}

/// The metric tables `metrics::point` reads for `current`: its own, and those of the observation
/// before it in the same run, from which its rates come.
fn poll_store_tables(
    previous: Option<&LiveObservation>,
    current: &LiveObservation,
) -> metrics::Tables {
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
    tables
}

/// A selection of `second` with nothing read from the saved minutes.
fn select_unsaved(selection: ContextSelection) -> Value {
    json!({"second": selection.second(), "context_last_second": selection.context_last_second(),
        "selected": null, "alternatives": [], "summary": null, "trends": [],
        "context_seconds": 900})
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
        pending_refresh_polls: 0,
    }));

    rehydrate_source_read(independent, selected)
}

fn rehydrate_source_read(source: &mut Source, selected: &Value) -> Result<()> {
    if !rehydrate_source_try_read(source, selected)? {
        return Err(Error::Bound("unsealed publication no longer retained"));
    }

    Ok(())
}

fn rehydrate_source_try_read(source: &mut Source, selected: &Value) -> Result<bool> {
    let publication = selected["publication"]
        .as_u64()
        .ok_or(Error::Bound("selected publication"))?;

    source.reader.seek_publication(publication)?;
    match source.read_tick()? {
        Found::Publication {
            publication: found, ..
        } if found == publication => {}
        _ => return Ok(false),
    }

    if Some(source.tick.run_tick) != selected["run_tick"].as_u64() {
        return Err(Error::Bound("changed inspection tick identity"));
    }

    if source.tick.times.map(|times| times.wall_ns) != selected["wall_ns"].as_u64() {
        return Err(Error::Bound("changed inspection wall identity"));
    }

    Ok(true)
}

fn project_observation(
    source: &mut Source,
    publication: u64,
    connection: &Engine,
    operation: &Operation,
) -> Result<Option<LiveObservation>> {
    let Some(times) = source.tick.times else {
        source.losses.unknown = true;
        return Ok(None);
    };

    operation.check()?;

    let mut app_frame = None;
    Frame::capture(
        &Publication {
            reader: &source.reader,
            tick: &source.tick,
            identities: &source.identities,
            publication,
        },
        &mut app_frame,
    )?;
    let mut app_frame = app_frame
        .unwrap_or_else(|| unreachable!("successful App frame capture initializes its output"));

    let mut window = Window::new(
        WindowIdentity {
            run: source.identity,
            window: 0,
        },
        Limits::from_shape(&source.reader.shape())?,
    );
    project_observation_stage(source, publication, &times, &mut window, &mut app_frame)?;

    let tables = project_observation_tables(connection, operation, &window)?;
    let bytes = tables.values().map(table_bytes).sum::<usize>() + app_frame.bytes();
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
        app_frame,
        app_sample: None,
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
    app_frame: &mut Frame,
) -> Result<()> {
    let observation = persistence::Observation {
        publication,
        wall_ns: Some(times.wall_ns),
        continuous_ns: Some(times.continuous_ns),
        awake_ns: Some(times.awake_ns),
    };

    project_observation_capture_events(
        source,
        publication,
        app_frame,
        |source, events, numbered| {
            if source.reader.dictionary_capacity().is_some() {
                window.push_tick_with_identity(
                    &observation,
                    &source.tick,
                    numbered,
                    &persistence::IdentitySource {
                        reader: &source.reader,
                        processes: &source.identities,
                        events: &events.identities,
                    },
                )?;
            } else {
                window.push_tick(&observation, &source.tick, numbered)?;
            }

            Ok(())
        },
    )
}

fn project_observation_capture_events(
    source: &mut Source,
    publication: u64,
    frame: &mut Frame,
    consume: impl FnOnce(&Source, &ProjectionEvents, &[NumberedEvent<'_>]) -> Result<()>,
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

    project_observation_stage_capture(source, publication, frame, &numbered, &events.identities)?;

    consume(source, &events, &numbered)
}

fn project_observation_stage_capture(
    source: &Source,
    publication: u64,
    app_frame: &mut Frame,
    numbered: &[NumberedEvent<'_>],
    identities: &[IdentityObservation],
) -> Result<()> {
    app_frame.capture_exits(
        &Publication {
            reader: &source.reader,
            tick: &source.tick,
            identities: &source.identities,
            publication,
        },
        &PublicationEvents {
            events: numbered,
            identities,
        },
    )
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
                "history_refresh_polls_max": HISTORY_REFRESH_POLLS_MAX
            }))
            .collect::<Vec<_>>()
    )
}

fn poll_commit_history_due(source: &mut Source) -> bool {
    assert!(source.pending_refresh_polls < HISTORY_REFRESH_POLLS_MAX);
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

/// The summary of a live observation a hover inspects, which carries neither the observation
/// it names, as a selection's summary does not, nor a segment, which only a context numbers.
fn inspect_summary(point: &Value) -> Result<Value> {
    let mut summary = metrics::public_point(point)?;
    summary
        .as_object_mut()
        .ok_or(Error::Bound("point shape"))?
        .remove("segment");
    Ok(summary)
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
    connection: &Engine,
    operation: &Operation,
    window: &Window,
) -> Result<BTreeMap<&'static str, Table>> {
    let mut tables = BTreeMap::new();
    for (table, _, order) in persistence::schema::TABLES {
        let name = table.name();

        connection.register(name, persistence::frame(table, window)?)?;
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
    tables.insert(
        metrics::core_frequency::CORE_LEVEL_STATES,
        metrics::core_frequency::core_level_states(
            connection,
            &persistence::query::table_source(
                connection,
                persistence::schema::Table::CoreStates,
                persistence::query::TableSource::Staged,
            )?,
            operation,
        )?,
    );

    Ok(tables)
}

/// Keeps the restored points from `first_ns` up to `end_ns`, where newly sealed history
/// takes over.
fn follow_restore_kept(
    points: &[Value],
    first_ns: u64,
    end_ns: u64,
    output: &mut Vec<Value>,
) -> Result<()> {
    assert!(output.is_empty());

    if first_ns < end_ns {
        let range = ContextRange { first_ns, end_ns };
        for point in points {
            if context_contains(range, point)? {
                output.push(point.clone());
            }
        }
    }

    Ok(())
}

/// Counts the sealed minutes that begin before `through_ns`, or at an unknown time.
fn refresh_history_before(windows: &[persistence::Manifest], through_ns: u64) -> usize {
    windows
        .iter()
        .filter(|window| window.wall_ns.is_none_or(|wall| wall.min < through_ns))
        .count()
}

/// The first second of the stretch that holds the first of the full-detail seconds the charts
/// of the context that ends at `context_last_second` hold, where its older tail ends. The
/// division floors.
fn earlier_limit_second(context_last_second: u64) -> u64 {
    let charted_first = context_last_second.saturating_sub(CHARTED_SECONDS - 1);
    charted_first / EARLIER_STRIDE_SECONDS * EARLIER_STRIDE_SECONDS
}

fn context_contains(range: ContextRange, point: &Value) -> Result<bool> {
    let wall = point["wall_ns"]
        .as_u64()
        .ok_or(Error::Bound("point clock"))?;
    Ok((range.first_ns..range.end_ns).contains(&wall))
}

fn context_bytes_of(points: &[&Value]) -> Result<()> {
    if points.len() > OBSERVATIONS_MAX {
        return Err(Error::Bound("context observations"));
    }

    if serde_json::to_vec(points)
        .map_err(std::io::Error::other)?
        .len()
        > CONTEXT_BYTES_MAX
    {
        return Err(Error::Bound("context bytes"));
    }

    Ok(())
}

fn context_bytes(points: &[Value]) -> Result<()> {
    if points.len() > OBSERVATIONS_MAX {
        return Err(Error::Bound("context observations"));
    }

    if serde_json::to_vec(points)
        .map_err(std::io::Error::other)?
        .len()
        > CONTEXT_BYTES_MAX
    {
        return Err(Error::Bound("context bytes"));
    }

    Ok(())
}

fn follow_merge_point(merge: PointMerge<'_>) -> Result<()> {
    let PointMerge { live, committed } = merge;
    for name in [
        "run",
        "boot",
        "publication",
        "run_tick",
        "wall_ns",
        "continuous_ns",
    ] {
        if live[name] != committed[name] {
            return Err(Error::Bound("changed committed point identity"));
        }
    }

    let fields = live.as_object_mut().ok_or(Error::Bound("point shape"))?;
    if fields.len() > 128 {
        return Err(Error::Bound("point fields"));
    }

    for (name, value) in fields {
        if value.is_null() {
            if let Some(committed) = committed.get(name) {
                *value = committed.clone();
            }
        }
    }

    if let Some(storage) = live.get_mut("storage") {
        if let Some(committed) = committed.get("storage") {
            storage_merge::point(PointMerge {
                live: storage,
                committed,
            })?;
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "live_fixture.rs"]
pub(crate) mod identity_fixture;

#[cfg(test)]
#[path = "live_app_process_history_tests.rs"]
mod app_process_history_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use collector::recording::{Registration, private};
    use collector::ring::{Dropped, ObservationTimes, Place, Readings, Shape, Writer};
    use std::sync::atomic::{AtomicU64, Ordering};

    mod cpu_history_tests {
        use super::*;
        include!("cpu_history_live_tests.rs");
    }

    mod fan_history_tests {
        use super::*;
        include!("live_fan_tests.rs");
    }

    mod storage_tests {
        use super::*;
        include!("live_storage_tests.rs");
    }

    mod battery_tests {
        use super::*;
        include!("live_battery_tests.rs");
    }

    mod app_event_tests {
        use super::*;
        include!("live_app_event_tests.rs");
    }

    mod commit_selection_tests {
        use super::*;
        include!("live_commit_selection_tests.rs");
    }

    mod inspect_tests {
        use super::*;
        include!("live_inspect_tests.rs");
    }

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct RingFixture {
        root: PathBuf,
        writer: Writer,
        following_fans: u16,
    }

    impl RingFixture {
        fn new() -> Self {
            Self::new_with_cpu_capacity(1)
        }

        fn new_with_cpu_capacity(cpu_capacity: u64) -> Self {
            assert!(cpu_capacity > 0);
            assert!(cpu_capacity <= 2);

            Self::new_with_shape(&Shape {
                slot_count: 2,
                cpu_capacity,
                interface_capacity: 1,
                core_capacity: 1,
                state_capacity: 1,
                fan_capacity: 1,
                process_capacity: 1,
                coalition_capacity: 1,
                event_capacity: 8,
            })
        }

        fn new_with_shape(shape: &Shape) -> Self {
            assert!(shape.slot_count <= 120);
            assert!(shape.cpu_capacity <= 18);

            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("flightdeck-retry-{}-{serial}", std::process::id()));
            private::create_private_dir(&root).unwrap();
            let place = Place {
                lock_path: root.join("ring.lock"),
                name_prefix: format!("/fdretry{}x{serial}", std::process::id()),
            };
            let (writer, registered) = Registration::start(&root, &place, *shape, 0)
                .unwrap()
                .into_parts();
            registered.unwrap();

            Self {
                root,
                writer,
                following_fans: 2,
            }
        }

        fn new_following() -> Self {
            Self::new_following_fans(2)
        }

        fn new_following_fans(following_fans: u16) -> Self {
            assert!((2..=10).contains(&following_fans));
            let mut fixture = Self::new_with_shape(&Shape {
                slot_count: 120,
                cpu_capacity: 18,
                interface_capacity: 1,
                core_capacity: 1,
                state_capacity: 1,
                fan_capacity: u64::from(following_fans),
                process_capacity: 1,
                coalition_capacity: 1,
                event_capacity: 8,
            });
            fixture.following_fans = following_fans;
            fixture
        }

        fn publish_following(&mut self, tick: u32) {
            self.publish_following_wall(tick, 1000 + u64::from(tick));
        }

        fn publish_following_wall(&mut self, tick: u32, wall_second: u64) {
            assert!((1..=901).contains(&tick));

            let cpus = [collector::machine::CPUTicks {
                user: tick * 20,
                system: tick * 20,
                nice: tick * 10,
                idle: tick * 50,
            }; 18];
            let memory = collector::machine::Memory {
                total_bytes: 48 * 1024 * 1024 * 1024,
                active_bytes: u64::from(tick) * 1024 * 1024,
                ..collector::machine::Memory::default()
            };
            let fan_rpm: Vec<f32> = (0..self.following_fans)
                .map(|channel| match channel {
                    0 => f32::from(u16::try_from(tick).unwrap()),
                    1 => 0.0,
                    _ => 1000.0 + f32::from(channel) * 17.0,
                })
                .collect();
            self.writer.publish_observed(
                &Readings {
                    run_tick: u64::from(tick),
                    cpus: &cpus,
                    memory: &memory,
                    interfaces: &[],
                    ioreport: None,
                    smc: Some(collector::ring::SmcReadings {
                        power_watts: 5.0,
                        soc_celsius: 40.0,
                        cpu_celsius: Some(61.5),
                        gpu_celsius: Some(52.25),
                        fan_rpm: &fan_rpm,
                    }),
                    processes: &[],
                    processes_refused: 0,
                    processes_gone: 0,
                    coalitions: &[],
                    events: &[],
                    dropped: Dropped::default(),
                },
                &ObservationTimes {
                    wall_ns: wall_second * NANOSECONDS_PER_SECOND,
                    continuous_ns: u64::from(tick - 1) * NANOSECONDS_PER_SECOND,
                    awake_ns: u64::from(tick - 1) * NANOSECONDS_PER_SECOND,
                },
            );
        }

        fn publish_cpu_activity(&mut self, tick: u32) {
            let cpus = [
                collector::machine::CPUTicks {
                    user: tick * 25,
                    idle: tick * 75,
                    ..collector::machine::CPUTicks::default()
                },
                collector::machine::CPUTicks {
                    user: tick * 75,
                    idle: tick * 25,
                    ..collector::machine::CPUTicks::default()
                },
            ];
            self.publish_cpu_readings(tick, &cpus);
        }

        fn publish_cpu_readings(&mut self, tick: u32, cpus: &[collector::machine::CPUTicks]) {
            self.writer.publish_observed(
                &Readings {
                    run_tick: u64::from(tick),
                    cpus,
                    memory: &collector::machine::Memory::default(),
                    interfaces: &[],
                    ioreport: None,
                    smc: None,
                    processes: &[],
                    processes_refused: 0,
                    processes_gone: 0,
                    coalitions: &[],
                    events: &[],
                    dropped: Dropped::default(),
                },
                &ObservationTimes {
                    wall_ns: (1000 + u64::from(tick)) * NANOSECONDS_PER_SECOND,
                    continuous_ns: u64::from(tick) * NANOSECONDS_PER_SECOND,
                    awake_ns: u64::from(tick) * NANOSECONDS_PER_SECOND,
                },
            );
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
    fn restores_fifteen_minutes_when_following_a_cold_or_reopened_live_source() {
        // Goal: cold and reopened following includes all available last-900-second readings
        // and the lead-in before them.
        // Method: seal 840 genuine one-Hz publications and retain a 120-slot overlapping tail.
        let mut fixture = RingFixture::new_following();
        follow_restoration_publish(&mut fixture);
        let mut cold = Value::Null;
        let mut reopened = Value::Null;

        follow_restoration_read(&fixture.root, "cold", &mut cold);
        follow_restoration_read(&fixture.root, "reopened", &mut reopened);

        assert_eq!(cold, reopened);
        assert_eq!(cold["trends"][1]["fan_rpm"], json!([2.0, 0.0]));
        assert_eq!(cold["trends"][701]["fan_rpm"], json!([702.0, 0.0]));
        assert_eq!(cold["summary"]["fan_rpm"], json!([901.0, 0.0]));
        follow_restoration_assert_window(&cold);
        follow_restoration_assert_window(&reopened);
        let mut session = LiveSession::open(&fixture.root).unwrap();
        let selected = session.select(1002, 0).unwrap();
        assert_eq!(selected["selected"]["publication"], 2);
        assert_eq!(selected["summary"]["fan_rpm"], json!([2.0, 0.0]));
        assert_eq!(
            session.follow_latest().unwrap()["summary"]["fan_rpm"],
            json!([901.0, 0.0])
        );
    }

    #[test]
    fn following_reuses_restored_history_while_the_live_range_advances() {
        // Goal: ordinary following advances without repeating committed-history queries.
        // Method: advance a genuine source after caching one committed minute.
        let mut fixture = RingFixture::new_following();
        follow_restoration_short(&mut fixture);
        let mut session = LiveSession::open(&fixture.root).unwrap();
        session.follow_latest().unwrap();
        let reads = session.history.as_ref().unwrap().source_reads.get();

        fixture.publish_following(63);
        session.poll().unwrap();
        let response = session.follow_latest().unwrap();
        let repeated = session.follow_latest().unwrap();

        assert_eq!(response, repeated);
        assert_eq!(response["selected"]["publication"], 63);
        assert_eq!(session.history.as_ref().unwrap().source_reads.get(), reads);
        assert_eq!(response["trends"].as_array().unwrap().len(), 63);
    }

    #[test]
    fn following_reads_only_the_newly_sealed_minute_into_restored_history() {
        // Goal: a sealed minute extends the restored context without re-reading earlier ones,
        // and the extended context reads as a full restoration would.
        // Method: follow fourteen sealed minutes, seal the fifteenth, then compare the follow
        // and its saved-source reads with those of a newly opened session.
        let mut fixture = RingFixture::new_following();
        let mut pending = None;
        follow_restoration_publish_pending(&mut fixture, &mut pending);
        let mut session = LiveSession::open(&fixture.root).unwrap();
        session.follow_latest().unwrap();
        seal_window(&fixture.root, pending.as_ref().unwrap());
        assert!(!session.poll().unwrap());
        let before = session.history.as_ref().unwrap().source_reads.get();

        let extended = session.follow_latest().unwrap();

        let mut opened = LiveSession::open(&fixture.root).unwrap();
        let restored = opened.follow_latest().unwrap();
        let reads_extended = session.history.as_ref().unwrap().source_reads.get() - before;
        let reads_restored = opened.history.as_ref().unwrap().source_reads.get();
        assert_eq!(extended["trends"], restored["trends"]);
        assert_eq!(session.restored.as_ref().unwrap().points.len(), 900);
        assert!(reads_extended * 7 < reads_restored);
    }

    #[test]
    fn following_rebuilds_an_empty_requested_range_after_clock_rollback() {
        // Goal: clock rollback rebuilds coverage and caches an honestly empty historical range.
        // Method: move live wall time before every committed tick, then follow twice.
        let mut fixture = RingFixture::new_following();
        follow_restoration_short(&mut fixture);
        let mut session = LiveSession::open(&fixture.root).unwrap();
        session.follow_latest().unwrap();

        fixture.publish_following_wall(63, 500);
        session.poll().unwrap();
        let response = session.follow_latest().unwrap();
        let reads = session.history.as_ref().unwrap().source_reads.get();
        let repeated = session.follow_latest().unwrap();

        assert_eq!(response, repeated);
        assert_eq!(
            response["selected"]["wall_ns"],
            500 * NANOSECONDS_PER_SECOND
        );
        assert_eq!(response["trends"].as_array().unwrap().len(), 1);
        assert_eq!(response["summary"]["cpu_percent"], Value::Null);
        assert!(session.restored.as_ref().unwrap().points.is_empty());
        assert_eq!(session.restored.as_ref().unwrap().range.first_ns, 0);
        assert_eq!(
            session.restored.as_ref().unwrap().range.end_ns,
            501 * NANOSECONDS_PER_SECOND
        );
        assert_eq!(session.history.as_ref().unwrap().source_reads.get(), reads);
    }

    #[test]
    fn following_keeps_a_missing_tick_as_a_segment_break() {
        // Goal: restored and live points do not bridge a real scheduled gap.
        // Method: omit run tick63 and compare the final two merged segments.
        let mut fixture = RingFixture::new_following();
        follow_restoration_short(&mut fixture);
        let mut session = LiveSession::open(&fixture.root).unwrap();
        fixture.publish_following(64);
        session.poll().unwrap();

        let response = session.follow_latest().unwrap();
        let points = response["trends"].as_array().unwrap();

        assert_eq!(response["selected"]["publication"], 63);
        assert_eq!(response["selected"]["run_tick"], 64);
        assert_eq!(response["summary"]["cpu_percent"], Value::Null);
        assert!(
            points[points.len() - 1]["segment"].as_u64().unwrap()
                > points[points.len() - 2]["segment"].as_u64().unwrap()
        );
    }

    #[test]
    fn following_preserves_the_old_context_when_restoration_is_canceled() {
        // Goal: a canceled public follow never installs partial restored context.
        // Method: cancel rollback restoration, then retry through the next public generation.
        let mut fixture = RingFixture::new_following();
        follow_restoration_short(&mut fixture);
        let control = Arc::new(Cancellation::default());
        let mut session = LiveSession::open_with(&fixture.root, control.clone(), 0).unwrap();
        session.follow_latest().unwrap();
        let first = session.restored.as_ref().unwrap().range.first_ns;
        let points = session.restored.as_ref().unwrap().points.clone();
        assert_eq!(points[0]["fan_rpm"], json!([1.0, 0.0]));
        fixture.publish_following_wall(63, 500);
        session.poll().unwrap();
        let cursor = session.sources[0].reader.next_publication();
        let losses = session.sources[0].losses;
        let cancel = control.clone();
        session.before_restore = Some(Box::new(move || cancel.cancel().unwrap()));

        let result = session.follow_latest();

        assert!(matches!(result, Err(Error::Io(error))
            if error.kind()==std::io::ErrorKind::Interrupted));
        assert_eq!(session.restored.as_ref().unwrap().range.first_ns, first);
        assert_eq!(session.restored.as_ref().unwrap().points, points);
        assert_eq!(session.sources[0].reader.next_publication(), cursor);
        assert_eq!(session.sources[0].losses, losses);
        session.begin(control.epoch()).unwrap();
        let response = session.follow_latest().unwrap();
        assert_eq!(response["selected"]["publication"], 63);
        assert_eq!(response["summary"]["fan_rpm"], json!([63.0, 0.0]));
        assert_eq!(
            response["selected"]["wall_ns"],
            500 * NANOSECONDS_PER_SECOND
        );
        assert_eq!(response["trends"].as_array().unwrap().len(), 1);
        assert_eq!(session.restored.as_ref().unwrap().range.first_ns, 0);
        assert!(session.restored.as_ref().unwrap().points.is_empty());
        let reads = session.history.as_ref().unwrap().source_reads.get();
        assert_eq!(session.follow_latest().unwrap(), response);
        assert_eq!(session.history.as_ref().unwrap().source_reads.get(), reads);
    }

    #[test]
    fn following_retains_live_interval_values_when_a_commit_has_no_predecessor() {
        // Goal: incomplete committed intervals cannot erase valid live metric fields.
        // Method: commit only the second of two genuinely captured live CPU readings.
        let mut fixture = RingFixture::new_with_cpu_capacity(2);
        fixture.publish_cpu_activity(1);
        let mut session = LiveSession::open(&fixture.root).unwrap();
        fixture.publish_cpu_activity(2);
        session.poll().unwrap();
        let source = &session.sources[0];
        let mut window = Window::new(
            WindowIdentity {
                run: source.identity,
                window: 0,
            },
            Limits::from_shape(&source.reader.shape()).unwrap(),
        );
        cpu_detail_stage(&mut window, source, 2);
        seal_window(&fixture.root, &window);
        session.refresh_history().unwrap();

        let response = session.follow_latest().unwrap();

        assert_eq!(
            session.restored.as_ref().unwrap().points[0]["cpu_percent"],
            Value::Null
        );
        assert_eq!(response["summary"]["cpu_percent"], 50.0);
        assert_eq!(response["trends"][1]["cpu_percent"], 50.0);
    }

    #[test]
    fn context_rejects_more_than_4096_scalar_points() {
        // Goal: the context budget refuses a result larger than the public observation bound.
        // Method: measure 4097 individually tiny scalar values, whose bytes fit the other bound.
        let points = vec![json!({"cpu_percent":0}); 4097];

        let result = context_bytes(&points);

        assert!(matches!(result, Err(Error::Bound("context observations"))));
    }

    #[test]
    fn context_rejects_more_than_eight_mib_of_serialized_scalars() {
        // Goal: a small point count cannot evade the separate serialized context byte bound.
        // Method: encode one 8 MiB value, whose JSON framing exceeds the 8 MiB contract.
        let points = vec![Value::String("x".repeat(8 * 1024 * 1024))];

        let result = context_bytes(&points);

        assert!(matches!(result, Err(Error::Bound("context bytes"))));
    }

    #[test]
    fn following_retains_context_across_unchanged_catalog_refreshes() {
        // Goal: catalog discovery must not repeat metric queries for unchanged manifests.
        // Method: follow real history, poll beyond two refresh periods, and compare the response.
        let mut fixture = RingFixture::new_following();
        follow_restoration_short(&mut fixture);
        let mut session = LiveSession::open(&fixture.root).unwrap();
        let before = session.follow_latest().unwrap();
        let reads = session.history.as_ref().unwrap().source_reads.get();
        let cursor = session.sources[0].reader.next_publication();
        let losses = session.sources[0].losses;

        for _poll in 0..513 {
            assert!(!session.poll().unwrap());
        }
        let after = session.follow_latest().unwrap();

        assert_eq!(after, before);
        assert_eq!(session.history.as_ref().unwrap().source_reads.get(), reads);
        assert_eq!(session.sources[0].reader.next_publication(), cursor);
        assert_eq!(session.sources[0].losses, losses);
    }

    #[test]
    fn following_opens_live_when_initial_history_is_unreadable() {
        // Goal: optional history cannot prevent a healthy live source from opening.
        // Method: hide genuine sealed data before opening, then follow and read its memory page.
        let mut fixture = RingFixture::new_following();
        follow_restoration_short(&mut fixture);
        std::fs::rename(fixture.root.join("data"), fixture.root.join("held-data")).unwrap();

        let mut session = LiveSession::open(&fixture.root).unwrap();
        let response = session.follow_latest().unwrap();

        assert_eq!(response["selected"]["publication"], 62);
        assert_eq!(response["summary"]["publication"], 62);
        assert!(response["context_error"].is_string());
        assert_eq!(
            session.catalog()["history_error"],
            response["context_error"]
        );
        assert_eq!(
            session.page("memory", 0).unwrap()["observation"],
            response["selected"]
        );
    }

    #[test]
    fn selecting_a_live_second_keeps_its_readings_when_a_saved_minute_fails_verification() {
        // Goal: a damaged saved minute cannot hide the readings the ring still holds, as an
        // unreadable history cannot at open.
        // Method: put a stray file beside a sealed minute's tables, then select a second whose
        // context reads that minute.
        let mut fixture = RingFixture::new_following();
        follow_restoration_short(&mut fixture);
        let sealed = Session::open(&fixture.root).unwrap().windows.remove(0);
        let stray = fixture
            .root
            .join(&sealed.data_directory)
            .join("stray.parquet");
        std::fs::write(stray, b"stray").unwrap();
        let mut session = LiveSession::open(&fixture.root).unwrap();

        let response = session.select(1062, 0).unwrap();

        assert_eq!(response["selected"]["publication"], 62);
        assert!(session.catalog()["history_error"].is_string());
    }

    #[test]
    fn following_keeps_live_metrics_and_retries_an_unreadable_context() {
        // Goal: failed historical restoration preserves usable live metrics and recovers publicly.
        // Method: move real data aside, roll the clock back into the saved minute to force its
        // restoration, then restore it. The live reading keeps the cached minute's first half.
        let mut fixture = RingFixture::new_following();
        follow_restoration_short(&mut fixture);
        let mut session = LiveSession::open(&fixture.root).unwrap();
        session.follow_latest().unwrap();
        let cached = session.restored.as_ref().unwrap().points.clone();
        let data = fixture.root.join("data");
        let held = fixture.root.join("held-data");
        std::fs::rename(&data, &held).unwrap();
        for _poll in 0..256 {
            assert!(!session.poll().unwrap());
        }
        assert!(session.catalog()["history_error"].is_string());
        assert_eq!(session.restored.as_ref().unwrap().points, cached);
        fixture.publish_following_wall(63, 1030);
        assert!(session.poll().unwrap());
        let cursor = session.sources[0].reader.next_publication();
        let losses = session.sources[0].losses;

        let degraded = session.follow_latest().unwrap();

        assert_eq!(degraded["selected"]["publication"], 63);
        assert_eq!(degraded["summary"]["publication"], 63);
        assert_eq!(degraded["trends"].as_array().unwrap().len(), 31);
        assert!(!degraded["context_error"].as_str().unwrap().is_empty());
        assert!(degraded["context_error"].as_str().unwrap().chars().count() <= 512);
        assert_eq!(
            session.catalog()["history_error"],
            degraded["context_error"]
        );
        assert_eq!(session.restored.as_ref().unwrap().points, cached);
        assert_eq!(
            session.page("memory", 0).unwrap()["observation"],
            degraded["selected"]
        );
        std::fs::rename(&held, &data).unwrap();
        let recovered = session.follow_latest().unwrap();
        assert_eq!(recovered["selected"], degraded["selected"]);
        assert!(recovered["context_error"].is_null());
        assert!(session.catalog()["history_error"].is_null());
        assert_eq!(session.sources[0].reader.next_publication(), cursor);
        assert_eq!(session.sources[0].losses, losses);
    }

    #[test]
    fn following_discovers_an_older_late_commit_outside_the_retained_ring() {
        // Goal: a late minute absent from the cold ring restores the real historical gap.
        // Method: withhold minute12, then seal it and poll256 times without another publication.
        let mut fixture = RingFixture::new_following();
        let mut missing = None;
        follow_restoration_publish_omitting(&mut fixture, &mut missing);
        let mut session = LiveSession::open(&fixture.root).unwrap();
        let initial = session.follow_latest().unwrap();
        assert_eq!(initial["trends"].as_array().unwrap().len(), 841);
        assert!(!session.sources[0].pending_windows.contains(&12));
        let cursor = session.sources[0].reader.next_publication();
        let losses = session.sources[0].losses;
        seal_window(&fixture.root, missing.as_ref().unwrap());

        for _poll in 0..256 {
            assert!(!session.poll().unwrap());
        }
        let response = session.follow_latest().unwrap();

        assert_eq!(response["selected"], initial["selected"]);
        assert_eq!(session.sources[0].reader.next_publication(), cursor);
        assert_eq!(session.sources[0].losses, losses);
        println!(
            "{}",
            json!({"phase":"older_late_commit",
            "points":response["trends"].as_array().unwrap().len(),
            "windows":session.catalog()["windows"],"cursor":cursor,"polls":256})
        );
        follow_restoration_assert_window(&response);
        assert_eq!(response["trends"][720]["publication"], 721);
        assert_eq!(response["trends"][720]["cpu_percent"], 50.0);
        assert_eq!(response["trends"][720]["fan_rpm"], json!([721.0, 0.0]));
        assert_eq!(response["trends"][780]["publication"], 781);
        assert_eq!(response["trends"][780]["cpu_percent"], 50.0);
        assert_eq!(response["trends"][780]["fan_rpm"], json!([781.0, 0.0]));
    }

    fn follow_restoration_publish_omitting(
        fixture: &mut RingFixture,
        missing: &mut Option<Window>,
    ) {
        assert!(missing.is_none());

        let mut reader = RingReader::open_generation(fixture.writer.name()).unwrap();
        let mut tick = persistence::tick_buffer(&reader.shape()).unwrap();
        let identity = reader.identity().unwrap();
        let limits = Limits::from_shape(&reader.shape()).unwrap();
        for minute in 0..14 {
            let mut window = Window::new(
                WindowIdentity {
                    run: identity,
                    window: minute,
                },
                limits,
            );
            for offset in 1..=60 {
                let publication = minute * 60 + offset;
                fixture.publish_following(u32::try_from(publication).unwrap());
                follow_restoration_stage(&mut reader, &mut tick, &mut window, publication);
            }
            if minute == 12 {
                *missing = Some(window);
            } else {
                seal_window(&fixture.root, &window);
            }
        }
        for publication in 841..=901 {
            fixture.publish_following(publication);
        }
    }

    fn follow_restoration_short(fixture: &mut RingFixture) {
        let mut reader = RingReader::open_generation(fixture.writer.name()).unwrap();
        let mut tick = persistence::tick_buffer(&reader.shape()).unwrap();
        let mut window = Window::new(
            WindowIdentity {
                run: reader.identity().unwrap(),
                window: 0,
            },
            Limits::from_shape(&reader.shape()).unwrap(),
        );
        for publication in 1..=60 {
            fixture.publish_following(publication);
            assert!(matches!(
                reader.read_into(&mut tick).unwrap(),
                Found::Publication { .. }
            ));
            let times = tick.times.unwrap();
            window
                .push_tick(
                    &persistence::Observation {
                        publication: u64::from(publication),
                        wall_ns: Some(times.wall_ns),
                        continuous_ns: Some(times.continuous_ns),
                        awake_ns: Some(times.awake_ns),
                    },
                    &tick,
                    &[],
                )
                .unwrap();
        }
        seal_window(&fixture.root, &window);
        fixture.publish_following(61);
        fixture.publish_following(62);
    }

    fn follow_restoration_publish(fixture: &mut RingFixture) {
        let mut pending = None;
        follow_restoration_publish_pending(fixture, &mut pending);
    }

    fn follow_restoration_publish_pending(fixture: &mut RingFixture, pending: &mut Option<Window>) {
        follow_restoration_publish_observing(fixture, pending, |_fixture| {});
    }

    fn follow_restoration_publish_observing(
        fixture: &mut RingFixture,
        pending: &mut Option<Window>,
        mut observe: impl FnMut(&RingFixture),
    ) {
        assert!(pending.is_none());
        let mut reader = RingReader::open_generation(fixture.writer.name()).unwrap();
        let mut tick = persistence::tick_buffer(&reader.shape()).unwrap();
        let identity = reader.identity().unwrap();
        let limits = Limits::from_shape(&reader.shape()).unwrap();
        for minute in 0..14 {
            let mut window = Window::new(
                WindowIdentity {
                    run: identity,
                    window: minute,
                },
                limits,
            );
            for offset in 1..=60 {
                let publication = minute * 60 + offset;
                fixture.publish_following(u32::try_from(publication).unwrap());
                observe(fixture);
                follow_restoration_stage(&mut reader, &mut tick, &mut window, publication);
            }
            seal_window(&fixture.root, &window);
        }
        let window = pending.insert(Window::new(
            WindowIdentity {
                run: identity,
                window: 14,
            },
            limits,
        ));
        for publication in 841..=900 {
            fixture.publish_following(publication);
            observe(fixture);
            follow_restoration_stage(&mut reader, &mut tick, window, u64::from(publication));
        }
        fixture.publish_following(901);
        observe(fixture);
    }

    fn follow_restoration_stage(
        reader: &mut RingReader,
        tick: &mut Tick,
        window: &mut Window,
        publication: u64,
    ) {
        assert_eq!(
            reader.read_into(tick).unwrap(),
            Found::Publication {
                publication,
                tick: publication
            }
        );
        let times = tick.times.unwrap();
        assert_eq!(
            times.continuous_ns / (60 * NANOSECONDS_PER_SECOND),
            window.identity.window
        );
        window
            .push_tick(
                &persistence::Observation {
                    publication,
                    wall_ns: Some(times.wall_ns),
                    continuous_ns: Some(times.continuous_ns),
                    awake_ns: Some(times.awake_ns),
                },
                tick,
                &[],
            )
            .unwrap();
    }

    #[test]
    fn following_restores_a_late_commit_without_a_new_publication_and_measures_cost() {
        // Goal: a late seal refreshes available context without a new tick or warm re-query.
        // Method: measure a genuine 18-CPU cold session, warm reads and its fifteenth seal.
        follow_restoration_measure_fans(2);
    }

    #[test]
    fn following_measures_ten_channel_restoration_cost() {
        // Goal: measure the same restoration paths at the largest acquired fan count.
        // Method: replay the genuine eighteen-CPU fixture with ten distinct channels.
        follow_restoration_measure_fans(10);
    }

    fn follow_restoration_measure_fans(fan_count: u16) {
        let mut fixture = RingFixture::new_following_fans(fan_count);
        let mut pending = None;
        follow_restoration_publish_pending(&mut fixture, &mut pending);
        let mut session_slot = None;
        follow_restoration_measure("cold_open", 0, || {
            session_slot = Some(LiveSession::open(&fixture.root).unwrap());
        });
        let mut session = session_slot.take().unwrap();
        follow_restoration_measure("cold_follow", 0, || {
            let response = session.follow_latest().unwrap();
            follow_restoration_assert_window(&response);
        });
        let reads = session.history.as_ref().unwrap().source_reads.get();
        follow_restoration_measure_context(&session, "cold_context", fan_count);
        for sample in 0..20 {
            follow_restoration_measure("warm_follow", sample, || {
                let response = session.follow_latest().unwrap();
                assert_eq!(response["trends"].as_array().unwrap().len(), 901);
            });
        }
        assert_eq!(session.history.as_ref().unwrap().source_reads.get(), reads);
        follow_restoration_measure_context(&session, "warm_context", fan_count);
        assert_eq!(session.restored.as_ref().unwrap().points.len(), 840);
        let selected = session.selected.clone();
        let cursor = session.sources[0].reader.next_publication();
        seal_window(&fixture.root, pending.as_ref().unwrap());

        follow_restoration_measure("post_commit_poll", 0, || {
            assert!(!session.poll().unwrap());
        });
        assert_eq!(session.restored.as_ref().unwrap().points.len(), 840);
        follow_restoration_measure("post_commit_follow", 0, || {
            let response = session.follow_latest().unwrap();
            follow_restoration_assert_window(&response);
        });

        assert_eq!(session.selected, selected);
        assert_eq!(session.sources[0].reader.next_publication(), cursor);
        assert_eq!(session.restored.as_ref().unwrap().points.len(), 900);
        follow_restoration_measure_context(&session, "late_context", fan_count);
        let restored = session.restored.as_ref().unwrap();
        println!(
            "{}",
            json!({"phase":"retained_scalar_context","points":restored.points.len(),
            "serialized_bytes":serde_json::to_vec(&restored.points).unwrap().len(),
            "coverage_first_ns":restored.range.first_ns,"coverage_end_ns":restored.range.end_ns})
        );
    }

    #[test]
    fn following_measures_the_full_warmed_live_context_with_committed_overlap() {
        // Goal: measure warm merging after 901 genuine ticks have filled the live context.
        // Method: poll each publication, retain 14 sealed minutes, then repeat 900-point follows.
        let mut fixture = RingFixture::new_following();
        let mut pending = None;
        let mut live_slot: Option<LiveSession> = None;
        follow_restoration_publish_observing(&mut fixture, &mut pending, |fixture| {
            if let Some(live) = &mut live_slot {
                live.poll().unwrap();
            } else {
                live_slot = Some(LiveSession::open(&fixture.root).unwrap());
            }
        });
        let mut live = live_slot.take().unwrap();
        assert_eq!(live.points.len(), 900);
        assert_eq!(live.history.as_ref().unwrap().windows.len(), 14);
        follow_restoration_measure("full_warm_first_follow", 0, || {
            let response = live.follow_latest().unwrap();
            follow_restoration_assert_window(&response);
        });
        let reads = live.history.as_ref().unwrap().source_reads.get();

        for sample in 0..20 {
            follow_restoration_measure("full_warm_follow", sample, || {
                let response = live.follow_latest().unwrap();
                assert_eq!(response["trends"].as_array().unwrap().len(), 901);
            });
        }

        assert_eq!(live.history.as_ref().unwrap().source_reads.get(), reads);
        println!(
            "{}",
            json!({"phase":"full_warm_context","live_points":live.points.len(),
            "committed_points":live.restored.as_ref().unwrap().points.len(),
            "historical_source_reads_before":reads,
            "historical_source_reads_after":live.history.as_ref().unwrap().source_reads.get()})
        );
    }

    fn follow_restoration_measure_context(session: &LiveSession, phase: &str, fan_count: u16) {
        let restored = session.restored.as_ref().unwrap();
        println!(
            "{}",
            json!({"phase":phase,"fan_count":fan_count,
                "source_reads":session.history.as_ref().unwrap().source_reads.get(),
                "serialized_bytes":serde_json::to_vec(&restored.points).unwrap().len(),
                "points":restored.points.len(),"oldest_fan_rpm":restored.points[0]["fan_rpm"]})
        );
    }

    fn follow_restoration_measure(phase: &str, sample: u32, work: impl FnOnce()) {
        let timebase = collector::clock::Timebase::read().unwrap();
        let pid = collector::processes::own_pid().unwrap();
        let before = collector::processes::read_usage(timebase, pid)
            .unwrap()
            .unwrap();
        let started = std::time::Instant::now();

        work();

        let wall_ns = started.elapsed().as_nanos();
        let after = collector::processes::read_usage(timebase, pid)
            .unwrap()
            .unwrap();
        let cpu_ns = (after.user_ns + after.system_ns) - (before.user_ns + before.system_ns);
        println!(
            "{}",
            json!({"phase":phase,"sample":sample,"wall_ns":wall_ns,"cpu_ns":cpu_ns,
            "resident_before_bytes":before.resident_bytes,
            "resident_after_bytes":after.resident_bytes,
            "footprint_before_bytes":before.footprint_bytes,
            "footprint_after_bytes":after.footprint_bytes})
        );
    }

    fn follow_restoration_read(root: &Path, phase: &str, output: &mut Value) {
        let mut session = LiveSession::open(root).unwrap();
        assert_eq!(session.sources.len(), 1);
        assert_eq!(session.sources[0].reader.shape().slot_count, 120);
        assert_eq!(session.sources[0].reader.shape().cpu_capacity, 18);
        assert_eq!(session.catalog()["windows"], 14);
        let cursor = session.sources[0].reader.next_publication();
        let event_cursor = session.sources[0].reader.next_event();
        let losses = session.sources[0].losses;

        *output = session.follow_latest().unwrap();
        let selected = session.selected.clone();
        let repeated = session.follow_latest().unwrap();

        assert_eq!(*output, repeated);
        assert_eq!(session.selected, selected);
        assert_eq!(session.selected.as_ref(), Some(&output["selected"]));
        assert_eq!(session.sources[0].reader.next_publication(), cursor);
        assert_eq!(session.sources[0].reader.next_event(), event_cursor);
        assert_eq!(session.sources[0].losses, losses);
        println!(
            "{}",
            json!({"phase":phase,"selected":output["selected"]["publication"],
            "context_count":output["trends"].as_array().unwrap().len(),
            "first_publication":output["trends"][0]["publication"],
            "last_publication":output["trends"].as_array().unwrap().last().unwrap()["publication"],
            "cursor":cursor,"event_cursor":event_cursor,
            "reader_losses":{"publications":losses.publications,"events":losses.events,
                "unknown":losses.unknown}})
        );
    }

    fn follow_restoration_assert_window(response: &Value) {
        let trends = response["trends"].as_array().unwrap();
        let publications = trends
            .iter()
            .map(|point| point["publication"].as_u64().unwrap())
            .collect::<Vec<_>>();
        // The charts' readings hold the lead-in before the 900-second context, which reaches
        // the first publication.
        let expected = (1..=901_u64).collect::<Vec<_>>();

        assert_eq!(response["selected"]["publication"], 901);
        assert_eq!(response["selected"]["run_tick"], 901);
        assert_eq!(
            response["selected"]["wall_ns"],
            1901 * NANOSECONDS_PER_SECOND
        );
        assert_eq!(response["context_seconds"], 900);
        assert_eq!(publications.len(), 901);
        assert_eq!(publications, expected);
        assert_eq!(trends[1]["cpu_percent"], 50.0);
        assert_eq!(trends[781]["publication"], 782);
        assert_eq!(trends[781]["cpu_percent"], 50.0);
        assert_eq!(trends[1]["memory_active_bytes"], 2 * 1024 * 1024);
        assert_eq!(response["summary"]["cpu_percent"], 50.0);
        for point in trends {
            assert_eq!(point["run"], response["selected"]["run"]);
            assert_eq!(point["boot"], response["selected"]["boot"]);
            assert_eq!(point["run_tick"], point["publication"]);
            assert_eq!(
                point["wall_ns"].as_u64().unwrap(),
                (1000 + point["publication"].as_u64().unwrap()) * NANOSECONDS_PER_SECOND
            );
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

    #[test]
    fn preserves_zero_full_and_independent_cpu_intervals_while_following() {
        // Goal: exact pinned CPU readings survive repeated queries and return to following.
        // Method: publish genuine zero/full and 25/75 intervals through the real ring.
        use collector::machine::CPUTicks;

        let mut fixture = RingFixture::new_with_cpu_capacity(2);
        fixture.publish_cpu_activity(1);
        let mut session = LiveSession::open(&fixture.root).unwrap();
        fixture.publish_cpu_readings(
            2,
            &[
                CPUTicks {
                    user: 25,
                    idle: 175,
                    ..CPUTicks::default()
                },
                CPUTicks {
                    user: 175,
                    idle: 25,
                    ..CPUTicks::default()
                },
            ],
        );
        session.poll().unwrap();
        fixture.publish_cpu_readings(
            3,
            &[
                CPUTicks {
                    user: 50,
                    idle: 250,
                    ..CPUTicks::default()
                },
                CPUTicks {
                    user: 250,
                    idle: 50,
                    ..CPUTicks::default()
                },
            ],
        );
        session.poll().unwrap();
        session.select(1002, 0).unwrap();

        let pinned = session.cpu_detail().unwrap();
        let repeated = session.cpu_detail().unwrap();
        session.follow_latest().unwrap();
        let latest = session.cpu_detail().unwrap();

        assert_eq!(pinned, repeated);
        assert_eq!(pinned["observation"]["publication"], 2);
        assert_eq!(
            pinned["cpus"],
            json!([
                {"cpu":0,"busy_percent":0.0}, {"cpu":1,"busy_percent":100.0}
            ])
        );
        assert_eq!(latest["observation"]["publication"], 3);
        assert_eq!(
            latest["cpus"],
            json!([
                {"cpu":0,"busy_percent":25.0}, {"cpu":1,"busy_percent":75.0}
            ])
        );
    }

    #[test]
    fn preserves_cpu_detail_across_live_and_sealed_sources() {
        // Goal: the bounded live and saved adapters derive the same selected CPU interval.
        // Method: seal two genuine ring publications and compare both public projections.
        let mut fixture = RingFixture::new_with_cpu_capacity(2);
        fixture.publish_cpu_activity(1);
        let mut live = LiveSession::open(&fixture.root).unwrap();
        let source = &live.sources[0];
        let mut window = Window::new(
            WindowIdentity {
                run: source.identity,
                window: 0,
            },
            Limits::from_shape(&source.reader.shape()).unwrap(),
        );
        cpu_detail_stage(&mut window, source, 1);
        fixture.publish_cpu_activity(2);
        live.poll().unwrap();
        cpu_detail_stage(&mut window, &live.sources[0], 2);
        let selection = live.select(1002, 0).unwrap();
        let cursor = live.sources[0].reader.next_publication();
        let event_cursor = live.sources[0].reader.next_event();
        let losses = live.sources[0].losses;

        let first = live.cpu_detail().unwrap();
        let repeated = live.cpu_detail().unwrap();
        seal_window(&fixture.root, &window);
        let mut saved = Session::open(&fixture.root).unwrap();
        saved.select(1002, 0).unwrap();
        let sealed = saved.cpu_detail().unwrap();

        assert_eq!(first, repeated);
        assert_eq!(
            first["cpus"],
            json!([{ "cpu":0,"busy_percent":25.0 }, {"cpu":1,"busy_percent":75.0}])
        );
        assert_eq!(
            sealed["cpus"],
            json!([
                {"cpu":0,"busy_percent":25.0}, {"cpu":1,"busy_percent":75.0}
            ])
        );
        assert_eq!(sealed["interval_available"], true);
        assert_eq!(live.selected.as_ref(), Some(&selection["selected"]));
        assert_eq!(live.sources[0].reader.next_publication(), cursor);
        assert_eq!(live.sources[0].reader.next_event(), event_cursor);
        assert_eq!(live.sources[0].losses, losses);
        assert_eq!(first["observation"]["run_tick"], 2);
        assert_eq!(first["previous_observation"]["run_tick"], 1);
    }

    #[test]
    fn gaps_cpu_detail_after_an_unsealed_predecessor_is_evicted() {
        // Goal: an evicted predecessor never causes full-process rehydration or invented rates.
        // Method: expire the first raw observation while keeping the selected second one.
        let mut fixture = RingFixture::new_with_cpu_capacity(2);
        fixture.publish_cpu_activity(1);
        let mut live = LiveSession::open(&fixture.root).unwrap();
        fixture.publish_cpu_activity(2);
        live.poll().unwrap();
        live.select(1002, 0).unwrap();
        let cursor = live.sources[0].reader.next_publication();
        let selected = live.selected.clone();
        live.poll_evict(1002 * NANOSECONDS_PER_SECOND).unwrap();

        let result = live.cpu_detail().unwrap();

        assert_eq!(result["interval_available"], false);
        assert_eq!(result["previous_observation"], Value::Null);
        assert_eq!(
            result["unavailable_reason"],
            "Previous CPU reading unavailable"
        );
        assert_eq!(
            result["cpus"],
            json!([{ "cpu":0,"busy_percent":null }, {"cpu":1,"busy_percent":null}])
        );
        assert_eq!(live.sources[0].reader.next_publication(), cursor);
        assert_eq!(live.selected, selected);
    }

    #[test]
    fn recovers_cpu_detail_predecessor_from_its_exact_commit() {
        // Goal: CPU-only committed queries fill a raw-cache gap without moving selection.
        // Method: seal two ring readings, evict the predecessor, then project the live selection.
        let mut fixture = RingFixture::new_with_cpu_capacity(2);
        fixture.publish_cpu_activity(1);
        let mut live = LiveSession::open(&fixture.root).unwrap();
        let source = &live.sources[0];
        let mut window = Window::new(
            WindowIdentity {
                run: source.identity,
                window: 0,
            },
            Limits::from_shape(&source.reader.shape()).unwrap(),
        );
        cpu_detail_stage(&mut window, source, 1);
        fixture.publish_cpu_activity(2);
        live.poll().unwrap();
        cpu_detail_stage(&mut window, &live.sources[0], 2);
        live.select(1002, 0).unwrap();
        seal_window(&fixture.root, &window);
        live.refresh_history().unwrap();
        live.poll_evict(1002 * NANOSECONDS_PER_SECOND).unwrap();
        let selected = live.selected.clone();
        let cursor = live.sources[0].reader.next_publication();

        let result = live.cpu_detail().unwrap();

        assert_eq!(result["interval_available"], true);
        assert_eq!(
            result["cpus"],
            json!([{ "cpu":0,"busy_percent":25.0 }, {"cpu":1,"busy_percent":75.0}])
        );
        assert_eq!(live.selected, selected);
        assert_eq!(live.sources[0].reader.next_publication(), cursor);
    }

    #[test]
    fn reads_cpu_detail_from_commits_without_changing_the_historical_selection() {
        // Goal: exact CPU recovery cannot rewrite a different historical selection.
        // Method: evict both live raw rows after sealing and leave history selected elsewhere.
        let mut fixture = RingFixture::new_with_cpu_capacity(2);
        fixture.publish_cpu_activity(1);
        let mut live = LiveSession::open(&fixture.root).unwrap();
        let source = &live.sources[0];
        let mut window = Window::new(
            WindowIdentity {
                run: source.identity,
                window: 0,
            },
            Limits::from_shape(&source.reader.shape()).unwrap(),
        );
        cpu_detail_stage(&mut window, source, 1);
        fixture.publish_cpu_activity(2);
        live.poll().unwrap();
        cpu_detail_stage(&mut window, &live.sources[0], 2);
        live.select(1002, 0).unwrap();
        seal_window(&fixture.root, &window);
        live.refresh_history().unwrap();
        live.history.as_mut().unwrap().select(1001, 0).unwrap();
        live.poll_evict(1003 * NANOSECONDS_PER_SECOND).unwrap();
        let selected = live.selected.clone();
        let history_before = live.history.as_ref().unwrap().page("cpus", 0).unwrap();

        let result = live.cpu_detail().unwrap();
        let history_after = live.history.as_ref().unwrap().page("cpus", 0).unwrap();

        assert_eq!(
            result["cpus"],
            json!([{ "cpu":0,"busy_percent":25.0 }, {"cpu":1,"busy_percent":75.0}])
        );
        assert_eq!(result["observation"]["publication"], 2);
        assert_eq!(result["observation"]["run_tick"], 2);
        assert_eq!(
            result["observation"]["wall_ns"],
            1002 * NANOSECONDS_PER_SECOND
        );
        assert_eq!(live.selected, selected);
        assert_eq!(history_before, history_after);
    }

    #[test]
    fn gaps_cpu_detail_when_its_committed_predecessor_has_no_wall_clock() {
        // Goal: a clockless committed predecessor leaves selected CPU values unavailable.
        // Method: seal a clockless first row, evict it from live cache, then query its successor.
        let mut fixture = RingFixture::new_with_cpu_capacity(2);
        fixture.publish_cpu_activity(1);
        let mut live = LiveSession::open(&fixture.root).unwrap();
        let source = &live.sources[0];
        let mut window = Window::new(
            WindowIdentity {
                run: source.identity,
                window: 0,
            },
            Limits::from_shape(&source.reader.shape()).unwrap(),
        );
        let times = source.tick.times.unwrap();
        window
            .push_tick(
                &persistence::Observation {
                    publication: 1,
                    wall_ns: None,
                    continuous_ns: Some(times.continuous_ns),
                    awake_ns: Some(times.awake_ns),
                },
                &source.tick,
                &[],
            )
            .unwrap();
        fixture.publish_cpu_activity(2);
        live.poll().unwrap();
        cpu_detail_stage(&mut window, &live.sources[0], 2);
        live.select(1002, 0).unwrap();
        seal_window(&fixture.root, &window);
        live.refresh_history().unwrap();
        live.poll_evict(1002 * NANOSECONDS_PER_SECOND).unwrap();
        let selected = live.selected.clone();
        let cursor = live.sources[0].reader.next_publication();
        let losses = live.sources[0].losses;

        let response = live.cpu_detail();
        let mut saved = Session::open(&fixture.root).unwrap();
        saved.select(1002, 0).unwrap();
        let saved_response = saved.cpu_detail();

        assert!(response.is_ok(), "{response:?}");
        assert!(saved_response.is_ok(), "{saved_response:?}");
        let saved_result = saved_response.unwrap();
        assert_eq!(saved_result["interval_available"], false);
        assert_eq!(saved_result["previous_observation"], Value::Null);
        assert_eq!(
            saved_result["cpus"],
            json!([
                {"cpu":0,"busy_percent":null}, {"cpu":1,"busy_percent":null}
            ])
        );
        let result = response.unwrap();
        assert_eq!(result["interval_available"], false);
        assert_eq!(result["previous_observation"], Value::Null);
        assert_eq!(
            result["unavailable_reason"],
            "Previous CPU reading unavailable"
        );
        assert_eq!(
            result["cpus"],
            json!([{ "cpu":0,"busy_percent":null }, {"cpu":1,"busy_percent":null}])
        );
        assert_eq!(live.selected, selected);
        assert_eq!(live.sources[0].reader.next_publication(), cursor);
        assert_eq!(live.sources[0].losses, losses);
    }

    fn cpu_detail_stage(window: &mut Window, source: &Source, publication: u64) {
        let times = source.tick.times.unwrap();
        window
            .push_tick(
                &persistence::Observation {
                    publication,
                    wall_ns: Some(times.wall_ns),
                    continuous_ns: Some(times.continuous_ns),
                    awake_ns: Some(times.awake_ns),
                },
                &source.tick,
                &[],
            )
            .unwrap();
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
                    wall_ns: seal_window_bounds(window, |value| value.wall_ns),
                    continuous_ns: seal_window_bounds(window, |value| value.continuous_ns),
                    awake_ns: seal_window_bounds(window, |value| value.awake_ns),
                    losses: Losses::default(),
                    data_directory: String::new(),
                    tables,
                },
            )
            .unwrap();
    }

    /// Bounds a sealed minute's times as the persister does, so a fixture's minutes carry
    /// the wall times that let a read skip them.
    fn seal_window_bounds(
        window: &Window,
        field: impl Fn(&persistence::Observation) -> Option<u64>,
    ) -> Option<persistence::TimeBounds> {
        let mut result: Option<persistence::TimeBounds> = None;
        for observation in window.observations() {
            let value = field(observation)?;
            result = Some(match result {
                Some(bounds) => persistence::TimeBounds {
                    min: bounds.min.min(value),
                    max: bounds.max.max(value),
                },
                None => persistence::TimeBounds {
                    min: value,
                    max: value,
                },
            });
        }
        result
    }

    #[test]
    fn keeps_null_context_gaps_when_optional_app_history_is_unreadable() {
        // Goal: missing saved facts remain explicit gaps beside independently available detail.
        // Method: discard compact facts after selection and withhold the committed data.
        let mut fixture = identity_fixture::Fixture::new();
        fixture.publish_with_system_rows(1001);
        fixture.publish_with_system_rows(1002);
        fixture.seal();
        let mut session = LiveSession::open(&fixture.root).unwrap();
        session.follow_latest().unwrap();
        session.app_history = History::new();
        let data = persistence::layout::base(&fixture.root, persistence::layout::Kind::Data);
        let withheld = fixture.root.join("withheld-data");
        std::fs::rename(&data, &withheld).unwrap();

        let history = session
            .app_history(KeyKind::Bundle, b"/Apps/Current.app")
            .unwrap();
        let detail = session
            .app_detail(KeyKind::Bundle, b"/Apps/Current.app", 0)
            .unwrap();

        assert_eq!(history["points"].as_array().unwrap().len(), 2);
        assert_eq!(history["points"][0]["source_available"], false);
        assert_eq!(history["points"][1]["resident_bytes"], Value::Null);
        assert_eq!(history["coverage"]["retention_limited"], true);
        assert!(history["restoration_error"].is_string());
        assert_eq!(detail["app"]["resident_bytes"]["known"], "11");
        std::fs::rename(&withheld, &data).unwrap();
    }

    #[test]
    fn refuses_structural_and_canceled_app_history_instead_of_degrading_it() {
        // Goal: degraded history cannot conceal conflicting identity or superseded work.
        // Method: submit structural/interruption errors at the fallback and cancel a public call.
        let mut fixture = identity_fixture::Fixture::new();
        fixture.publish_with_system_rows(1001);
        let control = Arc::new(Cancellation::default());
        let mut session = LiveSession::open_with(&fixture.root, control.clone(), 0).unwrap();
        session.follow_latest().unwrap();
        session
            .with_selected_apps(|sample| {
                let target = Target {
                    kind: KeyKind::Bundle,
                    bytes: b"/Apps/Current.app",
                };
                let structural = session.app_history_degraded(
                    &crate::live_app_response::Selected::ending_at(sample.selected).unwrap(),
                    &target,
                    Error::Bound("conflicting App history observation"),
                );
                assert!(matches!(structural, Err(Error::Bound(_))));
                let canceled = session.app_history_degraded(
                    &crate::live_app_response::Selected::ending_at(sample.selected).unwrap(),
                    &target,
                    std::io::Error::from(std::io::ErrorKind::Interrupted).into(),
                );
                assert!(matches!(canceled, Err(Error::Io(problem))
                if problem.kind() == std::io::ErrorKind::Interrupted));
                Ok(Value::Null)
            })
            .unwrap();
        control.cancel().unwrap();

        let result = session.app_history(KeyKind::Bundle, b"/Apps/Current.app");

        assert!(matches!(result, Err(Error::Io(problem))
            if problem.kind() == std::io::ErrorKind::Interrupted));
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
        session.connection.unregister("processes").unwrap();
        session.connection.unregister("events").unwrap();
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

#[cfg(test)]
#[path = "live_app_process_groups_history_tests.rs"]
pub(crate) mod app_process_groups_history_tests;
