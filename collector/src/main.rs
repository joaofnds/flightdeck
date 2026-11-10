mod owner;
mod stop;

#[cfg(test)]
mod identity_report;
#[cfg(test)]
use collector as identity_report_source;

#[cfg(test)]
mod owner_run_tests;

use std::ffi::c_int;
use std::io::{self, Write};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::time::Duration;

use collector::clock::{self, Timebase};
use collector::collector::{Capacities, Collector};
use collector::cost::{Cost, Meter, Snapshot, Source};
use collector::diagnostics::{self, Group, Role};
use collector::processes::{self, Unreadable, Usage};
use collector::recording::{self, Registration};
use collector::ring::{DictionaryCapacity, ObservationTimes, Place, Shape, Writer};
use collector::schedule::Schedule;
use collector::tracker::{Counts, EVENTS_PER_TICK_MAX, Event, Noticed};
use collector::u64_from_usize;

use collector::publication::Publication;

const QOS_CLASS_DEFAULT: u32 = 0x15;
const TICK_PERIOD_NS: u64 = 1_000_000_000;
const TICKS_PER_WINDOW: u64 = 10;
const TICKS_PER_MINUTE: u64 = 60;
const BUDGET_SHARE_OF_A_CORE: f64 = 0.05;
const DEFAULT_RESCAN_INTERVAL: Duration = Duration::ZERO;
/// The ring readers find unless the collector is told another.
const DEFAULT_RING: &str = "flightdeck";
/// The ticks the ring holds, two minutes of them, so a reader that stops for less loses none.
const RING_SLOTS: u64 = 120;
/// The most events 121 ticks in a row held across the user's recording, measured on 2026-10-30
/// over 34,462,946 events in 152 runs.
const EVENTS_IN_121_TICKS_MAX: usize = 584_210;
/// The process events the ring's log holds. The log holds the events of the ticks in the ring
/// while the writer logs the next tick's. A burst past the log's room drops its events at
/// admission and counts them by kind.
const RING_EVENTS: usize = 655_360;
// The log holds the events of the 120 ticks in the ring and the tick the writer logs, as many
// as any 121 ticks in a row held.
const _: () = assert!(RING_EVENTS >= EVENTS_IN_121_TICKS_MAX);
// A new ring has room for the most events one tick admits.
const _: () = assert!(RING_EVENTS >= EVENTS_PER_TICK_MAX);
// A persister of 0.2.6 refuses a ring whose log holds more than 1,966,080 events.
const _: () = assert!(RING_EVENTS <= 1_966_080);

unsafe extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: c_int) -> c_int;
}

fn main() {
    // Public startup becomes the recording controller before any diagnostics thread exists.
    // Only a worker bearing the current cohort authority enters acquisition directly.
    let parsed = collector::arguments("collector")
        .and_then(|arguments| options_from(&arguments).map(|options| (arguments, options)));
    let (arguments, options) = match parsed {
        Ok(parsed) => parsed,
        Err(usage) => {
            diagnostics::install(Role::Collector);
            diagnostics::error(format_args!("{usage}"));
            diagnostics::exit(2);
        }
    };
    if options.recorder_authority.is_none() {
        use std::os::unix::process::CommandExt;
        let error = match std::env::current_exe().and_then(|path| path.canonicalize()) {
            Ok(path) => std::process::Command::new(path.with_file_name("recorder"))
                .args(arguments)
                .exec(),
            Err(error) => error,
        };
        diagnostics::install(Role::Collector);
        diagnostics::error(format_args!("cannot start recorder: {error}"));
        diagnostics::exit(1);
    }

    diagnostics::install(Role::Collector);
    let status = match run(&options) {
        Ok(()) => 0,
        Err(error) => {
            diagnostics::error(format_args!("collector: {error}"));
            1
        }
    };
    diagnostics::exit(status);
}

#[derive(Debug, PartialEq)]
struct Options {
    seconds: Option<NonZeroU64>,
    rescan_interval: Duration,
    ring_name: String,
    recording: Option<PathBuf>,
    parent_pid: Option<i32>,
    recorder_authority: Option<collector::ring::Identity>,
}

fn options_from(arguments: &[String]) -> Result<Options, String> {
    let usage = || {
        "usage: collector [--seconds N] [--rescan-ms N] [--ring NAME] \
        [--recording PATH] [--parent-pid PID]"
            .to_owned()
    };
    let mut options = Options {
        seconds: None,
        rescan_interval: DEFAULT_RESCAN_INTERVAL,
        ring_name: DEFAULT_RING.to_owned(),
        recording: None,
        parent_pid: None,
        recorder_authority: None,
    };

    for pair in arguments.chunks(2) {
        match pair {
            [flag, seconds] if flag == "--seconds" => {
                options.seconds = Some(seconds.parse().map_err(|_| usage())?);
            }
            [flag, milliseconds] if flag == "--rescan-ms" => {
                options.rescan_interval =
                    Duration::from_millis(milliseconds.parse().map_err(|_| usage())?);
            }
            [flag, ring] if flag == "--ring" => options.ring_name.clone_from(ring),
            [flag, path] if flag == "--recording" => options.recording = Some(PathBuf::from(path)),
            [flag, authority] if flag == "--recorder-authority" => {
                options.recorder_authority =
                    Some(recording::parse_identity(authority).map_err(|_| usage())?);
            }
            [flag, parent] if flag == "--parent-pid" => {
                options.parent_pid = Some(owner::parse_parent_pid(parent).map_err(|()| usage())?);
            }
            _ => return Err(usage()),
        }
    }

    // Each flag took the word after it, so the words came in pairs.
    assert!(arguments.len().is_multiple_of(2));
    Ok(options)
}

fn run(options: &Options) -> io::Result<()> {
    let owner = owner::Owner::bind(options.parent_pid)?;
    run_set_qos_class_default()?;
    let _stop = stop::Stop::install()?;

    let timebase = Timebase::read()?;
    let first_marker_pid = run_throwaway_child();
    let mut out = diagnostics::stdout_group();
    let place = Place::for_user(&options.ring_name)?;
    let recording = options
        .recording
        .clone()
        .unwrap_or_else(|| place.lock_path.with_extension("recording"));
    let control = run_control(&recording, options.recorder_authority)?;
    let started = Run::start(
        timebase,
        TICK_PERIOD_NS,
        options.rescan_interval,
        &place,
        &recording,
        &mut *out,
    );
    if started.is_ok() {
        out.submit();
    } else {
        out.begin();
    }
    let mut run = started?;
    let mut health = control
        .map(|control| {
            recording::control::Reporter::new(control, recording::control::Role::Collector)
        })
        .transpose()?;
    let result = run_collect(
        options,
        owner,
        &mut run,
        first_marker_pid,
        &mut out,
        &mut health,
    );
    if let Some(health) = &mut health {
        let state = if result.is_ok() {
            recording::control::State::Stopped
        } else {
            recording::control::State::Failed
        };
        health.update(
            state,
            run.ring
                .identity()
                .map_or(collector::ring::Identity([0; 2]), |identity| identity.run),
            run.ring.publication_newest(),
        )?;
    }
    if result.is_ok() {
        out.submit();
    } else {
        out.begin();
    }

    result
}

fn run_control(
    recording: &Path,
    authority: Option<collector::ring::Identity>,
) -> io::Result<Option<recording::control::Control>> {
    if let Some(authority) = authority {
        let control = recording::control::Control::open_writable(recording)?;
        if control.authority() != authority {
            return Err(io::Error::from(io::ErrorKind::PermissionDenied));
        }

        Ok(Some(control))
    } else {
        Ok(None)
    }
}

/// The newest publication of the run `identity` names that the persister reports saved, or
/// none where it reports another run or nothing this build can read, so that the ring's log
/// keeps its events as it does with no persister.
fn run_collect_saved(
    control: &recording::control::Control,
    identity: Option<collector::ring::RunIdentity>,
) -> Option<u64> {
    let identity = identity?;
    // A record caught mid-write reads whole on a later tick, and one another build wrote is no
    // report this build can trust, so either reports nothing.
    let Ok(health) = control.read(recording::control::Role::Persister) else {
        return None;
    };

    (health.run == identity.run).then_some(health.publication)
}

fn run_collect(
    options: &Options,
    owner: owner::Owner,
    run: &mut Run,
    first_marker_pid: io::Result<i32>,
    out: &mut Group,
    health: &mut Option<recording::control::Reporter>,
) -> io::Result<()> {
    let mut tick = run.first_tick;
    // Tick 0 is the warm-up tick, which `Run::start` ran.
    assert!(tick > 0);
    for _step in 0..u64::MAX {
        let result = run.tick(tick, out);
        if result.is_ok() {
            out.submit();
        } else {
            out.begin();
        }
        let requested = if let Some(health) = health {
            health.update(
                recording::control::State::Running,
                run.ring
                    .identity()
                    .map_or(collector::ring::Identity([0; 2]), |identity| identity.run),
                run.ring.publication_newest(),
            )?;
            if let Some(publication) = run_collect_saved(health.control(), run.ring.identity()) {
                run.ring.publications_saved(publication);
            }
            health.control().stopping()
        } else {
            false
        };
        let owner_departed = owner.departed() || requested;
        let step = match result {
            Ok(step) => step,
            Err(error) => {
                if error.kind() == io::ErrorKind::Interrupted {
                    if stop::Stop::requested() {
                        return run.finish_recording(out);
                    }
                    if owner_departed {
                        return run.finish_recording(out);
                    }
                }
                return Err(error);
            }
        };
        assert_eq!(step.requested, tick);
        assert!(step.following > step.observed);
        let elapsed = options
            .seconds
            .is_some_and(|seconds| step.following > seconds.get());
        if elapsed {
            return run_finish(run, step.observed, first_marker_pid, out);
        }
        if stop::Stop::requested() {
            return run_finish(run, step.observed, first_marker_pid, out);
        }
        if owner_departed {
            return run_finish(run, step.observed, first_marker_pid, out);
        }

        tick = step.following;
    }
    unreachable!("collector exhausted its process-lifetime tick bound");
}

fn run_finish(
    run: &mut Run,
    tick: u64,
    first_marker_pid: io::Result<i32>,
    out: &mut impl Write,
) -> io::Result<()> {
    let markers = first_marker_pid.and_then(|first| {
        Ok(Markers {
            first,
            last: run_throwaway_child()?,
        })
    });
    run.finish(tick, &markers, out)
}

/// The shape of a ring with room for the readings of every tick the collector of `capacities`
/// reads, and a log of `RING_EVENTS` events.
fn start_ring_shape(capacities: Capacities) -> Shape {
    Shape {
        slot_count: RING_SLOTS,
        cpu_capacity: u64_from_usize(capacities.cpus),
        interface_capacity: u64_from_usize(capacities.interfaces),
        core_capacity: u64_from_usize(capacities.cores),
        state_capacity: u64_from_usize(capacities.states),
        fan_capacity: u64_from_usize(capacities.fans),
        process_capacity: u64_from_usize(capacities.processes),
        coalition_capacity: u64_from_usize(capacities.coalitions),
        event_capacity: u64_from_usize(RING_EVENTS),
    }
}

fn observation_or_unavailable(
    measured: io::Result<ObservationTimes>,
    out: &mut impl Write,
) -> io::Result<Option<ObservationTimes>> {
    match measured {
        Ok(times) => Ok(Some(times)),
        Err(error) => {
            writeln!(
                out,
                "observation clocks unavailable kind={:?} raw_status={:?}",
                error.kind(),
                error.raw_os_error()
            )?;
            Ok(None)
        }
    }
}

fn run_set_qos_class_default() -> io::Result<()> {
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
    ring: Writer,
    publication: Publication,
    live_at_first_scan: Vec<i32>,
    warm_up_starts: u64,
    first_tick: u64,
    whole: Span,
    minute: Span,
    window: Span,
}

/// Requires private registration before managed acquisition publishes its first reading.
fn start_recording_ring(
    place: &Place,
    shape: Shape,
    root: &Path,
    timebase: Timebase,
    out: &mut impl Write,
) -> io::Result<Writer> {
    let start_ns = timebase.nanoseconds(clock::now())?;
    let capacity = DictionaryCapacity {
        entries: 65_536,
        arena_bytes: 64 * 1024 * 1024,
    };
    let size_bytes = Writer::system_size_bytes(shape, capacity)?;

    writeln!(
        out,
        "identity, battery, disks and system ring bytes={size_bytes} \
        reader_limit_bytes=1073741824"
    )?;

    let started = Registration::start_with_system(root, place, shape, capacity, start_ns)?;
    if let Some(error) = started.announcement_error() {
        writeln!(
            out,
            "recording announcement failed: {error}; collection and saving continue; \
            another Flightdeck cannot attach"
        )?;
    }
    let (ring, registered) = started.into_parts();
    registered?;
    Ok(ring)
}

/// The pids of the children spawned before the first scan and after the last tick, which
/// bound the pids of the processes that started over the run.
struct Markers {
    first: i32,
    last: i32,
}

impl Markers {
    /// The processes that started between the marker children, less those live at the first
    /// scan, or `None` where the pids may have wrapped. `live_at_first_scan` holds each pid once,
    /// in increasing order.
    fn processes_started(&self, live_at_first_scan: &[i32], forks_counted: u64) -> Option<u64> {
        for listed in live_at_first_scan.windows(2) {
            assert!(listed[0] < listed[1]);
        }

        if self.last <= self.first {
            // The pids wrapped between the marks.
            return None;
        }
        if forks_counted >= processes_started_pid_lap() {
            // The pids may have wrapped past the first mark again.
            return None;
        }

        let between = u64::from((self.last - self.first - 1).cast_unsigned());
        let mut live_between = 0;
        for pid in live_at_first_scan {
            if (self.first + 1..self.last).contains(pid) {
                live_between += 1;
            }
        }
        // Each pid counted is a distinct one between the marks, and one of the listing's.
        assert!(live_between <= between);
        assert!(live_between <= u64_from_usize(live_at_first_scan.len()));
        Some(between - live_between)
    }
}

impl Run {
    /// Starts the collector, reports what starting cost, and runs the warm-up tick, which
    /// no report counts.
    fn start(
        timebase: Timebase,
        tick_period_ns: u64,
        rescan_interval: Duration,
        place: &Place,
        recording: &Path,
        out: &mut impl Write,
    ) -> io::Result<Self> {
        assert!(tick_period_ns > 0);

        let mut meter = Meter::default();
        let mut collector = Collector::new(timebase, rescan_interval)?;
        // A second collector stops here, before its first tick, so the first runs on.
        let mut ring = start_recording_ring(
            place,
            start_ring_shape(collector.capacities()),
            recording,
            timebase,
            out,
        )?;

        let mut publication = Publication::new(&ring)?;
        let live_at_first_scan = Self::start_live_at_first_scan(collector.tracker.last_listing());
        let schedule = Schedule::starting_now(timebase.ticks(tick_period_ns)?)?;

        start_report(out, &meter, timebase)?;

        publication.wait_until(&mut collector, schedule.deadline(0), &mut meter, &mut ring)?;
        let observed = observation_or_unavailable(recording::observation_times(timebase), out)?;
        publication.read(&mut collector, &mut meter, &mut ring, observed.as_ref())?;
        meter.measure(Source::Ring, || {
            publication.publish(&mut ring, &collector, 0, observed.as_ref())
        })?;
        let warm_up_starts = start_warm_up_starts(collector.events());
        publication.report(&ring, "warm-up", out)?;

        // Tick 0 is the warm-up tick, so the spans start past it.
        let first_tick = schedule.first_ahead_of(clock::now()).max(1);
        let start = Mark::take(&meter, timebase, &collector)?;
        Ok(Self {
            timebase,
            schedule,
            meter,
            collector,
            ring,
            publication,
            live_at_first_scan,
            warm_up_starts,
            first_tick,
            whole: Span::begin(start, first_tick),
            minute: Span::begin(start, first_tick),
            window: Span::begin(start, first_tick),
        })
    }

    /// The pids of `listing` in increasing order, each once, where a listing comes in no set
    /// order and may hold a pid twice.
    fn start_live_at_first_scan(listing: &[i32]) -> Vec<i32> {
        let mut live_at_first_scan = listing.to_vec();
        live_at_first_scan.sort_unstable();
        live_at_first_scan.dedup();
        live_at_first_scan
    }

    /// Runs one observation after the requested deadline and reports the spans it closes.
    fn tick(&mut self, requested: u64, out: &mut impl Write) -> io::Result<Step> {
        self.tick_with_observation(requested, out, clock::now, recording::observation_times)
    }

    fn tick_with_observation(
        &mut self,
        requested: u64,
        out: &mut impl Write,
        mut now: impl FnMut() -> u64,
        observe: impl FnOnce(Timebase) -> io::Result<ObservationTimes>,
    ) -> io::Result<Step> {
        assert!(requested >= self.first_tick);

        let deadline = self.schedule.deadline(requested);
        self.publication.wait_until(
            &mut self.collector,
            deadline,
            &mut self.meter,
            &mut self.ring,
        )?;
        let started_at = now();
        let observed = requested.max(self.schedule.first_ahead_of(started_at).saturating_sub(1));
        let late_ticks = started_at.saturating_sub(deadline);
        let late = Duration::from_nanos(self.timebase.nanoseconds(late_ticks)?);
        let times = observation_or_unavailable(observe(self.timebase), out)?;
        self.publication.read(
            &mut self.collector,
            &mut self.meter,
            &mut self.ring,
            times.as_ref(),
        )?;
        self.meter.measure(Source::Ring, || {
            self.publication
                .publish(&mut self.ring, &self.collector, observed, times.as_ref())
        })?;

        let following = self.schedule.first_ahead_of(now()).max(observed + 1);
        let slips = following - requested - 1;
        self.whole.tally.add(late, slips, &self.collector);
        self.minute.tally.add(late, slips, &self.collector);
        self.window.tally.add(late, slips, &self.collector);
        // A span closes on its last tick, so it never runs more ticks than it spans.
        assert!(self.window.tally.ticks <= TICKS_PER_WINDOW);
        assert!(self.minute.tally.ticks <= TICKS_PER_MINUTE);
        assert_eq!(following, self.whole.following());
        assert_eq!(following, self.minute.following());
        assert_eq!(following, self.window.following());

        let step = Step {
            requested,
            observed,
            following,
        };
        if step.closes_span(TICKS_PER_WINDOW) {
            let now = Mark::take(&self.meter, self.timebase, &self.collector)?;
            self.window.report(out, "ticks", observed, &now)?;
            self.window = Span::begin(now, following);
        }
        if step.closes_span(TICKS_PER_MINUTE) {
            let now = Mark::take(&self.meter, self.timebase, &self.collector)?;
            self.minute.report(out, "minute, ticks", observed, &now)?;
            self.publication.report(&self.ring, "minute", out)?;
            self.minute = Span::begin(now, following);
        }

        Ok(step)
    }

    /// Writes the whole run's report, which ends at `last_tick`, and the processes it missed.
    fn finish(
        &mut self,
        last_tick: u64,
        markers: &io::Result<Markers>,
        out: &mut impl Write,
    ) -> io::Result<()> {
        assert!(last_tick >= self.first_tick);
        // The run finishes only after its first tick.
        assert!(self.whole.tally.ticks > 0);

        let end = Mark::take(&self.meter, self.timebase, &self.collector)?;
        self.whole
            .report(out, "whole run, ticks", last_tick, &end)?;
        match markers {
            Ok(markers) => finish_report_missed(
                out,
                &Starts {
                    started: markers.processes_started(&self.live_at_first_scan, end.counts.forks),
                    found: self.warm_up_starts + self.whole.tally.starts,
                    dropped: end.counts.dropped_starts,
                },
            ),
            Err(error) => writeln!(
                out,
                "  processes missed are not counted, a marker child failed: {error}"
            ),
        }?;
        self.finish_recording(out)
    }

    fn finish_recording(&mut self, out: &mut impl Write) -> io::Result<()> {
        self.publication.report(&self.ring, "tail", out)?;
        self.ring.finish();
        Ok(())
    }
}

fn start_report(out: &mut impl Write, meter: &Meter, timebase: Timebase) -> io::Result<()> {
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
        assert!(first_tick > 0);

        Self {
            start,
            first_tick,
            tally: Tally::default(),
        }
    }

    /// The tick after the span's last, since a span runs or slips each of its ticks.
    fn following(&self) -> u64 {
        self.first_tick + self.tally.ticks + self.tally.slips
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
        assert!(!label.is_empty());
        assert!(last_tick >= self.first_tick);
        // A slipped tick is in the span but never ran.
        assert!(self.tally.ticks <= last_tick - self.first_tick + 1);

        let cost = end.snapshot.cost_since(&self.start.snapshot)?;
        let counts = end.counts.since(&self.start.counts);
        let late_average = self.tally.late_average()?;

        write!(out, "{label} {}-{last_tick}: ", self.first_tick)?;
        report_cost(out, &cost, &self.tally, late_average)?;
        report_sources(out, &cost)?;
        report_counts(out, &self.tally, &counts)
    }
}

/// The requested deadline, the observation it led to, and the next deadline still ahead.
struct Step {
    requested: u64,
    observed: u64,
    following: u64,
}

impl Step {
    /// Whether the requested tick's span ends before the following tick, even if the
    /// observation is already beyond that boundary. Spans count from tick 1.
    fn closes_span(&self, ticks_per_span: u64) -> bool {
        // `Run::start` runs the warm-up tick, 0, apart from every span.
        assert!(self.requested > 0);
        assert!(self.requested <= self.observed);
        assert!(self.observed < self.following);
        assert!(ticks_per_span > 0);

        // Each division floors, to the span the tick falls in.
        (self.following - 1) / ticks_per_span > (self.requested - 1) / ticks_per_span
    }
}

fn run_throwaway_child() -> io::Result<i32> {
    let mut child = std::process::Command::new("/usr/bin/true").spawn()?;
    child.wait()?;
    Ok(child.id().cast_signed())
}

fn start_warm_up_starts(events: &[Event]) -> u64 {
    let mut starts = 0;
    for event in events {
        if matches!(event, Event::Start { .. }) {
            starts += 1;
        }
    }

    assert!(starts <= u64_from_usize(events.len()));
    starts
}

/// The pids macOS gives out before it wraps to low pids again.
fn processes_started_pid_lap() -> u64 {
    const { assert!(processes::PID_MAX > 0) };

    u64::from(processes::PID_MAX.cast_unsigned())
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

impl Starts {
    /// The processes started that no event reported, or `None` where the run gives no count
    /// of the processes started or found more than it counted.
    fn missed(&self) -> Option<u64> {
        self.started
            .and_then(|started| started.checked_sub(self.found))
    }
}

/// Writes how many processes started over the run and how many of them no event reported,
/// which holds the starts dropped from a full event list.
fn finish_report_missed(out: &mut impl Write, starts: &Starts) -> io::Result<()> {
    let &Starts {
        started,
        found,
        dropped,
    } = starts;

    match (started, starts.missed()) {
        (Some(started), Some(missed)) => {
            assert!(missed <= started);
            assert!(found <= started);
            writeln!(
                out,
                "  processes started over the run {started}, found {found}, missed {missed}, \
             {dropped} of them dropped from a full event list"
            )
        }
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
    processes_read: u64,
    processes_refused: u32,
    processes_gone: u32,
    coalitions: u64,
    interfaces: u64,
    cpus: u64,
}

impl Tally {
    fn add(&mut self, late: Duration, slips: u64, collector: &Collector) {
        self.ticks += 1;
        self.slips += slips;
        self.late_total += late;
        self.late_max = self.late_max.max(late);
        assert!(self.late_max <= self.late_total);

        let events_counted = self.events_counted();
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
        // Each event counts in exactly one of the counts.
        assert_eq!(
            events_counted + u64_from_usize(collector.events().len()),
            self.events_counted()
        );

        self.processes_read = u64_from_usize(collector.processes.readings.len());
        self.processes_refused = collector.processes.refused;
        self.processes_gone = collector.processes.gone;
        self.coalitions = u64_from_usize(collector.coalitions.readings.len());
        self.interfaces = u64_from_usize(collector.interfaces.readings().len());
        self.cpus = u64_from_usize(collector.machine.cpus().len());
    }

    /// The events the tally has counted, in all of its counts of them.
    fn events_counted(&self) -> u64 {
        let exits = self.seen_exiting.count() + self.found_exited.count();
        self.starts + self.execs + exits
    }

    /// The lateness of the average tick the tally counts, floored to the nanosecond, or an
    /// error where the ticks are more than a u32 holds, which `Duration` divides by.
    fn late_average(&self) -> io::Result<Duration> {
        // A span reports only after a tick has added to its tally.
        assert!(self.ticks > 0);

        let Ok(divisor) = u32::try_from(self.ticks) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "a span of {} ticks, past the {} a lateness averages over",
                    self.ticks,
                    u32::MAX
                ),
            ));
        };

        let late_average = self.late_total / divisor;
        assert!(late_average <= self.late_total);
        // The average tick is no later than the latest.
        assert!(late_average <= self.late_max);
        Ok(late_average)
    }
}

#[derive(Default)]
struct Exits {
    read: u64,
    refused: u64,
    gone: u64,
}

impl Exits {
    /// The exits counted, each in exactly one of the three.
    fn count(&self) -> u64 {
        self.read + self.refused + self.gone
    }

    fn add(&mut self, last: &Result<Usage, Unreadable>) {
        let total = self.count();

        match last {
            Ok(_) => self.read += 1,
            Err(Unreadable::Refused) => self.refused += 1,
            Err(Unreadable::Gone) => self.gone += 1,
        }

        // Each exit counts in exactly one of the three.
        assert_eq!(total + 1, self.count());
    }
}

impl std::fmt::Display for Exits {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} ({} read, {} refused, {} gone)",
            self.count(),
            self.read,
            self.refused,
            self.gone
        )
    }
}

fn report_cost(
    out: &mut impl Write,
    cost: &Cost,
    tally: &Tally,
    late_average: Duration,
) -> io::Result<()> {
    // A span reports after its first tick. The average tick is no later than the latest, and
    // the latest no later than all of the span's ticks together.
    assert!(tally.ticks > 0);
    assert!(late_average <= tally.late_max);
    assert!(tally.late_max <= tally.late_total);

    let share = cost.share_of_a_core();
    write!(
        out,
        "collector CPU {:.2}% of a core, {:.0}% of the combined budget, {:.2} mW, ",
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
    writeln!(
        out,
        "; ticks late {:.1} ms on average, {:.1} ms at most, {} slips",
        milliseconds(late_average),
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
    // A span reports after its first tick, whose read these counts hold.
    assert!(tally.ticks > 0);

    writeln!(
        out,
        "  last tick read {} processes ({} refused, {} gone), {} coalitions, {} CPUs, \
         {} interfaces; over these ticks {} forks, \
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
        counts.rescans(),
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
    use std::panic::AssertUnwindSafe;
    use std::sync::atomic::{AtomicBool, Ordering};

    use collector::ring::{Found, FoundEvent, Reader};

    use super::*;

    mod activation;
    mod diagnostic_groups;
    #[path = "../../tests/support/process.rs"]
    mod process;
    mod tick_timing;

    const TEST_TICK_PERIOD_NS: u64 = 20_000_000;

    unsafe extern "C" {
        fn shm_unlink(name: *const std::ffi::c_char) -> c_int;
    }

    /// A ring only one test uses, whose lock file and ring are removed when the test
    /// ends, since a shared-memory object outlives the process that made it.
    struct TestRing {
        place: Place,
    }

    struct TestRecording {
        ring: TestRing,
        root: PathBuf,
    }

    impl TestRecording {
        fn new(name: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "flightdeck-main-recording-{}-{name}",
                std::process::id()
            ));
            assert!(!root.exists());
            Self {
                ring: TestRing::new(name),
                root,
            }
        }
    }

    impl Drop for TestRecording {
        fn drop(&mut self) {
            if self.root.exists() {
                std::fs::remove_dir_all(&self.root).unwrap();
            }
        }
    }

    impl TestRing {
        fn new(test: &str) -> Self {
            let name = format!("fdm{}{test}", std::process::id());
            Self {
                place: Place::for_user(&name).unwrap(),
            }
        }
    }

    impl Drop for TestRing {
        fn drop(&mut self) {
            // The lock file names the ring, as the writer recorded it, so a change to how rings
            // are named cannot leave one behind unseen.
            let name = match std::fs::read(&self.place.lock_path) {
                Ok(name) => name,
                Err(error) => {
                    assert_eq!(io::ErrorKind::NotFound, error.kind());
                    return;
                }
            };
            if !name.is_empty() {
                let ring = std::ffi::CString::new(name).unwrap();
                assert_eq!(0, unsafe { shm_unlink(ring.as_ptr()) }, "unlink {ring:?}");
            }
            std::fs::remove_file(&self.place.lock_path).unwrap();
        }
    }

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
    pub(crate) fn allocator_calls_in(work: impl FnOnce()) -> u64 {
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
            // The scope waits for the forking thread, so it is stopped on a panic too.
            let result = std::panic::catch_unwind(AssertUnwindSafe(work));
            stop.store(true, Ordering::Relaxed);
            match result {
                Ok(result) => result,
                Err(panic) => std::panic::resume_unwind(panic),
            }
        })
    }

    #[test]
    fn allocates_nothing_in_a_warm_tick_or_the_reports_it_closes() {
        // Goal: a tick after the warm-up allocates nothing, with the window and minute
        // reports it writes, as docs/coding-style.md asks of the `collector` binary.
        // Method: run ticks 20 ms apart until the minute closes, so they write six window
        // reports while another thread starts processes.
        // Count the allocator calls made on this thread after private startup completes.
        let timebase = Timebase::read().unwrap();
        let mut sink = io::sink();
        let recording = TestRecording::new("alloc");
        let mut run = Run::start(
            timebase,
            TEST_TICK_PERIOD_NS,
            DEFAULT_RESCAN_INTERVAL,
            &recording.ring.place,
            &recording.root,
            &mut sink,
        )
        .unwrap();

        let calls = while_forking(|| {
            allocator_calls_in(|| {
                let mut tick = run.first_tick;
                while tick <= TICKS_PER_MINUTE {
                    tick = run.tick(tick, &mut sink).unwrap().following;
                }
            })
        });

        assert!(run.whole.tally.starts > 0);
        assert_eq!(0, calls);
    }

    #[test]
    fn managed_collection_and_health_allocate_nothing_after_startup() {
        // Goal: the managed production loop retains startup-only collector allocation.
        // Method: run its real orchestration and mapped health for an accelerated minute.
        if std::env::var_os("FLIGHTDECK_MANAGED_ALLOCATION").is_none() {
            let mut child = process::OwnedChild::spawn(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "tests::managed_collection_and_health_allocate_nothing_after_startup",
                        "--nocapture",
                    ])
                    .env("FLIGHTDECK_MANAGED_ALLOCATION", "1")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::inherit()),
            );
            assert!(child.wait(Duration::from_secs(5)).success());
            return;
        }
        diagnostics::install(Role::Collector);
        let timebase = Timebase::read().unwrap();
        let recording = TestRecording::new("managedalloc");
        recording::private::create_private_dir(&recording.root).unwrap();
        let _owner =
            recording::private::FileLock::try_lock(&recording.root.join("recorder.lock")).unwrap();
        let control = recording::control::Control::create(&recording.root).unwrap();
        let options = Options {
            seconds: NonZeroU64::new(TICKS_PER_MINUTE),
            rescan_interval: DEFAULT_RESCAN_INTERVAL,
            ring_name: "managedalloc".into(),
            recording: Some(recording.root.clone()),
            parent_pid: None,
            recorder_authority: Some(control.authority()),
        };
        let mut out = diagnostics::stdout_group();
        let mut run = Run::start(
            timebase,
            TEST_TICK_PERIOD_NS,
            DEFAULT_RESCAN_INTERVAL,
            &recording.ring.place,
            &recording.root,
            &mut *out,
        )
        .unwrap();
        out.submit();
        let mut health = Some(
            recording::control::Reporter::new(control, recording::control::Role::Collector)
                .unwrap(),
        );
        let owner = owner::Owner::bind(None).unwrap();
        // The marker process belongs to the final missed-process diagnostic; make that
        // independent diagnostic unavailable so this measurement covers collection itself.
        let marker = Err(io::Error::from(io::ErrorKind::NotFound));
        let calls = allocator_calls_in(|| {
            run_collect(&options, owner, &mut run, marker, &mut out, &mut health).unwrap();
        });
        assert_eq!(calls, 0);
        let current = health
            .as_ref()
            .unwrap()
            .control()
            .read(recording::control::Role::Collector)
            .unwrap();
        assert_eq!(current.publication, run.ring.publication_newest());
        assert!(current.publication > 1);
    }

    #[test]
    fn takes_the_saved_publication_only_from_a_persister_of_the_same_run() {
        // Goal: the collector tells its ring that events are saved only where the persister
        // reports the ring's own run, so a persister still saving an earlier run lets go of none
        // of this run's events.
        // Method: report a saved publication for one run in the persister's record, and read it
        // for that run, for another run and for a ring that names no run.
        use collector::ring::{Identity, RunIdentity};
        use recording::control::{Control, Reporter, Role as Worker, State};

        let recording = TestRecording::new("savedrun");
        recording::private::create_private_dir(&recording.root).unwrap();
        let control = Control::create(&recording.root).unwrap();
        let mut persister = Reporter::new(
            Control::open_writable(&recording.root).unwrap(),
            Worker::Persister,
        )
        .unwrap();
        let run = |run| RunIdentity {
            recording: Identity([1, 2]),
            run: Identity(run),
            boot: Identity([5, 6]),
        };
        persister
            .update(State::Running, Identity([3, 4]), 61)
            .unwrap();

        assert_eq!(Some(61), run_collect_saved(&control, Some(run([3, 4]))));
        assert_eq!(None, run_collect_saved(&control, Some(run([7, 8]))));
        assert_eq!(None, run_collect_saved(&control, None));
    }

    #[test]
    fn preserves_the_registration_refusal_before_managed_collection_starts() {
        // Goal: a managed worker refuses unregistered acquisition with the original cause.
        // Method: offer a public directory through the actual run startup boundary.
        use std::os::unix::fs::PermissionsExt;

        let timebase = Timebase::read().unwrap();
        let recording = TestRecording::new("registrationrefusal");
        std::fs::create_dir(&recording.root).unwrap();
        std::fs::set_permissions(&recording.root, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut output = io::Cursor::new(vec![0_u8; 32_768].into_boxed_slice());
        let refused = Run::start(
            timebase,
            TEST_TICK_PERIOD_NS,
            DEFAULT_RESCAN_INTERVAL,
            &recording.ring.place,
            &recording.root,
            &mut output,
        )
        .err()
        .expect("managed startup must retain registration failure");
        assert_eq!(refused.kind(), io::ErrorKind::PermissionDenied);
        assert!(!recording.root.join("runs").exists());
    }

    #[test]
    fn keeps_collecting_when_observation_clocks_are_unavailable() {
        // Goal: a clock snapshot outside its representable range cannot stop collection.
        // Method: refuse the clock at the actual tick boundary and read its next publication.
        let timebase = Timebase::read().unwrap();
        let recording = TestRecording::new("clockrefusal");
        let mut output = io::Cursor::new(vec![0_u8; 32_768].into_boxed_slice());
        let mut run = Run::start(
            timebase,
            TEST_TICK_PERIOD_NS,
            DEFAULT_RESCAN_INTERVAL,
            &recording.ring.place,
            &recording.root,
            &mut output,
        )
        .unwrap();

        let calls = allocator_calls_in(|| {
            run.tick_with_observation(run.first_tick, &mut output, clock::now, |_timebase| {
                Err(io::Error::from(io::ErrorKind::InvalidData))
            })
            .unwrap();
        });

        let mut reader = Reader::open(&recording.ring.place.lock_path).unwrap();
        reader.read().unwrap();
        reader.read().unwrap();
        assert_eq!(None, reader.tick().times);
        assert!(!reader.tick().processes.is_empty());
        assert_eq!(0, calls);
        let count = usize::try_from(output.position()).unwrap();
        let report = std::str::from_utf8(&output.get_ref()[..count]).unwrap();
        assert!(report.contains("observation clocks unavailable kind=InvalidData"));
    }

    #[test]
    fn publishes_the_events_cpus_and_interfaces_each_tick_read() {
        // Goal: each tick hands the ring the process events it drained, under the event
        // numbers its publication names, with the CPUs and interfaces it read.
        // Method: run ten ticks 20 ms apart while another thread starts processes, then read
        // every publication and every event the ring holds.
        let timebase = Timebase::read().unwrap();
        let mut sink = io::sink();
        let recording = TestRecording::new("event");
        let mut run = Run::start(
            timebase,
            TEST_TICK_PERIOD_NS,
            DEFAULT_RESCAN_INTERVAL,
            &recording.ring.place,
            &recording.root,
            &mut sink,
        )
        .unwrap();
        while_forking(|| {
            let mut tick = run.first_tick;
            while tick <= 10 {
                tick = run.tick(tick, &mut sink).unwrap().following;
            }
        });

        let mut reader = Reader::open(&recording.ring.place.lock_path).unwrap();
        let mut named = 0;
        while let Found::Publication { .. } = reader.read().unwrap() {
            assert!(!reader.tick().cpus.is_empty());
            assert!(!reader.tick().interfaces.is_empty());
            named += reader.tick().events.end - reader.tick().events.start;
        }
        let mut starts = 0;
        let mut found = 0;
        while let FoundEvent::Event { event, .. } = reader.read_event().unwrap() {
            found += 1;
            if let Event::Start { .. } = event {
                starts += 1;
            }
        }

        assert!(starts > 0);
        assert_eq!(named, found);
    }

    fn arguments(words: &[&str]) -> Vec<String> {
        words.iter().map(|&word| word.to_owned()).collect()
    }

    #[test]
    fn runs_until_stopped_without_arguments() {
        // Goal: a run given no arguments runs until it is stopped.
        // Method: parse an empty argument list.
        assert_eq!(None, options_from(&arguments(&[])).unwrap().seconds);
    }

    #[test]
    fn accepts_an_explicit_parent_pid_for_an_app_owned_run() {
        // Goal: app-launched collection can bind to its actual parent through an explicit flag.
        // Method: parse an otherwise ordinary run with the highest permitted parent PID.
        assert_eq!(
            Some(99_999),
            options_from(&arguments(&["--parent-pid", "99999"]))
                .unwrap()
                .parent_pid
        );
    }

    macro_rules! parent_options {
        ($($name:ident, $value:expr;)*) => {$(
            #[test]
            fn $name() {
                // Goal: an invalid parent PID is a startup usage error, never an assertion.
                // Method: parse one fixed invalid parent flag value through the real CLI parser.
                assert!(options_from(&arguments(&["--parent-pid", $value])).is_err());
            }
        )*};
    }

    parent_options! {
        rejects_a_zero_parent_pid, "0";
        rejects_launchd_as_the_expected_parent, "1";
        rejects_a_negative_parent_pid, "-2";
        rejects_a_non_numeric_parent_pid, "parent";
        rejects_a_parent_pid_past_the_kernel_bound, "100000";
        rejects_a_parent_pid_past_i32, "2147483648";
    }

    #[test]
    fn rejects_a_missing_parent_pid_value() {
        // Goal: parent ownership requires the explicit numeric value after its flag.
        // Method: parse the flag alone.
        assert!(options_from(&arguments(&["--parent-pid"])).is_err());
    }

    #[test]
    fn ordinary_collection_has_no_parent_binding() {
        // Goal: external CLI collection remains independent of its original caller.
        // Method: parse the default options and confirm no ownership was requested.
        assert_eq!(None, options_from(&arguments(&[])).unwrap().parent_pid);
    }

    #[test]
    fn rejects_a_wrong_parent_before_using_the_ring_or_sensors() {
        // Goal: startup refuses ownership before expensive collection or invalid ring setup.
        // Method: request this process as its own parent and an unusable ring name.
        let own_pid = processes::own_pid().unwrap().to_string();
        let options = options_from(&arguments(&[
            "--parent-pid",
            &own_pid,
            "--ring",
            "invalid/ring",
        ]))
        .unwrap();

        let error = run(&options).unwrap_err();

        assert_eq!(io::ErrorKind::InvalidInput, error.kind());
        assert!(error.to_string().contains("actual parent PID"));
    }

    #[test]
    fn binds_the_actual_parent_and_checks_it_without_allocating() {
        // Goal: startup binding and collection-step parent checks use no Rust heap operations.
        // Method: read the real parent and measure binding plus 1000000 scalar checks.
        let parent = owner::parent_pid();
        let timebase = Timebase::read().unwrap();
        let pid = processes::own_pid().unwrap();
        let before = processes::read_usage(timebase, pid).unwrap().unwrap();
        let started = std::time::Instant::now();
        let mut checks = 0;
        let calls = allocator_calls_in(|| {
            let bound = owner::Owner::bind(Some(parent)).unwrap();
            let independent = owner::Owner::bind(None).unwrap();
            for _attempt in 0..1_000_000 {
                assert!(!bound.departed());
                assert!(!independent.departed());
                checks += 1;
            }
        });
        let elapsed_ns = started.elapsed().as_nanos();
        let after = processes::read_usage(timebase, pid).unwrap().unwrap();

        assert_eq!(1_000_000, checks);
        assert_eq!(0, calls);
        eprintln!(
            "parent checks={checks} wall_ns={elapsed_ns} rust_allocator_calls={calls} cpu_ns={}",
            after.user_ns + after.system_ns - before.user_ns - before.system_ns
        );
    }

    #[test]
    fn runs_for_the_seconds_given() {
        // Goal: --seconds sets how long a run lasts.
        // Method: parse --seconds 5.
        assert_eq!(
            NonZeroU64::new(5),
            options_from(&arguments(&["--seconds", "5"]))
                .unwrap()
                .seconds
        );
    }

    #[test]
    fn records_to_the_explicit_private_destination() {
        // Goal: an isolated recording path leaves the selected ring name intact.
        // Method: parse both independent destination options.
        let options = options_from(&arguments(&[
            "--ring",
            "isolated",
            "--recording",
            "/tmp/isolated-recording",
        ]))
        .unwrap();

        assert_eq!(options.ring_name, "isolated");
        assert_eq!(
            options.recording,
            Some(PathBuf::from("/tmp/isolated-recording"))
        );
    }

    #[test]
    fn rejects_a_run_of_zero_seconds() {
        // Goal: a run of no seconds is a usage error, since it would report nothing.
        // Method: parse --seconds 0.
        assert!(options_from(&arguments(&["--seconds", "0"])).is_err());
    }

    #[test]
    fn takes_no_rescan_interval_without_arguments() {
        // Goal: a run given no arguments rescans after every wake that carries a fork.
        // Method: parse an empty argument list.
        assert_eq!(
            Duration::ZERO,
            options_from(&arguments(&[])).unwrap().rescan_interval
        );
    }

    #[test]
    fn rescans_at_the_interval_given_in_milliseconds() {
        // Goal: --rescan-ms sets the rescan interval in milliseconds.
        // Method: parse --rescan-ms 20.
        assert_eq!(
            Duration::from_millis(20),
            options_from(&arguments(&["--rescan-ms", "20"]))
                .unwrap()
                .rescan_interval
        );
    }

    #[test]
    fn takes_both_options_in_either_order() {
        // Goal: the options parse in any order.
        // Method: parse --rescan-ms before --seconds.
        assert_eq!(
            Ok(Options {
                seconds: NonZeroU64::new(5),
                rescan_interval: Duration::from_millis(20),
                ring_name: DEFAULT_RING.to_owned(),
                recording: None,
                parent_pid: None,
                recorder_authority: None,
            }),
            options_from(&arguments(&["--rescan-ms", "20", "--seconds", "5"]))
        );
    }

    #[test]
    fn rejects_an_unknown_option() {
        // Goal: a flag the collector does not take is a usage error.
        // Method: parse --minutes 5.
        assert!(options_from(&arguments(&["--minutes", "5"])).is_err());
    }

    #[test]
    fn counts_the_pids_between_the_marks_less_those_live_at_the_first_scan() {
        // Goal: the processes started are the pids between the marks, less those the first scan
        // found already live.
        // Method: count between pids 100 and 110 with two of the nine pids between them live at the
        // first scan and three outside them.
        let markers = Markers {
            first: 100,
            last: 110,
        };
        assert_eq!(
            Some(7),
            markers.processes_started(&[50, 103, 105, 110, 200], 1_000)
        );
    }

    #[test]
    fn keeps_each_pid_live_at_the_first_scan_once_in_order() {
        // Goal: the first scan's listing, which comes in no set order and may hold a pid
        // twice as the tracker allows, reaches the count sorted and with each pid once.
        // Method: take a listing in decreasing order that holds one pid twice.
        let listing = [103, 101, 103];

        let live_at_first_scan = Run::start_live_at_first_scan(&listing);

        assert_eq!(vec![101, 103], live_at_first_scan);
    }

    #[test]
    #[should_panic(expected = "listed[0] < listed[1]")]
    fn refuses_a_first_scan_out_of_order() {
        // Goal: the count takes the first scan's pids in increasing order.
        // Method: count with a listing in decreasing order.
        let markers = Markers {
            first: 100,
            last: 104,
        };
        markers.processes_started(&[103, 101], 1_000);
    }

    #[test]
    #[should_panic(expected = "listed[0] < listed[1]")]
    fn refuses_a_first_scan_that_lists_a_pid_twice() {
        // Goal: the count takes each of the first scan's pids once, since a pid listed
        // twice would take two pids off the processes started.
        // Method: count between marks one pid apart with that pid listed twice.
        let markers = Markers {
            first: 100,
            last: 102,
        };
        markers.processes_started(&[101, 101], 1_000);
    }

    #[test]
    fn gives_no_count_once_the_pids_wrapped() {
        // Goal: a last mark below the first means the pids wrapped, so no count is given.
        // Method: count with the last mark's pid below the first's.
        let markers = Markers {
            first: 99_000,
            last: 300,
        };
        assert_eq!(None, markers.processes_started(&[], 1_000));
    }

    #[test]
    fn gives_no_count_once_the_forks_counted_could_fill_a_lap_of_pids() {
        // Goal: enough forks to lap the pids could put the last mark past the first again, so
        // no count is given.
        // Method: count with as many forks as a lap of pids holds.
        let markers = Markers {
            first: 100,
            last: 110,
        };
        assert_eq!(
            None,
            markers.processes_started(&[], processes_started_pid_lap())
        );
    }

    #[test]
    fn counts_as_missed_the_processes_started_but_never_found() {
        // Goal: the processes missed are those started less those found.
        // Method: take 7 found from 10 started.
        let starts = Starts {
            started: Some(10),
            found: 7,
            dropped: 0,
        };
        assert_eq!(Some(3), starts.missed());
    }

    #[test]
    fn gives_no_miss_count_when_more_were_found_than_the_pids_say_started() {
        // Goal: more found than started means the count of starts is wrong, so no miss count
        // is given.
        // Method: take 11 found from 10 started.
        let starts = Starts {
            started: Some(10),
            found: 11,
            dropped: 0,
        };
        assert_eq!(None, starts.missed());
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
        let tally = Tally {
            ticks: 1,
            ..Tally::default()
        };
        let mut written = Vec::new();

        report_counts(&mut written, &tally, &counts).unwrap();

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
        finish_report_missed(&mut written, &starts).unwrap();

        assert_eq!(
            "  processes started over the run 10, found 7, missed 3, \
             2 of them dropped from a full event list\n",
            String::from_utf8(written).unwrap()
        );
    }

    #[test]
    fn averages_the_lateness_over_the_ticks() {
        // Goal: a span's average lateness is its total lateness over its ticks.
        // Method: average 30 ms over 3 ticks, the latest of them 15 ms late.
        let tally = Tally {
            ticks: 3,
            late_total: Duration::from_millis(30),
            late_max: Duration::from_millis(15),
            ..Tally::default()
        };

        assert_eq!(Duration::from_millis(10), tally.late_average().unwrap());
    }

    #[test]
    fn refuses_an_average_over_more_ticks_than_a_u32_holds() {
        // Goal: a span of more ticks than a u32 holds is an error, not an average over
        // u32::MAX ticks.
        // Method: average over one tick past u32::MAX.
        let tally = Tally {
            ticks: u64::from(u32::MAX) + 1,
            late_total: Duration::from_secs(1),
            ..Tally::default()
        };

        let average = tally.late_average();

        assert_eq!(io::ErrorKind::InvalidData, average.unwrap_err().kind());
    }

    #[test]
    fn writes_none_of_a_span_whose_lateness_cannot_be_averaged() {
        // Goal: a span whose lateness cannot be averaged is an error that writes none of its
        // line, so no partial reading reaches the report.
        // Method: report a span counting one tick past u32::MAX into a buffer, between two
        // snapshots a cost can be taken over.
        let timebase = Timebase::read().unwrap();
        let meter = Meter::default();
        let start = Mark {
            snapshot: Snapshot::take(&meter, timebase).unwrap(),
            counts: Counts::default(),
        };
        let end = Mark {
            snapshot: Snapshot::take(&meter, timebase).unwrap(),
            counts: Counts::default(),
        };
        assert!(end.snapshot.cost_since(&start.snapshot).is_ok());
        let mut span = Span::begin(start, 1);
        span.tally.ticks = u64::from(u32::MAX) + 1;
        let mut written = Vec::new();

        let report = span.report(&mut written, "ticks", span.tally.ticks, &end);

        assert_eq!(io::ErrorKind::InvalidData, report.unwrap_err().kind());
        assert_eq!("", String::from_utf8(written).unwrap());
    }

    #[test]
    fn closes_a_window_after_its_tenth_tick() {
        // Goal: a window closes after its tenth tick.
        // Method: ask whether tick 10 closes a window, with tick 11 following it.
        let step = Step {
            requested: 10,
            observed: 10,
            following: 11,
        };
        assert!(step.closes_span(TICKS_PER_WINDOW));
    }

    #[test]
    fn keeps_a_window_open_before_its_tenth_tick() {
        // Goal: a window stays open through its ninth tick.
        // Method: ask whether tick 9 closes a window, with tick 10 following it.
        let step = Step {
            requested: 9,
            observed: 9,
            following: 10,
        };
        assert!(!step.closes_span(TICKS_PER_WINDOW));
    }

    #[test]
    fn closes_a_window_whose_tenth_tick_slipped() {
        // Goal: a window whose tenth tick slipped still closes, after the tick before it.
        // Method: ask whether tick 9 closes a window, with tick 11 following it.
        let step = Step {
            requested: 9,
            observed: 9,
            following: 11,
        };
        assert!(step.closes_span(TICKS_PER_WINDOW));
    }

    #[test]
    fn closes_a_minute_after_its_sixtieth_tick() {
        // Goal: a minute closes after its sixtieth tick.
        // Method: ask whether tick 60 closes a minute, with tick 61 following it.
        let step = Step {
            requested: 60,
            observed: 60,
            following: 61,
        };
        assert!(step.closes_span(TICKS_PER_MINUTE));
    }

    #[test]
    #[should_panic(expected = "self.requested > 0")]
    fn refuses_the_warm_up_tick() {
        // Goal: the warm-up tick, which `Run::start` runs apart, never reaches a span.
        // Method: ask whether tick 0 closes a window.
        let step = Step {
            requested: 0,
            observed: 0,
            following: 1,
        };
        step.closes_span(TICKS_PER_WINDOW);
    }
}
