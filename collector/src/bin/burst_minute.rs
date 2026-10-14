//! Runs the collector for one minute and, inside it, `followprobe` with a burst of forks, then
//! prints the date, the load average and the power source, the collector's report and
//! followprobe's report, in that order. Each report holds what its program wrote to standard
//! output and standard error, in the order it wrote them.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

const COLLECTOR_SECONDS: u64 = 60;
const BURST_START_SECONDS: u64 = 15;
const BURST_SECONDS: u64 = 30;
const BURST_CHILD_MS: u64 = 6;
const BURST_GENERATORS: u64 = 2;
/// Sets how long past its own run a program may take to end before the run stops it.
const GRACE_SECONDS: u64 = 30;
/// Sets how long date, sysctl or pmset may take, though each ends at once.
const CONDITION_MS_MAX: u64 = 10_000;
const POLL_MS: u64 = 100;
/// Counts the output files this process has created, so that no two share a path.
static OUTPUT_FILES_CREATED: AtomicU64 = AtomicU64::new(0);
const USAGE: &str = "usage: burst_minute --rescan-ms N --per-second N -- COLLECTOR [ARGUMENT...]";

fn main() -> ExitCode {
    let options = match collector::arguments("burst_minute").and_then(|words| options_from(&words))
    {
        Ok(options) => options,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };

    match run(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("burst_minute: {error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, PartialEq)]
struct Options {
    rescan_ms: u64,
    per_second: u64,
    collector: CollectorCommand,
}

/// Holds the program that runs as the collector and its own arguments, to which the run adds
/// `--seconds` with `COLLECTOR_SECONDS`.
#[derive(Debug, PartialEq)]
struct CollectorCommand {
    program: String,
    arguments: Vec<String>,
}

fn options_from(words: &[String]) -> Result<Options, String> {
    let Some(separator) = words.iter().position(|word| word == "--") else {
        return Err(USAGE.to_owned());
    };
    let Some((program, arguments)) = words[separator + 1..].split_first() else {
        return Err(USAGE.to_owned());
    };
    if program.is_empty() {
        return Err(USAGE.to_owned());
    }

    let mut rescan_ms = None;
    let mut per_second = None;
    for pair in words[..separator].chunks(2) {
        let [flag, number] = pair else {
            return Err(USAGE.to_owned());
        };
        let target = match flag.as_str() {
            "--rescan-ms" => &mut rescan_ms,
            "--per-second" => &mut per_second,
            _ => return Err(USAGE.to_owned()),
        };
        if target.is_some() {
            return Err(format!("{flag} is given twice"));
        }
        match number.parse() {
            Ok(parsed) => *target = Some(parsed),
            Err(error) => return Err(format!("{flag} {number}: {error}")),
        }
    }

    let Some(rescan_ms) = rescan_ms else {
        return Err(USAGE.to_owned());
    };
    let Some(per_second) = per_second else {
        return Err(USAGE.to_owned());
    };

    // Each flag took the word after it, so the words before the separator came in pairs.
    assert!(separator.is_multiple_of(2));
    assert!(!program.is_empty());
    Ok(Options {
        rescan_ms,
        per_second,
        collector: CollectorCommand {
            program: program.clone(),
            arguments: arguments.to_vec(),
        },
    })
}

fn run(options: &Options) -> io::Result<()> {
    const { assert!(BURST_START_SECONDS + BURST_SECONDS < COLLECTOR_SECONDS) };
    const { assert!(BURST_GENERATORS > 0) };
    const { assert!(CONDITION_MS_MAX >= POLL_MS) };
    assert!(!options.collector.program.is_empty());

    let followprobe = run_followprobe_path()?;
    let mut stdout = io::stdout().lock();
    run_write_conditions(&mut stdout)?;

    let reports = Reports {
        collector: OutputFile::create("collector")?,
        followprobe: OutputFile::create("followprobe")?,
    };
    assert!(reports.collector.path != reports.followprobe.path);
    let minute = run_minute(options, &followprobe, &reports);

    // Both reports are written whatever the minute's outcome, since a program that failed
    // says why in its report.
    stdout.write_all(&reports.collector.read()?)?;
    stdout.write_all(&reports.followprobe.read()?)?;
    stdout.flush()?;
    minute
}

/// Runs the collector for its minute with followprobe's burst inside it. Once both have
/// ended, fails with the collector's failure, else with followprobe's.
fn run_minute(options: &Options, followprobe: &Path, reports: &Reports) -> io::Result<()> {
    // The two programs write to reports of their own, so neither's output lands in the other's.
    assert!(reports.collector.path != reports.followprobe.path);
    assert!(followprobe.ends_with("followprobe"));

    let mut collector = StartedProcess::start(
        "collector",
        Command::new(&options.collector.program)
            .args(run_minute_collector_arguments(&options.collector))
            .stdin(Stdio::null())
            .stdout(reports.collector.stdio()?)
            .stderr(reports.collector.stdio()?),
    )?;

    thread::sleep(Duration::from_secs(BURST_START_SECONDS));
    let burst = match collector.child.try_wait()? {
        Some(status) => Err(io::Error::other(format!(
            "the collector ended before the burst, {status}"
        ))),
        None => run_burst(options, followprobe, &reports.followprobe),
    };
    let minute = collector.wait((COLLECTOR_SECONDS + GRACE_SECONDS) * 1000);

    minute?;
    burst
}

/// Runs followprobe for the burst and waits for it to end.
fn run_burst(options: &Options, followprobe: &Path, report: &OutputFile) -> io::Result<()> {
    assert!(followprobe.ends_with("followprobe"));

    let mut probe = StartedProcess::start(
        "followprobe",
        Command::new(followprobe)
            .args(run_burst_arguments(options))
            .stdin(Stdio::null())
            .stdout(report.stdio()?)
            .stderr(report.stdio()?),
    )?;

    probe.wait((BURST_SECONDS + GRACE_SECONDS) * 1000)
}

/// Lists the collector's arguments, its own and then the minute it runs for.
fn run_minute_collector_arguments(collector: &CollectorCommand) -> Vec<String> {
    let mut arguments = collector.arguments.clone();
    arguments.push("--seconds".to_owned());
    arguments.push(COLLECTOR_SECONDS.to_string());

    assert_eq!(arguments.len(), collector.arguments.len() + 2);
    assert_eq!(Some(&COLLECTOR_SECONDS.to_string()), arguments.last());
    arguments
}

/// Lists followprobe's arguments in the order it reads them, `RESCAN_MS PER_SECOND SECONDS
/// CHILD_MS GENERATORS`.
fn run_burst_arguments(options: &Options) -> [String; 5] {
    let arguments = [
        options.rescan_ms,
        options.per_second,
        BURST_SECONDS,
        BURST_CHILD_MS,
        BURST_GENERATORS,
    ]
    .map(|number| number.to_string());

    assert_eq!(options.rescan_ms.to_string(), arguments[0]);
    assert_eq!(BURST_GENERATORS.to_string(), arguments[4]);
    arguments
}

/// Finds the `followprobe` built beside this program, in the same target directory.
fn run_followprobe_path() -> io::Result<PathBuf> {
    let path = std::env::current_exe()?.with_file_name("followprobe");
    assert!(path.ends_with("followprobe"));

    match fs::exists(&path) {
        Ok(true) => Ok(path),
        Ok(false) => Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{} is not built", path.display()),
        )),
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!("{}: {error}", path.display()),
        )),
    }
}

/// Writes the date, the load average and the power source, which say what else the machine
/// was doing during the minute.
fn run_write_conditions(stdout: &mut impl Write) -> io::Result<()> {
    stdout.write_all(&run_write_conditions_stdout("/bin/date", &[])?)?;
    stdout.write_all(&run_write_conditions_stdout(
        "/usr/sbin/sysctl",
        &["-n", "vm.loadavg"],
    )?)?;
    let power = run_write_conditions_stdout("/usr/bin/pmset", &["-g", "batt"])?;
    match run_write_conditions_first_line(&power) {
        Some(line) => stdout.write_all(line)?,
        None => return Err(io::Error::other("pmset -g batt printed nothing")),
    }
    stdout.flush()
}

/// Runs a program to its end, within `CONDITION_MS_MAX`, and returns what it wrote to
/// standard output, or fails with its status and what it wrote to standard error.
fn run_write_conditions_stdout(program: &'static str, arguments: &[&str]) -> io::Result<Vec<u8>> {
    assert!(program.starts_with('/'));

    let output = OutputFile::create("output")?;
    let errors = OutputFile::create("errors")?;
    assert!(output.path != errors.path);
    let mut process = StartedProcess::start(
        program,
        Command::new(program)
            .args(arguments)
            .stdin(Stdio::null())
            .stdout(output.stdio()?)
            .stderr(errors.stdio()?),
    )?;

    match process.wait(CONDITION_MS_MAX) {
        Ok(()) => output.read(),
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!("{error}\n{}", String::from_utf8_lossy(&errors.read()?)),
        )),
    }
}

/// Returns the first line of `text` with its newline, or `None` when `text` is empty.
fn run_write_conditions_first_line(text: &[u8]) -> Option<&[u8]> {
    let line = text.split_inclusive(|&byte| byte == b'\n').next();
    if let Some(line) = line {
        assert!(!line.is_empty());
        assert!(text.starts_with(line));
    }
    line
}

/// Holds the two report files of one run, by name so they cannot be swapped.
struct Reports {
    collector: OutputFile,
    followprobe: OutputFile,
}

/// Owns a process the run started, and stops it and waits for it when dropped, so no way out
/// of the run leaves it running.
struct StartedProcess {
    name: &'static str,
    child: Child,
}

impl StartedProcess {
    /// Starts `command`, or fails naming its program, which the operating system's error
    /// leaves out.
    fn start(name: &'static str, command: &mut Command) -> io::Result<StartedProcess> {
        assert!(!name.is_empty());

        match command.spawn() {
            Ok(child) => Ok(StartedProcess { name, child }),
            Err(error) => Err(io::Error::new(
                error.kind(),
                format!("cannot start {}: {error}", command.get_program().display()),
            )),
        }
    }

    /// Waits up to `limit_ms` for the process to end, then stops it. Fails unless the process
    /// ended within the limit and succeeded.
    fn wait(&mut self, limit_ms: u64) -> io::Result<()> {
        const { assert!(POLL_MS > 0) };
        assert!(!self.name.is_empty());
        assert!(limit_ms >= POLL_MS);

        for _ in 0..limit_ms.div_ceil(POLL_MS) {
            match self.child.try_wait()? {
                Some(status) => return self.wait_status(status),
                None => thread::sleep(Duration::from_millis(POLL_MS)),
            }
        }

        self.child.kill()?;
        let status = self.child.wait()?;
        Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!(
                "{} ran past {limit_ms} ms and was stopped, {status}",
                self.name
            ),
        ))
    }

    fn wait_status(&self, status: ExitStatus) -> io::Result<()> {
        assert!(!self.name.is_empty());

        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!("{}: {status}", self.name)))
        }
    }
}

impl Drop for StartedProcess {
    fn drop(&mut self) {
        assert!(!self.name.is_empty());

        match self.child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) => match self.child.kill() {
                Ok(()) => {
                    if let Err(error) = self.child.wait() {
                        eprintln!("burst_minute: cannot wait for {}: {error}", self.name);
                    }
                }
                Err(error) => eprintln!("burst_minute: cannot stop {}: {error}", self.name),
            },
            Err(error) => eprintln!(
                "burst_minute: cannot tell whether {} ended: {error}",
                self.name
            ),
        }
    }
}

/// Owns a file in the temporary directory that a program writes its output to, and removes
/// it when dropped.
struct OutputFile {
    path: PathBuf,
    file: File,
}

impl OutputFile {
    fn create(name: &str) -> io::Result<OutputFile> {
        assert!(!name.is_empty());
        assert!(!name.contains('/'));

        let sequence = OUTPUT_FILES_CREATED.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "burst_minute-{}-{sequence}-{name}.txt",
            std::process::id()
        ));
        let file = OpenOptions::new()
            .read(false)
            .write(true)
            .append(false)
            .truncate(false)
            .create(false)
            .create_new(true)
            .open(&path)?;

        Ok(OutputFile { path, file })
    }

    /// Returns a handle on the file for a program's standard output or standard error. The
    /// handles share one offset, so the file holds both streams in the order they were
    /// written.
    fn stdio(&self) -> io::Result<Stdio> {
        Ok(Stdio::from(self.file.try_clone()?))
    }

    fn read(&self) -> io::Result<Vec<u8>> {
        fs::read(&self.path)
    }
}

impl Drop for OutputFile {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_file(&self.path) {
            eprintln!(
                "burst_minute: cannot remove {}: {error}",
                self.path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn arguments(words: &[&str]) -> Vec<String> {
        words.iter().map(|&word| word.to_owned()).collect()
    }

    fn options(collector: CollectorCommand) -> Options {
        Options {
            rescan_ms: 5,
            per_second: 1000,
            collector,
        }
    }

    #[test]
    fn takes_the_flags_and_the_collector_command() {
        // Goal: the flags set the probe's rate, and what follows `--` is the collector's
        // program and its arguments, its own flags included.
        // Method: every flag, and a collector command with a flag of its own.
        let words = arguments(&[
            "--rescan-ms",
            "5",
            "--per-second",
            "1000",
            "--",
            "./collector",
            "--rescan-ms",
            "5",
        ]);

        let parsed = options_from(&words);

        assert_eq!(
            parsed,
            Ok(options(CollectorCommand {
                program: "./collector".to_owned(),
                arguments: arguments(&["--rescan-ms", "5"]),
            }))
        );
    }

    #[test]
    fn refuses_a_missing_collector_command() {
        // Goal: a run needs a collector to measure.
        // Method: the flags with no `--`, with nothing after it, and with an empty program.
        let without_separator = arguments(&["--rescan-ms", "5", "--per-second", "1000"]);
        let without_command = arguments(&["--rescan-ms", "5", "--per-second", "1000", "--"]);
        let empty_program = arguments(&["--rescan-ms", "5", "--per-second", "1000", "--", ""]);

        assert_eq!(options_from(&without_separator), Err(USAGE.to_owned()));
        assert_eq!(options_from(&without_command), Err(USAGE.to_owned()));
        assert_eq!(options_from(&empty_program), Err(USAGE.to_owned()));
    }

    #[test]
    fn refuses_a_missing_flag() {
        // Goal: the probe's rate has no default, as the script it replaces had none.
        // Method: each flag left out in turn.
        let without_rescan = arguments(&["--per-second", "1000", "--", "./collector"]);
        let without_rate = arguments(&["--rescan-ms", "5", "--", "./collector"]);

        assert_eq!(options_from(&without_rescan), Err(USAGE.to_owned()));
        assert_eq!(options_from(&without_rate), Err(USAGE.to_owned()));
    }

    #[test]
    fn refuses_a_flag_that_is_not_a_number() {
        // Goal: a rate that is not a number fails before the minute starts, naming the flag.
        // Method: a word in place of the interval.
        let words = arguments(&["--rescan-ms", "five", "--per-second", "1000", "--", "c"]);

        let parsed = options_from(&words);

        assert_eq!(
            parsed,
            Err("--rescan-ms five: invalid digit found in string".to_owned())
        );
    }

    #[test]
    fn refuses_an_unknown_flag() {
        // Goal: a flag the run does not read is refused rather than ignored.
        // Method: a third flag beside the two.
        let words = arguments(&[
            "--rescan-ms",
            "5",
            "--per-second",
            "1000",
            "--seconds",
            "60",
            "--",
            "c",
        ]);

        assert_eq!(options_from(&words), Err(USAGE.to_owned()));
    }

    #[test]
    fn refuses_a_flag_given_twice() {
        // Goal: a flag given twice is refused, rather than the last one silently winning.
        // Method: the interval given twice, each time a number.
        let words = arguments(&[
            "--rescan-ms",
            "5",
            "--rescan-ms",
            "50",
            "--per-second",
            "1000",
            "--",
            "c",
        ]);

        let parsed = options_from(&words);

        assert_eq!(parsed, Err("--rescan-ms is given twice".to_owned()));
    }

    #[test]
    fn gives_the_collector_its_own_arguments_and_then_the_minute() {
        // Goal: the collector runs for the minute, after whatever flags it was given.
        // Method: a collector command with a flag of its own.
        let collector = CollectorCommand {
            program: "./collector".to_owned(),
            arguments: arguments(&["--rescan-ms", "5"]),
        };

        let collector_arguments = run_minute_collector_arguments(&collector);

        assert_eq!(
            collector_arguments,
            arguments(&["--rescan-ms", "5", "--seconds", "60"])
        );
    }

    #[test]
    fn gives_followprobe_its_arguments_in_the_order_it_reads_them() {
        // Goal: followprobe reads RESCAN_MS PER_SECOND SECONDS CHILD_MS GENERATORS by
        // position, so the burst's settings land in those places.
        // Method: the options from the flags, and the burst's constants.
        let options = options(CollectorCommand {
            program: "./collector".to_owned(),
            arguments: Vec::new(),
        });

        let followprobe_arguments = run_burst_arguments(&options);

        assert_eq!(followprobe_arguments, ["5", "1000", "30", "6", "2"]);
    }

    #[test]
    fn takes_the_first_line_of_the_power_source() {
        // Goal: the power source is pmset's first line, with its newline, and nothing when
        // pmset printed nothing.
        // Method: two lines, then no text.
        let power = b"Now drawing from 'AC Power'\n -InternalBattery-0\n";

        assert_eq!(
            run_write_conditions_first_line(power),
            Some(&b"Now drawing from 'AC Power'\n"[..])
        );
        assert_eq!(run_write_conditions_first_line(b""), None);
    }

    #[test]
    fn returns_what_a_program_wrote_to_standard_output() {
        // Goal: a program that succeeds gives what it wrote to standard output, and nothing it
        // wrote to standard error.
        // Method: a shell that writes a line to each.
        let output = run_write_conditions_stdout("/bin/sh", &["-c", "echo out; echo noise >&2"]);

        assert_eq!(output.unwrap(), b"out\n");
    }

    #[test]
    fn reports_what_a_failed_program_wrote_to_standard_error() {
        // Goal: a program that fails is reported by its status and by what it wrote to
        // standard error, which says why.
        // Method: a shell that writes a line to standard error and exits 3.
        let output =
            run_write_conditions_stdout("/bin/sh", &["-c", "echo out; echo why >&2; exit 3"]);

        assert_eq!(
            output.unwrap_err().to_string(),
            "/bin/sh: exit status: 3\nwhy\n"
        );
    }

    #[test]
    fn waits_for_a_program_that_ends_within_its_limit() {
        // Goal: a program that succeeds within its limit passes, and one that fails is
        // reported by its status.
        // Method: `true` and `false`, each with a second to end.
        let mut success =
            StartedProcess::start("true", &mut Command::new("/usr/bin/true")).unwrap();
        let mut failure =
            StartedProcess::start("false", &mut Command::new("/usr/bin/false")).unwrap();

        let succeeded = success.wait(1000);
        let failed = failure.wait(1000);

        assert!(succeeded.is_ok(), "{succeeded:?}");
        assert_eq!(failed.unwrap_err().to_string(), "false: exit status: 1");
    }

    #[test]
    fn stops_a_program_past_its_limit() {
        // Goal: a program that runs past its limit is stopped, and the run fails as timed out.
        // Method: a sleep of ten seconds given a fifth of a second.
        let mut sleep = Command::new("/bin/sleep");
        let mut process = StartedProcess::start("sleep", sleep.arg("10")).unwrap();

        let waited = process.wait(200);

        assert_eq!(waited.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(process.child.try_wait().unwrap().is_some());
    }

    #[test]
    fn stops_a_running_program_when_dropped() {
        // Goal: no way out of the run, an error returned early included, leaves a program it
        // started running, or waits for it to end on its own.
        // Method: drop a sleep of ten seconds, time the drop, then ask ps for its pid, which
        // ps finds only while the process exists.
        let mut sleep = Command::new("/bin/sleep");
        let process = StartedProcess::start("sleep", sleep.arg("10")).unwrap();
        let pid = process.child.id().to_string();
        let started = Instant::now();

        drop(process);

        let elapsed = started.elapsed();
        let listed = Command::new("/bin/ps").args(["-p", &pid]).output().unwrap();
        assert!(elapsed < Duration::from_secs(1), "{elapsed:?}");
        assert!(!listed.status.success(), "{listed:?}");
    }

    #[test]
    fn names_a_program_that_cannot_start() {
        // Goal: a program that cannot start is named, which the operating system's error
        // leaves out.
        // Method: a program at a path that does not exist.
        let mut missing = Command::new("/nonexistent/collector");

        let started = StartedProcess::start("collector", &mut missing);

        assert_eq!(
            started.err().unwrap().to_string(),
            "cannot start /nonexistent/collector: No such file or directory (os error 2)"
        );
    }
}
