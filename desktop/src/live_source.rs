use std::collections::BTreeMap;
use std::path::PathBuf;
#[cfg(feature = "automation")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::live_source_mode::SourceMode;
use serde_json::Value;
use viewer::live_app_response::KeyKind;
use viewer::{Cancellation, LiveSession};

#[path = "live_store_reader.rs"]
mod store_reader;
pub use store_reader::ownership::{Part, PartsWindow, RanksWindow};
pub use store_reader::presentation::Presentation;

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
    pub selection: Presentation<Value>,
    pub pages: BTreeMap<String, Presentation<Value>>,
    pub apps: Option<Presentation<Value>>,
    pub detail: Option<Presentation<Value>>,
    pub history: Option<Presentation<Value>>,
    pub cpu_detail: Option<Presentation<crate::live_cpu::CPUActivity>>,
    pub cpu_history: Option<Arc<Presentation<crate::live_cpu_history::CPUHistory>>>,
    pub resident_history: Option<Arc<Presentation<crate::live_resident_history::ResidentHistory>>>,
    pub app_attribution_history:
        Option<Arc<Presentation<crate::live_app_attribution_history::AppAttributionHistory>>>,
    pub overview_app_histories:
        Option<Arc<crate::live_overview_app_histories::OverviewAppHistories>>,
    pub process_history: Option<Arc<Presentation<crate::live_process_history::ProcessHistory>>>,
    pub rankings: Option<Arc<Presentation<crate::live_rankings::Rankings>>>,
    /// The top Apps of each observation of the selected window the viewer has ranked, so a
    /// hover shows its observation's at once.
    pub app_ranks: Option<Arc<RanksWindow>>,
    /// The selected App's part of each observation of the selected window the viewer has kept,
    /// so a hover on the Apps page shows its observation's App at once.
    pub app_parts: Option<Arc<PartsWindow>>,
    /// The page's App histories, or the older tail its charts draw, are still being read, and
    /// a later snapshot carries them.
    pub histories_pending: bool,
    pub read_ms: f64,
}

pub enum Update {
    Ready(Box<Snapshot>),
    Failed { generation: u64, message: String },
}

struct Shared {
    #[cfg(test)]
    measurement_staged: AtomicBool,
    #[cfg(test)]
    measurement_taken: AtomicBool,
    #[cfg(test)]
    measurement_raw: AtomicBool,
    #[cfg(test)]
    measurement_gate: Mutex<Option<measurement::Gate>>,
    #[cfg(test)]
    measurement_wake: Condvar,
    #[cfg(feature = "automation")]
    requests: AtomicU64,
    mailbox: Mutex<Mailbox>,
    /// Wakes the worker from its pause when a request or a stop arrives.
    wake: Condvar,
    stopped: AtomicBool,
    cancellation: Arc<Cancellation>,
}

struct Mailbox {
    #[cfg(test)]
    frozen_payloads: BTreeMap<&'static str, Value>,
    #[cfg(test)]
    taken_payloads: BTreeMap<&'static str, Value>,
    #[cfg(test)]
    output_complete: bool,
    generation: u64,
    request: Option<Command>,
    output: Option<Update>,
    /// The window reading through the worker closed, so the worker drops its page and keeps its
    /// session following the recording until the next window asks for one.
    rest: bool,
    /// The worker reads no page, so its step opens the session a request reads through, or polls
    /// it, and cancelling would discard the open or cut the poll short for the request to redo.
    resting: bool,
    /// How the reader rests while it reads no page.
    rest_kind: Rest,
}

/// How a reader that reads no page spends its rest.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Rest {
    /// Its session follows the recording, so the next page reads only what arrived since the
    /// reader's last step.
    Following,
    /// Its session stays where it is, since its window draws its page from the store's View, and
    /// the next page reads what arrived since the rest began.
    Idle,
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
    Restore,
}

struct Reader {
    session: Option<LiveSession>,
    store: Option<store_reader::StoreReader>,
    last_catalog: Option<Value>,
    last_generation: u64,
    /// The route of the last published snapshot, so a newly opened page is told apart.
    last_route: String,
    /// What the last published snapshot left pending, which the worker reads at once.
    pending: Option<Pending>,
    retry_after: Instant,
}

/// A published snapshot that left its App histories, or the older tail its charts draw, to a
/// later step.
#[derive(Clone, Copy)]
struct Pending {
    generation: u64,
    histories: Histories,
}

/// Whether a step reads the page's App histories or leaves them to the next step, and how far it
/// ranks the Apps of the selected window.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Histories {
    /// Reads them, and ranks every observation of the window.
    Read,
    /// Reads them with a newly opened page's first readings, which the session found replay no
    /// saved minute. Ranks only what the window keeps, as `Defer` does, since ranking the rest
    /// replays the minutes that keep no ranks, and leaves the rest to a `Read` step after it.
    Early,
    /// Leaves them to the next step, and ranks only what the window keeps.
    Defer,
}

const POLL_INTERVAL: Duration = Duration::from_millis(250);
const RETRY_INTERVAL: Duration = Duration::from_secs(1);
const WORKER_STEPS_MAX: u64 = 7 * 24 * 60 * 60 * 4;
// Bounds the windows one reader serves, each of which gets WORKER_STEPS_MAX steps.
const WORKER_LEASES_MAX: u64 = 1 << 20;
const ALTERNATIVES_MAX: usize = 4096;
// Match the recording format's maximum captured program path length.
const APP_KEY_BYTES_MAX: usize = 4095;
const _: () = {
    assert!(WORKER_STEPS_MAX > 0);
    assert!(WORKER_LEASES_MAX > 0);
    assert!(APP_KEY_BYTES_MAX > 0);
    assert!(ALTERNATIVES_MAX <= 4096);
};

impl Worker {
    #[cfg(feature = "automation")]
    pub(crate) fn requests_submitted(&self) -> u64 {
        self.shared.requests.load(Ordering::Relaxed)
    }

    pub fn start(root: PathBuf) -> std::io::Result<Self> {
        Self::start_mode(root, SourceMode::Live, Rest::Following)
    }

    pub(crate) fn start_idle(root: PathBuf) -> std::io::Result<Self> {
        Self::start_mode(root, SourceMode::Live, Rest::Idle)
    }

    pub fn start_recorded(root: PathBuf) -> std::io::Result<Self> {
        Self::start_mode(root, SourceMode::Recorded, Rest::Following)
    }

    /// Starts the source-free live client over the canonical Store's independent query handle.
    #[cfg(test)]
    pub fn start_from_store(
        root: PathBuf,
        handle: viewer::store::QueryHandle,
    ) -> std::io::Result<Self> {
        Self::start_reader(root, SourceMode::Live, Rest::Idle, Some(handle))
    }

    pub fn mode(&self) -> SourceMode {
        self.mode
    }

    /// Whether the reader still answers requests.
    pub fn running(&self) -> bool {
        if self.shared.stopped.load(Ordering::Acquire) {
            return false;
        }

        self.thread
            .as_ref()
            .is_some_and(|thread| !thread.is_finished())
    }

    #[cfg(test)]
    pub fn thread_id(&self) -> Option<thread::ThreadId> {
        self.thread.as_ref().map(|thread| thread.thread().id())
    }

    fn start_mode(root: PathBuf, mode: SourceMode, rest: Rest) -> std::io::Result<Self> {
        Self::start_reader(root, mode, rest, None)
    }

    fn start_reader(
        root: PathBuf,
        mode: SourceMode,
        rest: Rest,
        store: Option<viewer::store::QueryHandle>,
    ) -> std::io::Result<Self> {
        let shared = Arc::new(Shared::with_rest(rest));
        let control = Arc::clone(&shared);
        let observation =
            persistence::work::ingestion::inherit(persistence::work::ingestion::Producer::Worker);
        let thread = thread::Builder::new()
            .name("flightdeck-live".to_owned())
            .stack_size(4 * 1024 * 1024)
            .spawn(move || {
                let observation = observation.enter();
                worker_run(&root, &control, store);
                drop(observation);
            })?;

        Ok(Self {
            shared,
            thread: Some(thread),
            mode,
        })
    }

    pub fn request(&mut self, request: Request) -> Result<u64, String> {
        self.submit(request, Context::Select)
    }

    pub(crate) fn restore(&mut self, request: Request) -> Result<u64, String> {
        assert!(request.second.is_some());
        self.submit(request, Context::Restore)
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
        let mut mailbox = self.mailbox()?;
        let generation = mailbox
            .generation
            .checked_add(1)
            .ok_or_else(|| "Live request generation exhausted".to_owned())?;
        if !mailbox.resting {
            self.shared
                .cancellation
                .cancel()
                .map_err(|error| error.to_string())?;
        }
        mailbox.generation = generation;
        mailbox.output = None;
        mailbox.request = Some(Command {
            generation,
            epoch: self.shared.cancellation.epoch(),
            mode: self.mode,
            request,
            context,
        });
        #[cfg(feature = "automation")]
        self.shared.requests.fetch_add(1, Ordering::Relaxed);
        self.shared.wake.notify_one();

        Ok(generation)
    }

    pub fn take(&mut self) -> Result<Option<Update>, String> {
        let mut mailbox = self.mailbox()?;
        let output = mailbox.output.take();
        #[cfg(test)]
        if output.is_some() {
            mailbox.taken_payloads = std::mem::take(&mut mailbox.frozen_payloads);
        }
        #[cfg(test)]
        self.shared.measurement_taken.store(
            output.is_some() && mailbox.output_complete,
            Ordering::Release,
        );
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

    /// Hands the reader to the next window without ingesting while no page reads it.
    /// The session stays where it is until a request arrives, and this handle refuses requests.
    pub fn hand_over(&mut self) -> Result<Option<Worker>, String> {
        // A recording the user opened in the window is read-only, and the window opened again
        // follows the live recording.
        if self.mode != SourceMode::Live {
            return Ok(None);
        }
        if self.shared.stopped.load(Ordering::Acquire) {
            return Err("Live reader has stopped".to_owned());
        }
        {
            let mut mailbox = self.mailbox()?;
            // A read the closed window asked for publishes under an older generation, which the
            // mailbox refuses.
            let generation = mailbox
                .generation
                .checked_add(1)
                .ok_or_else(|| "Live request generation exhausted".to_owned())?;
            self.shared
                .cancellation
                .cancel()
                .map_err(|error| error.to_string())?;
            mailbox.generation = generation;
            mailbox.request = None;
            mailbox.output = None;
            mailbox.rest = true;
            mailbox.rest_kind = Rest::Idle;
            self.shared.wake.notify_one();
        }

        let refusing = Shared::new();
        refusing.stopped.store(true, Ordering::Release);
        Ok(Some(Worker {
            shared: std::mem::replace(&mut self.shared, Arc::new(refusing)),
            thread: self.thread.take(),
            mode: self.mode,
        }))
    }

    fn mailbox(&self) -> Result<std::sync::MutexGuard<'_, Mailbox>, String> {
        self.shared
            .mailbox
            .lock()
            .map_err(|error| error.to_string())
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

impl Shared {
    fn new() -> Self {
        Self::with_rest(Rest::Following)
    }

    fn with_rest(rest_kind: Rest) -> Self {
        Self {
            #[cfg(test)]
            measurement_staged: AtomicBool::new(false),
            #[cfg(test)]
            measurement_taken: AtomicBool::new(false),
            #[cfg(test)]
            measurement_raw: AtomicBool::new(false),
            #[cfg(test)]
            measurement_gate: Mutex::new(None),
            #[cfg(test)]
            measurement_wake: Condvar::new(),
            #[cfg(feature = "automation")]
            requests: AtomicU64::new(0),
            mailbox: Mutex::new(Mailbox {
                #[cfg(test)]
                frozen_payloads: BTreeMap::new(),
                #[cfg(test)]
                taken_payloads: BTreeMap::new(),
                #[cfg(test)]
                output_complete: false,
                generation: 0,
                request: None,
                output: None,
                rest: false,
                resting: true,
                rest_kind,
            }),
            wake: Condvar::new(),
            stopped: AtomicBool::new(false),
            cancellation: Arc::new(Cancellation::default()),
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop();
    }
}

fn worker_run(root: &std::path::Path, shared: &Shared, store: Option<viewer::store::QueryHandle>) {
    let mut command = None;
    let mut reader = Reader {
        session: None,
        store: store.map(store_reader::StoreReader::new),
        last_catalog: None,
        last_generation: 0,
        last_route: String::new(),
        pending: None,
        retry_after: Instant::now(),
    };
    for _lease in 0..WORKER_LEASES_MAX {
        let lease = worker_lease(root, shared, &mut reader, &mut command, WORKER_STEPS_MAX);
        if lease == Lease::Ended {
            break;
        }
    }
    shared.stopped.store(true, Ordering::Release);
}

/// How a run of the worker's steps ended.
#[derive(Debug, PartialEq, Eq)]
enum Lease {
    /// A window took the resting reader, which gives that window the full step limit rather
    /// than what the windows before it left.
    Renewed,
    Ended,
}

/// Steps the worker at most `steps_max` times, until a window takes the resting reader.
fn worker_lease(
    root: &std::path::Path,
    shared: &Shared,
    reader: &mut Reader,
    command: &mut Option<Command>,
    steps_max: u64,
) -> Lease {
    for _step in 0..steps_max {
        if shared.stopped.load(Ordering::Acquire) {
            return Lease::Ended;
        }
        match worker_receive(shared, reader, command) {
            Received::Continued => {}
            Received::Taken => return Lease::Renewed,
            Received::Broken => return Lease::Ended,
        }
        let pause = worker_step(root, shared, reader, command);
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
    Lease::Ended
}

/// What the worker found in its mailbox at the top of a step.
#[derive(Debug, PartialEq, Eq)]
enum Received {
    /// The worker goes on with the page it reads, or with resting.
    Continued,
    /// A request reached the resting reader, so a window took it.
    Taken,
    /// The mailbox failed, so the worker stops.
    Broken,
}

fn worker_receive(shared: &Shared, reader: &mut Reader, command: &mut Option<Command>) -> Received {
    match shared.mailbox.lock() {
        Ok(mut mailbox) => {
            if mailbox.rest {
                mailbox.rest = false;
                *command = None;
                reader_rest(reader);
            }
            let Some(next) = mailbox.request.take() else {
                mailbox.resting = command.is_none();
                return Received::Continued;
            };
            mailbox.resting = false;
            let resting = command.is_none();
            *command = Some(next);
            reader.retry_after = Instant::now();
            if resting {
                Received::Taken
            } else {
                Received::Continued
            }
        }
        Err(error) => {
            eprintln!("flightdeck GPUI reader mailbox: {error}");
            shared.stopped.store(true, Ordering::Release);
            Received::Broken
        }
    }
}

/// Reads the page the worker reads, or keeps a resting reader following the recording, and
/// returns how long the worker waits before its next step.
fn worker_step(
    root: &std::path::Path,
    shared: &Shared,
    reader: &mut Reader,
    command: &mut Option<Command>,
) -> Duration {
    if let Some(command) = command {
        #[cfg(test)]
        if reader_pending(reader, command.generation) {
            measurement::checkpoint(shared, command.generation, measurement::Stage::Pending);
        }
        if let Some(output) = reader_read(reader, root, command, shared) {
            #[cfg(test)]
            shared.measurement_staged.store(
                reader_published(reader, command.generation),
                Ordering::Release,
            );
            #[cfg(test)]
            measurement::finished(shared, command.generation);
            worker_publish(shared, command.generation, output);
        }
        worker_pause(reader, command)
    } else {
        if worker_step_follows(shared) {
            reader_follow(reader, root, shared);
        }
        POLL_INTERVAL
    }
}

/// Whether the resting reader keeps its session following the recording.
fn worker_step_follows(shared: &Shared) -> bool {
    match shared.mailbox.lock() {
        Ok(mailbox) => mailbox.rest_kind == Rest::Following,
        Err(error) => {
            eprintln!("flightdeck GPUI reader mailbox: {error}");
            shared.stopped.store(true, Ordering::Release);
            false
        }
    }
}

/// How long the worker waits before its next step. A page whose readings were just published
/// with its App histories or its older tail pending has them read at once, since it shows them
/// as being read.
fn worker_pause(reader: &Reader, command: &Command) -> Duration {
    if reader.last_generation == command.generation {
        if reader_pending(reader, command.generation) {
            return Duration::ZERO;
        }
    }
    POLL_INTERVAL
}

/// Pauses the worker for `pause`, or until a request, a rest or a stop arrives, so a page the
/// reader asks for is read at once.
fn worker_pause_for(shared: &Shared, pause: Duration) {
    match shared.mailbox.lock() {
        Ok(mailbox) => {
            if mailbox.request.is_some() {
                return;
            }
            if mailbox.rest {
                return;
            }
            if shared.stopped.load(Ordering::Acquire) {
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

fn worker_publish(shared: &Shared, generation: u64, output: Update) {
    match shared.mailbox.lock() {
        Ok(mut mailbox) => {
            // Publishing and accepting requests use one lock, so supersession is atomic.
            if generation == mailbox.generation {
                #[cfg(test)]
                {
                    mailbox.output_complete = shared.measurement_staged.load(Ordering::Acquire);
                    mailbox.frozen_payloads = measurement::take_payloads();
                }
                mailbox.output = Some(output);
            }
        }
        Err(error) => {
            eprintln!("flightdeck GPUI reader publication: {error}");
            shared.stopped.store(true, Ordering::Release);
        }
    }
}

/// Drops the page the closed window read, so the next window's page reads as newly opened and
/// shows its readings before its App histories.
fn reader_rest(reader: &mut Reader) {
    reader.last_route.clear();
    if let Some(store) = &mut reader.store {
        store.rest();
    }
}

/// Keeps a resting reader's session following the recording, so the next page reads only what
/// arrived since this step rather than everything since its window closed. A reader without a
/// session opens one, and a session that fails to follow is dropped for the next step to open.
fn reader_follow(reader: &mut Reader, root: &std::path::Path, shared: &Shared) {
    if reader.store.is_some() {
        return;
    }
    let Some(session) = &mut reader.session else {
        reader_follow_open(reader, root, shared);
        return;
    };

    let followed = session
        .begin(shared.cancellation.epoch())
        .and_then(|()| session.poll());
    match followed {
        Ok(_changed) => {}
        Err(persistence::Error::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted => {}
        Err(error) => {
            eprintln!("Kept Flightdeck reader stopped following the recording: {error}");
            reader.session = None;
        }
    }
}

/// Opens the resting reader's session, at most once each retry interval, since a window closed
/// before its reader opened the recording would otherwise reopen over none.
fn reader_follow_open(reader: &mut Reader, root: &std::path::Path, shared: &Shared) {
    if Instant::now() < reader.retry_after {
        return;
    }
    let epoch = shared.cancellation.epoch();
    match LiveSession::open_with(root, Arc::clone(&shared.cancellation), epoch) {
        Ok(session) => reader.session = Some(session),
        Err(persistence::Error::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted => {}
        Err(error) => {
            eprintln!("Kept Flightdeck reader could not open the recording: {error}");
            reader.retry_after = Instant::now() + RETRY_INTERVAL;
        }
    }
}

fn reader_read(
    reader: &mut Reader,
    root: &std::path::Path,
    command: &mut Command,
    shared: &Shared,
) -> Option<Update> {
    if reader.store.is_some() {
        return store_reader::read(reader, command, shared);
    }
    if Instant::now() < reader.retry_after {
        return None;
    }
    #[cfg(test)]
    measurement::clear_payloads();
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
    if reader_snapshot_finished(command, published) {
        return Ok(None);
    }
    if command.request.route == "computer" {
        return reader_computer(reader, root, command, shared, published);
    }
    let Plan {
        mut histories,
        early,
        kept_tail_only,
    } = reader_snapshot_plan(reader, &command.request.route);
    let first_snapshot = reader.last_generation != command.generation;
    let session =
        reader_snapshot_session(&mut reader.session, root, command, shared, first_snapshot)?;
    let catalog = session.catalog();
    if published {
        if reader.last_catalog.as_ref() == Some(&catalog) {
            return Ok(None);
        }
    }
    let mut selection = reader_selection(session, &command.request, command.context)?;
    let tail_pending = reader_earlier(session, &command.request, kept_tail_only, &mut selection);
    let mut snapshot = reader_snapshot_new(command.generation, catalog, selection);
    if early {
        histories = reader_snapshot_histories(session, &snapshot);
    }
    reader_payload(
        session,
        &mut command.request,
        command.context,
        histories,
        &mut snapshot,
    )?;
    #[cfg(test)]
    measurement::raw_if_requested(session, shared, &snapshot.selection);
    if histories == Histories::Defer {
        snapshot.histories_pending = true;
    }
    if tail_pending {
        snapshot.histories_pending = true;
    }
    snapshot.read_ms = started.elapsed().as_secs_f64() * 1000.0;
    reader_snapshot_record(reader, command, &snapshot, histories);

    Ok(Some(snapshot))
}

fn reader_snapshot_finished(command: &Command, published: bool) -> bool {
    if !published {
        return false;
    }

    if command.mode == SourceMode::Recorded {
        return true;
    }

    if command.request.route == "computer" {
        return false;
    }

    reader_snapshot_is_pinned(&command.request)
}

/// What a step of the page reads, and what it leaves to a later step.
struct Plan {
    histories: Histories,
    /// Whether the step asks the session if the App histories it leaves would replay a saved
    /// minute, and reads them with its first readings where they would not.
    early: bool,
    /// Whether the step takes only an older tail its session keeps.
    kept_tail_only: bool,
}

/// What this step of the page `route` reads. A newly opened page publishes its readings first,
/// since replaying its App histories from saved minutes, or reading an older tail its session
/// does not keep, can take seconds. A page whose App histories read the parts its saved minutes
/// keep reads them with its first readings where they replay no saved minute, and the others
/// read them next, whatever request of the page comes next. An older tail it has to read comes
/// after them, since its charts draw the tail only over a range longer than their full detail.
fn reader_snapshot_plan(reader: &Reader, route: &str) -> Plan {
    let opened = reader.last_route != route;
    let histories_due = reader
        .pending
        .is_some_and(|pending| pending.histories == Histories::Defer);
    let mut histories = Histories::Read;
    let mut early = false;
    if opened {
        if reader_route_has_histories(route) {
            histories = Histories::Defer;
            early = reader_route_reads_kept_histories(route);
        }
    }

    Plan {
        histories,
        early,
        kept_tail_only: opened || histories_due,
    }
}

/// What a step planned to read the App histories early reads: them, where they replay no saved
/// minute, or else nothing of them, as a step that defers them. A failed check defers them, so
/// the next step reads them and shows the failure.
fn reader_snapshot_histories(session: &LiveSession, snapshot: &Snapshot) -> Histories {
    // A missing second has no payload, which the next step reads as a deferring step does.
    if snapshot.selection["selected"].is_null() {
        return Histories::Defer;
    }

    match session.app_histories_replay_nothing() {
        Ok(true) => Histories::Early,
        Ok(false) => Histories::Defer,
        Err(persistence::Error::Io(error)) if error.kind() == std::io::ErrorKind::Interrupted => {
            Histories::Defer
        }
        Err(error) => {
            eprintln!("App histories' saved minutes unchecked: {error}");
            Histories::Defer
        }
    }
}

/// The reader's session, opened on the first read and begun for the command's epoch. A live
/// selection first catches up the ring, then keeps that endpoint while deferred work finishes.
fn reader_snapshot_session<'session>(
    slot: &'session mut Option<LiveSession>,
    root: &std::path::Path,
    command: &Command,
    shared: &Shared,
    first_snapshot: bool,
) -> Result<&'session mut LiveSession, String> {
    let session = if let Some(session) = slot {
        session
    } else {
        let open = if command.context == Context::Restore {
            LiveSession::open_history_with
        } else {
            LiveSession::open_with
        };
        slot.insert(
            open(root, Arc::clone(&shared.cancellation), command.epoch)
                .map_err(|error| error.to_string())?,
        )
    };
    session
        .begin(command.epoch)
        .map_err(|error| error.to_string())?;
    #[cfg(test)]
    measurement::checkpoint(shared, command.generation, measurement::Stage::Reading);
    let poll = match command.request.route.as_str() {
        "self" => true,
        _ => match command.mode {
            SourceMode::Live => match command.request.second {
                None => true,
                Some(_) => first_snapshot && command.context == Context::Select,
            },
            SourceMode::Recorded => command.request.second.is_none(),
        },
    };
    if poll {
        session.poll().map_err(|error| {
            #[cfg(test)]
            measurement::interrupted(shared, command.generation, &error);
            error.to_string()
        })?;
    }

    Ok(session)
}

/// Whether the request asks for a chosen second of a page other than `self`, which a published
/// generation already answers.
fn reader_snapshot_is_pinned(request: &Request) -> bool {
    request.second.is_some() && request.route != "self"
}

/// Remembers what the reader published, so the next command can tell whether anything changed.
fn reader_snapshot_record(
    reader: &mut Reader,
    command: &Command,
    snapshot: &Snapshot,
    histories: Histories,
) {
    // A snapshot that leaves its App histories to the next step shows them as being read.
    if histories == Histories::Defer {
        assert!(snapshot.histories_pending);
    }
    reader.last_catalog = Some(snapshot.catalog.clone());
    reader.last_generation = command.generation;
    reader.last_route.clone_from(&command.request.route);
    // A step that read the histories early leaves the window's other ranks to a `Read` step.
    let ranks_pending = histories == Histories::Early;
    reader.pending = (snapshot.histories_pending || ranks_pending).then_some(Pending {
        generation: command.generation,
        histories,
    });
}

/// Whether the reader has published everything `generation` asks for.
fn reader_published(reader: &Reader, generation: u64) -> bool {
    if reader.last_generation != generation {
        return false;
    }
    !reader_pending(reader, generation)
}

/// Whether the last published snapshot of `generation` left part of its page to a later step.
fn reader_pending(reader: &Reader, generation: u64) -> bool {
    reader
        .pending
        .is_some_and(|pending| pending.generation == generation)
}

/// Whether the route's pages show the selected second's top Apps, which every page of one App
/// measure does.
pub(crate) fn route_ranks_apps(route: &str) -> bool {
    matches!(route, "overview" | "memory")
        || crate::live_app_attribution_history::Metric::from_route(route).is_some()
}

/// Whether the route's App histories read the parts its saved minutes keep and replay only the
/// minutes that keep none. The App history of each page of one App measure replays every
/// saved minute of its range, so it never reads with a page's first readings.
fn reader_route_reads_kept_histories(route: &str) -> bool {
    matches!(route, "overview" | "memory")
}

/// Whether the route's payload replays App histories from saved minutes, which every page of
/// one App measure does.
fn reader_route_has_histories(route: &str) -> bool {
    matches!(route, "overview" | "apps" | "memory")
        || crate::live_app_attribution_history::Metric::from_route(route).is_some()
}

/// A snapshot of the catalog and selection, before the route's payload is read.
fn reader_snapshot_new(generation: u64, catalog: Value, selection: Value) -> Box<Snapshot> {
    reader_snapshot_owned(generation, catalog, Presentation::Legacy(selection))
}

fn reader_snapshot_owned(
    generation: u64,
    catalog: Value,
    selection: Presentation<Value>,
) -> Box<Snapshot> {
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

/// The Computer page's reading: the newest observation, whose summary carries the machine's
/// facts as the collector read them. A step reads it again only once the recording changed, so
/// a page opened before the collector's first tick shows the facts once that tick arrives.
fn reader_computer(
    reader: &mut Reader,
    root: &std::path::Path,
    command: &Command,
    shared: &Shared,
    published: bool,
) -> Result<Option<Box<Snapshot>>, String> {
    let first_snapshot = reader.last_generation != command.generation;
    let session =
        reader_snapshot_session(&mut reader.session, root, command, shared, first_snapshot)?;
    let catalog = session.catalog();
    if published {
        if reader.last_catalog.as_ref() == Some(&catalog) {
            return Ok(None);
        }
    }

    let selection = reader_latest(session)?;
    let snapshot = reader_snapshot_new(command.generation, catalog, selection);
    #[cfg(test)]
    measurement::raw_if_requested(session, shared, &snapshot.selection);
    reader_snapshot_record(reader, command, &snapshot, Histories::Read);
    Ok(Some(snapshot))
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
/// a longer range draw, and returns whether the page still waits for them. A step with
/// `kept_tail_only` takes only a tail its session keeps, and leaves one it would read from saved
/// minutes to a later step. A failed read leaves the charts at their full-detail context and
/// says why beside it.
fn reader_earlier(
    session: &mut LiveSession,
    request: &Request,
    kept_tail_only: bool,
    selection: &mut Value,
) -> bool {
    if request.earlier == Earlier::Skip || request.route == "apps" {
        return false;
    }
    let Some(last) = crate::live_projection::context_last_second(selection) else {
        return false;
    };
    let Some(fields) = selection.as_object_mut() else {
        return false;
    };
    let tail = if kept_tail_only {
        session.earlier_context_kept(last)
    } else {
        session.earlier_context(last).map(Some)
    };
    match tail {
        Ok(Some(Value::Object(tail))) => fields.extend(tail),
        Ok(Some(_)) => unreachable!("The older tail is an object"),
        Ok(None) => return true,
        Err(error) => {
            fields.insert("earlier_error".into(), error.to_string().into());
        }
    }

    false
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
        serde_json::json!({"second":null, "context_last_second":null, "selected":null,
        "alternatives":[], "summary":null, "trends":[], "context_seconds":900}),
    )
}

/// Reads the payload of the page. The page's App histories visit the selected App's saved parts
/// of the window several times, so the payload that reads them decodes each window once.
fn reader_payload(
    session: &mut LiveSession,
    request: &mut Request,
    context: Context,
    histories: Histories,
    snapshot: &mut Snapshot,
) -> Result<(), String> {
    if histories == Histories::Read {
        if let Some(key) = &request.app {
            let kind = app_key_kind(&key.kind)?;
            let bytes = app_key_bytes(&key.bytes_hex)?;
            return session.with_parts_read(kind, &bytes, |session| {
                reader_payload_read(session, request, context, histories, snapshot)
            });
        }
    }

    reader_payload_read(session, request, context, histories, snapshot)
}

fn reader_payload_read(
    session: &mut LiveSession,
    request: &mut Request,
    context: Context,
    histories: Histories,
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
    reader_payload_read_pages(session, tables, snapshot)?;
    if request.route == "apps" {
        reader_apps(session, request, context, histories, snapshot)?;
    }
    let ranked = route_ranks_apps(&request.route);
    if ranked {
        snapshot.rankings = Some(Arc::new(Presentation::Legacy(
            crate::live_rankings::from_selected(
                session.selected_ranks().map_err(|error| error.to_string()),
            ),
        )));
    }
    if request.route == "cpu" {
        let detail = session.cpu_detail().map_err(|error| error.to_string())?;
        #[cfg(test)]
        measurement::payload("cpu_detail", &detail);
        snapshot.cpu_detail = Some(Presentation::Legacy(crate::live_cpu::CPUActivity::decode(
            detail,
            &snapshot.selection["selected"],
        )?));
        if context != Context::Keep {
            reader_cpu_history(session, request, snapshot)?;
        }
    }
    // Keeping App parts replays the saved minutes of each window that keeps none, so it follows
    // the histories the page shows, and a hover's own read keeps none.
    if histories != Histories::Defer {
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
            if histories == Histories::Read {
                reader_app_ranks(session.rank_window());
            } else {
                reader_app_ranks(session.rank_window_kept());
            }
            snapshot.app_ranks = Some(Arc::new(RanksWindow::Legacy(session.app_ranks())));
        }
    }

    Ok(())
}

fn reader_payload_read_pages(
    session: &mut LiveSession,
    tables: &[&str],
    snapshot: &mut Snapshot,
) -> Result<(), String> {
    for table in tables {
        let page = reader_page(session, table, 0)?;
        identity_validate(&page["observation"], &snapshot.selection["selected"])?;
        snapshot
            .pages
            .insert((*table).to_owned(), Presentation::Legacy(page));
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
    snapshot.app_parts = Some(Arc::new(PartsWindow::Legacy(session.app_parts())));

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
) -> Arc<Presentation<crate::live_app_attribution_history::AppAttributionHistory>> {
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
) -> Arc<Presentation<crate::live_app_attribution_history::AppAttributionHistory>> {
    use crate::live_app_attribution_history::AppAttributionHistory;
    Arc::new(Presentation::Legacy(match result {
        Ok(payload) => {
            #[cfg(test)]
            measurement::payload(metric.panel_id(), &payload);
            AppAttributionHistory::prepare(metric, &payload, selection)
        }
        Err(error) => {
            AppAttributionHistory::Unavailable(crate::bounded::bounded(&error.to_string(), 512))
        }
    }))
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
            let unavailable = Arc::new(Presentation::Legacy(AppAttributionHistory::Unavailable(
                crate::bounded::bounded(&error.to_string(), 512),
            )));
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
            #[cfg(test)]
            measurement::payload("resident_history", &payload);
            crate::live_resident_history::ResidentHistory::prepare(&payload, &snapshot.selection)
        }
        Err(error) => crate::live_resident_history::ResidentHistory::Unavailable(
            crate::bounded::bounded(&error.to_string(), 512),
        ),
    };
    snapshot.resident_history = Some(Arc::new(Presentation::Legacy(history)));
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
    #[cfg(test)]
    measurement::payload("cpu_history", &payload);
    snapshot.cpu_history = Some(Arc::new(Presentation::Legacy(
        crate::live_cpu_history::CPUHistory::decode(payload, &snapshot.selection)?,
    )));

    Ok(())
}

fn reader_apps(
    session: &LiveSession,
    request: &mut Request,
    context: Context,
    histories: Histories,
    snapshot: &mut Snapshot,
) -> Result<(), String> {
    // The Apps page ranks no App, so it never reads its histories early, and reads its App's
    // history and process history only in a step that reads them all.
    assert!(matches!(histories, Histories::Read | Histories::Defer));
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
        if context != Context::Keep {
            if histories == Histories::Read {
                let history = session
                    .app_history(kind, &bytes)
                    .map_err(|error| error.to_string())?;
                identity_validate(&history["selected"], &snapshot.selection["selected"])?;
                snapshot.history = Some(Presentation::Legacy(history));
            }
        }
        snapshot.detail = Some(Presentation::Legacy(detail));
    }
    if request.load_process_history {
        if histories == Histories::Read {
            reader_process_history(session, request, snapshot);
        }
    }
    snapshot.apps = Some(Presentation::Legacy(apps));

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
    snapshot.process_history = Some(Arc::new(Presentation::Legacy(history)));
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

pub(crate) fn clamped_presentation_page(
    requested: u32,
    read: impl Fn(u32) -> Result<Presentation<Value>, String>,
) -> Result<Presentation<Value>, String> {
    let first = read(0)?;
    if first.is_null() {
        return Ok(first);
    }
    let offset = page_offset(u64::from(requested), &first["page"]["total"])?;
    if offset == 0 {
        return Ok(first);
    }
    drop(first);

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
#[path = "live_source_measurement.rs"]
pub(crate) mod measurement;

#[cfg(test)]
#[path = "live_source_tests.rs"]
pub(crate) mod tests;
