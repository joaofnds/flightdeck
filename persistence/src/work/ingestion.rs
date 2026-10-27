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
}

thread_local! {
    static ACTIVE: RefCell<Option<Arc<Context>>> = const { RefCell::new(None) };
}

pub struct Inherited(Option<Arc<Context>>);

pub struct Scope {
    previous: Option<Arc<Context>>,
    thread: PhantomData<Rc<()>>,
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

        let mut state = context.shared.state.lock().unwrap_or_else(|_| {
            unreachable!("a trace lock is held only while copying bounded events")
        });
        if state.events.len() == EVENTS_MAX {
            state.overflowed = true;
            return;
        }
        state.events.push(Event {
            producer: context.producer,
            purpose: context.purpose,
            key,
            kind,
            table: context.table,
            bytes,
        });
    });
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
