//! Startup-only spawning and allocation-free observation of the persistence child.
mod native;

use std::io;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use crate::clock::Timebase;
use crate::kernel::ESRCH;
use native::kill;

pub const OWNERSHIP_EXIT_STATUS: u8 = 73;
const SIGKILL: i32 = 9;

pub struct Persister {
    pid: i32,
    timebase: Timebase,
    state: State,
}

#[derive(Debug, Clone, Copy)]
pub struct ChildCost {
    pub cpu_ns: u64,
    pub footprint_bytes: u64,
    pub footprint_bytes_max: u64,
}

#[derive(Debug, Clone, Copy)]
pub struct Finished {
    pub cpu_ns: u64,
    pub status: i32,
    pub resident_bytes_max: u64,
}

#[derive(Clone, Copy)]
enum State {
    Running,
    Exited(Finished),
    Unmeasured,
}

impl Finished {
    #[must_use]
    pub fn owned_by_surviving_persister(&self) -> bool {
        self.status == i32::from(OWNERSHIP_EXIT_STATUS) << 8
    }

    #[must_use]
    pub fn successful(&self) -> bool {
        self.status == 0
    }
}

impl Persister {
    /// Runs only at startup; sampling never starts a replacement process.
    pub fn spawn(executable: &Path, recording: &Path, timebase: Timebase) -> io::Result<Self> {
        let mut command = Command::new(executable);
        command.arg("--recording").arg(recording).process_group(0);
        Self::spawn_command(&mut command, timebase)
    }

    /// Starts a caller-configured child and takes responsibility for its final wait4.
    pub fn spawn_command(command: &mut Command, timebase: Timebase) -> io::Result<Self> {
        let mut child = command.spawn()?;
        let pid = child.id().cast_signed();
        if pid <= 0 {
            child.kill()?;
            child.wait()?;
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        // std::process::Child has no Drop wait: this owner takes its PID and uses wait4
        // so the last CPU, including final encoding after sampling stops, survives exit.
        drop(child);
        Ok(Self {
            pid,
            timebase,
            state: State::Running,
        })
    }

    /// A successful poll allocates nothing, including when the child has exited.
    pub fn poll(&mut self) -> io::Result<Option<Finished>> {
        match self.state {
            State::Exited(finished) => return Ok(Some(finished)),
            State::Unmeasured => return Err(io::Error::from(io::ErrorKind::InvalidData)),
            State::Running => {}
        }
        let Some(reaped) = native::wait(self.pid, false)? else {
            return Ok(None);
        };
        self.state = State::Unmeasured;
        let finished = reaped.finished()?;
        self.state = State::Exited(finished);
        Ok(Some(finished))
    }

    pub fn sample(&mut self) -> io::Result<Option<ChildCost>> {
        if self.poll()?.is_some() {
            return Ok(None);
        }
        match native::sample(self.pid, self.timebase) {
            Ok(cost) => Ok(Some(cost)),
            Err(error) => {
                if self.poll()?.is_some() {
                    return Ok(None);
                }
                Err(error)
            }
        }
    }

    pub fn finish(&mut self, timeout: Duration) -> io::Result<Finished> {
        if timeout > super::PERSISTER_FINISH_LIMIT {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let start = Instant::now();
        let attempts = timeout.as_millis().div_ceil(10) + 1;
        for _attempt in 0..attempts {
            if let Some(finished) = self.poll()? {
                return Ok(finished);
            }
            if start.elapsed() >= timeout {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        self.terminate()
    }

    fn terminate(&mut self) -> io::Result<Finished> {
        if let Some(finished) = self.poll()? {
            return Ok(finished);
        }
        if unsafe { kill(self.pid, SIGKILL) } != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(ESRCH) {
                return Err(error);
            }
        }
        let Some(reaped) = native::wait(self.pid, true)? else {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        };
        self.state = State::Unmeasured;
        let finished = reaped.finished()?;
        self.state = State::Exited(finished);
        Ok(finished)
    }
}

impl Drop for Persister {
    fn drop(&mut self) {
        if let State::Running = self.state {
            if let Err(error) = self.terminate() {
                crate::diagnostics::error(format_args!(
                    "collector: persistence child cleanup failed: {error}"
                ));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Timebase;
    use std::process::Command;
    use std::time::Duration;
    const ECHILD: i32 = 10;

    #[test]
    fn reaps_a_failed_child_with_its_final_cpu_usage() {
        // Goal: a failed persister cannot become an unreaped zombie or reported success.
        // Method: launch a real failing executable and collect its final wait4 CPU totals.
        let mut command = Command::new("/usr/bin/false");
        let mut persister =
            Persister::spawn_command(&mut command, Timebase::read().unwrap()).unwrap();

        let finished = persister.finish(Duration::from_secs(2)).unwrap();

        assert!(!finished.successful());
        assert_eq!(1 << 8, finished.status);
        assert_eq!(finished.cpu_ns, persister.poll().unwrap().unwrap().cpu_ns);
    }

    #[test]
    fn refuses_to_wait_longer_than_the_persister_finish_limit() {
        // Goal: no caller can hold a stopping collector past the limit its stop deadline allows.
        // Method: ask a running child to finish within a millisecond more than the limit.
        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        let mut persister =
            Persister::spawn_command(&mut command, Timebase::read().unwrap()).unwrap();

        let refused = persister
            .finish(super::super::PERSISTER_FINISH_LIMIT + Duration::from_millis(1))
            .unwrap_err();

        assert_eq!(io::ErrorKind::InvalidInput, refused.kind());
        assert!(!persister.finish(Duration::ZERO).unwrap().successful());
    }

    #[test]
    fn kills_and_reaps_a_child_that_exceeds_the_drain_bound() {
        // Goal: finite collection cannot leave a stuck encoder child alive indefinitely.
        // Method: launch sleep, finish with a zero timeout and observe a reaped failure.
        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        let mut persister =
            Persister::spawn_command(&mut command, Timebase::read().unwrap()).unwrap();

        let finished = persister.finish(Duration::ZERO).unwrap();

        assert!(!finished.successful());
        assert_eq!(
            Some(ECHILD),
            native::wait(persister.pid, false)
                .err()
                .unwrap()
                .raw_os_error()
        );
    }
}
