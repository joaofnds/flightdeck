use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::Value;
use viewer::live_app_response::KeyKind;
use viewer::{Cancellation, LiveSession};

pub struct Worker {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
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
}

#[derive(Clone, Debug, PartialEq)]
pub struct AppKey {
    pub kind: String,
    pub bytes_hex: String,
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
    pub read_ms: f64,
}

pub enum Update {
    Ready(Box<Snapshot>),
    Failed { generation: u64, message: String },
}

struct Shared {
    mailbox: Mutex<Mailbox>,
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
    request: Request,
}

struct Reader {
    session: Option<LiveSession>,
    last_catalog: Option<Value>,
    last_generation: u64,
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
        let shared = Arc::new(Shared {
            mailbox: Mutex::new(Mailbox {
                generation: 0,
                request: None,
                output: None,
            }),
            stopped: AtomicBool::new(false),
            cancellation: Arc::new(Cancellation::default()),
        });
        let control = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name("flightdeck-gpui-live".to_owned())
            .stack_size(4 * 1024 * 1024)
            .spawn(move || worker_run(&root, &control))?;

        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    pub fn request(&mut self, request: Request) -> Result<u64, String> {
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
            request,
        });

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
        last_catalog: None,
        last_generation: 0,
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
        if let Some(command) = &mut command {
            if let Some(output) = reader_read(&mut reader, root, command, shared) {
                worker_publish(shared, command.generation, output);
            }
        }
        thread::sleep(POLL_INTERVAL);
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
    if command.request.route == "computer" {
        return Ok(reader_computer(reader, command));
    }
    if command.request.second.is_some() {
        if reader.last_generation == command.generation {
            return Ok(None);
        }
    }
    if reader.session.is_none() {
        reader.session = Some(
            LiveSession::open_with(root, Arc::clone(&shared.cancellation), command.epoch)
                .map_err(|error| error.to_string())?,
        );
    }
    let session = reader
        .session
        .as_mut()
        .ok_or_else(|| "Live session did not open".to_owned())?;
    session
        .begin(command.epoch)
        .map_err(|error| error.to_string())?;
    if command.request.second.is_none() {
        session.poll().map_err(|error| error.to_string())?;
    }
    let catalog = session.catalog();
    if reader.last_generation == command.generation {
        if reader.last_catalog.as_ref() == Some(&catalog) {
            return Ok(None);
        }
    }
    let selection = reader_selection(session, &command.request)?;
    let mut snapshot = Box::new(Snapshot {
        generation: command.generation,
        catalog,
        selection,
        pages: BTreeMap::new(),
        apps: None,
        detail: None,
        history: None,
        cpu_detail: None,
        cpu_history: None,
        read_ms: 0.0,
    });
    reader_payload(session, &mut command.request, &mut snapshot)?;
    snapshot.read_ms = started.elapsed().as_secs_f64() * 1000.0;
    reader.last_catalog = Some(snapshot.catalog.clone());
    reader.last_generation = command.generation;

    Ok(Some(snapshot))
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
        read_ms: 0.0,
    }))
}

fn reader_selection(session: &mut LiveSession, request: &Request) -> Result<Value, String> {
    if request.route == "computer" {
        return Ok(Value::Null);
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

fn reader_payload(
    session: &mut LiveSession,
    request: &mut Request,
    snapshot: &mut Snapshot,
) -> Result<(), String> {
    // A missing second has no raw payload, and must never borrow a sibling observation.
    if snapshot.selection["selected"].is_null() {
        return Ok(());
    }
    let tables: &[&str] = match request.route.as_str() {
        "overview" => &["fans"],
        "cpu" => &["processes", "ticks", "memory", "cpus", "interfaces"],
        "raw" => &[request.table.as_str()],
        _ => &[],
    };
    for table in tables {
        let offset = match (request.route.as_str(), *table) {
            ("raw", _) | ("cpu", "processes") => request.offset,
            _ => 0,
        };
        let page = reader_page(session, table, offset)?;
        identity_validate(&page["observation"], &snapshot.selection["selected"])?;
        snapshot.pages.insert((*table).to_owned(), page);
    }
    if request.route == "apps" {
        reader_apps(session, request, snapshot)?;
    }
    if request.route == "cpu" {
        let detail = session.cpu_detail().map_err(|error| error.to_string())?;
        snapshot.cpu_detail = Some(crate::live_cpu::CPUActivity::decode(
            detail,
            &snapshot.selection["selected"],
        )?);
        reader_cpu_history(session, request, snapshot)?;
    }

    Ok(())
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
    snapshot: &mut Snapshot,
) -> Result<(), String> {
    let apps = reader_apps_page(session, request.apps_offset)?;
    identity_validate(&apps["selected"], &snapshot.selection["selected"])?;
    reader_apps_remember(request, &apps)?;
    if let Some(key) = &request.app {
        let kind = app_key_kind(&key.kind)?;
        let bytes = app_key_bytes(&key.bytes_hex)?;
        let mut detail = session
            .app_detail(kind, &bytes, 0)
            .map_err(|error| error.to_string())?;
        if !detail.is_null() {
            let offset = page_offset(u64::from(request.members_offset), &detail["page"]["total"])?;
            if offset > 0 {
                detail = session
                    .app_detail(
                        kind,
                        &bytes,
                        u32::try_from(offset).map_err(|error| error.to_string())?,
                    )
                    .map_err(|error| error.to_string())?;
            }
            identity_validate(&detail["selected"], &snapshot.selection["selected"])?;
        }
        let history = session
            .app_history(kind, &bytes)
            .map_err(|error| error.to_string())?;
        identity_validate(&history["selected"], &snapshot.selection["selected"])?;
        snapshot.detail = Some(detail);
        snapshot.history = Some(history);
    }
    snapshot.apps = Some(apps);

    Ok(())
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
    let first = session.apps_page(0).map_err(|error| error.to_string())?;
    let offset = page_offset(u64::from(offset), &first["page"]["total"])?;
    if offset == 0 {
        return Ok(first);
    }
    session
        .apps_page(u32::try_from(offset).map_err(|error| error.to_string())?)
        .map_err(|error| error.to_string())
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
    let stamp = match value.get("stamp") {
        Some(stamp) => stamp,
        None => value,
    };
    let publication = match &stamp["publication"] {
        Value::String(text) => text.parse::<u64>().map_err(|error| error.to_string())?,
        value => value
            .as_u64()
            .ok_or_else(|| "Missing observation publication".to_owned())?,
    };

    Ok((
        identity_text(&stamp["run"])?,
        identity_text(&stamp["boot"])?,
        publication,
    ))
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

fn app_key_kind(kind: &str) -> Result<KeyKind, String> {
    match kind {
        "bundle" => Ok(KeyKind::Bundle),
        "executable" => Ok(KeyKind::Executable),
        _ => Err("Invalid live App key kind".to_owned()),
    }
}

fn app_key_bytes(text: &str) -> Result<Vec<u8>, String> {
    if text.len() > APP_KEY_BYTES_MAX * 2 {
        return Err("Live App key exceeds the byte bound".to_owned());
    }
    if !text.len().is_multiple_of(2) {
        return Err("Live App key has an incomplete hexadecimal byte".to_owned());
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().chunks_exact(2) {
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
    if !request.cpu_offset.is_multiple_of(32) {
        return Err("Logical CPU history offset must be a page of 32".into());
    }

    if request.cpu_offset >= 1024 {
        return Err("Logical CPU history offset exceeds the CPU domain".into());
    }

    if !matches!(
        request.route.as_str(),
        "overview" | "cpu" | "apps" | "memory" | "gpu" | "energy" | "network" | "computer" | "raw"
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
#[path = "live_source_tests.rs"]
mod tests;
