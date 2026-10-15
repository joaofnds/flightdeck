use std::ffi::{c_int, c_void};
use std::io;
use std::mem::offset_of;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::Duration;

use crate::clock::{self, Timebase};
use crate::coalitions;
use crate::cost::{Meter, Source};
use crate::kernel::{EPERM, ESRCH, Timespec};
use crate::processes::{self, Unreadable, Usage};
use crate::push_without_growing;

#[derive(Debug, PartialEq)]
pub enum Event {
    Start {
        pid: i32,
        coalition: Option<u64>,
    },
    Exec {
        pid: i32,
    },
    Exit {
        pid: i32,
        last: Result<Usage, Unreadable>,
        noticed: Noticed,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Noticed {
    OnExit,
    AlreadyExited,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counts {
    pub forks: u64,
    pub refused_watches: u64,
    pub leading_rescans: u64,
    pub trailing_rescans: u64,
    pub listing_rescans: u64,
    pub dropped_starts: u64,
    pub dropped_execs: u64,
    pub dropped_exits: u64,
}

impl Counts {
    /// The rescans counted, whatever their cause.
    #[must_use]
    pub fn rescans(&self) -> u64 {
        self.leading_rescans + self.trailing_rescans + self.listing_rescans
    }

    #[must_use]
    pub fn since(&self, earlier: &Counts) -> Counts {
        assert!(earlier.forks <= self.forks);
        assert!(earlier.refused_watches <= self.refused_watches);
        assert!(earlier.leading_rescans <= self.leading_rescans);
        assert!(earlier.trailing_rescans <= self.trailing_rescans);
        assert!(earlier.listing_rescans <= self.listing_rescans);
        assert!(earlier.dropped_starts <= self.dropped_starts);
        assert!(earlier.dropped_execs <= self.dropped_execs);
        assert!(earlier.dropped_exits <= self.dropped_exits);

        Counts {
            forks: self.forks - earlier.forks,
            refused_watches: self.refused_watches - earlier.refused_watches,
            leading_rescans: self.leading_rescans - earlier.leading_rescans,
            trailing_rescans: self.trailing_rescans - earlier.trailing_rescans,
            listing_rescans: self.listing_rescans - earlier.listing_rescans,
            dropped_starts: self.dropped_starts - earlier.dropped_starts,
            dropped_execs: self.dropped_execs - earlier.dropped_execs,
            dropped_exits: self.dropped_exits - earlier.dropped_exits,
        }
    }
}

/// The most process events a tick holds. A burst past it drops the rest and counts them by
/// kind, since a burst is a condition the collector runs under and no fault. The busiest
/// bursts docs/recording-research.md measured forked 1,745 times a second, each fork a start
/// and an exit and most an exec, so 16,384 holds about three seconds of them.
pub const EVENTS_PER_TICK_MAX: usize = 16_384;

const _: () = assert!(EVENTS_PER_TICK_MAX > 0);

/// The room a tracker takes once, when it starts.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    /// The most pids a listing holds.
    pub process_limit: usize,
    /// The most process events a tick holds.
    pub events_per_tick_max: usize,
}

pub struct Tracker {
    queue: OwnedFd,
    timebase: Timebase,
    listing: Vec<i32>,
    known: Vec<Known>,
    reuse_candidates: Vec<(i32, usize)>,
    scans: u64,
    throttle: Throttle,
    timers: Vec<Kevent>,
    changes: Vec<Kevent>,
    eventlist: Vec<Kevent>,
    starts: Vec<i32>,
    exits: Vec<(i32, Noticed)>,
    events: Vec<Event>,
    counts: Counts,
    /// Set for the length of every call that can fail and cleared as it succeeds, so it holds
    /// after a call that returned an error. That call left the lists and the kqueue part-way
    /// through a change, so no call follows it.
    failed: bool,
}

#[derive(Clone, Copy, Default)]
struct Known {
    listed_in: u64,
    exited: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Wake {
    due: bool,
    rescan_timer_fired: bool,
    forked: bool,
}

struct Throttle {
    interval_ticks: u64,
    rescan_ticks_last: u64,
    pending: bool,
    deferred: bool,
    armed: bool,
}

#[derive(Debug, PartialEq, Eq)]
enum Step {
    Rescan(Cause),
    ArmAt(u64),
    Wait,
}

#[derive(Debug, PartialEq, Eq)]
enum Cause {
    Leading,
    Trailing,
}

impl Throttle {
    fn new(interval_ticks: u64) -> Self {
        Self {
            interval_ticks,
            rescan_ticks_last: 0,
            pending: false,
            deferred: false,
            armed: false,
        }
    }

    fn woke(&mut self, wake: Wake) {
        // A rescan is deferred only while one is pending.
        if self.deferred {
            assert!(self.pending);
        }

        if wake.rescan_timer_fired {
            self.armed = false;
        }
        if wake.forked {
            self.pending = true;
        }
    }

    fn rescanned(&mut self, now_ticks: u64) {
        // A rescan is deferred only while one is pending.
        if self.deferred {
            assert!(self.pending);
        }

        self.rescan_ticks_last = now_ticks;
        self.pending = false;
        self.deferred = false;
    }

    fn step(&mut self, now_ticks: u64) -> Step {
        if self.deferred {
            assert!(self.pending);
        }

        if !self.pending {
            return Step::Wait;
        }

        if now_ticks.saturating_sub(self.rescan_ticks_last) >= self.interval_ticks {
            return Step::Rescan(if self.deferred {
                Cause::Trailing
            } else {
                Cause::Leading
            });
        }

        self.deferred = true;
        if self.armed {
            Step::Wait
        } else {
            self.armed = true;
            let at_ticks = self.rescan_ticks_last.saturating_add(self.interval_ticks);
            assert!(now_ticks <= at_ticks);
            Step::ArmAt(at_ticks)
        }
    }
}

const DEADLINE_TIMER: usize = 1;
const RESCAN_TIMER: usize = 2;
/// The timers a wait arms at most, the deadline and the rescan, each once.
const TIMER_COUNT_MAX: usize = 2;
const EVENTS_PER_WAIT_MAX: usize = 64;

const _: () = assert!(DEADLINE_TIMER != RESCAN_TIMER);
const _: () = assert!(EVENTS_PER_WAIT_MAX > 0);

/// One change to a kqueue, or one event it reports, as `struct kevent` declares it.
#[repr(C)]
#[derive(Clone, Copy)]
struct Kevent {
    ident: usize,
    filter: i16,
    flags: u16,
    fflags: u32,
    data: isize,
    udata: *mut c_void,
}

const _: () = assert!(size_of::<Kevent>() == 32);
const _: () = assert!(offset_of!(Kevent, ident) == 0);
const _: () = assert!(offset_of!(Kevent, filter) == 8);
const _: () = assert!(offset_of!(Kevent, flags) == 10);
const _: () = assert!(offset_of!(Kevent, fflags) == 12);
const _: () = assert!(offset_of!(Kevent, data) == 16);
const _: () = assert!(offset_of!(Kevent, udata) == 24);

impl Kevent {
    fn new(ident: usize, filter: i16, flags: u16, filter_flags: u32, data: isize) -> Self {
        Self {
            ident,
            filter,
            flags,
            fflags: filter_flags,
            data,
            udata: std::ptr::null_mut(),
        }
    }
}

const EVFILT_PROC: i16 = -5;
const EVFILT_TIMER: i16 = -7;
const EV_ADD: u16 = 0x1;
const EV_ONESHOT: u16 = 0x10;
const EV_RECEIPT: u16 = 0x40;
const EV_ERROR: u16 = 0x4000;
const NOTE_EXIT: u32 = 0x8000_0000;
const NOTE_FORK: u32 = 0x4000_0000;
const NOTE_EXEC: u32 = 0x2000_0000;
const NOTE_ABSOLUTE: u32 = 0x8;
const NOTE_MACHTIME: u32 = 0x100;
const NOTE_MACH_CONTINUOUS_TIME: u32 = 0x80;

unsafe extern "C" {
    fn kqueue() -> c_int;
    fn kevent(
        queue: c_int,
        changes: *const Kevent,
        change_count: c_int,
        events: *mut Kevent,
        event_capacity: c_int,
        timeout: *const Timespec,
    ) -> c_int;
}

impl Tracker {
    pub fn new(timebase: Timebase, rescan_interval: Duration, limits: Limits) -> io::Result<Self> {
        assert!(limits.process_limit > 0);
        assert!(limits.events_per_tick_max > 0);

        let Ok(interval_ns) = u64::try_from(rescan_interval.as_nanos()) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("a rescan interval of {rescan_interval:?} passes u64 nanoseconds"),
            ));
        };

        // A scan, a registration and a wait each hold at most one entry for every pid a
        // listing holds, so each list gets the listing's capacity once and never grows. A wait
        // takes up to EVENTS_PER_WAIT_MAX events too, so the eventlist holds at least that.
        let listing = processes::pid_list(limits.process_limit);
        let capacity = listing.capacity();
        let known_count = usize::try_from(processes::PID_MAX).map_err(io::Error::other)? + 1;

        let queue = unsafe { kqueue() };
        if queue < 0 {
            return Err(io::Error::last_os_error());
        }
        let queue = unsafe { OwnedFd::from_raw_fd(queue) };

        let mut tracker = Self {
            queue,
            timebase,
            listing,
            known: vec![Known::default(); known_count],
            reuse_candidates: Vec::with_capacity(capacity),
            scans: 0,
            throttle: Throttle::new(timebase.ticks(interval_ns)?),
            timers: Vec::with_capacity(TIMER_COUNT_MAX),
            changes: Vec::with_capacity(capacity),
            eventlist: vec![Kevent::new(0, 0, 0, 0, 0); capacity.max(EVENTS_PER_WAIT_MAX)],
            starts: Vec::with_capacity(capacity),
            exits: Vec::with_capacity(capacity),
            events: Vec::with_capacity(limits.events_per_tick_max),
            counts: Counts::default(),
            failed: false,
        };
        tracker.rescan(&mut Meter::default())?;

        assert_eq!(1, tracker.scans);
        assert!(tracker.timers.is_empty());
        assert!(tracker.starts.is_empty());
        assert!(tracker.exits.is_empty());
        assert!(EVENTS_PER_WAIT_MAX <= tracker.eventlist.len());
        Ok(tracker)
    }

    pub fn follow_until(&mut self, deadline_ticks: u64, meter: &mut Meter) -> io::Result<()> {
        assert!(!self.failed, "a call to the tracker failed before");
        assert!(self.timers.len() < TIMER_COUNT_MAX);
        self.failed = true;
        push_without_growing(
            &mut self.timers,
            follow_until_timer(DEADLINE_TIMER, deadline_ticks)?,
        );

        loop {
            let woke = meter.measure(Source::Events, || {
                let count = self.follow_until_wait()?;
                follow_until_handle(
                    &self.eventlist[..count],
                    &mut self.events,
                    &mut self.counts,
                    &mut self.exits,
                )
            })?;
            assert!(self.timers.is_empty());

            self.read_exits(meter)?;
            self.throttle.woke(woke);

            match self.throttle.step(clock::now()) {
                Step::Rescan(cause) => {
                    self.rescan(meter)?;
                    match cause {
                        Cause::Leading => self.counts.leading_rescans += 1,
                        Cause::Trailing => self.counts.trailing_rescans += 1,
                    }
                }
                Step::ArmAt(at_ticks) => {
                    push_without_growing(
                        &mut self.timers,
                        follow_until_timer(RESCAN_TIMER, at_ticks)?,
                    );
                }
                Step::Wait => {}
            }

            if woke.due {
                assert!(self.starts.is_empty());
                assert!(self.exits.is_empty());
                // The tracker scans once as it starts, and counts each rescan since by its cause.
                assert_eq!(self.scans, 1 + self.counts.rescans());
                self.failed = false;
                return Ok(());
            }
        }
    }

    pub fn observe(&mut self, listing: &[i32], meter: &mut Meter) -> io::Result<()> {
        assert!(!self.failed, "a call to the tracker failed before");
        self.failed = true;

        meter.measure(Source::Scans, || {
            self.compare(listing)?;
            self.observe_reused_pids()
        })?;
        self.throttle.rescanned(clock::now());
        self.counts.listing_rescans += 1;
        self.read_starts(meter)?;
        self.submit_watches(meter)?;

        assert!(self.starts.is_empty());
        assert!(self.exits.is_empty());
        // The tracker scans once as it starts, and counts each rescan since by its cause.
        assert_eq!(self.scans, 1 + self.counts.rescans());
        self.failed = false;
        Ok(())
    }

    pub fn drain_events(&mut self) -> std::vec::Drain<'_, Event> {
        assert!(!self.failed, "a call to the tracker failed before");
        assert!(self.starts.is_empty());
        assert!(self.exits.is_empty());
        self.events.drain(..)
    }

    /// What the tracker has counted since it started.
    #[must_use]
    pub fn counts(&self) -> Counts {
        assert!(!self.failed, "a call to the tracker failed before");
        self.counts
    }

    #[must_use]
    pub fn last_listing(&self) -> &[i32] {
        assert!(!self.failed, "a call to the tracker failed before");
        assert!(self.listing.capacity() > 0);
        &self.listing
    }

    fn rescan(&mut self, meter: &mut Meter) -> io::Result<()> {
        let scans = self.scans;
        let capacity = self.listing.capacity();
        let mut listing = std::mem::take(&mut self.listing);
        let scanned = meter.measure(Source::Scans, || {
            processes::list_pids(&mut listing)?;
            self.compare(&listing)
        });
        self.listing = listing;
        assert_eq!(capacity, self.listing.capacity());
        scanned?;
        assert_eq!(scans + 1, self.scans);
        self.throttle.rescanned(clock::now());

        self.read_starts(meter)?;
        self.submit_watches(meter)
    }

    fn compare(&mut self, listing: &[i32]) -> io::Result<()> {
        assert!(listing.len() < self.changes.capacity());
        assert!(self.starts.is_empty());
        // The exits queued before are read, so each known pid's `exited` is current.
        assert!(self.exits.is_empty());
        let previous = self.scans;
        self.scans += 1;
        let reporting = previous > 0;
        self.changes.clear();
        self.reuse_candidates.clear();

        for &pid in listing {
            let index = known_index(pid, self.known.len())?;
            let known = &mut self.known[index];
            if known.listed_in == self.scans {
                // A pid the listing already held names the process compared there.
                continue;
            }

            assert!(known.listed_in <= previous);
            let listed_before = if reporting {
                known.listed_in == previous
            } else {
                false
            };

            known.listed_in = self.scans;
            if listed_before {
                if known.exited {
                    push_without_growing(&mut self.reuse_candidates, (pid, index));
                }
            } else {
                known.exited = false;
                push_without_growing(&mut self.changes, watch(index));
                if reporting {
                    push_without_growing(&mut self.starts, pid);
                }
            }
        }

        // A pid adds a watch or becomes a candidate, never both, so the candidates' watches fit.
        assert!(self.changes.len() + self.reuse_candidates.len() <= listing.len());
        Ok(())
    }

    fn observe_reused_pids(&mut self) -> io::Result<()> {
        assert!(self.changes.len() + self.reuse_candidates.len() <= self.changes.capacity());

        for &(pid, index) in &self.reuse_candidates {
            assert!(self.known[index].exited);
            assert_eq!(self.scans, self.known[index].listed_in);
            if processes::is_alive(pid)? {
                self.known[index].exited = false;
                push_without_growing(&mut self.changes, watch(index));
                push_without_growing(&mut self.starts, pid);
            }
        }

        Ok(())
    }

    fn read_starts(&mut self, meter: &mut Meter) -> io::Result<()> {
        if self.starts.is_empty() {
            return Ok(());
        }

        meter.measure(Source::StartReads, || {
            for pid in self.starts.drain(..) {
                assert!(pid >= 0);
                let start = Event::Start {
                    pid,
                    coalition: coalitions::coalition_of(pid)?,
                };
                record(&mut self.events, &mut self.counts, start);
            }

            io::Result::Ok(())
        })?;

        assert!(self.starts.is_empty());
        Ok(())
    }

    fn read_exits(&mut self, meter: &mut Meter) -> io::Result<()> {
        // The starts are read first, so a process's start reaches the events before its exit.
        assert!(self.starts.is_empty());
        if self.exits.is_empty() {
            return Ok(());
        }

        meter.measure(Source::ExitReads, || {
            for (pid, noticed) in self.exits.drain(..) {
                let index = known_index(pid, self.known.len())?;
                self.known[index].exited = true;
                let exit = Event::Exit {
                    pid,
                    last: processes::read_usage(self.timebase, pid)?,
                    noticed,
                };
                record(&mut self.events, &mut self.counts, exit);
            }

            io::Result::Ok(())
        })?;

        assert!(self.exits.is_empty());
        Ok(())
    }

    fn submit_watches(&mut self, meter: &mut Meter) -> io::Result<()> {
        if self.changes.is_empty() {
            return Ok(());
        }

        assert!(self.exits.is_empty());
        meter.measure(Source::Watches, || {
            let receipts = self.submit_watches_register()?;
            submit_watches_receipts(
                &self.eventlist[..receipts],
                &mut self.counts,
                &mut self.exits,
            )
        })?;
        self.read_exits(meter)?;

        assert!(self.exits.is_empty());
        Ok(())
    }

    fn submit_watches_register(&mut self) -> io::Result<usize> {
        assert!(!self.changes.is_empty());
        assert!(self.changes.len() <= self.eventlist.len());
        let count = c_int::try_from(self.changes.len()).map_err(io::Error::other)?;
        let none_pending = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let receipts = unsafe {
            kevent(
                self.queue.as_raw_fd(),
                self.changes.as_ptr(),
                count,
                self.eventlist.as_mut_ptr(),
                count,
                &raw const none_pending,
            )
        };

        let Ok(receipts) = usize::try_from(receipts) else {
            return Err(io::Error::last_os_error());
        };
        if receipts <= self.changes.len() {
            assert!(receipts <= self.eventlist.len());
            Ok(receipts)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("kevent returned {receipts} receipts for {count} changes"),
            ))
        }
    }

    fn follow_until_wait(&mut self) -> io::Result<usize> {
        assert!(self.timers.len() <= TIMER_COUNT_MAX);
        assert!(EVENTS_PER_WAIT_MAX <= self.eventlist.len());
        let timer_count = c_int::try_from(self.timers.len()).map_err(io::Error::other)?;
        let event_count_max = c_int::try_from(EVENTS_PER_WAIT_MAX).map_err(io::Error::other)?;

        let count = unsafe {
            kevent(
                self.queue.as_raw_fd(),
                self.timers.as_ptr(),
                timer_count,
                self.eventlist.as_mut_ptr(),
                event_count_max,
                std::ptr::null(),
            )
        };
        let Ok(count) = usize::try_from(count) else {
            return Err(io::Error::last_os_error());
        };

        self.timers.clear();
        if count <= EVENTS_PER_WAIT_MAX {
            // The caller reads the events up to `count`, which the list holds.
            assert!(count <= self.eventlist.len());
            Ok(count)
        } else {
            Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("kevent returned {count} events for room of {EVENTS_PER_WAIT_MAX}"),
            ))
        }
    }
}

/// Handles the events one wait returned in its `eventlist`: records each exec among the tick's
/// `events`, queues each exit among the `exits` to read, counts each fork, and returns what the
/// wait woke for. An event that reports an error is that error.
fn follow_until_handle(
    eventlist: &[Kevent],
    events: &mut Vec<Event>,
    counts: &mut Counts,
    exits: &mut Vec<(i32, Noticed)>,
) -> io::Result<Wake> {
    assert!(eventlist.len() <= EVENTS_PER_WAIT_MAX);
    assert!(exits.is_empty());

    let mut wake = Wake::default();
    for event in eventlist {
        if event.flags & EV_ERROR != 0 {
            return Err(io::Error::from_raw_os_error(errno_of(event.data)?));
        }

        if event.filter == EVFILT_TIMER {
            if event.ident == DEADLINE_TIMER {
                wake.due = true;
            } else {
                wake.rescan_timer_fired = true;
            }
        } else {
            let pid = pid_of(event.ident)?;
            if event.fflags & NOTE_EXEC != 0 {
                record(events, counts, Event::Exec { pid });
            }
            if event.fflags & NOTE_EXIT != 0 {
                push_without_growing(exits, (pid, Noticed::OnExit));
            }
            if event.fflags & NOTE_FORK != 0 {
                counts.forks += 1;
                wake.forked = true;
            }
        }
    }

    assert!(exits.len() <= eventlist.len());
    Ok(wake)
}

/// Reads the `receipts` one registration of watches returned: counts each watch the kernel
/// refused, and queues each process already gone among the `exits` to read. A receipt that
/// reports any other error is that error, and one without `EV_ERROR` holds no errno to read.
fn submit_watches_receipts(
    receipts: &[Kevent],
    counts: &mut Counts,
    exits: &mut Vec<(i32, Noticed)>,
) -> io::Result<()> {
    assert!(exits.is_empty());

    for receipt in receipts {
        let pid = pid_of(receipt.ident)?;
        if receipt.flags & EV_ERROR != 0 {
            match errno_of(receipt.data)? {
                0 => {}
                EPERM => counts.refused_watches += 1,
                ESRCH => push_without_growing(exits, (pid, Noticed::AlreadyExited)),
                error => return Err(io::Error::from_raw_os_error(error)),
            }
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("the kevent receipt for pid {pid} carries no error code"),
            ));
        }
    }

    assert!(exits.len() <= receipts.len());
    Ok(())
}

/// Keeps `event` among the tick's `events` while they have room for it, and otherwise
/// counts it among the dropped events of its kind, so the list never grows.
fn record(events: &mut Vec<Event>, counts: &mut Counts, event: Event) {
    let capacity = events.capacity();
    assert!(capacity > 0);

    if events.len() < capacity {
        events.push(event);
    } else {
        match event {
            Event::Start { .. } => counts.dropped_starts += 1,
            Event::Exec { .. } => counts.dropped_execs += 1,
            Event::Exit { .. } => counts.dropped_exits += 1,
        }
    }
}

/// The index of `pid` in a table of `known_count` pids. A pid the table cannot hold is an
/// error, since the kernel gives no pid below 0 or past `PID_MAX`.
fn known_index(pid: i32, known_count: usize) -> io::Result<usize> {
    assert!(known_count > 0);
    let outside = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("pid {pid} is outside the known table, which holds pids below {known_count}"),
        )
    };

    let Ok(index) = usize::try_from(pid) else {
        return Err(outside());
    };
    if index < known_count {
        Ok(index)
    } else {
        Err(outside())
    }
}

/// The pid a process event or a watch's receipt names in its ident.
fn pid_of(ident: usize) -> io::Result<i32> {
    let Ok(pid) = i32::try_from(ident) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a kevent ident of {ident} names no pid"),
        ));
    };

    Ok(pid)
}

/// The errno a kevent reports in the data of an error or a watch's receipt.
fn errno_of(data: isize) -> io::Result<i32> {
    let refused = || {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a kevent error code of {data} holds no errno"),
        )
    };

    let Ok(errno) = i32::try_from(data) else {
        return Err(refused());
    };
    if errno >= 0 {
        Ok(errno)
    } else {
        Err(refused())
    }
}

/// A watch on the process at `index` in the known table, whose index is its pid.
fn watch(index: usize) -> Kevent {
    // A process event names its pid in the ident the watch gives it.
    assert!(i32::try_from(index).is_ok());

    Kevent::new(
        index,
        EVFILT_PROC,
        EV_ADD | EV_RECEIPT,
        NOTE_FORK | NOTE_EXEC | NOTE_EXIT,
        0,
    )
}

fn follow_until_timer(ident: usize, deadline_ticks: u64) -> io::Result<Kevent> {
    if ident != DEADLINE_TIMER {
        assert_eq!(RESCAN_TIMER, ident);
    }

    let Ok(data) = isize::try_from(deadline_ticks) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("a deadline of {deadline_ticks} ticks passes what a kevent timer holds"),
        ));
    };

    Ok(Kevent::new(
        ident,
        EVFILT_TIMER,
        EV_ADD | EV_ONESHOT,
        NOTE_ABSOLUTE | NOTE_MACHTIME | NOTE_MACH_CONTINUOUS_TIME,
        data,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock;
    use crate::cost::Snapshot;
    use crate::kernel::EINVAL;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    const INTERVAL: Duration = Duration::from_millis(50);

    fn limits() -> Limits {
        Limits {
            process_limit: processes::process_limit().unwrap(),
            events_per_tick_max: EVENTS_PER_TICK_MAX,
        }
    }

    fn tracker(rescan_interval: Duration) -> Tracker {
        Tracker::new(Timebase::read().unwrap(), rescan_interval, limits()).unwrap()
    }

    /// The pids this Mac runs now, in a list with the room a tracker gives its own.
    fn current_listing() -> Vec<i32> {
        let mut listing = processes::pid_list(limits().process_limit);
        processes::list_pids(&mut listing).unwrap();
        listing
    }

    /// The highest pid `listing` does not hold, so no process this Mac ran when it listed.
    fn unlisted_pid(listing: &[i32]) -> i32 {
        (0..=processes::PID_MAX)
            .rev()
            .find(|pid| !listing.contains(pid))
            .expect("a pid the listing does not hold")
    }

    fn rescans_from_forks(counts: &Counts) -> u64 {
        counts.leading_rescans + counts.trailing_rescans
    }

    fn spawn_sleeper() -> std::process::Child {
        std::process::Command::new("/bin/sleep")
            .arg("0.2")
            .spawn()
            .unwrap()
    }

    fn while_forking_every_2_ms<T>(follow: impl FnOnce() -> T) -> T {
        let stop = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    std::process::Command::new("/usr/bin/true")
                        .status()
                        .unwrap();
                    std::thread::sleep(Duration::from_millis(2));
                }
            });
            // A panic in the follow stops the forks too, or the scope would wait on them forever
            // and a failing test would hang.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(follow));
            stop.store(true, Ordering::Relaxed);
            match result {
                Ok(result) => result,
                Err(panic) => std::panic::resume_unwind(panic),
            }
        })
    }

    /// A tick's event list with room for one event, which it already holds.
    fn full_event_list() -> Vec<Event> {
        vec![Event::Exec { pid: 1 }]
    }

    #[test]
    fn keeps_an_event_while_the_tick_has_room_for_it() {
        // Goal: an event is kept while the tick's list has room for it, and nothing is dropped.
        // Method: record one event into a list with room for one.
        let mut events = Vec::with_capacity(1);
        let mut counts = Counts::default();

        record(&mut events, &mut counts, Event::Exec { pid: 1 });

        assert_eq!(1, events.len());
        assert_eq!(Counts::default(), counts);
    }

    #[test]
    fn drops_a_start_past_the_bound_and_counts_it() {
        // Goal: a start past the tick's bound is dropped and counted as a start, and the list
        // keeps its room.
        // Method: record a start into a full list with room for one.
        let mut events = full_event_list();
        let capacity = events.capacity();
        let mut counts = Counts::default();

        let start = Event::Start {
            pid: 2,
            coalition: None,
        };
        record(&mut events, &mut counts, start);

        assert_eq!(1, events.len());
        assert_eq!(capacity, events.capacity());
        let dropped = Counts {
            dropped_starts: 1,
            ..Counts::default()
        };
        assert_eq!(dropped, counts);
    }

    #[test]
    fn drops_an_exec_past_the_bound_and_counts_it() {
        // Goal: an exec past the tick's bound is dropped and counted as an exec, and the list
        // keeps its room.
        // Method: record an exec into a full list with room for one.
        let mut events = full_event_list();
        let capacity = events.capacity();
        let mut counts = Counts::default();

        record(&mut events, &mut counts, Event::Exec { pid: 2 });

        assert_eq!(1, events.len());
        assert_eq!(capacity, events.capacity());
        let dropped = Counts {
            dropped_execs: 1,
            ..Counts::default()
        };
        assert_eq!(dropped, counts);
    }

    #[test]
    fn drops_an_exit_past_the_bound_and_counts_it() {
        // Goal: an exit past the tick's bound is dropped and counted as an exit, and the list
        // keeps its room.
        // Method: record an exit into a full list with room for one.
        let mut events = full_event_list();
        let capacity = events.capacity();
        let mut counts = Counts::default();

        let exit = Event::Exit {
            pid: 2,
            last: Err(Unreadable::Gone),
            noticed: Noticed::OnExit,
        };
        record(&mut events, &mut counts, exit);

        assert_eq!(1, events.len());
        assert_eq!(capacity, events.capacity());
        let dropped = Counts {
            dropped_exits: 1,
            ..Counts::default()
        };
        assert_eq!(dropped, counts);
    }

    #[test]
    fn counts_the_events_dropped_since_an_earlier_count() {
        // Goal: the counts since an earlier count hold the starts, execs and exits dropped in
        // between, so each window reports its own drops.
        // Method: take a count from a later one that dropped a different number of each kind.
        let earlier = Counts {
            dropped_starts: 1,
            dropped_execs: 2,
            dropped_exits: 3,
            ..Counts::default()
        };
        let later = Counts {
            dropped_starts: 5,
            dropped_execs: 7,
            dropped_exits: 9,
            ..Counts::default()
        };

        let since = later.since(&earlier);

        let dropped = Counts {
            dropped_starts: 4,
            dropped_execs: 5,
            dropped_exits: 6,
            ..Counts::default()
        };
        assert_eq!(dropped, since);
    }

    #[test]
    fn keeps_a_tick_to_its_event_bound_and_counts_the_rest_as_dropped() {
        // Goal: a tick that sees more process events than the tracker's bound keeps to the
        // bound and counts the rest as dropped.
        // Method: with room for one event, follow one tick without draining while a thread forks
        // every 2 ms, each fork a start and an exit, until a dropped start or exit is counted.
        let limits = Limits {
            events_per_tick_max: 1,
            ..limits()
        };
        let mut tracker = Tracker::new(Timebase::read().unwrap(), INTERVAL, limits).unwrap();

        while_forking_every_2_ms(|| {
            follow_to_count(&mut tracker, "dropped start or exit", |counts| {
                counts.dropped_starts + counts.dropped_exits > 0
            });
        });

        assert_eq!(1, tracker.drain_events().count());
    }

    #[test]
    fn reports_a_process_limit_below_what_this_mac_runs_as_an_error() {
        // Goal: a process limit too small for the listing is an error and trips no assertion,
        // even below the events one wait takes.
        // Method: start a tracker with room for one process on this Mac, which runs hundreds.
        let limits = Limits {
            process_limit: 1,
            ..limits()
        };

        let started = Tracker::new(Timebase::read().unwrap(), INTERVAL, limits);

        let refused = started.err().map(|error| error.kind());
        assert_eq!(Some(io::ErrorKind::QuotaExceeded), refused);
    }

    #[test]
    fn refuses_a_rescan_interval_whose_nanoseconds_pass_u64() {
        // Goal: a rescan interval longer than u64 holds in nanoseconds is an error, rather than
        // an interval cut short to fit.
        // Method: start a tracker with the longest interval a Duration holds.
        let started = Tracker::new(Timebase::read().unwrap(), Duration::MAX, limits());

        let refused = started.err().map(|error| error.kind());
        assert_eq!(Some(io::ErrorKind::InvalidInput), refused);
    }

    #[test]
    fn refuses_a_listed_pid_past_the_pid_ceiling() {
        // Goal: a listed pid past the pid ceiling is an error, since the known table cannot
        // hold it.
        // Method: observe a listing that holds PID_MAX + 1.
        let mut tracker = tracker(INTERVAL);

        let observed = tracker.observe(&[processes::PID_MAX + 1], &mut Meter::default());

        assert_eq!(io::ErrorKind::InvalidData, observed.unwrap_err().kind());
    }

    #[test]
    #[should_panic(expected = "a call to the tracker failed before")]
    fn refuses_a_call_after_one_failed() {
        // Goal: a tracker is not used after a call to it failed, since what it holds then is
        // part-way through a change.
        // Method: observe a listing that holds a negative pid, which fails, then this Mac's.
        let mut tracker = tracker(INTERVAL);
        let listing = current_listing();
        assert!(tracker.observe(&[-1], &mut Meter::default()).is_err());

        tracker.observe(&listing, &mut Meter::default()).unwrap();
    }

    #[test]
    fn refuses_a_negative_listed_pid() {
        // Goal: a negative listed pid is an error, since the known table cannot hold it.
        // Method: observe a listing that holds -1.
        let mut tracker = tracker(INTERVAL);

        let observed = tracker.observe(&[-1], &mut Meter::default());

        assert_eq!(io::ErrorKind::InvalidData, observed.unwrap_err().kind());
    }

    /// What handling one wait's events left behind.
    struct Handled {
        wake: io::Result<Wake>,
        events: Vec<Event>,
        counts: Counts,
        exits: Vec<(i32, Noticed)>,
    }

    /// Handles `eventlist` as the events one wait returned, into lists with room for all of them.
    fn handle(eventlist: &[Kevent]) -> Handled {
        let mut events = Vec::with_capacity(EVENTS_PER_WAIT_MAX);
        let mut counts = Counts::default();
        let mut exits = Vec::with_capacity(EVENTS_PER_WAIT_MAX);

        let wake = follow_until_handle(eventlist, &mut events, &mut counts, &mut exits);

        Handled {
            wake,
            events,
            counts,
            exits,
        }
    }

    fn process_event(ident: usize, filter_flags: u32) -> Kevent {
        Kevent::new(ident, EVFILT_PROC, 0, filter_flags, 0)
    }

    fn timer_event(ident: usize) -> Kevent {
        Kevent::new(ident, EVFILT_TIMER, 0, 0, 1)
    }

    /// What reading one registration's receipts left behind.
    struct ReceiptsRead {
        result: io::Result<()>,
        counts: Counts,
        exits: Vec<(i32, Noticed)>,
    }

    /// Reads `receipts` as the receipts one registration returned, into a list with room for all
    /// of them.
    fn read_receipts(receipts: &[Kevent]) -> ReceiptsRead {
        let mut counts = Counts::default();
        let mut exits = Vec::with_capacity(receipts.len());

        let result = submit_watches_receipts(receipts, &mut counts, &mut exits);

        ReceiptsRead {
            result,
            counts,
            exits,
        }
    }

    /// The receipt the kernel returns for a watch on `ident`, reporting `errno`, with
    /// `EV_ERROR` set as macOS 27.0.1 sets it on every receipt. It sets the ident after
    /// `watch` builds the rest, since `watch` takes only an ident that names a pid, and a
    /// receipt can name any.
    fn receipt(ident: usize, errno: isize) -> Kevent {
        let mut receipt = watch(0);
        receipt.ident = ident;
        receipt.flags |= EV_ERROR;
        receipt.data = errno;
        receipt
    }

    #[test]
    fn counts_a_watch_the_kernel_refused() {
        // Goal: a watch the kernel refused is counted as refused, and its process is not queued
        // as gone.
        // Method: read one receipt that reports EPERM, which no watch a test makes draws.
        let read = read_receipts(&[receipt(7, isize::try_from(EPERM).unwrap())]);

        read.result.unwrap();
        assert_eq!(1, read.counts.refused_watches);
        assert!(read.exits.is_empty());
    }

    #[test]
    fn queues_a_watch_on_a_process_already_gone_as_already_exited() {
        // Goal: a watch on a process that exited before the watch took is queued as an exit to
        // read, noticed as already exited.
        // Method: read one receipt that reports ESRCH.
        let read = read_receipts(&[receipt(7, isize::try_from(ESRCH).unwrap())]);

        read.result.unwrap();
        assert_eq!(vec![(7, Noticed::AlreadyExited)], read.exits);
    }

    #[test]
    fn refuses_a_receipt_without_an_error_code() {
        // Goal: a receipt without EV_ERROR, whose data then holds no errno, is an error of
        // invalid data, rather than its data read as an errno.
        // Method: read one receipt with EV_ERROR cleared and data 0, which reads as a watch taken.
        let mut taken = receipt(7, 0);
        taken.flags &= !EV_ERROR;

        let read = read_receipts(&[taken]);

        assert_eq!(io::ErrorKind::InvalidData, read.result.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_receipt_whose_ident_names_no_pid() {
        // Goal: a receipt whose ident is past what a pid's i32 holds is an error, rather than
        // pid -1.
        // Method: read one receipt whose ident is the first past i32::MAX.
        let ident = usize::try_from(i32::MAX).unwrap() + 1;

        let read = read_receipts(&[receipt(ident, 0)]);

        assert_eq!(io::ErrorKind::InvalidData, read.result.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_receipt_error_code_no_errno_holds() {
        // Goal: a receipt's error code past what an errno's i32 holds is an error of invalid
        // data, rather than EINVAL.
        // Method: read one receipt whose code is the first past i32::MAX.
        let data = isize::try_from(i32::MAX).unwrap() + 1;

        let read = read_receipts(&[receipt(7, data)]);

        assert_eq!(io::ErrorKind::InvalidData, read.result.unwrap_err().kind());
    }

    #[test]
    fn records_an_exec_under_its_pid() {
        // Goal: an exec a wait receives is recorded among the tick's events under its pid.
        // Method: handle one exec event.
        let handled = handle(&[process_event(7, NOTE_EXEC)]);

        assert!(
            matches!(handled.events[..], [Event::Exec { pid: 7 }]),
            "{:?}",
            handled.events
        );
    }

    #[test]
    fn queues_an_exit_to_read_as_noticed_on_exit() {
        // Goal: an exit a wait receives is queued to read, noticed as it exited.
        // Method: handle one exit event.
        let handled = handle(&[process_event(7, NOTE_EXIT)]);

        assert_eq!(vec![(7, Noticed::OnExit)], handled.exits);
    }

    #[test]
    fn counts_each_fork_and_wakes_for_them() {
        // Goal: each fork a wait receives is counted, and the wake carries them to the throttle.
        // Method: handle two fork events.
        let fork = process_event(7, NOTE_FORK);

        let handled = handle(&[fork, fork]);

        let forked = Wake {
            forked: true,
            ..Wake::default()
        };
        assert_eq!(forked, handled.wake.unwrap());
        assert_eq!(2, handled.counts.forks);
    }

    #[test]
    fn wakes_due_when_the_deadline_timer_fires() {
        // Goal: the deadline timer firing makes the wake due, and nothing else.
        // Method: handle the deadline timer's event.
        let handled = handle(&[timer_event(DEADLINE_TIMER)]);

        let due = Wake {
            due: true,
            ..Wake::default()
        };
        assert_eq!(due, handled.wake.unwrap());
    }

    #[test]
    fn wakes_for_the_rescan_timer_when_it_fires() {
        // Goal: the rescan timer firing is carried to the throttle, and nothing else.
        // Method: handle the rescan timer's event.
        let handled = handle(&[timer_event(RESCAN_TIMER)]);

        let fired = Wake {
            rescan_timer_fired: true,
            ..Wake::default()
        };
        assert_eq!(fired, handled.wake.unwrap());
    }

    #[test]
    fn reports_the_error_a_received_event_carries() {
        // Goal: an event a wait receives flagged as an error is that error.
        // Method: handle an error event that carries ESRCH.
        let failed = Kevent::new(7, EVFILT_PROC, EV_ERROR, 0, isize::try_from(ESRCH).unwrap());

        let handled = handle(&[failed]);

        assert_eq!(Some(ESRCH), handled.wake.unwrap_err().raw_os_error());
    }

    #[test]
    fn refuses_a_received_error_code_no_errno_holds() {
        // Goal: an error code past what an errno's i32 holds is an error of invalid data,
        // rather than EINVAL.
        // Method: handle an error event whose code is the first past i32::MAX.
        let data = isize::try_from(i32::MAX).unwrap() + 1;
        let failed = Kevent::new(7, EVFILT_PROC, EV_ERROR, 0, data);

        let handled = handle(&[failed]);

        assert_eq!(io::ErrorKind::InvalidData, handled.wake.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_received_error_code_below_zero() {
        // Goal: an error code below zero, which no errno is, is an error of invalid data,
        // rather than an OS error with a negative code.
        // Method: handle an error event whose code is -1.
        let failed = Kevent::new(7, EVFILT_PROC, EV_ERROR, 0, -1);

        let handled = handle(&[failed]);

        assert_eq!(io::ErrorKind::InvalidData, handled.wake.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_received_process_event_whose_ident_names_no_pid() {
        // Goal: a process event whose ident is past what a pid's i32 holds is an error, rather
        // than pid -1.
        // Method: handle an exec event whose ident is the first past i32::MAX.
        let ident = usize::try_from(i32::MAX).unwrap() + 1;

        let handled = handle(&[process_event(ident, NOTE_EXEC)]);

        assert_eq!(io::ErrorKind::InvalidData, handled.wake.unwrap_err().kind());
    }

    #[test]
    fn refuses_an_exit_whose_pid_is_outside_the_known_table() {
        // Goal: an exit queued for a pid the known table cannot hold is an error, rather than
        // an exit skipped.
        // Method: queue an exit for PID_MAX + 1 and read the exits. Every exit the kernel
        // reports names a pid the tracker watched, so only a test queues one.
        let mut tracker = tracker(INTERVAL);
        tracker
            .exits
            .push((processes::PID_MAX + 1, Noticed::OnExit));

        let read = tracker.read_exits(&mut Meter::default());

        assert_eq!(io::ErrorKind::InvalidData, read.unwrap_err().kind());
    }

    #[test]
    fn reports_an_error_a_watch_receipt_carries_besides_refused_and_gone() {
        // Goal: a receipt that carries an error other than a refused watch or a gone process is
        // that error.
        // Method: register a change whose filter kqueue does not know, which draws EINVAL. Every
        // watch the tracker builds is one kqueue takes, so only a test registers one.
        let mut tracker = tracker(INTERVAL);
        tracker.changes.clear();
        tracker
            .changes
            .push(Kevent::new(1, -99, EV_ADD | EV_RECEIPT, 0, 0));

        let submitted = tracker.submit_watches(&mut Meter::default());

        assert_eq!(Some(EINVAL), submitted.unwrap_err().raw_os_error());
    }

    #[test]
    fn refuses_a_deadline_a_kevent_timer_cannot_take() {
        // Goal: a deadline past what a kevent timer's isize holds is an error, rather than a
        // timer armed at isize::MAX, which never fires.
        // Method: follow until the latest deadline a u64 holds.
        let mut tracker = tracker(INTERVAL);

        let followed = tracker.follow_until(u64::MAX, &mut Meter::default());

        assert_eq!(io::ErrorKind::InvalidInput, followed.unwrap_err().kind());
    }

    fn follow_for(tracker: &mut Tracker, meter: &mut Meter, millis: u64) {
        let timebase = Timebase::read().unwrap();
        tracker
            .follow_until(
                clock::now() + timebase.ticks(millis * 1_000_000).unwrap(),
                meter,
            )
            .unwrap();
    }

    /// The most steps a test follows the tracker for while it waits on an outcome. A count of
    /// steps rather than a time bounds the wait, so a slow machine only stretches it.
    const STEPS_MAX: u32 = 100;
    const STEP_MS: u64 = 100;

    /// Follows `tracker` a step at a time until it reports the `awaited` event, and returns the
    /// drained events about the child `awaited` names from this call alone, so one wait never
    /// ends on an event an earlier wait drained. Fails after `STEPS_MAX` steps, naming that
    /// event and the child's events.
    fn follow_to_event(tracker: &mut Tracker, meter: &mut Meter, awaited: Awaited) -> Vec<Event> {
        let mut events = Vec::new();
        for _ in 0..STEPS_MAX {
            follow_for(tracker, meter, STEP_MS);
            let about_child = tracker
                .drain_events()
                .filter(|event| pid_of_event(event) == awaited.pid());
            events.extend(about_child);
            if events.iter().any(|event| awaited.is(event)) {
                return events;
            }
        }
        panic!("no {awaited:?} after {STEPS_MAX} steps of {STEP_MS} ms: {events:?}");
    }

    /// An event a test waits for, about the child it names by its pid.
    #[derive(Debug, Clone, Copy)]
    enum Awaited {
        Start(i32),
        Exit(i32),
    }

    impl Awaited {
        fn pid(self) -> i32 {
            match self {
                Awaited::Start(pid) | Awaited::Exit(pid) => pid,
            }
        }

        fn is(self, event: &Event) -> bool {
            match (self, event) {
                (Awaited::Start(pid), Event::Start { pid: p, .. })
                | (Awaited::Exit(pid), Event::Exit { pid: p, .. }) => *p == pid,
                _ => false,
            }
        }
    }

    fn pid_of_event(event: &Event) -> i32 {
        match event {
            Event::Start { pid, .. } | Event::Exec { pid } | Event::Exit { pid, .. } => *pid,
        }
    }

    /// Follows `tracker` a step at a time, draining nothing, until its counts satisfy
    /// `is_reached`. Fails after `STEPS_MAX` steps, naming the count by its `description`.
    fn follow_to_count(
        tracker: &mut Tracker,
        description: &str,
        is_reached: impl Fn(&Counts) -> bool,
    ) {
        for _ in 0..STEPS_MAX {
            follow_for(tracker, &mut Meter::default(), STEP_MS);
            if is_reached(&tracker.counts) {
                return;
            }
        }
        panic!(
            "no {description} after {STEPS_MAX} steps of {STEP_MS} ms: {:?}",
            tracker.counts
        );
    }

    /// Spawns `program` with its stdin on a pipe this test holds, so a child that reads it
    /// waits there until `release`.
    fn spawn_held(program: &str, args: &[&str]) -> std::process::Child {
        std::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    }

    /// Lets a held child go on, by closing the pipe it reads.
    fn release(child: &mut std::process::Child) {
        let stdin = child.stdin.take().expect("a held child's stdin");
        drop(stdin);
    }

    #[test]
    fn reports_a_child_exiting_with_its_final_usage() {
        // Goal: a child the tracker follows is reported as an exit that carries the CPU time it
        // used, noticed as it exited.
        // Method: hold a child on its stdin until the tracker reports its start, which comes
        // after the tracker watches it, then let it exit and follow until its exit. The child is
        // reaped only after that, since its usage is read from the unreaped child.
        let mut tracker = tracker(INTERVAL);
        let mut child = spawn_held("/bin/cat", &[]);
        let pid = child.id().cast_signed();
        follow_to_event(&mut tracker, &mut Meter::default(), Awaited::Start(pid));

        release(&mut child);
        let released = follow_to_event(&mut tracker, &mut Meter::default(), Awaited::Exit(pid));
        child.wait().unwrap();

        let exited_with_usage = |event: &Event| match event {
            Event::Exit {
                pid: p,
                last: Ok(last),
                noticed: Noticed::OnExit,
            } => *p == pid && last.user_ns + last.system_ns > 0,
            _ => false,
        };
        assert!(
            released.iter().any(exited_with_usage),
            "child {pid}: {released:?}"
        );
    }

    #[test]
    fn reports_a_watched_process_replacing_its_program() {
        // Goal: a watched process that replaces its program is reported as an exec under its pid.
        // Method: hold a shell child on its stdin until the tracker reports its start, which
        // comes after the tracker watches it, then let it exec true and follow until it exits.
        // Bash runs the shell itself, where /bin/sh execs the shell it selects before it reads.
        let mut tracker = tracker(INTERVAL);
        let mut child = spawn_held("/bin/bash", &["-c", "read -r line; exec /usr/bin/true"]);
        let pid = child.id().cast_signed();
        let held = follow_to_event(&mut tracker, &mut Meter::default(), Awaited::Start(pid));

        release(&mut child);
        let released = follow_to_event(&mut tracker, &mut Meter::default(), Awaited::Exit(pid));
        child.wait().unwrap();

        let exec = Event::Exec { pid };
        assert!(!held.contains(&exec), "{held:?}");
        assert!(released.contains(&exec), "{released:?}");
    }

    #[test]
    fn reports_a_child_with_the_coalition_it_started_in() {
        // Goal: a child's start carries the coalition it started in, which is its parent's.
        // Method: hold a child of this test on its stdin, so it lives until the tracker reports
        // its start, and compare the coalition its start carries with this test process's own.
        let mut tracker = tracker(INTERVAL);
        let own = coalitions::coalition_of(std::process::id().cast_signed())
            .unwrap()
            .expect("this test process's coalition");
        let mut child = spawn_held("/bin/cat", &[]);
        let pid = child.id().cast_signed();

        let held = follow_to_event(&mut tracker, &mut Meter::default(), Awaited::Start(pid));
        release(&mut child);
        child.wait().unwrap();

        let start = Event::Start {
            pid,
            coalition: Some(own),
        };
        assert!(held.contains(&start), "{start:?}: {held:?}");
    }

    #[test]
    fn charges_each_step_of_following_a_child_to_its_own_source() {
        // Goal: scanning, reading a start, watching and reading an exit each charge their time to
        // their own source.
        // Method: follow a child held on its stdin until the tracker reports its start, then let
        // it exit and follow until its exit, and read each source's time from the cost of
        // following.
        let timebase = Timebase::read().unwrap();
        let mut tracker = tracker(INTERVAL);
        let mut meter = Meter::default();
        let start = Snapshot::take(&meter, timebase).unwrap();
        let mut child = spawn_held("/bin/cat", &[]);
        let pid = child.id().cast_signed();

        follow_to_event(&mut tracker, &mut meter, Awaited::Start(pid));
        release(&mut child);
        follow_to_event(&mut tracker, &mut meter, Awaited::Exit(pid));
        child.wait().unwrap();

        let cost = Snapshot::take(&meter, timebase)
            .unwrap()
            .cost_since(&start)
            .unwrap();
        let charged = [
            Source::Scans,
            Source::StartReads,
            Source::Watches,
            Source::ExitReads,
        ]
        .map(|s| cost.ms_per_second(s));
        assert!(charged.iter().all(|&ms| ms > 0.0), "{charged:?}");
    }

    #[test]
    fn returns_once_the_deadline_has_passed() {
        // Goal: following returns once its deadline has passed, and never before it.
        // Method: follow until 100 ms from now, and require the return to come at the deadline or
        // after it. How long after depends on how much macOS coalesces the timer at the
        // scheduling tier the test runs at, up to 78 ms at background, so no upper bound holds.
        let timebase = Timebase::read().unwrap();
        let mut tracker = tracker(INTERVAL);
        let deadline_ticks = clock::now() + timebase.ticks(100_000_000).unwrap();

        tracker
            .follow_until(deadline_ticks, &mut Meter::default())
            .unwrap();

        assert!(deadline_ticks <= clock::now());
    }

    #[test]
    fn reports_a_process_found_already_exited_with_its_final_usage() {
        // Goal: a listed process the tracker finds already exited is reported as an exit with its
        // final usage, noticed as already exited.
        // Method: observe a listing that holds a child which has exited and is not yet reaped.
        let mut tracker = tracker(INTERVAL);
        let mut child = processes::child_that_has_exited();
        let listing = current_listing();

        tracker.observe(&listing, &mut Meter::default()).unwrap();
        child.wait().unwrap();

        let pid = child.id().cast_signed();
        let events: Vec<Event> = tracker.drain_events().collect();
        let found_exited = |event: &Event| match event {
            Event::Exit {
                pid: p,
                last: Ok(_),
                noticed: Noticed::AlreadyExited,
            } => *p == pid,
            _ => false,
        };
        assert!(events.iter().any(found_exited), "child {pid}: {events:?}");
    }

    #[test]
    fn reports_nothing_more_for_an_exited_process_while_it_stays_listed() {
        // Goal: an exited process reported once is not reported again while listings still hold it.
        // Method: observe the same listing twice with an exited child not yet reaped in it, and
        // read the events of the second observation.
        let mut tracker = tracker(INTERVAL);
        let mut child = processes::child_that_has_exited();
        let listing = current_listing();
        tracker.observe(&listing, &mut Meter::default()).unwrap();
        tracker.drain_events();

        tracker.observe(&listing, &mut Meter::default()).unwrap();
        child.wait().unwrap();

        let pid = child.id().cast_signed();
        let events: Vec<Event> = tracker.drain_events().collect();
        let about_child = |event: &Event| match event {
            Event::Start { pid: p, .. } | Event::Exec { pid: p } | Event::Exit { pid: p, .. } => {
                *p == pid
            }
        };
        assert!(!events.iter().any(about_child), "child {pid}: {events:?}");
    }

    #[test]
    fn reports_a_live_process_at_a_pid_whose_process_exited_as_a_start() {
        // Goal: a pid a listing holds again after its process exited, and which names a live
        // process, is reported as a start, since macOS gave the pid to a new process.
        // Method: mark this test process's pid exited, as an exit the tracker read would, and
        // observe this Mac's listing, which holds it. No test can make macOS reuse a pid.
        let mut tracker = tracker(INTERVAL);
        let listing = current_listing();
        let pid = std::process::id().cast_signed();
        let index = known_index(pid, tracker.known.len()).unwrap();
        tracker.known[index].exited = true;

        tracker.observe(&listing, &mut Meter::default()).unwrap();

        let events: Vec<Event> = tracker.drain_events().collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Start { pid: p, .. } if *p == pid)),
            "{pid}: {events:?}"
        );
    }

    #[test]
    fn reports_a_pid_a_listing_holds_twice_as_one_start() {
        // Goal: a pid a listing holds twice names one process, reported with one start.
        // Method: observe this Mac's listing with an unlisted pid added twice, and count the
        // starts reported for that pid.
        let mut tracker = tracker(INTERVAL);
        let mut listing = current_listing();
        let pid = unlisted_pid(&listing);
        listing.extend([pid, pid]);

        tracker.observe(&listing, &mut Meter::default()).unwrap();

        let starts = tracker
            .drain_events()
            .filter(|e| matches!(e, Event::Start { pid: p, .. } if *p == pid))
            .count();
        assert_eq!(1, starts);
    }

    #[test]
    fn reports_nothing_more_for_an_exited_process_a_listing_holds_twice() {
        // Goal: an exited process a later listing holds twice is reported nothing more, and
        // trips no assertion.
        // Method: observe a listing with an exited child not yet reaped in it, then the same
        // listing with the child's pid added again, and read the events of the second.
        let mut tracker = tracker(INTERVAL);
        let mut child = processes::child_that_has_exited();
        let mut listing = current_listing();
        tracker.observe(&listing, &mut Meter::default()).unwrap();
        tracker.drain_events();
        let pid = child.id().cast_signed();
        listing.push(pid);

        tracker.observe(&listing, &mut Meter::default()).unwrap();
        child.wait().unwrap();

        let events: Vec<Event> = tracker.drain_events().collect();
        let about_child = |event: &Event| match event {
            Event::Start { pid: p, .. } | Event::Exec { pid: p } | Event::Exit { pid: p, .. } => {
                *p == pid
            }
        };
        assert!(!events.iter().any(about_child), "child {pid}: {events:?}");
    }

    #[test]
    fn reports_a_pid_first_seen_in_a_listing_as_a_start() {
        // Goal: a pid first seen in a listing, with no fork the tracker caught, is reported as a
        // start.
        // Method: observe this Mac's listing with an unlisted pid added to it.
        let mut tracker = tracker(INTERVAL);
        let mut listing = current_listing();
        let pid = unlisted_pid(&listing);
        listing.push(pid);

        tracker.observe(&listing, &mut Meter::default()).unwrap();

        let events: Vec<Event> = tracker.drain_events().collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Start { pid: p, .. } if *p == pid)),
            "{pid}: {events:?}"
        );
    }

    #[test]
    fn lists_the_processes_its_first_scan_found() {
        // Goal: a new tracker's last listing holds the processes its first scan found.
        // Method: start a child, then a tracker, and look for the child in the tracker's listing.
        let mut child = spawn_sleeper();

        let tracker = tracker(INTERVAL);
        child.wait().unwrap();

        assert!(tracker.last_listing().contains(&child.id().cast_signed()));
    }

    #[test]
    fn rescans_an_interval_after_forks_that_came_too_soon() {
        // Goal: forks within an interval of the last rescan are rescanned for once the interval
        // passes, as a trailing rescan.
        // Method: follow with a 1 s interval while a thread forks every 2 ms, until a trailing
        // rescan is counted. The test keeps one window: it fails with no defect only if every
        // wake for its whole bound comes a second or more after the rescan before it.
        let mut tracker = tracker(Duration::from_secs(1));

        while_forking_every_2_ms(|| {
            follow_to_count(&mut tracker, "trailing rescan", |counts| {
                counts.trailing_rescans > 0
            });
        });
    }

    #[test]
    fn waits_an_interval_after_a_listing_before_rescanning_on_a_fork() {
        // Goal: a listing the tracker observes counts as a rescan, so a fork within an interval of
        // it waits for the interval rather than rescanning at once.
        // Method: with the last rescan put long past and a 10 s interval, observe a listing, start
        // a child, and follow for 100 ms. Without the listing the child's fork would bring a
        // rescan at once, and with it only a 10 s stall before the fork's wake could.
        let mut tracker = tracker(Duration::from_secs(10));
        tracker.throttle.rescan_ticks_last = 0;
        let listing = current_listing();
        tracker.observe(&listing, &mut Meter::default()).unwrap();
        let before = tracker.counts;

        let mut child = spawn_sleeper();
        follow_for(&mut tracker, &mut Meter::default(), 100);
        child.wait().unwrap();

        let since = tracker.counts.since(&before);
        assert!(since.forks > 0, "{since:?}");
        assert_eq!(0, since.leading_rescans, "{since:?}");
        assert_eq!(1, tracker.counts.listing_rescans);
    }

    #[test]
    fn rescans_at_once_on_a_fork_an_interval_after_the_last_rescan() {
        // Goal: a fork an interval or more after the last rescan is rescanned for at once, as a
        // leading rescan.
        // Method: 250 ms after the tracker's first scan, start a child and follow for 100 ms with
        // a 200 ms interval.
        let mut tracker = tracker(Duration::from_millis(200));
        std::thread::sleep(Duration::from_millis(250));

        let mut child = spawn_sleeper();
        follow_for(&mut tracker, &mut Meter::default(), 100);
        child.wait().unwrap();

        assert!(tracker.counts.leading_rescans >= 1, "{:?}", tracker.counts);
    }

    #[test]
    fn rescans_at_most_once_an_interval_however_often_processes_fork() {
        // Goal: forks however frequent cause at most one rescan an interval.
        // Method: follow for 300 ms with a 50 ms interval while a thread forks every 2 ms, and hold
        // the rescans from forks to one more than the intervals the follow took, however long
        // that was. Each rescan from a fork begins an interval or more after the one before.
        let timebase = Timebase::read().unwrap();
        let mut tracker = tracker(INTERVAL);
        let interval_ns = u64::try_from(INTERVAL.as_nanos()).unwrap();
        let interval_ticks = timebase.ticks(interval_ns).unwrap();
        let start_ticks = clock::now();

        while_forking_every_2_ms(|| follow_for(&mut tracker, &mut Meter::default(), 300));

        // The division floors.
        let cap = (clock::now() - start_ticks) / interval_ticks + 1;
        assert!(
            rescans_from_forks(&tracker.counts) <= cap,
            "{:?} over {cap}",
            tracker.counts
        );
    }

    #[test]
    fn returns_no_sooner_than_the_deadline_while_rescans_wait_on_their_timer() {
        // Goal: following returns no sooner than its deadline while forks keep the rescan timer
        // armed.
        // Method: follow for 100 ms with a 5 ms interval while a thread forks every 2 ms.
        let timebase = Timebase::read().unwrap();
        let mut tracker = tracker(Duration::from_millis(5));
        let deadline_ticks = clock::now() + timebase.ticks(100_000_000).unwrap();

        while_forking_every_2_ms(|| tracker.follow_until(deadline_ticks, &mut Meter::default()))
            .unwrap();

        assert!(
            clock::now() >= deadline_ticks,
            "returned before the deadline"
        );
    }

    mod throttle {
        use super::super::{Cause, Step, Throttle, Wake};

        const INTERVAL_TICKS: u64 = 50;
        const RESCAN_TICKS_LAST: u64 = 1_000;
        const FORK: Wake = Wake {
            due: false,
            rescan_timer_fired: false,
            forked: true,
        };
        const TIMER: Wake = Wake {
            due: false,
            rescan_timer_fired: true,
            forked: false,
        };

        fn throttle() -> Throttle {
            let mut throttle = Throttle::new(INTERVAL_TICKS);
            throttle.rescanned(RESCAN_TICKS_LAST);
            throttle
        }

        #[test]
        fn rescans_on_a_fork_an_interval_after_the_last_rescan() {
            // Goal: a fork an interval after the last rescan is rescanned for at once, as leading.
            // Method: step an interval after the last rescan with a fork pending.
            let mut throttle = throttle();

            throttle.woke(FORK);

            assert_eq!(
                Step::Rescan(Cause::Leading),
                throttle.step(RESCAN_TICKS_LAST + INTERVAL_TICKS)
            );
        }

        #[test]
        fn arms_the_timer_for_an_interval_after_the_last_rescan_on_an_early_fork() {
            // Goal: a fork within an interval of the last rescan arms the timer for an interval
            // after that rescan.
            // Method: step just after the last rescan with a fork pending.
            let mut throttle = throttle();

            throttle.woke(FORK);

            assert_eq!(
                Step::ArmAt(RESCAN_TICKS_LAST + INTERVAL_TICKS),
                throttle.step(RESCAN_TICKS_LAST + 1)
            );
        }

        #[test]
        fn waits_on_an_early_fork_while_the_timer_is_armed() {
            // Goal: a second early fork waits on the timer the first armed and arms no other.
            // Method: step after one early fork, which arms the timer, and again after a second.
            let mut throttle = throttle();
            throttle.woke(FORK);
            throttle.step(RESCAN_TICKS_LAST + 1);

            throttle.woke(FORK);

            assert_eq!(Step::Wait, throttle.step(RESCAN_TICKS_LAST + 2));
        }

        #[test]
        fn rescans_when_the_timer_fires_with_a_fork_pending() {
            // Goal: the timer firing with an early fork still pending rescans, as trailing.
            // Method: arm the timer on an early fork, fire it, and step an interval after the last
            // rescan.
            let mut throttle = throttle();
            throttle.woke(FORK);
            throttle.step(RESCAN_TICKS_LAST + 1);

            throttle.woke(TIMER);

            assert_eq!(
                Step::Rescan(Cause::Trailing),
                throttle.step(RESCAN_TICKS_LAST + INTERVAL_TICKS)
            );
        }

        #[test]
        fn waits_when_the_timer_fires_with_nothing_pending() {
            // Goal: the timer firing after a rescan has covered the early fork rescans nothing.
            // Method: arm the timer on an early fork, rescan, fire the timer, and step at its
            // deadline.
            let mut throttle = throttle();
            throttle.woke(FORK);
            throttle.step(RESCAN_TICKS_LAST + 1);
            throttle.rescanned(RESCAN_TICKS_LAST + 2);

            throttle.woke(TIMER);

            assert_eq!(
                Step::Wait,
                throttle.step(RESCAN_TICKS_LAST + INTERVAL_TICKS)
            );
        }

        #[test]
        fn rearms_when_the_timer_fires_early_with_a_fork_pending() {
            // Goal: the timer firing less than an interval after the last rescan, with a fork
            // pending, arms it again for an interval after that rescan.
            // Method: arm the timer on an early fork, rescan just after, fork, fire the timer, and
            // step at the deadline it was armed for.
            let mut throttle = throttle();
            throttle.woke(FORK);
            throttle.step(RESCAN_TICKS_LAST + 1);
            throttle.rescanned(RESCAN_TICKS_LAST + 2);
            throttle.woke(FORK);

            throttle.woke(TIMER);

            assert_eq!(
                Step::ArmAt(RESCAN_TICKS_LAST + 2 + INTERVAL_TICKS),
                throttle.step(RESCAN_TICKS_LAST + INTERVAL_TICKS)
            );
        }

        #[test]
        fn rescans_as_trailing_when_an_early_fork_comes_due_before_its_timer_fires() {
            // Goal: an early fork that comes due before its timer fires is rescanned for as
            // trailing.
            // Method: arm the timer on an early fork, and step at its deadline without firing it.
            let mut throttle = throttle();
            throttle.woke(FORK);
            throttle.step(RESCAN_TICKS_LAST + 1);

            assert_eq!(
                Step::Rescan(Cause::Trailing),
                throttle.step(RESCAN_TICKS_LAST + INTERVAL_TICKS)
            );
        }

        #[test]
        fn arms_the_timer_for_the_last_tick_when_the_interval_runs_past_it() {
            // Goal: an interval that runs past the last tick a u64 holds arms the timer for that
            // tick rather than wrapping.
            // Method: step just after the last rescan with the longest interval and a fork pending.
            let mut throttle = Throttle::new(u64::MAX);
            throttle.rescanned(RESCAN_TICKS_LAST);

            throttle.woke(FORK);

            assert_eq!(Step::ArmAt(u64::MAX), throttle.step(RESCAN_TICKS_LAST + 1));
        }

        #[test]
        fn rescans_on_a_fork_at_the_instant_of_the_last_rescan_with_no_interval() {
            // Goal: with no interval, a fork is rescanned for at once, even at the instant of the
            // last rescan.
            // Method: step at the instant of the last rescan with an interval of 0 and a fork
            // pending.
            let mut throttle = Throttle::new(0);
            throttle.rescanned(RESCAN_TICKS_LAST);

            throttle.woke(FORK);

            assert_eq!(
                Step::Rescan(Cause::Leading),
                throttle.step(RESCAN_TICKS_LAST)
            );
        }

        #[test]
        fn waits_with_nothing_pending() {
            // Goal: a step with no fork pending rescans nothing and arms no timer.
            // Method: step an interval after the last rescan with no fork.
            assert_eq!(
                Step::Wait,
                throttle().step(RESCAN_TICKS_LAST + INTERVAL_TICKS)
            );
        }
    }

    #[test]
    fn reports_nothing_for_pids_the_previous_listing_held() {
        // Goal: a pid the previous listing held is not reported again in the next.
        // Method: observe the same listing twice and count the events of the second observation.
        let mut tracker = tracker(INTERVAL);
        let listing = current_listing();
        tracker.observe(&listing, &mut Meter::default()).unwrap();
        tracker.drain_events();

        tracker.observe(&listing, &mut Meter::default()).unwrap();

        assert_eq!(0, tracker.drain_events().count());
    }
}
