//! The bounded ring-to-window loop, hosted only by the separate persistence binary.

use std::io;
use std::path::Path;
use std::thread;
use std::time::Duration;

use collector::processes::{self, Unreadable};
use collector::recording::{RegisteredRun, Registry};
use collector::ring::{Found, FoundEvent, IdentityObservation, RingReader, RunIdentity, Tick};
use collector::tracker::Event;

use crate::resources::{WORKING_BYTES_MAX, Watchdog};
use crate::{
    Begin, Encoder, Limits, Losses, Manifest, NumberedEvent, Observation, Result, Store,
    TimeBounds, Window, WindowIdentity,
};

const WINDOW_NS: u64 = 60_000_000_000;

pub enum Outcome {
    Complete,
    Owned,
}

pub fn run(root: &Path, memory_limit: u64) -> Result<Outcome> {
    let watchdog = Watchdog::start(memory_limit)?;
    let operation = watchdog.operation()?;
    let mut store = match Store::open(root) {
        Ok(store) => store,
        Err(error) if crate::ownership_contended(&error) => return Ok(Outcome::Owned),
        Err(error) => return Err(error.into()),
    };
    let encoder = Encoder;
    drop(operation);
    let registry_path = root.join("runs");
    let mut registry_buffer = [0_u8; 4097];
    let mut recovery = Recovery::default();
    for _pass in 0..u64::MAX {
        let operation = watchdog.operation()?;
        let runs = run_registry(&registry_path, &mut registry_buffer, &mut recovery)?;
        let waiting = run_pass(root, &store, &encoder, &runs, &mut recovery)?;
        if !waiting {
            if let Some(owned) = run_release(root, store, &recovery.states)? {
                store = owned;
            } else {
                drop(operation);
                drop(watchdog);
                let memory = crate::resources::Footprint::read()?;
                eprintln!(
                    "persister finished footprint_bytes={} footprint_bytes_max={}",
                    memory.current_bytes, memory.lifetime_bytes_max
                );
                return Ok(Outcome::Complete);
            }
        }
        drop(operation);
        thread::sleep(Duration::from_secs(1));
    }
    unreachable!("persistence polling bound exhausted")
}

fn run_release(root: &Path, store: Store, states: &[RunState; 2]) -> Result<Option<Store>> {
    let registration = match store.registration_lock() {
        Ok(lock) => lock,
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(Some(store)),
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
            return Ok(Some(store));
        }
    }
    // A replacement cannot register between the final check and releasing ownership.
    drop(store);
    drop(registration);
    Ok(None)
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
            eprintln!("recording retention: retired ring evicted; unsealed extent unknown");
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
    encoder: &Encoder,
    runs: &[RegisteredRun],
    recovery: &mut Recovery,
) -> Result<bool> {
    run_states(&mut recovery.states, runs);
    for (index, registered) in runs.iter().enumerate() {
        let closed = index + 1 < runs.len();
        let state = &mut recovery.states[index];
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
        match persist_one(store, encoder, registered, closed, state)? {
            Progress::Sealed => {
                if !state.losses.unknown {
                    recovery.pending_unknown = false;
                }
                return Ok(true);
            }
            Progress::Waiting => return Ok(true),
            Progress::Complete => Registry::retire(root, &registered.identity)?,
            Progress::Unavailable => {
                if !unavailable {
                    recovery.pending_unknown = true;
                }
            }
        }
    }
    Ok(false)
}

#[must_use]
pub fn default_memory_limit() -> u64 {
    WORKING_BYTES_MAX
}

enum Progress {
    Sealed,
    Waiting,
    Complete,
    Unavailable,
}

#[derive(Default)]
struct Recovery {
    states: [RunState; 2],
    evictions: Option<u64>,
    // Prior unsealed history has unknown extent; this does not count lost current rows.
    pending_unknown: bool,
}

#[derive(Default)]
struct RunState {
    identity: Option<RunIdentity>,
    next_publication: u64,
    losses: Losses,
    source: SourceState,
    loaded: bool,
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
    eprintln!(
        "recording unavailable run={} source={}: {error}; unsealed extent unknown",
        collector::recording::identity_text(registered.identity.run),
        registered.ring_name
    );
    state.source = SourceState::Unavailable;
}

fn persist_one(
    store: &Store,
    encoder: &Encoder,
    registered: &RegisteredRun,
    closed: bool,
    state: &mut RunState,
) -> Result<Progress> {
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
        let progress = store.progress(&registered.identity)?;
        state.next_publication = progress.map_or(reader.publication_first(), |manifest| {
            manifest.publication_end
        });
        state.loaded = true;
        if state.next_publication > 0 {
            reader.seek_publication(state.next_publication)?;
        }
    }
    match persist_one_stage(reader, store, registered, closed, state)? {
        Staged::Window(window) => {
            let committed = seal(store, encoder, &window, state.losses)?;
            state.next_publication = window.publication_range().end;
            if committed {
                state.losses = Losses::default();
            }
            Ok(Progress::Sealed)
        }
        Staged::Waiting => Ok(Progress::Waiting),
        Staged::Complete => Ok(Progress::Complete),
    }
}

enum Staged {
    Window(Window),
    Waiting,
    Complete,
}

// Owning the mapping here guarantees its release before the caller enters the engine.
fn persist_one_stage(
    mut reader: RingReader,
    store: &Store,
    registered: &RegisteredRun,
    closed: bool,
    state: &mut RunState,
) -> Result<Staged> {
    let limits = Limits::from_shape(&reader.shape())?;
    let mut tick = crate::tick_buffer(&reader.shape())?;
    let closed = persist_one_closed(&mut reader, &mut tick, registered.collector_pid, closed)?;
    for _step in 0..reader.shape().slot_count + 2 {
        match reader.read_into(&mut tick)? {
            Found::Nothing => {
                let complete = closed || reader.completed();
                if complete {
                    state.source = SourceState::Complete;
                }
                return Ok(if complete {
                    Staged::Complete
                } else {
                    Staged::Waiting
                });
            }
            Found::Missed { first, end } => {
                state.losses.publications += end - first;
                state.next_publication = reader.next_publication();
            }
            Found::Publication { publication, .. } => {
                let identity = WindowIdentity {
                    run: registered.identity,
                    window: window_number(&reader, &tick)?,
                };
                if let Some(manifest) = store.existing(&identity)? {
                    reader.seek_publication(manifest.publication_end)?;
                    state.next_publication = manifest.publication_end;
                    continue;
                }
                if !window_ready(&mut reader, &mut tick, &identity, closed)? {
                    state.next_publication = publication;
                    return Ok(Staged::Waiting);
                }
                reader.seek_publication(publication)?;
                let window =
                    stage_window(&mut reader, &mut tick, identity, limits, &mut state.losses)?;
                return Ok(Staged::Window(window));
            }
        }
    }
    Err(crate::Error::Bound("ring recovery scan"))
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
            ) =>
        {
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
    reader: &mut RingReader,
    tick: &mut Tick,
    identity: &WindowIdentity,
    closed: bool,
) -> Result<bool> {
    if closed || reader.completed() {
        return Ok(true);
    }
    let newest = reader.publication_newest()?;
    reader.seek_publication(newest)?;
    match reader.read_into(tick)? {
        Found::Publication { .. } => Ok(window_number(reader, tick)? > identity.window),
        Found::Missed { .. } | Found::Nothing => Ok(false),
    }
}

fn stage_window(
    reader: &mut RingReader,
    tick: &mut Tick,
    identity: WindowIdentity,
    limits: Limits,
    losses: &mut Losses,
) -> Result<Window> {
    let mut window = Window::new(identity, limits);
    let captured = reader.dictionary_capacity().is_some();
    let mut identities = Vec::with_capacity(if captured { limits.processes } else { 0 });

    for _step in 0..=reader.shape().slot_count {
        let found = if captured {
            reader.read_identity_into(tick, &mut identities)?
        } else {
            reader.read_into(tick)?
        };
        match found {
            Found::Nothing => break,
            Found::Missed { first, end } => losses.publications += end - first,
            Found::Publication { publication, .. } => {
                if window_number(reader, tick)? != identity.window {
                    break;
                }

                let mut events = ReadEvents {
                    records: Vec::new(),
                    identities: Vec::new(),
                };
                read_events(reader, tick, losses, &mut events)?;
                let numbered = stage_window_numbered(&events);
                let observation = Observation {
                    publication,
                    wall_ns: tick.times.map(|times| times.wall_ns),
                    continuous_ns: tick.times.map(|times| times.continuous_ns),
                    awake_ns: tick.times.map(|times| times.awake_ns),
                };

                if captured {
                    window.push_tick_with_identity(
                        &observation,
                        tick,
                        &numbered,
                        &crate::IdentitySource {
                            reader,
                            processes: &identities,
                            events: &events.identities,
                        },
                    )?;
                } else {
                    window.push_tick(&observation, tick, &numbered)?;
                }
            }
        }
    }

    Ok(window)
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

fn seal(store: &Store, encoder: &Encoder, window: &Window, losses: Losses) -> Result<bool> {
    let started = std::time::Instant::now();
    match store.begin(&window.identity)? {
        Begin::Committed(_) => Ok(false),
        Begin::Pending(pending) => {
            let tables = encoder.encode(window, pending.path())?;
            let publications = window.publication_range();
            let events = window.event_range();
            let manifest = Manifest {
                identity: window.identity,
                publication_first: publications.start,
                publication_end: publications.end,
                events_first: events.start,
                events_end: events.end,
                wall_ns: bounds(window, |value| value.wall_ns),
                continuous_ns: bounds(window, |value| value.continuous_ns),
                awake_ns: bounds(window, |value| value.awake_ns),
                losses,
                data_directory: String::new(),
                tables,
            };
            let committed = store.commit(pending, manifest)?;
            eprintln!(
                "persister sealed window={} publications={}..{} loss={} events_lost={} \
                seal_ns={} staged_bytes={}",
                committed.identity.window,
                committed.publication_first,
                committed.publication_end,
                committed.losses.publications,
                committed.losses.events,
                started.elapsed().as_nanos(),
                window.staged_bytes()
            );
            Ok(true)
        }
    }
}

fn bounds(window: &Window, field: impl Fn(&Observation) -> Option<u64>) -> Option<TimeBounds> {
    let mut result: Option<TimeBounds> = None;
    for observation in window.observations() {
        let value = field(observation)?;
        result = Some(match result {
            Some(bounds) => TimeBounds {
                min: bounds.min.min(value),
                max: bounds.max.max(value),
            },
            None => TimeBounds {
                min: value,
                max: value,
            },
        });
    }
    result
}

fn window_number(reader: &RingReader, tick: &Tick) -> Result<u64> {
    match (tick.times, reader.run_start_continuous_ns()) {
        (Some(times), Some(start)) => times
            .continuous_ns
            .checked_sub(start)
            .map(|elapsed| elapsed / WINDOW_NS)
            .ok_or(crate::Error::Bound("continuous time order")),
        _ => Ok(tick.run_tick / 60),
    }
}

fn persist_one_closed(
    reader: &mut RingReader,
    tick: &mut Tick,
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
    let latest_awake_ns = persist_one_latest_awake_ns(reader, tick)?;
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

fn persist_one_latest_awake_ns(reader: &mut RingReader, tick: &mut Tick) -> Result<Option<u64>> {
    let newest = reader.publication_newest()?;
    if newest == 0 {
        return Ok(None);
    }
    let position = reader.next_publication();
    reader.seek_publication(newest)?;
    let found = reader.read_into(tick);
    reader.seek_publication(position)?;
    match found? {
        Found::Publication { .. } => Ok(tick.times.map(|times| times.awake_ns)),
        Found::Missed { .. } | Found::Nothing => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use collector::ring::{Identity, Shape};

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
                    publication: number * 60 + 1,
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
        let place = collector::ring::Place {
            lock_path: root.0.join("ring.lock"),
            name_prefix: format!("/fdworker{}", std::process::id()),
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
        let mut writer = collector::ring::Writer::create_with_identity(
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
        publish_readable_identity(&mut writer);
        writer.finish();
        let mut state = RunState::default();

        let result = persist_one(&store, &Encoder, &registered, true, &mut state);

        collector::ring::unlink_generation(writer.name()).unwrap();
        assert!(matches!(result.unwrap(), Progress::Sealed));
        assert_eq!(state.next_publication, 2);
        assert_saved_identity(&root, &store, &registered.identity);
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
        let connection = duckdb::Connection::open_in_memory().unwrap();
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
        let row: (i32, u64, i32, Vec<u8>, Option<u64>, u8) = connection
            .query_row(
                &format!(
                    "SELECT pid,process_unique_id,pid_version,executable_name, \
                    resident_bytes,usage_availability FROM {source}"
                ),
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();

        assert_eq!(
            (41, u64::MAX, i32::MIN, b"worker\xff".to_vec(), None, 1),
            row
        );
        let event_path = root.0.join(&manifest.data_directory).join("events.parquet");
        assert_saved_identity_event(&connection, &event_path);
    }

    fn assert_saved_identity_event(connection: &duckdb::Connection, path: &Path) {
        let events = crate::query::table_source(
            connection,
            crate::schema::Table::Events,
            crate::query::TableSource::Parquet(path),
        )
        .unwrap();
        let event: (u64, i32, Vec<u8>, u8) = connection
            .query_row(
                &format!(
                    "SELECT process_unique_id,pid_version,executable_name, \
                    usage_availability FROM {events}"
                ),
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();

        assert_eq!((u64::MAX, i32::MIN, b"worker\xff".to_vec(), 3), event);
    }

    #[test]
    fn keeps_new_unknown_loss_pending_when_an_existing_key_is_replayed() {
        // Goal: replaying an old canonical key cannot acknowledge a newly unknown extent.
        // Method: seal, replay with new loss, then publish that status in the next key.
        let root = Directory::new();
        let store = Store::open(&root.0).unwrap();
        let encoder = Encoder;
        let first = window(store.recording(), 0);
        let unknown = Losses {
            unknown: true,
            ..Losses::default()
        };
        assert!(seal(&store, &encoder, &first, Losses::default()).unwrap());

        let acknowledged = seal(&store, &encoder, &first, unknown).unwrap();

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
        assert!(seal(&store, &encoder, &next, unknown).unwrap());
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

        let store = run_release(&root.0, store, &[RunState::default(), RunState::default()])
            .unwrap()
            .unwrap();

        assert!(crate::ownership_contended(
            &Store::open(&root.0).err().unwrap()
        ));
        Registry::default().write(&root.0).unwrap();
        assert!(
            run_release(&root.0, store, &[RunState::default(), RunState::default()])
                .unwrap()
                .is_none()
        );
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
                losses: Losses {
                    publications: 7,
                    events: 9,
                    unknown: true,
                },
                source: SourceState::Unavailable,
                loaded: true,
            },
        ];

        run_states(&mut states, &[run]);

        assert_eq!(states[0].next_publication, 17);
        assert_eq!(states[0].losses.publications, 7);
        assert_eq!(states[0].losses.events, 9);
        assert!(states[0].losses.unknown);
        assert_eq!(states[0].source, SourceState::Unavailable);
        assert!(states[0].loaded);
        assert_eq!(states[1].identity, None);
    }
}
