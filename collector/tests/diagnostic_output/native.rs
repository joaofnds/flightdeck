use std::ffi::c_void;
use std::io;
use std::process::Child;
use std::time::{Duration, Instant};

use collector::programs::{CreationError, read_creation};
use collector::ring::ProcessCreation;

pub(super) struct Children {
    known: [Option<Identity>; 16],
    current_pids: [i32; 16],
}

pub(super) struct OwnedChild {
    pub(super) child: Child,
    reaped: bool,
}

#[derive(Clone, Copy)]
struct Identity {
    pid: i32,
    creation: ProcessCreation,
    persister: bool,
}

unsafe extern "C" {
    fn __error() -> *mut i32;
    fn proc_listchildpids(parent: i32, buffer: *mut c_void, size: i32) -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn close(descriptor: i32) -> i32;
}

impl Children {
    pub(super) fn new() -> Self {
        Self {
            known: [None; 16],
            current_pids: [0; 16],
        }
    }

    pub(super) fn capture(&mut self, parent: u32) -> io::Result<()> {
        let mut listed = [0_i32; 16];
        unsafe {
            *__error() = 0;
        }
        let returned = unsafe {
            proc_listchildpids(
                i32::try_from(parent).map_err(io::Error::other)?,
                listed.as_mut_ptr().cast(),
                i32::try_from(size_of_val(&listed)).map_err(io::Error::other)?,
            )
        };
        if returned < 0 {
            return Err(io::Error::last_os_error());
        }
        if returned == 0 {
            let status = unsafe { *__error() };
            if status != 0 {
                return Err(io::Error::from_raw_os_error(status));
            }
        }
        let count = usize::try_from(returned).map_err(io::Error::other)?;
        if count >= listed.len() {
            return Err(io::ErrorKind::QuotaExceeded.into());
        }
        self.current_pids = listed;

        for pid in &listed[..count] {
            if let Some(identity) = Identity::read(*pid)? {
                self.remember(identity)?;
            }
        }

        Ok(())
    }

    fn remember(&mut self, identity: Identity) -> io::Result<()> {
        if let Some(known) = self
            .known
            .iter_mut()
            .flatten()
            .find(|known| known.pid == identity.pid)
        {
            if known.creation == identity.creation {
                known.persister |= identity.persister;
            } else {
                *known = identity;
            }
        } else {
            let empty = self
                .known
                .iter_mut()
                .find(|known| known.is_none())
                .ok_or(io::ErrorKind::QuotaExceeded)?;
            eprintln!(
                "tracked child pid={} creation={:?} persister={}",
                identity.pid, identity.creation, identity.persister
            );
            *empty = Some(identity);
        }

        Ok(())
    }

    pub(super) fn persister_count(&self) -> u64 {
        let count = self
            .known
            .iter()
            .flatten()
            .filter(|identity| identity.persister)
            .count();

        u64::try_from(count).unwrap()
    }

    pub(super) fn all_absent(&self) -> bool {
        self.known
            .iter()
            .flatten()
            .all(|identity| match identity.matches() {
                Ok(same) => !same,
                Err(error) => {
                    eprintln!("child absence unavailable pid={}: {error}", identity.pid);
                    false
                }
            })
    }

    pub(super) fn stop_and_capture(&mut self, child: &mut OwnedChild) -> io::Result<bool> {
        signal(child.child.id(), 17)?;
        let start = Instant::now();
        let mut stopped = false;

        for _attempt in 0..101 {
            let mut status = 0;
            let returned = unsafe {
                waitpid(
                    i32::try_from(child.child.id()).map_err(io::Error::other)?,
                    &raw mut status,
                    1 | 2,
                )
            };
            if returned < 0 {
                return Err(io::Error::last_os_error());
            }
            if returned > 0 {
                if status & 0xff != 0x7f {
                    child.reaped = true;
                    return Ok(false);
                }
                stopped = true;
                break;
            }
            if start.elapsed() >= Duration::from_secs(1) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if !stopped {
            return Err(io::ErrorKind::TimedOut.into());
        }
        self.capture(child.child.id())?;

        Ok(true)
    }

    pub(super) fn kill_verified(&self) -> io::Result<()> {
        for identity in self.known.iter().flatten() {
            if self.current_pids.contains(&identity.pid) {
                if identity.matches()? {
                    signal(identity.pid.cast_unsigned(), 9)?;
                }
            }
        }

        Ok(())
    }
}

impl Identity {
    fn read(pid: i32) -> io::Result<Option<Self>> {
        let mut creation = Err(CreationError::NotRead);
        read_creation(pid, false, &mut creation);
        let creation = match creation {
            Ok(creation) => creation,
            Err(CreationError::Gone(_)) => return Ok(None),
            Err(error) => return Err(io::Error::other(format!("child {pid}: {error:?}"))),
        };
        let mut path = [0_u8; 4096];
        let persister = match collector::programs::read_path(pid, &mut path) {
            Ok(length) => path[..length].ends_with(b"/persister"),
            Err(collector::programs::PathError::Gone(_)) => return Ok(None),
            Err(error) => return Err(io::Error::other(format!("child {pid}: {error:?}"))),
        };

        Ok(Some(Self {
            pid,
            creation,
            persister,
        }))
    }

    fn matches(&self) -> io::Result<bool> {
        let mut creation = Err(CreationError::NotRead);
        read_creation(self.pid, true, &mut creation);

        match creation {
            Ok(current) => Ok(current == self.creation),
            Err(CreationError::Gone(_)) => Ok(false),
            Err(error) => Err(io::Error::other(format!("child {}: {error:?}", self.pid))),
        }
    }
}

pub(super) fn signal(pid: u32, number: i32) -> io::Result<()> {
    let returned = unsafe { kill(i32::try_from(pid).map_err(io::Error::other)?, number) };

    if returned != 0 {
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(3) {
            return Err(error);
        }
    }

    Ok(())
}

pub(super) fn close_stdout() -> io::Result<()> {
    if unsafe { close(1) } != 0 {
        return Err(io::Error::last_os_error());
    }

    Ok(())
}

impl OwnedChild {
    pub(super) fn new(child: Child) -> Self {
        Self {
            child,
            reaped: false,
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.reaped {
            return;
        }
        match self.child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(error) => {
                eprintln!("owned child status unavailable: {error}");
                if error.raw_os_error() == Some(10) {
                    return;
                }
            }
        }
        if let Err(error) = signal(self.child.id(), 19) {
            eprintln!("owned child resume unavailable: {error}");
        }
        if let Err(error) = self.child.kill() {
            eprintln!("owned child kill unavailable: {error}");
        }
        let start = Instant::now();
        for _attempt in 0..101 {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => {}
                Err(error) => {
                    eprintln!("owned child reap unavailable: {error}");
                    return;
                }
            }
            if start.elapsed() >= Duration::from_secs(1) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        eprintln!(
            "owned child still present after kill pid={}",
            self.child.id()
        );
    }
}
