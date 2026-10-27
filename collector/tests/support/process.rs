use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

pub struct OwnedChild {
    pub child: Child,
}

impl OwnedChild {
    pub fn spawn(command: &mut Command) -> Self {
        Self {
            child: command.spawn().unwrap(),
        }
    }

    pub fn try_wait(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().unwrap()
    }

    pub fn wait(&mut self, limit: Duration) -> ExitStatus {
        assert!(limit <= Duration::from_secs(10));
        let start = Instant::now();
        let attempts = limit.as_millis() + 1;

        for _attempt in 0..attempts {
            if let Some(status) = self.try_wait() {
                return status;
            }
            if start.elapsed() >= limit {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("child exceeded external deadline pid={}", self.child.id());
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        match self.child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(error) => {
                eprintln!("child status unavailable: {error}");
                if error.raw_os_error() == Some(10) {
                    return;
                }
            }
        }
        if let Err(error) = self.child.kill() {
            eprintln!("child cleanup kill failed: {error}");
        }
        for _attempt in 0..1_001 {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    eprintln!(
                        "child cleanup reaped pid={} status={status}",
                        self.child.id()
                    );
                    return;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(1)),
                Err(error) => {
                    eprintln!("child cleanup reap failed: {error}");
                    return;
                }
            }
        }
        eprintln!("child remains after kill pid={}", self.child.id());
    }
}
