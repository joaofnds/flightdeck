//! An isolated real controller, including its native descendant cleanup responsibility.
use std::fs::{self, File};
use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

use collector::recording::control::{self, Control, Failure, Health, Role, State};
use collector::recording::{Registry, private};
use collector::ring::{Identity, Place, RunIdentity};

#[path = "managed_native.rs"]
mod native;

#[path = "managed_tests.rs"]
mod tests;

pub(super) struct Recorder {
    root: PathBuf,
    helpers: PathBuf,
    ring: String,
    place: Place,
    child: Option<Child>,
    identity: Option<native::Process>,
    authority: Option<Identity>,
    descendants: Vec<native::Process>,
    exit: Option<ExitStatus>,
    inventories: u64,
    inventoried: u64,
    stopped: bool,
}

#[derive(Debug)]
pub(super) struct Recovery {
    pub cause_clear_ns: u64,
    pub restored_ns: u64,
    pub due_ns: u64,
    pub worker_seen_ns: u64,
    pub worker_previous_poll_ns: u64,
    pub canonical_ns: u64,
    pub worker: Health,
}

impl Recorder {
    pub(super) fn new(root: &Path, persister: &Path) -> io::Result<Self> {
        let helpers = root.join("helpers");
        private::create_private_dir(&helpers)?;
        for name in ["collector", "recorder", "persister"] {
            let source = if name == "persister" {
                persister.to_owned()
            } else {
                persister.with_file_name(name)
            };
            let destination = if name == "persister" {
                "persister.withheld"
            } else {
                name
            };
            fs::copy(&source, helpers.join(destination))?;
            eprintln!(
                "managed_helper source={} destination={} bytes={}",
                source.display(),
                helpers.join(destination).display(),
                fs::metadata(&source)?.len()
            );
        }
        let leaf = root
            .file_name()
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
        let ring = format!("managed-{}", leaf.to_string_lossy());
        let place = Place::for_user(&ring)?;
        if place.lock_path.exists() {
            return Err(io::Error::from(io::ErrorKind::AlreadyExists));
        }
        Ok(Self {
            root: root.to_owned(),
            helpers,
            ring,
            place,
            child: None,
            identity: None,
            authority: None,
            descendants: Vec::with_capacity(64),
            exit: None,
            inventories: 0,
            inventoried: 0,
            stopped: false,
        })
    }

    pub(super) fn launch(&mut self) -> io::Result<()> {
        self.launch_with(|_command| {})
    }

    pub(super) fn launch_with(&mut self, configure: impl FnOnce(&mut Command)) -> io::Result<()> {
        assert!(self.child.is_none());
        let log = File::create(self.root.join("managed.log"))?;
        let mut command = Command::new(self.helpers.join("recorder"));
        command
            .arg("--recording")
            .arg(&self.root)
            .arg("--ring")
            .arg(&self.ring)
            .args(["--seconds", "1", "--memory-bytes", "268435456"])
            .stdout(log.try_clone()?)
            .stderr(log);
        configure(&mut command);
        // The controller's workers make their own groups but inherit this private session.
        unsafe {
            command.pre_exec(native::new_session);
        }
        self.child = Some(command.spawn()?);
        let pid = i32::try_from(self.child.as_ref().unwrap().id()).map_err(io::Error::other)?;
        self.identity = Some(native::Process::capture_owned(pid)?);
        if self
            .identity
            .as_ref()
            .unwrap()
            .session()?
            .is_some_and(|session| session != pid)
        {
            return Err(io::Error::other("owned recorder lacks its private session"));
        }
        Ok(())
    }

    pub(super) fn recover(&mut self, run: &RunIdentity, source: &str) -> io::Result<Recovery> {
        let retry = self.wait_backoff(run, source)?;
        let cause_clear_ns = control::now()?;
        if retry.next_attempt_ns.saturating_sub(cause_clear_ns) < 4_500_000_000 {
            return Err(io::Error::other(
                "managed observation missed the worst retry phase",
            ));
        }
        fs::rename(
            self.helpers.join("persister.withheld"),
            self.helpers.join("persister"),
        )?;
        let restored_ns = control::now()?;
        if retry.next_attempt_ns.saturating_sub(restored_ns) < 4_500_000_000 {
            return Err(io::Error::other(
                "helper restoration missed the worst retry phase",
            ));
        }
        eprintln!(
            "managed_cause_clear before_ns={cause_clear_ns} restored_ns={restored_ns} \
            retry={retry:?}"
        );
        self.recover_wait(
            run,
            source,
            Recovery {
                cause_clear_ns,
                restored_ns,
                due_ns: retry.next_attempt_ns,
                worker_seen_ns: 0,
                worker_previous_poll_ns: 0,
                canonical_ns: 0,
                worker: Health::default(),
            },
        )
    }

    fn recover_wait(
        &mut self,
        run: &RunIdentity,
        source: &str,
        mut recovery: Recovery,
    ) -> io::Result<Recovery> {
        let mut first: Option<Health> = None;
        let mut previous_poll_ns = recovery.restored_ns;
        for _step in 0..18_000 {
            let control = self
                .observe()?
                .ok_or_else(|| io::Error::other("managed control disappeared"))?;
            let worker = read_role(&control, Role::Persister)?;
            let observed_ns = control::now()?;
            if let Some(worker) = worker.filter(|worker| worker.pid > 0) {
                if let Some(first_worker) = &first {
                    if worker.pid != first_worker.pid || worker.creation != first_worker.creation {
                        return Err(io::Error::other(
                            "first managed recovery worker was replaced",
                        ));
                    }
                } else {
                    if observed_ns < recovery.due_ns {
                        return Err(io::Error::other("managed worker observed before retry due"));
                    }
                    first = Some(worker);
                    recovery.worker_seen_ns = observed_ns;
                    recovery.worker_previous_poll_ns = previous_poll_ns;
                }
            }
            if worker.is_some_and(|worker| worker.pid > 0 && worker.state == State::Stopped)
                && Registry::completed(&self.root, run, source)?
            {
                first.ok_or_else(|| {
                    io::Error::other("canonical tail without observed owned worker")
                })?;
                recovery.canonical_ns = control::now()?;
                recovery.worker = worker.unwrap();
                return Ok(recovery);
            }
            self.require_running()?;
            previous_poll_ns = observed_ns;
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(io::Error::other(
            "managed recovery exceeded180second fixture observation deadline",
        ))
    }

    fn wait_backoff(&mut self, run: &RunIdentity, source: &str) -> io::Result<Health> {
        for _step in 0..3000 {
            if let Some(control) = self.observe()? {
                if let Some(health) = read_role(&control, Role::Controller)? {
                    if health.failure == Failure::Launch && health.retries >= 4 {
                        let registry = Registry::read(&self.root)?;
                        if registry.runs.len() != 1
                            || registry.runs[0].identity != *run
                            || Registry::completed(&self.root, run, source)?
                        {
                            return Err(io::Error::other(
                                "recovery failure replaced or retired the source",
                            ));
                        }
                        if read_role(&control, Role::Persister)?
                            .is_some_and(|worker| worker.pid > 0)
                        {
                            return Err(io::Error::other(
                                "withheld persister unexpectedly launched",
                            ));
                        }
                        return Ok(health);
                    }
                }
            }
            self.require_running()?;
            std::thread::sleep(Duration::from_millis(10));
        }
        Err(io::Error::other(
            "managed launch refusal did not reach maximum backoff",
        ))
    }

    fn observe(&mut self) -> io::Result<Option<Control>> {
        let control = match Control::open(&self.root) {
            Ok(control) => control,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if self
            .authority
            .is_some_and(|authority| authority != control.authority())
        {
            return Err(io::Error::other("owned control authority changed"));
        }
        self.authority = Some(control.authority());
        for role in [Role::Collector, Role::Persister] {
            if let Some(health) = read_role(&control, role)? {
                if health.pid > 0 {
                    native::retain(
                        &mut self.descendants,
                        native::Process {
                            pid: health.pid,
                            creation: health.creation,
                        },
                    )?;
                }
            }
        }
        Ok(Some(control))
    }

    fn require_running(&mut self) -> io::Result<()> {
        if !self.identity.as_ref().unwrap().alive()? {
            return Err(io::Error::other(
                "managed recorder exited before canonical recovery",
            ));
        }
        Ok(())
    }

    pub(super) fn stop(&mut self) -> io::Result<()> {
        if self.stopped {
            return Ok(());
        }
        let started = Instant::now();
        let before = cleanup_cost();
        let request = self.stop_request();
        let cleanup = self.stop_cleanup(request.is_ok());
        eprintln!(
            "managed_cleanup_only wall_ns={} inventories={} inventoried={} uid_buffer_bytes=16388 \
            identity_capacity=64 before={before:?} after={:?}",
            started.elapsed().as_nanos(),
            self.inventories,
            self.inventoried,
            cleanup_cost()
        );
        match (request, cleanup) {
            (Ok(()), Ok(())) => Ok(()),
            (Err(original), Err(cleanup)) => Err(io::Error::new(
                original.kind(),
                format!("{original}; native/ring cleanup: {cleanup}"),
            )),
            (Err(original), Ok(())) => Err(original),
            (Ok(()), Err(cleanup)) => Err(cleanup),
        }
    }

    fn stop_request(&mut self) -> io::Result<()> {
        if self.child.is_none() {
            return Ok(());
        }
        if let Some(control) = self.observe()? {
            let writable = Control::open_writable(&self.root)?;
            if control.authority() != writable.authority() {
                return Err(io::Error::other("owned stop authority changed"));
            }
            writable.request_stop()?;
        }
        Ok(())
    }

    fn stop_cleanup(&mut self, graceful: bool) -> io::Result<()> {
        if let Some(child) = &mut self.child
            && self.identity.is_none()
        {
            child.kill()?;
            return Err(io::Error::other(
                "unreaped controller retained: native session anchor unavailable",
            ));
        }
        let cooperative = graceful
            && self
                .identity
                .as_ref()
                .map(native::Process::alive)
                .transpose()?
                .unwrap_or(false);
        if !cooperative || !self.wait_stopped(500)? {
            self.stop_boundary();
            self.stop_forced()?;
        }
        if !self.wait_stopped(1000)? {
            return Err(io::Error::other(
                "owned managed descendants survived bounded cleanup",
            ));
        }
        self.stop_rings()?;
        self.stopped = true;
        Ok(())
    }

    pub(super) fn exit_status(&mut self) -> io::Result<Option<ExitStatus>> {
        if !self.stopped {
            return Err(io::Error::other(
                "controller wait requested before native/ring cleanup",
            ));
        }
        Ok(self.exit)
    }

    fn wait_stopped(&mut self, steps: usize) -> io::Result<bool> {
        assert!(steps <= 1000);
        for _step in 0..steps {
            let exited = self.child.is_none() || !self.identity.as_ref().unwrap().alive()?;
            let workers = exited && self.wait_members()?;
            let unlocked = !private::FileLock::held(&self.root.join("collector.lock"))?
                && !private::FileLock::held(&self.root.join("persister.lock"))?;
            if exited && workers && unlocked {
                if let Some(child) = &mut self.child {
                    let Some(status) = child.try_wait()? else {
                        std::thread::sleep(Duration::from_millis(10));
                        continue;
                    };
                    self.exit = Some(status);
                    self.child.take();
                    eprintln!("managed_controller_exit={status}");
                }
                return Ok(true);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(false)
    }

    fn wait_members(&mut self) -> io::Result<bool> {
        if self.child.is_none() {
            return Ok(true);
        }
        self.inventory()?;
        Ok(self.descendants.is_empty())
    }

    fn inventory(&mut self) -> io::Result<()> {
        let count = native::session(self.identity.as_ref().unwrap(), &mut self.descendants)?;
        self.inventories += 1;
        self.inventoried += u64::try_from(count).map_err(io::Error::other)?;
        Ok(())
    }

    /// Reports what kept the stop waiting, which the forced stop's SIGKILL otherwise hides. A
    /// read that fails is reported too, since cleanup must go on past corrupt control.
    fn stop_boundary(&self) {
        let roles = Control::open(&self.root).map(|control| {
            [Role::Controller, Role::Collector, Role::Persister]
                .map(|role| read_role(&control, role))
        });
        eprintln!("managed_forced_stop controller_collector_persister={roles:?}");
    }

    fn stop_forced(&mut self) -> io::Result<()> {
        if self.child.is_some() {
            let controller = *self.identity.as_ref().unwrap();
            controller.signal(17)?;
            for _step in 0..100 {
                if controller.stopped()? || !controller.alive()? {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            if controller.alive()? && !controller.stopped()? {
                return Err(io::Error::other(
                    "owned recorder did not stop for child inventory",
                ));
            }
            // A stopped controller cannot race another launch between inventory and kill.
            self.inventory()?;
            for worker in &self.descendants {
                if worker != &controller {
                    worker.signal_in_session(&controller, 9)?;
                }
            }
            controller.signal_in_session(&controller, 9)?;
        }
        Ok(())
    }

    fn stop_rings(&self) -> io::Result<()> {
        for run in Registry::read(&self.root)?.runs {
            if run.ring_name.starts_with(&self.place.name_prefix) {
                unlink(&run.ring_name)?;
            }
        }
        if self.place.lock_path.exists() {
            let name = fs::read_to_string(&self.place.lock_path)?;
            if !name.trim().starts_with(&self.place.name_prefix) {
                return Err(io::Error::other("foreign managed fixture ring"));
            }
            unlink(name.trim())?;
            fs::remove_file(&self.place.lock_path)?;
        }
        Ok(())
    }

    pub(super) fn report(&self) -> io::Result<()> {
        let mut output = String::new();
        File::open(self.root.join("managed.log"))?
            .take(1024 * 1024 + 1)
            .read_to_string(&mut output)?;
        if output.len() > 1024 * 1024 {
            return Err(io::Error::other("managed diagnostics bound"));
        }
        eprintln!("managed_diagnostics root={}\n{output}", self.root.display());
        Ok(())
    }
}

fn cleanup_cost() -> io::Result<collector::recording::child::ChildCost> {
    collector::recording::child::sample_process(
        i32::try_from(std::process::id()).map_err(io::Error::other)?,
        collector::clock::Timebase::read()?,
    )
}

fn read_role(control: &Control, role: Role) -> io::Result<Option<Health>> {
    match control.read(role) {
        Ok(health) => Ok(Some(health)),
        Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error),
    }
}

fn unlink(name: &str) -> io::Result<()> {
    match collector::ring::unlink_generation(name) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
