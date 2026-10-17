use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use collector::programs::{CreationError, read_creation};

use super::{Ack, EXECUTIONS, HELPER, MODE, PHASE, ROOT};

#[path = "fork.rs"]
mod fork;
#[path = "ports.rs"]
mod ports;
#[path = "setexec.rs"]
mod setexec;

pub fn run(test: &str) -> ! {
    std::hint::black_box(unsafe { alarm(20) });
    let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
    let phase = std::env::var(PHASE).unwrap().parse::<u32>().unwrap();
    assert!(phase <= EXECUTIONS);
    let mode = std::env::var(MODE).unwrap();
    let mut peer = UnixStream::connect(root.join("control")).unwrap();
    peer.set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    if mode == "fork" {
        fork::run(&mut peer);
    }
    let mut creation = Err(CreationError::NotRead);
    let pid = collector::processes::own_pid().unwrap();
    read_creation(pid, false, &mut creation);
    let ack = Ack {
        pid,
        phase,
        creation: creation.unwrap(),
    };
    ack.write(&mut peer);
    let mut command = [0; 1];
    peer.read_exact(&mut command).unwrap();
    if command == *b"Q" {
        std::process::exit(0);
    }
    if command == *b"P" {
        ports::observe(&ack, &root);
        std::process::exit(0);
    }
    assert_eq!(command, *b"E");
    assert!(phase < EXECUTIONS);
    let next = phase + 1;
    let app = if mode == "same" {
        "A.app"
    } else {
        if next.is_multiple_of(2) {
            "A.app"
        } else {
            "B.app"
        }
    };
    let target = root.join(app).join(HELPER);
    if mode == "setexec" {
        setexec::replace(&setexec::Replacement {
            image: &target,
            root: &root,
            test,
            phase: next,
        });
    }
    let error = Command::new(target)
        .args(["--exact", test, "--nocapture"])
        .env(PHASE, next.to_string())
        .exec();
    peer.write_all(b"exec-error").unwrap();
    panic!("owned image exec failed: {error}");
}

unsafe extern "C" {
    fn alarm(seconds: u32) -> u32;
}
