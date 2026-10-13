//! Runs the collector for one minute and, inside it, `followprobe` with a burst of forks, then
//! prints the date, the load average and the power source, the collector's report and
//! followprobe's report, in that order.

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Output};
use std::thread;
use std::time::Duration;

const COLLECTOR_SECONDS: u64 = 60;
const BURST_START_SECONDS: u64 = 15;
const BURST_SECONDS: u64 = 30;
const BURST_CHILD_MS: u64 = 6;
const BURST_GENERATORS: u64 = 2;

fn main() -> ExitCode {
    let options = match options_from(&std::env::args().skip(1).collect::<Vec<_>>()) {
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
    /// The collector's program and its arguments, to which the run adds `--seconds 60`.
    collector: Vec<String>,
}

fn options_from(args: &[String]) -> Result<Options, String> {
    let usage = "usage: burst_minute --rescan-ms N --per-second N -- COLLECTOR [ARGUMENT...]";
    let Some(separator) = args.iter().position(|word| word == "--") else {
        return Err(usage.to_owned());
    };
    let collector = &args[separator + 1..];
    if collector.is_empty() {
        return Err(usage.to_owned());
    }

    let mut rescan_ms = None;
    let mut per_second = None;
    for pair in args[..separator].chunks(2) {
        match pair {
            [flag, number] if flag == "--rescan-ms" => rescan_ms = Some(number_from(flag, number)?),
            [flag, number] if flag == "--per-second" => {
                per_second = Some(number_from(flag, number)?);
            }
            _ => return Err(usage.to_owned()),
        }
    }

    match (rescan_ms, per_second) {
        (Some(rescan_ms), Some(per_second)) => Ok(Options {
            rescan_ms,
            per_second,
            collector: collector.to_vec(),
        }),
        _ => Err(usage.to_owned()),
    }
}

fn number_from(flag: &str, word: &str) -> Result<u64, String> {
    word.parse()
        .map_err(|error| format!("{flag} {word}: {error}"))
}

fn run(options: &Options) -> io::Result<()> {
    const { assert!(BURST_START_SECONDS + BURST_SECONDS < COLLECTOR_SECONDS) };
    assert!(!options.collector.is_empty());

    let followprobe = followprobe_path()?;
    let mut stdout = io::stdout().lock();
    stdout.write_all(&command_stdout("/bin/date", &[])?)?;
    stdout.write_all(&command_stdout("/usr/sbin/sysctl", &["-n", "vm.loadavg"])?)?;
    let power = command_stdout("/usr/bin/pmset", &["-g", "batt"])?;
    match power.split_inclusive(|&byte| byte == b'\n').next() {
        Some(line) => stdout.write_all(line)?,
        None => return Err(io::Error::other("pmset -g batt printed nothing")),
    }
    stdout.flush()?;

    let mut collector = Command::new(&options.collector[0]);
    collector
        .args(&options.collector[1..])
        .args(["--seconds", &COLLECTOR_SECONDS.to_string()]);
    let collector = thread::spawn(move || collector.output());

    thread::sleep(Duration::from_secs(BURST_START_SECONDS));
    let probe = Command::new(followprobe)
        .args(
            [
                options.rescan_ms,
                options.per_second,
                BURST_SECONDS,
                BURST_CHILD_MS,
                BURST_GENERATORS,
            ]
            .map(|number| number.to_string()),
        )
        .output();
    let collector = match collector.join() {
        Ok(collector) => collector,
        Err(panic) => std::panic::resume_unwind(panic),
    };

    let collector = collector?;
    let probe = probe?;
    write_output(&mut stdout, "collector", &collector)?;
    write_output(&mut stdout, "followprobe", &probe)
}

/// The `followprobe` built beside this program, in the same target directory.
fn followprobe_path() -> io::Result<PathBuf> {
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

fn command_stdout(program: &str, args: &[&str]) -> io::Result<Vec<u8>> {
    let output = Command::new(program).args(args).output()?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(io::Error::other(format!("{program}: {}", output.status)))
    }
}

/// Writes what a program printed, its standard output and then its standard error, and
/// fails when the program did.
fn write_output(stdout: &mut impl Write, name: &str, output: &Output) -> io::Result<()> {
    stdout.write_all(&output.stdout)?;
    stdout.write_all(&output.stderr)?;
    stdout.flush()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!("{name}: {}", output.status)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(words: &[&str]) -> Vec<String> {
        words.iter().map(|&word| word.to_owned()).collect()
    }

    #[test]
    fn takes_the_flags_and_the_collector_command() {
        // Goal: the flags set the probe's rate, and what follows `--` is the collector's
        // command, its own flags included.
        // Method: every flag, and a collector command with a flag of its own.
        let args = arguments(&[
            "--rescan-ms",
            "5",
            "--per-second",
            "1000",
            "--",
            "./collector",
            "--rescan-ms",
            "5",
        ]);

        let options = options_from(&args);

        assert_eq!(
            options,
            Ok(Options {
                rescan_ms: 5,
                per_second: 1000,
                collector: arguments(&["./collector", "--rescan-ms", "5"]),
            })
        );
    }

    #[test]
    fn refuses_a_missing_collector_command() {
        // Goal: a run needs a collector to measure.
        // Method: the flags with no `--`, and with nothing after it.
        let without_separator = arguments(&["--rescan-ms", "5", "--per-second", "1000"]);
        let without_command = arguments(&["--rescan-ms", "5", "--per-second", "1000", "--"]);

        assert!(options_from(&without_separator).is_err());
        assert!(options_from(&without_command).is_err());
    }

    #[test]
    fn refuses_a_missing_flag() {
        // Goal: the probe's rate has no default, as the script it replaces had none.
        // Method: each flag left out in turn.
        let without_rescan = arguments(&["--per-second", "1000", "--", "./collector"]);
        let without_rate = arguments(&["--rescan-ms", "5", "--", "./collector"]);

        assert!(options_from(&without_rescan).is_err());
        assert!(options_from(&without_rate).is_err());
    }

    #[test]
    fn refuses_a_flag_that_is_not_a_number() {
        // Goal: a rate that is not a number fails before the minute starts.
        // Method: a word in place of the interval.
        let args = arguments(&["--rescan-ms", "five", "--per-second", "1000", "--", "c"]);

        assert!(options_from(&args).is_err());
    }
}
