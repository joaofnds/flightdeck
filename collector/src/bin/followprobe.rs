use std::collections::HashSet;
use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, ExitCode};
use std::time::Duration;

use collector::clock::{self, Timebase};
use collector::cost::{Meter, Snapshot, Source};
use collector::processes;
use collector::tracker::{EVENTS_PER_TICK_MAX, Event, Limits, Tracker};

const TRACKING: [Source; 5] = [
    Source::Events,
    Source::Scans,
    Source::StartReads,
    Source::Watches,
    Source::ExitReads,
];

struct Probe {
    rescan_interval: Duration,
    per_second: u64,
    seconds: u64,
    child_ms: u64,
    generators: u64,
}

fn main() -> ExitCode {
    let probe = match probe_from(&std::env::args().skip(1).collect::<Vec<_>>()) {
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

fn probe_from(args: &[String]) -> Result<Probe, String> {
    let usage = || "usage: followprobe RESCAN_MS PER_SECOND SECONDS CHILD_MS GENERATORS".to_owned();
    let numbers: Vec<u64> = args
        .iter()
        .map(|word| word.parse().map_err(|_| usage()))
        .collect::<Result<_, _>>()?;
    let [rescan_ms, per_second, seconds, child_ms, generators] = numbers[..] else {
        return Err(usage());
    };

    Ok(Probe {
        rescan_interval: Duration::from_millis(rescan_ms),
        per_second,
        seconds,
        child_ms,
        generators: generators.max(1),
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
        .map(|n| std::env::temp_dir().join(format!("followprobe-{}-{n}.pids", std::process::id())))
        .collect();
    let followed = follow(
        probe,
        timebase,
        &mut tracker,
        &mut meter,
        &pid_files,
        process_limit,
    );
    let removed = remove_pid_files(&pid_files);
    let (started, read_at_exit, children) = followed?;
    removed?;

    let cost = Snapshot::take(&meter, timebase)?.cost_since(&start);
    let found = children.iter().filter(|pid| started.contains(pid)).count();
    let read = children
        .iter()
        .filter(|pid| read_at_exit.contains(pid))
        .count();
    let counts = tracker.counts;
    let tracking: Vec<String> = TRACKING
        .iter()
        .map(|&source| format!("{} {:.2}", source.name(), cost.ms_per_second(source)))
        .collect();
    let tracking_total: f64 = TRACKING.iter().map(|&s| cost.ms_per_second(s)).sum();

    println!(
        "rescan interval {} ms: children {}, found {found}, missed {}, \
         final usage read for {read} ({:.1}% of children)",
        probe.rescan_interval.as_millis(),
        children.len(),
        children.len() - found,
        percent(read, children.len()),
    );
    println!(
        "  tracking ms of CPU a second {tracking_total:.2}: {}; process CPU {:.2}% of a core",
        tracking.join(", "),
        cost.share_of_a_core() * 100.0,
    );
    println!(
        "  {} forks, rescans {} leading, {} trailing, {} listing",
        counts.forks, counts.leading_rescans, counts.trailing_rescans, counts.listing_rescans,
    );
    println!(
        "  dropped from a full event list {} starts, {} execs, {} exits",
        counts.dropped_starts, counts.dropped_execs, counts.dropped_exits,
    );
    Ok(())
}

type Followed = (HashSet<i32>, HashSet<i32>, HashSet<i32>);

fn follow(
    probe: &Probe,
    timebase: Timebase,
    tracker: &mut Tracker,
    meter: &mut Meter,
    pid_files: &[PathBuf],
    process_limit: usize,
) -> io::Result<Followed> {
    let mut generators = Vec::new();
    let followed = (|| {
        for file in pid_files {
            generators.push(start_generator(probe, file)?);
        }

        let mut started = HashSet::new();
        let mut read_at_exit = HashSet::new();
        let mut listing = processes::pid_list(process_limit);
        let second = timebase.ticks(1_000_000_000)?;
        let mut deadline = clock::now() + second;
        loop {
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

            if all_exited(&mut generators)? {
                break;
            }
            deadline += second;
        }

        Ok((started, read_at_exit, read_children(pid_files)?))
    })();

    if followed.is_err() {
        for generator in &mut generators {
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
    followed
}

fn remove_pid_files(pid_files: &[PathBuf]) -> io::Result<()> {
    for file in pid_files {
        match std::fs::remove_file(file) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
            _ => {}
        }
    }
    Ok(())
}

fn start_generator(probe: &Probe, pid_file: &std::path::Path) -> io::Result<Child> {
    let forkrate = std::env::current_exe()?.with_file_name("forkrate");
    Command::new(forkrate)
        .arg((probe.per_second / probe.generators).to_string())
        .arg(probe.seconds.to_string())
        .arg(probe.child_ms.to_string())
        .arg("--pids")
        .arg(pid_file)
        .spawn()
}

fn all_exited(generators: &mut [Child]) -> io::Result<bool> {
    for generator in generators {
        if generator.try_wait()?.is_none() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn read_children(pid_files: &[PathBuf]) -> io::Result<HashSet<i32>> {
    let mut children = HashSet::new();
    for file in pid_files {
        for line in std::fs::read_to_string(file)?.lines() {
            let pid = line
                .parse()
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, line.to_owned()))?;
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

fn percent(part: usize, whole: usize) -> f64 {
    let part = u32::try_from(part).unwrap_or(u32::MAX);
    let whole = u32::try_from(whole.max(1)).unwrap_or(u32::MAX);
    f64::from(part) / f64::from(whole) * 100.0
}
