//! What a recording holds on disk after many retention periods, through the worker's own passes.

use std::collections::HashSet;

use super::super::super::{Pass, Recovery, run_pass, run_registry};
use super::*;

const MINUTE_NS: u64 = 60_000_000_000;
const PERIOD_MINUTES: u64 = 2;
const GRACE_MINUTES: u64 = 1;
/// The minutes before the oldest reading has outlived the period and the grace, its window has
/// waited out its successor, and a pass has taken it.
const SETTLED_MINUTES: usize = 6;
/// Beyond the period: the grace, the window waiting for its successor's first reading to expire,
/// the one the pass has not reached yet, and the frontier a registered run holds.
const WINDOWS_ABOVE_PERIOD_MAX: u64 = GRACE_MINUTES + 3;
const HOLDING_DIRECTORIES_MAX: usize = 1 << 16;
const PASSES_PER_MINUTE_MAX: usize = 64;
const RUN_MINUTES: u64 = 3;
const RUN_ENDS: u64 = 5;
const CYCLES: u64 = 3;
/// The samples of one cycle of run ends, one per minute of each of them.
const CYCLE_MINUTES: usize = 15;
/// Where every boot's own clock starts, past this test process's start, so the process reads as
/// the collector that published.
const CONTINUOUS_STARTED_NS: u64 = LIVE_MINUTE * MINUTE_NS;
/// Where the retention clock starts. It is far past the real wall clock, so a publication that asks
/// the clock from this test's real boot adds nothing to it.
const CLOCK_STARTED_NS: u64 = 1 << 62;
/// Every boot's offset, at the retention clock's start.
const OFFSET_STARTED_NS: u64 = CLOCK_STARTED_NS - CONTINUOUS_STARTED_NS;
/// An entry's hour directory may appear in a later cycle and in no earlier one.
const HOURS_CROSSED_MAX: u64 = 1;

/// How a test's recording runs.
struct PlateauPlan {
    period_minutes: u64,
    readings_per_minute: u64,
    slot_count: u64,
    /// How often to read the whole root, which costs more the more the recording holds.
    holding_minutes: u64,
}

impl PlateauPlan {
    const SHORT: Self = Self {
        period_minutes: PERIOD_MINUTES,
        readings_per_minute: 1,
        slot_count: 8,
        holding_minutes: 1,
    };
}

/// What a recording holds at one moment. Every saved name is a hard link to its proof, so names
/// and files differ.
#[derive(Clone, Copy, Debug, Default)]
struct Holding {
    names: u64,
    files: u64,
    directories: u64,
    bytes: u64,
    windows: u64,
}

impl Holding {
    fn of(root: &Path) -> Self {
        let mut holding = Self {
            windows: u64::try_from(std::fs::read_dir(root.join("data")).unwrap().count()).unwrap(),
            ..Self::default()
        };
        let mut files = HashSet::new();
        let mut directories = vec![root.to_path_buf()];
        for _directory in 0..HOLDING_DIRECTORIES_MAX {
            let Some(directory) = directories.pop() else {
                return holding;
            };
            holding.directories += 1;
            for entry in std::fs::read_dir(directory).unwrap() {
                let entry = entry.unwrap();
                let metadata = entry.metadata().unwrap();
                if metadata.is_dir() {
                    directories.push(entry.path());
                } else {
                    holding.names += 1;
                    if files.insert(metadata.ino()) {
                        holding.files += 1;
                        holding.bytes += metadata.blocks() * 512;
                    }
                }
            }
        }

        panic!("a recording holds more directories than a test can walk");
    }

    fn peak(holdings: &[Self]) -> Self {
        holdings.iter().fold(Self::default(), |peak, holding| Self {
            names: peak.names.max(holding.names),
            files: peak.files.max(holding.files),
            directories: peak.directories.max(holding.directories),
            bytes: peak.bytes.max(holding.bytes),
            windows: peak.windows.max(holding.windows),
        })
    }
}

/// How a run ends before the next one starts.
#[derive(Clone, Copy)]
enum RunEnd {
    Stop,
    Crash(Interruption),
}

impl RunEnd {
    fn of_run(number: u64) -> Self {
        match number % RUN_ENDS {
            0 => Self::Stop,
            1 => Self::Crash(Interruption::Registered),
            2 => Self::Crash(Interruption::EventsStaged),
            3 => Self::Crash(Interruption::TablesWritten),
            _ => Self::Crash(Interruption::Proved),
        }
    }
}

/// A worker over one recording, a minute at a time, on a clock the test gives its removal passes.
struct Plateau {
    plan: PlateauPlan,
    recording: Recording,
    recovery: Recovery,
    registry: Registry,
    holdings: Vec<Holding>,
    minutes: u64,
    windows_removed: u64,
    removal: Duration,
    removal_max: Duration,
}

impl Plateau {
    fn new(plan: PlateauPlan) -> Self {
        let shape = Shape {
            slot_count: plan.slot_count,
            ..shape()
        };
        let recording = Recording::create(false, shape);
        let root = PrivateDirectory::open(&recording.root).unwrap();
        Period::from_seconds(plan.period_minutes * 60)
            .unwrap()
            .write(&root)
            .unwrap();
        boot_offset_save(&root, recording.run.boot, OFFSET_STARTED_NS).unwrap();
        let started = ClockReading {
            boot: recording.run.boot,
            continuous_ns: CONTINUOUS_STARTED_NS,
            wall_ns: CLOCK_STARTED_NS,
        };
        Clock::started_at(&started).write(&root).unwrap();
        let mut plateau = Self {
            plan,
            recording,
            recovery: Self::recovery(),
            registry: Registry::default(),
            holdings: Vec::new(),
            minutes: 0,
            windows_removed: 0,
            removal: Duration::ZERO,
            removal_max: Duration::ZERO,
        };
        plateau.register();

        plateau
    }

    fn recovery() -> Recovery {
        Recovery {
            memory_limit: crate::resources::WORKING_BYTES_MAX,
            ..Recovery::default()
        }
    }

    fn register(&mut self) {
        let root = &self.recording.root;
        let run = RegisteredRun {
            identity: self.recording.run,
            ring_name: self.recording.ring_name().to_owned(),
            collector_pid: std::process::id(),
        };
        self.registry.add(root, run).unwrap();
        self.registry.write(root).unwrap();
    }

    /// A reading, every save it makes ready, and a removal pass on the clock of this minute.
    fn minute(&mut self, store: &Store, minute_of_boot: u64, wall_ns: u64) {
        for _reading in 0..self.plan.readings_per_minute {
            self.recording
                .publish_in_minute(LIVE_MINUTE + minute_of_boot);
        }
        self.saves(store);
        let now = ClockReading {
            boot: self.recording.run.boot,
            continuous_ns: CONTINUOUS_STARTED_NS + minute_of_boot * MINUTE_NS,
            wall_ns,
        };
        let runs: Vec<_> = self.registry.runs.iter().map(|run| run.identity).collect();
        let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
        let started = Instant::now();
        self.windows_removed += store.retire_expired(&now, &runs, &budget).unwrap();
        let removal = started.elapsed();
        self.removal += removal;
        self.removal_max = self.removal_max.max(removal);
        self.minutes += 1;
        if self.minutes.is_multiple_of(self.plan.holding_minutes) {
            self.holdings.push(Holding::of(&self.recording.root));
        }
    }

    fn saves(&mut self, store: &Store) {
        let root = self.recording.root.clone();
        let mut buffer = [0; 4097];
        let derive = SealedApps::default();
        let writers = Writers {
            encoder: Encoder,
            derive: &derive,
        };
        let runs = run_registry(&root.join("runs"), &mut buffer, &mut self.recovery).unwrap();
        for _pass in 0..PASSES_PER_MINUTE_MAX {
            let pass = run_pass(&root, store, &writers, &runs, &mut self.recovery).unwrap();
            if !matches!(pass, Pass::Sealed) {
                return;
            }
        }

        panic!("the worker went on finding minutes to save");
    }

    /// Ends the run the way `end` says, having saved all but the minute that was under way.
    fn end_run(&mut self, store: &Store, end: RunEnd) {
        match end {
            RunEnd::Stop => {
                self.recording.writer.finish();
                self.saves(store);
            }
            RunEnd::Crash(interruption) => {
                self.recording.publish_in_minute(LIVE_MINUTE + RUN_MINUTES);
                let attempted = InterruptedWindow {
                    number: LIVE_MINUTE + RUN_MINUTES - 1,
                    publication: RUN_MINUTES,
                    publications_lost: 0,
                    unknown: false,
                };
                self.recording
                    .interrupted_seal_of(store, &attempted, interruption);
                self.recording.lose_source();
            }
        }
    }

    /// Goes on in a new run on a new boot of the Mac, whose readings start `offset_ns` along the
    /// retention clock, as the first save of that boot would place them. A fresh worker follows,
    /// since a reboot ends the old one.
    fn reboot(&mut self, number: u64, offset_ns: u64) {
        let run = RunIdentity {
            recording: self.recording.run.recording,
            run: Identity([28, number]),
            boot: Identity([30, number]),
        };
        let place = Place {
            lock_path: self.recording.base.join(format!("plateau-{number}.lock")),
            name_prefix: format!(
                "/fdp{:x}{:x}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ),
        };
        let root = PrivateDirectory::open(&self.recording.root).unwrap();
        boot_offset_save(&root, run.boot, OFFSET_STARTED_NS + offset_ns).unwrap();
        self.recording.writer = Writer::create_recording(&place, shape(), &run, 1).unwrap();
        self.recording.run = run;
        self.recording.tick = 0;
        self.recording.source_lost = false;
        self.recovery = Self::recovery();
        self.register();
    }

    fn report(&self, label: &str) {
        let peak = Holding::peak(&self.holdings);
        println!(
            "plateau {label} minutes={} peak={peak:?} last={:?} windows_removed={} removal={:?} \
             removal_max={:?}",
            self.minutes,
            self.holdings.last().unwrap(),
            self.windows_removed,
            self.removal,
            self.removal_max,
        );
    }
}

impl Drop for Plateau {
    /// The rings of the earlier runs the registry still names. `Recording` removes its own.
    fn drop(&mut self) {
        for run in &self.registry.runs {
            if run.identity != self.recording.run {
                match collector::ring::unlink_generation(&run.ring_name) {
                    Ok(()) => {}
                    Err(error) if error.kind() == ErrorKind::NotFound => {}
                    Err(error) => panic!("removing a ring: {error}"),
                }
            }
        }
    }
}

/// Asserts that the last `cycle` samples rise no higher than the `cycle` after sample `settled`. A
/// recording that grew by even one name per cycle would break this.
fn plateau_assert(plateau: &Plateau, settled: usize, cycle: usize) {
    let holdings = &plateau.holdings;
    let period_minutes = plateau.plan.period_minutes;
    let earlier = Holding::peak(&holdings[settled..settled + cycle]);
    let later = Holding::peak(&holdings[holdings.len() - cycle..]);

    assert!(later.names <= earlier.names, "{later:?} {earlier:?}");
    assert!(later.files <= earlier.files, "{later:?} {earlier:?}");
    assert!(
        later.directories <= earlier.directories + HOURS_CROSSED_MAX,
        "{later:?} {earlier:?}"
    );
    assert!(later.bytes <= earlier.bytes, "{later:?} {earlier:?}");
    assert!(earlier.windows > period_minutes, "{earlier:?}");
    assert!(
        later.windows <= period_minutes + WINDOWS_ABOVE_PERIOD_MAX,
        "{later:?}"
    );
}

#[test]
fn a_recording_many_periods_long_holds_no_more_than_one_period_does() {
    // Goal: once its oldest reading has outlived the period, a recording loses a minute for each
    // one it saves, in names, files, directories and bytes.
    // Method: save nine periods a minute at a time, with a removal pass each minute, and compare
    // the peak of the last period with the peak of the first after it settled.
    let mut plateau = Plateau::new(PlateauPlan::SHORT);
    let store = Store::open(&plateau.recording.root).unwrap();

    for minute in 0..PERIOD_MINUTES * 9 {
        plateau.minute(&store, minute, CLOCK_STARTED_NS + minute * MINUTE_NS);
    }

    plateau.report("one-run");
    plateau_assert(&plateau, SETTLED_MINUTES, 2);
}

#[test]
fn restarts_crashes_and_a_stopped_wall_clock_leave_a_recording_no_larger() {
    // Goal: what runs leave behind is bounded too: a retirement receipt for each one that stopped,
    // a terminal record, a boot record for each boot, and the pending and ready files of a seal a
    // crash interrupted. A wall clock that stands still cannot keep any of them, nor any reading.
    // Method: a new run on a new boot every few minutes on a wall clock that stands still, so a
    // reboot adds no time, each run ending by a stop or by a crash at the next stage of a seal with
    // its source lost, and compare the peaks of a whole cycle of those ends.
    let mut plateau = Plateau::new(PlateauPlan::SHORT);
    let mut store = Store::open(&plateau.recording.root).unwrap();
    let mut offset_ns = 0;

    for number in 0..RUN_ENDS * CYCLES {
        for minute in 0..RUN_MINUTES {
            plateau.minute(&store, minute, CLOCK_STARTED_NS);
        }
        plateau.end_run(&store, RunEnd::of_run(number));
        drop(store);
        offset_ns += (RUN_MINUTES - 1) * MINUTE_NS;
        plateau.reboot(number, offset_ns);
        store = Store::open(&plateau.recording.root).unwrap();
    }

    plateau.report("restarts");
    plateau_assert(&plateau, SETTLED_MINUTES, CYCLE_MINUTES);
}

/// Runs too long for the checks. `--features long-recording -- plateau_long --nocapture` reports
/// the holding each hour of them.
#[cfg(feature = "long-recording")]
mod plateau_long {
    use super::*;

    const HOUR_MINUTES: u64 = 60;
    const DAY_MINUTES: u64 = 24 * HOUR_MINUTES;

    /// Saves `days` at a period of a day, a reading of the root each hour, and compares the peak of
    /// the second day with the peak of the last.
    fn plateau_long_days(label: &str, days: u64, readings_per_minute: u64, slot_count: u64) {
        let mut plateau = Plateau::new(PlateauPlan {
            period_minutes: DAY_MINUTES,
            readings_per_minute,
            slot_count,
            holding_minutes: HOUR_MINUTES,
        });
        let store = Store::open(&plateau.recording.root).unwrap();

        for minute in 0..days * DAY_MINUTES {
            plateau.minute(&store, minute, CLOCK_STARTED_NS + minute * MINUTE_NS);
            if (minute + 1).is_multiple_of(HOUR_MINUTES) {
                plateau.report(label);
            }
        }

        plateau_assert(&plateau, 25, 24);
    }

    #[test]
    fn thirty_days_at_the_default_period_hold_no_more_than_the_second_day_does() {
        // Goal: the whole lifetime the plan names, at the period a recording starts with, stays at
        // what one day holds.
        // Method: thirty days of minutes of one reading, so a run takes hours and not days.
        plateau_long_days("thirty-days", 30, 1, 8);
    }

    #[test]
    fn three_days_of_full_minutes_hold_no_more_than_the_second_day_does() {
        // Goal: the trees a real day of names needs shrink as they grew.
        // Method: three days of minutes of sixty readings over the production ring's size, which
        // holds as many names as the recording of a real day does.
        plateau_long_days("full-minutes", 3, 60, 120);
    }
}
