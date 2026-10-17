use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use collector::ring::ProcessCreation;

use crate::sample::Sample;

#[path = "child.rs"]
mod child;
#[path = "exit.rs"]
mod exit;
#[path = "sweep.rs"]
mod sweep;

pub const ROLE: &str = "FLIGHTDECK_ASSOC_ROLE";
pub const ROOT: &str = "FLIGHTDECK_ASSOC_ROOT";
pub const PHASE: &str = "FLIGHTDECK_ASSOC_PHASE";
pub const MODE: &str = "FLIGHTDECK_ASSOC_MODE";
pub const EXECUTIONS: u32 = 8;
pub const HELPER: &str = "Contents/Frameworks/Nested.app/Contents/MacOS/identity_probe";
const SAMPLES_MAX: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Spawn,
    SamePath,
    CrossApp,
    SetExec,
    Fork,
    Ports,
}

impl Mode {
    pub fn text(self) -> &'static str {
        match self {
            Self::Spawn => "spawn",
            Self::SamePath => "same",
            Self::CrossApp => "cross",
            Self::SetExec => "setexec",
            Self::Fork => "fork",
            Self::Ports => "ports",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Ack {
    pub pid: i32,
    pub phase: u32,
    pub creation: ProcessCreation,
}

impl Ack {
    pub const EMPTY: Self = Self {
        pid: 0,
        phase: 0,
        creation: ProcessCreation {
            unique_id: 0,
            pid_version: 0,
        },
    };

    pub fn read(&mut self, peer: &mut UnixStream) {
        let mut bytes = [0; 20];
        peer.read_exact(&mut bytes).unwrap();
        self.pid = i32::from_le_bytes(bytes[..4].try_into().unwrap());
        self.phase = u32::from_le_bytes(bytes[4..8].try_into().unwrap());
        self.creation.unique_id = u64::from_le_bytes(bytes[8..16].try_into().unwrap());
        self.creation.pid_version = i32::from_le_bytes(bytes[16..].try_into().unwrap());
    }

    pub fn write(&self, peer: &mut UnixStream) {
        let mut bytes = [0; 20];
        bytes[..4].copy_from_slice(&self.pid.to_le_bytes());
        bytes[4..8].copy_from_slice(&self.phase.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.creation.unique_id.to_le_bytes());
        bytes[16..].copy_from_slice(&self.creation.pid_version.to_le_bytes());
        peer.write_all(&bytes).unwrap();
    }
}

pub struct Fixture {
    root: PathBuf,
    pub app_a: PathBuf,
    app_b: PathBuf,
    pub image_a: PathBuf,
    image_b: PathBuf,
    mode: Mode,
    child: Option<Child>,
    listener: UnixListener,
    peer: Option<UnixStream>,
    pub ack: Ack,
}

impl Fixture {
    pub fn start(output: &mut Option<Box<Self>>, test: &str, mode: Mode) {
        let token =
            collector::recording::identity_text(collector::recording::random_identity().unwrap())
                .to_string();
        let root = Path::new("/tmp").join(format!("fdassoc-{token}"));
        collector::recording::private::create_private_dir(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let app_a = root.join("A.app");
        let app_b = root.join("B.app");
        let image_a = app_a.join(HELPER);
        let image_b = app_b.join(HELPER);
        std::fs::create_dir_all(image_a.parent().unwrap()).unwrap();
        std::fs::create_dir_all(image_b.parent().unwrap()).unwrap();
        let original = std::env::current_exe().unwrap();
        assert!(std::fs::metadata(&original).unwrap().len() <= 64 * 1024 * 1024);
        std::fs::copy(&original, &image_a).unwrap();
        std::fs::copy(&original, &image_b).unwrap();
        let listener = UnixListener::bind(root.join("control")).unwrap();
        listener.set_nonblocking(true).unwrap();
        *output = Some(Box::new(Self {
            root,
            app_a,
            app_b,
            image_a,
            image_b,
            mode,
            child: None,
            listener,
            peer: None,
            ack: Ack::EMPTY,
        }));
        let fixture = output.as_mut().unwrap();
        fixture.child = Some(
            Command::new(&fixture.image_a)
                .args(["--exact", test, "--nocapture"])
                .env(ROLE, "child")
                .env(ROOT, &fixture.root)
                .env(PHASE, "0")
                .env(MODE, mode.text())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        let mut peer = accept(&fixture.listener);
        fixture.ack.read(&mut peer);
        if mode == Mode::Fork {
            assert_ne!(
                fixture.child.as_ref().unwrap().id(),
                fixture.ack.pid.cast_unsigned()
            );
        } else {
            assert_eq!(
                fixture.child.as_ref().unwrap().id(),
                fixture.ack.pid.cast_unsigned()
            );
        }
        assert_eq!(0, fixture.ack.phase);
        fixture.peer = Some(peer);
    }

    pub fn pid(&self) -> i32 {
        self.ack.pid
    }

    pub fn exercise_exec(&mut self) {
        let mut baseline = Sample::EMPTY;
        baseline.capture(self.pid());
        baseline.assert_image(&self.ack, &self.image_a, &self.app_a);
        baseline.report("exec-baseline");
        for phase in 1..=EXECUTIONS {
            let before = self.ack;
            self.peer.take().unwrap().write_all(b"E").unwrap();
            let mut samples = Vec::with_capacity(SAMPLES_MAX);
            let mut peer = None;
            for _attempt in 0..SAMPLES_MAX {
                samples.push(Sample::EMPTY);
                samples.last_mut().unwrap().capture(self.pid());
                if let Some(accepted) = try_accept(&self.listener) {
                    peer = Some(accepted);
                    break;
                }
                std::thread::sleep(Duration::from_micros(25));
            }
            let mut peer = peer.unwrap_or_else(|| accept(&self.listener));
            self.ack.read(&mut peer);
            assert_eq!(before.pid, self.ack.pid);
            assert_eq!(phase, self.ack.phase);
            assert_eq!(before.creation.unique_id, self.ack.creation.unique_id);
            assert_ne!(before.creation.pid_version, self.ack.creation.pid_version);
            self.check_samples(&before, &samples);
            let mut stable = Sample::EMPTY;
            stable.capture(self.pid());
            let (image, app) = self.expected(phase);
            stable.assert_image(&self.ack, image, app);
            stable.report("exec-acknowledged");
            self.peer = Some(peer);
        }
        self.finish();
    }

    fn check_samples(&self, before: &Ack, samples: &[Sample]) {
        let mut accepted = 0_u32;
        let mut accepted_before = 0_u32;
        let mut allocations = 0_u64;
        let mut elapsed_ns = 0_u64;
        for sample in samples {
            allocations += sample.allocations;
            elapsed_ns += sample.elapsed_ns;
            if sample.acquisition.candidate {
                let ack = if sample.acquisition.creation_before == Ok(before.creation) {
                    accepted_before += 1;
                    before
                } else {
                    &self.ack
                };
                let (image, app) = self.expected(ack.phase);
                sample.assert_image(ack, image, app);
                accepted += 1;
            } else {
                sample.report("rejected-during-exec");
            }
        }
        eprintln!(
            "transition mode={:?} phase={} before={before:?} after={:?} samples={} \
            accepted={accepted} accepted_before={accepted_before} \
            elapsed_ns={elapsed_ns} rust_allocator_calls={allocations}",
            self.mode,
            self.ack.phase,
            self.ack,
            samples.len()
        );
    }

    fn expected(&self, phase: u32) -> (&PathBuf, &PathBuf) {
        if self.mode == Mode::SamePath {
            return (&self.image_a, &self.app_a);
        }
        if phase.is_multiple_of(2) {
            (&self.image_a, &self.app_a)
        } else {
            (&self.image_b, &self.app_b)
        }
    }

    pub fn finish(&mut self) {
        if let Some(mut peer) = self.peer.take() {
            peer.write_all(b"Q").unwrap();
        }
        let child = self.child.as_mut().unwrap();
        wait_until(|| match child.try_wait().unwrap() {
            Some(status) => {
                assert!(status.success(), "owned image failed: {status}");
                true
            }
            None => false,
        });
    }

    pub fn exercise_ports(&mut self) {
        self.peer.take().unwrap().write_all(b"P").unwrap();
        self.finish();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            if child.try_wait().unwrap().is_none() {
                child.kill().unwrap();
                let status = child.wait().unwrap();
                eprintln!("owned image cleanup status={status}");
            }
        }
        if let Err(error) = std::fs::remove_dir_all(&self.root) {
            eprintln!("owned image cleanup: {error}");
        }
    }
}

pub fn child_role(test: &str) -> bool {
    match std::env::var(ROLE) {
        Ok(role) => {
            assert_eq!(role, "child");
            child::run(test);
        }
        Err(std::env::VarError::NotPresent) => false,
        Err(error) => panic!("invalid native probe role: {error}"),
    }
}

fn try_accept(listener: &UnixListener) -> Option<UnixStream> {
    match listener.accept() {
        Ok((peer, _address)) => {
            peer.set_nonblocking(false).unwrap();
            peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            Some(peer)
        }
        Err(error) => {
            assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock);
            None
        }
    }
}

fn accept(listener: &UnixListener) -> UnixStream {
    let mut peer = None;
    wait_until(|| {
        peer = try_accept(listener);
        peer.is_some()
    });
    peer.unwrap()
}

fn wait_until(mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(5);
    for _attempt in 0..5000 {
        if ready() {
            return;
        }
        if Instant::now() >= deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("owned image handshake exceeded five seconds");
}
