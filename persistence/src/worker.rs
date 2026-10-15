//! The bounded ring-to-window loop, hosted only by the separate persistence binary.

use std::io;
use std::path::Path;
use std::thread;
use std::time::Duration;

use collector::processes::{self, Unreadable};
use collector::recording::{RegisteredRun, Registry};
use collector::ring::{Found, FoundEvent, RingReader, RunIdentity, Shape, Tick, TickBuffers};
use collector::tracker::Event;

use crate::resources::{WORKING_BYTES_MAX, Watchdog};
use crate::{
    Begin, Encoder, Limits, Losses, Manifest, NumberedEvent, Observation, Result, Store,
    TimeBounds, Window, WindowIdentity,
};

const WINDOW_NS: u64 = 60_000_000_000;

pub fn run(root: &Path, parent_pid: u32, memory_limit: u64) -> Result<()> {
    let watchdog = Watchdog::start(memory_limit)?;
    let store = Store::open(root)?;
    let encoder = Encoder::new()?;
    let registry_path = root.join("runs");
    let mut registry_buffer = [0_u8; 4097];
    let mut states = [RunState::default(), RunState::default()];
    let mut evictions = 0;
    for _pass in 0..u64::MAX {
        let registry = Registry::read_into(&registry_path, &mut registry_buffer)?;
        let runs: Vec<_> = registry
            .runs()
            .map(|run| {
                run.map(|run| RegisteredRun {
                    identity: run.identity,
                    ring_name: run.ring_name.to_owned(),
                    collector_pid: run.collector_pid,
                })
            })
            .collect::<io::Result<_>>()?;
        if registry.evictions()? > evictions {
            evictions = registry.evictions()?;
            eprintln!(
                "recording retention: retired ring evicted; any unsealed history is unavailable"
            );
        }
        let mut waiting = false;
        for (index, registered) in runs.iter().enumerate() {
            let state = &mut states[index];
            if state.identity != Some(registered.identity) {
                *state = RunState::starting(&store, &registered.identity)?;
            }
            let closed = index + 1 < runs.len();
            match persist_one(&store, &encoder, registered, closed, state)? {
                Progress::Sealed | Progress::Waiting => {
                    waiting = true;
                    break;
                }
                Progress::Complete => {}
            }
        }
        if !waiting {
            drop(watchdog);
            let memory = crate::resources::Footprint::read()?;
            eprintln!(
                "persister finished footprint_bytes={} footprint_bytes_max={}",
                memory.current_bytes, memory.lifetime_bytes_max
            );
            return Ok(());
        }
        if parent_pid == 0 {
            return Err(crate::Error::Bound("collector pid"));
        }
        thread::sleep(Duration::from_secs(1));
    }
    unreachable!("persistence polling bound exhausted")
}

#[must_use]
pub fn default_memory_limit() -> u64 {
    WORKING_BYTES_MAX
}

enum Progress {
    Sealed,
    Waiting,
    Complete,
}

#[derive(Default)]
struct RunState {
    identity: Option<RunIdentity>,
    next_publication: u64,
    losses: Losses,
    complete: bool,
}

impl RunState {
    fn starting(store: &Store, identity: &RunIdentity) -> Result<Self> {
        let progress = store.progress(identity)?;
        Ok(Self {
            identity: Some(*identity),
            next_publication: progress.map_or(0, |manifest| manifest.publication_end),
            losses: Losses::default(),
            complete: false,
        })
    }
}

fn persist_one(
    store: &Store,
    encoder: &Encoder,
    registered: &RegisteredRun,
    closed: bool,
    state: &mut RunState,
) -> Result<Progress> {
    if state.complete {
        return Ok(Progress::Complete);
    }
    let Some(mut reader) = persist_one_open(registered, state)? else {
        return Ok(Progress::Complete);
    };
    let limits = Limits::from_shape(&reader.shape())?;
    let mut tick = tick_buffer(&reader.shape())?;
    let closed = persist_one_closed(&mut reader, &mut tick, registered.collector_pid, closed)?;
    for _step in 0..reader.shape().slot_count + 2 {
        match reader.read_into(&mut tick)? {
            Found::Nothing => {
                state.complete = closed || reader.completed();
                return Ok(if state.complete {
                    Progress::Complete
                } else {
                    Progress::Waiting
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
                    return Ok(Progress::Waiting);
                }
                reader.seek_publication(publication)?;
                let window =
                    stage_window(&mut reader, &mut tick, identity, limits, &mut state.losses)?;
                drop(reader);
                drop(tick);
                seal(store, encoder, &window, state.losses)?;
                state.next_publication = window.publication_range().end;
                state.losses = Losses::default();
                return Ok(Progress::Sealed);
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
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            eprintln!("recording gap: registered ring disappeared; unsealed extent unknown");
            state.complete = true;
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    if let Some(identity) = reader.identity() {
        if identity != registered.identity {
            return Err(crate::Error::Bound("registered identity"));
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
    let mut window = Window::new(identity, limits)?;
    for _step in 0..=reader.shape().slot_count {
        match reader.read_into(tick)? {
            Found::Nothing => break,
            Found::Missed { first, end } => losses.publications += end - first,
            Found::Publication { publication, .. } => {
                if window_number(reader, tick)? != identity.window {
                    break;
                }
                let events = read_events(reader, tick, losses)?;
                let numbered: Vec<_> = events
                    .iter()
                    .map(|(number, run_tick, event)| NumberedEvent {
                        number: *number,
                        tick: *run_tick,
                        event,
                    })
                    .collect();
                let observation = Observation {
                    publication,
                    wall_ns: tick.times.map(|times| times.wall_ns),
                    continuous_ns: tick.times.map(|times| times.continuous_ns),
                    awake_ns: tick.times.map(|times| times.awake_ns),
                };
                window.push_tick(&observation, tick, &numbered)?;
            }
        }
    }
    Ok(window)
}

fn read_events(
    reader: &mut RingReader,
    tick: &Tick,
    losses: &mut Losses,
) -> Result<Vec<(u64, u64, Event)>> {
    let mut events = Vec::new();
    reader.seek_event(tick.events.start)?;
    let bound = reader.shape().event_capacity + 1;
    for _step in 0..bound {
        if reader.next_event() >= tick.events.end {
            return Ok(events);
        }
        match reader.read_event()? {
            FoundEvent::Event {
                number,
                tick,
                event,
            } => events.push((number, tick, event)),
            FoundEvent::Missed { first, end } => {
                losses.events += end.min(tick.events.end).saturating_sub(first);
            }
            FoundEvent::Nothing => {
                losses.events += tick.events.end.saturating_sub(reader.next_event());
                return Ok(events);
            }
        }
    }
    Err(crate::Error::Bound("event scan"))
}

fn seal(store: &Store, encoder: &Encoder, window: &Window, losses: Losses) -> Result<()> {
    let started = std::time::Instant::now();
    match store.begin(&window.identity)? {
        Begin::Committed(_) => Ok(()),
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
            Ok(())
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

fn tick_buffer(shape: &Shape) -> Result<Tick> {
    let capacity = |value| usize::try_from(value).map_err(|_| crate::Error::Bound("tick capacity"));
    let states = capacity(shape.state_capacity)?;
    let cores = capacity(shape.core_capacity)?;
    Ok(Tick::from_buffers(TickBuffers {
        cpus: Vec::with_capacity(capacity(shape.cpu_capacity)?),
        interfaces: Vec::with_capacity(capacity(shape.interface_capacity)?),
        gpu_active_ns: Vec::with_capacity(states),
        core_active_ns: vec![0; cores * states],
        core_state_counts: Vec::with_capacity(cores),
        state_capacity: states,
        fan_rpm: Vec::with_capacity(capacity(shape.fan_capacity)?),
        processes: Vec::with_capacity(capacity(shape.process_capacity)?),
        coalitions: Vec::with_capacity(capacity(shape.coalition_capacity)?),
    }))
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
        Err(Unreadable::Refused) => Err(io::Error::from(io::ErrorKind::PermissionDenied).into()),
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
