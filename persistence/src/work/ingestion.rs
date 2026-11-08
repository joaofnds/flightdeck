//! Bounded opt-in source-work events inherited only by explicitly enrolled child threads.
use std::cell::RefCell;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use collector::ring::{Identity, RunIdentity};

const EVENTS_MAX: usize = 65_536;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Producer {
    Caller,
    Worker,
    Store,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Ordinary,
    Selection,
    Raw,
    Predecessor,
    /// A read of saved minutes for what the live Store does not hold.
    History,
    /// Work whose result was thrown away before publication, as a decoding begun from a seed
    /// that a later check refused.
    Discarded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    RingReadAttempt,
    RingRead,
    FrameCaptureAttempt,
    FrameCapture,
    CommittedFrameCaptureAttempt,
    CommittedFrameCapture,
    AppDerivation,
    RawCaptureAttempt,
    RawCapture,
    MachineReduction,
    MachineProjectionRefused,
    Accepted,
    AttemptFailed,
    NativeScan,
    BufferedScan,
    NativeCollect,
    ProjectionDecode,
    ContentCaptureAttempt,
    ContentCapture,
    AdmissionRead,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Unattributed {
        recording: Identity,
    },
    Publication {
        run: RunIdentity,
        publication: u64,
    },
    Minute {
        run: RunIdentity,
        first: u64,
        end: u64,
    },
}

impl Key {
    fn recording(&self) -> Identity {
        match self {
            Self::Unattributed { recording } => *recording,
            Self::Publication { run, .. } | Self::Minute { run, .. } => run.recording,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub producer: Producer,
    pub purpose: Purpose,
    pub key: Key,
    pub kind: Kind,
    pub table: Option<&'static str>,
    pub bytes: u64,
}

pub struct Trace {
    shared: Arc<Shared>,
}

struct Shared {
    recording: Identity,
    state: Mutex<State>,
}

struct State {
    events: Vec<Event>,
    overflowed: bool,
}

#[derive(Clone)]
struct Context {
    shared: Arc<Shared>,
    producer: Producer,
    purpose: Purpose,
    key: Option<Key>,
    table: Option<&'static str>,
    /// Where events go until their `Tentative` is settled, in place of the trace.
    tentative: Option<Arc<Mutex<State>>>,
}

thread_local! {
    static ACTIVE: RefCell<Option<Arc<Context>>> = const { RefCell::new(None) };
}

pub struct Inherited(Option<Arc<Context>>);

pub struct Scope {
    previous: Option<Arc<Context>>,
    thread: PhantomData<Rc<()>>,
}

/// The events of work whose result may yet be thrown away. They reach the trace, or the tentative
/// work around them, as recorded once kept, and as `Purpose::Discarded` once discarded or dropped
/// unkept.
#[must_use]
pub struct Tentative(Option<(Sink, Arc<Mutex<State>>)>);

/// Where settled tentative events go.
enum Sink {
    Trace(Arc<Shared>),
    Tentative(Arc<Mutex<State>>),
}

impl Sink {
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        let state = match self {
            Self::Trace(shared) => &shared.state,
            Self::Tentative(events) => events,
        };

        state.lock().unwrap_or_else(|_| {
            unreachable!("a trace lock is held only while pushing or copying bounded events")
        })
    }
}

impl Trace {
    #[must_use]
    pub fn new(recording: Identity) -> Self {
        Self {
            shared: Arc::new(Shared {
                recording,
                state: Mutex::new(State {
                    events: Vec::with_capacity(EVENTS_MAX),
                    overflowed: false,
                }),
            }),
        }
    }

    #[must_use]
    pub fn enter(&self) -> Scope {
        enter(Some(Arc::new(Context {
            shared: Arc::clone(&self.shared),
            producer: Producer::Caller,
            purpose: Purpose::Ordinary,
            key: None,
            table: None,
            tentative: None,
        })))
    }

    pub fn snapshot(&self, output: &mut Vec<Event>) -> Result<(), &'static str> {
        assert!(output.is_empty());
        let state = self
            .shared
            .state
            .lock()
            .map_err(|_| "trace lock poisoned")?;
        if state.overflowed {
            return Err("ingestion trace capacity exhausted");
        }

        output.extend_from_slice(&state.events);

        Ok(())
    }

    #[must_use]
    pub fn capacity_bytes(&self) -> usize {
        let state = self.shared.state.lock().unwrap_or_else(|_| {
            unreachable!("a trace lock is held only while copying bounded events")
        });

        2 * size_of::<usize>() + size_of::<Shared>() + state.events.capacity() * size_of::<Event>()
    }
}

#[must_use]
pub fn inherit(producer: Producer) -> Inherited {
    let mut context = ACTIVE.with(|active| active.borrow().clone());
    if let Some(context) = &mut context {
        Arc::make_mut(context).producer = producer;
    }

    Inherited(context)
}

#[must_use]
pub fn inherit_current() -> Inherited {
    Inherited(ACTIVE.with(|active| active.borrow().clone()))
}

#[must_use]
pub fn is_active() -> bool {
    ACTIVE.with(|active| active.borrow().is_some())
}

impl Inherited {
    #[must_use]
    pub fn enter(self) -> Scope {
        enter(self.0)
    }
}

#[must_use]
pub fn source(key: Key, table: Option<&'static str>) -> Scope {
    change(|context| {
        context.key = Some(key);
        context.table = table;
    })
}

#[must_use]
pub fn purpose(purpose: Purpose) -> Scope {
    change(|context| context.purpose = purpose)
}

/// Keeps every event this thread and the threads it enrols record, until the returned scope ends,
/// from the trace until the returned `Tentative` is settled.
pub fn tentative() -> (Scope, Tentative) {
    let mut context = ACTIVE.with(|active| active.borrow().clone());
    let mut tentative = Tentative(None);
    if let Some(context) = &mut context {
        let sink = match &context.tentative {
            Some(around) => Sink::Tentative(Arc::clone(around)),
            None => Sink::Trace(Arc::clone(&context.shared)),
        };
        let events = Arc::new(Mutex::new(State {
            events: Vec::new(),
            overflowed: false,
        }));
        tentative = Tentative(Some((sink, Arc::clone(&events))));
        Arc::make_mut(context).tentative = Some(events);
    }

    (enter(context), tentative)
}

impl Tentative {
    /// Hands the trace the events as they were recorded.
    pub fn keep(mut self) {
        self.settle(true);
    }

    /// Hands the trace the events as discarded work.
    pub fn discard(mut self) {
        self.settle(false);
    }

    fn settle(&mut self, kept: bool) {
        let Some((sink, tentative)) = self.0.take() else {
            return;
        };
        let tentative = Sink::Tentative(tentative);
        let events = tentative.lock();
        let mut state = sink.lock();

        if events.overflowed {
            state.overflowed = true;
        }
        for event in &events.events {
            let purpose = if kept {
                event.purpose
            } else {
                Purpose::Discarded
            };
            push(&mut state, Event { purpose, ..*event });
        }
    }
}

impl Drop for Tentative {
    fn drop(&mut self) {
        self.settle(false);
    }
}

fn change(update: impl FnOnce(&mut Context)) -> Scope {
    let mut context = ACTIVE.with(|active| active.borrow().clone());
    if let Some(context) = &mut context {
        update(Arc::make_mut(context));
    }

    enter(context)
}

fn enter(context: Option<Arc<Context>>) -> Scope {
    Scope {
        previous: ACTIVE.with(|active| active.replace(context)),
        thread: PhantomData,
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        drop(ACTIVE.with(|active| active.replace(self.previous.take())));
    }
}

pub fn record(key: Key, kind: Kind) {
    record_inner(Some(key), kind, 0);
}

pub fn record_publication(run: RunIdentity, publication: u64, kind: Kind) {
    if is_active() {
        let key = Key::Publication { run, publication };

        record(key, kind);
    }
}

pub fn record_content(kind: Kind, bytes: u64) {
    record_inner(None, kind, bytes);
}

fn record_inner(key: Option<Key>, kind: Kind, bytes: u64) {
    ACTIVE.with(|active| {
        let active = active.borrow();
        let Some(context) = &*active else { return };
        let key = match key.or(context.key) {
            Some(key) => key,
            None => Key::Unattributed {
                recording: context.shared.recording,
            },
        };
        if key.recording() != context.shared.recording {
            return;
        }

        let event = Event {
            producer: context.producer,
            purpose: context.purpose,
            key,
            kind,
            table: context.table,
            bytes,
        };
        let state = match &context.tentative {
            Some(tentative) => tentative,
            None => &context.shared.state,
        };
        let mut state = state.lock().unwrap_or_else(|_| {
            unreachable!("a trace lock is held only while copying bounded events")
        });
        push(&mut state, event);
    });
}

/// Adds `event` to `state`, which marks itself overflowed rather than grow past `EVENTS_MAX`.
fn push(state: &mut State, event: Event) {
    if state.events.len() == EVENTS_MAX {
        state.overflowed = true;
        return;
    }

    state.events.push(event);
}

#[must_use]
pub fn minute_source(manifest: &crate::Manifest, table: Option<&'static str>) -> Scope {
    source(
        Key::Minute {
            run: manifest.identity.run,
            first: manifest.publication_first,
            end: manifest.publication_end,
        },
        table,
    )
}

#[must_use]
pub fn table_name(name: &str) -> Option<&'static str> {
    crate::schema::TABLES
        .into_iter()
        .map(crate::schema::Table::name)
        .chain(crate::schema::DERIVED_TABLES)
        .find(|table| *table == name)
}

const _: () = {
    assert!(size_of::<Scope>() <= 16);
    assert!(size_of::<Inherited>() <= 16);
    assert!(size_of::<Event>() <= 128);
    assert!(EVENTS_MAX * size_of::<Event>() <= 8 * 1024 * 1024);
};

#[cfg(test)]
mod tests;
