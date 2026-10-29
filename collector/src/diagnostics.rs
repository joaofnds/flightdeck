mod group;
mod writer;

use std::fmt;
use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Barrier, Mutex, OnceLock};
use std::thread::Thread;
use std::time::{Duration, Instant};

pub use group::Group;

pub const GROUP_BYTES: usize = 8 * 1024;
const QUEUE_SLOTS: usize = 4;
const WRITER_STACK_BYTES: usize = 128 * 1024;
const DRAIN_LIMIT: Duration = Duration::from_millis(100);

const _: () = {
    assert!(GROUP_BYTES > 5_028);
    assert!(QUEUE_SLOTS > 1);
    assert!(WRITER_STACK_BYTES > GROUP_BYTES * 2);
};

static INSTALLED: AtomicBool = AtomicBool::new(false);
static REPORTER: Reporter = Reporter::new();

#[derive(Clone, Copy)]
pub enum Role {
    Collector,
    Persister,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stream {
    Stdout,
    Stderr,
}

impl Stream {
    const fn index(self) -> usize {
        match self {
            Self::Stdout => 0,
            Self::Stderr => 1,
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Stdout => "stdout",
            Self::Stderr => "stderr",
        }
    }
}

struct Reporter {
    // Only producers take admission and store admitted, and only the writer stores taken,
    // so the writer copying a group out of its slot never holds anything a producer needs.
    admission: Mutex<()>,
    slots: [Mutex<Slot>; QUEUE_SLOTS],
    admitted: AtomicUsize,
    taken: AtomicUsize,
    staging: Mutex<Group>,
    loss: [Loss; 2],
    enabled: [AtomicBool; 2],
    role: AtomicU8,
    thread: OnceLock<Thread>,
    ready: Barrier,
    stopping: AtomicBool,
    stopped: AtomicBool,
}

struct Slot {
    group: Group,
    stream: Stream,
}

struct Loss {
    queue_full: AtomicU64,
    staging_contention: AtomicU64,
    queue_contention: AtomicU64,
    oversize: AtomicU64,
    failed_delivery: AtomicU64,
    setup_status: AtomicI32,
    counts_inexact: AtomicBool,
}

impl Loss {
    const fn new() -> Self {
        Self {
            queue_full: AtomicU64::new(0),
            staging_contention: AtomicU64::new(0),
            queue_contention: AtomicU64::new(0),
            oversize: AtomicU64::new(0),
            failed_delivery: AtomicU64::new(0),
            setup_status: AtomicI32::new(0),
            counts_inexact: AtomicBool::new(false),
        }
    }
}

/// Installs process-lifetime reporting before collection or persistence starts.
pub fn install(role: Role) {
    assert!(!INSTALLED.swap(true, Ordering::AcqRel));
    REPORTER.role.store(
        match role {
            Role::Collector => 0,
            Role::Persister => 1,
        },
        Ordering::Relaxed,
    );
    REPORTER.prepare();
    writer::start(&REPORTER);
}

/// Allocates the collector's separate stdout staging buffer before collection starts.
#[must_use]
pub fn stdout_group() -> Box<Group> {
    Box::new(Group::empty())
}

/// Reports one complete diagnostic event, retaining other callers' current policy.
pub fn error(arguments: fmt::Arguments<'_>) {
    if INSTALLED.load(Ordering::Acquire) {
        REPORTER.diagnostic(arguments);
    } else {
        eprintln!("{arguments}");
    }
}

/// Terminates normally after recording resources are released and a bounded final drain.
pub fn exit(status: i32) -> ! {
    if INSTALLED.load(Ordering::Acquire) {
        REPORTER.stopping.store(true, Ordering::Release);
        REPORTER.notify();
        let deadline = Instant::now() + DRAIN_LIMIT;

        for _attempt in 0..100 {
            if REPORTER.stopped.load(Ordering::Acquire) {
                break;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            std::thread::sleep(remaining.min(Duration::from_millis(1)));
        }
    }
    std::process::exit(status)
}

impl Reporter {
    #[expect(
        clippy::large_stack_arrays,
        reason = "The production reporter is static; only tests construct local reporters."
    )]
    const fn new() -> Self {
        Self {
            admission: Mutex::new(()),
            slots: [const {
                Mutex::new(Slot {
                    group: Group::empty(),
                    stream: Stream::Stdout,
                })
            }; QUEUE_SLOTS],
            admitted: AtomicUsize::new(0),
            taken: AtomicUsize::new(0),
            staging: Mutex::new(Group::empty()),
            loss: [const { Loss::new() }; 2],
            enabled: [const { AtomicBool::new(true) }; 2],
            role: AtomicU8::new(0),
            thread: OnceLock::new(),
            ready: Barrier::new(2),
            stopping: AtomicBool::new(false),
            stopped: AtomicBool::new(false),
        }
    }

    fn prepare(&self) {
        drop(match self.admission.lock() {
            Ok(admission) => admission,
            Err(error) => unreachable!("diagnostic admission poisoned: {error}"),
        });
        for slot in &self.slots {
            drop(match slot.lock() {
                Ok(slot) => slot,
                Err(error) => unreachable!("diagnostic slot poisoned: {error}"),
            });
        }
        drop(match self.staging.lock() {
            Ok(staging) => staging,
            Err(error) => unreachable!("diagnostic staging poisoned: {error}"),
        });
    }

    fn diagnostic(&self, arguments: fmt::Arguments<'_>) {
        let stream = Stream::Stderr;
        let loss = &self.loss[stream.index()];
        if !self.enabled[stream.index()].load(Ordering::Relaxed) {
            self.reject(loss, &loss.failed_delivery);
            return;
        }
        let Ok(mut staging) = self.staging.try_lock() else {
            self.reject(loss, &loss.staging_contention);
            return;
        };
        staging.begin();
        if fmt::write(&mut *staging, arguments).is_err() {
            self.reject(loss, &loss.failed_delivery);
            return;
        }
        if staging.write_all(b"\n").is_err() {
            unreachable!("bounded staging absorbs byte-write errors");
        }

        self.admit(stream, &staging);
    }

    fn admit(&self, stream: Stream, group: &Group) {
        let loss = &self.loss[stream.index()];
        if !self.enabled[stream.index()].load(Ordering::Relaxed) {
            self.reject(loss, &loss.failed_delivery);
            return;
        }
        if group.oversized {
            self.reject(loss, &loss.oversize);
            return;
        }
        if group.length == 0 {
            return;
        }
        let Ok(admission) = self.admission.try_lock() else {
            self.reject(loss, &loss.queue_contention);
            return;
        };
        let admitted = self.admitted.load(Ordering::Relaxed);
        let taken = self.taken.load(Ordering::Acquire);
        assert!(taken <= admitted);
        assert!(admitted - taken <= QUEUE_SLOTS);
        if admitted - taken == QUEUE_SLOTS {
            self.reject(loss, &loss.queue_full);
            return;
        }
        let index = admitted % QUEUE_SLOTS;
        let mut slot = match self.slots[index].try_lock() {
            Ok(slot) => slot,
            Err(error) => unreachable!("diagnostic slot {index} held past its take: {error}"),
        };
        slot.group.copy_from(group);
        slot.stream = stream;
        drop(slot);
        self.admitted.store(admitted + 1, Ordering::Release);
        drop(admission);

        self.notify();
    }

    fn take(&self, copy: impl FnOnce(&Group)) -> Option<Stream> {
        let taken = self.taken.load(Ordering::Relaxed);
        let admitted = self.admitted.load(Ordering::Acquire);
        assert!(taken <= admitted);
        assert!(admitted - taken <= QUEUE_SLOTS);
        if admitted == taken {
            return None;
        }
        let index = taken % QUEUE_SLOTS;
        let slot = match self.slots[index].try_lock() {
            Ok(slot) => slot,
            Err(error) => unreachable!("diagnostic slot {index} held past its admission: {error}"),
        };
        copy(&slot.group);
        let stream = slot.stream;
        drop(slot);
        let previous = self.taken.fetch_add(1, Ordering::Release);
        assert_eq!(taken, previous, "only the writer takes diagnostic groups");

        Some(stream)
    }

    fn notify(&self) {
        if let Some(thread) = self.thread.get() {
            thread.unpark();
        }
    }

    fn reject(&self, loss: &Loss, counter: &AtomicU64) {
        increment(counter, &loss.counts_inexact);
        self.notify();
    }
}

fn increment(counter: &AtomicU64, counts_inexact: &AtomicBool) {
    let count = counter.load(Ordering::Relaxed);
    let result = counter.compare_exchange(
        count,
        count.saturating_add(1),
        Ordering::Relaxed,
        Ordering::Relaxed,
    );

    if result.is_err() {
        counts_inexact.store(true, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests;
