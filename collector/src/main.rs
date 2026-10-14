use std::ffi::c_int;
use std::io::{self, Write};
use std::num::NonZeroU64;
use std::process::ExitCode;
use std::time::Duration;

use collector::clock::{self, Timebase};
use collector::collector::Collector;
use collector::cost::{Cost, Meter, Snapshot, Source};
use collector::processes::{self, Unreadable, Usage};
use collector::schedule::Schedule;
use collector::tracker::{Counts, Event, Noticed};

const QOS_CLASS_DEFAULT: u32 = 0x15;
const TICK_PERIOD_NS: u64 = 1_000_000_000;
const TICKS_PER_WINDOW: u64 = 10;
const TICKS_PER_MINUTE: u64 = 60;
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

    let timebase = Timebase::read()?;
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
        let schedule = Schedule::starting_now(timebase.ticks(tick_period_ns)?)?;

        report_start(out, &meter, timebase)?;

        collector.wait_until(schedule.deadline(0), &mut meter)?;
        collector.read(&mut meter)?;
        let warm_up_starts = starts_in(collector.events());

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
        let late = Duration::from_nanos(self.timebase.nanoseconds(late_ticks)?);
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
                &Starts {
                    started: processes_started(
                        *first,
                        *last,
                        &self.live_at_first_scan,
                        end.counts.forks,
                    ),
                    found: self.warm_up_starts + self.whole.tally.starts,
                    dropped: end.counts.dropped_starts,
                },
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
        milliseconds(Duration::from_nanos(snapshot.cpu_ns()))
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
        let cost = end.snapshot.cost_since(&self.start.snapshot)?;
        let counts = end.counts.since(&self.start.counts);

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
    if last_marker_pid <= first_marker_pid || forks_counted >= pid_lap() {
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

/// The pids macOS gives out before it wraps to low pids again.
fn pid_lap() -> u64 {
    u64::from(processes::PID_MAX.cast_unsigned())
}

fn missed(started: Option<u64>, found: u64) -> Option<u64> {
    started.and_then(|started| started.checked_sub(found))
}

/// The processes that started over a run, as the run's report counts them.
struct Starts {
    /// The processes started over the run, or `None` where pids may have wrapped.
    started: Option<u64>,
    /// The starts an event reported.
    found: u64,
    /// The starts dropped from a full event list, which no event reported.
    dropped: u64,
}

/// Writes how many processes started over the run and how many of them no event reported,
/// which holds the starts dropped from a full event list.
fn report_missed(out: &mut impl Write, starts: &Starts) -> io::Result<()> {
    let &Starts {
        started,
        found,
        dropped,
    } = starts;

    match (started, missed(started, found)) {
        (Some(started), Some(missed)) => writeln!(
            out,
            "  processes started over the run {started}, found {found}, missed {missed}, \
             {dropped} of them dropped from a full event list"
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
            counts: collector.tracker.counts(),
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

        for event in collector.events() {
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
        self.interfaces = collector.interfaces.read().len();
        self.cpus = collector.machine.cpus().len();
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
    write!(
        out,
        "CPU {:.2}% of a core, {:.0}% of the budget, {:.2} mW, ",
        share * 100.0,
        share / BUDGET_SHARE_OF_A_CORE * 100.0,
        cost.milliwatts(),
    )?;
    match cost.share_on_super_cores() {
        Some(super_share) => write!(
            out,
            "{:.0}% of instructions on Super cores",
            super_share * 100.0
        )?,
        None => write!(out, "no instructions run")?,
    }
    let late_average = report_cost_late_average(tally.late_total, tally.ticks)?;
    writeln!(
        out,
        "; ticks late {:.1} ms on average, {:.1} ms at most, {} slips",
        milliseconds(late_average),
        milliseconds(tally.late_max),
        tally.slips,
    )
}

/// The lateness of the average tick in a span of `ticks`, floored to the nanosecond, or an
/// error where the ticks are more than a u32 holds, which `Duration` divides by.
fn report_cost_late_average(late_total: Duration, ticks: u64) -> io::Result<Duration> {
    // A span reports only after a tick has added to its tally.
    assert!(ticks > 0);

    let Ok(divisor) = u32::try_from(ticks) else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "a span of {ticks} ticks, past the {} a lateness averages over",
                u32::MAX
            ),
        ));
    };
    let late_average = late_total / divisor;
    assert!(late_average <= late_total);
    Ok(late_average)
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
         {} starts ({} watches refused), {} execs, exits seen {}, found already exited {}; \
         {} starts, {} execs and {} exits dropped from a full event list",
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
        counts.dropped_starts,
        counts.dropped_execs,
        counts.dropped_exits,
    )
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

#[cfg(test)]
mod tests {
    use std::alloc::{GlobalAlloc, Layout, System};
    use std::cell::Cell;
    use std::sync::atomic::{AtomicBool, Ordering};

    use super::*;

    const TEST_TICK_PERIOD_NS: u64 = 20_000_000;

    /// The system's allocator, counting the calls made to it on a thread while that thread
    /// asks it to, so a test can see whether its own thread allocates.
    struct Counting;

    #[global_allocator]
    static COUNTING: Counting = Counting;

    thread_local! {
        static COUNTING_THIS_THREAD: Cell<bool> = const { Cell::new(false) };
        static CALLS_THIS_THREAD: Cell<u64> = const { Cell::new(0) };
    }

    fn count_call() {
        if COUNTING_THIS_THREAD.get() {
            CALLS_THIS_THREAD.set(CALLS_THIS_THREAD.get() + 1);
        }
    }

    unsafe impl GlobalAlloc for Counting {
        unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
            count_call();
            unsafe { System.alloc(layout) }
        }

        unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
            count_call();
            unsafe { System.alloc_zeroed(layout) }
        }

        unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
            count_call();
            unsafe { System.dealloc(pointer, layout) }
        }

        unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
            count_call();
            unsafe { System.realloc(pointer, layout, size) }
        }
    }

    /// Returns how many times `work` called the allocator on this thread.
    fn allocator_calls_in(work: impl FnOnce()) -> u64 {
        CALLS_THIS_THREAD.set(0);
        COUNTING_THIS_THREAD.set(true);
        work();
        COUNTING_THIS_THREAD.set(false);
        CALLS_THIS_THREAD.get()
    }

    /// Runs `work` while another thread starts a process every 2 ms, so the collector's
    /// ticks carry starts and exits.
    fn while_forking<T>(work: impl FnOnce() -> T) -> T {
        let stop = AtomicBool::new(false);
        std::thread::scope(|scope| {
            scope.spawn(|| {
                while !stop.load(Ordering::Relaxed) {
                    std::process::Command::new("/usr/bin/true")
                        .status()
                        .unwrap();
                    std::thread::sleep(Duration::from_millis(2));
                }
            });
            let result = work();
            stop.store(true, Ordering::Relaxed);
            result
        })
    }

    #[test]
    fn allocates_nothing_in_a_warm_tick_or_the_reports_it_closes() {
        // Goal: a tick after the warm-up allocates nothing, with the window and minute
        // reports it writes, as Tiger Style's allocation at startup only asks.
        // Method: run ticks 20 ms apart until the minute closes, so they write six window
        // reports and a minute report, while another thread starts processes, and count the
        // allocator calls made on this thread.
        let timebase = Timebase::read().unwrap();
        let mut sink = io::sink();
        let mut run = Run::start(
            timebase,
            TEST_TICK_PERIOD_NS,
            DEFAULT_RESCAN_INTERVAL,
            &mut sink,
        )
        .unwrap();

        let calls = while_forking(|| {
            allocator_calls_in(|| {
                let mut tick = run.first_tick;
                while tick <= TICKS_PER_MINUTE {
                    tick = run.tick(tick, &mut sink).unwrap();
                }
            })
        });

        assert!(run.whole.tally.starts > 0);
        assert_eq!(0, calls);
    }

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
        assert_eq!(None, processes_started(100, 110, &[], pid_lap()));
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
    fn reports_the_events_dropped_from_a_full_event_list_by_kind() {
        // Goal: a window's report names the starts, execs and exits a full event list dropped.
        // Method: write the counts line for a window with a drop of each kind.
        let counts = Counts {
            dropped_starts: 2,
            dropped_execs: 1,
            dropped_exits: 3,
            ..Counts::default()
        };
        let mut written = Vec::new();

        report_counts(&mut written, &Tally::default(), &counts).unwrap();

        let line = String::from_utf8(written).unwrap();
        let dropped = "; 2 starts, 1 execs and 3 exits dropped from a full event list\n";
        assert!(line.ends_with(dropped), "{line}");
    }

    #[test]
    fn reports_the_dropped_starts_among_the_processes_missed() {
        // Goal: the missed line says how many of the processes missed were dropped from a full
        // event list.
        // Method: write the missed line for a run that found 7 of 10 starts and dropped 2.
        let mut written = Vec::new();

        let starts = Starts {
            started: Some(10),
            found: 7,
            dropped: 2,
        };
        report_missed(&mut written, &starts).unwrap();

        assert_eq!(
            "  processes started over the run 10, found 7, missed 3, \
             2 of them dropped from a full event list\n",
            String::from_utf8(written).unwrap()
        );
    }

    #[test]
    fn averages_the_lateness_over_the_ticks() {
        // Goal: a span's average lateness is its total lateness over its ticks.
        // Method: average 30 ms over 3 ticks.
        assert_eq!(
            Duration::from_millis(10),
            report_cost_late_average(Duration::from_millis(30), 3).unwrap()
        );
    }

    #[test]
    fn refuses_an_average_over_more_ticks_than_a_u32_holds() {
        // Goal: a span of more ticks than a u32 holds is an error, not an average over
        // u32::MAX ticks.
        // Method: average over one tick past u32::MAX.
        let ticks = u64::from(u32::MAX) + 1;

        let average = report_cost_late_average(Duration::from_secs(1), ticks);

        assert_eq!(io::ErrorKind::InvalidData, average.unwrap_err().kind());
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
