use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

use crate::clock::Timebase;
use crate::processes::{self, Unreadable, Usage};

#[derive(Debug)]
pub enum Event {
    Start {
        pid: i32,
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
    last_seen: Vec<u64>,
    scans: u64,
    changes: Vec<libc::kevent>,
    received: Vec<libc::kevent>,
    events: Vec<Event>,
    pub counts: Counts,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Counts {
    pub forks: u64,
    pub refused_watches: u64,
}

impl Counts {
    #[must_use]
    pub fn since(self, earlier: Counts) -> Counts {
        Counts {
            forks: self.forks - earlier.forks,
            refused_watches: self.refused_watches - earlier.refused_watches,
        }
    }
}

const TIMER: usize = 1;
const RECEIVED_PER_WAIT: usize = 64;

impl Tracker {
    pub fn new(timebase: Timebase) -> io::Result<Self> {
        let queue = unsafe { libc::kqueue() };
        if queue < 0 {
            return Err(io::Error::last_os_error());
        }

        let mut tracker = Self {
            queue: unsafe { OwnedFd::from_raw_fd(queue) },
            timebase,
            listing: Vec::new(),
            last_seen: Vec::new(),
            scans: 0,
            changes: Vec::new(),
            received: Vec::new(),
            events: Vec::new(),
            counts: Counts::default(),
        };
        tracker.rescan();
        Ok(tracker)
    }

    pub fn follow_until(&mut self, deadline: u64) -> io::Result<()> {
        let timer = kevent(
            TIMER,
            libc::EVFILT_TIMER,
            libc::EV_ADD | libc::EV_ONESHOT,
            libc::NOTE_ABSOLUTE | libc::NOTE_MACHTIME | libc::NOTE_MACH_CONTINUOUS_TIME,
            isize::try_from(deadline).unwrap_or(isize::MAX),
        );
        let mut pending: &[libc::kevent] = std::slice::from_ref(&timer);

        loop {
            let count = self.wait(pending)?;
            pending = &[];

            let mut due = false;
            let mut forked = false;
            for index in 0..count {
                let event = self.received[index];
                if event.flags & libc::EV_ERROR != 0 {
                    return Err(io::Error::from_raw_os_error(
                        i32::try_from(event.data).unwrap_or(libc::EINVAL),
                    ));
                }
                if event.filter == libc::EVFILT_TIMER {
                    due = true;
                } else {
                    forked |= self.handle(event);
                }
            }

            if forked {
                self.rescan();
            }
            if due {
                return Ok(());
            }
        }
    }

    pub fn observe(&mut self, listing: &[i32]) {
        let previous = self.scans;
        self.scans += 1;
        let reporting = previous > 0;
        self.changes.clear();

        for &pid in listing {
            let Ok(index) = usize::try_from(pid) else {
                continue;
            };
            if index >= self.last_seen.len() {
                self.last_seen.resize(index + 1, 0);
            }
            let seen_in_previous_scan = reporting && self.last_seen[index] == previous;
            if !seen_in_previous_scan {
                self.changes.push(watch(pid));
                if reporting {
                    self.events.push(Event::Start { pid });
                }
            }
            self.last_seen[index] = self.scans;
        }

        self.submit_watches();
    }

    pub fn drain_events(&mut self) -> std::vec::Drain<'_, Event> {
        self.events.drain(..)
    }

    fn rescan(&mut self) {
        let mut listing = std::mem::take(&mut self.listing);
        processes::list_pids(&mut listing);
        self.observe(&listing);
        self.listing = listing;
    }

    fn handle(&mut self, event: libc::kevent) -> bool {
        let pid = i32::try_from(event.ident).unwrap_or(-1);
        if event.fflags & libc::NOTE_EXEC != 0 {
            self.events.push(Event::Exec { pid });
        }
        if event.fflags & libc::NOTE_EXIT != 0 {
            self.record_exit(pid, Noticed::OnExit);
        }

        let forked = event.fflags & libc::NOTE_FORK != 0;
        if forked {
            self.counts.forks += 1;
        }
        forked
    }

    fn record_exit(&mut self, pid: i32, noticed: Noticed) {
        self.events.push(Event::Exit {
            pid,
            last: processes::read_usage(pid, self.timebase),
            noticed,
        });
    }

    fn submit_watches(&mut self) {
        if self.changes.is_empty() {
            return;
        }

        let count = libc::c_int::try_from(self.changes.len()).unwrap_or(libc::c_int::MAX);
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

        let receipts = usize::try_from(receipts).unwrap_or(0);
        for index in 0..receipts {
            let receipt = self.received[index];
            if receipt.data == libc::EPERM as isize {
                self.counts.refused_watches += 1;
            } else if receipt.data == libc::ESRCH as isize {
                let pid = i32::try_from(receipt.ident).unwrap_or(-1);
                self.record_exit(pid, Noticed::AlreadyExited);
            }
        }
    }

    fn wait(&mut self, pending: &[libc::kevent]) -> io::Result<usize> {
        self.received
            .resize(RECEIVED_PER_WAIT, kevent(0, 0, 0, 0, 0));
        loop {
            let count = unsafe {
                libc::kevent(
                    self.queue.as_raw_fd(),
                    pending.as_ptr(),
                    libc::c_int::try_from(pending.len()).unwrap_or(0),
                    self.received.as_mut_ptr(),
                    libc::c_int::try_from(RECEIVED_PER_WAIT).unwrap_or(1),
                    std::ptr::null(),
                )
            };
            if let Ok(count) = usize::try_from(count) {
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

    fn follow_for(tracker: &mut Tracker, millis: u64) {
        let timebase = Timebase::read();
        tracker
            .follow_until(clock::now() + timebase.ticks(millis * 1_000_000))
            .unwrap();
    }

    #[test]
    fn reports_a_child_starting_and_exiting_with_its_final_usage() {
        let mut tracker = Tracker::new(Timebase::read()).unwrap();

        let mut child = std::process::Command::new("/bin/sleep")
            .arg("0.2")
            .spawn()
            .unwrap();
        follow_for(&mut tracker, 500);
        child.wait().unwrap();

        let pid = child.id().cast_signed();
        let events: Vec<Event> = tracker.drain_events().collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Start { pid: p } if *p == pid)),
            "child {pid}: {events:?}"
        );
        assert!(events.iter().any(|e| matches!(e, Event::Exit { pid: p, last: Ok(last), noticed: Noticed::OnExit } if *p == pid && last.user_ns + last.system_ns > 0)), "child {pid}: {events:?}");
    }

    #[test]
    fn returns_once_the_deadline_has_passed() {
        let timebase = Timebase::read();
        let mut tracker = Tracker::new(timebase).unwrap();
        let deadline = clock::now() + timebase.ticks(100_000_000);

        tracker.follow_until(deadline).unwrap();

        let late_ns = timebase.nanos(clock::now() - deadline);
        assert!(
            late_ns < 50_000_000,
            "returned {late_ns} ns after the deadline"
        );
    }

    #[test]
    fn reports_a_process_found_already_exited_with_its_final_usage() {
        let mut tracker = Tracker::new(Timebase::read()).unwrap();
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        let mut listing = Vec::new();
        processes::list_pids(&mut listing);

        tracker.observe(&listing);
        child.wait().unwrap();

        let pid = child.id().cast_signed();
        let events: Vec<Event> = tracker.drain_events().collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Exit { pid: p, last: Ok(_), noticed: Noticed::AlreadyExited } if *p == pid)),
            "child {pid}: {events:?}"
        );
    }

    #[test]
    fn reports_a_pid_first_seen_in_a_listing_as_a_start() {
        let mut tracker = Tracker::new(Timebase::read()).unwrap();
        let mut listing = Vec::new();
        processes::list_pids(&mut listing);
        listing.push(99_999);

        tracker.observe(&listing);

        let events: Vec<Event> = tracker.drain_events().collect();
        assert!(
            events
                .iter()
                .any(|e| matches!(e, Event::Start { pid: 99_999 })),
            "{events:?}"
        );
    }

    #[test]
    fn reports_nothing_for_pids_the_previous_listing_held() {
        let mut tracker = Tracker::new(Timebase::read()).unwrap();
        let mut listing = Vec::new();
        processes::list_pids(&mut listing);
        tracker.observe(&listing);
        tracker.drain_events();

        tracker.observe(&listing);

        assert_eq!(0, tracker.drain_events().count());
    }
}
