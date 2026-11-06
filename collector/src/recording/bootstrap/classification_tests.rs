use super::codec::{NamespaceState, SlotKind, encode_idle, encode_state};
use super::{Classification, classify, initialize};
use crate::recording::private::{self, FileLock, PrivateDirectory};
use crate::ring::Identity;
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

#[test]
fn classifier_retries_when_registered_busy_replaces_its_foreground_observation() {
    // Goal: valid writer progress cannot become a cross-file corruption verdict.
    // Method: publish Registered then Busy after the actual foreground read in a native child.
    verify_interleaving("runtime");
}

#[test]
fn classifier_retries_when_identity_publication_finishes_initialization() {
    // Goal: a bootstrap observation cannot survive identity publication and bootstrap removal.
    // Method: publish prebuilt identity and unlink bootstrap after its actual read.
    verify_interleaving("identity");
}

#[test]
fn classifier_retries_when_enumerated_pending_is_published() {
    // Goal: a pending name removed by publication cannot become stable corruption.
    // Method: rename pending after the kernel fills the actual root enumeration batch.
    verify_interleaving("pending");
}

#[test]
fn classifier_defers_a_mismatch_verdict_until_busy_observation_is_stable() {
    // Goal: a validation error from mixed observations cannot escape before stability checking.
    // Method: after reading Busy(A), publish Quiet then Registered(B) and Busy(B).
    verify_interleaving("runtime-error");
}

#[test]
fn classifier_retries_when_fresh_scratch_becomes_bootstrap_authority() {
    // Goal: a fresh observation cannot authorize a scaffold created under new bootstrap authority.
    // Method: publish bootstrap and one child after the actual bootstrap.pending read.
    verify_interleaving("bootstrap");
}

fn verify_interleaving(action: &str) {
    if let Some(base) = std::env::var_os("FLIGHTDECK_CLASSIFY_PROBE") {
        let base = Path::new(&base);
        let root = PrivateDirectory::open(&base.join("recording")).unwrap();
        let mut scratch = [0; 4097];
        let result = classify(&root, &mut scratch);
        assert_eq!(fs::read(base.join("observed")).unwrap(), b"x");
        assert!(
            matches!(result, Err(ref error) if error.kind() == io::ErrorKind::WouldBlock),
            "actual classifier result: {result:?}"
        );
        return;
    }
    let fixture = Fixture::new();
    let target = fixture.prepare(action);
    let library = fixture.base.join("classification.dylib");
    let build_log = File::create(fixture.base.join("build.log")).unwrap();
    let mut build = OwnedChild(
        Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
            .args(["--edition=2024", "--crate-type=cdylib", "--deny=warnings"])
            .arg(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("src/recording/bootstrap/classification_interpose.rs"),
            )
            .arg("-o")
            .arg(&library)
            .stdout(Stdio::from(build_log.try_clone().unwrap()))
            .stderr(Stdio::from(build_log))
            .spawn()
            .unwrap(),
    );
    assert!(
        build.wait().success(),
        "{}",
        fs::read_to_string(fixture.base.join("build.log")).unwrap()
    );
    let test_name = match action {
        "runtime" => "classifier_retries_when_registered_busy_replaces_its_foreground_observation",
        "runtime-error" => "classifier_defers_a_mismatch_verdict_until_busy_observation_is_stable",
        "bootstrap" => "classifier_retries_when_fresh_scratch_becomes_bootstrap_authority",
        "identity" => "classifier_retries_when_identity_publication_finishes_initialization",
        "pending" => "classifier_retries_when_enumerated_pending_is_published",
        _ => panic!("unknown classifier fixture"),
    };
    let log = File::create(fixture.base.join("child.log")).unwrap();
    let mut child = OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                &format!("recording::bootstrap::classification_tests::{test_name}"),
                "--no-capture",
                "--test-threads=1",
            ])
            .env("DYLD_INSERT_LIBRARIES", &library)
            .env("FLIGHTDECK_CLASSIFY_PROBE", &fixture.base)
            .env("FLIGHTDECK_CLASSIFY_TARGET", target)
            .env("FLIGHTDECK_CLASSIFY_ACTION", action)
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap(),
    );
    assert!(
        child.wait().success(),
        "{}",
        fs::read_to_string(fixture.base.join("child.log")).unwrap()
    );
}

static NEXT: AtomicU64 = AtomicU64::new(0);
struct Fixture {
    base: PathBuf,
    identity: Identity,
}
impl Fixture {
    fn new() -> Self {
        let base = std::env::temp_dir().join(format!(
            "fd-classification-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        private::create_private_dir(&base).unwrap();
        let base = base.canonicalize().unwrap();
        private::create_private_dir(&base.join("recording")).unwrap();
        let root = PrivateDirectory::open(&base.join("recording")).unwrap();
        let registration = FileLock::try_lock_in(&root, "registration.lock").unwrap();
        let identity = initialize(&root, &registration, &mut [0; 4097]).unwrap();
        fs::write(base.join("observed"), b"").unwrap();
        Self { base, identity }
    }
    fn prepare(&self, action: &str) -> PathBuf {
        if action == "runtime-error" {
            return self.prepare_runtime_error();
        }
        if action == "bootstrap" {
            return self.prepare_bootstrap();
        }
        if action == "runtime" {
            self.prepare_busy();
            return self.base.join("recording/foreground");
        }
        let root = PrivateDirectory::open(&self.base.join("recording")).unwrap();
        let mut bytes = [0; 64];
        super::codec::encode_bootstrap(self.identity, &mut bytes).unwrap();
        private::atomic_write_in(&root, "bootstrap", &bytes).unwrap();
        fs::rename(
            self.base.join("recording/identity"),
            self.base.join("next-identity"),
        )
        .unwrap();
        if action == "pending" {
            fs::rename(
                self.base.join("recording/foreground"),
                self.base.join("recording/foreground.pending"),
            )
            .unwrap();
            return self.base.join("recording");
        }
        assert_eq!(action, "identity");
        self.base.join("recording/bootstrap")
    }
    fn prepare_runtime_error(&self) -> PathBuf {
        let root = PrivateDirectory::open(&self.base.join("recording")).unwrap();
        let mut foreground = [0; 4096];
        occupied(self.identity, 6, &mut foreground);
        private::atomic_write_in(&root, "foreground", &foreground).unwrap();
        let mut state = [0; 256];
        encode_state(
            self.identity,
            &NamespaceState::Busy {
                sequence: 1,
                transaction: Identity([0, 6]),
            },
            &mut state,
        )
        .unwrap();
        private::atomic_write_in(&root, "namespace-state", &state).unwrap();
        encode_state(
            self.identity,
            &NamespaceState::Quiet { sequence: 2 },
            &mut state,
        )
        .unwrap();
        private::atomic_write(&self.base, "next-quiet", &state).unwrap();
        self.prepare_busy();
        encode_state(
            self.identity,
            &NamespaceState::Busy {
                sequence: 3,
                transaction: Identity([0, 7]),
            },
            &mut state,
        )
        .unwrap();
        private::atomic_write(&self.base, "next-namespace-state", &state).unwrap();
        self.base.join("recording/namespace-state")
    }
    fn prepare_bootstrap(&self) -> PathBuf {
        let recording = self.base.join("recording");
        for name in [
            "data",
            "recovery-data",
            "ordinary",
            "observations-known",
            "observations-unknown",
            "terminal",
            "event-fragments",
            "retention",
        ] {
            fs::remove_dir(recording.join(name)).unwrap();
        }
        for name in ["foreground", "maintenance", "namespace-state", "identity"] {
            fs::remove_file(recording.join(name)).unwrap();
        }
        let mut bootstrap = [0; 64];
        super::codec::encode_bootstrap(self.identity, &mut bootstrap).unwrap();
        private::atomic_write(&recording, "bootstrap.pending", &bootstrap).unwrap();
        private::create_private_dir(&self.base.join("next-ordinary")).unwrap();
        recording.join("bootstrap.pending")
    }
    fn prepare_busy(&self) {
        let mut foreground = [0; 4096];
        occupied(self.identity, 7, &mut foreground);
        private::atomic_write(&self.base, "next-foreground", &foreground).unwrap();
        let mut state = [0; 256];
        encode_state(
            self.identity,
            &NamespaceState::Busy {
                sequence: 1,
                transaction: Identity([0, 7]),
            },
            &mut state,
        )
        .unwrap();
        private::atomic_write(&self.base, "next-namespace-state", &state).unwrap();
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.base).unwrap();
    }
}
fn occupied(identity: Identity, transaction: u8, bytes: &mut [u8; 4096]) {
    encode_idle(identity, SlotKind::Foreground, bytes).unwrap();
    bytes[47] = transaction;
    bytes[51] = 1;
    bytes[55] = 1;
    bytes[56..60].copy_from_slice(&2048_u32.to_be_bytes());
}
struct OwnedChild(Child);
impl OwnedChild {
    fn wait(&mut self) -> ExitStatus {
        let started = Instant::now();
        for _ in 0..3000 {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "classification child exceeded deadline"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("classification child exhausted wait bound");
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
            self.0.wait().unwrap();
        }
    }
}

#[test]
fn classifier_admits_stable_matching_busy_without_execution_authority() {
    // Goal: an occupied foreground and matching Busy remain structurally admissible.
    // Method: classify real immutable controls while counting collector allocations.
    verify_stable_busy(Some(7), 7, false);
}

#[test]
fn classifier_refuses_stable_busy_with_wrong_transaction() {
    // Goal: structural admission retains the foreground owner comparison.
    // Method: publish distinct occupied and Busy identities through real controls.
    verify_stable_busy(Some(7), 8, true);
}

#[test]
fn classifier_refuses_stable_busy_with_idle_foreground() {
    // Goal: Busy cannot name a transaction absent from the foreground slot.
    // Method: publish Busy alongside the initialized Idle and classify the root.
    verify_stable_busy(None, 7, true);
}

fn verify_stable_busy(foreground_transaction: Option<u8>, busy_transaction: u8, corrupt: bool) {
    let fixture = Fixture::new();
    let root = PrivateDirectory::open(&fixture.base.join("recording")).unwrap();
    if let Some(transaction) = foreground_transaction {
        let mut bytes = [0; 4096];
        occupied(fixture.identity, transaction, &mut bytes);
        private::atomic_write_in(&root, "foreground", &bytes).unwrap();
    }
    let mut state = [0; 256];
    encode_state(
        fixture.identity,
        &NamespaceState::Busy {
            sequence: 1,
            transaction: Identity([0, u64::from(busy_transaction)]),
        },
        &mut state,
    )
    .unwrap();
    private::atomic_write_in(&root, "namespace-state", &state).unwrap();
    let mut scratch = [0; 4097];
    let calls = crate::allocator_count::allocator_calls_in(|| {
        let result = classify(&root, &mut scratch).unwrap();
        if corrupt {
            assert_eq!(result, Classification::Corrupt);
        } else {
            assert_eq!(result, Classification::Initialized(fixture.identity));
        }
    });
    assert_eq!(calls, 0);
}
