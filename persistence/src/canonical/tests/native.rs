use super::*;
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

#[test]
fn native_inflight_state_replacement_overrides_absence_and_raw_error() {
    // Goal: state changes after entry admission dominate absence and selected raw failure.
    // Method: synchronously rename a prepared state inode inside native enumeration/read.
    if let Some(root) = std::env::var_os("FLIGHTDECK_STATE_ROOT") {
        child(&PathBuf::from(root));
        return;
    }
    for raw in [false, true] {
        let fixture = Fixture::new();
        let trigger = if raw {
            let metadata = fixture.metadata(CanonicalKind::LossOnly);
            fixture.root.join("ordinary").join(
                fixture
                    .publish(&metadata, Namespace::Ordinary, &[])
                    .1
                    .file_name(),
            )
        } else {
            fixture.root.join("ordinary")
        };
        let stage = fixture.base.join("state-stage");
        let mut bytes = [0; 256];
        bootstrap::codec::encode_state(
            fixture.run.recording,
            &bootstrap::codec::NamespaceState::Quiet { sequence: 1 },
            &mut bytes,
        )
        .unwrap();
        private::open_private_file(&stage, true)
            .unwrap()
            .write_all(&bytes)
            .unwrap();
        let library = build(&fixture);
        let observed = fixture.base.join("observed");
        fs::write(&observed, b"").unwrap();
        let armed = fixture.base.join("armed");
        fs::write(&armed, b"armed").unwrap();
        let mut command = Command::new(std::env::current_exe().unwrap());
        let test = concat!(
            "canonical::tests::native::",
            "native_inflight_state_replacement_overrides_absence_and_raw_error"
        );
        command
            .args(["--exact", test, "--nocapture"])
            .env("DYLD_INSERT_LIBRARIES", library)
            .env("FLIGHTDECK_STATE_ROOT", &fixture.root)
            .env("FLIGHTDECK_STATE_STAGE", &stage)
            .env(
                "FLIGHTDECK_STATE_TARGET",
                fixture.root.join("namespace-state"),
            )
            .env("FLIGHTDECK_STATE_TRIGGER", trigger.canonicalize().unwrap())
            .env("FLIGHTDECK_REGISTRATION_ARMED", armed)
            .env("FLIGHTDECK_PRIVATE_SYNC_OBSERVED", observed);
        if raw {
            command.env(
                "FLIGHTDECK_INVENTORY_READ_TARGET",
                trigger.canonicalize().unwrap(),
            );
        }
        assert!(Owned(command.spawn().unwrap()).wait().success());
        assert!(!stage.exists());
    }
}
fn child(path: &std::path::Path) {
    let store = crate::Store::open(path).unwrap();
    let run = RunIdentity {
        recording: store.recording(),
        run: Identity([8, 9]),
        boot: Identity([10, 11]),
    };
    let root = PrivateDirectory::open(path).unwrap();
    let mut state = [0; 256];
    let memory = budget();
    let error = if std::env::var_os("FLIGHTDECK_INVENTORY_READ_TARGET").is_some() {
        let mut query = SourceFrontierRead::begin(&root, run, &mut state, &memory).unwrap();
        query.step().unwrap_err()
    } else {
        let mut cursor = CanonicalCursor::begin(
            &root,
            run.recording,
            Namespace::Ordinary,
            SeekRange::source_tail(Namespace::Ordinary, &run).unwrap(),
            &mut state,
            &memory,
        )
        .unwrap();
        cursor.step().unwrap_err()
    };
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
}
fn build(fixture: &Fixture) -> PathBuf {
    let library = fixture.base.join("state.dylib");
    let log = fs::File::create(fixture.base.join("compiler.log")).unwrap();
    let status = Owned(
        Command::new("rustc")
            .args(["--edition=2024", "--crate-type=cdylib", "--deny=warnings"])
            .arg(
                PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("src/store/registration_sync_fault.rs"),
            )
            .arg("-o")
            .arg(&library)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .spawn()
            .unwrap(),
    )
    .wait();
    assert!(
        status.success(),
        "{}",
        fs::read_to_string(fixture.base.join("compiler.log")).unwrap()
    );
    library
}
struct Owned(Child);
impl Owned {
    fn wait(&mut self) -> ExitStatus {
        let start = Instant::now();
        for _ in 0..3000 {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(start.elapsed() < Duration::from_secs(30));
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("native child deadline");
    }
}
impl Drop for Owned {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
            self.0.wait().unwrap();
        }
    }
}
