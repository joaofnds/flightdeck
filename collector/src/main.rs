mod stop;

use std::ffi::c_int;
use std::io::{self, Write};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use collector::clock::{self, Timebase};
use collector::collector::{Capacities, Collector};
use collector::cost::{Cost, Meter, Snapshot, Source};
use collector::processes::{self, Unreadable, Usage};
use collector::recording::child::{ChildCost, Persister};
use collector::recording::{self, Registration};
use collector::ring::{
    Dropped, IOReportReadings, ObservationTimes, Place, Readings, Shape, SmcReadings, Writer,
};
use collector::schedule::Schedule;
use collector::tracker::{Counts, Event, Noticed};
use collector::u64_from_usize;

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

unsafe extern "C" {
    fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: c_int) -> c_int;
}

fn main() -> ExitCode {
    let options =
        match collector::arguments("collector").and_then(|arguments| options_from(&arguments)) {
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
    ring_name: String,
    recording: Option<PathBuf>,
}

fn options_from(arguments: &[String]) -> Result<Options, String> {
    let usage = || {
        "usage: collector [--seconds N] [--rescan-ms N] [--ring NAME] \
        [--recording PATH]"
            .to_owned()
    };
    let mut options = Options {
        seconds: None,
        rescan_interval: DEFAULT_RESCAN_INTERVAL,
        ring_name: DEFAULT_RING.to_owned(),
        recording: None,
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
            _ => return Err(usage()),
        }
    }

    // Each flag took the word after it, so the words came in pairs.
    assert!(arguments.len().is_multiple_of(2));
    Ok(options)
}

fn run(options: &Options) -> io::Result<()> {
    run_set_qos_class_default()?;
    let _stop = stop::Stop::install()?;

    let timebase = Timebase::read()?;
    let first_marker_pid = run_throwaway_child();
    let mut out = io::stdout().lock();
    let place = Place::for_user(&options.ring_name)?;
    let recording = options
        .recording
        .clone()
        .unwrap_or_else(|| place.lock_path.with_extension("recording"));
    let mut run = Run::start(
        timebase,
        TICK_PERIOD_NS,
        options.rescan_interval,
        &place,
        &recording,
        &mut out,
    )?;
    let mut tick = run.first_tick;
    // Tick 0 is the warm-up tick, which `Run::start` ran.
    assert!(tick > 0);
    loop {
        let following = match run.tick(tick, &mut out) {
            Ok(following) => following,
            Err(error) => {
                if error.kind() == io::ErrorKind::Interrupted {
                    if stop::Stop::requested() {
                        return run.finish_recording(&mut out);
                    }
                }
                return Err(error);
            }
        };
        assert!(following > tick);
        let elapsed = options
            .seconds
            .is_some_and(|seconds| following > seconds.get());
        if elapsed {
            return run_finish(&mut run, tick, first_marker_pid, &mut out);
        }
        if stop::Stop::requested() {
            return run_finish(&mut run, tick, first_marker_pid, &mut out);
        }

        tick = following;
    }
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

/// The shape of a ring with room for every tick the collector of `capacities` reads.
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
        // Every event of the ticks the ring holds, at the most a tick holds.
        event_capacity: RING_SLOTS * u64_from_usize(capacities.events),
    }
}

/// Publishes what `collector` read in `tick` to `ring`.
fn publish(ring: &mut Writer, collector: &Collector, tick: u64, times: Option<&ObservationTimes>) {
    let ioreport = collector
        .ioreport
        .as_ref()
        .map(|ioreport| IOReportReadings {
            gpu_energy_nj: ioreport.gpu_energy_nj(),
            gpu: ioreport.gpu(),
            cores: ioreport.cores(),
        });
    let smc = collector.smc.as_ref().map(|smc| SmcReadings {
        power_watts: smc.power_watts(),
        soc_celsius: smc.soc_celsius(),
        fan_rpm: smc.fan_rpm(),
    });
    let counts = collector.counts_tick();
    let readings = Readings {
        run_tick: tick,
        cpus: collector.machine.cpus(),
        memory: &collector.machine.memory,
        interfaces: collector.interfaces.readings(),
        ioreport,
        smc,
        processes: &collector.processes.readings,
        processes_refused: collector.processes.refused,
        processes_gone: collector.processes.gone,
        coalitions: &collector.coalitions.readings,
        events: collector.events(),
        dropped: Dropped {
            starts: counts.dropped_starts,
            execs: counts.dropped_execs,
            exits: counts.dropped_exits,
        },
    };
    match times {
        Some(times) => ring.publish_observed(&readings, times),
        None => ring.publish(&readings),
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
    persistence: Persistence,
    live_at_first_scan: Vec<i32>,
    warm_up_starts: u64,
    first_tick: u64,
    whole: Span,
    minute: Span,
    window: Span,
}

/// Starts recording only where private registration succeeded; failure never stops sampling.
fn start_recording_ring(
    place: &Place,
    shape: Shape,
    root: &Path,
    timebase: Timebase,
    out: &mut impl Write,
) -> io::Result<(Writer, bool)> {
    let start_ns = timebase.nanoseconds(clock::now())?;
    let (ring, registered) = Registration::start(root, place, shape, start_ns)?.into_parts();
    match registered {
        Ok(()) => Ok((ring, true)),
        Err(error) => {
            writeln!(
                out,
                "recording registration failed: {error}; collection continues; \
                unregistered history recovery is unavailable"
            )?;
            Ok((ring, false))
        }
    }
}

struct Persistence {
    child: Option<Persister>,
    reported: bool,
    minute_at_ns: u64,
    minute_collector_ns: u64,
    minute_persister_ns: Option<u64>,
}

struct RecordingInterval {
    elapsed: Duration,
    collector: Duration,
    persister: Option<Duration>,
}

struct OptionalNanoseconds(Option<u64>);

impl std::fmt::Display for OptionalNanoseconds {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            Some(value) => write!(formatter, "{value}"),
            None => formatter.write_str("unavailable"),
        }
    }
}

impl Persistence {
    fn start_with(
        root: Option<&Path>,
        start: &Mark,
        timebase: Timebase,
        out: &mut impl Write,
        launch: impl FnOnce(&Path, Timebase) -> io::Result<Persister>,
    ) -> io::Result<Self> {
        let minute_at_ns = timebase.nanoseconds(clock::now())?;
        let child = if let Some(root) = root {
            match launch(root, timebase) {
                Ok(child) => Some(child),
                Err(error) => {
                    writeln!(
                        out,
                        "persister startup failed: {error}; collection continues; \
                        unsealed history may be lost"
                    )?;
                    None
                }
            }
        } else {
            None
        };
        let minute_persister_ns = child.as_ref().map(|_child| 0);
        Ok(Self {
            child,
            reported: false,
            minute_at_ns,
            minute_collector_ns: start.snapshot.cpu_ns(),
            minute_persister_ns,
        })
    }

    fn start_child(root: &Path, timebase: Timebase) -> io::Result<Persister> {
        let executable = std::env::current_exe()?.canonicalize()?;
        let directory = executable
            .parent()
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        Persister::spawn(&directory.join("persister"), root, timebase)
    }

    fn poll(&mut self, out: &mut impl Write) -> io::Result<()> {
        let Some(child) = &mut self.child else {
            return Ok(());
        };
        match child.poll() {
            Ok(None) => {}
            Ok(Some(finished)) => {
                if !self.reported {
                    if finished.owned_by_surviving_persister() {
                        writeln!(
                            out,
                            "recording owned by surviving persister; CPU not measured; \
                            collection continues"
                        )?;
                    } else {
                        writeln!(
                            out,
                            "persister exited status={} cpu_ns={}; collection continues; \
                            unsealed history may be lost",
                            finished.status, finished.cpu_ns
                        )?;
                    }
                    self.reported = true;
                }
            }
            Err(error) => {
                if !self.reported {
                    writeln!(
                        out,
                        "persister status unavailable kind={:?} raw_status={:?}; \
                        collection continues",
                        error.kind(),
                        error.raw_os_error()
                    )?;
                    self.reported = true;
                }
            }
        }
        Ok(())
    }

    fn report(
        &mut self,
        label: &str,
        collector_cpu_ns: u64,
        timebase: Timebase,
        out: &mut impl Write,
    ) -> io::Result<()> {
        let at_ns = timebase.nanoseconds(clock::now())?;
        let elapsed_ns = at_ns
            .checked_sub(self.minute_at_ns)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
        if elapsed_ns == 0 {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        let collector_ns = collector_cpu_ns
            .checked_sub(self.minute_collector_ns)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
        let sample = self.sample(out)?;
        let child_ns = sample.as_ref().map(|cost| cost.cpu_ns);
        let delta = match (child_ns, self.minute_persister_ns) {
            (Some(end), Some(start)) => end.checked_sub(start),
            _ => None,
        };
        Self::report_interval(
            label,
            &RecordingInterval {
                elapsed: Duration::from_nanos(elapsed_ns),
                collector: Duration::from_nanos(collector_ns),
                persister: delta.map(Duration::from_nanos),
            },
            out,
        )?;
        Self::report_cumulative(collector_cpu_ns, sample.as_ref(), out)?;
        self.minute_at_ns = at_ns;
        self.minute_collector_ns = collector_cpu_ns;
        self.minute_persister_ns = child_ns;
        Ok(())
    }

    fn report_interval(
        label: &str,
        interval: &RecordingInterval,
        out: &mut impl Write,
    ) -> io::Result<()> {
        assert!(!interval.elapsed.is_zero());
        match interval.persister {
            Some(persister) => {
                let total = interval
                    .collector
                    .checked_add(persister)
                    .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
                let percent = total.div_duration_f64(interval.elapsed) * 100.0;
                writeln!(
                    out,
                    "recording {label} elapsed_ns={} \
                    collector_cpu_ns={} persister_cpu_ns={} \
                    combined_cpu_ns={} combined_percent={percent:.6}",
                    interval.elapsed.as_nanos(),
                    interval.collector.as_nanos(),
                    persister.as_nanos(),
                    total.as_nanos()
                )?;
            }
            None => writeln!(
                out,
                "recording {label} elapsed_ns={} \
                collector_cpu_ns={} persister_cpu_ns=unavailable \
                combined_cpu_ns=unavailable combined_percent=unavailable",
                interval.elapsed.as_nanos(),
                interval.collector.as_nanos()
            )?,
        }
        Ok(())
    }

    fn report_cumulative(
        collector_ns: u64,
        sample: Option<&ChildCost>,
        out: &mut impl Write,
    ) -> io::Result<()> {
        match sample {
            Some(cost) => writeln!(
                out,
                "recording cumulative collector_cpu_ns={collector_ns} \
                persister_cpu_ns={} current_footprint_bytes={} lifetime_footprint_bytes_max={}",
                cost.cpu_ns, cost.footprint_bytes, cost.footprint_bytes_max
            ),
            None => writeln!(
                out,
                "recording cumulative collector_cpu_ns={collector_ns} \
                persister_cpu_ns=unavailable current_footprint_bytes=unavailable \
                lifetime_footprint_bytes_max=unavailable"
            ),
        }
    }

    fn sample(&mut self, out: &mut impl Write) -> io::Result<Option<ChildCost>> {
        let Some(child) = &mut self.child else {
            return Ok(None);
        };
        match child.sample() {
            Ok(sample) => Ok(sample),
            Err(error) => {
                writeln!(
                    out,
                    "persister cost unavailable kind={:?} raw_status={:?}",
                    error.kind(),
                    error.raw_os_error()
                )?;
                Ok(None)
            }
        }
    }

    fn finish(
        &mut self,
        meter: &Meter,
        timebase: Timebase,
        out: &mut impl Write,
    ) -> io::Result<()> {
        let before = Snapshot::take(meter, timebase)?;
        let begin = timebase.nanoseconds(clock::now())?;
        let sampled = self.sample(out)?.map(|cost| cost.cpu_ns);
        let finished = match &mut self.child {
            Some(child) => Some(child.finish(Duration::from_mins(2))?),
            None => None,
        };
        let after = Snapshot::take(meter, timebase)?;
        let collector_ns = after
            .cpu_ns()
            .checked_sub(before.cpu_ns())
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
        let elapsed_ns = timebase
            .nanoseconds(clock::now())?
            .checked_sub(begin)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
        match finished {
            Some(finished) => {
                if finished.owned_by_surviving_persister() {
                    writeln!(
                        out,
                        "recording drain owned by surviving persister; CPU not measured; \
                        collector_cpu_ns={collector_ns} persister_cpu_ns=unavailable \
                        combined_cpu_ns=unavailable"
                    )?;
                    return Ok(());
                }

                let drain_ns = sampled.and_then(|sample| finished.cpu_ns.checked_sub(sample));
                writeln!(
                    out,
                    "recording drain elapsed_ns={elapsed_ns} \
                    collector_cpu_ns={collector_ns} persister_cpu_ns={} \
                    persister_total_cpu_ns={} persister_status={} \
                    persister_resident_bytes_max={}",
                    OptionalNanoseconds(drain_ns),
                    finished.cpu_ns,
                    finished.status,
                    finished.resident_bytes_max
                )?;
                if !finished.successful() {
                    writeln!(
                        out,
                        "persister failed during drain; \
                        unsealed history may be lost, extent unknown"
                    )?;
                }
            }
            None => writeln!(
                out,
                "recording drain elapsed_ns={elapsed_ns} \
                collector_cpu_ns={collector_ns} persister_cpu_ns=unavailable \
                persister_total_cpu_ns=unavailable"
            )?,
        }
        Ok(())
    }
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
        Self::start_with(
            timebase,
            tick_period_ns,
            rescan_interval,
            place,
            recording,
            out,
            Persistence::start_child,
        )
    }

    fn start_with(
        timebase: Timebase,
        tick_period_ns: u64,
        rescan_interval: Duration,
        place: &Place,
        recording: &Path,
        out: &mut impl Write,
        launch: impl FnOnce(&Path, Timebase) -> io::Result<Persister>,
    ) -> io::Result<Self> {
        assert!(tick_period_ns > 0);

        let mut meter = Meter::default();
        let mut collector = Collector::new(timebase, rescan_interval)?;
        // A second collector stops here, before its first tick, so the first runs on.
        let (mut ring, registered) = start_recording_ring(
            place,
            start_ring_shape(collector.capacities()),
            recording,
            timebase,
            out,
        )?;
        let live_at_first_scan = Self::start_live_at_first_scan(collector.tracker.last_listing());
        let schedule = Schedule::starting_now(timebase.ticks(tick_period_ns)?)?;

        start_report(out, &meter, timebase)?;

        collector.wait_until(schedule.deadline(0), &mut meter)?;
        let observed = observation_or_unavailable(recording::observation_times(timebase), out)?;
        collector.read(&mut meter)?;
        meter.measure(Source::Ring, || {
            publish(&mut ring, &collector, 0, observed.as_ref());
            Ok(())
        })?;
        let warm_up_starts = start_warm_up_starts(collector.events());

        // Tick 0 is the warm-up tick, so the spans start past it.
        let first_tick = schedule.first_ahead_of(clock::now()).max(1);
        let start = Mark::take(&meter, timebase, &collector)?;
        let registered_root = if registered { Some(recording) } else { None };
        let persistence = Persistence::start_with(registered_root, &start, timebase, out, launch)?;
        Ok(Self {
            timebase,
            schedule,
            meter,
            collector,
            ring,
            persistence,
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

    /// Runs `tick`, writes the reports of the spans it closes, and returns the tick that
    /// follows it, past any tick the run slipped.
    fn tick(&mut self, tick: u64, out: &mut impl Write) -> io::Result<u64> {
        self.tick_with_observation(tick, out, recording::observation_times)
    }

    fn tick_with_observation(
        &mut self,
        tick: u64,
        out: &mut impl Write,
        observe: impl FnOnce(Timebase) -> io::Result<ObservationTimes>,
    ) -> io::Result<u64> {
        assert!(tick >= self.first_tick);

        let deadline = self.schedule.deadline(tick);
        self.collector.wait_until(deadline, &mut self.meter)?;
        let late_ticks = clock::now().saturating_sub(deadline);
        let late = Duration::from_nanos(self.timebase.nanoseconds(late_ticks)?);
        let observed = observation_or_unavailable(observe(self.timebase), out)?;
        self.collector.read(&mut self.meter)?;
        self.meter.measure(Source::Ring, || {
            publish(&mut self.ring, &self.collector, tick, observed.as_ref());
            Ok(())
        })?;

        self.persistence.poll(out)?;
        let following = self.schedule.first_ahead_of(clock::now()).max(tick + 1);
        let slips = following - tick - 1;
        self.whole.tally.add(late, slips, &self.collector);
        self.minute.tally.add(late, slips, &self.collector);
        self.window.tally.add(late, slips, &self.collector);
        // A span closes on its last tick, so it never runs more ticks than it spans.
        assert!(self.window.tally.ticks <= TICKS_PER_WINDOW);
        assert!(self.minute.tally.ticks <= TICKS_PER_MINUTE);
        assert_eq!(following, self.whole.following());
        assert_eq!(following, self.minute.following());
        assert_eq!(following, self.window.following());

        let step = Step { tick, following };
        if step.closes_span(TICKS_PER_WINDOW) {
            let now = Mark::take(&self.meter, self.timebase, &self.collector)?;
            self.window.report(out, "ticks", tick, &now)?;
            self.window = Span::begin(now, following);
        }
        if step.closes_span(TICKS_PER_MINUTE) {
            let now = Mark::take(&self.meter, self.timebase, &self.collector)?;
            self.minute.report(out, "minute, ticks", tick, &now)?;
            self.persistence
                .report("minute", now.snapshot.cpu_ns(), self.timebase, out)?;
            self.minute = Span::begin(now, following);
        }

        Ok(following)
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
        let end = Snapshot::take(&self.meter, self.timebase)?;
        self.persistence
            .report("tail", end.cpu_ns(), self.timebase, out)?;
        self.ring.finish();
        self.persistence.finish(&self.meter, self.timebase, out)
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

/// A tick the run ran and the tick that follows it, past any tick the run slipped.
struct Step {
    tick: u64,
    following: u64,
}

impl Step {
    /// Whether the span of `ticks_per_span` ticks that holds the tick ends before the one
    /// that follows it, with spans counted from tick 1.
    fn closes_span(&self, ticks_per_span: u64) -> bool {
        // `Run::start` runs the warm-up tick, 0, apart from every span.
        assert!(self.tick > 0);
        assert!(self.following > self.tick);
        assert!(ticks_per_span > 0);

        // Each division floors, to the span the tick falls in.
        (self.following - 1) / ticks_per_span > (self.tick - 1) / ticks_per_span
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

    fn slow_persister(timebase: Timebase, seconds: u64) -> io::Result<Persister> {
        let mut command = std::process::Command::new("/bin/sleep");
        command.arg(seconds.to_string());
        Persister::spawn_command(&mut command, timebase)
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
        // reports it writes, as Tiger Style's allocation at startup only asks.
        // Method: run ticks 20 ms apart until the minute closes, so they write six window
        // reports and a live child's minute cost while another thread starts processes.
        // Count the allocator calls made on this thread after private startup completes.
        let timebase = Timebase::read().unwrap();
        let mut sink = io::sink();
        let recording = TestRecording::new("alloc");
        let mut run = Run::start_with(
            timebase,
            TEST_TICK_PERIOD_NS,
            DEFAULT_RESCAN_INTERVAL,
            &recording.ring.place,
            &recording.root,
            &mut sink,
            |_root, timebase| slow_persister(timebase, 30),
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

    #[test]
    fn keeps_publishing_while_its_started_persister_has_not_finished() {
        // Goal: storage work cannot hold collector startup or the collection loop.
        // Method: run actual collector ticks with a slow native child, before any seal.
        let timebase = Timebase::read().unwrap();
        let recording = TestRecording::new("continuing");
        let mut output = io::Cursor::new(vec![0_u8; 32_768].into_boxed_slice());
        let mut run = Run::start_with(
            timebase,
            TEST_TICK_PERIOD_NS,
            DEFAULT_RESCAN_INTERVAL,
            &recording.ring.place,
            &recording.root,
            &mut output,
            |_root, timebase| slow_persister(timebase, 3),
        )
        .unwrap();
        let before = collector::ring::RingReader::open(&recording.ring.place.lock_path)
            .unwrap()
            .publication_newest()
            .unwrap();

        let next = run.tick(run.first_tick, &mut output).unwrap();
        run.tick(next, &mut output).unwrap();

        let after = collector::ring::RingReader::open(&recording.ring.place.lock_path)
            .unwrap()
            .publication_newest()
            .unwrap();
        assert!(after > before);
        assert!(run.persistence.sample(&mut output).unwrap().is_some());
        assert!(!recording.root.join("commits").exists());
    }

    #[test]
    fn keeps_publishing_when_private_registration_is_refused() {
        // Goal: an unusable recording destination cannot stop collection or launch a child.
        // Method: use a public directory, then read the actual collector's next publication.
        use std::os::unix::fs::PermissionsExt;

        let timebase = Timebase::read().unwrap();
        let recording = TestRecording::new("registrationrefusal");
        std::fs::create_dir(&recording.root).unwrap();
        std::fs::set_permissions(&recording.root, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut output = io::Cursor::new(vec![0_u8; 32_768].into_boxed_slice());
        let mut run = Run::start_with(
            timebase,
            TEST_TICK_PERIOD_NS,
            DEFAULT_RESCAN_INTERVAL,
            &recording.ring.place,
            &recording.root,
            &mut output,
            |_root, _timebase| panic!("a refused destination cannot launch its persister"),
        )
        .unwrap();
        let before = collector::ring::RingReader::open(&recording.ring.place.lock_path)
            .unwrap()
            .publication_newest()
            .unwrap();

        run.tick(run.first_tick, &mut output).unwrap();

        let reader = collector::ring::RingReader::open(&recording.ring.place.lock_path).unwrap();
        assert!(reader.publication_newest().unwrap() > before);
        assert!(run.persistence.sample(&mut output).unwrap().is_none());
        let count = usize::try_from(output.position()).unwrap();
        let report = std::str::from_utf8(&output.get_ref()[..count]).unwrap();
        assert!(report.contains("recording registration failed:"));
    }

    #[test]
    fn keeps_collecting_when_observation_clocks_are_unavailable() {
        // Goal: a clock snapshot outside its representable range cannot stop collection.
        // Method: refuse the clock at the actual tick boundary and read its next publication.
        let timebase = Timebase::read().unwrap();
        let recording = TestRecording::new("clockrefusal");
        let mut output = io::Cursor::new(vec![0_u8; 32_768].into_boxed_slice());
        let mut run = Run::start_with(
            timebase,
            TEST_TICK_PERIOD_NS,
            DEFAULT_RESCAN_INTERVAL,
            &recording.ring.place,
            &recording.root,
            &mut output,
            |_root, timebase| slow_persister(timebase, 30),
        )
        .unwrap();

        let calls = allocator_calls_in(|| {
            run.tick_with_observation(run.first_tick, &mut output, |_timebase| {
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
        let mut run = Run::start_with(
            timebase,
            TEST_TICK_PERIOD_NS,
            DEFAULT_RESCAN_INTERVAL,
            &recording.ring.place,
            &recording.root,
            &mut sink,
            |_root, timebase| slow_persister(timebase, 30),
        )
        .unwrap();
        while_forking(|| {
            let mut tick = run.first_tick;
            while tick <= 10 {
                tick = run.tick(tick, &mut sink).unwrap();
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
    fn reports_the_sum_of_both_measured_cpu_costs() {
        // Goal: the budget line charges persistence as well as collection.
        // Method: render known independent costs over one measured minute.
        let mut output = Vec::new();
        let interval = RecordingInterval {
            elapsed: Duration::from_mins(1),
            collector: Duration::from_millis(600),
            persister: Some(Duration::from_millis(300)),
        };

        Persistence::report_interval("minute", &interval, &mut output).unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            concat!(
                "recording minute elapsed_ns=60000000000 collector_cpu_ns=600000000 ",
                "persister_cpu_ns=300000000 combined_cpu_ns=900000000 combined_percent=1.500000\n"
            )
        );
    }

    #[test]
    fn reports_unavailable_combined_cost_when_persistence_is_unmeasured() {
        // Goal: missing child cost cannot become a plausible zero or collector-only total.
        // Method: render a known collector cost with unavailable persistence.
        let mut output = Vec::new();
        let interval = RecordingInterval {
            elapsed: Duration::from_mins(1),
            collector: Duration::from_millis(600),
            persister: None,
        };

        Persistence::report_interval("minute", &interval, &mut output).unwrap();

        assert_eq!(
            String::from_utf8(output).unwrap(),
            concat!(
                "recording minute elapsed_ns=60000000000 collector_cpu_ns=600000000 ",
                "persister_cpu_ns=unavailable combined_cpu_ns=unavailable ",
                "combined_percent=unavailable\n"
            )
        );
    }

    #[test]
    fn runs_until_stopped_without_arguments() {
        // Goal: a run given no arguments runs until it is stopped.
        // Method: parse an empty argument list.
        assert_eq!(None, options_from(&arguments(&[])).unwrap().seconds);
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
    fn observes_a_persister_exit_without_allocating_after_startup() {
        // Goal: detecting and reaping worker failure stays outside the allocation boundary.
        // Method: start a real run with a failing child and count its exit report's calls.
        let timebase = Timebase::read().unwrap();
        let recording = TestRecording::new("persisterexit");
        let mut output = io::Cursor::new(vec![0_u8; 32_768].into_boxed_slice());
        let mut run = Run::start_with(
            timebase,
            TEST_TICK_PERIOD_NS,
            DEFAULT_RESCAN_INTERVAL,
            &recording.ring.place,
            &recording.root,
            &mut output,
            |root, timebase| Persister::spawn(Path::new("/usr/bin/false"), root, timebase),
        )
        .unwrap();
        output.set_position(0);

        let calls = allocator_calls_in(|| {
            for _attempt in 0..2_000 {
                run.persistence.poll(&mut output).unwrap();
                if output.position() > 0 {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        });

        let count = usize::try_from(output.position()).unwrap();
        let report = std::str::from_utf8(&output.get_ref()[..count]).unwrap();
        assert!(report.contains("persister exited status=256"), "{report}");
        assert!(report.contains("collection continues"), "{report}");
        assert!(report.contains("unsealed history may be lost"), "{report}");
        assert_eq!(calls, 0);
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
            tick: 10,
            following: 11,
        };
        assert!(step.closes_span(TICKS_PER_WINDOW));
    }

    #[test]
    fn keeps_a_window_open_before_its_tenth_tick() {
        // Goal: a window stays open through its ninth tick.
        // Method: ask whether tick 9 closes a window, with tick 10 following it.
        let step = Step {
            tick: 9,
            following: 10,
        };
        assert!(!step.closes_span(TICKS_PER_WINDOW));
    }

    #[test]
    fn closes_a_window_whose_tenth_tick_slipped() {
        // Goal: a window whose tenth tick slipped still closes, after the tick before it.
        // Method: ask whether tick 9 closes a window, with tick 11 following it.
        let step = Step {
            tick: 9,
            following: 11,
        };
        assert!(step.closes_span(TICKS_PER_WINDOW));
    }

    #[test]
    fn closes_a_minute_after_its_sixtieth_tick() {
        // Goal: a minute closes after its sixtieth tick.
        // Method: ask whether tick 60 closes a minute, with tick 61 following it.
        let step = Step {
            tick: 60,
            following: 61,
        };
        assert!(step.closes_span(TICKS_PER_MINUTE));
    }

    #[test]
    #[should_panic(expected = "self.tick > 0")]
    fn refuses_the_warm_up_tick() {
        // Goal: the warm-up tick, which `Run::start` runs apart, never reaches a span.
        // Method: ask whether tick 0 closes a window.
        let step = Step {
            tick: 0,
            following: 1,
        };
        step.closes_span(TICKS_PER_WINDOW);
    }
}
