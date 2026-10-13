//! Runs the collector for one minute and, inside it, `followprobe` with a burst of forks, then
//! prints the date, the load average and the power source, the collector's report and
//! followprobe's report, in that order. Each report holds what its program wrote to standard
//! output and standard error, in the order it wrote them.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus, Stdio};
use std::thread;
use std::time::Duration;

const COLLECTOR_SECONDS: u64 = 60;
const BURST_START_SECONDS: u64 = 15;
const BURST_SECONDS: u64 = 30;
const BURST_CHILD_MS: u64 = 6;
const BURST_GENERATORS: u64 = 2;
/// How long past its own run a program may take to end before the run stops it.
const GRACE_SECONDS: u64 = 30;
const POLL_MS: u64 = 100;
const USAGE: &str = "usage: burst_minute --rescan-ms N --per-second N -- COLLECTOR [ARGUMENT...]";

fn main() -> ExitCode {
    let words: Vec<String> = std::env::args().skip(1).collect();
    let options = match options_from(&words) {
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

/// The program that runs as the collector and its own arguments, to which the run adds
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
        match number.parse() {
            Ok(parsed) => *target = Some(parsed),
            Err(error) => return Err(format!("{flag} {number}: {error}")),
        }
    }

    match (rescan_ms, per_second) {
        (Some(rescan_ms), Some(per_second)) => {
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
        _ => Err(USAGE.to_owned()),
    }
}

fn run(options: &Options) -> io::Result<()> {
    const { assert!(BURST_START_SECONDS + BURST_SECONDS < COLLECTOR_SECONDS) };
    const { assert!(BURST_GENERATORS > 0) };
    assert!(!options.collector.program.is_empty());

    let followprobe = run_followprobe_path()?;
    let mut stdout = io::stdout().lock();
    run_write_conditions(&mut stdout)?;

    let reports = Reports {
        collector: ReportFile::create("collector")?,
        followprobe: ReportFile::create("followprobe")?,
    };
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
    let program = &options.collector.program;
    let mut collector = match Command::new(program)
        .args(run_collector_arguments(&options.collector))
        .stdin(Stdio::null())
        .stdout(reports.collector.stdio()?)
        .stderr(reports.collector.stdio()?)
        .spawn()
    {
        Ok(collector) => collector,
        Err(error) => return Err(run_start_error(program, &error)),
    };

    thread::sleep(Duration::from_secs(BURST_START_SECONDS));
    let burst = match collector.try_wait()? {
        Some(status) => Err(io::Error::other(format!(
            "the collector ended before the burst, {status}"
        ))),
        None => run_burst(options, followprobe, &reports.followprobe),
    };
    let minute = run_wait(
        &mut collector,
        "collector",
        (COLLECTOR_SECONDS + GRACE_SECONDS) * 1000,
    );

    minute?;
    burst
}

/// Runs followprobe for the burst and waits for it to end.
fn run_burst(options: &Options, followprobe: &Path, report: &ReportFile) -> io::Result<()> {
    let mut child = match Command::new(followprobe)
        .args(run_followprobe_arguments(options))
        .stdin(Stdio::null())
        .stdout(report.stdio()?)
        .stderr(report.stdio()?)
        .spawn()
    {
        Ok(child) => child,
        Err(error) => return Err(run_start_error(&followprobe.display().to_string(), &error)),
    };

    run_wait(
        &mut child,
        "followprobe",
        (BURST_SECONDS + GRACE_SECONDS) * 1000,
    )
}

/// Names the program that could not start, which the operating system's error leaves out.
fn run_start_error(program: &str, error: &io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("cannot start {program}: {error}"))
}

/// Lists the collector's arguments, its own and then the minute it runs for.
fn run_collector_arguments(collector: &CollectorCommand) -> Vec<String> {
    let mut arguments = collector.arguments.clone();
    arguments.push("--seconds".to_owned());
    arguments.push(COLLECTOR_SECONDS.to_string());

    assert_eq!(arguments.len(), collector.arguments.len() + 2);
    arguments
}

/// Lists followprobe's arguments in the order it reads them, `RESCAN_MS PER_SECOND SECONDS
/// CHILD_MS GENERATORS`.
fn run_followprobe_arguments(options: &Options) -> [String; 5] {
    [
        options.rescan_ms,
        options.per_second,
        BURST_SECONDS,
        BURST_CHILD_MS,
        BURST_GENERATORS,
    ]
    .map(|number| number.to_string())
}

/// Waits up to `limit_ms` for `child` to end, then stops it. Fails unless the child ended
/// within the limit and succeeded.
fn run_wait(child: &mut Child, name: &str, limit_ms: u64) -> io::Result<()> {
    const { assert!(POLL_MS > 0) };
    assert!(limit_ms >= POLL_MS);

    for _ in 0..limit_ms.div_ceil(POLL_MS) {
        match child.try_wait()? {
            Some(status) => return run_status(name, status),
            None => thread::sleep(Duration::from_millis(POLL_MS)),
        }
    }

    child.kill()?;
    let status = child.wait()?;
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("{name} ran past {limit_ms} ms and was stopped, {status}"),
    ))
}

fn run_status(name: &str, status: ExitStatus) -> io::Result<()> {
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{name}: {status}")))
    }
}

/// Finds the `followprobe` built beside this program, in the same target directory.
fn run_followprobe_path() -> io::Result<PathBuf> {
    let path = std::env::current_exe()?.with_file_name("followprobe");
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
    stdout.write_all(&run_command_stdout("/bin/date", &[])?)?;
    stdout.write_all(&run_command_stdout(
        "/usr/sbin/sysctl",
        &["-n", "vm.loadavg"],
    )?)?;
    let power = run_command_stdout("/usr/bin/pmset", &["-g", "batt"])?;
    match run_first_line(&power) {
        Some(line) => stdout.write_all(line)?,
        None => return Err(io::Error::other("pmset -g batt printed nothing")),
    }
    stdout.flush()
}

/// Runs a program to its end and returns what it wrote to standard output, or fails with
/// its status and what it wrote to standard error.
fn run_command_stdout(program: &str, arguments: &[&str]) -> io::Result<Vec<u8>> {
    let output = Command::new(program)
        .args(arguments)
        .stdin(Stdio::null())
        .output()?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(io::Error::other(format!(
            "{program}: {}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

/// Returns the first line of `text` with its newline, or `None` when `text` is empty.
fn run_first_line(text: &[u8]) -> Option<&[u8]> {
    let line = text.split_inclusive(|&byte| byte == b'\n').next();
    if let Some(line) = line {
        assert!(!line.is_empty());
        assert!(text.starts_with(line));
    }
    line
}

/// The two report files of one run, named so they cannot be swapped.
struct Reports {
    collector: ReportFile,
    followprobe: ReportFile,
}

/// A file in the temporary directory that one program writes its report to, removed when
/// dropped.
struct ReportFile {
    path: PathBuf,
    file: File,
}

impl ReportFile {
    fn create(name: &str) -> io::Result<ReportFile> {
        let path =
            std::env::temp_dir().join(format!("burst_minute-{}-{name}.txt", std::process::id()));
        let file = OpenOptions::new()
            .read(false)
            .write(true)
            .append(false)
            .truncate(true)
            .create(true)
            .create_new(false)
            .open(&path)?;

        Ok(ReportFile { path, file })
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

impl Drop for ReportFile {
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
    fn gives_the_collector_its_own_arguments_and_then_the_minute() {
        // Goal: the collector runs for the minute, after whatever flags it was given.
        // Method: a collector command with a flag of its own.
        let collector = CollectorCommand {
            program: "./collector".to_owned(),
            arguments: arguments(&["--rescan-ms", "5"]),
        };

        let collector_arguments = run_collector_arguments(&collector);

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

        let followprobe_arguments = run_followprobe_arguments(&options);

        assert_eq!(followprobe_arguments, ["5", "1000", "30", "6", "2"]);
    }

    #[test]
    fn takes_the_first_line_of_the_power_source() {
        // Goal: the power source is pmset's first line, with its newline, and nothing when
        // pmset printed nothing.
        // Method: two lines, then no text.
        let power = b"Now drawing from 'AC Power'\n -InternalBattery-0\n";

        assert_eq!(
            run_first_line(power),
            Some(&b"Now drawing from 'AC Power'\n"[..])
        );
        assert_eq!(run_first_line(b""), None);
    }

    #[test]
    fn waits_for_a_program_that_ends_within_its_limit() {
        // Goal: a program that succeeds within its limit passes, and one that fails is
        // reported by its status.
        // Method: `true` and `false`, each with a second to end.
        let mut success = Command::new("/usr/bin/true").spawn().unwrap();
        let mut failure = Command::new("/usr/bin/false").spawn().unwrap();

        let succeeded = run_wait(&mut success, "true", 1000);
        let failed = run_wait(&mut failure, "false", 1000);

        assert!(succeeded.is_ok(), "{succeeded:?}");
        assert_eq!(failed.unwrap_err().to_string(), "false: exit status: 1");
    }

    #[test]
    fn stops_a_program_past_its_limit() {
        // Goal: a program that runs past its limit is stopped, and the run fails as timed out.
        // Method: a sleep of ten seconds given a fifth of a second.
        let mut sleep = Command::new("/bin/sleep").arg("10").spawn().unwrap();

        let waited = run_wait(&mut sleep, "sleep", 200);

        assert_eq!(waited.unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert!(sleep.try_wait().unwrap().is_some());
    }
}
