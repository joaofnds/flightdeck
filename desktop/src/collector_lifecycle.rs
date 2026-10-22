use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

#[path = "collector_lifecycle_diagnostics.rs"]
mod diagnostics;
#[path = "collector_lifecycle_health.rs"]
mod health;
#[path = "collector_lifecycle_policy.rs"]
mod policy;

#[cfg(test)]
#[path = "collector_lifecycle_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "collector_lifecycle_test_child.rs"]
pub(super) mod test_child;

use collector::recording::Registry;
use collector::ring::{Place, previous_generation, unlink_generation};
use diagnostics::Output;
pub(super) use policy::Mode;
use policy::{Action, Policy, RecordingStatus, Source, State};

use crate::self_reports::SelfReports;

pub(super) const STARTUP_LIMIT: Duration = policy::STARTUP_LIMIT;

pub(super) struct Configuration {
    pub root: PathBuf,
    pub binary: PathBuf,
    pub isolated: bool,
    pub mode: Mode,
}

pub(super) struct CollectorSession(Box<SessionState>);

struct SessionState {
    configuration: Configuration,
    policy: Policy,
    child: Option<Child>,
    standard: Option<Output>,
    diagnostic: Option<Output>,
    health: health::Health,
    started: Instant,
    reports: SelfReports,
}

const OUTPUT_BYTES_MAX: usize = 16_384;
const STOP_INTERVAL: Duration = Duration::from_millis(25);
const STOP_ATTEMPTS_MAX: u128 = 5082;
const _: () = {
    assert!(size_of::<CollectorSession>() <= 16);
    assert!(
        STOP_INTERVAL.as_millis() * STOP_ATTEMPTS_MAX
            > policy::SHUTDOWN_LIMIT.as_millis() + policy::KILL_LIMIT.as_millis()
    );
};

impl CollectorSession {
    pub fn new(configuration: Configuration) -> Self {
        let mode = configuration.mode;
        Self(Box::new(SessionState {
            configuration,
            policy: Policy::new(mode),
            child: None,
            standard: None,
            diagnostic: None,
            health: health::Health::new(),
            started: Instant::now(),
            reports: SelfReports::default(),
        }))
    }

    pub fn tick(&mut self) {
        if let Err(error) = self.poll_child() {
            self.failed(&format!("Collector observation failed: {error}"));
        }
        let source = match self.source_if_needed() {
            Ok(source) => source,
            Err(error) => {
                self.failed(&format!("Collector discovery failed: {error}"));
                Source::default()
            }
        };
        let action = self.0.policy.step(self.0.started.elapsed(), &source);
        if let Err(error) = self.act(action) {
            self.failed(&format!("Collector operation failed: {error}"));
        }
        if !self.0.policy.needs_health() {
            self.0.health.clear();
        }
    }

    pub fn begin_stop(&mut self) {
        self.0.health.clear();
        if let Err(error) = self.poll_child() {
            if self.0.policy.failure.is_none() {
                self.0.policy.failure =
                    Some(format!("Collector shutdown observation failed: {error}"));
            }
        }
        let action = self.0.policy.stop(self.0.started.elapsed());
        if let Err(error) = self.act(action) {
            self.0.policy.failure = Some(format!("Collector shutdown failed: {error}"));
        }
    }

    pub fn finished(&self) -> bool {
        if self.0.policy.state == State::Stopped {
            return true;
        }

        if self.0.policy.state == State::Failed {
            return self.0.child.is_none();
        }

        false
    }

    pub fn retry(&mut self) {
        self.0.policy.retry();
    }

    pub fn failure(&self) -> Option<&str> {
        self.0.policy.failure.as_deref()
    }

    pub fn ready(&self) -> bool {
        self.0.policy.state == State::Running
    }

    pub fn owned_pid(&self) -> Option<u32> {
        self.0.policy.owned_pid
    }

    pub fn elapsed(&self) -> Duration {
        self.0.started.elapsed()
    }

    /// What the collector this app started reported of its own cost.
    pub fn reports(&self) -> &SelfReports {
        &self.0.reports
    }

    pub fn observed_publication(&self) -> Option<u64> {
        self.0.policy.observed_publication()
    }

    pub fn status(&self) -> Option<(String, bool)> {
        if self.0.configuration.mode == Mode::Attach {
            return None;
        }

        let notice = if let Some(failure) = &self.0.policy.failure {
            failure.clone()
        } else {
            match self.0.policy.state {
                State::Discovering => "Looking for a running collector".into(),
                State::Starting => "Starting collector; waiting for advancing readings".into(),
                State::Attached | State::Running => return None,
                State::Stopping => "Stopping collector and saving its final readings".into(),
                State::Killing => "Collector exceeded its shutdown deadline".into(),
                State::Stopped => "Collector stopped".into(),
                State::Failed => "Collector failed".into(),
            }
        };
        Some((notice, self.0.policy.can_retry()))
    }

    pub fn stop_and_wait(&mut self) -> Result<(), String> {
        self.begin_stop();
        for _attempt in 0..STOP_ATTEMPTS_MAX {
            self.tick();
            if self.finished() {
                return match &self.0.policy.failure {
                    Some(failure) => Err(failure.clone()),
                    None => Ok(()),
                };
            }

            std::thread::sleep(STOP_INTERVAL);
        }

        Err("Collector shutdown exceeded its 127-second bound".into())
    }

    fn source_if_needed(&mut self) -> Result<Source, String> {
        if !self.0.policy.needs_health() {
            self.0.health.clear();
            return Ok(Source::default());
        }

        match std::fs::metadata(&self.0.configuration.root) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Source {
                    recording: RecordingStatus::Missing,
                    ..Source::default()
                });
            }
            Err(error) => return Err(error.to_string()),
        }

        self.0
            .health
            .read(&self.0.configuration.root, self.0.policy.owned_pid)
            .map_err(|error| error.to_string())
    }

    fn act(&mut self, action: Action) -> io::Result<()> {
        match action {
            Action::None => Ok(()),
            Action::Spawn => self.spawn(),
            Action::Terminate => self.signal_term(),
            Action::Kill => match &mut self.0.child {
                Some(child) => child.kill(),
                None => Ok(()),
            },
        }
    }

    fn spawn(&mut self) -> io::Result<()> {
        assert!(self.0.child.is_none());
        let ring = if self.0.configuration.isolated {
            isolated_ring()
        } else {
            "flightdeck".into()
        };
        let standard = Output::new()?;
        let diagnostic = Output::new()?;
        let child = spawn_command(&self.0.configuration, &ring)
            .stdin(Stdio::null())
            .stdout(Stdio::from(standard.writer()?))
            .stderr(Stdio::from(diagnostic.writer()?))
            .spawn()?;
        self.0.policy.launched(child.id(), self.0.started.elapsed());
        eprintln!(
            "Started owned collector {} parent={}",
            child.id(),
            std::process::id()
        );
        self.0.reports.begin(wall_second()?);
        self.0.child = Some(child);
        self.0.standard = Some(standard);
        self.0.diagnostic = Some(diagnostic);
        Ok(())
    }

    fn poll_child(&mut self) -> io::Result<()> {
        let drained = self.drain_outputs();
        let status = match &mut self.0.child {
            Some(child) => child.try_wait()?.map(|status| (child.id(), status)),
            None => None,
        };
        if let Some((pid, status)) = status {
            self.0.child = None;
            let final_drained = self.drain_outputs();
            eprintln!(
                "Owned collector {pid} diagnostic file peak bytes: stdout={} stderr={}",
                self.0.standard.as_ref().map_or(0, Output::peak_bytes),
                self.0.diagnostic.as_ref().map_or(0, Output::peak_bytes)
            );
            let diagnostic = self
                .0
                .diagnostic
                .take()
                .map_or_else(String::new, |output| output.text());
            let standard = self
                .0
                .standard
                .take()
                .map_or_else(String::new, |output| output.text());
            eprintln!("Owned collector {pid} exited: {status}");
            if !standard.is_empty() {
                eprintln!("Owned collector {pid} output:\n{standard}");
            }
            if !diagnostic.is_empty() {
                eprintln!("Owned collector {pid} diagnostics:\n{diagnostic}");
            }
            self.0.policy.exited(
                &format!("Collector exited ({status}). {diagnostic} {standard}"),
                status.success(),
            );
            if self.0.configuration.isolated {
                self.remove_isolated_ring();
            }
            return drained.and(final_drained);
        }

        drained
    }

    /// Reports a failed removal and goes on, since the run it follows has ended either way.
    fn remove_isolated_ring(&self) {
        let removed = Place::for_user(&isolated_ring())
            .and_then(|place| remove_finished_ring(&self.0.configuration.root, &place));
        if let Err(error) = removed {
            eprintln!("Flightdeck isolated ring cleanup: {error}");
        }
    }

    fn drain_outputs(&mut self) -> io::Result<()> {
        let second = wall_second()?;
        if let Some(standard) = &mut self.0.standard {
            standard.drain()?;
            for line in standard.take_lines() {
                self.0.reports.standard(&line, second);
            }
        }
        if let Some(diagnostic) = &mut self.0.diagnostic {
            diagnostic.drain()?;
            for line in diagnostic.take_lines() {
                self.0.reports.diagnostic(&line);
            }
        }
        Ok(())
    }

    fn signal_term(&self) -> io::Result<()> {
        let Some(child) = &self.0.child else {
            return Ok(());
        };

        let pid = i32::try_from(child.id()).map_err(io::Error::other)?;
        eprintln!("Stopping owned collector {pid} with SIGTERM");
        // This owner alone reaps the child, so its PID cannot be reused before this signal.
        if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }

        Ok(())
    }

    fn failed(&mut self, message: &str) {
        if self.0.child.is_some() {
            if self.0.policy.failure.is_none() {
                self.0.policy.failure = Some(message.chars().take(OUTPUT_BYTES_MAX).collect());
            }
            self.begin_stop();
        } else {
            self.0.policy.fail(message);
        }
    }
}

impl Drop for CollectorSession {
    fn drop(&mut self) {
        if let Err(error) = self.signal_term() {
            eprintln!("Flightdeck owned collector cleanup: {error}");
        }
    }
}

pub(super) fn binary(executable: &Path) -> io::Result<PathBuf> {
    let directory = executable.parent().ok_or(io::ErrorKind::InvalidInput)?;
    if directory.file_name() == Some(std::ffi::OsStr::new("MacOS")) {
        let contents = directory.parent().ok_or(io::ErrorKind::InvalidInput)?;
        return Ok(contents.join("Helpers/collector"));
    }

    Ok(Path::new(env!("CARGO_MANIFEST_DIR")).join("../target/release/collector"))
}

/// The command that starts the collector on `ring`. It sets the library's thread defaults
/// whatever settings the app runs with, explicit ones included, since the app sizes its own for
/// its chart reads, so that the persister the collector starts keeps one Polars thread and
/// leaves the Mac's cores to the programs it records.
fn spawn_command(configuration: &Configuration, ring: &str) -> Command {
    let mut command = Command::new(&configuration.binary);
    command
        .arg("--ring")
        .arg(ring)
        .arg("--recording")
        .arg(&configuration.root)
        .arg("--parent-pid")
        .arg(std::process::id().to_string())
        .envs(persistence::engine::THREAD_DEFAULTS.environment());
    command
}

/// The ring of a collector this app starts on a recording of its own, named for this app's
/// process so that no two such apps contend for one lock file.
fn isolated_ring() -> String {
    format!("flightdeck-{}", std::process::id())
}

/// Removes each ring of `place` whose readings the recording at `root` has saved, and the lock
/// file once its newest ring is gone, since a ring outlives its collector with about 800 MB of
/// address space. A ring the recording may still need stays, with its lock file, where the user
/// can find it.
fn remove_finished_ring(root: &Path, place: &Place) -> io::Result<()> {
    let Some(newest) = previous_generation(place)? else {
        return Ok(());
    };
    let newest = newest.as_str();
    // Every ring of one lock file shares the lock's token and ends in a generation number.
    let Some(dot) = newest.rfind('.') else {
        unreachable!("ring {newest} holds a generation, as previous_generation parsed it");
    };
    let stem = &newest[..=dot];

    let registry = Registry::read(root)?;
    let mut newest_kept = true;
    for run in &registry.runs {
        if run.ring_name.starts_with(stem) {
            if Registry::completed(root, &run.identity, &run.ring_name)? {
                match unlink_generation(&run.ring_name) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error),
                }
                if run.ring_name == newest {
                    newest_kept = false;
                }
            } else {
                eprintln!(
                    "Kept ring {}, whose readings are not saved yet",
                    run.ring_name
                );
            }
        }
    }

    if newest_kept {
        return Ok(());
    }

    std::fs::remove_file(&place.lock_path)
}

fn wall_second() -> io::Result<u64> {
    let since_epoch = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_err(io::Error::other)?;
    Ok(since_epoch.as_secs())
}
