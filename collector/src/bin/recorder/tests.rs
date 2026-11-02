use super::*;

#[path = "../../../tests/support/process.rs"]
mod process;

#[test]
fn replacement_waits_for_the_previous_owned_persister_to_exit() {
    // Goal: a completed receipt cannot assign an old worker's exit to new acquisition.
    // Method: keep a native child alive after role release and observe normal paced steps.
    let root = DrainRoot::new("owned-release");
    let current = root.register_empty();
    recording::Registry::retire(&root.0, &current).unwrap();
    let mut fixture = OwnedSupervisor::new(&root);

    let waiting = fixture.inner.step();
    let before_exit = fixture.inner.reporter.control().read(Role::Controller);
    drop(
        fixture
            .inner
            .persistence
            .child
            .as_mut()
            .unwrap()
            .stdin
            .take(),
    );
    let resumed = fixture.wait_for_replacement(Duration::from_secs(2));

    assert!(!waiting.unwrap());
    assert_eq!(before_exit.unwrap().failure, Failure::None);
    assert_eq!(resumed.unwrap().failure, Failure::Launch);
}

#[test]
fn a_stalled_owned_worker_has_bounded_wait_and_cleanup() {
    // Goal: failed transition probes cannot hang or leak their native worker.
    // Method: leave the input open, reach the bounded refusal, then inspect native liveness.
    let root = DrainRoot::new("owned-stalled");
    let current = root.register_empty();
    recording::Registry::retire(&root.0, &current).unwrap();
    let mut fixture = OwnedSupervisor::new(&root);
    let health = fixture.child_health();

    let result = fixture.wait_for_replacement(Duration::from_millis(20));
    drop(fixture);

    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::TimedOut);
    assert!(!health.alive().unwrap());
}

struct OwnedSupervisor {
    inner: Supervisor,
}

impl OwnedSupervisor {
    fn new(root: &DrainRoot) -> Self {
        let mut inner = root.supervisor();
        inner.persistence.child = Some(
            Command::new("/bin/cat")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        Self { inner }
    }

    fn child_health(&self) -> Health {
        let pid = i32::try_from(self.inner.persistence.child.as_ref().unwrap().id()).unwrap();
        let mut creation = Err(collector::programs::CreationError::NotRead);
        collector::programs::read_creation(pid, false, &mut creation);
        Health {
            pid,
            creation: creation.unwrap(),
            ..Health::default()
        }
    }

    fn wait_for_replacement(&mut self, limit: Duration) -> io::Result<Health> {
        assert!(limit <= Duration::from_secs(2));
        let started = std::time::Instant::now();
        for _attempt in 0..2000 {
            if self.inner.step()? {
                return Err(io::Error::other("controller ended before replacement"));
            }
            let health = self.inner.reporter.control().read(Role::Controller)?;
            if health.failure == Failure::Launch {
                return Ok(health);
            }
            if started.elapsed() >= limit {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        Err(io::Error::from(io::ErrorKind::TimedOut))
    }
}

impl Drop for OwnedSupervisor {
    fn drop(&mut self) {
        for worker in [&mut self.inner.acquisition, &mut self.inner.persistence] {
            if let Some(child) = worker.child.take() {
                drop(process::OwnedChild { child });
            }
        }
    }
}

#[test]
fn replacement_waits_for_the_previous_adopted_persister_to_exit() {
    // Goal: a relaunched controller retains the same release-to-exit barrier for adoptees.
    // Method: watch a native child through its actual exit before the next paced step.
    let root = DrainRoot::new("adopted-release");
    let current = root.register_empty();
    recording::Registry::retire(&root.0, &current).unwrap();
    let mut supervisor = root.supervisor();
    let mut child = process::OwnedChild::spawn(
        Command::new("/bin/cat")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    );
    let pid = i32::try_from(child.child.id()).unwrap();
    let mut creation = Err(collector::programs::CreationError::NotRead);
    collector::programs::read_creation(pid, false, &mut creation);
    let health = Health {
        pid,
        creation: creation.unwrap(),
        ..Health::default()
    };
    supervisor.persistence.watch(&health, true).unwrap();

    let waiting = supervisor.step();
    let before_exit = supervisor.reporter.control().read(Role::Controller);
    drop(child.child.stdin.take());
    let exited = child.wait(Duration::from_secs(2));
    let after_exit = supervisor.step();

    assert!(!waiting.unwrap());
    assert_eq!(before_exit.unwrap().failure, Failure::None);
    assert!(exited.success());
    assert!(!after_exit.unwrap());
    assert_eq!(
        supervisor
            .reporter
            .control()
            .read(Role::Controller)
            .unwrap()
            .failure,
        Failure::Launch
    );
}

#[test]
fn replacement_waits_for_persister_ownership_without_readable_health() {
    // Goal: missing health cannot bypass independent persister ownership on controller relaunch.
    // Method: hold the real role lock beside a completed receipt and absent role health.
    let root = DrainRoot::new("unreported-release");
    let current = root.register_empty();
    recording::Registry::retire(&root.0, &current).unwrap();
    let mut supervisor = root.supervisor();
    let ownership = FileLock::try_lock(&root.0.join("persister.lock")).unwrap();

    let waiting = supervisor.step();
    let before_release = supervisor.reporter.control().read(Role::Controller);
    drop(ownership);
    let after_release = supervisor.step();

    assert!(!waiting.unwrap());
    assert_eq!(before_release.unwrap().failure, Failure::None);
    assert!(!after_release.unwrap());
    assert_eq!(
        supervisor
            .reporter
            .control()
            .read(Role::Controller)
            .unwrap()
            .failure,
        Failure::Launch
    );
}

#[test]
fn an_unretired_closed_generation_prevents_replacement_acquisition() {
    // Goal: a collector death before live observation cannot replace its recoverable source.
    // Method: offer a real closed generation through normal advance before and after retirement.
    let root = DrainRoot::new("closed-generation");
    let identity = root.register_empty();
    let mut supervisor = root.supervisor();
    let mut step = DrainRoot::step(identity.run);
    step.stopping = false;
    step.collector = None;
    step.persister = None;
    assert!(
        !std::env::current_exe()
            .unwrap()
            .with_file_name("collector")
            .exists()
    );

    assert!(!supervisor.advance(&step).unwrap());

    assert_eq!(supervisor.acquisition.attempts, 0);
    recording::Registry::retire(&root.0, &identity).unwrap();
    assert!(!supervisor.advance(&step).unwrap());
    assert_eq!(supervisor.acquisition.attempts, 1);
}

#[test]
fn a_completed_current_generation_does_not_wait_for_an_unrelated_older_receipt() {
    // Goal: unrelated old unavailable history cannot block a retired current generation.
    // Method: leave the older source unretired and advance after the current source completes.
    let root = DrainRoot::new("completed-current");
    root.register_empty();
    let current = root.register_empty();
    recording::Registry::retire(&root.0, &current).unwrap();
    let mut supervisor = root.supervisor();
    let mut step = DrainRoot::step(current.run);
    step.stopping = false;
    step.collector = None;
    step.persister = None;
    assert!(
        !std::env::current_exe()
            .unwrap()
            .with_file_name("collector")
            .exists()
    );

    assert!(!supervisor.advance(&step).unwrap());

    assert_eq!(supervisor.acquisition.attempts, 1);
}

#[test]
fn a_previous_stopped_persister_cannot_complete_a_new_registered_run() {
    // Goal: stopping between replacement registration and persister launch drains the new run.
    // Method: retain the old stopped health while an actual zero-publication run lacks its receipt.
    let root = DrainRoot::new("replacement");
    let first = root.register_empty();
    recording::Registry::retire(&root.0, &first).unwrap();
    let second = root.register_empty();
    let mut supervisor = root.supervisor();
    let step = DrainRoot::step(first.run);

    assert!(!supervisor.drain(&step).unwrap());
    assert_eq!(
        supervisor
            .reporter
            .control()
            .read(Role::Controller)
            .unwrap()
            .state,
        State::Stopping
    );
    recording::Registry::retire(&root.0, &second).unwrap();
    assert!(supervisor.drain(&step).unwrap());
}

#[test]
fn latest_completed_run_does_not_require_an_unavailable_older_run() {
    // Goal: an unrelated unavailable old source cannot invalidate the current drain receipt.
    // Method: leave the first empty source unretired and retire only its registered replacement.
    let root = DrainRoot::new("unavailable");
    let first = root.register_empty();
    let second = root.register_empty();
    recording::Registry::retire(&root.0, &second).unwrap();
    let registry = recording::Registry::read(&root.0).unwrap();
    assert!(!recording::Registry::completed(&root.0, &first, &registry.runs[0].ring_name).unwrap());
    let mut supervisor = root.supervisor();
    let mut step = DrainRoot::step(first.run);
    step.persister = None;

    assert!(supervisor.drain(&step).unwrap());
}

#[test]
fn a_collector_registering_its_replacement_prevents_final_receipt_read() {
    // Goal: a completed old run cannot end a controller while another registration is in flight.
    // Method: retain collector ownership and remove the registry while the old receipt exists.
    let root = DrainRoot::new("registering");
    let first = root.register_empty();
    recording::Registry::retire(&root.0, &first).unwrap();
    let mut supervisor = root.supervisor();
    let mut step = DrainRoot::step(first.run);
    step.locks.collector = true;
    std::fs::rename(root.0.join("runs"), root.0.join("runs.saved")).unwrap();
    std::fs::create_dir(root.0.join("runs")).unwrap();

    assert!(!supervisor.drain(&step).unwrap());
}

struct DrainRoot(PathBuf);

impl DrainRoot {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("fd-drain-{}-{name}", std::process::id()));
        private::create_private_dir(&root).unwrap();
        Self(root)
    }

    fn register_empty(&self) -> collector::ring::RunIdentity {
        let place = Place {
            lock_path: self.0.join("ring.lock"),
            name_prefix: "/fd.".to_owned(),
        };
        let shape = collector::ring::Shape {
            slot_count: 2,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 2,
        };
        let (mut writer, registered) = recording::Registration::start(&self.0, &place, shape, 0)
            .unwrap()
            .into_parts();
        registered.unwrap();
        let identity = writer.identity().unwrap();
        assert_eq!(writer.publication_newest(), 0);
        writer.finish();
        identity
    }

    fn supervisor(&self) -> Supervisor {
        let reporter = Reporter::new(Control::create(&self.0).unwrap(), Role::Controller).unwrap();
        let mut persistence = Worker::new();
        persistence.due = u64::MAX;
        Supervisor {
            options: Options {
                root: self.0.clone(),
                ring: "unused".to_owned(),
                seconds: None,
                rescan: 0,
                memory: 256 * 1024 * 1024,
                stop: false,
                parent: None,
            },
            reporter,
            acquisition: Worker::new(),
            persistence,
            finished: false,
            costs: resources::Accounting::new().unwrap(),
            owner: owner::Owner::bind(None).unwrap(),
        }
    }

    fn step(previous: Identity) -> Step {
        let health = Health {
            run: previous,
            state: State::Stopped,
            ..Health::default()
        };
        Step {
            now: 0,
            collector: None,
            persister: Some(health),
            collecting: false,
            persisting: false,
            locks: Locks {
                collector: false,
                persister: false,
            },
            stopping: true,
        }
    }
}

impl Drop for DrainRoot {
    fn drop(&mut self) {
        if self.0.join("runs").is_dir() {
            std::fs::remove_dir(self.0.join("runs")).unwrap();
            std::fs::rename(self.0.join("runs.saved"), self.0.join("runs")).unwrap();
        }
        for run in recording::Registry::read(&self.0).unwrap().runs {
            collector::ring::unlink_generation(&run.ring_name).unwrap();
        }
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn a_worker_disappearing_before_adoption_keeps_the_next_step_available() {
    // Goal: an exit between the caller's alive sample and registration cannot kill the controller.
    // Method: pass a formerly live observation whose native identity no longer matches.
    let mut health = Health::own().unwrap();
    health.creation.unique_id ^= 1;
    let mut worker = Worker::new();
    worker.watch(&health, true).unwrap();
    assert!(worker.idle());
    assert!(
        worker
            .reap(collector::clock::Timebase::read().unwrap())
            .unwrap()
            .is_none()
    );
    assert_eq!(worker.attempts, 0);
    assert_eq!(worker.due, 0);
}

#[test]
fn adoption_disappearance_does_not_hide_unrelated_kernel_failures() {
    // Goal: only a missing native process is recoverable through the next normal step.
    // Method: pass Darwin ESRCH and unrelated failures through the registration result owner.
    let mut worker = Worker::new();
    worker
        .watch_register(Err(io::Error::from_raw_os_error(3)))
        .unwrap();
    assert!(worker.idle());
    for kind in [
        io::ErrorKind::PermissionDenied,
        io::ErrorKind::Interrupted,
        io::ErrorKind::InvalidData,
        io::ErrorKind::Other,
    ] {
        assert_eq!(
            worker
                .watch_register(Err(io::Error::from(kind)))
                .unwrap_err()
                .kind(),
            kind
        );
        assert!(worker.idle());
    }
}
