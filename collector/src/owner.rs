//! Optional binding to the actual app parent; ordinary collectors stay independent.

use std::io;

#[derive(Clone, Copy)]
pub struct Owner {
    expected_parent: Option<i32>,
}

impl Owner {
    pub fn bind(expected_parent: Option<i32>) -> io::Result<Self> {
        if let Some(expected) = expected_parent {
            if !(2..=collector::processes::PID_MAX).contains(&expected) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "invalid parent PID",
                ));
            }

            let actual = parent_pid();
            if actual != expected {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("expected parent PID {expected}, actual parent PID {actual}"),
                ));
            }
        }

        Ok(Self { expected_parent })
    }

    /// Reads the current parent relationship once, without searching for a PID's liveness.
    pub fn departed(self) -> bool {
        self.expected_parent
            .is_some_and(|expected| parent_pid() != expected)
    }
}

pub fn parse_parent_pid(value: &str) -> Result<i32, ()> {
    let pid = value.parse::<i32>().map_err(|_| ())?;
    if !(2..=collector::processes::PID_MAX).contains(&pid) {
        return Err(());
    }

    Ok(pid)
}

pub(super) fn parent_pid() -> i32 {
    unsafe { getppid() }
}

unsafe extern "C" {
    fn getppid() -> i32;
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    use super::Owner;

    const TEST: &str = "owner::tests::recognizes_departure_of_its_real_parent";
    const ROLE: &str = "FLIGHTDECK_OWNER_TEST_ROLE";
    const ROOT: &str = "FLIGHTDECK_OWNER_TEST_ROOT";
    const EXPECTED: &str = "FLIGHTDECK_OWNER_TEST_EXPECTED";

    #[test]
    fn recognizes_departure_of_its_real_parent() {
        // Goal: real reparenting stops a bound child while ordinary collection stays independent.
        // Method: run this test as an intermediate parent and observer with bounded handshakes.
        match std::env::var(ROLE) {
            Ok(role) => {
                let root = PathBuf::from(std::env::var_os(ROOT).unwrap());
                match role.as_str() {
                    "parent" => parent(&root),
                    "observer" => observer(&root),
                    _ => panic!("unknown ownership test role"),
                }
                return;
            }
            Err(std::env::VarError::NotPresent) => {}
            Err(error) => panic!("invalid ownership test role: {error}"),
        }
        let root =
            Root(std::env::temp_dir().join(format!("flightdeck-owner-{}", std::process::id())));
        collector::recording::private::create_private_dir(&root.0).unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(ROLE, "parent")
            .env(ROOT, &root.0)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        let expected = i32::try_from(child.id()).unwrap();

        wait_child(&mut child);
        wait_file(&root.0.join("departed"));

        let report = std::fs::read(root.0.join("departed")).unwrap();
        assert_eq!(report.len(), 5);
        let actual = i32::from_le_bytes(report[..4].try_into().unwrap());
        assert_ne!(actual, expected);
        assert_eq!(
            report[4], 0,
            "ordinary mode must remain independent after reparenting"
        );
        eprintln!(
            "real owner departure expected_parent={expected} actual_parent={actual} independent=0"
        );
    }

    fn parent(root: &Path) {
        let mut observer = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture"])
            .env(ROLE, "observer")
            .env(ROOT, root)
            .env(EXPECTED, std::process::id().to_string())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap();
        for _attempt in 0..5000 {
            if root.join("bound").is_file() {
                // The observer must outlive this process to exercise the real parent relation.
                std::process::exit(0);
            }
            if let Some(status) = observer.try_wait().unwrap() {
                panic!("ownership observer exited before binding: {status}");
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        observer.kill().unwrap();
        let status = observer.wait().unwrap();
        panic!("ownership observer failed to bind: {status}");
    }

    fn observer(root: &Path) {
        let expected = std::env::var(EXPECTED).unwrap().parse::<i32>().unwrap();
        let owner = Owner::bind(Some(expected)).unwrap();
        let independent = Owner::bind(None).unwrap();
        assert!(!owner.departed());
        std::fs::write(root.join("bound"), b"bound").unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        for _attempt in 0..5000 {
            if owner.departed() {
                let mut report = [0; 5];
                report[..4].copy_from_slice(&super::parent_pid().to_le_bytes());
                report[4] = u8::from(independent.departed());
                std::fs::write(root.join("report"), report).unwrap();
                std::fs::rename(root.join("report"), root.join("departed")).unwrap();
                return;
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("ownership observer did not detect parent departure");
    }

    fn wait_child(child: &mut std::process::Child) {
        let deadline = Instant::now() + Duration::from_secs(5);
        for _attempt in 0..5000 {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "ownership parent failed: {status}");
                return;
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        child.kill().unwrap();
        let status = child.wait().unwrap();
        panic!("ownership parent timed out: {status}");
    }

    fn wait_file(path: &Path) {
        let deadline = Instant::now() + Duration::from_secs(5);
        for _attempt in 0..5000 {
            if path.is_file() {
                return;
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("ownership observer report timed out");
    }

    struct Root(PathBuf);

    impl Drop for Root {
        fn drop(&mut self) {
            if let Err(error) = std::fs::remove_dir_all(&self.0) {
                eprintln!("ownership test directory cleanup: {error}");
            }
        }
    }
}
