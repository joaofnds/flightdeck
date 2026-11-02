//! One kernel process watch for a verified non-child worker, without signaling authority.
use std::ffi::c_void;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;

use super::control::Health;

pub struct ExitWatch {
    descriptor: OwnedFd,
    pid: i32,
}

#[repr(C)]
#[derive(Default)]
struct Kevent {
    ident: usize,
    filter: i16,
    flags: u16,
    fflags: u32,
    data: isize,
    udata: *mut c_void,
}

#[repr(C)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

const EVFILT_PROC: i16 = -5;
const EV_ADD: u16 = 1;
const EV_ONESHOT: u16 = 0x10;
const EV_RECEIPT: u16 = 0x40;
const EV_ERROR: u16 = 0x4000;
const NOTE_EXIT: u32 = 0x8000_0000;
// Darwin permits NOTE_EXITSTATUS for a child or a process the same user may signal.
const NOTE_EXITSTATUS: u32 = 0x0400_0000;
const _: () = assert!(size_of::<Kevent>() == 32);
const _: () = assert!(size_of::<Timespec>() == 16);

unsafe extern "C" {
    fn kqueue() -> i32;
    fn fcntl(descriptor: i32, command: i32, ...) -> i32;
    fn kevent(
        descriptor: i32,
        changes: *const Kevent,
        count: i32,
        events: *mut Kevent,
        capacity: i32,
        timeout: *const Timespec,
    ) -> i32;
}

impl ExitWatch {
    /// The caller verifies the role lock and cohort before watching. Native creation must
    /// match on both sides of registration, so a reused PID cannot become this watch.
    pub fn register(health: &Health) -> io::Result<Self> {
        if !health.alive()? {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        let raw = unsafe { kqueue() };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        let descriptor = unsafe { OwnedFd::from_raw_fd(raw) };
        if unsafe { fcntl(descriptor.as_raw_fd(), 2, 1) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let request = Kevent {
            ident: usize::try_from(health.pid).map_err(io::Error::other)?,
            filter: EVFILT_PROC,
            flags: EV_ADD | EV_ONESHOT | EV_RECEIPT,
            fflags: NOTE_EXIT | NOTE_EXITSTATUS,
            ..Kevent::default()
        };
        let mut receipt = Kevent::default();
        if exchange(&descriptor, Some(&request), &mut receipt)? != 1 {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        if receipt.flags & EV_ERROR == 0 || receipt.data != 0 {
            return Err(io::Error::from_raw_os_error(
                i32::try_from(receipt.data).map_err(io::Error::other)?,
            ));
        }
        if !health.alive()? {
            return Err(io::Error::from(io::ErrorKind::NotFound));
        }
        Ok(Self {
            descriptor,
            pid: health.pid,
        })
    }

    pub fn poll(&self) -> io::Result<Option<ExitStatus>> {
        let mut event = Kevent::default();
        if exchange(&self.descriptor, None, &mut event)? == 0 {
            return Ok(None);
        }
        if event.ident != usize::try_from(self.pid).map_err(io::Error::other)?
            || event.filter != EVFILT_PROC
            || event.flags & EV_ERROR != 0
            || event.fflags & (NOTE_EXIT | NOTE_EXITSTATUS) != (NOTE_EXIT | NOTE_EXITSTATUS)
        {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        Ok(Some(ExitStatus::from_raw(
            i32::try_from(event.data).map_err(io::Error::other)?,
        )))
    }
}

fn exchange(descriptor: &OwnedFd, request: Option<&Kevent>, event: &mut Kevent) -> io::Result<i32> {
    let zero = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    for _interruption in 0..8 {
        let count = unsafe {
            kevent(
                descriptor.as_raw_fd(),
                request.map_or(std::ptr::null(), std::ptr::from_ref),
                i32::from(request.is_some()),
                event,
                1,
                &raw const zero,
            )
        };
        if count >= 0 {
            return Ok(count);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
    Err(io::Error::from(io::ErrorKind::Interrupted))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_a_live_pid_with_the_wrong_native_creation() {
        // Goal: a reused PID cannot obtain a watch for the previous worker's authority.
        // Method: present this process with a mismatching native creation identifier.
        let mut health = Health::own().unwrap();
        health.creation.unique_id ^= 1;
        assert_eq!(
            ExitWatch::register(&health).err().unwrap().kind(),
            io::ErrorKind::NotFound
        );
    }
}
