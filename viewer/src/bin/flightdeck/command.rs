use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

pub(super) struct Deadline {
    expires: Instant,
}

impl Deadline {
    pub(super) fn build() -> Self {
        Self::after(Duration::from_mins(15))
    }

    fn after(duration: Duration) -> Self {
        Self {
            expires: Instant::now() + duration,
        }
    }

    fn check(&self) -> io::Result<()> {
        if Instant::now() >= self.expires {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "helper build operation deadline exceeded",
            ));
        }

        Ok(())
    }
}

struct Stream<R> {
    reader: R,
    bytes: Vec<u8>,
    limit: usize,
    finished: bool,
    name: &'static str,
}

impl<R: Read + AsRawFd> Stream<R> {
    fn open(reader: R, name: &'static str, limit: usize) -> io::Result<Self> {
        // Darwin fcntl.h: F_GETFL=3, F_SETFL=4, O_NONBLOCK=4.
        let flags = unsafe { fcntl(reader.as_raw_fd(), 3, 0) };
        if flags < 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe { fcntl(reader.as_raw_fd(), 4, flags | 4) } < 0 {
            return Err(io::Error::last_os_error());
        }

        Ok(Self {
            reader,
            bytes: Vec::with_capacity(limit),
            limit,
            finished: false,
            name,
        })
    }

    fn collect(&mut self) -> io::Result<()> {
        assert!(self.bytes.len() <= self.limit);

        let mut buffer = [0; 8192];
        for _read in 0..8 {
            match self.reader.read(&mut buffer) {
                Ok(0) => {
                    self.finished = true;
                    return Ok(());
                }
                Ok(count) => {
                    if count > self.limit - self.bytes.len() {
                        return Err(io::Error::new(
                            io::ErrorKind::OutOfMemory,
                            format!("helper {} exceeds {} bytes", self.name, self.limit),
                        ));
                    }
                    self.bytes.extend_from_slice(&buffer[..count]);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }

        Ok(())
    }
}

pub(super) fn run(command: &mut Command, deadline: &Deadline) -> io::Result<Output> {
    deadline.check()?;

    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    match collect(&mut child, deadline) {
        Ok(output) => Ok(output),
        Err(error) => match terminate(&mut child) {
            Ok(()) => Err(io::Error::new(
                error.kind(),
                format!("{}: {error}", command.get_program().to_string_lossy()),
            )),
            Err(cleanup) => Err(io::Error::new(
                error.kind(),
                format!("{error}; owned helper cleanup: {cleanup}"),
            )),
        },
    }
}

fn collect(child: &mut Child, deadline: &Deadline) -> io::Result<Output> {
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("missing helper stdout pipe"))?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("missing helper stderr pipe"))?;
    let mut stdout = Stream::open(stdout, "stdout", 16 * 1024 * 1024)?;
    let mut stderr = Stream::open(stderr, "stderr", 1024 * 1024)?;
    let status = collect_status(child, deadline, &mut stdout, &mut stderr).map_err(|error| {
        let tail = &stderr.bytes[stderr.bytes.len().saturating_sub(8192)..];
        io::Error::new(
            error.kind(),
            format!(
                "{error}; helper stderr tail: {}",
                String::from_utf8_lossy(tail)
            ),
        )
    })?;

    Ok(Output {
        status,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
    })
}

fn collect_status(
    child: &mut Child,
    deadline: &Deadline,
    stdout: &mut Stream<std::process::ChildStdout>,
    stderr: &mut Stream<std::process::ChildStderr>,
) -> io::Result<std::process::ExitStatus> {
    let mut status = None;

    for _step in 0..900_001 {
        deadline.check()?;
        stdout.collect()?;
        stderr.collect()?;
        if status.is_none() {
            status = child.try_wait()?;
        }
        if let Some(status) = status {
            if stdout.finished {
                if stderr.finished {
                    return Ok(status);
                }
            }
        }

        std::thread::sleep(Duration::from_millis(2));
    }

    Err(io::Error::other("helper collection step bound exceeded"))
}

fn terminate(child: &mut Child) -> io::Result<()> {
    if child.try_wait()?.is_some() {
        return Ok(());
    }

    // Child remains unreaped and exclusively owned: its PID cannot have been reused.
    child.kill()?;
    let deadline = Instant::now() + Duration::from_secs(2);
    for _step in 0..1001 {
        if child.try_wait()?.is_some() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "owned helper did not reap within two seconds after SIGKILL",
            ));
        }

        std::thread::sleep(Duration::from_millis(2));
    }

    Err(io::Error::other("helper cleanup step bound exceeded"))
}

unsafe extern "C" {
    fn fcntl(descriptor: i32, command: i32, ...) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn bounds_stdout_while_the_child_produces_it() {
        // A real child exceeds the build output cap before exiting.
        let mut command = fixture("stdout");

        let result = run(&mut command, &Deadline::build());

        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::OutOfMemory);
    }

    #[test]
    fn bounds_stderr_while_the_child_produces_it() {
        // A separate real child's stderr cannot bypass stdout's independent bound.
        let mut command = fixture("stderr");

        let result = run(&mut command, &Deadline::build());

        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::OutOfMemory);
    }

    #[test]
    fn reads_both_streams_without_a_pipe_deadlock() {
        // Each stream exceeds the kernel pipe capacity while both remain within their caps.
        let mut command = fixture("both");

        let output = run(&mut command, &Deadline::after(Duration::from_secs(3))).unwrap();

        assert!(output.status.success());
        assert_eq!(output.stderr, vec![b'e'; 256 * 1024]);
        let payload = vec![b'X'; 256 * 1024];
        assert!(
            output
                .stdout
                .windows(payload.len())
                .any(|bytes| bytes == payload)
        );
    }

    #[test]
    fn stops_continuous_output_at_the_operation_deadline() {
        // Small continuing writes must not keep the deadline check from running.
        let mut command = fixture("continuous");
        let started = Instant::now();

        let result = run(&mut command, &Deadline::after(Duration::from_millis(100)));

        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn kills_and_reaps_an_owned_child_that_never_exits() {
        // The fixture reports its PID so waitpid can confirm the runner already reaped it.
        let path =
            std::env::temp_dir().join(format!("flightdeck-build-child-{}.pid", std::process::id()));
        let mut command = fixture("hung");
        command.env("FLIGHTDECK_BUILD_CHILD_PID", &path);
        let started = Instant::now();

        let result = run(&mut command, &Deadline::after(Duration::from_millis(200)));

        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(3));
        let pid: i32 = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        let mut status = 0;
        assert_eq!(unsafe { waitpid(pid, &raw mut status, 1) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(10));
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn carries_the_remaining_budget_into_the_next_command() {
        // Completing one helper cannot reset the common operation deadline for signing.
        let deadline = Deadline::after(Duration::from_millis(150));
        let mut first = fixture("delay");
        let first_output = run(&mut first, &deadline).unwrap();
        let started = Instant::now();
        let mut second = fixture("hung");

        let result = run(&mut second, &deadline);

        assert!(first_output.status.success());
        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(deadline.check().is_err());
    }

    #[test]
    fn refuses_a_new_child_after_the_shared_deadline() {
        // An already expired operation refuses before spawn, even for a nonexistent command.
        let deadline = Deadline::after(Duration::ZERO);
        let mut command = Command::new("/flightdeck-nonexistent-command");

        let result = run(&mut command, &deadline);

        assert_eq!(result.err().unwrap().kind(), io::ErrorKind::TimedOut);
    }

    #[test]
    fn child_output_fixture() {
        let Ok(mode) = std::env::var("FLIGHTDECK_BUILD_FIXTURE") else {
            return;
        };

        match mode.as_str() {
            "stdout" => write_stdout(2049),
            "stderr" => {
                for _index in 0..129 {
                    io::stderr().write_all(&[b'e'; 8192]).unwrap();
                }
            }
            "both" => {
                for _index in 0..32 {
                    write_stdout(1);
                    io::stderr().write_all(&[b'e'; 8192]).unwrap();
                }
            }
            "continuous" => {
                for _index in 0..1000 {
                    io::stdout().write_all(b"x").unwrap();
                    std::thread::sleep(Duration::from_millis(10));
                }
            }
            "hung" => {
                if let Some(path) = std::env::var_os("FLIGHTDECK_BUILD_CHILD_PID") {
                    std::fs::write(path, std::process::id().to_string()).unwrap();
                }
                std::thread::sleep(Duration::from_secs(10));
            }
            "delay" => std::thread::sleep(Duration::from_millis(50)),
            _ => panic!("unknown build fixture mode"),
        }
    }

    fn write_stdout(count: u32) {
        let bytes = [b'X'; 8192];
        for _index in 0..count {
            io::stdout().write_all(&bytes).unwrap();
        }
    }

    fn fixture(mode: &str) -> Command {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "command::tests::child_output_fixture",
                "--nocapture",
            ])
            .env("FLIGHTDECK_BUILD_FIXTURE", mode);

        command
    }

    unsafe extern "C" {
        fn waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
    }
}
