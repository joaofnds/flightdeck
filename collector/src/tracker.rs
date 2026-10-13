use std::ffi::c_int;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::time::Duration;

use crate::clock::{self, Timebase};
use crate::coalitions;
use crate::cost::{Meter, Source};
use crate::processes::{self, Unreadable, Usage};

#[derive(Debug)]
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

pub struct Tracker {
    queue: OwnedFd,
    timebase: Timebase,
    listing: Vec<i32>,
    known: Vec<Known>,
    lingering: Vec<(i32, usize)>,
    scans: u64,
    throttle: Throttle,
    arming: Vec<libc::kevent>,
    changes: Vec<libc::kevent>,
    received: Vec<libc::kevent>,
    starting: Vec<i32>,
    exiting: Vec<(i32, Noticed)>,
    events: Vec<Event>,
    pub counts: Counts,
}

#[derive(Clone, Copy, Default)]
struct Known {
    listed_in: u64,
    exited: bool,
}

#[derive(Default)]
struct Wake {
    due: bool,
    rescan_timer_fired: bool,
    forked: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Counts {
    pub forks: u64,
    pub refused_watches: u64,
    pub leading_rescans: u64,
    pub trailing_rescans: u64,
    pub listing_rescans: u64,
}

impl Counts {
    #[must_use]
    pub fn since(self, earlier: Counts) -> Counts {
        Counts {
            forks: self.forks - earlier.forks,
            refused_watches: self.refused_watches - earlier.refused_watches,
            leading_rescans: self.leading_rescans - earlier.leading_rescans,
            trailing_rescans: self.trailing_rescans - earlier.trailing_rescans,
            listing_rescans: self.listing_rescans - earlier.listing_rescans,
        }
    }
}

struct Throttle {
    interval: u64,
    last_rescan: u64,
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
    fn new(interval: u64) -> Self {
        Self {
            interval,
            last_rescan: 0,
            pending: false,
            deferred: false,
            armed: false,
        }
    }

    fn forked(&mut self) {
        self.pending = true;
    }

    fn fired(&mut self) {
        self.armed = false;
    }

    fn rescanned(&mut self, now: u64) {
        self.last_rescan = now;
        self.pending = false;
        self.deferred = false;
    }

    fn step(&mut self, now: u64) -> Step {
        if !self.pending {
            return Step::Wait;
        }

        if now.saturating_sub(self.last_rescan) >= self.interval {
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
            Step::ArmAt(self.last_rescan.saturating_add(self.interval))
        }
    }
}

const DEADLINE_TIMER: usize = 1;
const RESCAN_TIMER: usize = 2;
const RECEIVED_PER_WAIT: usize = 64;

impl Tracker {
    pub fn new(timebase: Timebase, rescan_interval: Duration) -> io::Result<Self> {
        let queue = unsafe { libc::kqueue() };
        if queue < 0 {
            return Err(io::Error::last_os_error());
        }

        let mut tracker = Self {
            queue: unsafe { OwnedFd::from_raw_fd(queue) },
            timebase,
            listing: Vec::new(),
            known: Vec::new(),
            lingering: Vec::new(),
            scans: 0,
            throttle: Throttle::new(
                timebase.ticks(u64::try_from(rescan_interval.as_nanos()).unwrap_or(u64::MAX)),
            ),
            arming: Vec::new(),
            changes: Vec::new(),
            received: Vec::new(),
            starting: Vec::new(),
            exiting: Vec::new(),
            events: Vec::new(),
            counts: Counts::default(),
        };
        tracker.rescan(&mut Meter::default())?;

        Ok(tracker)
    }

    pub fn follow_until(&mut self, deadline: u64, meter: &mut Meter) -> io::Result<()> {
        self.arming.push(timer_at(DEADLINE_TIMER, deadline));

        loop {
            let woke = meter.measure(Source::Events, || self.wait_and_handle())?;

            self.read_exits(meter);
            if woke.rescan_timer_fired {
                self.throttle.fired();
            }
            if woke.forked {
                self.throttle.forked();
            }

            match self.throttle.step(clock::now()) {
                Step::Rescan(cause) => {
                    self.rescan(meter)?;
                    match cause {
                        Cause::Leading => self.counts.leading_rescans += 1,
                        Cause::Trailing => self.counts.trailing_rescans += 1,
                    }
                }
                Step::ArmAt(at) => self.arming.push(timer_at(RESCAN_TIMER, at)),
                Step::Wait => {}
            }

            if woke.due {
                return Ok(());
            }
        }
    }

    pub fn observe(&mut self, listing: &[i32], meter: &mut Meter) -> io::Result<()> {
        meter.measure(Source::Scans, || {
            self.compare(listing);
            self.start_reused_pids();
        });
        self.throttle.rescanned(clock::now());
        self.counts.listing_rescans += 1;
        self.read_starts(meter);
        self.submit_watches(meter)
    }

    pub fn drain_events(&mut self) -> std::vec::Drain<'_, Event> {
        self.events.drain(..)
    }

    #[must_use]
    pub fn last_listing(&self) -> &[i32] {
        &self.listing
    }

    fn rescan(&mut self, meter: &mut Meter) -> io::Result<()> {
        let mut listing = std::mem::take(&mut self.listing);
        let scanned = meter.measure(Source::Scans, || {
            processes::list_pids(&mut listing)?;
            self.compare(&listing);
            io::Result::Ok(())
        });
        self.listing = listing;
        scanned?;
        self.throttle.rescanned(clock::now());

        self.read_starts(meter);
        self.submit_watches(meter)
    }

    fn compare(&mut self, listing: &[i32]) {
        let previous = self.scans;
        self.scans += 1;
        let reporting = previous > 0;
        self.changes.clear();
        self.lingering.clear();

        for &pid in listing {
            let Ok(index) = usize::try_from(pid) else {
                continue;
            };

            if index >= self.known.len() {
                self.known.resize(index + 1, Known::default());
            }

            let known = &mut self.known[index];
            let listed_before = reporting && known.listed_in == previous;

            known.listed_in = self.scans;
            if !listed_before {
                known.exited = false;
                self.changes.push(watch(pid));
                if reporting {
                    self.starting.push(pid);
                }
            } else if known.exited {
                self.lingering.push((pid, index));
            }
        }
    }

    fn start_reused_pids(&mut self) {
        for &(pid, index) in &self.lingering {
            if processes::is_alive(pid) {
                self.known[index].exited = false;
                self.changes.push(watch(pid));
                self.starting.push(pid);
            }
        }
    }

    fn wait_and_handle(&mut self) -> io::Result<Wake> {
        let count = self.wait()?;
        let mut woke = Wake::default();
        for index in 0..count {
            let event = self.received[index];
            if event.flags & libc::EV_ERROR != 0 {
                return Err(io::Error::from_raw_os_error(
                    i32::try_from(event.data).unwrap_or(libc::EINVAL),
                ));
            }

            if event.filter == libc::EVFILT_TIMER && event.ident == DEADLINE_TIMER {
                woke.due = true;
            } else if event.filter == libc::EVFILT_TIMER {
                woke.rescan_timer_fired = true;
            } else {
                woke.forked |= self.handle(event);
            }
        }

        Ok(woke)
    }

    fn handle(&mut self, event: libc::kevent) -> bool {
        let pid = i32::try_from(event.ident).unwrap_or(-1);
        if event.fflags & libc::NOTE_EXEC != 0 {
            self.events.push(Event::Exec { pid });
        }
        if event.fflags & libc::NOTE_EXIT != 0 {
            self.exiting.push((pid, Noticed::OnExit));
        }

        let forked = event.fflags & libc::NOTE_FORK != 0;
        if forked {
            self.counts.forks += 1;
        }

        forked
    }

    fn read_starts(&mut self, meter: &mut Meter) {
        if self.starting.is_empty() {
            return;
        }

        meter.measure(Source::StartReads, || {
            for pid in self.starting.drain(..) {
                self.events.push(Event::Start {
                    pid,
                    coalition: coalitions::coalition_of(pid),
                });
            }
        });
    }

    fn read_exits(&mut self, meter: &mut Meter) {
        if self.exiting.is_empty() {
            return;
        }

        meter.measure(Source::ExitReads, || {
            for (pid, noticed) in self.exiting.drain(..) {
                if let Some(known) = usize::try_from(pid)
                    .ok()
                    .and_then(|i| self.known.get_mut(i))
                {
                    known.exited = true;
                }
                self.events.push(Event::Exit {
                    pid,
                    last: processes::read_usage(pid, self.timebase),
                    noticed,
                });
            }
        });
    }

    fn submit_watches(&mut self, meter: &mut Meter) -> io::Result<()> {
        if self.changes.is_empty() {
            return Ok(());
        }

        meter.measure(Source::Watches, || self.register_watches())?;
        self.read_exits(meter);

        Ok(())
    }

    fn register_watches(&mut self) -> io::Result<()> {
        let count = c_int::try_from(self.changes.len()).unwrap_or(c_int::MAX);
        self.received
            .resize(self.changes.len(), kevent(0, 0, 0, 0, 0));
        let none_pending = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let receipts = unsafe {
            libc::kevent(
                self.queue.as_raw_fd(),
                self.changes.as_ptr(),
                count,
                self.received.as_mut_ptr(),
                count,
                &raw const none_pending,
            )
        };

        let Ok(receipts) = usize::try_from(receipts) else {
            return Err(io::Error::last_os_error());
        };

        for index in 0..receipts {
            let receipt = self.received[index];
            let pid = i32::try_from(receipt.ident).unwrap_or(-1);
            match i32::try_from(receipt.data).unwrap_or(libc::EINVAL) {
                0 => {}
                libc::EPERM => self.counts.refused_watches += 1,
                libc::ESRCH => self.exiting.push((pid, Noticed::AlreadyExited)),
                error => return Err(io::Error::from_raw_os_error(error)),
            }
        }

        Ok(())
    }

    fn wait(&mut self) -> io::Result<usize> {
        self.received
            .resize(RECEIVED_PER_WAIT, kevent(0, 0, 0, 0, 0));
        loop {
            let count = unsafe {
                libc::kevent(
                    self.queue.as_raw_fd(),
                    self.arming.as_ptr(),
                    c_int::try_from(self.arming.len()).unwrap_or(0),
                    self.received.as_mut_ptr(),
                    c_int::try_from(RECEIVED_PER_WAIT).unwrap_or(1),
                    std::ptr::null(),
                )
            };
            if let Ok(count) = usize::try_from(count) {
                self.arming.clear();
                return Ok(count);
            }

            let error = io::Error::last_os_error();
            if error.kind() != io::ErrorKind::Interrupted {
                return Err(error);
            }
        }
    }
}

fn watch(pid: i32) -> libc::kevent {
    kevent(
        usize::try_from(pid).unwrap_or(0),
        libc::EVFILT_PROC,
        libc::EV_ADD | libc::EV_RECEIPT,
        libc::NOTE_FORK | libc::NOTE_EXEC | libc::NOTE_EXIT,
        0,
    )
}

fn timer_at(ident: usize, deadline: u64) -> libc::kevent {
    kevent(
        ident,
        libc::EVFILT_TIMER,
        libc::EV_ADD | libc::EV_ONESHOT,
        libc::NOTE_ABSOLUTE | libc::NOTE_MACHTIME | libc::NOTE_MACH_CONTINUOUS_TIME,
        isize::try_from(deadline).unwrap_or(isize::MAX),
    )
}

fn kevent(ident: usize, filter: i16, flags: u16, filter_flags: u32, data: isize) -> libc::kevent {
    libc::kevent {
        ident,
        filter,
        flags,
        fflags: filter_flags,
        data,
        udata: std::ptr::null_mut(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock;
    use crate::cost::Snapshot;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    const INTERVAL: Duration = Duration::from_millis(50);

    fn rescans_from_forks(counts: Counts) -> u64 {
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
            let result = follow();
            stop.store(true, Ordering::Relaxed);
            result
        })
    }

    fn follow_for(tracker: &mut Tracker, meter: &mut Meter, millis: u64) {
        let timebase = Timebase::read();
        tracker
            .follow_until(clock::now() + timebase.ticks(millis * 1_000_000), meter)
            .unwrap();
    }

    #[test]
    fn reports_a_child_starting_and_exiting_with_its_final_usage() {
        let mut tracker = Tracker::new(Timebase::read(), INTERVAL).unwrap();

        let mut child = spawn_sleeper();
        follow_for(&mut tracker, &mut Meter::default(), 500);
        child.wait().unwrap();

        let pid = child.id().cast_signed();
        let events: Vec<Event> = tracker.drain_events().collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Start { pid: p, .. } if *p == pid)),
            "child {pid}: {events:?}"
        );
        let exited_with_usage = |event: &Event| match event {
            Event::Exit {
                pid: p,
                last: Ok(last),
                noticed: Noticed::OnExit,
            } => *p == pid && last.user_ns + last.system_ns > 0,
            _ => false,
        };
        assert!(
            events.iter().any(exited_with_usage),
            "child {pid}: {events:?}"
        );
    }

    #[test]
    fn reports_a_watched_process_replacing_its_program() {
        let mut tracker = Tracker::new(Timebase::read(), INTERVAL).unwrap();

        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "sleep 0.1; exec /usr/bin/true"])
            .spawn()
            .unwrap();
        follow_for(&mut tracker, &mut Meter::default(), 500);
        child.wait().unwrap();

        let pid = child.id().cast_signed();
        let events: Vec<Event> = tracker.drain_events().collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Exec { pid: p } if *p == pid)),
            "child {pid}: {events:?}"
        );
    }

    #[test]
    fn reports_a_child_with_the_coalition_it_started_in() {
        let mut tracker = Tracker::new(Timebase::read(), INTERVAL).unwrap();

        let mut child = spawn_sleeper();
        follow_for(&mut tracker, &mut Meter::default(), 500);
        child.wait().unwrap();

        let pid = child.id().cast_signed();
        let own = coalitions::coalition_of(std::process::id().cast_signed());
        let events: Vec<Event> = tracker.drain_events().collect();
        let started_in_own = |event: &Event| match event {
            Event::Start { pid: p, coalition } => *p == pid && *coalition == own,
            _ => false,
        };
        assert!(
            events.iter().any(started_in_own),
            "child {pid} in {own:?}: {events:?}"
        );
    }

    #[test]
    fn charges_each_step_of_following_a_child_to_its_own_source() {
        let timebase = Timebase::read();
        let mut tracker = Tracker::new(timebase, INTERVAL).unwrap();
        let mut meter = Meter::default();

        let mut child = spawn_sleeper();
        follow_for(&mut tracker, &mut meter, 500);
        child.wait().unwrap();

        let spent = Snapshot::take(&meter, timebase).unwrap().spent_ns;
        let charged = [
            Source::Scans,
            Source::StartReads,
            Source::Watches,
            Source::ExitReads,
        ]
        .map(|s| spent[s.index()]);
        assert!(charged.iter().all(|&ns| ns > 0), "{charged:?}");
    }

    #[test]
    fn returns_once_the_deadline_has_passed() {
        let timebase = Timebase::read();
        let mut tracker = Tracker::new(timebase, INTERVAL).unwrap();
        let deadline = clock::now() + timebase.ticks(100_000_000);

        tracker
            .follow_until(deadline, &mut Meter::default())
            .unwrap();

        let late_ns = timebase.nanos(
            clock::now()
                .checked_sub(deadline)
                .expect("returned before the deadline"),
        );
        assert!(
            late_ns < 50_000_000,
            "returned {late_ns} ns after the deadline"
        );
    }

    #[test]
    fn reports_a_process_found_already_exited_with_its_final_usage() {
        let mut tracker = Tracker::new(Timebase::read(), INTERVAL).unwrap();
        let mut child = processes::child_that_has_exited();
        let mut listing = Vec::new();
        processes::list_pids(&mut listing).unwrap();

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
        let mut tracker = Tracker::new(Timebase::read(), INTERVAL).unwrap();
        let mut child = processes::child_that_has_exited();
        let mut listing = Vec::new();
        processes::list_pids(&mut listing).unwrap();
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
    fn reports_a_pid_first_seen_in_a_listing_as_a_start() {
        let mut tracker = Tracker::new(Timebase::read(), INTERVAL).unwrap();
        let mut listing = Vec::new();
        processes::list_pids(&mut listing).unwrap();
        listing.push(99_999);

        tracker.observe(&listing, &mut Meter::default()).unwrap();

        let events: Vec<Event> = tracker.drain_events().collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Start { pid: 99_999, .. })),
            "{events:?}"
        );
    }

    #[test]
    fn lists_the_processes_its_first_scan_found() {
        let mut child = spawn_sleeper();

        let tracker = Tracker::new(Timebase::read(), INTERVAL).unwrap();
        child.wait().unwrap();

        assert!(tracker.last_listing().contains(&child.id().cast_signed()));
    }

    #[test]
    fn rescans_an_interval_after_forks_that_came_too_soon() {
        let mut tracker = Tracker::new(Timebase::read(), Duration::from_millis(300)).unwrap();

        let mut first = spawn_sleeper();
        std::thread::sleep(Duration::from_millis(1));
        let mut second = spawn_sleeper();
        follow_for(&mut tracker, &mut Meter::default(), 600);
        first.wait().unwrap();
        second.wait().unwrap();

        assert!(tracker.counts.trailing_rescans >= 1, "{:?}", tracker.counts);
    }

    #[test]
    fn waits_an_interval_after_a_listing_before_rescanning_on_a_fork() {
        let mut tracker = Tracker::new(Timebase::read(), Duration::from_millis(200)).unwrap();
        std::thread::sleep(Duration::from_millis(250));
        let mut listing = Vec::new();
        processes::list_pids(&mut listing).unwrap();
        tracker.observe(&listing, &mut Meter::default()).unwrap();
        let before = tracker.counts;

        let mut child = spawn_sleeper();
        follow_for(&mut tracker, &mut Meter::default(), 100);
        child.wait().unwrap();

        let since = tracker.counts.since(before);
        assert_eq!(0, since.leading_rescans, "{since:?}");
        assert_eq!(1, tracker.counts.listing_rescans);
    }

    #[test]
    fn rescans_at_once_on_a_fork_after_a_quiet_interval() {
        let mut tracker = Tracker::new(Timebase::read(), Duration::from_millis(200)).unwrap();
        std::thread::sleep(Duration::from_millis(250));

        let mut child = spawn_sleeper();
        follow_for(&mut tracker, &mut Meter::default(), 100);
        child.wait().unwrap();

        assert!(tracker.counts.leading_rescans >= 1, "{:?}", tracker.counts);
    }

    #[test]
    fn rescans_at_most_once_an_interval_however_often_processes_fork() {
        let mut tracker = Tracker::new(Timebase::read(), INTERVAL).unwrap();
        let follow_ms = 300;

        while_forking_every_2_ms(|| follow_for(&mut tracker, &mut Meter::default(), follow_ms));

        let cap = follow_ms / u64::try_from(INTERVAL.as_millis()).unwrap() + 2;
        assert!(
            rescans_from_forks(tracker.counts) <= cap,
            "{:?} over {cap}",
            tracker.counts
        );
    }

    #[test]
    fn returns_no_sooner_than_the_deadline_while_rescans_wait_on_their_timer() {
        let timebase = Timebase::read();
        let mut tracker = Tracker::new(timebase, Duration::from_millis(5)).unwrap();
        let deadline = clock::now() + timebase.ticks(100_000_000);

        while_forking_every_2_ms(|| tracker.follow_until(deadline, &mut Meter::default())).unwrap();

        assert!(clock::now() >= deadline, "returned before the deadline");
    }

    mod throttle {
        use super::super::{Cause, Step, Throttle};

        const INTERVAL: u64 = 50;
        const LAST_RESCAN: u64 = 1_000;

        fn throttle() -> Throttle {
            let mut throttle = Throttle::new(INTERVAL);
            throttle.rescanned(LAST_RESCAN);
            throttle
        }

        #[test]
        fn rescans_on_a_fork_an_interval_after_the_last_rescan() {
            let mut throttle = throttle();

            throttle.forked();

            assert_eq!(
                Step::Rescan(Cause::Leading),
                throttle.step(LAST_RESCAN + INTERVAL)
            );
        }

        #[test]
        fn arms_the_timer_for_an_interval_after_the_last_rescan_on_an_early_fork() {
            let mut throttle = throttle();

            throttle.forked();

            assert_eq!(
                Step::ArmAt(LAST_RESCAN + INTERVAL),
                throttle.step(LAST_RESCAN + 1)
            );
        }

        #[test]
        fn waits_on_an_early_fork_while_the_timer_is_armed() {
            let mut throttle = throttle();
            throttle.forked();
            throttle.step(LAST_RESCAN + 1);

            throttle.forked();

            assert_eq!(Step::Wait, throttle.step(LAST_RESCAN + 2));
        }

        #[test]
        fn rescans_when_the_timer_fires_with_a_fork_pending() {
            let mut throttle = throttle();
            throttle.forked();
            throttle.step(LAST_RESCAN + 1);

            throttle.fired();

            assert_eq!(
                Step::Rescan(Cause::Trailing),
                throttle.step(LAST_RESCAN + INTERVAL)
            );
        }

        #[test]
        fn waits_when_the_timer_fires_with_nothing_pending() {
            let mut throttle = throttle();
            throttle.forked();
            throttle.step(LAST_RESCAN + 1);
            throttle.rescanned(LAST_RESCAN + 2);

            throttle.fired();

            assert_eq!(Step::Wait, throttle.step(LAST_RESCAN + INTERVAL));
        }

        #[test]
        fn rearms_when_the_timer_fires_early_with_a_fork_pending() {
            let mut throttle = throttle();
            throttle.forked();
            throttle.step(LAST_RESCAN + 1);
            throttle.rescanned(LAST_RESCAN + 2);
            throttle.forked();

            throttle.fired();

            assert_eq!(
                Step::ArmAt(LAST_RESCAN + 2 + INTERVAL),
                throttle.step(LAST_RESCAN + INTERVAL)
            );
        }

        #[test]
        fn rescans_as_trailing_when_an_early_fork_comes_due_before_its_timer_fires() {
            let mut throttle = throttle();
            throttle.forked();
            throttle.step(LAST_RESCAN + 1);

            assert_eq!(
                Step::Rescan(Cause::Trailing),
                throttle.step(LAST_RESCAN + INTERVAL)
            );
        }

        #[test]
        fn arms_the_timer_for_the_last_tick_when_the_interval_runs_past_it() {
            let mut throttle = Throttle::new(u64::MAX);
            throttle.rescanned(LAST_RESCAN);

            throttle.forked();

            assert_eq!(Step::ArmAt(u64::MAX), throttle.step(LAST_RESCAN + 1));
        }

        #[test]
        fn rescans_on_a_fork_at_the_instant_of_the_last_rescan_with_no_interval() {
            let mut throttle = Throttle::new(0);
            throttle.rescanned(LAST_RESCAN);

            throttle.forked();

            assert_eq!(Step::Rescan(Cause::Leading), throttle.step(LAST_RESCAN));
        }

        #[test]
        fn waits_with_nothing_pending() {
            assert_eq!(Step::Wait, throttle().step(LAST_RESCAN + INTERVAL));
        }
    }

    #[test]
    fn reports_nothing_for_pids_the_previous_listing_held() {
        let mut tracker = Tracker::new(Timebase::read(), INTERVAL).unwrap();
        let mut listing = Vec::new();
        processes::list_pids(&mut listing).unwrap();
        tracker.observe(&listing, &mut Meter::default()).unwrap();
        tracker.drain_events();

        tracker.observe(&listing, &mut Meter::default()).unwrap();

        assert_eq!(0, tracker.drain_events().count());
    }
}
