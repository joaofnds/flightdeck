//! The readings the live window draws, held for the app's life. The store thread writes them
//! into buffers allocated when the store starts, and the main thread reads them through a View
//! that never changes while it holds it.

mod buffers;
mod ring;
mod second;

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub use buffers::{Buffers, Mailbox, Span};
pub use second::{MachineSecond, MemorySecond, Reading, Traffic, readings};

use ring::Ring;

#[cfg(test)]
#[path = "store_tests.rs"]
mod tests;

/// The seconds the store holds: the sixteen minutes a live chart draws a second apart, and
/// the two minutes the collector has not sealed yet.
const SECONDS_MAX: usize = 1_080;

/// The seconds past the span a View the main thread holds may keep, after which the store
/// takes no more seconds until the main thread lets go of it.
const SECONDS_HELD_MAX: usize = 64;

/// The Views the store writes: the one in the mailbox, the one the main thread draws, the one
/// it takes in place of that, and one to write.
const VIEWS_MAX: usize = 4;

/// The longest the store thread sleeps between steps, which bounds how long dropping the store
/// waits for its thread.
const WAIT: Duration = Duration::from_millis(100);

/// The steps of the store thread, which it never reaches, since it steps until the app quits.
const STEPS_MAX: u64 = u64::MAX;

/// What the store held at one step, which never changes while the main thread holds it.
pub struct View {
    /// Higher in each View than in the one before, since the store writes each View's buffer
    /// again.
    pub version: u64,
    /// The seconds the store holds, oldest first.
    pub seconds: Vec<Arc<MachineSecond>>,
    /// Why the store could not read the recording at its last step.
    pub failure: Option<Failure>,
}

/// Why the store could not read the recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Failure {
    /// The recording's identity could not be read, as before the collector ever ran.
    Recording(io::ErrorKind),
    /// The recording's register of runs could not be read.
    Registry(io::ErrorKind),
    /// The newest run belongs to another recording.
    ForeignRecording,
    /// The newest run's ring belongs to another run than the register names.
    ChangedIdentity,
    /// The newest run's ring maps more memory than the store reads.
    MappingSize,
    /// The ring could not be opened, read or waited on.
    Ring(io::ErrorKind),
    /// The ring's shape asks for buffers the store does not size.
    TickShape,
}

/// The store thread, and the main thread's way to its Views. Dropping the store stops its
/// thread.
pub struct Store {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

struct Shared {
    mailbox: Mailbox<View>,
    stop: AtomicBool,
}

impl Store {
    /// Starts the store thread over the recording at `root`, with every buffer it holds
    /// seconds and Views in allocated before the thread starts.
    pub fn start(root: &Path) -> io::Result<Self> {
        let shared = Arc::new(Shared {
            mailbox: Mailbox::new(),
            stop: AtomicBool::new(false),
        });
        let mut held = Held::new();
        let root = root.to_owned();

        let thread_shared = Arc::clone(&shared);
        let thread = thread::Builder::new()
            .name(String::from("flightdeck-store"))
            .spawn(move || {
                // A ring's mapping stays on the thread that maps it.
                let mut ring = Ring::new(root);
                run(&mut held, &mut ring, &thread_shared);
            })?;

        Ok(Self {
            shared,
            thread: Some(thread),
        })
    }

    /// The View the store handed over since the last take, or `None` when it handed over none
    /// or is handing one over now.
    #[must_use]
    pub fn take(&self) -> Option<Arc<View>> {
        self.shared.mailbox.take()
    }
}

impl Drop for Store {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        let Some(thread) = self.thread.take() else {
            unreachable!("only dropping the store takes its thread");
        };

        if let Err(panic) = thread.join() {
            if !thread::panicking() {
                std::panic::resume_unwind(panic);
            }
        }
    }
}

/// What the store thread holds between steps.
struct Held {
    span: Span<MachineSecond>,
    views: Buffers<View>,
    version: u64,
    failure: Option<Failure>,
    /// Whether the store holds what no View it handed over shows.
    unpublished: bool,
}

impl Held {
    fn new() -> Self {
        let seconds = Buffers::new(SECONDS_MAX + SECONDS_HELD_MAX, MachineSecond::default);
        let views = Buffers::new(VIEWS_MAX, || View {
            version: 0,
            seconds: Vec::with_capacity(SECONDS_MAX),
            failure: None,
        });

        Self {
            span: Span::new(SECONDS_MAX, seconds),
            views,
            version: 0,
            failure: None,
            unpublished: true,
        }
    }

    /// Hands the main thread a View of what the store holds, or leaves it to the next step
    /// where the main thread holds every View.
    fn publish(&mut self, mailbox: &Mailbox<View>) {
        self.views.release(|view| view.seconds.clear());
        let version = self.version + 1;
        let span = &self.span;
        let failure = self.failure;
        let written = self.views.write(|view| {
            assert!(view.seconds.is_empty());
            let capacity = view.seconds.capacity();

            view.version = version;
            view.seconds.extend(span.held().map(Arc::clone));
            view.failure = failure;

            assert_eq!(view.seconds.capacity(), capacity);
        });
        let Some(view) = written else {
            return;
        };

        mailbox.put(view);
        self.version = version;
        self.unpublished = false;
    }
}

/// Steps until the store is dropped.
fn run(held: &mut Held, ring: &mut Ring, shared: &Shared) {
    let mut waited = Ok(());
    for _ in 0..STEPS_MAX {
        if shared.stop.load(Ordering::Acquire) {
            return;
        }

        step(held, ring, waited, &shared.mailbox);
        waited = if held.failure.is_some() {
            // A ring that failed may fail again at once, so the thread sleeps instead.
            thread::sleep(WAIT);
            Ok(())
        } else {
            ring.wait(WAIT)
        };
    }

    unreachable!("the store thread steps until the store is dropped");
}

/// Takes in what the ring published since the last step, follows the newest run, and hands
/// over a View where anything changed. `waited` is how the wait before the step went.
fn step(held: &mut Held, ring: &mut Ring, waited: Result<(), Failure>, mailbox: &Mailbox<View>) {
    let mut failure = waited.err();

    let span = &mut held.span;
    let unpublished = &mut held.unpublished;
    let read = ring.read(|second| {
        let pushed = span.push(|buffer| *buffer = *second);
        if pushed {
            *unpublished = true;
        }
        pushed
    });
    if let Err(read) = read {
        failure = Some(read);
    }

    if let Err(followed) = ring.follow() {
        failure = Some(followed);
    }

    if failure != held.failure {
        held.failure = failure;
        held.unpublished = true;
    }
    if held.unpublished {
        held.publish(mailbox);
    }
}
