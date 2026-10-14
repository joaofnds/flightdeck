use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode};
use std::time::Duration;

use collector::clock::{self, Timebase};
use collector::cost::{Cost, Meter, Snapshot, Source};
use collector::processes;
use collector::tracker::{Counts, EVENTS_PER_TICK_MAX, Event, Limits, Tracker};

const TRACKING: [Source; 5] = [
    Source::Events,
    Source::Scans,
    Source::StartReads,
    Source::Watches,
    Source::ExitReads,
];

/// The seconds the probe follows past the generators' run and their children's sleep before
/// it gives up on them.
const FOLLOW_SECONDS_PAST_RUN: u64 = 10;

/// The most generators a probe runs, well past the two that `burst_minute` and the measured
/// research ran, so a probe's pid files stay a bounded list.
const GENERATORS_MAX: u64 = 64;

fn main() -> ExitCode {
    let probe =
        match collector::arguments("followprobe").and_then(|arguments| probe_from(&arguments)) {
            Ok(probe) => probe,
            Err(usage) => {
                eprintln!("{usage}");
                return ExitCode::from(2);
            }
        };

    match run(&probe) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("followprobe: {error}");
            ExitCode::FAILURE
        }
    }
}

/// A probe of `generators` forkrate processes forking `per_second` children a second between
/// them for `seconds`, each child sleeping `child_ms`, followed with `rescan_interval`.
#[derive(Debug, PartialEq)]
struct Probe {
    rescan_interval: Duration,
    per_second: u64,
    seconds: u32,
    child_ms: u32,
    generators: u64,
}

fn probe_from(arguments: &[String]) -> Result<Probe, String> {
    let usage = || "usage: followprobe RESCAN_MS PER_SECOND SECONDS CHILD_MS GENERATORS".to_owned();
    let [rescan_ms, per_second, seconds, child_ms, generators] = arguments else {
        return Err(usage());
    };
    let rescan_ms: u64 = rescan_ms.parse().map_err(|_| usage())?;
    let per_second: u64 = per_second.parse().map_err(|_| usage())?;
    let seconds: u32 = seconds.parse().map_err(|_| usage())?;
    let child_ms: u32 = child_ms.parse().map_err(|_| usage())?;
    let generators: u64 = generators.parse().map_err(|_| usage())?;

    if generators == 0 {
        return Err("followprobe: a probe of 0 generators forks nothing".to_owned());
    }
    if generators > GENERATORS_MAX {
        return Err(format!(
            "followprobe: {generators} generators pass the {GENERATORS_MAX} a probe runs"
        ));
    }
    if per_second < generators {
        return Err(format!(
            "followprobe: a rate of {per_second} a second leaves some of {generators} \
             generators no fork a second"
        ));
    }

    assert!(generators > 0);
    assert!(generators <= GENERATORS_MAX);
    assert!(per_second / generators > 0);
    Ok(Probe {
        rescan_interval: Duration::from_millis(rescan_ms),
        per_second,
        seconds,
        child_ms,
        generators,
    })
}

fn run(probe: &Probe) -> io::Result<()> {
    let timebase = Timebase::read()?;
    let process_limit = processes::process_limit()?;
    let limits = Limits {
        process_limit,
        events_per_tick_max: EVENTS_PER_TICK_MAX,
    };
    let mut tracker = Tracker::new(timebase, probe.rescan_interval, limits)?;
    let mut meter = Meter::default();
    let start = Snapshot::take(&meter, timebase)?;

    let pid_files: Vec<PathBuf> = (0..probe.generators)
        .map(|generator| {
            std::env::temp_dir().join(format!(
                "followprobe-{}-{generator}.pids",
                std::process::id()
            ))
        })
        .collect();
    let followed = run_follow(
        probe,
        timebase,
        &mut tracker,
        &mut meter,
        &pid_files,
        process_limit,
    );
    let removed = run_remove_pid_files(&pid_files);
    let followed = followed?;
    removed?;

    let cost = Snapshot::take(&meter, timebase)?.cost_since(&start)?;
    run_report(probe, &followed, &cost, &tracker.counts())
}

fn run_report(probe: &Probe, followed: &Followed, cost: &Cost, counts: &Counts) -> io::Result<()> {
    let children = run_report_children(followed)?;
    let tracking: Vec<String> = TRACKING
        .iter()
        .map(|&source| format!("{} {:.2}", source.name(), cost.ms_per_second(source)))
        .collect();
    let tracking_total: f64 = TRACKING
        .iter()
        .map(|&source| cost.ms_per_second(source))
        .sum();

    let mut out = io::stdout().lock();
    writeln!(
        out,
        "rescan interval {} ms: children {}, found {}, missed {}, \
         final usage read for {} ({:.1}% of children)",
        probe.rescan_interval.as_millis(),
        children.forked,
        children.found,
        children.forked - children.found,
        children.read,
        children.read_percent(),
    )?;
    writeln!(
        out,
        "  tracking ms of CPU a second {tracking_total:.2}: {}; process CPU {:.2}% of a core",
        tracking.join(", "),
        cost.share_of_a_core() * 100.0,
    )?;
    writeln!(
        out,
        "  {} forks, rescans {} leading, {} trailing, {} listing",
        counts.forks, counts.leading_rescans, counts.trailing_rescans, counts.listing_rescans,
    )?;
    writeln!(
        out,
        "  dropped from a full event list {} starts, {} execs, {} exits",
        counts.dropped_starts, counts.dropped_execs, counts.dropped_exits,
    )?;
    Ok(())
}

/// The pids the tracker reported started, those whose final usage it read at exit, and those
/// the generators forked.
struct Followed {
    started: HashSet<i32>,
    read_at_exit: HashSet<i32>,
    children: HashSet<i32>,
}

/// The children the generators forked, how many of them the tracker reported started, and
/// how many of them it read the final usage of at exit.
#[derive(Debug, PartialEq)]
struct Children {
    forked: u32,
    found: u32,
    read: u32,
}

impl Children {
    /// The share of the children forked whose final usage the tracker read, as a percentage.
    fn read_percent(&self) -> f64 {
        assert!(self.forked > 0);
        assert!(self.read <= self.forked);

        f64::from(self.read) / f64::from(self.forked) * 100.0
    }
}

/// Counts the children `followed` holds, or an error where the generators wrote none, since a
/// share of no children is no measurement.
fn run_report_children(followed: &Followed) -> io::Result<Children> {
    let Ok(forked) = u32::try_from(followed.children.len()) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "{} children pass the {} a probe counts",
                followed.children.len(),
                u32::MAX
            ),
        ));
    };

    if forked == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the generators wrote no children's pids",
        ));
    }

    let mut children = Children {
        forked,
        found: 0,
        read: 0,
    };
    for pid in &followed.children {
        if followed.started.contains(pid) {
            children.found += 1;
        }
        if followed.read_at_exit.contains(pid) {
            children.read += 1;
        }
    }

    assert!(children.found <= children.forked);
    assert!(children.read <= children.forked);
    Ok(children)
}

/// Starts a generator for each of `pid_files` and follows their children once a second
/// until every generator has exited, or until the generators run past their run and their
/// children's sleep by `FOLLOW_SECONDS_PAST_RUN`.
fn run_follow(
    probe: &Probe,
    timebase: Timebase,
    tracker: &mut Tracker,
    meter: &mut Meter,
    pid_files: &[PathBuf],
    process_limit: usize,
) -> io::Result<Followed> {
    let mut generators = Generators::start(probe, pid_files)?;

    let mut started = HashSet::new();
    let mut read_at_exit = HashSet::new();
    let mut listing = processes::pid_list(process_limit);
    let second = timebase.ticks(1_000_000_000)?;
    let mut deadline = clock::now() + second;
    // The children's sleep rounds up to a whole second.
    let seconds_max = u64::from(probe.seconds)
        + u64::from(probe.child_ms.div_ceil(1_000))
        + FOLLOW_SECONDS_PAST_RUN;

    for _ in 0..seconds_max {
        tracker.follow_until(deadline, meter)?;
        processes::list_pids(&mut listing)?;
        tracker.observe(&listing, meter)?;
        for event in tracker.drain_events() {
            match event {
                Event::Start { pid, .. } => {
                    started.insert(pid);
                }
                Event::Exit {
                    pid, last: Ok(_), ..
                } => {
                    read_at_exit.insert(pid);
                }
                Event::Exit { .. } | Event::Exec { .. } => {}
            }
        }

        if generators.all_exited()? {
            return Ok(Followed {
                started,
                read_at_exit,
                children: run_follow_read_children(pid_files)?,
            });
        }

        deadline += second;
    }

    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("the generators ran past {seconds_max} seconds"),
    ))
}

/// The forkrate processes a probe runs, which are stopped and reaped when dropped, so a
/// probe that fails leaves none running.
struct Generators {
    running: Vec<Child>,
}

impl Generators {
    fn start(probe: &Probe, pid_files: &[PathBuf]) -> io::Result<Self> {
        let mut generators = Self {
            running: Vec::with_capacity(pid_files.len()),
        };
        for pid_file in pid_files {
            generators.running.push(start_generator(probe, pid_file)?);
        }

        assert_eq!(pid_files.len(), generators.running.len());
        Ok(generators)
    }

    /// Whether every generator has exited, or an error where any one exited with a failure,
    /// since the children it wrote may then be short of those it forked.
    fn all_exited(&mut self) -> io::Result<bool> {
        let mut all_exited = true;
        for generator in &mut self.running {
            match generator.try_wait()? {
                None => all_exited = false,
                Some(status) => {
                    if !status.success() {
                        return Err(io::Error::other(format!(
                            "generator {} exited with {status}",
                            generator.id()
                        )));
                    }
                }
            }
        }
        Ok(all_exited)
    }
}

impl Drop for Generators {
    fn drop(&mut self) {
        for generator in &mut self.running {
            match generator.try_wait() {
                Ok(Some(_)) => continue,
                Ok(None) => {}
                Err(error) => eprintln!(
                    "followprobe: cannot check generator {}: {error}",
                    generator.id()
                ),
            }
            if let Err(error) = generator.kill() {
                eprintln!(
                    "followprobe: cannot stop generator {}: {error}",
                    generator.id()
                );
            }
            if let Err(error) = generator.wait() {
                eprintln!(
                    "followprobe: cannot reap generator {}: {error}",
                    generator.id()
                );
            }
        }
    }
}

fn start_generator(probe: &Probe, pid_file: &Path) -> io::Result<Child> {
    // Each generator's rate floors, so the generators fork up to one a second fewer each
    // than the probe's rate.
    let per_generator = probe.per_second / probe.generators;
    assert!(per_generator > 0);

    let forkrate = std::env::current_exe()?.with_file_name("forkrate");
    Command::new(forkrate)
        .arg(per_generator.to_string())
        .arg(probe.seconds.to_string())
        .arg(probe.child_ms.to_string())
        .arg("--pids")
        .arg(pid_file)
        .spawn()
}

fn run_remove_pid_files(pid_files: &[PathBuf]) -> io::Result<()> {
    for file in pid_files {
        if let Err(error) = std::fs::remove_file(file) {
            if error.kind() == io::ErrorKind::NotFound {
                // A generator that failed before it wrote its pids left no file.
                continue;
            }
            return Err(error);
        }
    }
    Ok(())
}

fn run_follow_read_children(pid_files: &[PathBuf]) -> io::Result<HashSet<i32>> {
    let mut children = HashSet::new();
    for file in pid_files {
        for line in std::fs::read_to_string(file)?.lines() {
            let pid = line.parse().map_err(|error| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{line:?} in {} is not a pid: {error}", file.display()),
                )
            })?;
            if !children.insert(pid) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("pid {pid} forked twice, so the pids wrapped"),
                ));
            }
        }
    }
    Ok(children)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(words: &[&str]) -> Vec<String> {
        words.iter().map(|&word| word.to_owned()).collect()
    }

    /// Writes `contents` to a file of its own under the temporary directory, named for
    /// `test`, and returns its path.
    fn pid_file(test: &str, contents: &str) -> PathBuf {
        let file = std::env::temp_dir().join(format!("{test}-{}.pids", std::process::id()));
        std::fs::write(&file, contents).unwrap();
        file
    }

    #[test]
    fn takes_the_interval_the_rate_the_run_the_child_time_and_the_generators() {
        // Goal: a probe takes its five numbers in order.
        // Method: parse a 20 ms interval, 300 forks a second, 15 seconds, 6 ms children and
        // 3 generators.
        assert_eq!(
            Ok(Probe {
                rescan_interval: Duration::from_millis(20),
                per_second: 300,
                seconds: 15,
                child_ms: 6,
                generators: 3,
            }),
            probe_from(&arguments(&["20", "300", "15", "6", "3"]))
        );
    }

    #[test]
    fn refuses_a_missing_number() {
        // Goal: a probe given four numbers is a usage error.
        // Method: parse the numbers without the generators.
        assert!(probe_from(&arguments(&["20", "300", "15", "6"])).is_err());
    }

    #[test]
    fn refuses_no_generators() {
        // Goal: a probe of no generators is a usage error, not a probe of one.
        // Method: parse 0 generators.
        assert!(probe_from(&arguments(&["0", "300", "15", "6", "0"])).is_err());
    }

    #[test]
    fn takes_as_many_generators_as_a_probe_runs() {
        // Goal: a probe of GENERATORS_MAX generators is taken.
        // Method: parse GENERATORS_MAX generators at a rate that gives each one a fork a
        // second.
        let generators = GENERATORS_MAX.to_string();

        let probe = probe_from(&arguments(&["0", "1000", "15", "6", &generators]));

        assert_eq!(GENERATORS_MAX, probe.unwrap().generators);
    }

    #[test]
    fn refuses_more_generators_than_a_probe_runs() {
        // Goal: a probe of more generators than GENERATORS_MAX is a usage error, not a list
        // of pid files sized to whatever was asked.
        // Method: parse one generator past GENERATORS_MAX, at a rate that gives each one a
        // fork a second.
        let generators = (GENERATORS_MAX + 1).to_string();

        let probe = probe_from(&arguments(&["0", "1000", "15", "6", &generators]));

        assert!(probe.is_err());
    }

    #[test]
    fn takes_a_rate_of_one_fork_a_second_for_each_generator() {
        // Goal: a rate that leaves each generator exactly one fork a second is taken.
        // Method: parse a rate of 4 a second over 4 generators.
        let probe = probe_from(&arguments(&["0", "4", "15", "6", "4"]));

        assert_eq!(4, probe.unwrap().per_second);
    }

    #[test]
    fn refuses_a_rate_under_one_fork_a_second_for_each_generator() {
        // Goal: a rate that leaves a generator no forks a second, once it floors, is a usage
        // error.
        // Method: parse a rate of 3 a second over 4 generators.
        assert!(probe_from(&arguments(&["0", "3", "15", "6", "4"])).is_err());
    }

    #[test]
    fn generators_that_each_succeeded_have_all_exited() {
        // Goal: generators that each exited successfully have all exited.
        // Method: wait on a child that runs true, then ask whether the generators exited.
        let mut succeeded = Command::new("/usr/bin/true").spawn().unwrap();
        succeeded.wait().unwrap();
        let mut generators = Generators {
            running: vec![succeeded],
        };

        let exited = generators.all_exited();

        assert!(exited.unwrap());
    }

    #[test]
    fn refuses_a_generator_that_failed() {
        // Goal: a generator that exits with a failure fails the probe, since the children it
        // wrote may be short of those it forked.
        // Method: wait on a child that runs false, then ask whether the generators exited.
        let mut failed = Command::new("/usr/bin/false").spawn().unwrap();
        failed.wait().unwrap();
        let mut generators = Generators {
            running: vec![failed],
        };

        let exited = generators.all_exited();

        assert_eq!(io::ErrorKind::Other, exited.unwrap_err().kind());
    }

    #[test]
    fn a_running_generator_has_not_exited() {
        // Goal: generators have not all exited while one still runs.
        // Method: start a child that sleeps a minute, then ask whether the generators exited.
        let running = Command::new("/bin/sleep").arg("60").spawn().unwrap();
        let mut generators = Generators {
            running: vec![running],
        };

        let exited = generators.all_exited();

        assert!(!exited.unwrap());
    }

    #[test]
    fn refuses_a_failed_generator_behind_a_running_one() {
        // Goal: a generator that failed fails the probe at once, though one before it still
        // runs, rather than once that one exits.
        // Method: put a child that sleeps a minute before a child that ran false and exited,
        // then ask whether the generators exited.
        let running = Command::new("/bin/sleep").arg("60").spawn().unwrap();
        let mut failed = Command::new("/usr/bin/false").spawn().unwrap();
        failed.wait().unwrap();
        let mut generators = Generators {
            running: vec![running, failed],
        };

        let exited = generators.all_exited();

        assert_eq!(io::ErrorKind::Other, exited.unwrap_err().kind());
    }

    #[test]
    fn dropping_the_generators_stops_those_still_running() {
        // Goal: generators dropped while one still runs stop it, rather than waiting for it,
        // so a probe that fails leaves none running.
        // Method: drop generators holding a child that sleeps a minute, timing the drop
        // against half that minute.
        let running = Command::new("/bin/sleep").arg("60").spawn().unwrap();
        let generators = Generators {
            running: vec![running],
        };
        let started = std::time::Instant::now();

        drop(generators);

        assert!(started.elapsed() < Duration::from_secs(30));
    }

    #[test]
    fn reads_the_children_each_generator_wrote() {
        // Goal: the children are the pids every generator's file holds.
        // Method: read two files of two pids each.
        let first = pid_file("reads_the_children_first", "101\n102\n");
        let second = pid_file("reads_the_children_second", "201\n202\n");

        let children = run_follow_read_children(&[first.clone(), second.clone()]);
        std::fs::remove_file(first).unwrap();
        std::fs::remove_file(second).unwrap();

        assert_eq!(HashSet::from([101, 102, 201, 202]), children.unwrap());
    }

    #[test]
    fn refuses_a_pid_forked_twice() {
        // Goal: a pid two children had means the pids wrapped, which is an error, since the
        // set of children would count it once.
        // Method: read a file that holds pid 101 twice.
        let file = pid_file("refuses_a_pid_forked_twice", "101\n102\n101\n");

        let children = run_follow_read_children(std::slice::from_ref(&file));
        std::fs::remove_file(file).unwrap();

        assert_eq!(io::ErrorKind::InvalidData, children.unwrap_err().kind());
    }

    #[test]
    fn refuses_a_line_that_is_not_a_pid() {
        // Goal: a line a generator wrote that is not a pid is an error.
        // Method: read a file whose second line is a word.
        let file = pid_file("refuses_a_line_that_is_not_a_pid", "101\nforked\n");

        let children = run_follow_read_children(std::slice::from_ref(&file));
        std::fs::remove_file(file).unwrap();

        assert_eq!(io::ErrorKind::InvalidData, children.unwrap_err().kind());
    }

    #[test]
    fn counts_the_children_the_tracker_found_and_read() {
        // Goal: a probe counts each child the generators forked, those the tracker reported
        // started and those whose final usage it read, and no pid outside the children.
        // Method: follow four children, two found, one of them read, and a started pid that
        // is not a child.
        let followed = Followed {
            started: HashSet::from([101, 102, 900]),
            read_at_exit: HashSet::from([102]),
            children: HashSet::from([101, 102, 103, 104]),
        };

        let children = run_report_children(&followed);

        assert_eq!(
            Children {
                forked: 4,
                found: 2,
                read: 1,
            },
            children.unwrap()
        );
    }

    #[test]
    fn refuses_a_probe_whose_generators_wrote_no_children() {
        // Goal: a probe whose generators wrote no pids is an error before its report, not a
        // report of no children.
        // Method: count a probe that followed no children.
        let followed = Followed {
            started: HashSet::from([900]),
            read_at_exit: HashSet::new(),
            children: HashSet::new(),
        };

        let children = run_report_children(&followed);

        assert_eq!(io::ErrorKind::InvalidData, children.unwrap_err().kind());
    }

    #[test]
    fn gives_the_share_of_the_children_read_as_a_percentage() {
        // Goal: the children whose final usage the tracker read read as a percentage of the
        // children forked.
        // Method: take 3 children read of 12 forked.
        let children = Children {
            forked: 12,
            found: 12,
            read: 3,
        };

        assert!((children.read_percent() - 25.0).abs() < f64::EPSILON);
    }

    #[test]
    #[should_panic(expected = "self.forked > 0")]
    fn refuses_a_share_of_no_children() {
        // Goal: a percentage of no children is never asked for, since run_report_children
        // refuses a probe with none before the report asks for one.
        // Method: take the share read of no children.
        let children = Children {
            forked: 0,
            found: 0,
            read: 0,
        };

        children.read_percent();
    }
}
