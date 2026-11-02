//! Native identity checks for bounded cleanup of this fixture's own descendants.
use std::ffi::{c_int, c_void};
use std::io;
use std::mem::offset_of;
use std::process::{Child, Command};
use std::time::Duration;

use collector::programs::CreationError;
use collector::ring::ProcessCreation;

const PROC_UID_ONLY: u32 = 4;
const PIDS_MAX: usize = 4097;
const PIDS_BYTES_MAX: usize = PIDS_MAX * size_of::<i32>();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Process {
    pub pid: i32,
    pub creation: ProcessCreation,
}

#[derive(Default)]
#[repr(C)]
struct ProcBsdshortinfo {
    pid: u32,
    ppid: u32,
    pgid: u32,
    status: u32,
    comm: [u8; 16],
    flags: u32,
    uid: u32,
    gid: u32,
    ruid: u32,
    rgid: u32,
    svuid: u32,
    svgid: u32,
    rfu: u32,
}

const _: () = {
    assert!(size_of::<ProcBsdshortinfo>() == 64);
    assert!(offset_of!(ProcBsdshortinfo, ppid) == 4);
    assert!(offset_of!(ProcBsdshortinfo, status) == 12);
    assert!(PIDS_BYTES_MAX == 16_388);
    assert!(size_of::<Process>() == 24);
};

unsafe extern "C" {
    fn proc_listchildpids(ppid: c_int, buffer: *mut c_void, buffersize: c_int) -> c_int;
    fn proc_pidinfo(
        pid: c_int,
        flavor: c_int,
        arg: u64,
        buffer: *mut c_void,
        buffersize: c_int,
    ) -> c_int;
    fn kill(pid: c_int, signal: c_int) -> c_int;
    fn setsid() -> c_int;
    fn getsid(pid: c_int) -> c_int;
    fn getuid() -> u32;
    fn proc_listpids(kind: u32, information: u32, buffer: *mut c_void, bytes: c_int) -> c_int;
    fn __error() -> *mut c_int;
}

impl Process {
    pub(super) fn capture(pid: i32) -> io::Result<Option<Self>> {
        Ok(creation(pid)?.map(|creation| Self { pid, creation }))
    }

    pub(super) fn alive(&self) -> io::Result<bool> {
        Ok(creation(self.pid)?.is_some_and(|creation| creation == self.creation))
    }

    pub(super) fn capture_owned(pid: i32) -> io::Result<Self> {
        creation_including_exited(pid, true)?
            .map(|creation| Self { pid, creation })
            .ok_or_else(|| io::Error::other("unreaped owned child identity unavailable"))
    }

    pub(super) fn anchor(&self) -> io::Result<()> {
        if creation_including_exited(self.pid, true)? != Some(self.creation) {
            return Err(io::Error::other(
                "private session anchor changed or was reaped",
            ));
        }
        Ok(())
    }

    pub(super) fn session(&self) -> io::Result<Option<i32>> {
        if !self.alive()? {
            return Ok(None);
        }
        let session = unsafe { getsid(self.pid) };
        if session < 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(3) {
                return Ok(None);
            }
            return Err(error);
        }
        Ok(self.alive()?.then_some(session))
    }

    pub(super) fn signal_in_session(&self, anchor: &Process, signal: i32) -> io::Result<()> {
        anchor.anchor()?;
        match self.session()? {
            Some(session) if session == anchor.pid => self.signal(signal),
            None => Ok(()),
            Some(_) => Err(io::Error::other(
                "owned native identity left its private session",
            )),
        }
    }

    pub(super) fn stopped(&self) -> io::Result<bool> {
        let Some(status) = status(self.pid)? else {
            return Ok(false);
        };
        Ok(status.status == 4 && self.alive()?)
    }

    pub(super) fn group(&self) -> io::Result<Option<u32>> {
        let information = status(self.pid)?;
        if !self.alive()? {
            return Ok(None);
        }
        Ok(information.map(|information| information.pgid))
    }

    pub(super) fn signal(&self, signal: i32) -> io::Result<()> {
        if !self.alive()? {
            return Ok(());
        }
        if unsafe { kill(self.pid, signal) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(3) {
                return Err(error);
            }
        }
        Ok(())
    }
}

/// Runs in `pre_exec`: `setsid` is the only native operation and errors allocate no message.
pub(super) fn new_session() -> io::Result<()> {
    if unsafe { setsid() } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// The unreaped direct child anchors the session ID even after it becomes a zombie.
/// Workers create separate process groups, so group or surviving-parent inventories omit them.
pub(super) fn session(anchor: &Process, members: &mut Vec<Process>) -> io::Result<usize> {
    anchor.anchor()?;
    let mut pids = vec![0_i32; PIDS_MAX];
    let bytes_max = i32::try_from(PIDS_BYTES_MAX).map_err(io::Error::other)?;
    let bytes = unsafe {
        *__error() = 0;
        proc_listpids(PROC_UID_ONLY, getuid(), pids.as_mut_ptr().cast(), bytes_max)
    };
    let count = session_count(bytes, io::Error::last_os_error())?;
    members.clear();
    for pid in &pids[..count] {
        if *pid > 0 {
            session_member(anchor, *pid, members)?;
        }
    }
    anchor.anchor()?;
    Ok(count)
}

fn session_count(bytes: i32, error: io::Error) -> io::Result<usize> {
    if bytes < 0 || (bytes == 0 && error.raw_os_error() != Some(0)) {
        return Err(error);
    }
    let bytes = usize::try_from(bytes).map_err(io::Error::other)?;
    if bytes == 0 || bytes % size_of::<i32>() != 0 || bytes >= PIDS_MAX * size_of::<i32>() {
        return Err(io::Error::other(
            "private-session UID inventory truncated or malformed",
        ));
    }
    Ok(bytes / size_of::<i32>())
}

#[test]
fn session_inventory_rejects_unobservable_or_truncated_lists() {
    // Goal: a bounded inventory never treats missing or saturated membership as empty.
    // Method: exercise native byte-count/error conversion without enumerating or signaling a PID.
    for bytes in [-1, 0, 1, 16_387, 16_388, 16_392] {
        assert!(session_count(bytes, io::Error::from_raw_os_error(0)).is_err());
    }
    assert!(session_count(0, io::Error::from_raw_os_error(13)).is_err());
    assert_eq!(
        session_count(4, io::Error::from_raw_os_error(0)).unwrap(),
        1
    );
    assert_eq!(
        session_count(16_384, io::Error::from_raw_os_error(0)).unwrap(),
        4096
    );
}

fn session_member(anchor: &Process, pid: i32, members: &mut Vec<Process>) -> io::Result<()> {
    let Some(process) = Process::capture(pid)? else {
        return Ok(());
    };
    if process.session()? != Some(anchor.pid) {
        return Ok(());
    }
    let Some(information) = status(pid)? else {
        return Ok(());
    };
    if information.uid != unsafe { getuid() } {
        return Err(io::Error::other("private-session member changed user"));
    }
    if process.session()? == Some(anchor.pid) {
        retain(members, process)?;
    }
    Ok(())
}

pub(super) fn children(parent: &Process, known: &mut Vec<Process>) -> io::Result<()> {
    if !parent.alive()? {
        return Ok(());
    }
    let mut pids = [0_i32; 9];
    let count = unsafe {
        *__error() = 0;
        proc_listchildpids(parent.pid, pids.as_mut_ptr().cast(), 36)
    };
    let error = io::Error::last_os_error();
    if count < 0 || (count == 0 && error.raw_os_error() != Some(0)) {
        return Err(error);
    }
    let count = usize::try_from(count).map_err(io::Error::other)?;
    if count >= pids.len() {
        return Err(io::Error::other("owned controller child inventory bound"));
    }
    for pid in &pids[..count] {
        let Some(child) = Process::capture(*pid)? else {
            continue;
        };
        let Some(status) = status(*pid)? else {
            continue;
        };
        if status.ppid == parent.pid.cast_unsigned() && child.alive()? {
            retain(known, child)?;
        }
    }
    Ok(())
}

pub(super) fn retain(known: &mut Vec<Process>, child: Process) -> io::Result<()> {
    if !known.contains(&child) {
        if known.len() == 64 {
            return Err(io::Error::other("owned descendant identity bound"));
        }
        known.push(child);
    }
    Ok(())
}

fn creation(pid: i32) -> io::Result<Option<ProcessCreation>> {
    creation_including_exited(pid, false)
}

fn creation_including_exited(
    pid: i32,
    include_exited: bool,
) -> io::Result<Option<ProcessCreation>> {
    let mut result = Err(CreationError::NotRead);
    collector::programs::read_creation(pid, include_exited, &mut result);
    match result {
        Ok(creation) => Ok(Some(creation)),
        Err(CreationError::Gone(_)) => Ok(None),
        Err(error) => Err(io::Error::other(format!(
            "owned native identity: {error:?}"
        ))),
    }
}

fn status(pid: i32) -> io::Result<Option<ProcBsdshortinfo>> {
    let mut value = ProcBsdshortinfo::default();
    let returned = unsafe { proc_pidinfo(pid, 13, 0, (&raw mut value).cast(), 64) };
    if returned != 64 {
        let error = io::Error::last_os_error();
        if returned == 0 && error.raw_os_error() == Some(3) {
            return Ok(None);
        }
        return Err(if returned == 0 && error.raw_os_error() != Some(0) {
            error
        } else {
            io::Error::from(io::ErrorKind::InvalidData)
        });
    }
    Ok(Some(value))
}

#[test]
#[ignore = "opt-in native identity cleanup proof for unreported owned children"]
fn captures_and_stops_an_owned_child_without_a_health_record() {
    // Goal: forced cleanup can identify an owned startup child before control health exists.
    // Method: inventory a real paused sleep child, validate its parent/creation, then reap it.
    let mut child = Owned(Command::new("/bin/sleep").arg("120").spawn().unwrap());
    let pid = i32::try_from(child.0.id()).unwrap();
    let process = Process::capture(pid).unwrap().unwrap();
    process.signal(17).unwrap();
    let parent = Process::capture(i32::try_from(std::process::id()).unwrap())
        .unwrap()
        .unwrap();
    let mut known = Vec::with_capacity(64);

    children(&parent, &mut known).unwrap();

    assert!(known.contains(&process));
    process.signal(9).unwrap();
    for _step in 0..1000 {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(!status.success());
            assert!(!process.alive().unwrap());
            return;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("owned native cleanup probe did not exit");
}

struct Owned(Child);

impl Drop for Owned {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
            for _step in 0..1000 {
                if self.0.try_wait().unwrap().is_some() {
                    return;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            panic!("owned native cleanup probe did not reap");
        }
    }
}

#[test]
#[ignore = "opt-in native private-session exclusion and unreaped-anchor proof"]
fn private_session_excludes_other_sessions_and_requires_its_unreaped_anchor() {
    // Goal: the private session cannot authorize a foreign PID or survive anchor reaping.
    // Method: use two owned sleep children in different sessions; signal0 never changes either.
    use std::os::unix::process::CommandExt;

    let mut command = Command::new("/bin/sleep");
    command.arg("120");
    unsafe {
        command.pre_exec(new_session);
    }
    let mut child = Owned(command.spawn().unwrap());
    let other = Owned(Command::new("/bin/sleep").arg("120").spawn().unwrap());
    let anchor = Process::capture_owned(i32::try_from(child.0.id()).unwrap()).unwrap();
    let foreign = Process::capture_owned(i32::try_from(other.0.id()).unwrap()).unwrap();
    let mut members = Vec::with_capacity(64);

    session(&anchor, &mut members).unwrap();
    assert!(members.contains(&anchor));
    assert!(!members.contains(&foreign));
    assert!(foreign.signal_in_session(&anchor, 0).is_err());
    assert!(foreign.alive().unwrap());
    anchor.signal_in_session(&anchor, 9).unwrap();
    for _step in 0..1000 {
        if !anchor.alive().unwrap() {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(!anchor.alive().unwrap());
    anchor.anchor().unwrap();
    session(&anchor, &mut members).unwrap();
    assert!(members.is_empty());
    assert!(child.0.try_wait().unwrap().is_some());
    assert!(anchor.anchor().is_err());
    assert!(session(&anchor, &mut members).is_err());
    assert!(foreign.alive().unwrap());
}
