use std::io::Read;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;

use collector::programs::{CreationError, read_creation};

use super::Ack;

pub fn run(peer: &mut UnixStream) -> ! {
    let mut descriptors = [-1; 2];
    assert_eq!(0, unsafe { pipe(descriptors.as_mut_ptr()) });
    let receive = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
    let release = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
    let peer_descriptor = peer.as_raw_fd();
    let receive_descriptor = receive.as_raw_fd();
    let release_descriptor = release.as_raw_fd();
    let pid = unsafe { fork() };
    if pid == 0 {
        // After multithreaded fork, the child calls only async-signal-safe libc functions.
        unsafe {
            if close(release_descriptor) != 0 {
                _exit(71);
            }
            if close(peer_descriptor) != 0 {
                _exit(72);
            }
            alarm(10);
            let mut command = [0_u8; 1];
            let count = read(receive_descriptor, command.as_mut_ptr(), 1);
            if close(receive_descriptor) != 0 {
                _exit(73);
            }
            if count == 1 {
                _exit(0);
            }
            _exit(74);
        }
    }
    assert!(pid > 0, "fork: {}", std::io::Error::last_os_error());
    drop(receive);
    let mut child = Forked {
        pid: Some(pid),
        release: Some(release),
    };
    let mut creation = Err(CreationError::NotRead);
    read_creation(pid, false, &mut creation);
    Ack {
        pid,
        phase: 0,
        creation: creation.unwrap(),
    }
    .write(peer);
    let mut command = [0; 1];
    peer.read_exact(&mut command).unwrap();
    assert_eq!(command, *b"Q");
    child.finish();
    std::process::exit(0);
}

struct Forked {
    pid: Option<i32>,
    release: Option<OwnedFd>,
}

impl Forked {
    fn finish(&mut self) {
        assert_eq!(1, unsafe {
            write(self.release.as_ref().unwrap().as_raw_fd(), b"Q".as_ptr(), 1)
        });
        let pid = self.pid.unwrap();
        let mut status = 0;
        assert_eq!(pid, unsafe { waitpid(pid, &raw mut status, 0) });
        self.pid = None;
        assert_eq!(0, status, "owned fork child status");
    }
}

impl Drop for Forked {
    fn drop(&mut self) {
        drop(self.release.take());
        if let Some(pid) = self.pid.take() {
            let mut status = 0;
            let returned = unsafe { waitpid(pid, &raw mut status, 0) };
            eprintln!("owned fork cleanup pid={pid} wait_return={returned} status={status}");
        }
    }
}

unsafe extern "C" {
    fn fork() -> i32;
    fn pipe(descriptors: *mut i32) -> i32;
    fn read(descriptor: i32, output: *mut u8, size: usize) -> isize;
    fn write(descriptor: i32, bytes: *const u8, size: usize) -> isize;
    fn close(descriptor: i32) -> i32;
    fn alarm(seconds: u32) -> u32;
    fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    fn _exit(status: i32) -> !;
}
