use std::io;
use std::process::ExitCode;
use std::time::Duration;

use collector::clock::{self, Timebase};
use collector::collector::Collector;
use collector::cost::{Cost, Meter, Snapshot, Source};
use collector::processes::{Unreadable, Usage};
use collector::schedule::Schedule;
use collector::tracker::{Counts, Event, Noticed};

const QOS_CLASS_DEFAULT: u32 = 0x15;
const TICKS_PER_WINDOW: u64 = 10;
const BUDGET_SHARE_OF_A_CORE: f64 = 0.03;

unsafe extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: libc::c_int)
    -> libc::c_int;
}

fn main() -> ExitCode {
    let seconds = match seconds_from(&std::env::args().skip(1).collect::<Vec<_>>()) {
        Ok(seconds) => seconds,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };

    match run(seconds) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("collector: {error}");
            ExitCode::FAILURE
        }
    }
}

fn seconds_from(args: &[String]) -> Result<Option<u64>, String> {
    let usage = "usage: collector [--seconds N]".to_owned();
    match args {
        [] => Ok(None),
        [flag, seconds] if flag == "--seconds" => seconds.parse().map(Some).map_err(|_| usage),
        _ => Err(usage),
    }
}

fn run(seconds: Option<u64>) -> io::Result<()> {
    let error = unsafe { pthread_set_qos_class_self_np(QOS_CLASS_DEFAULT, 0) };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error));
    }

    let timebase = Timebase::read();
    let mut meter = Meter::default();
    let mut collector = Collector::new(timebase)?;
    let schedule = Schedule::new(clock::now(), timebase.ticks(1_000_000_000));
    println!(
        "started in {:.1} ms of CPU",
        milliseconds(Duration::from_nanos(
            Snapshot::take(&meter, timebase).cpu_ns
        ))
    );

    let mut tick = 0;
    let mut run_start = Mark::default();
    let mut window_start = Mark::default();
    let mut run_tally = Tally::default();
    let mut window_tally = Tally::default();
    loop {
        let deadline = schedule.deadline(tick);
        collector.wait_until(deadline, &mut meter)?;
        let late = Duration::from_nanos(timebase.nanos(clock::now().saturating_sub(deadline)));
        collector.read(&mut meter)?;

        let following = schedule.first_ahead_of(clock::now()).max(tick + 1);
        let slips = following - tick - 1;
        if tick == 0 {
            run_start = Mark::take(&meter, timebase, &collector);
            window_start = run_start;
        } else {
            run_tally.add(late, slips, &collector);
            window_tally.add(late, slips, &collector);
        }

        if tick > 0 && tick % TICKS_PER_WINDOW == 0 {
            let now = Mark::take(&meter, timebase, &collector);
            report(
                &format!("ticks {}-{tick}", tick + 1 - window_tally.ticks),
                &window_start,
                &now,
                &window_tally,
            );
            window_start = now;
            window_tally = Tally::default();
        }

        if seconds.is_some_and(|seconds| following > seconds) {
            let now = Mark::take(&meter, timebase, &collector);
            report(
                &format!("whole run, ticks 1-{tick}"),
                &run_start,
                &now,
                &run_tally,
            );
            return Ok(());
        }
        tick = following;
    }
}

#[derive(Clone, Copy, Default)]
struct Mark {
    snapshot: Snapshot,
    counts: Counts,
}

impl Mark {
    fn take(meter: &Meter, timebase: Timebase, collector: &Collector) -> Self {
        Self {
            snapshot: Snapshot::take(meter, timebase),
            counts: collector.tracker.counts,
        }
    }
}

#[derive(Default)]
struct Tally {
    ticks: u64,
    slips: u64,
    late_total: Duration,
    late_max: Duration,
    starts: u64,
    execs: u64,
    seen_exiting: Exits,
    found_exited: Exits,
    processes_read: usize,
    processes_refused: u32,
    processes_gone: u32,
    coalitions: usize,
    interfaces: usize,
    cpus: usize,
}

impl Tally {
    fn add(&mut self, late: Duration, slips: u64, collector: &Collector) {
        self.ticks += 1;
        self.slips += slips;
        self.late_total += late;
        self.late_max = self.late_max.max(late);

        for event in &collector.events {
            match event {
                Event::Start { .. } => self.starts += 1,
                Event::Exec { .. } => self.execs += 1,
                Event::Exit {
                    last,
                    noticed: Noticed::OnExit,
                    ..
                } => self.seen_exiting.add(last),
                Event::Exit {
                    last,
                    noticed: Noticed::AlreadyExited,
                    ..
                } => self.found_exited.add(last),
            }
        }

        self.processes_read = collector.processes.read.len();
        self.processes_refused = collector.processes.refused;
        self.processes_gone = collector.processes.gone;
        self.coalitions = collector.coalitions.read.len();
        self.interfaces = collector.interfaces.read.len();
        self.cpus = collector.machine.cpus.len();
    }
}

#[derive(Default)]
struct Exits {
    read: u64,
    refused: u64,
    gone: u64,
}

impl Exits {
    fn add(&mut self, last: &Result<Usage, Unreadable>) {
        match last {
            Ok(_) => self.read += 1,
            Err(Unreadable::Refused) => self.refused += 1,
            Err(Unreadable::Gone) => self.gone += 1,
        }
    }
}

impl std::fmt::Display for Exits {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} ({} read, {} refused, {} gone)",
            self.read + self.refused + self.gone,
            self.read,
            self.refused,
            self.gone
        )
    }
}

fn report(label: &str, earlier: &Mark, later: &Mark, tally: &Tally) {
    let cost = Cost::between(&earlier.snapshot, &later.snapshot);
    let counts = later.counts.since(earlier.counts);
    let share = cost.share_of_a_core();
    println!(
        "{label}: CPU {:.2}% of a core, {:.0}% of the budget, {:.2} mW, {:.0}% of instructions on Super cores; ticks late {:.1} ms on average, {:.1} ms at most, {} slips",
        share * 100.0,
        share / BUDGET_SHARE_OF_A_CORE * 100.0,
        cost.milliwatts(),
        cost.share_on_super_cores() * 100.0,
        milliseconds(tally.late_total / u32::try_from(tally.ticks.max(1)).unwrap_or(u32::MAX)),
        milliseconds(tally.late_max),
        tally.slips,
    );

    let sources: Vec<String> = Source::ALL
        .iter()
        .map(|&source| format!("{} {:.2}", source.name(), cost.ms_per_second(source)))
        .collect();
    println!(
        "  ms of CPU a second: {}, unattributed {:.2}",
        sources.join(", "),
        cost.unattributed_ms_per_second()
    );
    println!(
        "  last tick read {} processes ({} refused, {} gone), {} coalitions, {} CPUs, {} interfaces; over the window {} forks, {} starts ({} watches refused), {} execs, exits seen {}, found already exited {}",
        tally.processes_read,
        tally.processes_refused,
        tally.processes_gone,
        tally.coalitions,
        tally.cpus,
        tally.interfaces,
        counts.forks,
        tally.starts,
        counts.refused_watches,
        tally.execs,
        tally.seen_exiting,
        tally.found_exited,
    );
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}
