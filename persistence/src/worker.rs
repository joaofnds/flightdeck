//! The bounded ring-to-window loop, hosted only by the separate persistence binary.

use std::io;
use std::path::Path;
use std::thread;
use std::time::Duration;

use collector::processes::{self, Unreadable};
use collector::recording::{RegisteredRun, Registry};
use collector::ring::{
    Found, FoundEvent, IdentityObservation, PublicationTiming, RingReader, RunIdentity, Tick,
};
use collector::tracker::Event;

use crate::resources::{WORKING_BYTES_MAX, Watchdog};
use crate::{
    Begin, Encoder, Limits, Losses, Manifest, NumberedEvent, Observation, Result, Store,
    TableManifest, Window, WindowIdentity,
};

mod registered;
mod scanning;
mod staging;

pub enum Outcome {
    Complete,
    Owned,
}

/// Derives tables from a window the persister staged and wrote, which seal with its tables.
pub trait Derive {
    /// Writes into the pending directory only tables `schema::DERIVED_TABLES` names.
    fn derive(&self, derivation: &Derivation<'_>) -> Result<Vec<TableManifest>>;
}

pub struct Derivation<'seal> {
    /// The window as the persister staged it, which the tables it wrote hold.
    pub window: &'seal Window,
    /// The directory the window's tables go in, which the persister's own may reach only after.
    pub pending: &'seal collector::recording::private::PrivateDirectory,
}

/// What writes a window's tables: the persister's own and those derived from them.
struct Writers<'derive> {
    encoder: Encoder,
    derive: &'derive dyn Derive,
}

pub fn run(root: &Path, memory_limit: u64, derive: &dyn Derive) -> Result<Outcome> {
    run_paced(root, memory_limit, derive, &mut thread::sleep, None)
}

pub fn run_managed(
    root: &Path,
    memory_limit: u64,
    derive: &dyn Derive,
    authority: Option<collector::ring::Identity>,
) -> Result<Outcome> {
    let control = if let Some(authority) = authority {
        let control = collector::recording::control::Control::open_writable(root)?;
        if control.authority() != authority {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied).into());
        }
        Some(control)
    } else {
        None
    };
    run_paced(root, memory_limit, derive, &mut thread::sleep, control)
}

fn run_paced(
    root: &Path,
    memory_limit: u64,
    derive: &dyn Derive,
    pause: &mut dyn FnMut(Duration),
    control: Option<collector::recording::control::Control>,
) -> Result<Outcome> {
    let watchdog = Watchdog::start(memory_limit)?;
    let operation = watchdog.operation()?;
    let store = match Store::open(root) {
        Ok(store) => store,
        Err(error) if crate::ownership_contended(&error) => return Ok(Outcome::Owned),
        Err(error) => return Err(error.into()),
    };
    let mut health = control
        .map(|control| {
            collector::recording::control::Reporter::new(
                control,
                collector::recording::control::Role::Persister,
            )
        })
        .transpose()?;
    let writers = Writers {
        encoder: Encoder,
        derive,
    };
    drop(operation);
    let registry_path = root.join("runs");
    let mut registry_buffer = [0_u8; 4097];
    let mut recovery = Recovery {
        memory_limit,
        ..Recovery::default()
    };
    for _pass in 0..u64::MAX {
        let operation = watchdog.operation()?;
        let runs = run_registry(&registry_path, &mut registry_buffer, &mut recovery)?;
        let pass = run_pass(root, &store, &writers, &runs, &mut recovery)?;
        report_progress(&mut health, &recovery)?;
        match pass {
            Pass::Drained => {
                if let Some(registration) = run_release(root, &store, &recovery.states)? {
                    drop(operation);
                    drop(watchdog);
                    let memory = crate::resources::Footprint::read()?;
                    collector::diagnostics::error(format_args!(
                        "persister finished footprint_bytes={} footprint_bytes_max={}",
                        memory.current_bytes, memory.lifetime_bytes_max
                    ));
                    if let Some(health) = &mut health {
                        health.set_state(collector::recording::control::State::Stopped)?;
                    }
                    // Reporter destruction can publish on failure, so it precedes role release.
                    drop(health);
                    drop(store);
                    drop(registration);
                    return Ok(Outcome::Complete);
                }
            }
            Pass::Sealed | Pass::Waiting => {}
        }
        drop(operation);
        match pass {
            // The window after a sealed one may be ready already, and a collector that is
            // stopping waits for the persister to seal its last window and exit.
            Pass::Sealed => {}
            Pass::Waiting | Pass::Drained => pause(Duration::from_secs(1)),
        }
    }
    unreachable!("persistence polling bound exhausted")
}

fn report_progress(
    health: &mut Option<collector::recording::control::Reporter>,
    recovery: &Recovery,
) -> Result<()> {
    if let Some(health) = health {
        let committed = recovery
            .states
            .iter()
            .rev()
            .find(|state| state.committed > 0);
        if let Some(state) = committed {
            let identity = state
                .identity
                .ok_or(crate::Error::Bound("committed run identity"))?;
            health.update(
                collector::recording::control::State::Running,
                identity.run,
                state.committed,
            )?;
        } else {
            health.refresh()?;
        }
    }
    Ok(())
}

fn run_release(
    root: &Path,
    store: &Store,
    states: &[RunState; 2],
) -> Result<Option<collector::recording::private::FileLock>> {
    let registration = match store.registration_lock() {
        Ok(lock) => lock,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut buffer = [0; 4097];
    let registry = Registry::read_into(&root.join("runs"), &mut buffer)?;
    for registered in registry.runs() {
        let registered = registered?;
        let terminal = states.iter().any(|state| {
            if state.identity != Some(registered.identity) {
                return false;
            }
            matches!(
                state.source,
                SourceState::Complete | SourceState::Unavailable
            )
        });
        if !terminal {
            return Ok(None);
        }
    }
    // The caller retains this barrier through its terminal report and role-lock release.
    Ok(Some(registration))
}

fn run_registry(
    path: &Path,
    buffer: &mut [u8; 4097],
    recovery: &mut Recovery,
) -> Result<Vec<RegisteredRun>> {
    let registry = Registry::read_into(path, buffer)?;
    let runs = registry
        .runs()
        .map(|run| {
            run.map(|run| RegisteredRun {
                identity: run.identity,
                ring_name: run.ring_name.to_owned(),
                collector_pid: run.collector_pid,
            })
        })
        .collect::<io::Result<_>>()?;
    let evictions = registry.evictions()?;
    if let Some(previous) = recovery.evictions {
        if evictions > previous {
            recovery.pending_unknown = true;
            collector::diagnostics::error(format_args!(
                "recording retention: retired ring evicted; unsealed extent unknown"
            ));
        }
    } else {
        // The counter survives a worker crash before a seal; its extent remains unknown.
        recovery.pending_unknown = evictions > 0;
    }
    recovery.evictions = Some(evictions);
    Ok(runs)
}

fn run_pass(
    root: &Path,
    store: &Store,
    writers: &Writers<'_>,
    runs: &[RegisteredRun],
    recovery: &mut Recovery,
) -> Result<Pass> {
    run_states(&mut recovery.states, runs);
    for (index, registered) in runs.iter().enumerate() {
        let closed = index + 1 < runs.len();
        let state = &mut recovery.states[index];
        state.memory_limit = recovery.memory_limit;
        state.unknown_boundary = recovery.unknown_boundary;
        if state.source == SourceState::Unchecked {
            state.source =
                if Registry::completed(root, &registered.identity, &registered.ring_name)? {
                    SourceState::Complete
                } else {
                    SourceState::Open
                };
        }
        let unavailable = state.source == SourceState::Unavailable;
        if recovery.pending_unknown {
            state.losses.unknown = true;
        }
        match persist_one(store, writers, registered, closed, state)? {
            Progress::Sealed => {
                if !state.losses.unknown {
                    recovery.pending_unknown = false;
                }
                return Ok(Pass::Sealed);
            }
            Progress::Waiting => return Ok(Pass::Waiting),
            Progress::Deferred => {}
            Progress::Complete => Registry::retire(root, &registered.identity)?,
            Progress::Unavailable => {
                if !unavailable {
                    recovery.pending_unknown = true;
                    recovery.unknown_boundary = Some(registered.identity);
                }
            }
        }
    }
    Ok(Pass::Drained)
}

#[must_use]
pub fn default_memory_limit() -> u64 {
    WORKING_BYTES_MAX
}

/// What one pass over the registered runs did.
enum Pass {
    Sealed,
    Waiting,
    /// Every registered run is complete or unavailable.
    Drained,
}

enum Progress {
    Sealed,
    Waiting,
    /// Another run's interrupted attempt holds the store's slot, so that run goes first.
    Deferred,
    Complete,
    Unavailable,
}

#[derive(Default)]
struct Recovery {
    states: [RunState; 2],
    evictions: Option<u64>,
    // Prior unsealed history has unknown extent; this does not count lost current rows.
    pending_unknown: bool,
    unknown_boundary: Option<RunIdentity>,
    memory_limit: u64,
}

#[derive(Default)]
struct RunState {
    identity: Option<RunIdentity>,
    next_publication: u64,
    next_event: u64,
    committed: u64,
    losses: Losses,
    source: SourceState,
    loaded: bool,
    memory_limit: u64,
    /// The last run seen unavailable, which a saved minute's unknown extent names.
    unknown_boundary: Option<RunIdentity>,
    /// Whether another run's interrupted attempt holds the slot this run needs.
    deferred: bool,
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
enum SourceState {
    #[default]
    Unchecked,
    Open,
    Complete,
    Unavailable,
}

fn run_states(states: &mut [RunState; 2], runs: &[RegisteredRun]) {
    assert!(runs.len() <= states.len());
    let mut next = [RunState::default(), RunState::default()];
    for (index, registered) in runs.iter().enumerate() {
        next[index] = match states
            .iter()
            .position(|state| state.identity == Some(registered.identity))
        {
            Some(previous) => std::mem::take(&mut states[previous]),
            None => RunState {
                identity: Some(registered.identity),
                ..RunState::default()
            },
        };
    }
    *states = next;
}

fn source_unavailable(
    registered: &RegisteredRun,
    state: &mut RunState,
    error: impl std::fmt::Display,
) {
    collector::diagnostics::error(format_args!(
        "recording unavailable run={} source={}: {error}; unsealed extent unknown",
        collector::recording::identity_text(registered.identity.run),
        registered.ring_name
    ));
    state.source = SourceState::Unavailable;
}

fn persist_one(
    store: &Store,
    writers: &Writers<'_>,
    registered: &RegisteredRun,
    closed: bool,
    state: &mut RunState,
) -> Result<Progress> {
    if store.registered() {
        return registered::persist_one(store, writers, registered, closed, state);
    }
    if state.source == SourceState::Unavailable {
        return Ok(Progress::Unavailable);
    }
    if registered.identity.recording != store.recording() {
        source_unavailable(registered, state, "foreign recording identity");
        return Ok(Progress::Unavailable);
    }
    if state.source == SourceState::Complete {
        return Ok(Progress::Complete);
    }
    let Some(mut reader) = persist_one_open(registered, state)? else {
        return Ok(Progress::Unavailable);
    };
    if !state.loaded {
        let durable = store.progress(&registered.identity)?;
        state.committed = durable
            .as_ref()
            .map_or(0, |manifest| manifest.publication_end - 1);
        state.next_event = durable
            .as_ref()
            .map_or(reader.event_first(), |manifest| manifest.events_end);
        state.next_publication = durable.map_or(reader.publication_first(), |manifest| {
            manifest.publication_end
        });
        state.loaded = true;
        if state.next_publication > 0 {
            reader.seek_publication(state.next_publication)?;
        }
    }
    match persist_one_stage(reader, store, registered, closed, state)? {
        Staged::Window(staged) => {
            let window = staged.window;
            seal_pending(store, writers, &window, state, staged.pending)?;
            let publication_end = window.publication_range().end;
            // The worker runs its next pass at once after a seal, so a seal must move on.
            assert!(state.next_publication < publication_end);
            state.next_publication = publication_end;
            state.next_event = window.event_range().end;
            state.losses = Losses::default();
            Ok(Progress::Sealed)
        }
        Staged::Waiting => Ok(Progress::Waiting),
        Staged::Complete => Ok(Progress::Complete),
        Staged::Terminal => persist_terminal(store, registered, state),
    }
}

enum Staged {
    Window(Box<StagedWindow>),
    Waiting,
    Complete,
    Terminal,
}

struct StagedWindow {
    window: Window,
    pending: crate::PendingWindow,
}

// Owning the mapping here guarantees its release before the caller enters the engine.
fn persist_one_stage(
    reader: RingReader,
    store: &Store,
    registered: &RegisteredRun,
    closed: bool,
    state: &mut RunState,
) -> Result<Staged> {
    let mut scan = scanning::Scanning::new(reader, registered, closed)?;

    for _step in 0..scan.steps() {
        if let Some(result) = scan.read(store, registered, state)? {
            return Ok(result);
        }
    }

    unreachable!("recovery scan exceeded its step bound")
}

fn persist_one_stage_end(
    reader: &RingReader,
    _registered: &RegisteredRun,
    closed: bool,
    state: &mut RunState,
) -> Result<Staged> {
    let complete = closed || reader.completed();
    if !complete {
        return Ok(Staged::Waiting);
    }

    // Completion can follow a final publication after the read that returned Nothing.
    // Observe its publication header after the completion acquire before retiring it.
    if reader.next_publication() <= reader.publication_newest()? {
        return Ok(Staged::Waiting);
    }

    let first = if state.next_event == 0 {
        reader.event_first()
    } else {
        state.next_event
    };
    if first <= reader.event_newest()? || state.losses != Losses::default() {
        return Ok(Staged::Terminal);
    }

    state.source = SourceState::Complete;

    Ok(Staged::Complete)
}

fn persist_terminal(
    store: &Store,
    registered: &RegisteredRun,
    state: &mut RunState,
) -> Result<Progress> {
    let pending = match store.begin_events(&registered.identity)? {
        crate::store::EventBegin::Committed(manifest) => {
            state.next_event = manifest.recovery.events_end;
            state.losses = Losses::default();
            state.source = SourceState::Complete;
            return Ok(Progress::Complete);
        }
        crate::store::EventBegin::Pending(pending) => pending,
    };
    let Some(reader) = persist_one_open(registered, state)? else {
        return Ok(Progress::Unavailable);
    };
    let first = if state.next_event == 0 {
        reader.event_first()
    } else {
        state.next_event
    };
    let end = reader
        .event_newest()?
        .checked_add(1)
        .ok_or(crate::Error::Bound("terminal event end"))?;
    let memory_limit = if state.memory_limit == 0 {
        WORKING_BYTES_MAX
    } else {
        state.memory_limit
    };
    let manifest = crate::event_recovery::write_tail(
        &pending.path,
        reader,
        &registered.identity,
        first..end,
        state.losses,
        usize::try_from(memory_limit).map_err(|_| crate::Error::Bound("terminal memory limit"))?,
    )?;
    let manifest = store.commit_events(pending, manifest, &mut |_| Ok(()))?;
    state.next_event = manifest.recovery.events_end;
    state.losses = Losses::default();
    state.source = SourceState::Complete;
    Ok(Progress::Complete)
}

fn persist_one_stage_selected(
    store: &Store,
    reader: &mut RingReader,
    tick: &mut Tick,
    identity: WindowIdentity,
    limits: Limits,
    state: &mut RunState,
) -> Result<Option<StagedWindow>> {
    let first = reader.next_publication();
    if state.next_event == 0 {
        state.next_event = reader.event_first();
    }
    reader.seek_event(state.next_event)?;
    let pending = match store.begin(&identity)? {
        Begin::Committed(manifest) => {
            reader.seek_publication(manifest.publication_end)?;
            state.next_publication = manifest.publication_end;
            state.next_event = manifest.events_end;
            return Ok(None);
        }
        Begin::Pending(pending) => pending,
    };
    let memory_limit = if state.memory_limit == 0 {
        WORKING_BYTES_MAX
    } else {
        state.memory_limit
    };
    let events = crate::event_table::writer::Writer::new(
        collector::recording::private::PrivateDirectory::open(pending.path())?
            .create_directory("events")?,
        identity,
        reader.next_event(),
        reader.dictionary_capacity().is_some(),
        memory_limit,
    )?;
    let window = stage_window(reader, tick, identity, limits, &mut state.losses, events)?;
    if window.tick_count() > 0 {
        return Ok(Some(StagedWindow { window, pending }));
    }

    let next = reader.next_publication();
    if next <= first {
        return Err(crate::Error::Bound("lost selection without progress"));
    }

    state.next_publication = next;

    Ok(None)
}

fn persist_one_open(
    registered: &RegisteredRun,
    state: &mut RunState,
) -> Result<Option<RingReader>> {
    let mut reader = match RingReader::open_generation(&registered.ring_name) {
        Ok(reader) => reader,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound
                    | io::ErrorKind::PermissionDenied
                    | io::ErrorKind::InvalidData
                    | io::ErrorKind::InvalidInput
                    | io::ErrorKind::Unsupported
                    | io::ErrorKind::WouldBlock
            ) =>
        {
            // Registration follows ready-ring publication. An announced unready object
            // is unavailable history, not a pending creation that can block other runs.
            source_unavailable(registered, state, error);
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    if let Some(identity) = reader.identity() {
        if identity != registered.identity {
            source_unavailable(registered, state, "registered identity mismatch");
            return Ok(None);
        }
    }
    if state.next_publication > 0 {
        reader.seek_publication(state.next_publication)?;
    }
    Ok(Some(reader))
}

fn window_ready(
    reader: &RingReader,
    newest: Option<&PublicationTiming>,
    identity: &WindowIdentity,
    closed: bool,
) -> Result<bool> {
    if closed || reader.completed() {
        return Ok(true);
    }

    match newest {
        Some(timing) => Ok(timing_window_number(reader, timing)? > identity.window),
        None => Ok(false),
    }
}

fn stage_window(
    reader: &mut RingReader,
    tick: &mut Tick,
    identity: WindowIdentity,
    limits: Limits,
    losses: &mut Losses,
    events: crate::event_table::writer::Writer,
) -> Result<Window> {
    let mut staged = staging::Staging::new(reader, identity, limits, events);

    for _step in 0..=reader.shape().slot_count {
        if !staged.read(reader, tick, losses)? {
            break;
        }
    }

    staged.finish(reader, losses)
}

struct ReadEvents {
    records: Vec<(u64, u64, Event)>,
    identities: Vec<IdentityObservation>,
}

fn stage_window_numbered(events: &ReadEvents) -> Vec<NumberedEvent<'_>> {
    events
        .records
        .iter()
        .map(|(number, run_tick, event)| NumberedEvent {
            number: *number,
            tick: *run_tick,
            event,
        })
        .collect()
}

fn read_events(
    reader: &mut RingReader,
    tick: &Tick,
    losses: &mut Losses,
    events: &mut ReadEvents,
) -> Result<()> {
    let count = tick
        .events
        .end
        .checked_sub(tick.events.start)
        .ok_or(crate::Error::Bound("tick event rows"))?;
    let count = usize::try_from(count).map_err(|_| crate::Error::Bound("tick event rows"))?;
    if count > collector::tracker::EVENTS_PER_TICK_MAX {
        return Err(crate::Error::Bound("tick event rows"));
    }

    let captured = reader.dictionary_capacity().is_some();

    events.records = Vec::with_capacity(count);
    events.identities = Vec::with_capacity(if captured { count } else { 0 });
    reader.seek_event(tick.events.start)?;
    let bound = reader.shape().event_capacity + 1;
    for _step in 0..bound {
        if reader.next_event() >= tick.events.end {
            return Ok(());
        }

        let mut identity = IdentityObservation::EMPTY;
        let found = if captured {
            reader.read_event_identity(&mut identity)?
        } else {
            reader.read_event()?
        };
        match found {
            FoundEvent::Event {
                number,
                tick,
                event,
            } => {
                events.records.push((number, tick, event));
                if captured {
                    events.identities.push(identity);
                }
            }
            FoundEvent::Missed { first, end } => {
                losses.events += end.min(tick.events.end).saturating_sub(first);
            }
            FoundEvent::Nothing => {
                losses.events += tick.events.end.saturating_sub(reader.next_event());
                return Ok(());
            }
        }
    }

    Err(crate::Error::Bound("event scan"))
}

/// Seals a window, and returns whether this seal committed it rather than finding it sealed.
#[cfg(test)]
fn seal(
    store: &Store,
    writers: &Writers<'_>,
    window: &Window,
    state: &mut RunState,
) -> Result<bool> {
    let pending = match store.begin(&window.identity)? {
        Begin::Committed(_) => return Ok(false),
        Begin::Pending(pending) => pending,
    };

    seal_pending(store, writers, window, state, pending)?;
    Ok(true)
}

fn seal_pending(
    store: &Store,
    writers: &Writers<'_>,
    window: &Window,
    state: &mut RunState,
    pending: crate::PendingWindow,
) -> Result<()> {
    let started = std::time::Instant::now();

    let directory = collector::recording::private::PrivateDirectory::open(pending.path())?;
    let mut tables = writers.encoder.encode_into(window, &directory)?;
    tables.extend(seal_derived(
        writers.derive,
        &Derivation {
            window,
            pending: &directory,
        },
    )?);
    drop(directory);

    let manifest = seal_manifest(window, tables, state.losses)?;
    let committed = store.commit(pending, manifest)?;
    state.committed = committed.publication_end - 1;
    collector::diagnostics::error(format_args!(
        "persister sealed window={} publications={}..{} loss={} events_lost={} \
        seal_ns={} staged_bytes={} tables={}",
        committed.identity.window,
        committed.publication_first,
        committed.publication_end,
        committed.losses.publications,
        committed.losses.events,
        started.elapsed().as_nanos(),
        window.staged_bytes(),
        committed.tables.len()
    ));

    Ok(())
}

pub(crate) fn seal_manifest(
    window: &Window,
    tables: Vec<TableManifest>,
    losses: Losses,
) -> Result<Manifest> {
    let publications = window.publication_range();
    let events = window.event_range();
    Ok(Manifest {
        identity: window.identity,
        publication_first: publications.start,
        publication_end: publications.end,
        events_first: events.start,
        events_end: events.end,
        wall_ns: crate::metadata::clock_bounds(window.observations().map(|value| value.wall_ns)),
        continuous_ns: crate::metadata::clock_bounds(
            window.observations().map(|value| value.continuous_ns),
        ),
        awake_ns: crate::metadata::clock_bounds(window.observations().map(|value| value.awake_ns)),
        losses,
        data_directory: String::new(),
        tables,
        event_batches: window
            .event_index
            .as_ref()
            .map(|index| {
                u32::try_from(index.batches.len())
                    .map_err(|_| crate::Error::Bound("event part count"))
            })
            .transpose()?,
    })
}

/// The tables derived for a window, or none when deriving fails, since a reader can derive
/// them again from the tables the window seals with.
fn seal_derived(derive: &dyn Derive, derivation: &Derivation<'_>) -> Result<Vec<TableManifest>> {
    match derive.derive(derivation) {
        Ok(tables) => {
            for table in &tables {
                assert!(crate::schema::DERIVED_TABLES.contains(&table.name));
            }

            Ok(tables)
        }
        Err(error) => {
            collector::diagnostics::error(format_args!(
                "persister derived no tables window={}: {error}",
                derivation.window.identity.window
            ));
            for name in crate::schema::DERIVED_TABLES {
                match derivation.pending.remove_file(&format!("{name}.parquet")) {
                    Ok(()) => {}
                    Err(problem) if problem.kind() == io::ErrorKind::NotFound => {}
                    Err(problem) => return Err(problem.into()),
                }
            }

            Ok(Vec::new())
        }
    }
}

fn window_number(reader: &RingReader, tick: &Tick) -> Result<u64> {
    timing_window_number(
        reader,
        &PublicationTiming {
            run_tick: tick.run_tick,
            times: tick.times,
        },
    )
}

pub(crate) fn timing_window_number(reader: &RingReader, timing: &PublicationTiming) -> Result<u64> {
    crate::transaction::elapsed_window(
        timing.run_tick,
        timing.times.map(|times| times.continuous_ns),
        reader.run_start_continuous_ns(),
    )
    .map_err(|_| crate::Error::Bound("continuous time order"))
}

fn persist_one_closed(
    reader: &mut RingReader,
    newest: &mut Option<PublicationTiming>,
    pid: u32,
    retired: bool,
) -> Result<bool> {
    if retired {
        return Ok(true);
    }
    if reader.completed() {
        return Ok(true);
    }
    if pid == 0 {
        return Ok(true);
    }
    let pid = i32::try_from(pid).map_err(|_| crate::Error::Bound("collector pid"))?;
    if pid > processes::PID_MAX {
        return Err(crate::Error::Bound("collector pid"));
    }
    *newest = persist_one_latest_timing(reader)?;
    let latest_awake_ns = newest.and_then(|timing| timing.times.map(|times| times.awake_ns));
    let timebase = collector::clock::Timebase::read()?;
    match processes::read_usage(timebase, pid)? {
        Ok(usage) => {
            // The original producer existed before this observation. A later birth
            // proves that its PID now belongs to another process, without joining them.
            Ok(latest_awake_ns.is_some_and(|awake_ns| usage.awake_ns_at_start > awake_ns))
        }
        Err(Unreadable::Gone) => Ok(true),
        Err(Unreadable::Refused) => {
            if crate::resources::producer_owner_changed(pid)? {
                return Ok(true);
            }
            Err(io::Error::from(io::ErrorKind::PermissionDenied).into())
        }
    }
}

fn persist_one_latest_timing(reader: &mut RingReader) -> Result<Option<PublicationTiming>> {
    let newest = reader.publication_newest()?;
    if newest == 0 {
        return Ok(None);
    }

    let position = reader.next_publication();
    reader.seek_publication(newest)?;
    let mut timing = PublicationTiming::default();
    let found = reader.read_timing_into(&mut timing);
    reader.seek_publication(position)?;

    match found? {
        Found::Publication { .. } => Ok(Some(timing)),
        Found::Missed { .. } | Found::Nothing => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collector::ring::{Identity, Shape};

    pub(super) mod forged_source;
    mod staging_tests;
    mod timing_tests;

    struct Directory(std::path::PathBuf);

    impl Directory {
        fn new() -> Self {
            let nonce = collector::recording::random_identity().unwrap();
            let root = std::env::temp_dir().join(format!(
                "fd-worker-{}-{}",
                std::process::id(),
                collector::recording::identity_text(nonce)
            ));
            collector::recording::private::create_private_dir(&root).unwrap();
            Self(root)
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            std::fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn registered(recording: Identity) -> RegisteredRun {
        RegisteredRun {
            identity: RunIdentity {
                recording,
                run: Identity([3, 4]),
                boot: Identity([5, 6]),
            },
            ring_name: "/fdhandoff.1".to_owned(),
            collector_pid: 0,
        }
    }

    fn window(recording: Identity, number: u64) -> Window {
        window_at(recording, number, number * 60 + 1)
    }

    fn window_at(recording: Identity, number: u64, publication: u64) -> Window {
        let shape = Shape {
            slot_count: 1,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 1,
        };
        let identity = WindowIdentity {
            run: registered(recording).identity,
            window: number,
        };
        let mut window = Window::new(identity, Limits::from_shape(&shape).unwrap());
        let mut tick = crate::tick_buffer(&shape).unwrap();
        tick.run_tick = number * 60;
        window
            .push_tick(
                &Observation {
                    publication,
                    wall_ns: None,
                    continuous_ns: None,
                    awake_ns: None,
                },
                &tick,
                &[],
            )
            .unwrap();
        window
    }

    #[test]
    fn seals_readable_identity_with_refused_usage_through_the_worker() {
        // Goal: the worker seals readable identity without inventing refused usage.
        // Method: publish a real v4 source, drain one window and query its committed row.
        let root = Directory::new();
        let store = Store::open(&root.0).unwrap();
        let mut registered = registered(store.recording());
        let mut writer = identity_ring(&root, "i", &mut registered);
        publish_readable_identity(&mut writer);
        writer.finish();
        let mut state = RunState::default();
        let writers = Writers {
            encoder: Encoder,
            derive: &Underived,
        };

        let result = persist_one(&store, &writers, &registered, true, &mut state);

        collector::ring::unlink_generation(writer.name()).unwrap();
        assert!(matches!(result.unwrap(), Progress::Sealed));
        assert_eq!(state.next_publication, 2);
        assert_saved_identity(&root, &store, &registered.identity);
    }

    #[test]
    fn pauses_while_its_collector_runs_and_exits_once_the_last_window_seals() {
        // Goal: the worker paces itself while its collector runs, and a stopping collector,
        // which waits for the worker to exit, waits for no pause after the last window seals.
        // Method: run the whole worker over a live collector's empty ring, and in its first pause
        // publish a tick and finish the ring as a stopping collector does.
        let root = Directory::new();
        let mut registered = registered(Store::open(&root.0).unwrap().recording());
        registered.collector_pid = std::process::id();
        let mut writer = identity_ring(&root, "p", &mut registered);
        let run = registered.identity;
        Registry {
            runs: vec![registered],
            evictions: 0,
        }
        .write(&root.0)
        .unwrap();
        let mut pauses = Vec::new();

        let outcome = run_paced(
            &root.0,
            WORKING_BYTES_MAX,
            &Underived,
            &mut |pause| {
                pauses.push(pause);
                assert_eq!(1, pauses.len(), "the worker paused after the ring finished");
                publish_readable_identity(&mut writer);
                writer.finish();
            },
            None,
        );

        collector::ring::unlink_generation(writer.name()).unwrap();
        assert!(matches!(outcome.unwrap(), Outcome::Complete));
        assert_eq!(vec![Duration::from_secs(1)], pauses);
        let store = Store::open(&root.0).unwrap();
        let window = WindowIdentity { run, window: 0 };
        assert!(store.existing(&window).unwrap().is_some());
    }

    /// Creates an empty ring of its own for the registered run. The label keeps the ring's name
    /// apart from another test's.
    fn identity_ring(
        root: &Directory,
        label: &str,
        registered: &mut RegisteredRun,
    ) -> collector::ring::Writer {
        let place = collector::ring::Place {
            lock_path: root.0.join("ring.lock"),
            name_prefix: format!("/fdworker{}{label}", std::process::id()),
        };
        let shape = Shape {
            slot_count: 1,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 1,
        };
        let writer = collector::ring::Writer::create_with_identity(
            &place,
            shape,
            collector::ring::DictionaryCapacity {
                entries: 1,
                arena_bytes: 32,
            },
            &registered.identity,
            1,
        )
        .unwrap();
        registered.ring_name = writer.name().to_owned();
        writer
    }

    fn publish_readable_identity(writer: &mut collector::ring::Writer) {
        use collector::ring::{IdentityObservation, IdentityPublication};
        let mut process = IdentityObservation::EMPTY;
        publish_readable_identity_process(writer, &mut process);
        let memory = collector::machine::Memory::default();
        let events = [Event::Start {
            pid: 41,
            coalition: None,
        }];
        let readings = collector::ring::Readings {
            run_tick: 1,
            cpus: &[],
            memory: &memory,
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[],
            processes_refused: 1,
            processes_gone: 0,
            coalitions: &[],
            events: &events,
            dropped: collector::ring::Dropped::default(),
        };

        writer
            .publish_with_identity(
                &readings,
                Some(&collector::ring::ObservationTimes {
                    wall_ns: 1_000_000_000_000,
                    continuous_ns: 1,
                    awake_ns: 1,
                }),
                &IdentityPublication {
                    processes: &[process],
                    events: &[IdentityObservation {
                        usage: collector::ring::UsageAvailability::NotApplicable,
                        ..process
                    }],
                },
            )
            .unwrap();
    }

    fn publish_readable_identity_process(
        writer: &mut collector::ring::Writer,
        process: &mut IdentityObservation,
    ) {
        let mut program = None;
        writer
            .intern_program(
                &collector::ring::ProgramBytes {
                    executable_name: b"worker\xff",
                    app_path: Some(b"/A.app"),
                },
                &mut program,
            )
            .unwrap();
        process.pid = 41;
        process.usage = collector::ring::UsageAvailability::Refused;
        process.identity = collector::ring::ProcessIdentity::Measured {
            creation: collector::ring::ProcessCreation {
                unique_id: u64::MAX,
                pid_version: i32::MIN,
            },
            program: program.unwrap(),
        };
    }

    fn assert_saved_identity(root: &Directory, store: &Store, run: &RunIdentity) {
        let manifest = store
            .existing(&WindowIdentity {
                run: *run,
                window: 0,
            })
            .unwrap()
            .unwrap();
        let connection = crate::engine::Engine::new();
        let path = root
            .0
            .join(&manifest.data_directory)
            .join("processes.parquet");
        let source = crate::query::table_source(
            &connection,
            crate::schema::Table::Processes,
            crate::query::TableSource::Parquet(&path),
        )
        .unwrap();
        let frame = connection
            .query(&format!(
                "SELECT pid,process_unique_id,pid_version,executable_name, \
                resident_bytes,usage_availability FROM {source}"
            ))
            .unwrap();
        assert_eq!(frame.height(), 1);
        let row = crate::engine::Row::new(&frame, 0).unwrap();
        let row: (i32, u64, i32, Vec<u8>, Option<u64>, u8) = (
            row.get(0).unwrap(),
            row.get(1).unwrap(),
            row.get(2).unwrap(),
            row.get(3).unwrap(),
            row.get(4).unwrap(),
            row.get(5).unwrap(),
        );

        assert_eq!(
            (41, u64::MAX, i32::MIN, b"worker\xff".to_vec(), None, 1),
            row
        );
        assert_saved_identity_event(&connection, &root.0, &manifest);
    }

    fn assert_saved_identity_event(
        connection: &crate::engine::Engine,
        root: &Path,
        manifest: &Manifest,
    ) {
        let budget = std::sync::Arc::new(crate::resources::ByteBudget::new(128 * 1024 * 1024));
        let events = connection.event_source(root, manifest, &budget).unwrap();
        let frame = connection
            .query(&format!(
                "SELECT process_unique_id,pid_version,executable_name, \
                usage_availability FROM {events}"
            ))
            .unwrap();
        assert_eq!(frame.height(), 1);
        let row = crate::engine::Row::new(&frame, 0).unwrap();
        let event: (u64, i32, Vec<u8>, u8) = (
            row.get(0).unwrap(),
            row.get(1).unwrap(),
            row.get(2).unwrap(),
            row.get(3).unwrap(),
        );

        assert_eq!((u64::MAX, i32::MIN, b"worker\xff".to_vec(), 3), event);
    }

    #[test]
    fn keeps_new_unknown_loss_pending_when_an_existing_key_is_replayed() {
        // Goal: replaying an old canonical key cannot acknowledge a newly unknown extent.
        // Method: seal, replay with new loss, then publish that status in the next key.
        let root = Directory::new();
        let store = Store::open(&root.0).unwrap();
        let writers = Writers {
            encoder: Encoder,
            derive: &Underived,
        };
        let first = window(store.recording(), 0);
        let mut unknown = RunState {
            losses: Losses {
                unknown: true,
                ..Losses::default()
            },
            ..RunState::default()
        };
        assert!(seal(&store, &writers, &first, &mut RunState::default()).unwrap());

        let acknowledged = seal(&store, &writers, &first, &mut unknown).unwrap();

        assert!(!acknowledged);
        assert!(
            !store
                .existing(&first.identity)
                .unwrap()
                .unwrap()
                .losses
                .unknown
        );
        let next = window(store.recording(), 1);
        assert!(seal(&store, &writers, &next, &mut unknown).unwrap());
        assert!(
            store
                .existing(&next.identity)
                .unwrap()
                .unwrap()
                .losses
                .unknown
        );
    }

    #[test]
    fn seeds_historical_unknown_extent_without_counting_a_new_eviction() {
        // Goal: worker restart preserves unknown history while seeding cumulative counters.
        // Method: read a retained counter twice and distinguish initial status from increases.
        let root = Directory::new();
        Registry {
            runs: Vec::new(),
            evictions: 3,
        }
        .write(&root.0)
        .unwrap();
        let mut recovery = Recovery::default();

        run_registry(&root.0.join("runs"), &mut [0; 4097], &mut recovery).unwrap();

        assert!(recovery.pending_unknown);
        assert_eq!(recovery.evictions, Some(3));
        recovery.pending_unknown = false;
        run_registry(&root.0.join("runs"), &mut [0; 4097], &mut recovery).unwrap();
        assert!(!recovery.pending_unknown);
    }

    #[test]
    fn retains_health_ownership_after_a_failed_final_registry_read() {
        // Goal: a final registry error cannot leave the failing reporter without its role lock.
        // Method: corrupt the registry at release and inspect ownership before reporter drop.
        use collector::recording::control::{Control, Reporter, Role, State};
        use collector::recording::private::FileLock;
        let root = Directory::new();
        let control = Control::create(&root.0).unwrap();
        let store = Store::open(&root.0).unwrap();
        let health = Reporter::new(control, Role::Persister).unwrap();
        std::fs::write(root.0.join("runs"), b"invalid registry").unwrap();

        let result = run_release(&root.0, &store, &[RunState::default(), RunState::default()]);
        let owned = FileLock::held(&root.0.join("persister.lock")).unwrap();
        drop(health);

        assert!(result.is_err());
        assert!(owned);
        assert_eq!(
            Control::open(&root.0)
                .unwrap()
                .read(Role::Persister)
                .unwrap()
                .state,
            State::Failed
        );
    }

    #[test]
    fn keeps_ownership_when_a_new_registration_arrives_before_release() {
        // Goal: a replacement never loses persistence between the final poll and owner exit.
        // Method: register a fresh identity before release, then inspect real lock ownership.
        let root = Directory::new();
        let store = Store::open(&root.0).unwrap();
        Registry {
            runs: vec![registered(store.recording())],
            evictions: 0,
        }
        .write(&root.0)
        .unwrap();

        assert!(
            run_release(&root.0, &store, &[RunState::default(), RunState::default()])
                .unwrap()
                .is_none()
        );

        assert!(crate::ownership_contended(
            &Store::open(&root.0).err().unwrap()
        ));
        Registry::default().write(&root.0).unwrap();
        let registration =
            run_release(&root.0, &store, &[RunState::default(), RunState::default()])
                .unwrap()
                .unwrap();
        assert!(
            collector::recording::private::FileLock::held(&root.0.join("persister.lock")).unwrap()
        );
        assert!(
            collector::recording::private::FileLock::held(&root.0.join("registration.lock"))
                .unwrap()
        );
        drop(store);
        drop(registration);
        assert!(Store::open(&root.0).is_ok());
    }

    #[test]
    fn keeps_progress_losses_and_unavailability_with_a_reordered_identity() {
        // Goal: retiring the older registry position cannot reset the remaining run's state.
        // Method: remove its predecessor and inspect retained cursor, losses and report state.
        let run = registered(Identity([1, 2]));
        let mut states = [
            RunState::default(),
            RunState {
                identity: Some(run.identity),
                next_publication: 17,
                next_event: 23,
                committed: 11,
                losses: Losses {
                    publications: 7,
                    events: 9,
                    unknown: true,
                },
                source: SourceState::Unavailable,
                loaded: true,
                memory_limit: WORKING_BYTES_MAX,
                deferred: true,
                unknown_boundary: None,
            },
        ];

        run_states(&mut states, &[run]);

        assert_eq!(states[0].next_publication, 17);
        assert_eq!(states[0].committed, 11);
        assert_eq!(states[0].losses.publications, 7);
        assert_eq!(states[0].losses.events, 9);
        assert!(states[0].losses.unknown);
        assert_eq!(states[0].source, SourceState::Unavailable);
        assert!(states[0].loaded);
        assert_eq!(states[1].identity, None);
    }

    /// Derives no table.
    struct Underived;

    impl Derive for Underived {
        fn derive(&self, _derivation: &Derivation<'_>) -> Result<Vec<TableManifest>> {
            Ok(Vec::new())
        }
    }

    /// Writes the App ranks table, then fails when told to, as a derivation cut short does.
    struct RanksStub {
        fails: bool,
    }

    impl Derive for RanksStub {
        fn derive(&self, derivation: &Derivation<'_>) -> Result<Vec<TableManifest>> {
            let mut ranks = derivation.pending.create_file("app_ranks.parquet")?;
            io::Write::write_all(&mut ranks, b"ranks")?;
            if self.fails {
                return Err(crate::Error::Bound("derivation test"));
            }

            Ok(vec![TableManifest {
                name: "app_ranks",
                rows: 3,
            }])
        }
    }

    #[test]
    fn seals_the_tables_derived_from_a_window_with_it() {
        // Goal: a derived table seals in the same commit as the tables it derives from.
        // Method: derive a table into the pending window, then reopen the committed window.
        let root = Directory::new();
        let store = Store::open(&root.0).unwrap();
        let writers = Writers {
            encoder: Encoder,
            derive: &RanksStub { fails: false },
        };
        let window = window(store.recording(), 0);

        seal(&store, &writers, &window, &mut RunState::default()).unwrap();

        let sealed = store.existing(&window.identity).unwrap().unwrap();
        assert_eq!(sealed.tables.last().unwrap().name, "app_ranks");
        assert_eq!(sealed.tables.len(), crate::schema::TABLES.len() + 1);
        let path = store.data_path(&sealed).join("app_ranks.parquet");
        assert_eq!(std::fs::read(path).unwrap(), b"ranks");
    }

    #[test]
    fn seals_a_window_without_its_derived_tables_when_deriving_fails() {
        // Goal: a failed derivation loses only what a reader can derive again.
        // Method: fail after writing part of a derived table, then reopen the committed window.
        let root = Directory::new();
        let store = Store::open(&root.0).unwrap();
        let writers = Writers {
            encoder: Encoder,
            derive: &RanksStub { fails: true },
        };
        let window = window(store.recording(), 0);

        let committed = seal(&store, &writers, &window, &mut RunState::default()).unwrap();

        assert!(committed);
        let reopened = store.existing(&window.identity).unwrap().unwrap();
        assert_eq!(reopened.tables.len(), crate::schema::TABLES.len());
        let path = store.data_path(&reopened).join("app_ranks.parquet");
        assert!(!path.exists());
    }
}
