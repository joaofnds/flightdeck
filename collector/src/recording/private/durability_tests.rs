use super::*;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Stdio};

#[test]
fn refuses_pending_file_sync_without_publishing_its_bytes() {
    // Goal: failed file durability leaves the previous publication and no pending bytes.
    // Method: inject native EIO at this publication's pending-file full-sync in a child.
    verify_sync_failure(SyncBoundary::PendingFile);
}

#[test]
fn reports_directory_sync_failure_after_complete_replacement() {
    // Goal: visible replacement is not reported durable when its directory full-sync fails.
    // Method: inject native EIO at this publication's directory full-sync in a child.
    verify_sync_failure(SyncBoundary::Directory);
}

#[derive(Clone, Copy)]
enum SyncBoundary {
    PendingFile,
    Directory,
}

impl SyncBoundary {
    fn test_name(self) -> &'static str {
        match self {
            Self::PendingFile => concat!(
                "recording::private::durability_tests::",
                "refuses_pending_file_sync_without_publishing_its_bytes"
            ),
            Self::Directory => concat!(
                "recording::private::durability_tests::",
                "reports_directory_sync_failure_after_complete_replacement"
            ),
        }
    }

    fn target(self, root: &Path) -> PathBuf {
        match self {
            Self::PendingFile => root.join("metadata.pending"),
            Self::Directory => root.to_path_buf(),
        }
    }

    fn visible_bytes(self) -> &'static [u8] {
        match self {
            Self::PendingFile => b"prior",
            Self::Directory => b"replacement",
        }
    }
}

fn verify_sync_failure(boundary: SyncBoundary) {
    // Only the spawned test process loads the interposer and enters the production publisher.
    if let Some(root) = std::env::var_os("FLIGHTDECK_PRIVATE_SYNC_ROOT") {
        verify_sync_failure_child(Path::new(&root), boundary);
        return;
    }

    let fixture = SyncFixture::new(boundary);
    let library = fixture.build_interposer();
    let log = File::create(fixture.root.join("child.log")).unwrap();
    let mut child = SyncChild(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                boundary.test_name(),
                "--no-capture",
                "--test-threads=1",
            ])
            .env("DYLD_INSERT_LIBRARIES", library)
            .env("FLIGHTDECK_PRIVATE_SYNC_ROOT", &fixture.root)
            .env(
                "FLIGHTDECK_PRIVATE_SYNC_TARGET",
                boundary.target(&fixture.root),
            )
            .env(
                "FLIGHTDECK_PRIVATE_SYNC_OBSERVED",
                fixture.root.join("observed"),
            )
            .stdout(Stdio::from(log.try_clone().unwrap()))
            .stderr(Stdio::from(log))
            .spawn()
            .unwrap(),
    );

    let status = child.wait();

    assert!(
        status.success(),
        "{}",
        fs::read_to_string(fixture.root.join("child.log")).unwrap()
    );
}

fn verify_sync_failure_child(root: &Path, boundary: SyncBoundary) {
    let result = atomic_write(root, "metadata", b"replacement");

    assert_eq!(Some(5), result.unwrap_err().raw_os_error());
    assert_eq!(b"x", fs::read(root.join("observed")).unwrap().as_slice());
    assert_eq!(
        boundary.visible_bytes(),
        fs::read(root.join("metadata")).unwrap()
    );
    assert_eq!(
        io::ErrorKind::NotFound,
        fs::symlink_metadata(root.join("metadata.pending"))
            .unwrap_err()
            .kind()
    );
}

struct SyncFixture {
    root: PathBuf,
}

impl SyncFixture {
    fn new(boundary: SyncBoundary) -> Self {
        let root = std::env::temp_dir().join(format!(
            "flightdeck-private-sync-{}-{}",
            boundary.test_name().rsplit("::").next().unwrap(),
            std::process::id()
        ));
        create_private_dir(&root).unwrap();
        let fixture = Self {
            root: root.canonicalize().unwrap(),
        };
        atomic_write(&fixture.root, "metadata", b"prior").unwrap();
        fs::write(fixture.root.join("observed"), b"").unwrap();

        fixture
    }

    fn build_interposer(&self) -> PathBuf {
        let library = self.root.join("sync-fault.dylib");
        let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
        let built = Command::new(rustc)
            .args(["--edition=2024", "--crate-type=cdylib", "--deny=warnings"])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/recording/private/sync_fault.rs"))
            .arg("-o")
            .arg(&library)
            .output()
            .unwrap();

        assert!(
            built.status.success(),
            "{}",
            String::from_utf8_lossy(&built.stderr)
        );

        library
    }
}

impl Drop for SyncFixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

struct SyncChild(Child);

impl SyncChild {
    fn wait(&mut self) -> ExitStatus {
        let start = Instant::now();
        for _ in 0..1000 {
            if let Some(status) = self.0.try_wait().unwrap() {
                return status;
            }

            assert!(
                start.elapsed() < Duration::from_secs(10),
                "sync probe timed out"
            );
            std::thread::sleep(Duration::from_millis(10));
        }

        panic!("sync probe exhausted its wait bound");
    }
}

impl Drop for SyncChild {
    fn drop(&mut self) {
        if self.0.try_wait().unwrap().is_none() {
            self.0.kill().unwrap();
            self.0.wait().unwrap();
        }
    }
}
