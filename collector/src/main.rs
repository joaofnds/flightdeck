use std::ffi::c_int;
use std::io::{self, Write};
use std::num::NonZeroU64;
use std::process::ExitCode;
use std::time::Duration;

use collector::clock::{self, Timebase};
use collector::collector::Collector;
use collector::cost::{Cost, Meter, Snapshot, Source};
use collector::processes::{Unreadable, Usage};
use collector::schedule::Schedule;
use collector::tracker::{Counts, Event, Noticed};

const QOS_CLASS_DEFAULT: u32 = 0x15;
const TICK_PERIOD_NS: u64 = 1_000_000_000;
const TICKS_PER_WINDOW: u64 = 10;
const TICKS_PER_MINUTE: u64 = 60;
const PID_LAP: u64 = 99_999;
const BUDGET_SHARE_OF_A_CORE: f64 = 0.05;
const DEFAULT_RESCAN_INTERVAL: Duration = Duration::ZERO;

unsafe extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: c_int) -> c_int;
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
    set_qos_class_default()?;

    let timebase = Timebase::read();
    let first_marker_pid = throwaway_child();
    let mut out = io::stdout().lock();
    let mut run = Run::start(timebase, TICK_PERIOD_NS, options.rescan_interval, &mut out)?;
    let mut tick = run.first_tick;
    loop {
        let following = run.tick(tick, &mut out)?;
        if options
            .seconds
            .is_some_and(|seconds| following > seconds.get())
        {
            let last_marker_pid = throwaway_child();
            let markers = Markers {
                first: first_marker_pid,
                last: last_marker_pid,
            };
            return run.finish(tick, &markers, &mut out);
        }

        tick = following;
    }
}

fn set_qos_class_default() -> io::Result<()> {
    let error = unsafe { pthread_set_qos_class_self_np(QOS_CLASS_DEFAULT, 0) };
    if error != 0 {
        return Err(io::Error::from_raw_os_error(error));
    }
    Ok(())
}

/// The collector and what a run keeps from one tick to the next.
struct Run {
    timebase: Timebase,
    schedule: Schedule,
    meter: Meter,
    collector: Collector,
    live_at_first_scan: Vec<i32>,
    warm_up_starts: u64,
    first_tick: u64,
    whole: Span,
    minute: Span,
    window: Span,
}

/// The pids of the children spawned before the first scan and after the last tick, which
/// bound the pids of the processes that started over the run.
struct Markers {
    first: io::Result<i32>,
    last: io::Result<i32>,
}

impl Run {
    /// Starts the collector, reports what starting cost, and runs the warm-up tick, which
    /// no report counts.
    fn start(
        timebase: Timebase,
        tick_period_ns: u64,
        rescan_interval: Duration,
        out: &mut impl Write,
    ) -> io::Result<Self> {
        let mut meter = Meter::default();
        let mut collector = Collector::new(timebase, rescan_interval)?;
        let live_at_first_scan = collector.tracker.last_listing().to_vec();
        let schedule = Schedule::new(clock::now(), timebase.ticks(tick_period_ns));

        report_start(out, &meter, timebase)?;

        collector.wait_until(schedule.deadline(0), &mut meter)?;
        collector.read(&mut meter)?;
        let warm_up_starts = starts_in(&collector.events);

        let first_tick = schedule.first_ahead_of(clock::now()).max(1);
        let start = Mark::take(&meter, timebase, &collector)?;
        Ok(Self {
            timebase,
            schedule,
            meter,
            collector,
            live_at_first_scan,
            warm_up_starts,
            first_tick,
            whole: Span::begin(start, first_tick),
            minute: Span::begin(start, first_tick),
            window: Span::begin(start, first_tick),
        })
    }

    /// Runs `tick`, writes the reports of the spans it closes, and returns the tick that
    /// follows it, past any tick the run slipped.
    fn tick(&mut self, tick: u64, out: &mut impl Write) -> io::Result<u64> {
        let deadline = self.schedule.deadline(tick);
        self.collector.wait_until(deadline, &mut self.meter)?;
        let late_ticks = clock::now().saturating_sub(deadline);
        let late = Duration::from_nanos(self.timebase.nanos(late_ticks));
        self.collector.read(&mut self.meter)?;

        let following = self.schedule.first_ahead_of(clock::now()).max(tick + 1);
        let slips = following - tick - 1;
        self.whole.tally.add(late, slips, &self.collector);
        self.minute.tally.add(late, slips, &self.collector);
        self.window.tally.add(late, slips, &self.collector);

        if closes_span(tick, following, TICKS_PER_WINDOW) {
            let now = Mark::take(&self.meter, self.timebase, &self.collector)?;
            self.window.report(out, "ticks", tick, &now)?;
            self.window = Span::begin(now, following);
        }
        if closes_span(tick, following, TICKS_PER_MINUTE) {
            let now = Mark::take(&self.meter, self.timebase, &self.collector)?;
            self.minute.report(out, "minute, ticks", tick, &now)?;
            self.minute = Span::begin(now, following);
        }

        Ok(following)
    }

    /// Writes the whole run's report, which ends at `last_tick`, and the processes it missed.
    fn finish(&self, last_tick: u64, markers: &Markers, out: &mut impl Write) -> io::Result<()> {
        let end = Mark::take(&self.meter, self.timebase, &self.collector)?;
        self.whole
            .report(out, "whole run, ticks", last_tick, &end)?;
        match (&markers.first, &markers.last) {
            (Ok(first), Ok(last)) => report_missed(
                out,
                processes_started(*first, *last, &self.live_at_first_scan, end.counts.forks),
                self.warm_up_starts + self.whole.tally.starts,
            ),
            (Err(error), _) | (_, Err(error)) => writeln!(
                out,
                "  processes missed are not counted, a marker child failed: {error}"
            ),
        }
    }
}

fn report_start(out: &mut impl Write, meter: &Meter, timebase: Timebase) -> io::Result<()> {
    let snapshot = Snapshot::take(meter, timebase)?;
    writeln!(
        out,
        "started in {:.1} ms of CPU",
        milliseconds(Duration::from_nanos(snapshot.cpu_ns))
    )
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

    /// Writes what the span's ticks, `first_tick` to `last_tick`, cost and counted, with
    /// `end` taken after its last tick.
    fn report(
        &self,
        out: &mut impl Write,
        label: &str,
        last_tick: u64,
        end: &Mark,
    ) -> io::Result<()> {
        let cost = end.snapshot.cost_since(&self.start.snapshot);
        let counts = end.counts.since(self.start.counts);

        write!(out, "{label} {}-{last_tick}: ", self.first_tick)?;
        report_cost(out, &cost, &self.tally)?;
        report_sources(out, &cost)?;
        report_counts(out, &self.tally, &counts)
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
    let mut starts = 0;
    for event in events {
        if matches!(event, Event::Start { .. }) {
            starts += 1;
        }
    }
    starts
}

fn processes_started(
    first_marker_pid: i32,
    last_marker_pid: i32,
    live_at_first_scan: &[i32],
    forks_counted: u64,
) -> Option<u64> {
    if last_marker_pid <= first_marker_pid || forks_counted >= PID_LAP {
        return None;
    }

    let between = u64::from((last_marker_pid - first_marker_pid - 1).cast_unsigned());
    let mut live_between = 0;
    for pid in live_at_first_scan {
        if (first_marker_pid + 1..last_marker_pid).contains(pid) {
            live_between += 1;
        }
    }
    Some(between - live_between)
}

fn missed(started: Option<u64>, found: u64) -> Option<u64> {
    started.and_then(|started| started.checked_sub(found))
}

fn report_missed(out: &mut impl Write, started: Option<u64>, found: u64) -> io::Result<()> {
    match (started, missed(started, found)) {
        (Some(started), Some(missed)) => writeln!(
            out,
            "  processes started over the run {started}, found {found}, missed {missed}"
        ),
        _ => writeln!(
            out,
            "  pids may have wrapped over the run, so the processes missed are not counted"
        ),
    }
}

#[derive(Clone, Copy)]
struct Mark {
    snapshot: Snapshot,
    counts: Counts,
}

impl Mark {
    fn take(meter: &Meter, timebase: Timebase, collector: &Collector) -> io::Result<Self> {
        Ok(Self {
            snapshot: Snapshot::take(meter, timebase)?,
            counts: collector.tracker.counts,
        })
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

fn report_cost(out: &mut impl Write, cost: &Cost, tally: &Tally) -> io::Result<()> {
    let share = cost.share_of_a_core();
    writeln!(
        out,
        "CPU {:.2}% of a core, {:.0}% of the budget, {:.2} mW, \
         {:.0}% of instructions on Super cores; \
         ticks late {:.1} ms on average, {:.1} ms at most, {} slips",
        share * 100.0,
        share / BUDGET_SHARE_OF_A_CORE * 100.0,
        cost.milliwatts(),
        cost.share_on_super_cores() * 100.0,
        milliseconds(tally.late_total / u32::try_from(tally.ticks.max(1)).unwrap_or(u32::MAX)),
        milliseconds(tally.late_max),
        tally.slips,
    )
}

fn report_sources(out: &mut impl Write, cost: &Cost) -> io::Result<()> {
    write!(out, "  ms of CPU a second: ")?;
    for source in Source::ALL {
        write!(out, "{} {:.2}, ", source.name(), cost.ms_per_second(source))?;
    }
    writeln!(out, "unattributed {:.2}", cost.unattributed_ms_per_second())
}

fn report_counts(out: &mut impl Write, tally: &Tally, counts: &Counts) -> io::Result<()> {
    writeln!(
        out,
        "  last tick read {} processes ({} refused, {} gone), {} coalitions, {} CPUs, \
         {} interfaces; over the window {} forks, \
         {} rescans ({} leading, {} trailing, {} listing), \
         {} starts ({} watches refused), {} execs, exits seen {}, found already exited {}",
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
    )
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
    fn takes_no_rescan_interval_without_arguments() {
        assert_eq!(
            Duration::ZERO,
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
    fn counts_as_missed_the_processes_started_but_never_found() {
        assert_eq!(Some(3), missed(Some(10), 7));
    }

    #[test]
    fn gives_no_miss_count_when_more_were_found_than_the_pids_say_started() {
        assert_eq!(None, missed(Some(10), 11));
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
