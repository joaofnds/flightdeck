use std::io;
use std::num::NonZeroU64;
use std::process::ExitCode;
use std::time::Duration;

use collector::clock::{self, Timebase};
use collector::collector::Collector;
use collector::cost::{Meter, Snapshot, Source};
use collector::processes::{self, Unreadable, Usage};
use collector::schedule::Schedule;
use collector::tracker::{Counts, Event, Noticed};

const QOS_CLASS_DEFAULT: u32 = 0x15;
const TICKS_PER_WINDOW: u64 = 10;
const TICKS_PER_MINUTE: u64 = 60;
const PID_LAP: u64 = 99_999;
const BUDGET_SHARE_OF_A_CORE: f64 = 0.03;
const DEFAULT_RESCAN_INTERVAL: Duration = Duration::from_millis(5);

unsafe extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: libc::c_int)
    -> libc::c_int;
}

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
            eprintln!("collector: {error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, PartialEq)]
struct Options {
    seconds: Option<NonZeroU64>,
    rescan_interval: Duration,
}

fn options_from(args: &[String]) -> Result<Options, String> {
    let usage = || "usage: collector [--seconds N] [--rescan-ms N]".to_owned();
    let mut options = Options {
        seconds: None,
        rescan_interval: DEFAULT_RESCAN_INTERVAL,
    };

    for pair in args.chunks(2) {
        match pair {
            [flag, seconds] if flag == "--seconds" => {
                options.seconds = Some(seconds.parse().map_err(|_| usage())?);
            }
            [flag, millis] if flag == "--rescan-ms" => {
                options.rescan_interval =
                    Duration::from_millis(millis.parse().map_err(|_| usage())?);
            }
            _ => return Err(usage()),
        }
    }

    Ok(options)
}

fn run(options: &Options) -> io::Result<()> {
    let error = unsafe { pthread_set_qos_class_self_np(QOS_CLASS_DEFAULT, 0) };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error));
    }

    let timebase = Timebase::read();
    let mut meter = Meter::default();
    let first_mark = throwaway_child()?;
    let mut collector = Collector::new(timebase, options.rescan_interval)?;
    let mut live_at_first_scan = Vec::new();
    processes::list_pids(&mut live_at_first_scan)?;
    let schedule = Schedule::new(clock::now(), timebase.ticks(1_000_000_000));

    println!(
        "started in {:.1} ms of CPU",
        milliseconds(Duration::from_nanos(
            Snapshot::take(&meter, timebase).cpu_ns
        ))
    );

    collector.wait_until(schedule.deadline(0), &mut meter)?;
    collector.read(&mut meter)?;
    let warm_up_starts = starts_in(&collector.events);

    let mut tick = schedule.first_ahead_of(clock::now()).max(1);
    let start = Mark::take(&meter, timebase, &collector);
    let mut whole = Span::begin(start, tick);
    let mut minute = Span::begin(start, tick);
    let mut window = Span::begin(start, tick);
    loop {
        let deadline = schedule.deadline(tick);
        collector.wait_until(deadline, &mut meter)?;
        let late = Duration::from_nanos(timebase.nanos(clock::now().saturating_sub(deadline)));
        collector.read(&mut meter)?;

        let following = schedule.first_ahead_of(clock::now()).max(tick + 1);
        let slips = following - tick - 1;
        whole.tally.add(late, slips, &collector);
        minute.tally.add(late, slips, &collector);
        window.tally.add(late, slips, &collector);

        if closes_span(tick, following, TICKS_PER_WINDOW) {
            let now = Mark::take(&meter, timebase, &collector);
            window.report("ticks", tick, &now);
            window = Span::begin(now, following);
        }
        if closes_span(tick, following, TICKS_PER_MINUTE) {
            let now = Mark::take(&meter, timebase, &collector);
            minute.report("minute, ticks", tick, &now);
            minute = Span::begin(now, following);
        }

        if options
            .seconds
            .is_some_and(|seconds| following > seconds.get())
        {
            let last_mark = throwaway_child()?;
            let end = Mark::take(&meter, timebase, &collector);
            whole.report("whole run, ticks", tick, &end);
            report_missed(
                processes_started(first_mark, last_mark, &live_at_first_scan, end.counts.forks),
                warm_up_starts + whole.tally.starts,
            );
            return Ok(());
        }

        tick = following;
    }
}

struct Span {
    start: Mark,
    first_tick: u64,
    tally: Tally,
}

impl Span {
    fn begin(start: Mark, first_tick: u64) -> Self {
        Self {
            start,
            first_tick,
            tally: Tally::default(),
        }
    }

    fn report(&self, label: &str, last_tick: u64, end: &Mark) {
        report(
            &format!("{label} {}-{last_tick}", self.first_tick),
            &self.start,
            end,
            &self.tally,
        );
    }
}

fn closes_span(tick: u64, following: u64, ticks_per_span: u64) -> bool {
    tick > 0 && (following - 1) / ticks_per_span > (tick - 1) / ticks_per_span
}

fn throwaway_child() -> io::Result<i32> {
    let mut child = std::process::Command::new("/usr/bin/true").spawn()?;
    child.wait()?;
    Ok(child.id().cast_signed())
}

fn starts_in(events: &[Event]) -> u64 {
    events
        .iter()
        .filter(|event| matches!(event, Event::Start { .. }))
        .count() as u64
}

fn processes_started(
    first_mark: i32,
    last_mark: i32,
    live_at_first_scan: &[i32],
    forks_counted: u64,
) -> Option<u64> {
    if last_mark <= first_mark || forks_counted >= PID_LAP {
        return None;
    }

    let between = u64::from((last_mark - first_mark - 1).cast_unsigned());
    let live_between = live_at_first_scan
        .iter()
        .filter(|&&pid| first_mark < pid && pid < last_mark)
        .count() as u64;
    Some(between - live_between)
}

fn report_missed(started: Option<u64>, found: u64) {
    match started {
        Some(started) => println!(
            "  processes started over the run {started}, found {found}, missed {}",
            started.saturating_sub(found)
        ),
        None => println!("  pids wrapped over the run, so the processes missed are not counted"),
    }
}

#[derive(Clone, Copy)]
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
    let cost = later.snapshot.cost_since(&earlier.snapshot);
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
        "  last tick read {} processes ({} refused, {} gone), {} coalitions, {} CPUs, {} interfaces; over the window {} forks, {} rescans ({} leading, {} trailing, {} listing), {} starts ({} watches refused), {} execs, exits seen {}, found already exited {}",
        tally.processes_read,
        tally.processes_refused,
        tally.processes_gone,
        tally.coalitions,
        tally.cpus,
        tally.interfaces,
        counts.forks,
        counts.leading_rescans + counts.trailing_rescans + counts.listing_rescans,
        counts.leading_rescans,
        counts.trailing_rescans,
        counts.listing_rescans,
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

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(words: &[&str]) -> Vec<String> {
        words.iter().map(|&word| word.to_owned()).collect()
    }

    #[test]
    fn runs_until_stopped_without_arguments() {
        assert_eq!(None, options_from(&arguments(&[])).unwrap().seconds);
    }

    #[test]
    fn runs_for_the_seconds_given() {
        assert_eq!(
            NonZeroU64::new(5),
            options_from(&arguments(&["--seconds", "5"]))
                .unwrap()
                .seconds
        );
    }

    #[test]
    fn rejects_a_run_of_zero_seconds() {
        assert!(options_from(&arguments(&["--seconds", "0"])).is_err());
    }

    #[test]
    fn rescans_at_the_default_interval_without_arguments() {
        assert_eq!(
            DEFAULT_RESCAN_INTERVAL,
            options_from(&arguments(&[])).unwrap().rescan_interval
        );
    }

    #[test]
    fn rescans_at_the_interval_given_in_milliseconds() {
        assert_eq!(
            Duration::from_millis(20),
            options_from(&arguments(&["--rescan-ms", "20"]))
                .unwrap()
                .rescan_interval
        );
    }

    #[test]
    fn takes_both_options_in_either_order() {
        assert_eq!(
            Ok(Options {
                seconds: NonZeroU64::new(5),
                rescan_interval: Duration::from_millis(20),
            }),
            options_from(&arguments(&["--rescan-ms", "20", "--seconds", "5"]))
        );
    }

    #[test]
    fn rejects_an_unknown_option() {
        assert!(options_from(&arguments(&["--minutes", "5"])).is_err());
    }

    #[test]
    fn counts_the_pids_between_the_marks_less_those_live_at_the_first_scan() {
        assert_eq!(
            Some(7),
            processes_started(100, 110, &[50, 103, 105, 110, 200], 1_000)
        );
    }

    #[test]
    fn gives_no_count_once_the_pids_wrapped() {
        assert_eq!(None, processes_started(99_000, 300, &[], 1_000));
    }

    #[test]
    fn gives_no_count_once_the_forks_counted_could_fill_a_lap_of_pids() {
        assert_eq!(None, processes_started(100, 110, &[], PID_LAP));
    }

    #[test]
    fn closes_a_window_after_its_tenth_tick() {
        assert!(closes_span(10, 11, TICKS_PER_WINDOW));
    }

    #[test]
    fn keeps_a_window_open_before_its_tenth_tick() {
        assert!(!closes_span(9, 10, TICKS_PER_WINDOW));
    }

    #[test]
    fn closes_a_window_whose_tenth_tick_slipped() {
        assert!(closes_span(9, 11, TICKS_PER_WINDOW));
    }

    #[test]
    fn closes_a_minute_after_its_sixtieth_tick() {
        assert!(closes_span(60, 61, TICKS_PER_MINUTE));
    }

    #[test]
    fn opens_no_window_on_the_warm_up_tick() {
        assert!(!closes_span(0, 1, TICKS_PER_WINDOW));
    }
}
