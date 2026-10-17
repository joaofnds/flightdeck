use std::fs::{self, File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

pub struct OwnedChild {
    child: Child,
}

pub struct Logs {
    pub stdout: PathBuf,
    pub stderr: PathBuf,
}

const POLL: Duration = Duration::from_millis(25);
const WAIT_STEPS_MAX: u32 = 30_000;
const LOG_BYTES_MAX: u64 = 16 * 1024 * 1024;
// The replay permits 10000 actions, plus bounded chart/hardware/window diagnostics.
pub const LOG_LINES_MAX: usize = 10_000 + 1024;
const _: () = assert!(LOG_LINES_MAX >= 10_000 + 256 + 256 + 64 + 16 + 8);
const _: () = assert!(WAIT_STEPS_MAX > 600 * 40);

impl OwnedChild {
    pub fn start(command: &mut Command, logs: &Logs) -> io::Result<Self> {
        let stdout = create(&logs.stdout)?;
        let stderr = create(&logs.stderr)?;
        let child = command
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()?;
        Ok(Self { child })
    }

    #[must_use]
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    pub fn status(&mut self) -> io::Result<Option<ExitStatus>> {
        self.child.try_wait()
    }

    pub fn wait(&mut self, budget: Duration) -> io::Result<ExitStatus> {
        let started = Instant::now();
        for _attempt in 0..WAIT_STEPS_MAX {
            if let Some(status) = self.status()? {
                return Ok(status);
            }
            if started.elapsed() >= budget {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "Owned process timed out",
                ));
            }
            thread::sleep(POLL);
        }
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "Process wait bound reached",
        ))
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        match self.status() {
            Ok(Some(_)) => return,
            Ok(None) => {}
            Err(error) => eprintln!("Owned process status: {error}"),
        }
        match i32::try_from(self.child.id()) {
            Ok(pid) => {
                if unsafe { libc::kill(pid, libc::SIGTERM) } != 0 {
                    eprintln!("Owned process SIGTERM: {}", io::Error::last_os_error());
                }
            }
            Err(error) => eprintln!("Owned process pid: {error}"),
        }
        if let Err(error) = self.wait(Duration::from_secs(2)) {
            eprintln!("Owned process SIGTERM wait: {error}");
            if let Err(error) = self.child.kill() {
                eprintln!("Owned process final shutdown: {error}");
            }
            if let Err(error) = self.wait(Duration::from_secs(2)) {
                eprintln!("Owned process reap: {error}");
            }
        }
    }
}

pub fn create(path: &Path) -> io::Result<File> {
    OpenOptions::new().write(true).create_new(true).open(path)
}

pub fn hash(path: &Path) -> io::Result<String> {
    let mut command = Command::new("/usr/bin/shasum");
    command.args(["-a", "256", "--"]).arg(path);
    let text = hash_output(&mut command, Duration::from_secs(30))?;
    let digest = text
        .split_whitespace()
        .next()
        .ok_or_else(|| io::Error::other("Empty hash"))?;
    if digest.len() != 64 {
        return Err(io::Error::other("Invalid SHA-256 length"));
    }
    if !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::other("Invalid SHA-256 encoding"));
    }
    Ok(digest.to_owned())
}

fn hash_output(command: &mut Command, budget: Duration) -> io::Result<String> {
    let child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut owned = OwnedChild { child };
    let status = owned.wait(budget)?;
    if !status.success() {
        return Err(io::Error::other(format!("Hash command failed: {status}")));
    }
    let output = owned
        .child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Missing hash output"))?;
    let mut bytes = Vec::with_capacity(4097);
    output.take(4097).read_to_end(&mut bytes)?;
    if bytes.len() > 4096 {
        return Err(io::Error::other("Hash output exceeds 4096 bytes"));
    }
    String::from_utf8(bytes).map_err(io::Error::other)
}

pub fn wall_ms() -> io::Result<u128> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .map_err(io::Error::other)
}

pub fn read_log(path: &Path) -> io::Result<String> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(LOG_BYTES_MAX + 1)
        .read_to_end(&mut bytes)?;
    if u64::try_from(bytes.len()).map_err(io::Error::other)? > LOG_BYTES_MAX {
        return Err(io::Error::other("Benchmark log exceeds 16 MiB"));
    }
    String::from_utf8(bytes).map_err(io::Error::other)
}

pub fn wait_ready(app: &mut OwnedChild, path: &Path) -> io::Result<Duration> {
    let started = Instant::now();
    for _attempt in 0..3000 {
        if app.status()?.is_some() {
            return Err(io::Error::other("App exited before replay started"));
        }
        for line in read_log(path)?.lines().take(LOG_LINES_MAX) {
            if line.contains("replay_started") {
                let value: Value = serde_json::from_str(line).map_err(io::Error::other)?;
                if value["event"] == "replay_started" {
                    return Ok(started.elapsed());
                }
            }
        }
        if started.elapsed() >= Duration::from_mins(1) {
            break;
        }
        thread::sleep(POLL);
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "Replay did not start within 60 seconds",
    ))
}

pub fn warmup(app: &mut OwnedChild, duration: Duration) -> io::Result<()> {
    let started = Instant::now();
    for _attempt in 0..3000 {
        if app.status()?.is_some() {
            return Err(io::Error::other("App exited during warmup"));
        }
        if started.elapsed() >= duration {
            return Ok(());
        }
        thread::sleep(POLL);
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        "Warmup exceeded its bound",
    ))
}

pub fn trace(path: &Path, expected: u64) -> io::Result<Vec<Value>> {
    let text = read_log(path)?;
    trace_text(&text, expected)
}

fn trace_text(text: &str, expected: u64) -> io::Result<Vec<Value>> {
    let mut trace = Vec::new();
    let mut summary = None;
    for (index, line) in text.lines().take(LOG_LINES_MAX + 1).enumerate() {
        if index == LOG_LINES_MAX {
            return Err(io::Error::other("Replay log line bound exceeded"));
        }
        if !line.starts_with('{') {
            continue;
        }
        let value: Value = serde_json::from_str(line).map_err(io::Error::other)?;
        if value["event"] == "replay_action" {
            let next = u64::try_from(trace.len()).map_err(io::Error::other)?;
            if value["index"].as_u64() != Some(next) {
                return Err(io::Error::other(
                    "Replay action was dropped, duplicated or reordered",
                ));
            }
            trace.push(
                json!({"index":next, "scheduled_at_ms":value["scheduled_at_ms"],
                "effective":value["effective"]}),
            );
        }
        if value["event"] == "replay_summary" {
            summary = Some(value);
        }
    }
    let summary = summary.ok_or_else(|| io::Error::other("Missing replay summary"))?;
    if summary["accepted"].as_u64() != Some(expected) {
        return Err(io::Error::other(
            "Replay did not accept every scheduled action",
        ));
    }
    if u64::try_from(trace.len()).map_err(io::Error::other)? != expected {
        return Err(io::Error::other("Replay action log is incomplete"));
    }
    Ok(trace)
}

pub fn write_json(path: &Path, value: &Value) -> io::Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    std::io::Write::write_all(&mut create(path)?, &bytes)
}

#[must_use]
pub fn logs(output: &Path, name: &str) -> Logs {
    Logs {
        stdout: output.join(format!("{name}.jsonl")),
        stderr: output.join(format!("{name}.stderr.log")),
    }
}

pub fn canonical(path: PathBuf) -> io::Result<PathBuf> {
    let path = fs::canonicalize(path)?;
    if !path.is_file() {
        return Err(io::Error::other("Expected an executable or action file"));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maximum_replay_keeps_its_summary_after_bounded_instrumentation() {
        // A valid 10000-action workload includes additional paint and geometry records.
        let mut text = String::new();
        for index in 0..10_000_u32 {
            text.push_str(
                &json!({"event":"replay_action","index":index,
                "scheduled_at_ms":index,"effective":{"route":"cpu"}})
                .to_string(),
            );
            text.push('\n');
        }
        for _ in 0..272 {
            text.push_str("{\"event\":\"hardware_visibility\"}\n");
        }
        text.push_str("{\"event\":\"replay_summary\",\"accepted\":10000}\n");
        assert_eq!(trace_text(&text, 10_000).unwrap().len(), 10_000);
    }

    #[test]
    fn a_timed_out_owned_process_is_reaped() {
        // Exercise real process ownership at a third-party boundary without waiting for sleep.
        let child = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = i32::try_from(child.id()).unwrap();
        let mut owned = OwnedChild { child };
        let error = owned.wait(Duration::ZERO).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        drop(owned);
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
        assert_eq!(io::Error::last_os_error().raw_os_error(), Some(libc::ESRCH));
    }

    #[test]
    fn hashing_uses_a_bounded_process_wait() {
        // A stalled external hashing command must return instead of blocking the benchmark.
        let mut command = Command::new("/bin/sleep");
        command.arg("30");
        let error = hash_output(&mut command, Duration::ZERO).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    }
}
