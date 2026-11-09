//! A viewer's session on the minutes a recording saved, which it lists again as they arrive.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use persistence::{Error, Result};
use serde_json::{Value, json};

use crate::cancellation::Operation;
use crate::live_app_history::{GroupTarget, History, Selection, Target};
use crate::live_app_response::{Detail, KeyKind, Page, Sample};
use crate::session::{
    ContextRange, ContextSelection, EARLIER_STRIDE_SECONDS, KeptTail, LEAD_IN_SECONDS,
    LOADED_SECONDS, OBSERVATIONS_MAX, TailRange, Verified,
};
use crate::{Cancellation, Session};

#[path = "app_resident_history.rs"]
pub(crate) mod resident_history;

#[path = "app_attribution_history.rs"]
pub(crate) mod attribution_history;

#[path = "live_app_parts.rs"]
pub(crate) mod app_parts;
#[path = "live_app_ranks.rs"]
pub(crate) mod app_ranks;
#[path = "live_window.rs"]
pub(crate) mod window;

#[path = "live_process_groups.rs"]
mod process_groups;

const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;
const CONTEXT_BYTES_MAX: usize = 8 * 1024 * 1024;
/// How far a context moves before `list_saved_for` lists the saved minutes again.
const SAVED_LISTING_SECONDS: u64 = 60;
pub struct LiveSession {
    root: PathBuf,
    history: Option<Session>,
    history_error: Option<String>,
    earlier: KeptTail,
    app_history: History,
    operation: Operation,
    cancellation: Arc<Cancellation>,
    selected: Option<Value>,
    selected_context: Vec<crate::cpu_history_sample::Identity>,
    /// The last second of the context the last selection read, which its histories span.
    context_last_second: Option<u64>,
    app_ranks: Arc<app_ranks::WindowRanks>,
    parts_app: Option<app_parts::PartsApp>,
    app_parts: Arc<app_parts::WindowParts>,
    /// The context's last second when this session last listed the saved minutes for it.
    saved_listed_second: u64,
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
        let mut session = Self {
            root: root.to_owned(),
            history: None,
            history_error: None,
            earlier: KeptTail::default(),
            app_history: History::new(),
            operation,
            cancellation,
            selected: None,
            selected_context: Vec::new(),
            context_last_second: None,
            app_ranks: Arc::default(),
            parts_app: None,
            app_parts: Arc::default(),
            saved_listed_second: 0,
        };
        if let Err(error) = session.refresh_history() {
            session.history_failure(error)?;
        }

        Ok(session)
    }

    pub fn begin(&mut self, epoch: u64) -> Result<()> {
        if self.operation.epoch() != epoch {
            self.selected = None;
        }
        self.operation = self.operation.begin(epoch)?;
        if let Some(history) = &mut self.history {
            history.begin(epoch)?;
        }
        Ok(())
    }

    /// Lists what the recording saved since the session last looked, and reads no ring, since a
    /// Store takes in the live Mac's readings. A recording whose identity no longer reads refuses
    /// the list. A refused list of its minutes leaves the session with the minutes it knows and
    /// says why beside them.
    pub fn list_saved(&mut self) -> Result<()> {
        let operation = self.operation.begin(self.operation.epoch())?;
        operation.run(|| {
            collector::recording::read_identity(&self.root)?;
            match self.refresh_history() {
                Ok(()) => Ok(()),
                Err(error) => self.history_failure(error),
            }
        })
    }

    /// `list_saved` for a page read once a second, which lists only as often as the recording
    /// saves a minute, however often its context ending at `context_last_second` moves.
    pub fn list_saved_for(&mut self, context_last_second: u64) -> Result<()> {
        if self.saved_listed_second.abs_diff(context_last_second) < SAVED_LISTING_SECONDS {
            return Ok(());
        }

        self.list_saved()?;
        self.saved_listed_second = context_last_second;

        Ok(())
    }

    #[must_use]
    pub fn catalog(&self) -> Value {
        let mut catalog = self.history.as_ref().map_or_else(
            || {
                json!({"path": self.root,
            "first_second": null, "last_second": null, "windows": 0,
            "unknown_clock_ticks": 0,
            "tables": persistence::schema::TABLES.map(persistence::schema::Table::name)})
            },
            Session::catalog,
        );
        catalog["history_error"] = json!(self.history_error);
        catalog
    }

    /// Selects `second` with the context around it, after listing the saved minutes again where
    /// the recording saved one since the session last listed them.
    pub fn select(&mut self, second: u64, alternative: usize) -> Result<Value> {
        self.selected = None;
        self.operation.check()?;
        select_time(second)?;
        if self.select_refresh_due()? {
            self.refresh_history()?;
        }

        self.select_listed(second, alternative)
    }

    /// Selects `second` with the context around it from the minutes the session lists, for a
    /// later step of a page a `select` began, so the context keeps the end that step read while
    /// the recording saves a newer minute.
    pub fn select_listed(&mut self, second: u64, alternative: usize) -> Result<Value> {
        self.selected = None;
        self.operation.check()?;
        select_time(second)?;
        let selection = ContextSelection::of(second, self.newest_second());
        let saved = match &mut self.history {
            Some(history) => {
                history.begin(self.operation.epoch())?;
                Some(history.select_context(selection, alternative))
            }
            None => None,
        };
        let response = match saved {
            Some(Ok(response)) => response,
            Some(Err(error)) => {
                self.history_failure(error)?;
                select_unsaved(selection)
            }
            None => select_unsaved(selection),
        };
        if !response["selected"].is_null() {
            self.selected = Some(response["selected"].clone());
        }
        self.select_cpu_context(selection, &response)?;

        Ok(response)
    }

    /// Selects the observation `identity` names at `second`, for a read of that second alone, as
    /// a hover's is. It builds no context, which costs `select` hundreds of milliseconds on a
    /// full window, and leaves the context the last `select` read, so only reads of the selected
    /// second may follow it, never a history. The summary carries no segment, which only a
    /// context numbers. A saved minute the session has not listed is found by listing them again.
    pub fn inspect(&mut self, second: u64, identity: &Value) -> Result<Value> {
        self.selected = None;
        self.operation.check()?;

        let mut read = self.inspect_history(second, identity)?;
        if read.is_none() {
            self.refresh_history()?;
            read = self.inspect_history(second, identity)?;
        }
        let response = read.ok_or(Error::Bound("inspected observation"))?;
        self.selected = Some(response["selected"].clone());

        Ok(response)
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
    /// seconds. Each sealed minute the tail needs is read once, at the latest when the tail first
    /// needs it, so following the newest second reads a minute about once a minute and a stride
    /// inside the kept minutes reads nothing.
    pub fn earlier_context(&mut self, context_last_second: u64) -> Result<Value> {
        if let Some(tail) = self.earlier_context_kept(context_last_second)? {
            return Ok(tail);
        }

        if let Some(history) = &mut self.history {
            history.begin(self.operation.epoch())?;
            let range = TailRange::of_context(context_last_second);
            history.load_span(range.context_last_second())?;
            self.earlier.fill(history, range)?;
        }
        self.earlier_context_kept(context_last_second)?
            .ok_or(Error::Bound("older tail"))
    }

    /// The older tail `earlier_context` gives for `context_last_second`, where the minutes it
    /// keeps answer it or the minutes it lacks read in one batch, as the tail of the newest
    /// second needs about once a minute, and `None` where answering would read more or list the
    /// saved minutes again.
    pub fn earlier_context_at_once(&mut self, context_last_second: u64) -> Result<Option<Value>> {
        if let Some(tail) = self.earlier_context_kept(context_last_second)? {
            return Ok(Some(tail));
        }
        let Some(history) = &self.history else {
            unreachable!("A session without saved history answers its older tail at once");
        };
        let range = TailRange::of_context(context_last_second);
        if !self.earlier.fills_in_one_batch(history, range)? {
            return Ok(None);
        }

        self.earlier_context(context_last_second).map(Some)
    }

    /// The older tail `earlier_context` gives for `context_last_second`, when the minutes it
    /// keeps answer it, and `None` when answering would read saved history.
    pub fn earlier_context_kept(&self, context_last_second: u64) -> Result<Option<Value>> {
        let first_second = context_last_second.saturating_sub(LOADED_SECONDS + LEAD_IN_SECONDS - 1);
        let respond = |points: &[Value]| {
            let points: Vec<&Value> = points
                .iter()
                .filter(|point| {
                    point["wall_ns"]
                        .as_u64()
                        .is_some_and(|wall| wall / NANOSECONDS_PER_SECOND >= first_second)
                })
                .collect();
            context_bytes_of(&points)?;

            Ok(json!({"earlier_trends": points,
                "earlier_stride_seconds": EARLIER_STRIDE_SECONDS}))
        };

        match &self.history {
            Some(history) => {
                let range = TailRange::of_context(context_last_second);
                self.earlier.answer(history, range, respond)
            }
            None => respond(&[]).map(Some),
        }
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
            retained: None,
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
        let history = self
            .history
            .as_ref()
            .ok_or(Error::Bound("missing historical selection"))?;
        let publication = selected["publication"]
            .as_u64()
            .ok_or(Error::Bound("selected publication"))?;
        let mut current_slot = None;
        let mut previous_slot = None;
        history.cpu_detail_publication(selected, publication, &mut current_slot)?;

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
            history.cpu_detail_publication(&identity, publication, &mut previous_slot)?;
        }

        crate::cpu_detail::project(current, previous_slot.as_ref())
    }

    pub fn apps_page(&self, offset: u32) -> Result<Value> {
        self.operation.run(|| {
            self.with_selected_apps(|sample| {
                sample.apps_page(Page {
                    offset,
                    limit: crate::live_app_response::APPS_PAGE_ROWS_MAX,
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
        let response = crate::live_app_response::bounded(response)?;
        self.operation.check()?;

        Ok(response)
    }

    fn app_history_selected(&self, target: &dyn Selection) -> Result<Value> {
        self.operation.run(|| {
            let selected = self.selected_in_context()?;
            let mut restored = false;
            let result = self
                .saved_session()?
                .with_app_history(&selected, target, |saved| {
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
        })
    }

    #[cfg(test)]
    pub(crate) fn page(&self, name: &str, offset: u64) -> Result<Value> {
        self.saved_session()?.page(name, offset)
    }

    /// The session on the saved minutes, which any selected observation comes from.
    fn saved_session(&self) -> Result<&Session> {
        self.history
            .as_ref()
            .ok_or(Error::Bound("missing historical selection"))
    }

    fn app_history_degraded(
        &self,
        selected: &crate::live_app_response::Selected,
        target: &dyn Selection,
        error: Error,
    ) -> Result<Value> {
        self.operation.check()?;
        match &error {
            Error::Bound(_) | Error::SecondExpired => return Err(error),
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
                        limit: crate::live_app_response::PROCESSES_PAGE_ROWS_MAX,
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
        self.history
            .as_ref()
            .ok_or(Error::Bound("live App raw observation unavailable"))?
            .with_app_sample(selected, consume)
    }

    /// The newest second the saved minutes hold.
    fn newest_second(&self) -> Option<u64> {
        self.history.as_ref().and_then(Session::last_second)
    }

    /// Whether `select` lists the saved minutes again before reading: the saved minutes are no
    /// longer the ones listed. A launch's session has just listed every saved minute.
    fn select_refresh_due(&self) -> Result<bool> {
        match &self.history {
            Some(history) => Ok(!history.lists_every_sealed_window()?),
            None => Ok(true),
        }
    }

    fn refresh_history(&mut self) -> Result<()> {
        if crate::session::saved_history_exists(&self.root)? {
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
                    return Ok(());
                }
            }

            if let Some(selected) = &self.selected {
                history.restore_selection(selected)?;
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

    /// Builds the CPU history context of the selected second from the trends of `response`.
    fn select_cpu_context(&mut self, selection: ContextSelection, response: &Value) -> Result<()> {
        let mut context = Vec::new();
        crate::cpu_history::context(
            response["trends"]
                .as_array()
                .ok_or(Error::Bound("CPU selected context"))?,
            ContextRange::ending_at(selection.context_last_second())?,
            &mut context,
        )?;
        self.selected_context = context;
        self.context_last_second = Some(selection.context_last_second());

        Ok(())
    }
}

/// Refuses a `second` whose end the clock cannot hold, before a selection reads anything for it.
fn select_time(second: u64) -> Result<()> {
    second
        .checked_add(1)
        .and_then(|end| end.checked_mul(NANOSECONDS_PER_SECOND))
        .ok_or(Error::Bound("selection time"))?;

    Ok(())
}

/// A selection of `second` with nothing read from the saved minutes.
fn select_unsaved(selection: ContextSelection) -> Value {
    json!({"second": selection.second(), "context_last_second": selection.context_last_second(),
        "selected": null, "alternatives": [], "summary": null, "trends": [],
        "context_seconds": 900})
}

fn same(left: &Value, right: &Value) -> bool {
    left["run"] == right["run"]
        && left["boot"] == right["boot"]
        && left["publication"] == right["publication"]
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

/// Selects the newest reading the session lists, the last of its second's alternatives.
#[cfg(test)]
pub(crate) fn select_newest(session: &mut LiveSession) -> Value {
    let second = session.catalog()["last_second"].as_u64().unwrap();
    let first = session.select(second, 0).unwrap();
    let alternatives = first["alternatives"].as_array().unwrap().len();
    session.select(second, alternatives - 1).unwrap()
}

#[cfg(test)]
#[path = "live_fixture.rs"]
pub(crate) mod identity_fixture;

/// The first file holding the events of the saved window `manifest`.
#[cfg(test)]
pub(crate) fn saved_events(root: &Path, manifest: &persistence::Manifest) -> PathBuf {
    let budget = Arc::new(persistence::resources::ByteBudget::new(1 << 28));
    persistence::event_table::EventTable::open_budgeted(root, manifest, &budget)
        .unwrap()
        .part(0)
        .unwrap()
        .path()
        .to_owned()
}

#[cfg(test)]
#[path = "live_forged_source.rs"]
pub(crate) mod forged_source;

#[cfg(test)]
#[path = "live_app_process_history_tests.rs"]
mod app_process_history_tests;

#[cfg(test)]
mod tests {

    use super::*;
    use crate::session::CHARTED_SECONDS;
    use collector::recording::{RegisteredRun, Registration, Registry, identity_text};
    use collector::ring::{
        Dropped, IdentityObservation, ObservationTimes, Place, Readings, RingReader, Shape, Writer,
    };
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

    mod earlier_tests {
        use super::*;
        include!("live_earlier_tests.rs");
    }

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct RingFixture {
        root: PathBuf,
        writer: Writer,
        following_fans: u16,
        owns_root: bool,
        persister: std::sync::Mutex<persistence::worker::HeldWindows>,
    }

    impl RingFixture {
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
                // The eight events of `publish` for each tick the ring holds and the one it
                // publishes, which the writer writes before it replaces the oldest.
                event_capacity: 3 * 8,
            })
        }

        fn new_with_shape(shape: &Shape) -> Self {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let root = std::env::temp_dir()
                .join(format!("flightdeck-retry-{}-{serial}", std::process::id()));
            Self::new_at(root, shape)
        }

        /// A run that records into `root`, which it finishes setting up where its setup stopped.
        fn new_at(root: PathBuf, shape: &Shape) -> Self {
            assert!(shape.slot_count <= 120);
            assert!(shape.cpu_capacity <= 18);

            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            drop(persistence::Store::open(&root).unwrap());
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
                owns_root: true,
                persister: std::sync::Mutex::default(),
            }
        }

        /// The run after `self`'s in the same recording, as after a collector restart, whose ring
        /// has `self`'s shape and which the registry lists after the runs already there.
        fn next_run(&self) -> Self {
            let serial = NEXT.fetch_add(1, Ordering::Relaxed);
            let place = Place {
                lock_path: self.root.join(format!("run-{serial}.lock")),
                name_prefix: format!("/fdrerun{}x{serial}", std::process::id()),
            };
            let recording = self.writer.identity().unwrap().recording;
            let identity = collector::recording::new_run_identity(recording).unwrap();
            let shape = RingReader::open_generation(self.writer.name())
                .unwrap()
                .shape();
            let writer = Writer::create_recording(&place, shape, &identity, 0).unwrap();
            let mut registry = Registry::read(&self.root).unwrap();
            // The registry holds two runs, so a third retires the first, whose readings are saved.
            if registry.runs.len() == 2 {
                registry.runs.remove(0);
            }
            registry.runs.push(RegisteredRun {
                identity,
                ring_name: writer.name().to_owned(),
                collector_pid: 0,
            });
            registry.write(&self.root).unwrap();

            Self {
                root: self.root.clone(),
                writer,
                following_fans: self.following_fans,
                owns_root: false,
                persister: std::sync::Mutex::default(),
            }
        }

        /// Saves every window the ring holds past what is saved, with the persister's own worker.
        fn seal_held(&self) {
            self.seal_held_through(None);
        }

        /// Saves the windows the ring holds past what is saved through `window_last`, as the
        /// minutes going by would, whether or not a later window has begun.
        fn seal_through(&self, window_last: u64) {
            self.seal_held_through(Some(window_last));
        }

        fn seal_held_through(&self, window_last: Option<u64>) {
            let store = persistence::Store::open(&self.root).unwrap();
            let run = RegisteredRun {
                identity: self.writer.identity().unwrap(),
                ring_name: self.writer.name().to_owned(),
                collector_pid: 0,
            };
            let mut held = self.persister.lock().unwrap();
            persistence::worker::seal_held_windows(
                &store,
                &run,
                window_last,
                &mut held,
                &identity_fixture::Underived,
            )
            .unwrap();
        }

        fn new_following() -> Self {
            Self::new_following_fans(2)
        }

        fn new_following_fans(following_fans: u16) -> Self {
            assert!((2..=10).contains(&following_fans));
            let mut fixture = Self::new_with_shape(&Self::following_shape(following_fans));
            fixture.following_fans = following_fans;
            fixture
        }

        /// The ring of a run that publishes `publish_following`'s readings.
        fn following_shape(following_fans: u16) -> Shape {
            Shape {
                slot_count: 120,
                cpu_capacity: 18,
                interface_capacity: 1,
                core_capacity: 1,
                state_capacity: 1,
                fan_capacity: u64::from(following_fans),
                process_capacity: 1,
                coalition_capacity: 1,
                event_capacity: 8,
            }
        }

        fn publish_following(&mut self, tick: u32) {
            self.publish_following_wall(tick, 1000 + u64::from(tick));
        }

        /// Publishes `tick`'s reading with no clocks, as a source with no clocks leaves it.
        fn publish_following_without_clocks(&mut self, tick: u32) {
            let cpus = [collector::machine::CPUTicks::default(); 18];
            let memory = collector::machine::Memory::default();
            self.writer
                .publish(&ring_fixture_cpu_readings(tick, &cpus, &memory));
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

        fn publish_cpu_without_clocks(&mut self, tick: u32) {
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
            let memory = collector::machine::Memory::default();
            self.writer
                .publish(&ring_fixture_cpu_readings(tick, &cpus, &memory));
        }
        fn publish_cpu_readings(&mut self, tick: u32, cpus: &[collector::machine::CPUTicks]) {
            let memory = collector::machine::Memory::default();
            self.writer.publish_observed(
                &ring_fixture_cpu_readings(tick, cpus, &memory),
                &ObservationTimes {
                    wall_ns: (1000 + u64::from(tick)) * NANOSECONDS_PER_SECOND,
                    continuous_ns: u64::from(tick) * NANOSECONDS_PER_SECOND,
                    awake_ns: u64::from(tick) * NANOSECONDS_PER_SECOND,
                },
            );
        }
    }

    fn ring_fixture_cpu_readings<'a>(
        tick: u32,
        cpus: &'a [collector::machine::CPUTicks],
        memory: &'a collector::machine::Memory,
    ) -> Readings<'a> {
        Readings {
            run_tick: u64::from(tick),
            cpus,
            memory,
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: Dropped::default(),
        }
    }

    impl Drop for RingFixture {
        fn drop(&mut self) {
            collector::ring::unlink_generation(self.writer.name()).unwrap();
            if self.owns_root {
                std::fs::remove_dir_all(&self.root).unwrap();
            }
        }
    }

    #[test]
    fn context_rejects_more_than_4096_scalar_points() {
        // Goal: the context budget refuses a result larger than the public observation bound.
        // Method: measure 4097 individually tiny scalar values, whose bytes fit the other bound.
        let points = vec![json!({"cpu_percent":0}); 4097];

        let result = context_bytes_of(&points.iter().collect::<Vec<_>>());

        assert!(matches!(result, Err(Error::Bound("context observations"))));
    }

    #[test]
    fn context_rejects_more_than_eight_mib_of_serialized_scalars() {
        // Goal: a small point count cannot evade the separate serialized context byte bound.
        // Method: encode one 8 MiB value, whose JSON framing exceeds the 8 MiB contract.
        let points = [Value::String("x".repeat(8 * 1024 * 1024))];

        let result = context_bytes_of(&points.iter().collect::<Vec<_>>());

        assert!(matches!(result, Err(Error::Bound("context bytes"))));
    }

    #[test]
    fn selecting_a_second_of_a_saved_minute_that_fails_verification_says_why() {
        // Goal: a damaged saved minute gives no reading and leaves the reason beside the answer.
        // Method: put a stray file beside a sealed minute's tables, then select its second.
        let mut fixture = RingFixture::new_following();
        fixture.publish_following(1);
        fixture.seal_held();
        let mut live = LiveSession::open(&fixture.root).unwrap();
        let second = live.catalog()["last_second"].as_u64().unwrap();
        let sealed = Session::open(&fixture.root).unwrap().windows.remove(0);
        let stray = fixture
            .root
            .join(&sealed.data_directory)
            .join("stray.parquet");
        std::fs::write(stray, b"stray").unwrap();

        let selection = live.select(second, 0).unwrap();

        assert!(selection["selected"].is_null());
        assert!(live.catalog()["history_error"].is_string());
    }

    #[test]
    fn preserves_cpu_detail_of_a_listing_session_and_a_saved_one() {
        // Goal: a session that lists and one that opens saved derive the same CPU interval.
        // Method: seal two genuine readings and compare both public projections.
        let mut fixture = RingFixture::new_with_cpu_capacity(2);
        fixture.publish_cpu_activity(1);
        let mut live = LiveSession::open(&fixture.root).unwrap();
        fixture.publish_cpu_activity(2);
        fixture.seal_held();
        live.list_saved().unwrap();
        let selection = live.select(1002, 0).unwrap();

        let first = live.cpu_detail().unwrap();
        let repeated = live.cpu_detail().unwrap();
        fixture.seal_through(0);
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
        assert_eq!(first["observation"]["run_tick"], 2);
        assert_eq!(first["previous_observation"]["run_tick"], 1);
    }

    #[test]
    fn recovers_cpu_detail_predecessor_from_its_exact_commit() {
        // Goal: CPU-only committed queries read the predecessor without moving selection.
        // Method: seal two readings, then project the selection from the saved minutes.
        let mut fixture = RingFixture::new_with_cpu_capacity(2);
        fixture.publish_cpu_activity(1);
        let mut live = LiveSession::open(&fixture.root).unwrap();
        fixture.publish_cpu_activity(2);
        fixture.seal_held();
        live.list_saved().unwrap();
        live.select(1002, 0).unwrap();
        fixture.seal_through(0);
        live.refresh_history().unwrap();
        let selected = live.selected.clone();

        let result = live.cpu_detail().unwrap();

        assert_eq!(result["interval_available"], true);
        assert_eq!(
            result["cpus"],
            json!([{ "cpu":0,"busy_percent":25.0 }, {"cpu":1,"busy_percent":75.0}])
        );
        assert_eq!(live.selected, selected);
    }

    #[test]
    fn reads_cpu_detail_from_commits_without_changing_the_historical_selection() {
        // Goal: exact CPU recovery cannot rewrite a different historical selection.
        // Method: seal the readings and leave history selected elsewhere.
        let mut fixture = RingFixture::new_with_cpu_capacity(2);
        fixture.publish_cpu_activity(1);
        let mut live = LiveSession::open(&fixture.root).unwrap();
        fixture.publish_cpu_activity(2);
        fixture.seal_held();
        live.list_saved().unwrap();
        live.select(1002, 0).unwrap();
        fixture.seal_through(0);
        live.refresh_history().unwrap();
        live.history.as_mut().unwrap().select(1001, 0).unwrap();
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
        // Method: publish a first row with no clocks and a timed successor, save the minute that
        // holds both, evict the first from the live cache, then query its successor.
        let mut fixture = RingFixture::new_with_cpu_capacity(2);
        fixture.publish_cpu_without_clocks(1);
        let mut live = LiveSession::open(&fixture.root).unwrap();
        fixture.publish_cpu_activity(2);
        fixture.seal_held();
        live.list_saved().unwrap();
        live.select(1002, 0).unwrap();
        fixture.seal_through(0);
        live.refresh_history().unwrap();
        let selected = live.selected.clone();

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
    }

    #[test]
    fn a_recording_of_an_older_format_reports_a_history_error() {
        // Goal: saved history this version cannot read says so and never reads as no history.
        // Method: open a session over a root as the previous format left it.
        let root = crate::root_fixture::older_root();

        let session = LiveSession::open(&root).unwrap();

        assert!(session.history.is_none());
        assert!(session.history_error.is_some());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_root_still_being_set_up_has_no_history_and_no_history_error() {
        // Goal: the last moment of a first launch reads as nothing saved yet and not as a fault.
        // Method: open a session over a root whose setup stopped after its identity.
        let root = crate::root_fixture::unfinished_root();

        let session = LiveSession::open(&root).unwrap();

        assert!(session.history.is_none());
        assert_eq!(session.history_error, None);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_session_opened_while_its_recording_was_set_up_selects_what_it_saved_since() {
        // Goal: a hold in a recording's first minutes reads what it saved after the page opened
        // over the recording still being set up.
        // Method: open a session over a root whose setup stopped after its identity, finish
        // setting it up, save a minute, and select that minute's second.
        let root = crate::root_fixture::unfinished_root();
        let mut live = LiveSession::open(&root).unwrap();
        assert!(live.history.is_none());
        let mut fixture = RingFixture::new_at(root, &RingFixture::following_shape(2));
        fixture.publish_following(1);
        fixture.seal_held();

        let selection = live.select(1001, 0).unwrap();

        assert!(!selection["selected"].is_null());
    }

    #[test]
    fn keeps_null_context_gaps_when_optional_app_history_is_unreadable() {
        // Goal: a saved minute of the context that no longer reads leaves explicit gaps beside
        // the reason, while the selected second still reads.
        // Method: select a second of the later of two minutes, then withhold the earlier one.
        let mut fixture = identity_fixture::Fixture::new();
        fixture.start_at_run_tick(59);
        fixture.publish_with_system_rows(1001);
        fixture.publish_with_system_rows(1061);
        fixture.publish_with_system_rows(1062);
        fixture.seal();
        let mut session = LiveSession::open(&fixture.root).unwrap();
        session.select(1062, 0).unwrap();
        session.app_history = History::new();
        let earlier = Session::open(&fixture.root).unwrap().windows.remove(0);
        let data = fixture.root.join(&earlier.data_directory);
        let withheld = fixture.root.join("withheld-data");
        std::fs::rename(&data, &withheld).unwrap();

        let history = session
            .app_history(KeyKind::Bundle, b"/Apps/Current.app")
            .unwrap();

        assert_eq!(history["points"].as_array().unwrap().len(), 3);
        assert_eq!(history["points"][0]["source_available"], false);
        assert_eq!(history["points"][2]["resident_bytes"], Value::Null);
        assert_eq!(history["coverage"]["retention_limited"], true);
        assert!(history["restoration_error"].is_string());
        std::fs::rename(&withheld, &data).unwrap();
    }

    #[test]
    fn refuses_structural_and_canceled_app_history_instead_of_degrading_it() {
        // Goal: degraded history cannot conceal conflicting identity or superseded work.
        // Method: submit structural/interruption errors at the fallback and cancel a public call.
        let mut fixture = identity_fixture::Fixture::new();
        fixture.publish_with_system_rows(1001);
        let control = Arc::new(Cancellation::default());
        fixture.seal();
        let mut session = LiveSession::open_with(&fixture.root, control.clone(), 0).unwrap();
        select_newest(&mut session);
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
}

#[cfg(test)]
#[path = "live_app_process_groups_history_tests.rs"]
pub(crate) mod app_process_groups_history_tests;
