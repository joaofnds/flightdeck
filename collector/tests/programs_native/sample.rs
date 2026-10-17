use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Instant;

use collector::processes::{Unreadable, Usage};
use collector::programs::{Acquisition, acquire, split_program};

use crate::allocation;
use crate::fixture::Ack;

pub struct Sample {
    pub acquisition: Acquisition,
    pub path: [u8; 4096],
    numeric: Option<io::Result<Result<Usage, Unreadable>>>,
    pub calls: u32,
    pub allocations: u64,
    pub elapsed_ns: u64,
}

impl Sample {
    pub const EMPTY: Self = Self {
        acquisition: Acquisition::EMPTY,
        path: [0; 4096],
        numeric: None,
        calls: 0,
        allocations: 0,
        elapsed_ns: 0,
    };

    pub fn capture(&mut self, pid: i32) {
        self.capture_using(pid, false);
    }

    pub fn capture_exited(&mut self, pid: i32) {
        self.capture_using(pid, true);
    }

    fn capture_using(&mut self, pid: i32, include_exited: bool) {
        let timebase = collector::clock::Timebase::read().unwrap();
        self.calls = 0;
        self.numeric = None;
        let started = Instant::now();
        self.allocations = allocation::calls_in(|| {
            acquire(
                pid,
                include_exited,
                &mut self.path,
                &mut self.acquisition,
                || {
                    self.numeric = Some(collector::processes::read_usage(timebase, pid));
                    self.calls += 1;
                },
            );
        });
        self.elapsed_ns = u64::try_from(started.elapsed().as_nanos()).unwrap();
        assert_eq!(1, self.calls);
        assert_eq!(
            0, self.allocations,
            "actual native acquisition allocated Rust memory"
        );
    }

    pub fn assert_image(&self, ack: &Ack, image: &Path, app: &Path) {
        assert!(self.acquisition.candidate);
        assert_eq!(Ok(ack.creation), self.acquisition.creation_before);
        assert_eq!(Ok(ack.creation), self.acquisition.creation_after);
        assert_eq!(Ok(()), self.acquisition.readiness);
        let length = self.acquisition.path.unwrap().unwrap();
        assert_eq!(image.as_os_str().as_bytes(), &self.path[..length]);
        let mut program = None;
        split_program(&self.path[..length], &mut program).unwrap();
        let program = program.unwrap();
        assert_eq!(b"identity_probe", program.executable_name);
        assert_eq!(Some(app.as_os_str().as_bytes()), program.app_path);
        assert_eq!(
            0, self.allocations,
            "actual accepted capture allocated Rust memory"
        );
    }

    pub fn report(&self, label: &str) {
        eprintln!(
            "sample label={label} before={:?} gate={:?} after={:?} candidate={} \
            path_result={:?} numeric={:?} elapsed_ns={} rust_allocator_calls={}",
            self.acquisition.creation_before,
            self.acquisition.readiness,
            self.acquisition.creation_after,
            self.acquisition.candidate,
            self.acquisition.path,
            self.numeric,
            self.elapsed_ns,
            self.allocations
        );
        if let Some(Ok(length)) = self.acquisition.path {
            eprintln!("path_bytes label={label} raw={:?}", &self.path[..length]);
        }
    }
}
