use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use collector::recording::{identity_text, private};
use collector::ring::Identity;
use persistence::engine::{Engine, Row};
use persistence::layout::{self, Kind};
use persistence::{Error, Manifest, Result, list_committed, read_committed, reread_committed};
use serde_json::{Value, json};

use crate::Cancellation;
use crate::cancellation::Operation;
use crate::data::{self, Table};
use crate::metrics;

pub(crate) const OBSERVATIONS_MAX: usize = 4096;
pub(crate) const PAGE_ROWS: u64 = 512;
const WINDOWS_MAX: usize = 65_536;
const NS: u64 = 1_000_000_000;

pub struct Session {
    root: PathBuf,
    pub(crate) windows: Vec<Manifest>,
    /// Whether each window's files were verified, which its first read does.
    verified: Vec<std::cell::Cell<bool>>,
    connection: Engine,
    first: Option<u64>,
    last: Option<u64>,
    unknown: u64,
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
    pub(crate) source_reads: std::cell::Cell<u64>,
    #[cfg(test)]
    pub(crate) parts_decodes: std::cell::Cell<u64>,
    #[cfg(test)]
    pub(crate) tick_queries: std::cell::Cell<u64>,
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

#[derive(Clone, Copy)]
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
    fn from_row(table: &Table, row: &[Option<String>], window: usize) -> Result<Self> {
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

    pub fn json(&self, session: &Session) -> Value {
        let run = session.windows[self.window].identity.run;
        let losses = session.windows[self.window].losses;
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
pub(crate) struct Verified(Vec<Manifest>);

impl Session {
    /// Opens only existing committed history. Never creates, locks or repairs a recording.
    pub fn open(root: &Path) -> Result<Self> {
        Self::open_with(root, Arc::new(Cancellation::default()), 0)
    }

    pub fn open_with(root: &Path, cancellation: Arc<Cancellation>, epoch: u64) -> Result<Self> {
        Self::reopen(root, cancellation, epoch, &Verified::default())
    }

    /// Opens the recording again. It reads each window's marker and leaves the window's files
    /// to its first read, which verifies them unless an earlier session's read verified them.
    /// A sealed window's files never change, so rereading its marker shows its commitment
    /// stands, as `validate_commitment` does around every read.
    pub(crate) fn reopen(
        root: &Path,
        cancellation: Arc<Cancellation>,
        epoch: u64,
        earlier: &Verified,
    ) -> Result<Self> {
        let operation = cancellation.operation(epoch)?;
        persistence::query::validate_root(root)?;
        private::verify_directory(root)?;
        let recording = collector::recording::read_identity(root)?;
        let mut earlier_markers = BTreeMap::new();
        for manifest in &earlier.0 {
            if manifest.identity.run.recording == recording {
                let marker = layout::WindowKey::from(&manifest.identity).marker(root);
                earlier_markers.insert(marker, manifest);
            }
        }
        let mut windows = Vec::new();
        discover(root, recording, &mut windows, &operation)?;
        let verified = windows
            .iter()
            .map(|manifest| {
                let marker = layout::WindowKey::from(&manifest.identity).marker(root);
                let kept = earlier_markers.get(&marker).copied();
                std::cell::Cell::new(kept == Some(manifest))
            })
            .collect();
        let connection = data::connection();
        operation.attach(&connection)?;
        let mut session = Self {
            root: root.to_owned(),
            windows,
            verified,
            connection,
            first: None,
            last: None,
            unknown: 0,
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
            source_reads: std::cell::Cell::new(0),
            #[cfg(test)]
            parts_decodes: std::cell::Cell::new(0),
            #[cfg(test)]
            tick_queries: std::cell::Cell::new(0),
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
        };
        session.operation.begin(epoch)?.run(|| session.extent())?;
        Ok(session)
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
            "tables": persistence::schema::TABLES.map(|entry| entry.0.name())})
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
        let range = ContextRange::charted_ending_at(selection.context_last_second)?;
        let mut observations = self.observations(range.first_ns, range.end_ns)?;
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
        let mut points = Vec::new();
        self.context_points(&observations, &mut points)?;
        let summary = chosen
            .as_ref()
            .and_then(|selected| {
                points.iter().find(|point| {
                    let identity = selected.json(self);
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
        let context = ContextRange::ending_at(selection.context_last_second)?;
        crate::cpu_history::context(context.held_points(&points), &mut selected_context)?;
        self.selected_context = selected_context;
        self.selected = chosen;
        self.context_last_second = Some(selection.context_last_second);
        Ok(
            json!({"second": selection.second, "context_last_second": selection.context_last_second,
            "selected": self.selected.as_ref().map(|value| value.json(self)),
            "alternatives": alternatives.iter().map(|value| value.json(self)).collect::<Vec<_>>(),
            "summary": summary, "trends": points, "context_seconds": 900}),
        )
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
        let end = second
            .checked_add(1)
            .and_then(|value| value.checked_mul(NS))
            .ok_or(Error::Bound("selection time"))?;

        let observations = self.observations(second * NS, end)?;
        let Some(chosen) = observations
            .into_iter()
            .find(|observation| named(&observation.json(self)))
        else {
            return Ok(None);
        };

        let selected = chosen.json(self);
        let mut points = Vec::with_capacity(1);
        self.context_points(std::slice::from_ref(&chosen), &mut points)?;
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
        self.operation.run(|| {
            let observations = self.observations(range.first_ns, range.end_ns)?;
            self.context_points(&observations, output)
        })
    }

    fn context_points(&self, observations: &[Observation], output: &mut Vec<Value>) -> Result<()> {
        assert!(observations.len() <= OBSERVATIONS_MAX);
        assert!(output.is_empty());

        let mut windows = BTreeMap::<usize, Vec<Observation>>::new();
        for observation in observations {
            windows
                .entry(observation.window)
                .or_default()
                .push(observation.clone());
        }

        for batch in windows.values() {
            self.operation.check()?;
            self.context_batch_windows(batch)?;
            output.append(&mut metrics::trends(self, batch)?);
        }

        output.sort_by(|left, right| {
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

        metrics::context_segments(output)
    }

    fn context_batch_windows(&self, observations: &[Observation]) -> Result<()> {
        let mut windows = BTreeSet::new();
        let publications = observations
            .iter()
            .map(|value| value.publication)
            .collect::<BTreeSet<_>>();
        for observation in observations {
            windows.insert(observation.window);
            if !publications.contains(&observation.publication.saturating_sub(1)) {
                if let Some(previous) = self.predecessor(observation)? {
                    windows.insert(previous.window);
                }
            }
            if windows.len() > 2 {
                return Err(Error::Bound("context batch windows"));
            }
        }

        Ok(())
    }

    pub fn cpu_detail(&self) -> Result<Value> {
        self.operation.run(|| {
            let selected = self
                .selected
                .as_ref()
                .ok_or(Error::Bound("no selected observation"))?;
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
            identity: observation.json(self),
            observation: observation.clone(),
            cpus,
        });

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
        let (_, _, order) = persistence::schema::TABLES
            .iter()
            .find(|entry| entry.0.name() == name)
            .ok_or(Error::Bound("table name"))?;
        if !offset.is_multiple_of(PAGE_ROWS) {
            return Err(Error::Bound("page offset"));
        }
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
            "offset": offset, "total": total, "observation": selected.json(self)}),
        )
    }

    pub(crate) fn source(&self, window: usize, name: &str) -> Result<String> {
        self.operation.check()?;
        #[cfg(test)]
        self.source_reads.set(self.source_reads.get() + 1);
        let (table, _, _) = persistence::schema::TABLES
            .iter()
            .find(|entry| entry.0.name() == name)
            .ok_or(Error::Bound("table name"))?;
        self.validate_commitment(window)?;
        let manifest = &self.windows[window];
        let path = self
            .root
            .join(&manifest.data_directory)
            .join(format!("{name}.parquet"));
        persistence::query::table_source(
            &self.connection,
            *table,
            persistence::query::TableSource::Parquet(&path),
        )
    }

    /// Checks that a window `discover` listed is still committed unchanged. Readers call it
    /// around every read, so it verifies the window's files on the first and rereads the marker
    /// after.
    fn validate_commitment(&self, window: usize) -> Result<()> {
        self.operation.check()?;

        assert_eq!(self.verified.len(), self.windows.len());
        let manifest = &self.windows[window];
        let marker = layout::WindowKey::from(&manifest.identity).marker(&self.root);
        let flag = &self.verified[window];
        let committed = if flag.get() {
            reread_committed(&self.root, &marker)?
        } else {
            read_committed(&self.root, &marker)?
        };
        if committed != *manifest {
            return Err(Error::Bound("changed commitment"));
        }

        flag.set(true);
        Ok(())
    }

    /// The windows this session's reads verified, which a reopen need not verify again.
    pub(crate) fn verified_windows(&self) -> Verified {
        assert_eq!(self.verified.len(), self.windows.len());
        Verified(
            self.windows
                .iter()
                .zip(&self.verified)
                .filter(|(_, flag)| flag.get())
                .map(|(manifest, _)| manifest.clone())
                .collect(),
        )
    }

    pub(crate) fn table(&self, window: usize, name: &str) -> Result<Table> {
        if name == metrics::core_frequency::CORE_LEVEL_STATES {
            return metrics::core_frequency::core_level_states(
                &self.connection,
                &self.source(window, "core_states")?,
                &self.operation,
            );
        }
        let order = if name == "fans" {
            "publication, fan"
        } else {
            "publication"
        };
        data::query(
            &self.connection,
            &format!(
                "SELECT * FROM {} ORDER BY {order}",
                self.source(window, name)?
            ),
            data::ROWS_MAX,
            &self.operation,
        )
    }

    pub(crate) fn predecessor(&self, observation: &Observation) -> Result<Option<Observation>> {
        let Some(publication) = observation.publication.checked_sub(1) else {
            return Ok(None);
        };
        let run = self.windows[observation.window].identity.run;
        for (window, manifest) in self.windows.iter().enumerate() {
            if manifest.identity.run == run {
                if (manifest.publication_first..manifest.publication_end).contains(&publication) {
                    let table = self.table(window, "ticks")?;
                    for row in &table.rows {
                        if table.integer(row, "publication")? == Some(publication) {
                            if table.integer(row, "wall_ns")?.is_some() {
                                return Ok(Some(Observation::from_row(&table, row, window)?));
                            }
                        }
                    }
                }
            }
        }
        Ok(None)
    }

    fn extent(&mut self) -> Result<()> {
        for window in 0..self.windows.len() {
            let bounds = self.windows[window].wall_ns;
            if let Some(bounds) = bounds {
                self.first = Some(
                    self.first
                        .map_or(bounds.min / NS, |v| v.min(bounds.min / NS)),
                );
                self.last = Some(
                    self.last
                        .map_or(bounds.max / NS, |v| v.max(bounds.max / NS)),
                );
            } else {
                let ticks = self.table(window, "ticks")?;
                for row in &ticks.rows {
                    if let Some(wall) = ticks.integer(row, "wall_ns")? {
                        self.first = Some(self.first.map_or(wall / NS, |v| v.min(wall / NS)));
                        self.last = Some(self.last.map_or(wall / NS, |v| v.max(wall / NS)));
                    } else {
                        self.unknown += 1;
                    }
                }
            }
        }
        Ok(())
    }

    fn observations(&self, first: u64, end: u64) -> Result<Vec<Observation>> {
        let mut observations = Vec::new();
        for (window, manifest) in self.windows.iter().enumerate() {
            if let Some(bounds) = manifest.wall_ns {
                if bounds.max < first {
                    continue;
                }
                if bounds.min >= end {
                    continue;
                }
            }
            let source = self.source(window, "ticks")?;
            let ticks = data::query(
                &self.connection,
                &format!("SELECT * FROM {source} WHERE wall_ns>={first} AND wall_ns<{end}"),
                OBSERVATIONS_MAX,
                &self.operation,
            )?;
            if observations.len() + ticks.rows.len() > OBSERVATIONS_MAX {
                return Err(Error::Bound("context observations"));
            }
            for row in &ticks.rows {
                observations.push(Observation::from_row(&ticks, row, window)?);
            }
        }
        Ok(observations)
    }
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

fn discover(
    root: &Path,
    recording: Identity,
    windows: &mut Vec<Manifest>,
    operation: &Operation,
) -> Result<()> {
    let base = layout::base(root, Kind::Commits);
    if !base.try_exists()? {
        return Err(io::Error::from(io::ErrorKind::InvalidData).into());
    }
    layout::visit_names(&base, |run| -> Result<()> {
        layout::validate_run(run)?;
        let run_path = base.join(run);
        layout::visit_names(&run_path, |day| -> Result<()> {
            layout::validate_day(day)?;
            layout::visit_names(&run_path.join(day), |name| -> Result<()> {
                operation.check()?;
                if name.starts_with('.') {
                    return Ok(());
                }
                if Path::new(name).extension() != Some(std::ffi::OsStr::new("seal")) {
                    return Err(Error::Bound("unexpected commit file"));
                }
                if windows.len() == WINDOWS_MAX {
                    return Err(Error::Bound("catalog windows"));
                }
                let marker = run_path.join(day).join(name);
                // The window's files are left to `validate_commitment` on its first read.
                windows.push(list_committed(root, &marker, recording)?);
                Ok(())
            })
        })
    })?;
    windows.sort_by_key(|manifest| (manifest.identity.run.run.0, manifest.publication_first));
    Ok(())
}

#[path = "cpu_history_saved.rs"]
mod cpu_history_saved;

mod app_history;
pub(crate) use app_history::AttributionReplay;
pub(crate) use app_history::ProcessGroupsReplay;
pub use app_history::SealedApps;
pub(crate) use app_history::{KeptAttribution, KeptResident};
mod committed_apps;
mod earlier;
#[cfg(test)]
mod reopen_tests;
mod selection;
pub use earlier::{
    CHARTED_SECONDS, CONTEXT_AFTER_SECONDS, EARLIER_STRIDE_SECONDS, LEAD_IN_SECONDS, LOADED_SECONDS,
};
pub(crate) use earlier::{DETAIL_SECONDS, Tail};
