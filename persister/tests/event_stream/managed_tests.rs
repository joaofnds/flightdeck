use super::super::Candidate;
use super::*;
use std::os::unix::fs::PermissionsExt;

#[test]
#[ignore = "opt-in native orphaned startup-worker cleanup regression"]
fn cleanup_finds_an_unreported_worker_after_its_controller_dies() {
    // Goal: cleanup owns a worker born before its first health/lock even after parent death.
    // Method: hold the real persister before main, kill its recorder, then inspect actual teardown.
    let mut candidate = Candidate::new(8);
    candidate.publish(8);
    let fault = Fault::new(&candidate.root);
    let startup = fault.hold("startup");
    candidate.managed = Some(recorder(&candidate));
    let recorder = candidate.managed.as_mut().unwrap();
    fs::rename(
        recorder.helpers.join("persister.withheld"),
        recorder.helpers.join("persister"),
    )
    .unwrap();
    recorder
        .launch_with(|command| fault.configure(command, Some(&startup), None))
        .unwrap();
    let mut owned = Vec::with_capacity(64);

    let observed = (|| -> io::Result<bool> {
        let worker = wait_startup(recorder, &fault, &mut owned)?;
        let controller = *recorder.identity.as_ref().unwrap();
        assert_eq!(controller.session()?, Some(controller.pid));
        assert_eq!(worker.session()?, Some(controller.pid));
        assert_eq!(worker.group()?, Some(worker.pid.cast_unsigned()));
        let control = wait_control(recorder)?;
        if control.read(Role::Persister)?.pid != 0
            || private::FileLock::held(&candidate.root.join("persister.lock"))?
        {
            return Err(io::Error::other(
                "startup injection reached health or role ownership",
            ));
        }
        controller.signal(9)?;
        wait_gone(&controller)?;
        controller.anchor()?;
        let mut orphaned = Vec::with_capacity(64);
        let inventoried = native::session(&controller, &mut orphaned)?;
        assert!(orphaned.contains(&worker));
        eprintln!(
            "private_session anchor={controller:?} worker={worker:?} uid_inventory={inventoried} \
            orphan_members={orphaned:?}"
        );
        let stopped = recorder.stop();
        eprintln!("orphan_cleanup result={stopped:?} worker={worker:?}");
        if stopped.is_ok() {
            assert!(controller.anchor().is_err());
        }
        Ok(stopped.is_ok() && !worker.alive()?)
    })();

    emergency(recorder, &owned);
    drop(startup);
    assert!(observed.unwrap());
}

#[test]
#[ignore = "opt-in native control-read refusal cleanup regression"]
fn cleanup_reports_control_corruption_after_stopping_its_owned_processes() {
    // Goal: corrupt control bytes cannot suppress independent cleanup of a known native owner.
    // Method: replace only the owned control path, call stop, then verify its process has exited.
    let mut candidate = Candidate::new(8);
    candidate.publish(8);
    candidate.managed = Some(recorder(&candidate));
    let recorder = candidate.managed.as_mut().unwrap();
    recorder.launch().unwrap();
    let root = candidate.root.clone();
    let backup = root.join("control.backup");

    let observed = (|| -> io::Result<bool> {
        drop(wait_control(recorder)?);
        fs::rename(root.join("recorder.control"), &backup)?;
        fs::write(root.join("recorder.control"), b"invalid owned control")?;
        fs::set_permissions(
            root.join("recorder.control"),
            fs::Permissions::from_mode(0o600),
        )?;
        let error = recorder
            .stop()
            .expect_err("control corruption remains reported");
        eprintln!("control_cleanup error={error}");
        Ok(error.kind() == io::ErrorKind::InvalidData
            && !recorder.identity.as_ref().unwrap().alive()?)
    })();

    if backup.exists() {
        fs::rename(&backup, root.join("recorder.control")).unwrap();
    }
    emergency(recorder, &[]);
    assert!(observed.unwrap());
}

#[test]
#[ignore = "opt-in native receipt-visible final-sync timing regression"]
fn recovery_timer_waits_for_the_matching_workers_final_retirement_sync() {
    // Goal: receipt visibility alone cannot finish a durable recovery timing observation.
    // Method: hold the exact retired-directory full sync, observe its receipt, then release it.
    let mut candidate = Candidate::new(8);
    candidate.publish(8);
    candidate.withhold_publications(8);
    let fault = Fault::new(&candidate.root);
    let hold = fault.hold("retired-sync");
    candidate.managed = Some(recorder(&candidate));
    let recorder = candidate.managed.as_mut().unwrap();
    recorder
        .launch_with(|command| fault.configure(command, None, Some(&hold)))
        .unwrap();
    let root = candidate.root.clone();
    let run = candidate.run;
    let source = candidate.source.clone();

    let result = std::thread::scope(|scope| {
        let recovery = scope.spawn(|| recorder.recover(&run, &source));
        let entered = wait_marker(&fault.root.join("retired-sync-observed"));
        let visible = Registry::completed(&root, &run, &source);
        let mut premature = false;
        for _step in 0..50 {
            if recovery.is_finished() {
                premature = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let released = control::now().unwrap();
        drop(hold);
        (
            entered,
            visible,
            premature,
            released,
            recovery.join().unwrap(),
        )
    });

    emergency(recorder, &[]);
    let (entered, visible, premature, released, recovery) = result;
    entered.unwrap();
    assert!(visible.unwrap());
    let recovery = recovery.unwrap();
    eprintln!(
        "retirement_sync receipt_visible=true premature={premature} released_ns={released} \
        canonical_ns={} worker={:?}",
        recovery.canonical_ns, recovery.worker
    );
    candidate.verify_terminal(8);
    assert!(!premature);
    assert!(recovery.canonical_ns >= released);
}

struct Fault {
    root: PathBuf,
    library: PathBuf,
}
struct Hold(PathBuf);

impl Fault {
    fn new(root: &Path) -> Self {
        private::create_private_dir(&root.join("retired")).unwrap();
        let library = root.join("sync-fault.dylib");
        let diagnostics = root.join("fault-build.log");
        eprintln!(
            "managed_fault_compile edition=2024 crate_type=cdylib \
            source={}/tests/faults/sync.rs output={}",
            env!("CARGO_MANIFEST_DIR"),
            library.display()
        );
        let log = File::create(&diagnostics).unwrap();
        let mut compiler =
            Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
                .args(["--edition=2024", "--crate-type=cdylib"])
                .arg(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/faults/sync.rs"))
                .arg("-o")
                .arg(&library)
                .stdout(log.try_clone().unwrap())
                .stderr(log)
                .spawn()
                .unwrap();
        let mut status = None;
        for _step in 0..3000 {
            status = compiler.try_wait().unwrap();
            if status.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        if status.is_none() {
            compiler.kill().unwrap();
            status = Some(compiler.wait().unwrap());
        }
        let mut output = String::new();
        File::open(diagnostics)
            .unwrap()
            .take(1024 * 1024 + 1)
            .read_to_string(&mut output)
            .unwrap();
        assert!(output.len() <= 1024 * 1024);
        assert!(status.unwrap().success(), "{output}");
        Self {
            root: root.canonicalize().unwrap(),
            library: library.canonicalize().unwrap(),
        }
    }

    fn hold(&self, name: &str) -> Hold {
        let path = self.root.join(format!("hold-{name}"));
        fs::write(&path, b"hold").unwrap();
        Hold(path)
    }

    fn configure(&self, command: &mut Command, startup: Option<&Hold>, retired: Option<&Hold>) {
        command.env("DYLD_INSERT_LIBRARIES", &self.library);
        if let Some(hold) = startup {
            command.env("FLIGHTDECK_TEST_HOLD_STARTUP", &hold.0).env(
                "FLIGHTDECK_TEST_STARTUP_OBSERVED",
                self.root.join("startup-observed"),
            );
        }
        if let Some(hold) = retired {
            command
                .env("FLIGHTDECK_TEST_HOLD_RETIRED_SYNC", &hold.0)
                .env(
                    "FLIGHTDECK_TEST_RETIRED_DIRECTORY",
                    self.root.join("retired"),
                )
                .env(
                    "FLIGHTDECK_TEST_RETIRED_SYNC_OBSERVED",
                    self.root.join("retired-sync-observed"),
                );
        }
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.0) {
            assert_eq!(error.kind(), io::ErrorKind::NotFound);
        }
    }
}

fn wait_marker(path: &Path) -> io::Result<()> {
    for _step in 0..3000 {
        match path.metadata() {
            Ok(metadata) if metadata.len() > 0 => return Ok(()),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(io::Error::other("owned fault marker deadline"))
}

fn wait_startup(
    recorder: &Recorder,
    fault: &Fault,
    owned: &mut Vec<native::Process>,
) -> io::Result<native::Process> {
    let marker = fault.root.join("startup-observed");
    wait_marker(&marker)?;
    let mut bytes = [0; 4];
    File::open(marker)?.read_exact(&mut bytes)?;
    let pid = i32::from_ne_bytes(bytes);
    native::children(recorder.identity.as_ref().unwrap(), owned)?;
    owned
        .iter()
        .find(|process| process.pid == pid)
        .copied()
        .ok_or_else(|| io::Error::other("startup worker is not a verified owned child"))
}

fn wait_control(recorder: &mut Recorder) -> io::Result<Control> {
    for _step in 0..1000 {
        if let Some(control) = recorder.observe()? {
            return Ok(control);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(io::Error::other("owned controller startup deadline"))
}

fn wait_gone(process: &native::Process) -> io::Result<()> {
    for _step in 0..1000 {
        if !process.alive()? {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(io::Error::other("owned process exit deadline"))
}

fn emergency(recorder: &mut Recorder, known: &[native::Process]) {
    // Failed red probes still own their original native identities; cleanup precedes assertions.
    let mut owned = Vec::with_capacity(64);
    if let Some(controller) = &recorder.identity {
        controller.signal(17).unwrap();
        for _step in 0..1000 {
            if !controller.alive().unwrap() || controller.stopped().unwrap() {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        if controller.alive().unwrap() {
            assert!(controller.stopped().unwrap());
        }
        native::children(controller, &mut owned).unwrap();
    }
    for worker in owned.iter().chain(known) {
        worker.signal(9).unwrap();
        wait_gone(worker).unwrap();
    }
    if let Some(controller) = &recorder.identity {
        controller.signal(9).unwrap();
        wait_gone(controller).unwrap();
    }
    if let Some(child) = &mut recorder.child {
        for _step in 0..1000 {
            if let Some(status) = child.try_wait().unwrap() {
                recorder.exit = Some(status);
                recorder.child.take();
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("owned red-probe controller did not reap");
    }
}

#[test]
fn ring_cleanup_retries_after_the_registry_is_repaired() {
    // Goal: a fallible ring release is not remembered as completed process-and-ring cleanup.
    // Method: corrupt the owned registry, repair it after refusal,
    // and verify the real ring unlinks.
    let mut candidate = Candidate::new(8);
    candidate.writer.as_mut().unwrap().finish();
    drop(candidate.writer.take());
    let mut recorder = recorder(&candidate);
    let name = managed_ring(&recorder);
    let registry = fs::read(candidate.root.join("runs")).unwrap();
    fs::write(candidate.root.join("runs"), b"invalid registry").unwrap();

    let first = recorder.stop();
    fs::write(candidate.root.join("runs"), registry).unwrap();
    let second = recorder.stop();
    let removed = !recorder.place.lock_path.exists();
    eprintln!("ring_cleanup first={first:?} second={second:?} removed={removed}");

    release_ring(&recorder.place, &name);
    assert!(first.is_err());
    assert!(second.is_ok());
    assert!(removed);
}

#[test]
fn persistent_ring_cleanup_failure_keeps_the_owned_recording() {
    // Goal: repeated teardown refusal keeps the evidence that names unresolved owned resources.
    // Method: leave a malformed registry through explicit stop and Candidate Drop, then inspect it.
    let mut candidate = Candidate::new(8);
    candidate.writer.as_mut().unwrap().finish();
    drop(candidate.writer.take());
    let recorder = recorder(&candidate);
    let name = managed_ring(&recorder);
    let place = Place {
        lock_path: recorder.place.lock_path.clone(),
        name_prefix: recorder.place.name_prefix.clone(),
    };
    let root = candidate.root.clone();
    let sources = candidate.sources.clone();
    candidate.managed = Some(recorder);
    fs::write(root.join("runs"), b"persistent invalid registry").unwrap();

    let first = candidate.managed.as_mut().unwrap().stop();
    drop(candidate);
    let retained = root.exists();
    let lock_retained = place.lock_path.exists();

    release_ring(&place, &name);
    for source in sources {
        unlink(&source).unwrap();
    }
    if root.exists() {
        fs::remove_dir_all(root).unwrap();
    }
    assert!(first.is_err());
    assert!(retained);
    assert!(lock_retained);
}

fn recorder(candidate: &Candidate) -> Recorder {
    Recorder::new(
        &candidate.root,
        &PathBuf::from(env!("CARGO_BIN_EXE_persister")),
    )
    .unwrap()
}

fn managed_ring(recorder: &Recorder) -> String {
    let mut writer = collector::ring::Writer::create(
        &recorder.place,
        collector::ring::Shape {
            slot_count: 2,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 8,
        },
    )
    .unwrap();
    let name = writer.name().to_owned();
    writer.finish();
    drop(writer);
    name
}

fn release_ring(place: &Place, name: &str) {
    unlink(name).unwrap();
    if place.lock_path.exists() {
        fs::remove_file(&place.lock_path).unwrap();
    }
}
