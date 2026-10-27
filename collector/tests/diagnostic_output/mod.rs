mod native;

use std::fs::{self, File};
use std::io::{self, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use collector::ring::{Place, RingReader};
use native::{Children, OwnedChild};

const COLLECTOR_LIMIT: Duration = Duration::from_secs(30);
const FILL_LIMIT: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(10);
static NEXT: AtomicU64 = AtomicU64::new(1);

pub enum Output {
    Supplied(Stdio),
    Absent,
}

pub struct Harness {
    root: PathBuf,
    name: String,
    place: Place,
    collector: Option<OwnedChild>,
    peer: Option<UnixStream>,
    children: Children,
    reader: Option<RingReader>,
    cleaned: bool,
}

#[derive(Debug, Default)]
pub struct Observed {
    pub status: Option<ExitStatus>,
    pub publications: u64,
    pub completed: bool,
    pub persister_count: u64,
    pub persister_absent: bool,
    pub sealed: bool,
}

impl Harness {
    pub fn new(test: &str) -> Self {
        let root = std::env::temp_dir().join(format!("flightdeck-{test}-{}", std::process::id()));
        fs::DirBuilder::new().mode(0o700).create(&root).unwrap();
        let instance = NEXT.fetch_add(1, Ordering::Relaxed);
        let name = format!("fdout{}n{instance}", std::process::id());
        let place = Place::for_user(&name).unwrap();
        assert!(
            !place.lock_path.exists(),
            "private fixture name already exists"
        );
        let collector = match std::env::var_os("FLIGHTDECK_DIAGNOSTIC_BINARIES") {
            Some(directory) => PathBuf::from(directory).join("collector"),
            None => PathBuf::from(env!("CARGO_BIN_EXE_collector")),
        };
        let persister = collector.parent().unwrap().join("persister");
        assert!(
            persister.is_file(),
            "build both actual binaries before this check"
        );
        fs::copy(&collector, root.join("collector")).unwrap();
        fs::copy(&persister, root.join("persister")).unwrap();

        Self {
            root,
            name,
            place,
            collector: None,
            peer: None,
            children: Children::new(),
            reader: None,
            cleaned: false,
        }
    }

    pub fn full_sink(&mut self, test: &str, role: &str) -> Stdio {
        let (peer, sink) = UnixStream::pair().unwrap();
        sink.set_nonblocking(true).unwrap();
        let mut filler = OwnedChild::new(
            Command::new(std::env::current_exe().unwrap())
                .args(["--exact", test, "--nocapture"])
                .env(role, "1")
                .stdout(Stdio::from(OwnedFd::from(sink.try_clone().unwrap())))
                .stderr(File::create(self.root.join("filler.log")).unwrap())
                .spawn()
                .unwrap(),
        );
        let status = wait(&mut filler.child, FILL_LIMIT);
        if status.is_none() {
            filler.child.kill().unwrap();
            assert!(wait(&mut filler.child, Duration::from_secs(1)).is_some());
            panic!("prefill child exceeded its independent controller deadline");
        }
        assert!(status.unwrap().success(), "prefill child failed");
        sink.set_nonblocking(false).unwrap();
        self.peer = Some(peer);

        Stdio::from(OwnedFd::from(sink))
    }

    pub fn start(&mut self, output: Output) {
        let stderr = File::create(self.root.join("collector-stderr.log")).unwrap();
        self.start_with_stderr(output, Stdio::from(stderr));
    }

    pub fn start_with_stderr(&mut self, output: Output, stderr: Stdio) {
        let mut command = Command::new(self.root.join("collector"));
        command
            .args(["--seconds", "12", "--ring"])
            .arg(&self.name)
            .arg("--recording")
            .arg(self.root.join("recording"))
            .stderr(stderr);
        match output {
            Output::Supplied(stdout) => {
                command.stdout(stdout);
            }
            Output::Absent => {
                command.stdout(Stdio::null());
                unsafe {
                    command.pre_exec(native::close_stdout);
                }
            }
        }
        self.collector = Some(OwnedChild::new(command.spawn().unwrap()));
    }

    pub fn broken_sink() -> Stdio {
        let (peer, sink) = UnixStream::pair().unwrap();
        drop(peer);

        Stdio::from(OwnedFd::from(sink))
    }

    pub fn observe(&mut self) -> Observed {
        let child = self.collector.as_mut().unwrap();
        let start = Instant::now();
        let mut observed = Observed::default();

        for _attempt in 0..3_001 {
            self.children.capture(child.child.id()).unwrap();
            if self.reader.is_none() {
                match RingReader::open(&self.place.lock_path) {
                    Ok(reader) => self.reader = Some(reader),
                    Err(error) => match error.kind() {
                        io::ErrorKind::NotFound | io::ErrorKind::WouldBlock => {}
                        _ => panic!("private ring opening failed: {error}"),
                    },
                }
            }
            if let Some(reader) = &self.reader {
                observed.publications = reader.publication_newest().unwrap();
                observed.completed = reader.completed();
            }
            if let Some(status) = child.child.try_wait().unwrap() {
                if let Some(reader) = &self.reader {
                    observed.publications = reader.publication_newest().unwrap();
                    observed.completed = reader.completed();
                }
                observed.status = Some(status);
                observed.persister_count = self.children.persister_count();
                observed.persister_absent = self.children.all_absent();
                observed.sealed = self.has_committed_window();
                self.collector = None;
                break;
            }
            if start.elapsed() >= COLLECTOR_LIMIT {
                break;
            }
            std::thread::sleep(POLL);
        }
        fs::write(self.root.join("observed.log"), format!("{observed:?}\n")).unwrap();
        eprintln!(
            "diagnostic controller root={} observed={observed:?}",
            self.root.display()
        );

        observed
    }

    fn has_committed_window(&self) -> bool {
        let runs = match fs::read_dir(self.root.join("recording/commits")) {
            Ok(entries) => entries,
            Err(error) => {
                assert_eq!(io::ErrorKind::NotFound, error.kind());
                return false;
            }
        };
        for run in runs.take(2) {
            for sequence in fs::read_dir(run.unwrap().path()).unwrap().take(2) {
                for window in fs::read_dir(sequence.unwrap().path()).unwrap().take(2) {
                    let window = window.unwrap();
                    if window.path().extension() == Some(std::ffi::OsStr::new("seal")) {
                        let metadata = window.metadata().unwrap();
                        if metadata.is_file() {
                            if metadata.len() > 0 {
                                return true;
                            }
                        }
                    }
                }
            }
        }

        false
    }

    pub fn cleanup(&mut self) -> io::Result<()> {
        if self.cleaned {
            return Ok(());
        }
        self.cleanup_collector()?;
        if !self.children.all_absent() {
            return Err(io::Error::other("private child absence remains unproved"));
        }
        if self.place.lock_path.exists() {
            let reader = RingReader::open(&self.place.lock_path)?;
            if !reader.name().starts_with(&self.place.name_prefix) {
                return Err(io::Error::other("private ring prefix changed"));
            }
            collector::ring::unlink_generation(reader.name())?;
            drop(reader);
            fs::remove_file(&self.place.lock_path)?;
        }
        self.reader = None;
        self.cleaned = true;

        Ok(())
    }

    fn cleanup_collector(&mut self) -> io::Result<()> {
        if let Some(mut child) = self.collector.take() {
            let stopped = self.children.stop_and_capture(&mut child);
            match stopped {
                Ok(true) => {
                    let killed = self.children.kill_verified();
                    let resumed = native::signal(child.child.id(), 19);
                    let terminated = native::signal(child.child.id(), 15);
                    if let Err(error) = killed {
                        eprintln!("private descendants cleanup unavailable: {error}");
                    }
                    resumed?;
                    terminated?;
                    if wait(&mut child.child, Duration::from_secs(3)).is_none() {
                        child.child.kill()?;
                        if wait(&mut child.child, Duration::from_secs(1)).is_none() {
                            return Err(io::ErrorKind::TimedOut.into());
                        }
                    }
                }
                Ok(false) => {}
                Err(error) => return Err(error),
            }
        }

        Ok(())
    }

    pub fn remove_successful_fixture(&self) {
        assert!(self.cleaned);
        if std::env::var_os("FLIGHTDECK_DIAGNOSTIC_RETAIN").is_some() {
            eprintln!("retained requested fixture {}", self.root.display());
            return;
        }
        let entries = fs::read_dir(&self.root)
            .unwrap()
            .take(8)
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(entries.len() <= 6);
        let names = [
            "collector",
            "persister",
            "filler.log",
            "collector-stderr.log",
            "observed.log",
            "recording",
        ];
        for entry in &entries {
            assert!(names.iter().any(|name| entry.file_name() == *name));
        }
        eprintln!(
            "removed successful private fixture entries={}",
            entries.len()
        );
        fs::remove_dir_all(&self.root).unwrap();
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            eprintln!("retained failed fixture {}: {error}", self.root.display());
        }
    }
}

fn wait(child: &mut Child, limit: Duration) -> Option<ExitStatus> {
    let start = Instant::now();
    let attempts = limit.as_millis().div_ceil(POLL.as_millis()) + 1;

    for _attempt in 0..attempts {
        if let Some(status) = child.try_wait().unwrap() {
            return Some(status);
        }
        if start.elapsed() >= limit {
            break;
        }
        std::thread::sleep(POLL);
    }

    None
}

pub fn fill_stdout() -> ! {
    let mut output = unsafe { File::from_raw_fd(1) };
    let mut accepted_bytes = 0_u64;

    for _attempt in 0..16_384 {
        match output.write(&[b'x'; 4096]) {
            Ok(count) => {
                assert!(count > 0);
                accepted_bytes += u64::try_from(count).unwrap();
            }
            Err(error) => {
                if error.kind() == io::ErrorKind::WouldBlock {
                    eprintln!("prefill accepted_bytes={accepted_bytes}");
                    std::process::exit(0);
                }
                panic!("prefill failed: {error}");
            }
        }
    }
    panic!("prefill exceeded fixed byte-attempt budget");
}
