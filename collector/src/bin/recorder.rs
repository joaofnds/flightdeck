//! One paced owner of recording workers; its death leaves their independent locks intact.
use std::io;
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus};
use std::time::Duration;

use collector::recording::{
    self,
    control::{self, Control, Failure, Health, Reporter, Role, State},
    private::{self, FileLock},
};
use collector::ring::{Identity, Place};

#[path = "../owner.rs"]
mod owner;
#[path = "../recording/recorder_resources.rs"]
mod resources;
#[path = "../stop.rs"]
mod stop;

#[cfg(test)]
#[path = "recorder/tests.rs"]
mod tests;

fn main() {
    collector::diagnostics::install(collector::diagnostics::Role::Recorder);
    let status = match run() {
        Ok(()) => 0,
        Err(error) => {
            collector::diagnostics::error(format_args!("recorder: {error}"));
            1
        }
    };
    collector::diagnostics::exit(status);
}

struct Options {
    root: PathBuf,
    ring: String,
    seconds: Option<u64>,
    rescan: u64,
    memory: u64,
    stop: bool,
    parent: Option<i32>,
}

struct Worker {
    child: Option<Child>,
    attempts: u64,
    due: u64,
    cost: resources::WorkerCost,
    adopted: Option<recording::exit_watch::ExitWatch>,
}

fn run() -> io::Result<()> {
    let options = options()?;
    let owner = owner::Owner::bind(options.parent)?;
    if options.stop {
        return Control::open_writable(&options.root)?.request_stop();
    }
    recording::check_writer(&options.root)?;
    private::create_private_dir(&options.root)?;
    let _owner = FileLock::try_lock(&options.root.join("recorder.lock"))?;
    let mapped = Control::create(&options.root)?;
    let reporter = Reporter::new(mapped, Role::Controller)?;
    let _signals = stop::Stop::install()?;
    let mut supervisor = Supervisor {
        options,
        reporter,
        acquisition: Worker::new(),
        persistence: Worker::new(),
        finished: false,
        costs: resources::Accounting::new()?,
        owner,
    };
    for _step in 0..u64::MAX {
        if supervisor.step()? {
            return Ok(());
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    unreachable!("recorder exhausted its process-lifetime step bound");
}

struct Supervisor {
    options: Options,
    reporter: Reporter,
    acquisition: Worker,
    persistence: Worker,
    finished: bool,
    costs: resources::Accounting,
    owner: owner::Owner,
}

struct Step {
    now: u64,
    collector: Option<Health>,
    persister: Option<Health>,
    collecting: bool,
    persisting: bool,
    locks: Locks,
    stopping: bool,
}

struct Locks {
    collector: bool,
    persister: bool,
}

impl Supervisor {
    fn step(&mut self) -> io::Result<bool> {
        let now = control::now()?;
        self.reporter.refresh()?;
        if (stop::Stop::requested() || self.owner.departed()) && !self.reporter.control().stopping()
        {
            self.reporter.control().request_stop()?;
        }
        let persister_finished = self.step_reap(now)?;
        let collector_locked = FileLock::held(&self.options.root.join("collector.lock"))?;
        let persister_locked = FileLock::held(&self.options.root.join("persister.lock"))?;
        let collector = observed(self.reporter.control(), Role::Collector)?;
        let persister = observed(self.reporter.control(), Role::Persister)?;
        let collecting = collector_locked
            && collector
                .as_ref()
                .map(Health::alive)
                .transpose()?
                .unwrap_or(false);
        let persisting = persister_locked
            && persister
                .as_ref()
                .map(Health::alive)
                .transpose()?
                .unwrap_or(false);
        if let Some(collector) = &collector {
            self.acquisition.watch(collector, collecting)?;
        }
        if let Some(persister) = &persister {
            self.persistence.watch(persister, persisting)?;
        }
        if persister_finished && collecting {
            self.persistence.failed(now);
            self.reporter.retry(
                Failure::Operating,
                0,
                self.persistence.attempts,
                self.persistence.due,
            )?;
        }
        // A relaunch may find the last finite collector already exited without its
        // controller having seen the success status. Its terminal role record carries it.
        self.finished |= !collecting
            && collector
                .as_ref()
                .is_some_and(|health| health.state == State::Stopped);
        let stopping = self.reporter.control().stopping() || self.finished;
        self.step_costs(collector.as_ref(), persister.as_ref())?;
        self.advance(&Step {
            now,
            collector,
            persister,
            collecting,
            persisting,
            locks: Locks {
                collector: collector_locked,
                persister: persister_locked,
            },
            stopping,
        })
    }

    fn step_reap(&mut self, now: u64) -> io::Result<bool> {
        let collector_exit = self.acquisition.reap(self.costs.timebase())?;
        let persister_exit = self.persistence.reap(self.costs.timebase())?;
        if let Some(status) = collector_exit {
            if status.success() {
                self.finished = true;
            } else {
                self.acquisition.exited(status, now, &mut self.reporter)?;
            }
        }
        if let Some(status) = persister_exit {
            if !status.success() {
                self.persistence.exited(status, now, &mut self.reporter)?;
            }
        }
        Ok(persister_exit.is_some_and(|status| status.success()))
    }

    fn step_costs(
        &mut self,
        collector: Option<&Health>,
        persister: Option<&Health>,
    ) -> io::Result<()> {
        self.costs.observe(
            &mut self.acquisition.cost,
            self.acquisition.child.as_ref(),
            collector,
        )?;
        self.costs.observe(
            &mut self.persistence.cost,
            self.persistence.child.as_ref(),
            persister,
        )?;
        self.costs
            .report_if_due(&mut self.acquisition.cost, &mut self.persistence.cost)
    }

    fn advance(&mut self, step: &Step) -> io::Result<bool> {
        if step.stopping {
            return self.drain(step);
        }
        if !step.collecting
            && self.acquisition.idle()
            && !step.locks.collector
            && !step.persisting
            && self.persistence.idle()
            && !step.locks.persister
        {
            if self.current_completed()? {
                self.acquisition.start(
                    &self.options,
                    Role::Collector,
                    self.reporter.control().authority(),
                    step.now,
                    &mut self.reporter,
                )?;
            } else {
                // A collector may die before any live observation launched its persister.
                // Recover its registered source before a replacement can evict retained data.
                self.persistence.start(
                    &self.options,
                    Role::Persister,
                    self.reporter.control().authority(),
                    step.now,
                    &mut self.reporter,
                )?;
            }
        }
        if step.collecting && !step.persisting && self.persistence.idle() && !step.locks.persister {
            self.persistence.start(
                &self.options,
                Role::Persister,
                self.reporter.control().authority(),
                step.now,
                &mut self.reporter,
            )?;
        }
        if step.collecting && step.persisting {
            if let (Some(collector), Some(persister)) = (&step.collector, &step.persister) {
                self.reporter
                    .update(State::Running, collector.run, persister.publication)?;
            }
        }
        Ok(false)
    }

    fn drain(&mut self, step: &Step) -> io::Result<bool> {
        if !self.reporter.control().stopping() {
            self.reporter.control().request_stop()?;
        }
        self.reporter.set_state(State::Stopping)?;
        let persistence_idle = !step.persisting && self.persistence.idle() && !step.locks.persister;
        let workers_idle = persistence_idle
            && !step.collecting
            && self.acquisition.idle()
            && !step.locks.collector;
        // A previous persister's stopped record says nothing about its replacement's tail.
        // Read the latest receipt only after no worker can still register another source.
        let completed = workers_idle && self.current_completed()?;
        if persistence_idle && !completed {
            self.persistence.start(
                &self.options,
                Role::Persister,
                self.reporter.control().authority(),
                step.now,
                &mut self.reporter,
            )?;
        }
        if completed {
            self.costs.report(
                "tail",
                &mut self.acquisition.cost,
                &mut self.persistence.cost,
            )?;
            self.reporter.set_state(State::Stopped)?;
            return Ok(true);
        }
        Ok(false)
    }

    fn current_completed(&self) -> io::Result<bool> {
        let mut buffer: recording::RegistryBuffer = [0; 4097];
        let registry =
            recording::Registry::read_into(&self.options.root.join("runs"), &mut buffer)?;
        match registry.runs().last().transpose()? {
            Some(run) => {
                recording::Registry::completed(&self.options.root, &run.identity, run.ring_name)
            }
            None => Ok(true),
        }
    }
}

fn observed(control: &Control, role: Role) -> io::Result<Option<Health>> {
    match control.read(role) {
        Ok(health) => Ok(Some(health)),
        // An interrupted publication is not proof of absence. The independent role
        // lock still prevents another launch until the writer exits or a successor owns it.
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error),
    }
}

fn failure(status: ExitStatus) -> Failure {
    use std::os::unix::process::ExitStatusExt;
    if matches!(status.signal(), Some(6 | 4 | 11)) {
        return Failure::Invariant;
    }
    match status.code() {
        Some(2) => Failure::Configuration,
        Some(101) => Failure::Invariant,
        Some(74 | 75) => Failure::Resource,
        Some(code) if code == i32::from(recording::child::OWNERSHIP_EXIT_STATUS) => {
            Failure::Ownership
        }
        _ => Failure::Operating,
    }
}

impl Worker {
    fn new() -> Self {
        Self {
            child: None,
            attempts: 0,
            due: 0,
            cost: resources::WorkerCost::default(),
            adopted: None,
        }
    }

    fn reap(&mut self, timebase: collector::clock::Timebase) -> io::Result<Option<ExitStatus>> {
        let Some(child) = &mut self.child else {
            if let Some(watch) = &self.adopted {
                let status = watch.poll()?;
                if status.is_some() {
                    self.adopted = None;
                }
                return Ok(status);
            }
            return Ok(None);
        };
        let reaped = recording::child::reap_owned(child, timebase)?;
        if let Some(reaped) = reaped {
            self.cost.finished(&reaped)?;
            let pid = child.id();
            let finished = reaped.finished;
            self.child = None;
            collector::diagnostics::error(format_args!(
                "worker reaped pid={pid} status={} cpu_ns={} resident_bytes_max={}",
                finished.status, finished.cpu_ns, finished.resident_bytes_max
            ));
            return Ok(Some(ExitStatus::from_raw(finished.status)));
        }
        Ok(None)
    }

    fn exited(&mut self, status: ExitStatus, now: u64, reporter: &mut Reporter) -> io::Result<()> {
        let class = failure(status);
        self.failed(now);
        reporter.retry(class, status.code().unwrap_or(-1), self.attempts, self.due)?;
        if matches!(class, Failure::Configuration | Failure::Invariant) {
            reporter.update(State::Failed, Identity([0; 2]), 0)?;
            return Err(io::Error::other(
                "worker stopped on an invariant or configuration failure",
            ));
        }
        Ok(())
    }

    fn idle(&self) -> bool {
        self.child.is_none() && self.adopted.is_none()
    }

    fn watch(&mut self, health: &Health, alive: bool) -> io::Result<()> {
        if self.child.is_none() && self.adopted.is_none() && alive {
            self.watch_register(recording::exit_watch::ExitWatch::register(health))?;
        }
        Ok(())
    }

    fn watch_register(
        &mut self,
        result: io::Result<recording::exit_watch::ExitWatch>,
    ) -> io::Result<()> {
        const ESRCH: i32 = 3;
        assert!(self.idle());
        match result {
            Ok(watch) => self.adopted = Some(watch),
            // Native ESRCH and a changed creation both mean this watch has no authority.
            // The next paced step rechecks the role lock and process before any relaunch.
            Err(error)
                if error.kind() == io::ErrorKind::NotFound
                    || error.raw_os_error() == Some(ESRCH) => {}
            Err(error) => return Err(error),
        }
        Ok(())
    }

    fn failed(&mut self, now: u64) {
        self.attempts = self.attempts.saturating_add(1);
        let delay = match self.attempts {
            0 | 1 => 1,
            2 => 2,
            3 => 4,
            _ => 5,
        };
        self.due = now.saturating_add(delay * 1_000_000_000);
    }

    fn start(
        &mut self,
        options: &Options,
        role: Role,
        authority: Identity,
        now: u64,
        reporter: &mut Reporter,
    ) -> io::Result<()> {
        if now < self.due {
            return Ok(());
        }
        let executable = std::env::current_exe()?.canonicalize()?;
        let name = match role {
            Role::Collector => "collector",
            Role::Persister => "persister",
            Role::Controller => unreachable!(),
        };
        let mut command = Command::new(executable.with_file_name(name));
        command
            .arg("--recording")
            .arg(&options.root)
            .arg("--recorder-authority")
            .arg(recording::identity_text(authority).to_string())
            .process_group(0);
        if role == Role::Collector {
            command
                .arg("--ring")
                .arg(&options.ring)
                .arg("--rescan-ms")
                .arg(options.rescan.to_string());
            if let Some(seconds) = options.seconds {
                command.arg("--seconds").arg(seconds.to_string());
            }
        } else {
            command
                .arg("--memory-bytes")
                .arg(options.memory.to_string());
        }
        match command.spawn() {
            Ok(child) => {
                self.cost.started();
                self.child = Some(child);
            }
            Err(error) => {
                self.failed(now);
                reporter.retry(
                    Failure::Launch,
                    error.raw_os_error().unwrap_or(0),
                    self.attempts,
                    self.due,
                )?;
            }
        }
        Ok(())
    }
}

fn options() -> io::Result<Options> {
    let arguments = collector::arguments("recorder").map_err(io::Error::other)?;
    let mut ring = "flightdeck".to_owned();
    let mut root = None;
    let mut seconds = None;
    let mut rescan = 0;
    let mut memory = 256 * 1024 * 1024;
    let mut stop = false;
    let mut parent = None;
    for pair in arguments.chunks(2) {
        match pair {
            [flag, value] if flag == "--recording" => root = Some(PathBuf::from(value)),
            [flag, value] if flag == "--ring" => ring.clone_from(value),
            [flag, value] if flag == "--seconds" => {
                seconds = Some(
                    value
                        .parse::<std::num::NonZeroU64>()
                        .map_err(io::Error::other)?
                        .get(),
                );
            }
            [flag, value] if flag == "--rescan-ms" => {
                rescan = value.parse().map_err(io::Error::other)?;
            }
            [flag, value] if flag == "--memory-bytes" => {
                memory = value.parse().map_err(io::Error::other)?;
            }
            [flag, value] if flag == "--stop" && value == "yes" => stop = true,
            [flag, value] if flag == "--parent-pid" => {
                parent = Some(
                    owner::parse_parent_pid(value)
                        .map_err(|()| io::Error::from(io::ErrorKind::InvalidInput))?,
                );
            }
            _ => {
                return Err(io::Error::other(
                    "usage: recorder --recording PATH [--ring NAME] [--seconds N] \
                     [--rescan-ms N] [--memory-bytes N] [--stop yes]",
                ));
            }
        }
    }
    let root = match root {
        Some(root) => root,
        None => Place::for_user(&ring)?
            .lock_path
            .with_extension("recording"),
    };
    Ok(Options {
        root,
        ring,
        seconds,
        rescan,
        memory,
        stop,
        parent,
    })
}
