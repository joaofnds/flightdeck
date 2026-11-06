use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use collector::recording::{bootstrap, identity_text, private};
use persistence::engine::{Engine, Row};
use persistence::retention::Retained;
use persistence::{Error, Manifest, Result, TimeBounds, read_committed, reread_committed};
use serde_json::{Value, json};

use crate::Cancellation;
use crate::cancellation::Operation;
use crate::data::{self, Table};
use crate::metrics;

pub(crate) const OBSERVATIONS_MAX: usize = 4096;
pub(crate) const PAGE_ROWS: u64 = 512;
const WINDOWS_MAX: usize = 65_536;
const NS: u64 = 1_000_000_000;
/// All context readers share this allowance for event-part inputs and retained query buffers.
const EVENT_QUERY_BYTES_MAX: usize = 256 * 1024 * 1024;

pub struct Session {
    root: PathBuf,
    pub(crate) windows: Vec<Manifest>,
    /// The wall-clock bounds of each window's readings, from its manifest, or from its ticks
    /// where the manifest holds none.
    wall_bounds: Vec<Option<TimeBounds>>,
    /// The older tail last assembled from these windows and the range it covers.
    earlier_tail: std::cell::RefCell<Option<(TailRange, Vec<Value>)>>,
    /// The context a select read last from these windows, which a select of the same context
    /// takes again without a read, since a session's sealed windows never change. It holds one
    /// context, of at most `OBSERVATIONS_MAX` observations.
    last_context: Option<LastContext>,
    /// Whether each window's files were verified, which its first read does.
    verified: Vec<AtomicBool>,
    connection: Engine,
    /// The connections a context's readers read through, one each, so their queries run at once.
    context_connections: Vec<Engine>,
    event_query_budget: Arc<persistence::resources::ByteBudget>,
    first: Option<u64>,
    last: Option<u64>,
    /// The readings with no known clock, none where the recording's catalog has not counted them.
    unknown: Option<u64>,
    /// How the session reaches its windows.
    registered: registered::Registered,
    selected: Option<Observation>,
    selected_context: Vec<crate::cpu_history_sample::Identity>,
    context_last_second: Option<u64>,
    pub(crate) cpu_history_cache: Option<crate::cpu_history::Cache>,
    // Separate from LiveSession's raw cache, bounded to one 8 MiB owned Sample.
    app_sample_cache: std::cell::RefCell<Option<committed_apps::Cache>>,
    app_history_cache: std::cell::RefCell<Option<app_history::Cache>>,
    app_process_history_cache: std::cell::RefCell<Option<app_history::Cache>>,
    app_history_ticks: std::cell::RefCell<app_history::Ticks>,
    app_history_kept_headers: std::cell::RefCell<app_history::KeptHeaders>,
    parts_read: std::cell::RefCell<Option<app_history::PartsRead>>,
    pub(crate) saved_history_context_preparations: std::cell::Cell<u64>,
    operation: Operation,
    #[cfg(test)]
    pub(crate) source_reads: Count,
    #[cfg(test)]
    pub(crate) parts_decodes: std::cell::Cell<u64>,
    #[cfg(test)]
    pub(crate) tick_queries: std::cell::Cell<u64>,
    #[cfg(test)]
    pub(crate) table_queries: Count,
    #[cfg(test)]
    pub(crate) kept_header_queries: std::cell::Cell<u64>,
    #[cfg(test)]
    pub(crate) before_cpu_history_install: Option<Box<dyn FnOnce()>>,
    #[cfg(test)]
    before_app_sample_install: std::cell::RefCell<Option<Box<dyn FnOnce()>>>,
    #[cfg(test)]
    before_app_history_install: std::cell::RefCell<Option<Box<dyn FnOnce()>>>,
    #[cfg(test)]
    pub(crate) after_process_groups_frame: std::cell::RefCell<Option<Box<dyn FnOnce()>>>,
}

/// What a session starts from, which `Session::reopen` found.
struct Listed<'root> {
    root: &'root Path,
    connection: Engine,
    event_query_budget: Arc<persistence::resources::ByteBudget>,
    registered: registered::Registered,
    operation: Operation,
}

/// The context a select read: its range, its observations, in no order, its points, in time
/// order, and the windows the read verified, which a select that takes it again rechecks.
#[derive(Clone)]
struct LastContext {
    range: ContextRange,
    observations: Vec<Observation>,
    points: Vec<Value>,
    windows: Vec<usize>,
}

/// A selected second and the last second of the context read with it, which runs past the
/// selected second where the recording holds later seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ContextSelection {
    second: u64,
    context_last_second: u64,
}

impl ContextSelection {
    /// The selection of `second` in a recording whose newest second is `newest_second`: its
    /// context holds up to `CONTEXT_AFTER_SECONDS` after it, and none past the newest second.
    #[must_use]
    pub(crate) fn of(second: u64, newest_second: Option<u64>) -> Self {
        let after = second.saturating_add(CONTEXT_AFTER_SECONDS);
        let context_last_second = newest_second.map_or(second, |newest| after.min(newest));

        Self {
            second,
            context_last_second: context_last_second.max(second),
        }
    }

    /// The selected second.
    pub(crate) fn second(self) -> u64 {
        self.second
    }

    /// The last second of the context read with the selected second.
    pub(crate) fn context_last_second(self) -> u64 {
        self.context_last_second
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ContextRange {
    pub first_ns: u64,
    pub end_ns: u64,
}

impl ContextRange {
    /// The wall clock range of the full-detail context that ends at `last_second`.
    pub(crate) fn ending_at(last_second: u64) -> Result<Self> {
        let end_ns = last_second
            .checked_add(1)
            .and_then(|end| end.checked_mul(NS))
            .ok_or(Error::Bound("context end"))?;

        Ok(Self {
            first_ns: last_second.saturating_sub(DETAIL_SECONDS - 1) * NS,
            end_ns,
        })
    }

    /// The wall clock range of the full-detail readings the charts of the context that ends at
    /// `last_second` draw: the context and the lead-in before it.
    pub(crate) fn charted_ending_at(last_second: u64) -> Result<Self> {
        let context = Self::ending_at(last_second)?;

        Ok(Self {
            first_ns: last_second.saturating_sub(CHARTED_SECONDS - 1) * NS,
            end_ns: context.end_ns,
        })
    }

    /// The points of `points`, in time order, from the range's first second on, which leaves
    /// out the lead-in the charts draw before the context.
    pub(crate) fn held_points(self, points: &[Value]) -> &[Value] {
        let first = points.partition_point(|point| {
            point["wall_ns"]
                .as_u64()
                .is_some_and(|wall| wall < self.first_ns)
        });

        &points[first..]
    }
}

#[derive(Clone)]
pub(crate) struct Observation {
    pub window: usize,
    pub publication: u64,
    pub run_tick: u64,
    pub wall: u64,
    pub continuous: Option<u64>,
}

impl Observation {
    pub(crate) fn from_row(table: &Table, row: &[Option<String>], window: usize) -> Result<Self> {
        let required = |name| {
            table
                .integer(row, name)?
                .ok_or(Error::Bound("missing tick field"))
        };
        Ok(Self {
            window,
            publication: required("publication")?,
            run_tick: required("run_tick")?,
            wall: required("wall_ns")?,
            continuous: table.integer(row, "continuous_ns")?,
        })
    }

    pub fn json(&self, windows: &[Manifest]) -> Value {
        let run = windows[self.window].identity.run;
        let losses = windows[self.window].losses;
        json!({"run": identity_text(run.run).to_string(),
            "boot": identity_text(run.boot).to_string(),
            "publication": self.publication, "run_tick": self.run_tick, "wall_ns": self.wall,
            "window_losses": {"publications": losses.publications, "events": losses.events,
                "unknown": losses.unknown}})
    }
}

/// The windows a session's reads verified. Only `Session::verified_windows` makes one, so a
/// reopen can never take a listed window for a verified one.
#[derive(Default)]
pub(crate) struct Verified(pub(super) Vec<Manifest>);

impl Session {
    /// Opens only existing committed history. Never creates, locks or repairs a recording.
    pub fn open(root: &Path) -> Result<Self> {
        Self::open_with(root, Arc::new(Cancellation::default()), 0)
    }

    pub fn open_with(root: &Path, cancellation: Arc<Cancellation>, epoch: u64) -> Result<Self> {
        Self::reopen(root, cancellation, epoch, &Verified::default())
    }

    /// Opens the recording again. It leaves each window's files to its first read, which verifies
    /// them unless an earlier session's read verified them. A sealed window's files never change,
    /// so rereading its proof shows its commitment stands, as `validate_commitment` does around
    /// every read.
    pub(crate) fn reopen(
        root: &Path,
        cancellation: Arc<Cancellation>,
        epoch: u64,
        earlier: &Verified,
    ) -> Result<Self> {
        let operation = cancellation.operation(epoch)?;
        persistence::query::validate_root(root)?;
        private::verify_directory(root)?;
        let event_query_budget = Arc::new(persistence::resources::ByteBudget::new(
            EVENT_QUERY_BYTES_MAX,
        ));
        let registered = registered::Registered::open(root, &event_query_budget, &operation)?;
        let connection = data::connection();
        operation.attach(&connection)?;
        let mut session = Self::listed(Listed {
            root,
            connection,
            event_query_budget,
            registered,
            operation,
        });
        session.reopen_registered(epoch, earlier)?;
        Ok(session)
    }

    /// A session of `listed`, with nothing read from its windows yet.
    fn listed(listed: Listed<'_>) -> Self {
        let Listed {
            root,
            connection,
            event_query_budget,
            registered,
            operation,
        } = listed;

        Self {
            root: root.to_owned(),
            windows: Vec::new(),
            verified: Vec::new(),
            connection,
            context_connections: (0..CONTEXT_READERS).map(|_| data::connection()).collect(),
            event_query_budget,
            first: None,
            last: None,
            unknown: Some(0),
            registered,
            wall_bounds: Vec::new(),
            earlier_tail: std::cell::RefCell::new(None),
            last_context: None,
            selected: None,
            selected_context: Vec::new(),
            context_last_second: None,
            cpu_history_cache: None,
            app_sample_cache: std::cell::RefCell::new(None),
            app_history_cache: std::cell::RefCell::new(None),
            app_process_history_cache: std::cell::RefCell::new(None),
            app_history_ticks: std::cell::RefCell::new(app_history::Ticks::new()),
            app_history_kept_headers: std::cell::RefCell::new(app_history::KeptHeaders::new()),
            parts_read: std::cell::RefCell::new(None),
            saved_history_context_preparations: std::cell::Cell::new(0),
            operation,
            #[cfg(test)]
            source_reads: Count::default(),
            #[cfg(test)]
            parts_decodes: std::cell::Cell::new(0),
            #[cfg(test)]
            tick_queries: std::cell::Cell::new(0),
            #[cfg(test)]
            table_queries: Count::default(),
            #[cfg(test)]
            kept_header_queries: std::cell::Cell::new(0),
            #[cfg(test)]
            before_cpu_history_install: None,
            #[cfg(test)]
            before_app_sample_install: std::cell::RefCell::new(None),
            #[cfg(test)]
            before_app_history_install: std::cell::RefCell::new(None),
            #[cfg(test)]
            after_process_groups_frame: std::cell::RefCell::new(None),
        }
    }

    /// Reads where a registered recording's clock begins and ends, and lists the windows of its
    /// newest span. A window an earlier session's read verified stays verified.
    fn reopen_registered(&mut self, epoch: u64, earlier: &Verified) -> Result<()> {
        self.operation.begin(epoch)?.run(|| {
            let ends = self.registered.ends(&self.root, &self.operation)?;
            self.unknown = self.registered.unknown_clock_ticks(&self.operation)?;
            if let Some(ends) = ends {
                self.first = Some(ends.first);
                self.last = Some(ends.last);
                self.load_span(ends.last)?;
                let verified = registered::verified_flags(&self.windows, &earlier.0);
                self.verified = verified;
            }

            Ok(())
        })
    }

    pub fn begin(&mut self, epoch: u64) -> Result<()> {
        if self.operation.epoch() != epoch {
            self.selected = None;
            self.app_sample_cache.get_mut().take();
            self.app_history_cache.get_mut().take();
            self.app_process_history_cache.get_mut().take();
        }
        self.operation = self.operation.begin(epoch)?;
        self.operation.attach(&self.connection)
    }

    /// The newest second the saved minutes hold.
    pub(crate) fn last_second(&self) -> Option<u64> {
        self.last
    }

    #[must_use]
    pub fn catalog(&self) -> Value {
        json!({"path": self.root, "first_second": self.first, "last_second": self.last,
            "windows": self.windows.len(), "unknown_clock_ticks": self.unknown,
            "tables": persistence::schema::TABLES.map(persistence::schema::Table::name)})
    }

    /// Every response belongs to one explicit saved second and optional repeated observation.
    pub fn select(&mut self, second: u64, alternative: usize) -> Result<Value> {
        self.select_context(ContextSelection::of(second, self.last), alternative)
    }

    /// Selects `selection`'s second with the context it names.
    pub(crate) fn select_context(
        &mut self,
        selection: ContextSelection,
        alternative: usize,
    ) -> Result<Value> {
        assert!(selection.second <= selection.context_last_second);
        assert!(selection.context_last_second - selection.second <= CONTEXT_AFTER_SECONDS);

        self.operation
            .begin(self.operation.epoch())?
            .run(|| self.select_inner(selection, alternative))
    }

    fn select_inner(&mut self, selection: ContextSelection, alternative: usize) -> Result<Value> {
        self.selected = None;
        self.context_last_second = None;
        self.app_sample_cache.get_mut().take();
        self.operation.check()?;
        self.load_span(selection.context_last_second)?;
        let range = ContextRange::charted_ending_at(selection.context_last_second)?;
        let LastContext {
            mut observations,
            points,
            ..
        } = self.select_inner_context(range)?;
        observations.sort_by_key(|value| (value.wall, value.window, value.publication));
        let alternatives: Vec<_> = observations
            .iter()
            .filter(|value| value.wall / NS == selection.second)
            .cloned()
            .collect();
        let chosen = if alternatives.is_empty() {
            None
        } else {
            Some(
                alternatives
                    .get(alternative)
                    .ok_or(Error::Bound("observation alternative"))?
                    .clone(),
            )
        };
        let summary = chosen
            .as_ref()
            .and_then(|selected| {
                points.iter().find(|point| {
                    let identity = selected.json(&self.windows);
                    if point["publication"].as_u64() != Some(selected.publication) {
                        return false;
                    }
                    if point["run"] != identity["run"] {
                        return false;
                    }
                    point["boot"] == identity["boot"]
                })
            })
            .cloned();
        let mut selected_context = Vec::new();
        crate::cpu_history::context(
            &points,
            ContextRange::ending_at(selection.context_last_second)?,
            &mut selected_context,
        )?;
        self.selected_context = selected_context;
        self.selected = chosen;
        self.context_last_second = Some(selection.context_last_second);
        let alternative_identities: Vec<_> = alternatives
            .iter()
            .map(|value| value.json(&self.windows))
            .collect();
        Ok(
            json!({"second": selection.second, "context_last_second": selection.context_last_second,
            "selected": self.selected.as_ref().map(|value| value.json(&self.windows)),
            "alternatives": alternative_identities,
            "summary": summary, "trends": points, "context_seconds": 900}),
        )
    }

    /// The context of `range`, read from the sealed windows unless the last select read it. A
    /// context taken again rechecks every window its read verified, so a window evicted since
    /// fails the select as a read of it would.
    fn select_inner_context(&mut self, range: ContextRange) -> Result<LastContext> {
        if let Some(last) = self.select_inner_context_kept(range) {
            for &window in &last.windows {
                self.validate_commitment(window)?;
            }
            return Ok(last.clone());
        }

        let mut points = Vec::new();
        let observations = self.context_read(range, &mut points)?;
        let windows = self.select_inner_context_windows(range, &observations);
        let last = LastContext {
            range,
            observations,
            points,
            windows,
        };

        self.last_context = Some(last.clone());
        Ok(last)
    }

    /// The context of `range` the last select read, unless a reading of it has expired since.
    fn select_inner_context_kept(&self, range: ContextRange) -> Option<&LastContext> {
        let last = self
            .last_context
            .as_ref()
            .filter(|last| last.range == range)?;
        let retained = self.reader().retained();
        let holds_every = last.observations.iter().all(|observation| {
            let boot = self.windows[observation.window].identity.run.boot;
            retained.holds(boot, observation.continuous)
        });

        holds_every.then_some(last)
    }

    /// The windows a read of `range` verified: those that may hold its readings, and those holding
    /// the reading before the first of each window's `observations`, which its first rates come
    /// from.
    fn select_inner_context_windows(
        &self,
        range: ContextRange,
        observations: &[Observation],
    ) -> Vec<usize> {
        let mut firsts: BTreeMap<usize, &Observation> = BTreeMap::new();
        for observation in observations {
            let first = firsts.entry(observation.window).or_insert(observation);
            if observation.publication < first.publication {
                *first = observation;
            }
        }
        let reader = self.reader();
        let retained = reader.retained();
        let mut windows: BTreeSet<usize> =
            self.context_windows(range, &retained).into_iter().collect();
        for first in firsts.values() {
            windows.extend(reader.predecessor_windows(first));
        }

        windows.into_iter().collect()
    }

    /// Selects the observation of `second` that `named` picks out of its JSON, with its summary,
    /// or none where the saved minutes hold no such observation. Unlike `select` it builds no
    /// context, so it reads the observation's window and its predecessor's, and it leaves the
    /// context the last selection read. The summary carries no segment, which only a context
    /// numbers.
    pub(crate) fn inspect(
        &mut self,
        second: u64,
        named: impl Fn(&Value) -> bool,
    ) -> Result<Option<Value>> {
        self.operation
            .begin(self.operation.epoch())?
            .run(|| self.inspect_inner(second, named))
    }

    fn inspect_inner(
        &mut self,
        second: u64,
        named: impl Fn(&Value) -> bool,
    ) -> Result<Option<Value>> {
        self.selected = None;
        self.app_sample_cache.get_mut().take();
        self.load_span(second)?;
        let end = second
            .checked_add(1)
            .and_then(|value| value.checked_mul(NS))
            .ok_or(Error::Bound("selection time"))?;

        let observations = self.observations(ContextRange {
            first_ns: second * NS,
            end_ns: end,
        })?;
        let Some(chosen) = observations
            .into_iter()
            .find(|observation| named(&observation.json(&self.windows)))
        else {
            return Ok(None);
        };

        let selected = chosen.json(&self.windows);
        let mut tables = metrics::Tables::new();
        let mut points =
            metrics::trends(&self.reader(), std::slice::from_ref(&chosen), &mut tables)?;
        assert_eq!(points.len(), 1);
        let mut summary = points.swap_remove(0);
        summary
            .as_object_mut()
            .ok_or(Error::Bound("point shape"))?
            .remove("segment");
        self.selected = Some(chosen);
        Ok(Some(
            json!({"second": second, "selected": selected, "summary": summary}),
        ))
    }

    pub(crate) fn context(&self, range: ContextRange, output: &mut Vec<Value>) -> Result<()> {
        assert!(range.first_ns < range.end_ns);
        assert!(output.is_empty());

        self.operation.attach(&self.connection)?;
        self.operation
            .run(|| self.context_read(range, output).map(|_observations| ()))
    }

    /// The observations of `range`, in no order, with their points in `output`, in time order.
    /// Up to `CONTEXT_READERS` threads read them, each a contiguous run of the windows that may
    /// hold them through a connection of its own, so the reads wait on each other no more than
    /// the file system makes them.
    fn context_read(
        &self,
        range: ContextRange,
        output: &mut Vec<Value>,
    ) -> Result<Vec<Observation>> {
        assert!(range.first_ns < range.end_ns);
        assert!(output.is_empty());
        assert_eq!(self.context_connections.len(), CONTEXT_READERS);
        self.span_check(range)?;

        let retained = self.reader().retained();
        let windows = self.context_windows(range, &retained);
        let chunk = context_chunk(windows.len());
        assert!(context_reader_count(windows.len()) <= self.context_connections.len());
        let parts: Vec<&[usize]> = windows.chunks(chunk).collect();
        assert_eq!(parts.len(), context_reader_count(windows.len()));
        let mut observations = Vec::new();
        let read = |reader: &Reader<'_>, part| context_read_part(reader, part, range, &retained);
        for part in self.context_readers("flightdeck-context", parts, read)? {
            let (mut part_observations, mut part_points) = part?;
            if observations.len() + part_observations.len() > OBSERVATIONS_MAX {
                return Err(Error::Bound("context observations"));
            }
            observations.append(&mut part_observations);
            output.append(&mut part_points);
        }
        assert_eq!(observations.len(), output.len());

        context_points_order(output);
        metrics::context_segments(output)?;
        Ok(observations)
    }

    /// Reads each of `parts` with `read` on a thread named `name` and a context connection of its
    /// own, so the reads wait on each other no more than the file system makes them, and gives
    /// each part's result in the order of `parts`.
    pub(crate) fn context_readers<P: Send, T: Send>(
        &self,
        name: &str,
        parts: Vec<P>,
        read: impl Fn(&Reader<'_>, P) -> Result<T> + Sync,
    ) -> Result<Vec<Result<T>>> {
        assert!(parts.len() <= self.context_connections.len());

        let count = parts.len();
        let read = &read;
        let results = std::thread::scope(|scope| -> Result<Vec<Result<T>>> {
            let mut readers = Vec::with_capacity(CONTEXT_READERS);
            for (part, connection) in parts.into_iter().zip(&self.context_connections) {
                let reader = self.reader_through(connection);
                let observation = persistence::work::ingestion::inherit_current();
                let thread = std::thread::Builder::new()
                    .name(name.to_owned())
                    .stack_size(CONTEXT_READER_STACK_BYTES)
                    .spawn_scoped(scope, move || {
                        let observation = observation.enter();
                        let result = read(&reader, part);
                        drop(observation);
                        result
                    })?;
                readers.push(thread);
            }

            let mut results = Vec::with_capacity(readers.len());
            for thread in readers {
                let result = thread
                    .join()
                    .unwrap_or_else(|panic| std::panic::resume_unwind(panic));
                results.push(result);
            }
            Ok(results)
        })?;

        assert_eq!(results.len(), count);
        Ok(results)
    }

    /// The windows whose readings may fall in `range`: those whose wall-clock bounds overlap it,
    /// and those whose manifest holds no bounds.
    fn context_windows(&self, range: ContextRange, retained: &Retained) -> Vec<usize> {
        let mut windows = Vec::new();
        for (window, manifest) in self.windows.iter().enumerate() {
            if let Some(bounds) = manifest.wall_ns {
                if bounds.max < range.first_ns {
                    continue;
                }
                if bounds.min >= range.end_ns {
                    continue;
                }
            }
            if window_may_hold_retained(manifest, retained) {
                windows.push(window);
            }
        }

        windows
    }

    pub fn cpu_detail(&self) -> Result<Value> {
        self.operation.run(|| {
            let selected = self
                .selected
                .as_ref()
                .ok_or(Error::Bound("no selected observation"))?;
            self.selected_retained(selected)?;
            let mut current_slot = None;
            let mut previous_slot = None;
            self.cpu_detail_sample(selected, &mut current_slot)?;
            let current = current_slot
                .as_ref()
                .ok_or(Error::Bound("missing selected CPU reading"))?;
            if let Some(publication) = selected.publication.checked_sub(1) {
                self.cpu_detail_publication(&current.identity, publication, &mut previous_slot)?;
            }

            crate::cpu_detail::project(current, previous_slot.as_ref())
        })
    }

    pub(crate) fn cpu_detail_publication(
        &self,
        identity: &Value,
        publication: u64,
        output: &mut Option<crate::cpu_detail::Sample>,
    ) -> Result<()> {
        assert!(output.is_none());

        for (window, manifest) in self.windows.iter().enumerate() {
            if identity["run"] != identity_text(manifest.identity.run.run).to_string() {
                continue;
            }

            if identity["boot"] != identity_text(manifest.identity.run.boot).to_string() {
                continue;
            }

            if !(manifest.publication_first..manifest.publication_end).contains(&publication) {
                continue;
            }

            let _observation = self.source_scope(window, Some("ticks"));
            let source = self.source(window, "ticks")?;
            let table = data::query(
                &self.connection,
                &format!("SELECT * FROM {source} WHERE publication={publication} LIMIT 2"),
                1,
                &self.operation,
            )?;
            if let Some(row) = table.rows.first() {
                if table.integer(row, "wall_ns")?.is_none() {
                    return Ok(());
                }

                let observation = Observation::from_row(&table, row, window)?;
                self.cpu_detail_sample(&observation, output)?;

                return Ok(());
            }
        }

        Ok(())
    }

    fn cpu_detail_sample(
        &self,
        observation: &Observation,
        output: &mut Option<crate::cpu_detail::Sample>,
    ) -> Result<()> {
        assert!(output.is_none());

        let _observation = self.source_scope(observation.window, Some("cpus"));
        let source = self.source(observation.window, "cpus")?;
        let publication = observation.publication;
        let limit = crate::cpu_detail::CPUS_MAX + 1;
        let cpus = data::query(
            &self.connection,
            &format!("SELECT * FROM {source} WHERE publication={publication} LIMIT {limit}"),
            crate::cpu_detail::CPUS_MAX,
            &self.operation,
        )?;

        *output = Some(crate::cpu_detail::Sample {
            identity: observation.json(&self.windows),
            observation: observation.clone(),
            cpus: crate::cpu_detail::Source::Saved(cpus),
        });

        Ok(())
    }

    /// Refuses a further read of a selected reading the retention period no longer covers, so a
    /// hold keeps what it already holds in memory and pins nothing on disk.
    pub(crate) fn selected_retained(&self, selected: &Observation) -> Result<()> {
        let boot = self.windows[selected.window].identity.run.boot;
        if !self.reader().retained().holds(boot, selected.continuous) {
            return Err(Error::SecondExpired);
        }

        Ok(())
    }

    pub fn page(&self, name: &str, offset: u64) -> Result<Value> {
        self.operation.run(|| self.page_inner(name, offset))
    }

    fn page_inner(&self, name: &str, offset: u64) -> Result<Value> {
        self.operation.check()?;
        let selected = self
            .selected
            .as_ref()
            .ok_or(Error::Bound("no selected observation"))?;
        self.selected_retained(selected)?;
        let order = persistence::schema::TABLES
            .into_iter()
            .find(|table| table.name() == name)
            .ok_or(Error::Bound("table name"))?
            .order();
        if !offset.is_multiple_of(PAGE_ROWS) {
            return Err(Error::Bound("page offset"));
        }
        let _observation = self.source_scope(selected.window, Some(name));
        let source = self.source(selected.window, name)?;
        let publication = selected.publication;
        let count = self.connection.query(&format!(
            "SELECT count(*) FROM {source} WHERE publication={publication}"
        ))?;
        let total: u64 = Row::new(&count, 0)?.get(0)?;
        if offset > total {
            return Err(Error::Bound("page past end"));
        }
        let table = data::query(
            &self.connection,
            &format!(
                "SELECT * FROM {source} WHERE publication={publication} ORDER BY {order} \
            LIMIT {PAGE_ROWS} OFFSET {offset}"
            ),
            usize::try_from(PAGE_ROWS).map_err(|_| Error::Bound("page size"))?,
            &self.operation,
        )?;
        Ok(
            json!({"name": name, "columns": table.columns, "rows": table.rows,
            "offset": offset, "total": total, "observation": selected.json(&self.windows)}),
        )
    }

    pub(crate) fn source(&self, window: usize, name: &str) -> Result<String> {
        self.reader().source(window, name)
    }

    pub(crate) fn source_scope(
        &self,
        window: usize,
        table: Option<&str>,
    ) -> Option<persistence::work::ingestion::Scope> {
        self.reader().source_scope(window, table)
    }

    fn validate_commitment(&self, window: usize) -> Result<()> {
        self.reader().validate_commitment(window)
    }

    /// The windows this session's reads verified, which a reopen need not verify again.
    pub(crate) fn verified_windows(&self) -> Verified {
        assert_eq!(self.verified.len(), self.windows.len());
        Verified(
            self.windows
                .iter()
                .zip(&self.verified)
                .filter(|(_, flag)| flag.load(Ordering::Acquire))
                .map(|(manifest, _)| manifest.clone())
                .collect(),
        )
    }

    pub(crate) fn table(&self, window: usize, name: &str) -> Result<Table> {
        self.reader().table(window, name)
    }

    pub(crate) fn predecessor_windows_holding(
        &self,
        window: usize,
        publication: u64,
    ) -> Vec<usize> {
        self.reader()
            .predecessor_windows_holding(window, publication)
    }

    /// A reader of this session's windows through the session's connection.
    pub(crate) fn reader(&self) -> Reader<'_> {
        self.reader_through(&self.connection)
    }

    fn reader_through<'session>(&'session self, connection: &'session Engine) -> Reader<'session> {
        Reader {
            root: &self.root,
            windows: &self.windows,
            verified: &self.verified,
            connection,
            event_query_budget: &self.event_query_budget,
            operation: &self.operation,
            #[cfg(test)]
            source_reads: &self.source_reads,
            #[cfg(test)]
            table_queries: &self.table_queries,
        }
    }

    fn observations(&self, range: ContextRange) -> Result<Vec<Observation>> {
        self.span_check(range)?;
        let reader = self.reader();
        let retained = reader.retained();
        let mut observations = Vec::new();
        for window in self.context_windows(range, &retained) {
            reader.window_observations(window, range, &retained, &mut observations)?;
        }
        Ok(observations)
    }
}

/// Whether the window of `manifest` may hold a reading to return. One that holds none is read for
/// nothing but the rates of the reading after it, since removal may have taken it from under a
/// list that still names it.
fn window_may_hold_retained(manifest: &Manifest, retained: &Retained) -> bool {
    retained.holds_any(manifest.identity.run.boot, manifest.continuous_ns)
}

/// Whether `root` has anywhere saved history could be, which one no writer finished setting up has
/// not yet. A root of another format or a damaged one counts, so that opening it says what is
/// wrong.
pub(crate) fn saved_history_exists(root: &Path) -> io::Result<bool> {
    let directory = private::PrivateDirectory::open(root)?;
    let classification = bootstrap::classify_settled(&directory, &mut [0; 4097])?;

    Ok(!matches!(
        classification,
        bootstrap::Classification::Fresh | bootstrap::Classification::Initializing(_)
    ))
}

/// Threads that read a context's windows. On an M5 Pro the newest sixteen minutes of a
/// recording took 280 ms to read on one thread, about 120 ms on six, and no less on more.
pub(crate) const CONTEXT_READERS: usize = 6;

/// The stack of each thread that reads part of a context, the stack the desktop's live worker
/// gives the same reads when they run on its own thread.
const CONTEXT_READER_STACK_BYTES: usize = 4 * 1024 * 1024;

/// The windows each reader of a context of `window_count` windows reads, the last reader
/// reading those left.
const fn context_chunk(window_count: usize) -> usize {
    let chunk = window_count.div_ceil(CONTEXT_READERS);
    if chunk == 0 { 1 } else { chunk }
}

/// The readers a context of `window_count` windows starts.
pub(crate) const fn context_reader_count(window_count: usize) -> usize {
    window_count.div_ceil(context_chunk(window_count))
}

/// The sealed windows of a session and a connection to read them through. Each thread that
/// reads a context holds a reader of a connection of its own, so the threads share the windows,
/// their verification and the operation, and wait on no one's queries.
pub(crate) struct Reader<'session> {
    root: &'session Path,
    pub(crate) windows: &'session [Manifest],
    verified: &'session [AtomicBool],
    connection: &'session Engine,
    event_query_budget: &'session Arc<persistence::resources::ByteBudget>,
    operation: &'session Operation,
    #[cfg(test)]
    source_reads: &'session Count,
    #[cfg(test)]
    table_queries: &'session Count,
}

impl Reader<'_> {
    fn source_scope(
        &self,
        window: usize,
        table: Option<&str>,
    ) -> Option<persistence::work::ingestion::Scope> {
        if !persistence::work::ingestion::is_active() {
            return None;
        }

        self.windows.get(window).map(|manifest| {
            persistence::work::ingestion::minute_source(
                manifest,
                table.and_then(persistence::work::ingestion::table_name),
            )
        })
    }

    pub(crate) fn source(&self, window: usize, name: &str) -> Result<String> {
        self.operation.check()?;
        #[cfg(test)]
        self.source_reads.increment();
        let table = persistence::schema::TABLES
            .into_iter()
            .find(|table| table.name() == name)
            .ok_or(Error::Bound("table name"))?;
        self.validate_commitment(window)?;
        let manifest = &self.windows[window];
        if name == "events" {
            let _observation =
                persistence::work::ingestion::minute_source(manifest, Some("events"));
            return self
                .connection
                .event_source(self.root, manifest, self.event_query_budget);
        }

        let path = self
            .root
            .join(&manifest.data_directory)
            .join(format!("{name}.parquet"));
        let _observation =
            persistence::work::ingestion::minute_source(manifest, Some(table.name()));
        persistence::query::table_source(
            self.connection,
            table,
            persistence::query::TableSource::Parquet(&path),
        )
    }

    /// Checks that a window the session listed is still committed unchanged. Readers call it
    /// around every read, so it verifies the window's files on the first and rereads the proof
    /// after. Two readers may both verify a window, which reads its files twice and changes
    /// nothing.
    fn validate_commitment(&self, window: usize) -> Result<()> {
        self.operation.check()?;

        assert_eq!(self.verified.len(), self.windows.len());
        let manifest = &self.windows[window];
        let marker = persistence::commitment_path(self.root, manifest);
        let flag = &self.verified[window];
        let committed = if flag.load(Ordering::Acquire) {
            reread_committed(self.root, &marker)?
        } else {
            read_committed(self.root, &marker)?
        };
        if committed != *manifest {
            return Err(Error::Bound("changed commitment"));
        }

        flag.store(true, Ordering::Release);
        Ok(())
    }

    pub(crate) fn table(&self, window: usize, name: &str) -> Result<Table> {
        let _observation = self.source_scope(window, Some(name));
        #[cfg(test)]
        self.table_queries.increment();
        if name == metrics::core_frequency::CORE_LEVEL_STATES {
            let sources = metrics::core_frequency::CoreLevelSources {
                core_states: &self.source(window, "core_states")?,
                ticks: &self.source(window, "ticks")?,
            };
            return metrics::core_frequency::core_level_states(
                self.connection,
                &sources,
                self.operation,
            );
        }
        let order = if name == "fans" {
            "publication, fan"
        } else {
            "publication"
        };
        data::query(
            self.connection,
            &format!(
                "SELECT * FROM {} ORDER BY {order}",
                self.source(window, name)?
            ),
            data::ROWS_MAX,
            self.operation,
        )
    }

    /// What the retention period still covers of the readings of these windows.
    pub(crate) fn retained(&self) -> Retained {
        Retained::read(
            self.root,
            self.windows
                .iter()
                .map(|manifest| manifest.identity.run.boot),
        )
    }

    /// Appends to `observations` the readings of `window` that fall in `range` and that `retained`
    /// holds. An expired reading stays readable through the tables, where the reading after it
    /// takes its rates from it, and only this list says which readings a read returns.
    fn window_observations(
        &self,
        window: usize,
        range: ContextRange,
        retained: &Retained,
        observations: &mut Vec<Observation>,
    ) -> Result<()> {
        let _observation = self.source_scope(window, Some("ticks"));
        let source = self.source(window, "ticks")?;
        let ContextRange { first_ns, end_ns } = range;
        let ticks = data::query(
            self.connection,
            &format!("SELECT * FROM {source} WHERE wall_ns>={first_ns} AND wall_ns<{end_ns}"),
            OBSERVATIONS_MAX,
            self.operation,
        )?;
        if observations.len() + ticks.rows.len() > OBSERVATIONS_MAX {
            return Err(Error::Bound("context observations"));
        }

        let boot = self.windows[window].identity.run.boot;
        for row in &ticks.rows {
            let observation = Observation::from_row(&ticks, row, window)?;
            if retained.holds(boot, observation.continuous) {
                observations.push(observation);
            }
        }
        Ok(())
    }

    /// The windows of `observation`'s run whose publications include the one before it.
    pub(crate) fn predecessor_windows(&self, observation: &Observation) -> Vec<usize> {
        let Some(publication) = observation.publication.checked_sub(1) else {
            return Vec::new();
        };

        self.predecessor_windows_holding(observation.window, publication)
    }

    /// The windows of the run of `window` whose publications include `publication`.
    pub(crate) fn predecessor_windows_holding(
        &self,
        window: usize,
        publication: u64,
    ) -> Vec<usize> {
        let run = self.windows[window].identity.run;
        let mut windows = Vec::new();
        for (window, manifest) in self.windows.iter().enumerate() {
            if manifest.identity.run == run {
                if (manifest.publication_first..manifest.publication_end).contains(&publication) {
                    windows.push(window);
                }
            }
        }

        windows
    }
}

/// The observations of `range` that `windows` hold and their points, read through `reader`.
/// The points of a window's first readings take their rates from the window before, which a
/// reader reads where another reader holds it.
fn context_read_part(
    reader: &Reader<'_>,
    windows: &[usize],
    range: ContextRange,
    retained: &Retained,
) -> Result<(Vec<Observation>, Vec<Value>)> {
    let mut tables = metrics::Tables::new();
    let mut observations = Vec::new();
    let mut points = Vec::new();
    let mut batch = Vec::new();
    for &window in windows {
        batch.clear();
        reader.window_observations(window, range, retained, &mut batch)?;
        if !batch.is_empty() {
            if observations.len() + batch.len() > OBSERVATIONS_MAX {
                return Err(Error::Bound("context observations"));
            }
            points.append(&mut metrics::trends(reader, &batch, &mut tables)?);
            observations.append(&mut batch);
        }
    }

    assert_eq!(observations.len(), points.len());
    Ok((observations, points))
}

/// A count tests read of the reads a session makes, which its readers make from several threads.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct Count(std::sync::atomic::AtomicU64);

#[cfg(test)]
impl Count {
    pub(crate) fn get(&self) -> u64 {
        self.0.load(Ordering::Acquire)
    }

    fn increment(&self) {
        self.0.fetch_add(1, Ordering::AcqRel);
    }
}

/// Orders context points by time, then by run, boot and publication.
fn context_points_order(points: &mut [Value]) {
    points.sort_by(|left, right| {
        (
            left["wall_ns"].as_u64(),
            left["run"].as_str(),
            left["boot"].as_str(),
            left["publication"].as_u64(),
        )
            .cmp(&(
                right["wall_ns"].as_u64(),
                right["run"].as_str(),
                right["boot"].as_str(),
                right["publication"].as_u64(),
            ))
    });
}

pub(crate) fn partition(table: &Table) -> Result<BTreeMap<u64, Table>> {
    let mut result = BTreeMap::new();
    for row in &table.rows {
        let publication = table
            .integer(row, "publication")?
            .ok_or(Error::Bound("publication"))?;
        result
            .entry(publication)
            .or_insert_with(|| Table {
                columns: table.columns.clone(),
                rows: Vec::new(),
            })
            .rows
            .push(row.clone());
    }
    Ok(result)
}

#[path = "cpu_history_saved.rs"]
mod cpu_history_saved;

mod app_history;
pub(crate) use app_history::AttributionReplay;
pub(crate) use app_history::ProcessGroupsReplay;
pub(crate) use app_history::SourceFileKind;
pub(crate) use app_history::{KeptAttribution, KeptResident};
mod committed_apps;
#[cfg(test)]
mod context_tests;
mod earlier;
mod earlier_reach;
pub(crate) mod registered;
#[cfg(test)]
mod registered_tests;
#[cfg(test)]
pub(crate) mod reopen_tests;
#[cfg(test)]
pub(crate) mod retention_tests;
mod selection;
pub use earlier::{
    CHARTED_SECONDS, CONTEXT_AFTER_SECONDS, EARLIER_STRIDE_SECONDS, LEAD_IN_SECONDS, LOADED_SECONDS,
};
pub(crate) use earlier::{DETAIL_SECONDS, KeptTail, TailRange};
