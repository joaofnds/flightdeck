use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::live_source_mode::SourceMode;
use serde_json::Value;
use viewer::live_app_response::KeyKind;
use viewer::{Cancellation, LiveSession};

pub struct Worker {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
    mode: SourceMode,
}

/// Whether a selection carries the readings older than its full-detail context.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Earlier {
    Load,
    Skip,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub route: String,
    pub second: Option<u64>,
    pub identity: Option<Value>,
    pub table: String,
    pub offset: u64,
    pub app: Option<AppKey>,
    pub apps_offset: u32,
    pub members_offset: u32,
    pub cpu_offset: u32,
    pub load_resident_history: bool,
    pub load_app_attribution_history: bool,
    pub load_process_history: bool,
    /// Whether the selection carries the older tail the charts need to span the loaded hour,
    /// which a hover's preview of one observation does without.
    pub earlier: Earlier,
    pub executable: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppKey {
    pub kind: String,
    pub bytes_hex: String,
}

/// The lowercase hexadecimal text of an App key's bytes.
pub fn app_key_bytes_hex(bytes: &[u8]) -> String {
    let mut bytes_hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        for nibble in [byte >> 4, byte & 0x0f] {
            bytes_hex.push(char::from(b"0123456789abcdef"[usize::from(nibble)]));
        }
    }

    bytes_hex
}

pub fn app_key_from_json(app: &Value) -> Result<AppKey, String> {
    let kind = app["key"]["kind"]
        .as_str()
        .ok_or("App key kind unavailable")?;
    if !matches!(kind, "bundle" | "executable") {
        return Err("Invalid App key kind".into());
    }

    let bytes_hex = app["key"]["bytes_hex"]
        .as_str()
        .ok_or("App key bytes unavailable")?;
    if bytes_hex.len() > APP_KEY_BYTES_MAX * 2 {
        return Err("App key exceeds the captured path bound".into());
    }

    if !bytes_hex.len().is_multiple_of(2) {
        return Err("App key has an incomplete byte".into());
    }

    if !bytes_hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("App key has invalid hexadecimal bytes".into());
    }

    Ok(AppKey {
        kind: kind.into(),
        bytes_hex: bytes_hex.to_ascii_lowercase(),
    })
}

pub struct Snapshot {
    pub generation: u64,
    pub catalog: Value,
    pub selection: Value,
    pub pages: BTreeMap<String, Value>,
    pub apps: Option<Value>,
    pub detail: Option<Value>,
    pub history: Option<Value>,
    pub cpu_detail: Option<crate::live_cpu::CPUActivity>,
    pub cpu_history: Option<Arc<crate::live_cpu_history::CPUHistory>>,
    pub resident_history: Option<Arc<crate::live_resident_history::ResidentHistory>>,
    pub app_attribution_history:
        Option<Arc<crate::live_app_attribution_history::AppAttributionHistory>>,
    pub overview_app_histories:
        Option<Arc<crate::live_overview_app_histories::OverviewAppHistories>>,
    pub process_history: Option<Arc<crate::live_process_history::ProcessHistory>>,
    pub rankings: Option<Arc<crate::live_rankings::Rankings>>,
    /// The top Apps of each observation of the selected window the viewer has ranked, so a
    /// hover shows its observation's at once.
    pub app_ranks: Option<Arc<viewer::WindowRanks>>,
    /// The selected App's part of each observation of the selected window the viewer has kept,
    /// so a hover on the Apps page shows its observation's App at once.
    pub app_parts: Option<Arc<viewer::WindowParts>>,
    /// The page's App histories are still being read, and a later snapshot carries them.
    pub histories_pending: bool,
    pub read_ms: f64,
}

pub enum Update {
    Ready(Box<Snapshot>),
    Failed { generation: u64, message: String },
}

struct Shared {
    mailbox: Mutex<Mailbox>,
    /// Wakes the worker from its pause when a request or a stop arrives.
    wake: Condvar,
    stopped: AtomicBool,
    cancellation: Arc<Cancellation>,
}

struct Mailbox {
    generation: u64,
    request: Option<Command>,
    output: Option<Update>,
}

struct Command {
    generation: u64,
    epoch: u64,
    mode: SourceMode,
    request: Request,
    context: Context,
}

/// Whether a read of a chosen second builds the 900 seconds around it, which the page's charts
/// draw, or keeps the context the page read last, as a hover's read of that second alone does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Context {
    Select,
    Keep,
}

struct Reader {
    session: Option<LiveSession>,
    system: crate::live_system::SystemSamples,
    last_catalog: Option<Value>,
    last_generation: u64,
    /// The route of the last published snapshot, so a newly opened page is told apart.
    last_route: String,
    /// The generation whose last snapshot left its App histories pending.
    pending_generation: Option<u64>,
    retry_after: Instant,
}

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const RETRY_INTERVAL: Duration = Duration::from_secs(1);
const WORKER_STEPS_MAX: u64 = 7 * 24 * 60 * 60 * 4;
const ALTERNATIVES_MAX: usize = 4096;
// Match the recording format's maximum captured program path length.
const APP_KEY_BYTES_MAX: usize = 4095;
const _: () = {
    assert!(WORKER_STEPS_MAX > 0);
    assert!(APP_KEY_BYTES_MAX > 0);
    assert!(ALTERNATIVES_MAX <= 4096);
};

impl Worker {
    pub fn start(root: PathBuf) -> std::io::Result<Self> {
        Self::start_mode(root, SourceMode::Live)
    }

    pub fn start_recorded(root: PathBuf) -> std::io::Result<Self> {
        Self::start_mode(root, SourceMode::Recorded)
    }

    pub fn mode(&self) -> SourceMode {
        self.mode
    }

    fn start_mode(root: PathBuf, mode: SourceMode) -> std::io::Result<Self> {
        let shared = Arc::new(Shared {
            mailbox: Mutex::new(Mailbox {
                generation: 0,
                request: None,
                output: None,
            }),
            wake: Condvar::new(),
            stopped: AtomicBool::new(false),
            cancellation: Arc::new(Cancellation::default()),
        });
        let control = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("flightdeck-live".to_owned())
            .stack_size(4 * 1024 * 1024)
            .spawn(move || worker_run(&root, &control))?;

        Ok(Self {
            shared,
            thread: Some(thread),
            mode,
        })
    }

    pub fn request(&mut self, request: Request) -> Result<u64, String> {
        self.submit(request, Context::Select)
    }

    /// Reads the observation `request` names at its second alone, keeping the context the last
    /// request read, so a hover shows its second without rebuilding the 900 seconds around it.
    /// The snapshot carries the second's readings and none of the histories over a context,
    /// since a history read over the kept context would mix it with the inspected second.
    pub fn inspect(&mut self, request: Request) -> Result<u64, String> {
        assert!(request.second.is_some());
        assert!(request.identity.is_some());
        assert!(!request.load_resident_history);
        assert!(!request.load_app_attribution_history);
        assert!(!request.load_process_history);
        assert_eq!(request.earlier, Earlier::Skip);
        self.submit(request, Context::Keep)
    }

    fn submit(&mut self, request: Request, context: Context) -> Result<u64, String> {
        request_validate(&request)?;
        if self.shared.stopped.load(Ordering::Acquire) {
            return Err("Live reader has stopped".to_owned());
        }
        let mut mailbox = self
            .shared
            .mailbox
            .lock()
            .map_err(|error| error.to_string())?;
        let generation = mailbox
            .generation
            .checked_add(1)
            .ok_or_else(|| "Live request generation exhausted".to_owned())?;
        self.shared
            .cancellation
            .cancel()
            .map_err(|error| error.to_string())?;
        mailbox.generation = generation;
        mailbox.output = None;
        mailbox.request = Some(Command {
            generation,
            epoch: self.shared.cancellation.epoch(),
            mode: self.mode,
            request,
            context,
        });
        self.shared.wake.notify_one();

        Ok(generation)
    }

    pub fn take(&mut self) -> Result<Option<Update>, String> {
        let mut mailbox = self
            .shared
            .mailbox
            .lock()
            .map_err(|error| error.to_string())?;
        let output = mailbox.output.take();
        if output.is_none() {
            if self.thread.as_ref().is_some_and(JoinHandle::is_finished) {
                return Err("Live reader has exited".to_owned());
            }
        }

        Ok(output)
    }

    pub fn stop(&mut self) {
        self.signal_stop();
        if let Some(worker) = self.thread.take() {
            // Cancellation interrupts queries; never let a blocked filesystem stall the UI exit.
            for _attempt in 0..80 {
                if worker.is_finished() {
                    if worker.join().is_err() {
                        eprintln!("flightdeck GPUI reader panicked");
                    }
                    return;
                }
                thread::sleep(Duration::from_millis(25));
            }
            eprintln!("flightdeck GPUI reader did not stop within two seconds");
        }
    }

    pub fn retire(mut self) -> Option<JoinHandle<()>> {
        self.signal_stop();
        self.thread.take()
    }

    fn signal_stop(&self) {
        self.shared.stopped.store(true, Ordering::Release);
        if let Err(error) = self.shared.cancellation.cancel() {
            eprintln!("flightdeck GPUI reader cancellation: {error}");
        }
        self.shared.wake.notify_one();
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn worker_run(root: &std::path::Path, shared: &Shared) {
    let mut command = None;
    let mut reader = Reader {
        session: None,
        system: crate::live_system::SystemSamples::new(),
        last_catalog: None,
        last_generation: 0,
        last_route: String::new(),
        pending_generation: None,
        retry_after: Instant::now(),
    };
    for _step in 0..WORKER_STEPS_MAX {
        if shared.stopped.load(Ordering::Acquire) {
            return;
        }
        match shared.mailbox.lock() {
            Ok(mut mailbox) => {
                if let Some(next) = mailbox.request.take() {
                    command = Some(next);
                    reader.retry_after = Instant::now();
                }
            }
            Err(error) => {
                eprintln!("flightdeck GPUI reader mailbox: {error}");
                shared.stopped.store(true, Ordering::Release);
                return;
            }
        }
        worker_sample(&mut reader.system);
        let pause = match &mut command {
            Some(command) => {
                if let Some(output) = reader_read(&mut reader, root, command, shared) {
                    worker_publish(shared, command.generation, output);
                }
                worker_pause(&reader, command)
            }
            None => POLL_INTERVAL,
        };
        worker_pause_for(shared, pause);
    }
    if let Some(command) = command {
        worker_publish(
            shared,
            command.generation,
            Update::Failed {
                generation: command.generation,
                message: "Live reader reached its seven-day session limit".to_owned(),
            },
        );
    }
    shared.stopped.store(true, Ordering::Release);
}

/// How long the worker waits before its next step. A page whose readings were just published
/// with its App histories pending has them read at once, since it shows them as being read.
fn worker_pause(reader: &Reader, command: &Command) -> Duration {
    if reader.last_generation == command.generation {
        if reader.pending_generation == Some(command.generation) {
            return Duration::ZERO;
        }
    }
    POLL_INTERVAL
}

/// Pauses the worker for `pause`, or until a request or a stop arrives, so a page the reader
/// asks for is read at once.
fn worker_pause_for(shared: &Shared, pause: Duration) {
    match shared.mailbox.lock() {
        Ok(mailbox) => {
            if mailbox.request.is_some() || shared.stopped.load(Ordering::Acquire) {
                return;
            }
            if let Err(error) = shared.wake.wait_timeout(mailbox, pause) {
                eprintln!("flightdeck GPUI reader pause: {error}");
                shared.stopped.store(true, Ordering::Release);
            }
        }
        Err(error) => {
            eprintln!("flightdeck GPUI reader mailbox: {error}");
            shared.stopped.store(true, Ordering::Release);
        }
    }
}

/// Samples the readings the collector does not record, once in each wall-clock second.
fn worker_sample(system: &mut crate::live_system::SystemSamples) {
    match std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) {
        Ok(now) => system.sample(now.as_secs()),
        Err(error) => eprintln!("flightdeck system clock precedes 1970: {error}"),
    }
}

fn worker_publish(shared: &Shared, generation: u64, output: Update) {
    match shared.mailbox.lock() {
        Ok(mut mailbox) => {
            // Publishing and accepting requests use one lock, so supersession is atomic.
            if generation == mailbox.generation {
                mailbox.output = Some(output);
            }
        }
        Err(error) => {
            eprintln!("flightdeck GPUI reader publication: {error}");
            shared.stopped.store(true, Ordering::Release);
        }
    }
}

fn reader_read(
    reader: &mut Reader,
    root: &std::path::Path,
    command: &mut Command,
    shared: &Shared,
) -> Option<Update> {
    if Instant::now() < reader.retry_after {
        return None;
    }
    match reader_snapshot(reader, root, command, shared) {
        Ok(Some(snapshot)) => Some(Update::Ready(snapshot)),
        Ok(None) => None,
        Err(message) => {
            reader.retry_after = Instant::now() + RETRY_INTERVAL;
            reader.last_generation = 0;
            Some(Update::Failed {
                generation: command.generation,
                message,
            })
        }
    }
}

fn reader_snapshot(
    reader: &mut Reader,
    root: &std::path::Path,
    command: &mut Command,
    shared: &Shared,
) -> Result<Option<Box<Snapshot>>, String> {
    let started = Instant::now();
    let published = reader_published(reader, command.generation);
    if command.mode == SourceMode::Recorded {
        if published {
            return Ok(None);
        }
    }
    if command.request.route == "computer" {
        return Ok(reader_computer(reader, command));
    }
    if published {
        if reader_snapshot_is_pinned(&command.request) {
            return Ok(None);
        }
    }
    if reader.session.is_none() {
        reader.session = Some(
            LiveSession::open_with(root, Arc::clone(&shared.cancellation), command.epoch)
                .map_err(|error| error.to_string())?,
        );
    }
    let session = reader.session.as_mut().ok_or("Live session did not open")?;
    session
        .begin(command.epoch)
        .map_err(|error| error.to_string())?;
    let poll = match command.request.route.as_str() {
        "self" => true,
        _ => command.request.second.is_none(),
    };
    if poll {
        session.poll().map_err(|error| error.to_string())?;
    }
    let catalog = session.catalog();
    if published {
        if reader.last_catalog.as_ref() == Some(&catalog) {
            return Ok(None);
        }
    }
    let mut selection = reader_selection(session, &command.request, command.context)?;
    reader_earlier(session, &command.request, &mut selection);
    reader.system.annotate(&mut selection);
    let mut snapshot = reader_snapshot_new(command.generation, catalog, selection);
    // A newly opened page publishes its readings first, since replaying its App histories
    // from saved minutes can take seconds, and the next step reads them.
    if reader.last_route != command.request.route {
        snapshot.histories_pending = reader_route_has_histories(&command.request.route);
    }
    reader_payload(
        session,
        &mut command.request,
        command.context,
        &mut snapshot,
    )?;
    reader_snapshot_annotate_app(&reader.system, &command.request, &mut snapshot);
    snapshot.read_ms = started.elapsed().as_secs_f64() * 1000.0;
    reader_snapshot_record(reader, command, &snapshot);

    Ok(Some(snapshot))
}

/// Whether the request asks for a chosen second of a page other than `self`, which a published
/// generation already answers.
fn reader_snapshot_is_pinned(request: &Request) -> bool {
    request.second.is_some() && request.route != "self"
}

/// Remembers what the reader published, so the next command can tell whether anything changed.
fn reader_snapshot_record(reader: &mut Reader, command: &Command, snapshot: &Snapshot) {
    reader.last_catalog = Some(snapshot.catalog.clone());
    reader.last_generation = command.generation;
    reader.last_route.clone_from(&command.request.route);
    reader.pending_generation = snapshot.histories_pending.then_some(command.generation);
}

/// Marks the selected App's history with the machine's own samples on the Apps page.
fn reader_snapshot_annotate_app(
    system: &crate::live_system::SystemSamples,
    request: &Request,
    snapshot: &mut Snapshot,
) {
    if request.route != "apps" {
        return;
    }
    if let Some(app) = &request.app {
        if let Some(history) = &mut snapshot.history {
            system.annotate_app(history, app);
        }
    }
}

/// Whether the reader has published everything `generation` asks for.
fn reader_published(reader: &Reader, generation: u64) -> bool {
    if reader.last_generation != generation {
        return false;
    }
    reader.pending_generation != Some(generation)
}

/// Whether the route's pages show the selected second's top Apps.
pub(crate) fn route_ranks_apps(route: &str) -> bool {
    matches!(route, "overview" | "storage" | "cpu" | "memory" | "energy")
}

/// Whether the route's payload replays App histories from saved minutes.
fn reader_route_has_histories(route: &str) -> bool {
    matches!(
        route,
        "overview" | "cpu" | "apps" | "memory" | "energy" | "storage"
    )
}

/// A snapshot of the catalog and selection, before the route's payload is read.
fn reader_snapshot_new(generation: u64, catalog: Value, selection: Value) -> Box<Snapshot> {
    Box::new(Snapshot {
        generation,
        catalog,
        selection,
        pages: BTreeMap::new(),
        apps: None,
        detail: None,
        history: None,
        cpu_detail: None,
        cpu_history: None,
        resident_history: None,
        app_attribution_history: None,
        overview_app_histories: None,
        histories_pending: false,
        process_history: None,
        rankings: None,
        app_ranks: None,
        app_parts: None,
        read_ms: 0.0,
    })
}

fn reader_computer(reader: &mut Reader, command: &Command) -> Option<Box<Snapshot>> {
    if reader.last_generation == command.generation {
        return None;
    }
    let catalog = reader
        .session
        .as_ref()
        .map_or(Value::Null, LiveSession::catalog);
    reader.last_generation = command.generation;
    reader.last_route.clone_from(&command.request.route);
    reader.pending_generation = None;
    Some(Box::new(Snapshot {
        generation: command.generation,
        catalog,
        selection: Value::Null,
        pages: BTreeMap::new(),
        apps: None,
        detail: None,
        history: None,
        cpu_detail: None,
        cpu_history: None,
        resident_history: None,
        app_attribution_history: None,
        overview_app_histories: None,
        histories_pending: false,
        process_history: None,
        rankings: None,
        app_ranks: None,
        app_parts: None,
        read_ms: 0.0,
    }))
}

fn reader_selection(
    session: &mut LiveSession,
    request: &Request,
    context: Context,
) -> Result<Value, String> {
    if matches!(request.route.as_str(), "computer" | "self") {
        return Ok(Value::Null);
    }
    if context == Context::Keep {
        return reader_selection_kept(session, request);
    }
    let selection = match request.second {
        Some(second) => session.select(second, 0).map_err(|error| error.to_string()),
        None => reader_latest(session),
    }?;
    if let Some(identity) = &request.identity {
        if let Some(second) = request.second {
            let index = selection_alternative(&selection, identity)?;
            let exact = session
                .select(second, index)
                .map_err(|error| error.to_string())?;
            identity_validate(&exact["selected"], identity)?;
            return Ok(exact);
        }
    }

    Ok(selection)
}

/// Selects the observation a hover names at its second, without the context around it.
fn reader_selection_kept(session: &mut LiveSession, request: &Request) -> Result<Value, String> {
    let second = request
        .second
        .ok_or("A kept context reads a chosen second")?;
    let identity = request
        .identity
        .as_ref()
        .ok_or("A kept context reads a named observation")?;

    let selection = session
        .inspect(second, identity)
        .map_err(|error| error.to_string())?;
    identity_validate(&selection["selected"], identity)?;

    Ok(selection)
}

/// Adds the saved readings older than the selection's full-detail context, which the charts of
/// a longer range draw. A failed read leaves the charts at their full-detail context and says
/// why beside it.
fn reader_earlier(session: &mut LiveSession, request: &Request, selection: &mut Value) {
    if request.earlier == Earlier::Skip || request.route == "apps" {
        return;
    }
    let Some(second) = selection["second"].as_u64() else {
        return;
    };
    let Some(fields) = selection.as_object_mut() else {
        return;
    };
    match session.earlier_context(second) {
        Ok(Value::Object(tail)) => fields.extend(tail),
        Ok(_) => unreachable!("The older tail is an object"),
        Err(error) => {
            fields.insert("earlier_error".into(), error.to_string().into());
        }
    }
}

fn reader_latest(session: &mut LiveSession) -> Result<Value, String> {
    let catalog = session.catalog();
    if !catalog["latest_observation"].is_null() {
        return session.follow_latest().map_err(|error| error.to_string());
    }

    if let Some(second) = catalog["last_second"].as_u64() {
        let first = session
            .select(second, 0)
            .map_err(|error| error.to_string())?;
        let alternatives = first["alternatives"]
            .as_array()
            .ok_or("Saved observation alternatives unavailable")?;
        if alternatives.len() > ALTERNATIVES_MAX {
            return Err("Saved observation alternatives exceed 4096".into());
        }

        if alternatives.len() <= 1 {
            return Ok(first);
        }

        return session
            .select(second, alternatives.len() - 1)
            .map_err(|error| error.to_string());
    }

    if let Some(error) = catalog["history_error"].as_str() {
        return Err(format!("Saved recording history unavailable: {error}"));
    }

    Ok(
        serde_json::json!({"second":null, "selected":null, "alternatives":[],
        "summary":null, "trends":[], "context_seconds":900}),
    )
}

/// Reads the payload of the page. The page's App histories visit the selected App's saved parts
/// of the window several times, so the payload that reads them decodes each window once.
fn reader_payload(
    session: &mut LiveSession,
    request: &mut Request,
    context: Context,
    snapshot: &mut Snapshot,
) -> Result<(), String> {
    if !snapshot.histories_pending {
        if let Some(key) = &request.app {
            let kind = app_key_kind(&key.kind)?;
            let bytes = app_key_bytes(&key.bytes_hex)?;
            return session.with_parts_read(kind, &bytes, |session| {
                reader_payload_read(session, request, context, snapshot)
            });
        }
    }

    reader_payload_read(session, request, context, snapshot)
}

fn reader_payload_read(
    session: &mut LiveSession,
    request: &mut Request,
    context: Context,
    snapshot: &mut Snapshot,
) -> Result<(), String> {
    // A missing second has no raw payload, and must never borrow a sibling observation.
    if snapshot.selection["selected"].is_null() {
        return Ok(());
    }
    let tables: &[&str] = match request.route.as_str() {
        "overview" => &["fans"],
        _ => &[],
    };
    for table in tables {
        let page = reader_page(session, table, 0)?;
        identity_validate(&page["observation"], &snapshot.selection["selected"])?;
        snapshot.pages.insert((*table).to_owned(), page);
    }
    if request.route == "apps" {
        reader_apps(session, request, context, snapshot)?;
    }
    let ranked = route_ranks_apps(&request.route);
    if ranked {
        snapshot.rankings = Some(Arc::new(crate::live_rankings::from_selected(
            session.selected_ranks().map_err(|error| error.to_string()),
        )));
    }
    if request.route == "cpu" {
        let detail = session.cpu_detail().map_err(|error| error.to_string())?;
        snapshot.cpu_detail = Some(crate::live_cpu::CPUActivity::decode(
            detail,
            &snapshot.selection["selected"],
        )?);
        if context == Context::Select {
            reader_cpu_history(session, request, snapshot)?;
        }
    }
    // Keeping App parts replays the saved minutes of each window that keeps none, so it follows
    // the histories the page shows, and a hover's own read keeps none.
    if !snapshot.histories_pending {
        reader_payload_histories(session, request, snapshot);
        if request.route == "apps" {
            if request.load_process_history {
                reader_app_parts(session, request, snapshot)?;
            }
        }
    }
    // Ranking the window replays the saved minutes that keep no ranks, so it comes with the
    // page's App histories, which a hover's own read leaves out. A newly opened page brings the
    // ranks its window keeps, in memory or in its sealed minutes, with its first readings, so a
    // hover during its history read shows the top Apps they hold at once.
    if ranked {
        if request.load_app_attribution_history {
            if snapshot.histories_pending {
                reader_app_ranks(session.rank_window_kept());
            } else {
                reader_app_ranks(session.rank_window());
            }
            snapshot.app_ranks = Some(session.app_ranks());
        }
    }

    Ok(())
}

/// Reports a failed ranking of the selected window's observations, which a hover shows at once.
/// A window ranked in part still serves the observations it holds, and the next read goes on
/// from them.
fn reader_app_ranks(ranking: persistence::Result<()>) {
    match ranking {
        Ok(()) => {}
        Err(persistence::Error::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted => {}
        Err(error) => eprintln!("App ranks of the selected window unavailable: {error}"),
    }
}

/// Keeps the selected App's part of each observation of the selected window, so a hover on
/// the Apps page shows any of their App details at once. A window kept in part still serves
/// the observations it holds, and the next read goes on from them.
fn reader_app_parts(
    session: &mut LiveSession,
    request: &Request,
    snapshot: &mut Snapshot,
) -> Result<(), String> {
    let Some(key) = &request.app else {
        return Ok(());
    };

    let kind = app_key_kind(&key.kind)?;
    let bytes = app_key_bytes(&key.bytes_hex)?;
    match session.part_window(kind, &bytes) {
        Ok(()) => {}
        Err(persistence::Error::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted => {}
        Err(error) => eprintln!("App parts of the selected window unavailable: {error}"),
    }
    snapshot.app_parts = Some(session.app_parts());

    Ok(())
}

fn reader_payload_histories(session: &LiveSession, request: &Request, snapshot: &mut Snapshot) {
    if request.load_app_attribution_history {
        if request.route == "overview" {
            reader_overview_app_histories(session, snapshot);
        } else {
            if let Some(metric) =
                crate::live_app_attribution_history::Metric::from_route(&request.route)
            {
                snapshot.app_attribution_history = Some(reader_app_attribution_history(
                    session,
                    &snapshot.selection,
                    metric,
                ));
            }
        }
    }
    if matches!(request.route.as_str(), "memory" | "overview") {
        if request.load_resident_history {
            reader_resident_history(session, snapshot);
        }
    }
}

fn reader_app_attribution_history(
    session: &LiveSession,
    selection: &Value,
    metric: crate::live_app_attribution_history::Metric,
) -> Arc<crate::live_app_attribution_history::AppAttributionHistory> {
    reader_app_attribution_result(
        session.app_attribution_history(metric.source()),
        selection,
        metric,
    )
}

fn reader_app_attribution_result(
    result: persistence::Result<Value>,
    selection: &Value,
    metric: crate::live_app_attribution_history::Metric,
) -> Arc<crate::live_app_attribution_history::AppAttributionHistory> {
    use crate::live_app_attribution_history::AppAttributionHistory;
    Arc::new(match result {
        Ok(payload) => AppAttributionHistory::prepare(metric, &payload, selection),
        Err(error) => {
            AppAttributionHistory::Unavailable(crate::bounded::bounded(&error.to_string(), 512))
        }
    })
}

fn reader_overview_app_histories(session: &LiveSession, snapshot: &mut Snapshot) {
    reader_overview_app_histories_result(session.app_attribution_histories(), snapshot);
}

fn reader_overview_app_histories_result(
    result: persistence::Result<Box<viewer::AppAttributionHistories>>,
    snapshot: &mut Snapshot,
) {
    use crate::live_app_attribution_history::{AppAttributionHistory, Metric};
    use crate::live_overview_app_histories::OverviewAppHistories;
    let histories = match result {
        Ok(histories) => histories,
        Err(error) => {
            let unavailable = Arc::new(AppAttributionHistory::Unavailable(
                crate::bounded::bounded(&error.to_string(), 512),
            ));
            snapshot.overview_app_histories = Some(Arc::new(OverviewAppHistories {
                cpu: Arc::clone(&unavailable),
                power: Arc::clone(&unavailable),
                disk: unavailable,
            }));
            return;
        }
    };

    snapshot.overview_app_histories = Some(Arc::new(OverviewAppHistories {
        cpu: reader_app_attribution_result(histories.cpu, &snapshot.selection, Metric::Cpu),
        power: reader_app_attribution_result(histories.power, &snapshot.selection, Metric::Power),
        disk: reader_app_attribution_result(histories.disk, &snapshot.selection, Metric::Disk),
    }));
}

fn reader_resident_history(session: &LiveSession, snapshot: &mut Snapshot) {
    let history = match session.app_resident_history() {
        Ok(payload) => {
            crate::live_resident_history::ResidentHistory::prepare(&payload, &snapshot.selection)
        }
        Err(error) => crate::live_resident_history::ResidentHistory::Unavailable(
            crate::bounded::bounded(&error.to_string(), 512),
        ),
    };
    snapshot.resident_history = Some(Arc::new(history));
}

fn reader_cpu_history(
    session: &mut LiveSession,
    request: &mut Request,
    snapshot: &mut Snapshot,
) -> Result<(), String> {
    let total = snapshot
        .cpu_detail
        .as_ref()
        .ok_or("Missing selected CPU detail")?
        .cpus
        .len();
    let offset = usize::try_from(request.cpu_offset).map_err(|error| error.to_string())?;
    if offset >= total {
        request.cpu_offset = 0;
    }
    let payload = session
        .cpu_history(request.cpu_offset)
        .map_err(|error| error.to_string())?;
    snapshot.cpu_history = Some(Arc::new(crate::live_cpu_history::CPUHistory::decode(
        payload,
        &snapshot.selection,
    )?));

    Ok(())
}

fn reader_apps(
    session: &LiveSession,
    request: &mut Request,
    context: Context,
    snapshot: &mut Snapshot,
) -> Result<(), String> {
    let apps = reader_apps_page(session, request.apps_offset)?;
    identity_validate(&apps["selected"], &snapshot.selection["selected"])?;
    reader_apps_remember(request, &apps)?;
    if let Some(key) = &request.app {
        let kind = app_key_kind(&key.kind)?;
        let bytes = app_key_bytes(&key.bytes_hex)?;
        let detail = clamped_page(request.members_offset, |offset| {
            session
                .app_detail(kind, &bytes, offset)
                .map_err(|error| error.to_string())
        })?;
        if !detail.is_null() {
            identity_validate(&detail["selected"], &snapshot.selection["selected"])?;
        }
        if context == Context::Select {
            if !snapshot.histories_pending {
                let history = session
                    .app_history(kind, &bytes)
                    .map_err(|error| error.to_string())?;
                identity_validate(&history["selected"], &snapshot.selection["selected"])?;
                snapshot.history = Some(history);
            }
        }
        snapshot.detail = Some(detail);
    }
    if request.load_process_history {
        if !snapshot.histories_pending {
            reader_process_history(session, request, snapshot);
        }
    }
    snapshot.apps = Some(apps);

    Ok(())
}

fn reader_process_history(session: &LiveSession, request: &Request, snapshot: &mut Snapshot) {
    use crate::live_process_history::ProcessHistory;
    let read = || -> Result<ProcessHistory, String> {
        let Some(filter) = request.executable.as_deref() else {
            let key = request.app.as_ref().ok_or("Selected App unavailable")?;
            let kind = app_key_kind(&key.kind)?;
            let bytes = app_key_bytes(&key.bytes_hex)?;
            let payload = session
                .app_process_groups_history(kind, &bytes)
                .map_err(|error| error.to_string())?;

            return Ok(ProcessHistory::prepare_all(
                payload,
                request,
                &snapshot.selection,
            ));
        };
        let name = crate::live_process_history::name_bytes(filter)?;
        let Some(name) = name else {
            return Err(ProcessHistory::filter_reason(Some(filter))
                .unwrap_or("Named process history unavailable")
                .into());
        };

        let key = request.app.as_ref().ok_or("Selected App unavailable")?;
        let kind = app_key_kind(&key.kind)?;
        let bytes = app_key_bytes(&key.bytes_hex)?;
        let payload = session
            .app_process_history(kind, &bytes, &name)
            .map_err(|error| error.to_string())?;
        Ok(ProcessHistory::prepare(
            payload,
            request,
            &snapshot.selection,
        ))
    };
    let history = match read() {
        Ok(history) => history,
        Err(error) => ProcessHistory::Unavailable(crate::bounded::bounded(&error, 512)),
    };
    snapshot.process_history = Some(Arc::new(history));
}

fn reader_apps_remember(request: &mut Request, apps: &Value) -> Result<(), String> {
    // Choosing the default once keeps its captured key stable through reordering and exit.
    if request.app.is_none() {
        request.app = app_first_key(apps)?;
    }
    Ok(())
}

fn reader_page(session: &LiveSession, table: &str, offset: u64) -> Result<Value, String> {
    let first = session.page(table, 0).map_err(|error| error.to_string())?;
    let offset = page_offset(offset, &first["total"])?;
    if offset == 0 {
        return Ok(first);
    }
    session
        .page(table, offset)
        .map_err(|error| error.to_string())
}

fn reader_apps_page(session: &LiveSession, offset: u32) -> Result<Value, String> {
    clamped_page(offset, |offset| {
        session.apps_page(offset).map_err(|error| error.to_string())
    })
}

/// The page `read` gives at `requested`, or its first page where `requested` lies past the
/// end. A null page, the detail of an App absent from its second, has no end.
pub(crate) fn clamped_page(
    requested: u32,
    read: impl Fn(u32) -> Result<Value, String>,
) -> Result<Value, String> {
    let first = read(0)?;
    if first.is_null() {
        return Ok(first);
    }

    let offset = page_offset(u64::from(requested), &first["page"]["total"])?;
    if offset == 0 {
        return Ok(first);
    }
    read(u32::try_from(offset).map_err(|error| error.to_string())?)
}

fn page_offset(requested: u64, total: &Value) -> Result<u64, String> {
    let total = total.as_u64().ok_or("Missing page total")?;
    if requested < total {
        Ok(requested)
    } else {
        Ok(0)
    }
}

fn selection_alternative(selection: &Value, identity: &Value) -> Result<usize, String> {
    let target = identity_signature(identity)?;
    let alternatives = selection["alternatives"]
        .as_array()
        .ok_or_else(|| "Selection alternatives are missing".to_owned())?;
    if alternatives.len() > ALTERNATIVES_MAX {
        return Err("Selection alternatives exceed the bound".to_owned());
    }
    for (index, candidate) in alternatives.iter().enumerate() {
        if identity_signature(candidate)? == target {
            return Ok(index);
        }
    }

    Err("The selected observation is no longer available".to_owned())
}

pub(crate) fn identity_validate(observed: &Value, selected: &Value) -> Result<(), String> {
    if identity_signature(observed)? != identity_signature(selected)? {
        return Err("Live payload does not match the selected observation".to_owned());
    }

    Ok(())
}

pub(crate) fn identity_signature(value: &Value) -> Result<(String, String, u64), String> {
    let publication = identity_publication(value)?;
    let stamp = match value.get("stamp") {
        Some(stamp) => stamp,
        None => value,
    };

    Ok((
        identity_text(&stamp["run"])?,
        identity_text(&stamp["boot"])?,
        publication,
    ))
}

pub(crate) fn identity_publication(value: &Value) -> Result<u64, String> {
    let stamp = match value.get("stamp") {
        Some(stamp) => stamp,
        None => value,
    };

    match &stamp["publication"] {
        Value::String(text) => text.parse::<u64>().map_err(|error| error.to_string()),
        value => value
            .as_u64()
            .ok_or_else(|| "Missing observation publication".to_owned()),
    }
}

fn identity_text(value: &Value) -> Result<String, String> {
    if let Some(text) = value.as_str() {
        if text.len() == 32 {
            if text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Ok(text.to_ascii_lowercase());
            }
        }
        return Err("Invalid observation identity".to_owned());
    }
    let parts = value
        .as_array()
        .ok_or_else(|| "Missing observation identity".to_owned())?;
    if parts.len() != 2 {
        return Err("Invalid observation identity words".to_owned());
    }
    let first = identity_word(&parts[0])?;
    let second = identity_word(&parts[1])?;

    Ok(format!("{first:016x}{second:016x}"))
}

fn identity_word(value: &Value) -> Result<u64, String> {
    match value {
        Value::String(text) => text.parse::<u64>().map_err(|error| error.to_string()),
        value => value
            .as_u64()
            .ok_or_else(|| "Invalid observation identity word".to_owned()),
    }
}

fn app_first_key(apps: &Value) -> Result<Option<AppKey>, String> {
    let apps = apps["apps"]
        .as_array()
        .ok_or_else(|| "Missing live App rows".to_owned())?;
    if apps.len() > 512 {
        return Err("Live App page exceeds the row bound".to_owned());
    }
    let Some(app) = apps.first() else {
        return Ok(None);
    };
    let kind = app["key"]["kind"]
        .as_str()
        .ok_or_else(|| "Missing App key kind".to_owned())?;
    let bytes = app["key"]["bytes_hex"]
        .as_str()
        .ok_or_else(|| "Missing App key bytes".to_owned())?;

    Ok(Some(AppKey {
        kind: kind.to_owned(),
        bytes_hex: bytes.to_owned(),
    }))
}

pub(crate) fn app_key_kind(kind: &str) -> Result<KeyKind, String> {
    match kind {
        "bundle" => Ok(KeyKind::Bundle),
        "executable" => Ok(KeyKind::Executable),
        _ => Err("Invalid live App key kind".to_owned()),
    }
}

pub(crate) fn app_key_bytes(text: &str) -> Result<Vec<u8>, String> {
    if text.len() > APP_KEY_BYTES_MAX * 2 {
        return Err("Live App key exceeds the byte bound".to_owned());
    }
    if !text.len().is_multiple_of(2) {
        return Err("Live App key has an incomplete hexadecimal byte".to_owned());
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().as_chunks::<2>().0 {
        let high = char::from(pair[0]).to_digit(16);
        let low = char::from(pair[1]).to_digit(16);
        match (high, low) {
            (Some(high), Some(low)) => {
                bytes.push(u8::try_from(high * 16 + low).map_err(|error| error.to_string())?);
            }
            _ => return Err("Live App key contains a non-hexadecimal byte".to_owned()),
        }
    }

    Ok(bytes)
}

fn request_validate(request: &Request) -> Result<(), String> {
    if let Some(filter) = &request.executable {
        drop(crate::live_process_history::name_bytes(filter)?);
    }
    if !request.cpu_offset.is_multiple_of(32) {
        return Err("Logical CPU history offset must be a page of 32".into());
    }

    if request.cpu_offset >= 1024 {
        return Err("Logical CPU history offset exceeds the CPU domain".into());
    }

    if !matches!(
        request.route.as_str(),
        "overview"
            | "cpu"
            | "apps"
            | "memory"
            | "gpu"
            | "energy"
            | "network"
            | "storage"
            | "battery"
            | "computer"
            | "self"
    ) {
        return Err("Unknown live route".to_owned());
    }
    if request.table.len() > 64 {
        return Err("Live table name exceeds the byte bound".to_owned());
    }
    if let Some(identity) = &request.identity {
        identity_signature(identity)?;
        if identity.to_string().len() > 4096 {
            return Err("Live observation identity exceeds the byte bound".to_owned());
        }
    }
    if let Some(key) = &request.app {
        app_key_kind(&key.kind)?;
        app_key_bytes(&key.bytes_hex)?;
    }

    Ok(())
}

#[cfg(test)]
#[path = "live_app_key_tests.rs"]
pub(crate) mod app_key_tests;

#[cfg(test)]
#[path = "live_source_tests.rs"]
pub(crate) mod tests;
