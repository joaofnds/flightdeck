//! Materializes the fixed reference input of P00's contract for W10, a generated Mac's hour,
//! into a recording of its own: the collector's ring writer publishes each second and the
//! persister's seal worker saves each minute, as a recording's collector and persister do. Every
//! reading is a function of the second and the process, so two generations read the same.
//!
//! The recording is left as a running collector leaves it, its last run live with the minute it
//! ends in still in its ring, which stays registered for readers until `--unlink` removes it.

use std::fs::DirBuilder;
use std::io::{self, Write};
use std::ops::{Range, RangeInclusive};
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread::JoinHandle;
use std::time::Duration;

use collector::battery::BatteryReading;
use collector::disks::{DISKS_MAX, DiskReading, DiskSnapshot, NAME_BYTES, ScanState};
use collector::gpu_memory::GpuMemoryReading;
use collector::interfaces::{IFNAMSIZ, Interface, Name};
use collector::ioreport::States;
use collector::machine::{CPUTicks, Memory, Pressure};
use collector::processes::{Process, Task, Usage};
use collector::recording::control::{Control, Failure, Role, State};
use collector::recording::private::PrivateDirectory;
use collector::recording::{FixedRun, Registration, Registry};
use collector::ring::{
    DictionaryCapacity, Dropped, IOReportReadings, Identity, IdentityObservation,
    IdentityPublication, ObservationTimes, Place, ProcessCreation, ProcessIdentity, ProgramBytes,
    ProgramReference, Readings, Shape, SmcReadings, SupplementalReadings, SystemPublication,
    UsageAvailability, Writer,
};
use collector::system::{FactText, MachineFacts, PerformanceLevel, SystemReading};
use collector::tracker::{Event, Noticed};
use persistence::retention::Period;

const USAGE: &str = "usage: reference_input --directory PATH --seconds N | reference_input \
    --expected-events --seconds N | reference_input --unlink PATH";
/// The longest input made, W10's hour, the shortest of the contract's durations.
const SECONDS_MAX: u64 = 3_600;
/// The shortest input whose saved minutes hold seconds 300 to 306, each special, since the
/// minute an input ends in stays in its ring.
const SECONDS_MIN: u64 = 370;
const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;
const SECONDS_PER_MINUTE: u64 = 60;
/// The contract repeats its incidents and special seconds every period.
const PERIOD_SECONDS: u64 = 600;
/// The second of each period that reads exactly zero, as `t mod 600 = 300` in the contract.
const ZERO_SECOND: u64 = 300;
/// The second of each period whose tick is missed, as `t mod 600 = 301` in the contract.
const MISSED_SECOND: u64 = 301;
/// The second of each period whose SMC is not read, as the contract's unavailable source at 302.
const SMC_UNAVAILABLE_SECOND: u64 = 302;
/// The seconds of each period at fair and at serious thermal states, between the nominal seconds
/// 303 and 306 of the contract's transitions.
const THERMAL_FAIR_SECOND: u64 = 304;
const THERMAL_SERIOUS_SECOND: u64 = 305;
/// The offsets of each period's incident seconds, one at each position of a ten-second bucket.
const INCIDENT_OFFSETS: [u64; 10] = [10, 21, 32, 43, 54, 65, 76, 87, 98, 109];
/// The process of the lowest mean-ranked App whose CPU time and energy rise in an incident.
const INCIDENT_PROCESS: u64 = 0;
/// How many times faster than usual the incident process spends CPU time and energy in one.
const INCIDENT_FOLD: u64 = 20;
/// The first process that takes new creations, so processes 0 and 1 never rotate and no
/// incident loses its process.
const ROTATING_FIRST: u64 = 2;
/// The processes that take a new creation each second, as the contract's two a second.
const ROTATIONS_PER_SECOND: u64 = 2;
/// The seconds from one rotation of a process to its next.
const ROTATION_ROUND_SECONDS: u64 = (PROCESSES - ROTATING_FIRST) / ROTATIONS_PER_SECOND;
/// The identity of the first creation a rotation starts, after the input's first creations.
const CREATION_ROTATED_FIRST: u64 = PROCESSES + 1;
/// The recording's identity. This and the other identities are arbitrary constants, the fixed
/// opaque identities the contract gives the recording, each run and the boot, so every
/// generation names them alike.
const RECORDING_IDENTITY: Identity = Identity([0x9e37_79b9_7f4a_7c15, 0xbf58_476d_1ce4_e5b9]);
/// The one boot session every run of the input reads in.
const BOOT_IDENTITY: Identity = Identity([0x94d0_49bb_1331_11eb, 0x2545_f491_4f6c_dd1d]);
/// The first primary run's identity.
const RUN_FIRST_IDENTITY: Identity = Identity([0xd6e8_feb8_6659_fd93, 0xa076_1d64_78bd_642f]);
/// The second primary run's identity.
const RUN_SECOND_IDENTITY: Identity = Identity([0xe703_7ed1_a0b4_28db, 0x8ebc_6af0_9c88_c6e3]);
/// The three-observation run's identity.
const RUN_OBSERVATIONS_IDENTITY: Identity =
    Identity([0x2d35_8dcc_aa6c_78a5, 0x8bb8_4b93_962e_acc9]);
/// The wall seconds the three-observation run reads a second time, the contract's 120 to 122.
const OBSERVATIONS_SECONDS: Range<u64> = 120..123;
/// How much later on the continuous and awake clocks the three-observation run reads each of
/// its seconds than the first primary run, so the two observations of a second differ.
const OBSERVATIONS_LATER_NS: u64 = NANOSECONDS_PER_SECOND / 2;
/// The three-observation run.
const OBSERVATIONS_RUN: RunPlan = RunPlan {
    identity: RUN_OBSERVATIONS_IDENTITY,
    first: OBSERVATIONS_SECONDS.start,
    later_ns: OBSERVATIONS_LATER_NS,
};
/// The first primary run, which starts at the input's first second.
const FIRST_RUN: RunPlan = RunPlan {
    identity: RUN_FIRST_IDENTITY,
    first: 0,
    later_ns: 0,
};
/// The contract's origin, 1,800,000,000 seconds after the epoch.
const ORIGIN_WALL_NS: u64 = 1_800_000_000 * NANOSECONDS_PER_SECOND;
/// The continuous and awake clocks at the first second, past the uptime of any Mac, since a
/// persister takes a run whose newest awake time precedes its writer's start for ended.
const ORIGIN_CONTINUOUS_NS: u64 = 100_000_000 * NANOSECONDS_PER_SECOND;
/// Long enough for a range test, as the contract asks.
const RETENTION_SECONDS: u64 = 31 * 86_400;
/// The publications the collector's ring holds, `RING_SLOTS` in `collector/src/main.rs`.
const RING_SLOTS: u64 = 120;
/// The events the collector's ring holds, `RING_EVENTS` in `collector/src/main.rs`.
const RING_EVENTS: u64 = 655_360;
const PROCESSES: u64 = 256;
const APPS: u64 = 32;
const PID_FIRST: i32 = 41_000;
const CPUS: u64 = 18;
const CPU_BASE_NS: u64 = 1 << 53;
const PROGRAM_NAME_BYTES: usize = 32;
const PROGRAM_PATH_BYTES: usize = 96;
/// The process whose program name is the contract's one non-UTF-8 identity.
const NON_UTF8_PROCESS: u64 = PROCESSES - 1;
/// A byte that neither starts nor continues a UTF-8 character.
const NON_UTF8_BYTE: u8 = 0xFF;
/// macOS counts a hundred CPU ticks a second.
const NANOSECONDS_PER_TICK: u64 = 10_000_000;
const INTERFACES: u16 = 4;
const DISKS: usize = 4;
const DISK_DRIVER_ID_FIRST: u64 = 0x1_0000;
const DISK_MEDIA_ID_FIRST: u64 = 0x2_0000;
/// The GPU's active states and each core's, as many as the ring has room for.
const GPU_STATES: usize = 2;
const CORE_STATES: usize = 2;
/// The contract's baseline GPU, 10% active, and its peak, 90%, half of each in each state.
const GPU_STATE_ACTIVE_NS_PER_SECOND: u64 = 50_000_000;
const GPU_STATE_ACTIVE_NS_PER_INCIDENT_SECOND: u64 = 450_000_000;
const GPU_ENERGY_NJ_PER_SECOND: u64 = 500_000_000;
const GIB: u64 = 1 << 30;
const MEMORY_TOTAL_BYTES: u64 = 24 * GIB;
/// The contract's baseline gauges.
const GAUGES_BASELINE: Gauges = Gauges {
    power_watts: 5.0,
    fan_rpm: [1_200.0, 1_300.0],
    temperatures_celsius: [35.0, 36.0, 37.0],
    memory_used_bytes: 8 * GIB,
};
/// The contract's peak gauges, which each incident second reads.
const GAUGES_PEAK: Gauges = Gauges {
    power_watts: 40.0,
    fan_rpm: [4_000.0, 4_100.0],
    temperatures_celsius: [80.0, 81.0, 82.0],
    memory_used_bytes: 12 * GIB,
};
/// The gauges of each period's zero second.
const GAUGES_ZERO: Gauges = Gauges {
    power_watts: 0.0,
    fan_rpm: [0.0; 2],
    temperatures_celsius: [0.0; 3],
    memory_used_bytes: 0,
};
const LOAD_AVERAGE: [f64; 3] = [2.0, 1.5, 1.0];
/// The thermal pressure levels macOS publishes as nominal, moderate and heavy, which the charts
/// name Nominal, Fair and Serious.
const THERMAL_NOMINAL: u8 = 0;
const THERMAL_MODERATE: u8 = 1;
const THERMAL_HEAVY: u8 = 2;
/// How long the generator waits for the worker to save a minute before it gives up.
const SEAL_WAIT_MAX: Duration = Duration::from_mins(2);
const SEAL_POLL: Duration = Duration::from_millis(10);

const _: () = {
    assert!(SECONDS_MIN <= SECONDS_MAX);
    assert!(PROCESSES.is_multiple_of(APPS));
    // Each period holds its zero second and its missed tick as two seconds of its own.
    assert!(ZERO_SECOND < PERIOD_SECONDS);
    assert!(MISSED_SECOND < PERIOD_SECONDS);
    assert!(ZERO_SECOND != MISSED_SECOND);
    // The SMC is unavailable in a second of its own, whose counters advance.
    assert!(SMC_UNAVAILABLE_SECOND < PERIOD_SECONDS);
    assert!(SMC_UNAVAILABLE_SECOND != ZERO_SECOND);
    assert!(SMC_UNAVAILABLE_SECOND != MISSED_SECOND);
    // The thermal transitions follow the other special seconds, each a second of its own.
    assert!(SMC_UNAVAILABLE_SECOND + 1 < THERMAL_FAIR_SECOND);
    assert!(THERMAL_FAIR_SECOND + 1 == THERMAL_SERIOUS_SECOND);
    assert!(THERMAL_SERIOUS_SECOND + 1 < PERIOD_SECONDS);
    // The incidents take the ten positions of a ten-second bucket in order, each a valid second
    // before the other special seconds.
    let mut incident = 0;
    let mut position: u64 = 0;
    while incident < INCIDENT_OFFSETS.len() {
        assert!(INCIDENT_OFFSETS[incident] % 10 == position);
        if 0 < incident {
            assert!(INCIDENT_OFFSETS[incident - 1] < INCIDENT_OFFSETS[incident]);
        }
        incident += 1;
        position += 1;
    }
    assert!(INCIDENT_OFFSETS[INCIDENT_OFFSETS.len() - 1] < ZERO_SECOND);
    assert!(INCIDENT_PROCESS < ROTATING_FIRST);
    // Each primary run holds a saved minute at least.
    assert!(0 < run_split(SECONDS_MIN));
    // The three-observation run reads seconds of the first primary run, none of them special.
    assert!(OBSERVATIONS_SECONDS.end <= run_split(SECONDS_MIN));
    assert!(INCIDENT_OFFSETS[INCIDENT_OFFSETS.len() - 1] < OBSERVATIONS_SECONDS.start);
    // An observation stays within its own second.
    assert!(OBSERVATIONS_LATER_NS < NANOSECONDS_PER_SECOND);
    // The non-UTF-8 name rotates, so its exits and starts name it too.
    assert!(ROTATING_FIRST <= NON_UTF8_PROCESS);
    assert!(NON_UTF8_PROCESS < PROCESSES);
    // The rotating processes take their turns in whole pairs.
    assert!((PROCESSES - ROTATING_FIRST).is_multiple_of(ROTATIONS_PER_SECOND));
    // A round outlasts the missed tick, so the tick after it carries each process's exit once.
    assert!(1 < ROTATION_ROUND_SECONDS);
    // Each CPU idles for what it does not spend, so no CPU's idle ticks fall below zero.
    assert!(CPUS < 85);
    // A third of the cores are super cores.
    assert!(CPUS.is_multiple_of(3));
    assert!(DISKS <= DISKS_MAX);
    // The GPU's states add up to the contract's 10%.
    assert!(2 * GPU_STATE_ACTIVE_NS_PER_SECOND == NANOSECONDS_PER_SECOND / 10);
    assert!(2 * GPU_STATE_ACTIVE_NS_PER_INCIDENT_SECOND == 9 * NANOSECONDS_PER_SECOND / 10);
    // The memory chart's parts of the machine memory are exact.
    assert!(GAUGES_BASELINE.memory_used_bytes.is_multiple_of(8));
    assert!(GAUGES_BASELINE.memory_used_bytes < MEMORY_TOTAL_BYTES);
    assert!(GAUGES_PEAK.memory_used_bytes.is_multiple_of(8));
    assert!(GAUGES_PEAK.memory_used_bytes < MEMORY_TOTAL_BYTES);
};

fn main() -> ExitCode {
    let command =
        match collector::arguments("reference_input").and_then(|words| command_from(&words)) {
            Ok(command) => command,
            Err(usage) => {
                eprintln!("{usage}");
                return ExitCode::from(2);
            }
        };

    let done = match &command {
        Command::Generate { directory, seconds } => generate(directory, *seconds),
        Command::ExpectedEvents { seconds } => print_expected_events(*seconds),
        Command::Unlink { directory } => unlink(directory),
    };
    match done {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("reference_input: {error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, PartialEq)]
enum Command {
    Generate { directory: PathBuf, seconds: u64 },
    ExpectedEvents { seconds: u64 },
    Unlink { directory: PathBuf },
}

fn command_from(words: &[String]) -> Result<Command, String> {
    match words {
        [unlink, directory] if unlink == "--unlink" => Ok(Command::Unlink {
            directory: PathBuf::from(directory),
        }),
        [expected, flag, value] if expected == "--expected-events" && flag == "--seconds" => {
            Ok(Command::ExpectedEvents {
                seconds: command_from_seconds(value)?,
            })
        }
        [first, first_value, second, second_value] => {
            let mut directory = None;
            let mut seconds = None;
            for (flag, value) in [(first, first_value), (second, second_value)] {
                match flag.as_str() {
                    "--directory" => directory = Some(PathBuf::from(value)),
                    "--seconds" => seconds = Some(command_from_seconds(value)?),
                    _ => return Err(USAGE.to_owned()),
                }
            }
            match (directory, seconds) {
                (Some(directory), Some(seconds)) => Ok(Command::Generate { directory, seconds }),
                _ => Err(USAGE.to_owned()),
            }
        }
        _ => Err(USAGE.to_owned()),
    }
}

fn command_from_seconds(value: &str) -> Result<u64, String> {
    match value.parse::<u64>() {
        Ok(seconds) if (SECONDS_MIN..=SECONDS_MAX).contains(&seconds) => Ok(seconds),
        _ => Err(format!(
            "{USAGE}\n{value} is not {SECONDS_MIN} to {SECONDS_MAX} seconds"
        )),
    }
}

/// Generates the input into `directory`, which must not exist, so no recording, the user's
/// included, ever has readings added to it.
fn generate(directory: &Path, seconds: u64) -> Result<(), Box<dyn std::error::Error>> {
    persistence::engine::initialize_process()?;
    DirBuilder::new().mode(0o700).create(directory)?;
    // The recording takes its fixed identity before the store opens it, since the first to set a
    // recording up names it.
    collector::recording::initialize_as(directory, RECORDING_IDENTITY)?;
    drop(persistence::Store::open(directory)?);
    let period = Period::from_seconds(RETENTION_SECONDS).ok_or("retention period")?;
    period.write(&PrivateDirectory::open(directory)?)?;
    // The worker reports each pass here, as a recorder's persister does.
    let control = Control::create(directory)?;

    let place = Place {
        lock_path: directory.join("ring.lock"),
        name_prefix: format!("/fdri{}", std::process::id()),
    };
    let [observations, first, (plan, last_seconds)] = runs(seconds);

    // Each run but the last ends as a collector that stops in order ends it, before the next
    // registers.
    for (plan, run_seconds) in [observations, first] {
        let mut run = Run::start(directory, &place, &control, &plan)?;
        run.publish(run_seconds)?;
        run.finish()?;
    }

    let mut second = Run::start(directory, &place, &control, &plan)?;
    let last_minute_first = second.publish(last_seconds)?;
    second.paced.wait_for_saved(last_minute_first)?;
    // A pass that finds nothing to save ends in the worker's pause, so the process ends with the
    // worker asleep rather than part way through a save.
    second.paced.wait_for_pass()?;

    println!(
        "reference_input directory={} seconds={seconds} split={} ring={} saved_end={} \
        publication_newest={}",
        directory.display(),
        plan.first,
        second.writer.name(),
        second.paced.saved_end,
        second.writer.publication_newest(),
    );
    // The process ends with the second run live and the minute it ends in in its ring, as a
    // recording stands while its collector and persister run.
    Ok(())
}

/// Prints each exit and start the input of `seconds` publishes, by the identity of the run that
/// publishes it, the second whose tick publishes it, `exit` or `start`, the pid and the creation
/// it names, as lines of tab-separated fields under a header.
fn print_expected_events(seconds: u64) -> Result<(), Box<dyn std::error::Error>> {
    let mut out = io::BufWriter::new(io::stdout().lock());
    writeln!(out, "run\tsecond\tevent\tpid\tcreation")?;
    for (run, run_seconds) in runs(seconds) {
        for second in ticked(run_seconds) {
            print_expected_events_tick(&mut out, &run, second)?;
        }
    }
    out.flush()?;

    Ok(())
}

/// Prints the exits and starts the tick of `second` in the run `run` publishes.
fn print_expected_events_tick(
    out: &mut impl Write,
    run: &RunPlan,
    second: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let run_text = collector::recording::identity_text(run.identity);
    for rotation in rotations(rotated_seconds(run, second)) {
        let pid = PID_FIRST + i32::try_from(rotation.process)?;
        writeln!(
            out,
            "{run_text}\t{second}\texit\t{pid}\t{}",
            rotation.ended.identity
        )?;
        writeln!(
            out,
            "{run_text}\t{second}\tstart\t{pid}\t{}",
            rotation.started.identity
        )?;
    }

    Ok(())
}

/// The input's runs for `seconds`, each with the seconds it reads, in the order the generator
/// publishes them. The three-observation run goes first, so the second primary run's
/// registration evicts it rather than the first primary run from the registry of two, which then
/// lists the primary runs.
fn runs(seconds: u64) -> [(RunPlan, Range<u64>); 3] {
    let split = run_split(seconds);

    [
        (OBSERVATIONS_RUN, OBSERVATIONS_SECONDS),
        (FIRST_RUN, 0..split),
        (
            RunPlan {
                identity: RUN_SECOND_IDENTITY,
                first: split,
                later_ns: 0,
            },
            split..seconds,
        ),
    ]
}

/// The seconds of `seconds` whose tick is not missed, since a missed tick publishes nothing.
fn ticked(seconds: Range<u64>) -> impl Iterator<Item = u64> {
    seconds.filter(|second| second % PERIOD_SECONDS != MISSED_SECOND)
}

/// The seconds whose rotations the tick of `second` in the run `run` publishes. The tick after a
/// missed tick carries the missed second's rotations too, as a collector publishes every event
/// since its last tick. A run's first tick carries none, since a collector's first scan finds
/// its processes already running.
fn rotated_seconds(run: &RunPlan, second: u64) -> RangeInclusive<u64> {
    let rotated_first = if second % PERIOD_SECONDS == MISSED_SECOND + 1 {
        second - 1
    } else {
        second
    };

    rotated_first.max(run.first + 1)..=second
}

/// The second the second primary run starts at: half the input's `seconds`, the divisions
/// flooring it to the minute boundary before, so no minute spans the two runs.
const fn run_split(seconds: u64) -> u64 {
    seconds / 2 / SECONDS_PER_MINUTE * SECONDS_PER_MINUTE
}

/// Which run the generator publishes and when it reads the input's seconds.
#[derive(Clone, Copy)]
struct RunPlan {
    identity: Identity,
    /// The input's second the run starts at, its tick 0.
    first: u64,
    /// How much later than the input's continuous and awake clocks the run reads each second.
    later_ns: u64,
}

/// One run as the generator publishes it: its writer, the generated Mac its ring names, and its
/// worker's progress.
struct Run<'a> {
    writer: Writer,
    machine: Machine,
    paced: Paced<'a>,
}

impl<'a> Run<'a> {
    /// Registers the run `plan` names of the recording `directory` at the ring place `place`,
    /// and starts its worker, which reports to `control`.
    fn start(
        directory: &Path,
        place: &Place,
        control: &'a Control,
        plan: &RunPlan,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let started = Registration::start_with_system_as(
            directory,
            place,
            RUN_SHAPE,
            DictionaryCapacity {
                entries: 65_536,
                arena_bytes: 64 * 1024 * 1024,
            },
            ORIGIN_CONTINUOUS_NS + plan.first * NANOSECONDS_PER_SECOND + plan.later_ns,
            FixedRun {
                run: plan.identity,
                boot: BOOT_IDENTITY,
            },
        )?;
        let (mut writer, registered) = started.into_parts();
        registered?;
        // The worker starts once the run is registered, since a worker that finds no run exits.
        let worker = generate_worker(directory, control.authority())?;
        let machine = Machine::start(&mut writer, plan)?;
        let saved_end = writer.publication_newest() + 1;

        Ok(Self {
            writer,
            machine,
            paced: Paced {
                control,
                run: plan.identity,
                worker,
                saved_end,
                heartbeat_ns: 0,
            },
        })
    }

    /// Publishes the input's `seconds`, waiting on the worker wherever the ring would otherwise
    /// overwrite a publication it has yet to save, and returns the first publication of the
    /// minute they end in.
    fn publish(&mut self, seconds: Range<u64>) -> Result<u64, Box<dyn std::error::Error>> {
        let mut last_minute_first = None;
        for second in ticked(seconds.clone()) {
            let publication = self.writer.publication_newest() + 1;
            self.paced.wait_for_room(publication)?;
            // Both divisions floor, to the minute a second falls in.
            if second / SECONDS_PER_MINUTE == (seconds.end - 1) / SECONDS_PER_MINUTE {
                last_minute_first.get_or_insert(publication);
            }
            self.machine.publish(&mut self.writer, second)?;
        }

        last_minute_first.ok_or_else(|| "no last minute".into())
    }

    /// Ends the run as a collector that stops in order ends it, and waits for its worker to save
    /// and retire it and exit, which the worker does once every registered run is retired.
    fn finish(mut self) -> Result<(), Box<dyn std::error::Error>> {
        self.writer.finish();
        self.paced.wait_for_exit()?;

        match self.paced.worker.join() {
            Ok(Ok(persistence::worker::Outcome::Complete)) => Ok(()),
            Ok(Ok(persistence::worker::Outcome::Owned)) => {
                Err("another worker owned the recording".into())
            }
            Ok(Err(error)) => Err(error.into()),
            Err(_) => Err("the seal worker panicked".into()),
        }
    }
}

/// Seals the recording on a thread of its own, as the persister does in a process of its own,
/// under the recorder's authority `authority`.
fn generate_worker(
    directory: &Path,
    authority: Identity,
) -> io::Result<JoinHandle<persistence::Result<persistence::worker::Outcome>>> {
    let root = directory.to_path_buf();
    std::thread::Builder::new()
        .name("seal worker".to_owned())
        .spawn(move || {
            let derive = persistence::sealed_apps::SealedApps::default();
            persistence::worker::run_managed(
                &root,
                persistence::worker::default_memory_limit(),
                &derive,
                Some(authority),
            )
        })
}

/// Unlinks the rings of the runs `directory` registers, which outlive the generator otherwise.
/// It refuses a recording the generator did not make, whose rings may be a collector's.
fn unlink(directory: &Path) -> Result<(), Box<dyn std::error::Error>> {
    // A path that names no directory refuses here, rather than reading as a recording of no run.
    drop(PrivateDirectory::open(directory)?);
    if collector::recording::read_identity(directory)? != RECORDING_IDENTITY {
        return Err("refusing a recording the generator did not make".into());
    }
    let registry = Registry::read(directory)?;
    for run in &registry.runs {
        match collector::ring::unlink_generation(&run.ring_name) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }

    Ok(())
}

/// The room a run of the generated Mac takes: the collector's ring, with records for what the
/// generated Mac has.
const RUN_SHAPE: Shape = Shape {
    slot_count: RING_SLOTS,
    cpu_capacity: CPUS,
    interface_capacity: 4,
    core_capacity: CPUS,
    state_capacity: 2,
    fan_capacity: 2,
    process_capacity: PROCESSES,
    coalition_capacity: 1,
    event_capacity: RING_EVENTS,
};

/// What the generator knows of the worker's progress on the run it publishes, from the reports
/// the worker makes after each pass.
struct Paced<'a> {
    control: &'a Control,
    run: Identity,
    worker: JoinHandle<persistence::Result<persistence::worker::Outcome>>,
    /// The publication after the newest the worker has saved of the run.
    saved_end: u64,
    /// When the worker's newest report was made.
    heartbeat_ns: u64,
}

impl Paced<'_> {
    /// Waits until publishing `publication` overwrites no publication the worker has yet to
    /// save, since a ring overrun would lose readings the input holds.
    fn wait_for_room(&mut self, publication: u64) -> Result<(), Box<dyn std::error::Error>> {
        if publication < self.saved_end + RING_SLOTS {
            return Ok(());
        }

        self.wait_for_saved(publication - RING_SLOTS + 1)
    }

    /// Waits until the worker has saved every publication of the run before `end`.
    fn wait_for_saved(&mut self, end: u64) -> Result<(), Box<dyn std::error::Error>> {
        for _poll in 0..SEAL_WAIT_MAX.as_millis() / SEAL_POLL.as_millis() {
            self.read()?;
            if end <= self.saved_end {
                return Ok(());
            }
            std::thread::sleep(SEAL_POLL);
        }

        Err(format!("the worker saved no minute in {SEAL_WAIT_MAX:?}").into())
    }

    /// Waits for the worker to exit.
    fn wait_for_exit(&self) -> Result<(), Box<dyn std::error::Error>> {
        for _poll in 0..SEAL_WAIT_MAX.as_millis() / SEAL_POLL.as_millis() {
            if self.worker.is_finished() {
                return Ok(());
            }
            std::thread::sleep(SEAL_POLL);
        }

        Err(format!("the worker did not finish the run in {SEAL_WAIT_MAX:?}").into())
    }

    /// Waits for the worker's next report.
    fn wait_for_pass(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        let heartbeat_ns = self.heartbeat_ns;
        for _poll in 0..SEAL_WAIT_MAX.as_millis() / SEAL_POLL.as_millis() {
            self.read()?;
            if heartbeat_ns < self.heartbeat_ns {
                return Ok(());
            }
            std::thread::sleep(SEAL_POLL);
        }

        Err(format!("the worker made no pass in {SEAL_WAIT_MAX:?}").into())
    }

    fn read(&mut self) -> Result<(), Box<dyn std::error::Error>> {
        if self.worker.is_finished() {
            return Err("the seal worker stopped".into());
        }

        let health = self.control.read(Role::Persister)?;
        if health.failure != Failure::None {
            return Err(format!("the seal worker failed: {health:?}").into());
        }

        assert!(self.heartbeat_ns <= health.heartbeat_ns);
        self.heartbeat_ns = health.heartbeat_ns;
        if health.state == State::Running && health.run == self.run {
            let saved_end = health.publication + 1;
            assert!(self.saved_end <= saved_end);
            self.saved_end = saved_end;
        }
        Ok(())
    }
}

/// The generated Mac: its processes in increasing pid order, the programs they run, and what
/// it is, which no second changes.
struct Machine {
    /// The run that reads the generated Mac.
    run: RunPlan,
    programs: Vec<ProgramReference>,
    facts: MachineFacts,
    /// Each process's GPU time, which the contract does not name, so it reads zero.
    process_gpu_ns: Vec<Option<u64>>,
}

impl Machine {
    /// The generated Mac as the run `run`, which publishes through `writer`, reads it.
    fn start(writer: &mut Writer, run: &RunPlan) -> Result<Self, Box<dyn std::error::Error>> {
        // The ring has room for each device the generated Mac has, and for no other.
        assert_eq!(RUN_SHAPE.interface_capacity, u64::from(INTERFACES));
        assert_eq!(
            RUN_SHAPE.fan_capacity,
            collector::u64_from_usize(GAUGES_BASELINE.fan_rpm.len())
        );
        assert_eq!(
            RUN_SHAPE.state_capacity,
            collector::u64_from_usize(GPU_STATES)
        );
        assert_eq!(
            RUN_SHAPE.state_capacity,
            collector::u64_from_usize(CORE_STATES)
        );

        let mut programs = Vec::new();
        for process in 0..PROCESSES {
            programs.push(start_program(writer, process)?);
        }

        Ok(Self {
            run: *run,
            programs,
            facts: start_facts()?,
            process_gpu_ns: vec![Some(0); usize::try_from(PROCESSES)?],
        })
    }

    fn publish(
        &mut self,
        writer: &mut Writer,
        second: u64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let times = ObservationTimes {
            wall_ns: ORIGIN_WALL_NS + second * NANOSECONDS_PER_SECOND,
            continuous_ns: ORIGIN_CONTINUOUS_NS
                + second * NANOSECONDS_PER_SECOND
                + self.run.later_ns,
            awake_ns: ORIGIN_CONTINUOUS_NS + second * NANOSECONDS_PER_SECOND + self.run.later_ns,
        };
        let read = Second::read(&self.programs, &self.run, second, &times)?;
        let smc = if second % PERIOD_SECONDS == SMC_UNAVAILABLE_SECOND {
            None
        } else {
            Some(SmcReadings {
                power_watts: read.gauges.power_watts,
                soc_celsius: read.gauges.temperatures_celsius[0],
                cpu_celsius: Some(read.gauges.temperatures_celsius[1]),
                gpu_celsius: Some(read.gauges.temperatures_celsius[2]),
                fan_rpm: &read.gauges.fan_rpm,
            })
        };
        let readings = Readings {
            // A collector counts its run's ticks from its warm-up tick, tick 0.
            run_tick: second - self.run.first,
            cpus: &read.cpus,
            memory: &read.memory,
            interfaces: &read.interfaces,
            ioreport: Some(IOReportReadings {
                gpu_energy_nj: read.gpu_energy_nj,
                gpu_active_ns: &read.gpu_active_ns,
                cores: &read.cores,
            }),
            smc,
            processes: &read.processes,
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &read.events,
            dropped: Dropped::default(),
        };
        let system = SystemPublication {
            reading: &read.system,
            machine: &self.facts,
            clock: None,
            process_gpu_ns: &self.process_gpu_ns,
        };
        writer.publish_with_disks(
            &readings,
            Some(&times),
            &IdentityPublication {
                processes: &read.identities,
                events: &read.event_identities,
            },
            &SupplementalReadings {
                battery: Some(&read.battery),
                disks: Some(&read.disks),
                system: Some(&system),
            },
        )?;

        Ok(())
    }
}

/// What the generated Mac reads in one second, for its publication to borrow.
struct Second {
    processes: Vec<Process>,
    identities: Vec<IdentityObservation>,
    /// The exits and starts of the processes that rotated since the tick before.
    events: Vec<Event>,
    /// The identity each of `events` names.
    event_identities: Vec<IdentityObservation>,
    cpus: Vec<CPUTicks>,
    gauges: Gauges,
    memory: Memory,
    interfaces: Vec<Interface>,
    gpu_energy_nj: u64,
    gpu_active_ns: [u64; GPU_STATES],
    cores: Vec<States>,
    battery: BatteryReading,
    disks: DiskSnapshot,
    system: SystemReading,
}

impl Second {
    /// The readings of `second`, which the run `run` reads at `times`, of processes that run
    /// `programs`.
    fn read(
        programs: &[ProgramReference],
        run: &RunPlan,
        second: u64,
        times: &ObservationTimes,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let valid = valid_seconds(second);
        let incidents = incident_seconds(second);
        let offset = second % PERIOD_SECONDS;
        let gauges = if offset == ZERO_SECOND {
            GAUGES_ZERO
        } else if INCIDENT_OFFSETS.contains(&offset) {
            GAUGES_PEAK
        } else {
            GAUGES_BASELINE
        };
        let (events, event_identities) = publish_rotations(programs, rotated_seconds(run, second))?;
        let thermal_pressure = match offset {
            THERMAL_FAIR_SECOND => THERMAL_MODERATE,
            THERMAL_SERIOUS_SECOND => THERMAL_HEAVY,
            _ => THERMAL_NOMINAL,
        };

        Ok(Self {
            processes: (0..PROCESSES)
                .map(|process| publish_process(process, creation_at(process, second), second))
                .collect::<Result<_, _>>()?,
            identities: (0..PROCESSES)
                .zip(programs)
                .map(|(process, program)| {
                    publish_identity(
                        process,
                        creation_at(process, second).identity,
                        UsageAvailability::Measured,
                        *program,
                    )
                })
                .collect::<Result<_, _>>()?,
            events,
            event_identities,
            cpus: (0..CPUS)
                .map(|cpu| publish_cpu(cpu, second))
                .collect::<Result<_, _>>()?,
            gauges,
            memory: publish_memory(gauges.memory_used_bytes),
            interfaces: (0..INTERFACES)
                .map(|interface| publish_interface(interface, valid))
                .collect::<Result<_, _>>()?,
            gpu_energy_nj: valid * GPU_ENERGY_NJ_PER_SECOND,
            gpu_active_ns: [valid * GPU_STATE_ACTIVE_NS_PER_SECOND
                + incidents
                    * (GPU_STATE_ACTIVE_NS_PER_INCIDENT_SECOND - GPU_STATE_ACTIVE_NS_PER_SECOND);
                GPU_STATES],
            cores: (0..CPUS).map(|core| publish_core(core, valid)).collect(),
            battery: publish_battery(times.continuous_ns),
            disks: publish_disks(times.continuous_ns, valid)?,
            system: publish_system(thermal_pressure),
        })
    }
}

/// The gauges the contract names but the GPU's, which its counters give, as the generated Mac
/// reads them in one second.
#[derive(Clone, Copy)]
struct Gauges {
    power_watts: f32,
    fan_rpm: [f32; 2],
    /// The chip's, the CPU's and the GPU's.
    temperatures_celsius: [f32; 3],
    /// The app, wired and compressed memory added up, as the memory chart stacks them.
    memory_used_bytes: u64,
}

/// The contract's valid seconds from 1 to `second`, every second but each period's zero second
/// and missed tick, which add to no counter.
fn valid_seconds(second: u64) -> u64 {
    // The division floors, to the periods that end by `second`.
    let periods = second / PERIOD_SECONDS;
    let offset = second % PERIOD_SECONDS;
    let invalid =
        2 * periods + u64::from(ZERO_SECOND <= offset) + u64::from(MISSED_SECOND <= offset);

    second - invalid
}

/// The contract's incident seconds from 1 to `second`.
fn incident_seconds(second: u64) -> u64 {
    // The division floors, to the periods that end by `second`.
    let periods = second / PERIOD_SECONDS;
    let offset = second % PERIOD_SECONDS;
    let incidents_in_period = INCIDENT_OFFSETS
        .iter()
        .filter(|incident| **incident <= offset)
        .count();

    collector::u64_from_usize(INCIDENT_OFFSETS.len()) * periods
        + collector::u64_from_usize(incidents_in_period)
}

/// One creation of a process: its identity, and the second it started in, `None` for the input's
/// first creations, which started before it.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Creation {
    identity: u64,
    started: Option<u64>,
}

/// The creation process `process` runs at `second`. The rotating processes take their turns in
/// pairs, one pair a second from second 1, and each rotation starts the next creation identity.
fn creation_at(process: u64, second: u64) -> Creation {
    let first = Creation {
        identity: process + 1,
        started: None,
    };
    if process < ROTATING_FIRST {
        return first;
    }

    let rotating = process - ROTATING_FIRST;
    // The division floors, to the pair the process is in.
    let turn_first = rotating / ROTATIONS_PER_SECOND + 1;
    if second < turn_first {
        return first;
    }

    // The division floors, to the pair's latest turn by `second`.
    let started =
        turn_first + (second - turn_first) / ROTATION_ROUND_SECONDS * ROTATION_ROUND_SECONDS;
    Creation {
        identity: CREATION_ROTATED_FIRST
            + ROTATIONS_PER_SECOND * (started - 1)
            + rotating % ROTATIONS_PER_SECOND,
        started: Some(started),
    }
}

/// One process taking a new creation in `second`: the creation that exits and its successor.
struct Rotation {
    second: u64,
    process: u64,
    ended: Creation,
    started: Creation,
}

/// The rotations of `seconds`, in the order a tick publishes them.
fn rotations(seconds: RangeInclusive<u64>) -> Vec<Rotation> {
    let mut rotations = Vec::new();
    for second in seconds {
        let first = ROTATING_FIRST + ROTATIONS_PER_SECOND * ((second - 1) % ROTATION_ROUND_SECONDS);
        for process in first..first + ROTATIONS_PER_SECOND {
            let started = creation_at(process, second);
            assert_eq!(Some(second), started.started);
            rotations.push(Rotation {
                second,
                process,
                ended: creation_at(process, second - 1),
                started,
            });
        }
    }

    rotations
}

/// The exits and starts of the processes that rotate in `seconds`, which run `programs`, with
/// the identity each names. Each old creation's exit comes before its successor's start.
fn publish_rotations(
    programs: &[ProgramReference],
    seconds: RangeInclusive<u64>,
) -> Result<(Vec<Event>, Vec<IdentityObservation>), Box<dyn std::error::Error>> {
    let mut events = Vec::new();
    let mut identities = Vec::new();
    for rotation in rotations(seconds) {
        let pid = PID_FIRST + i32::try_from(rotation.process)?;
        let program = programs[usize::try_from(rotation.process)?];

        // The exit's endpoint is the old creation's counters in the second it exits, so its App
        // loses no growth to the rotation.
        events.push(Event::Exit {
            pid,
            last: Ok(publish_usage(
                rotation.process,
                rotation.ended,
                rotation.second,
            )),
            noticed: Noticed::OnExit,
        });
        identities.push(publish_identity(
            rotation.process,
            rotation.ended.identity,
            UsageAvailability::Measured,
            program,
        )?);
        events.push(Event::Start {
            pid,
            coalition: None,
        });
        identities.push(publish_identity(
            rotation.process,
            rotation.started.identity,
            UsageAvailability::NotApplicable,
            program,
        )?);
    }

    Ok((events, identities))
}

/// Process `process`'s usage at `second` in its creation `creation`, as the contract gives it:
/// a creation's CPU time starts at 2^53 + 1,000 ns for each creation before it, its other
/// counters at zero, and each grows at the process's pace in the valid seconds since it started.
fn publish_usage(process: u64, creation: Creation, second: u64) -> Usage {
    let since_start = |count: fn(u64) -> u64| count(second) - creation.started.map_or(0, count);
    let valid = since_start(valid_seconds);
    let incidents = since_start(incident_seconds);
    // The incident process spends `INCIDENT_FOLD` seconds' CPU time and energy in each incident.
    let spending = valid + u64::from(process == INCIDENT_PROCESS) * (INCIDENT_FOLD - 1) * incidents;
    // A first creation started a second before the input, and a rotated one half a second
    // before the tick of the second it started in.
    let awake_ns_at_start = match creation.started {
        None => ORIGIN_CONTINUOUS_NS - NANOSECONDS_PER_SECOND,
        Some(started) => {
            ORIGIN_CONTINUOUS_NS + started * NANOSECONDS_PER_SECOND - NANOSECONDS_PER_SECOND / 2
        }
    };

    Usage {
        awake_ns_at_start,
        user_ns: CPU_BASE_NS
            + 1_000 * (creation.identity - 1)
            + spending * 1_000_000 * (process % 8 + 1),
        system_ns: 0,
        energy_nj: spending * 1_000 * (process + 1),
        instructions: 0,
        super_instructions: 0,
        cycles: 0,
        idle_wakeups: 0,
        interrupt_wakeups: 0,
        pageins: 0,
        resident_bytes: 1_048_576 * (process % APPS + 1),
        footprint_bytes: 1_048_576 * (process % APPS + 1),
        disk_read_bytes: valid * 4_096 * (process % 4 + 1),
        disk_written_bytes: valid * 2_048 * (process % 4 + 1),
    }
}

/// Process `process`'s readings at `second` in its creation `creation`.
fn publish_process(
    process: u64,
    creation: Creation,
    second: u64,
) -> Result<Process, std::num::TryFromIntError> {
    Ok(Process {
        pid: PID_FIRST + i32::try_from(process)?,
        usage: publish_usage(process, creation, second),
        task: Task {
            threads: 1,
            faults: 0,
            context_switches: 0,
            mach_syscalls: 0,
            unix_syscalls: 0,
        },
    })
}

fn publish_identity(
    process: u64,
    creation: u64,
    usage: UsageAvailability,
    program: ProgramReference,
) -> Result<IdentityObservation, std::num::TryFromIntError> {
    Ok(IdentityObservation {
        pid: PID_FIRST + i32::try_from(process)?,
        usage,
        identity: ProcessIdentity::Measured {
            creation: ProcessCreation {
                unique_id: creation,
                pid_version: i32::try_from(creation)?,
            },
            program,
        },
    })
}

/// CPU `cpu`'s ticks at `second`, a hundred a second as macOS counts them, all of them idle in
/// a second that is not valid, since the generated Mac does no work in a second its processes
/// do none.
fn publish_cpu(cpu: u64, second: u64) -> Result<CPUTicks, std::num::TryFromIntError> {
    let cpu = u32::try_from(cpu)?;
    let valid = u32::try_from(valid_seconds(second))?;
    let invalid = u32::try_from(second)? - valid;

    Ok(CPUTicks {
        user: valid * (10 + cpu),
        system: valid * 5,
        nice: 0,
        idle: valid * (85 - cpu) + invalid * 100,
    })
}

/// Core `core`'s time in its two active states after `valid` valid seconds, the user and
/// system ticks `publish_cpu` gives its CPU, in nanoseconds.
fn publish_core(core: u64, valid: u64) -> States {
    States::with_active_ns(&[
        valid * (10 + core) * NANOSECONDS_PER_TICK,
        valid * 5 * NANOSECONDS_PER_TICK,
    ])
}

/// The memory of a Mac that uses `used_bytes`, which the memory chart stacks as app memory,
/// three quarters of it, and wired and compressed memory, an eighth each.
fn publish_memory(used_bytes: u64) -> Memory {
    assert!(used_bytes.is_multiple_of(8));
    let eighth_bytes = used_bytes / 8;
    let app_bytes = 6 * eighth_bytes;

    Memory {
        total_bytes: MEMORY_TOTAL_BYTES,
        free_bytes: MEMORY_TOTAL_BYTES - used_bytes,
        active_bytes: app_bytes,
        wired_bytes: eighth_bytes,
        compressed_bytes: eighth_bytes,
        anonymous_bytes: app_bytes,
        pressure: Pressure::Normal,
        ..Memory::default()
    }
}

/// Interface `interface`'s counters after `valid` valid seconds, under an index and a name of
/// its own.
fn publish_interface(interface: u16, valid: u64) -> Result<Interface, String> {
    let text = format!("en{interface}");
    let mut bytes = [0; IFNAMSIZ];
    bytes[..text.len()].copy_from_slice(text.as_bytes());
    let name = Name::from_bytes(bytes, text.len()).ok_or_else(|| format!("name {text}"))?;
    let scale = u64::from(interface) + 1;

    Ok(Interface {
        index: interface + 1,
        name,
        received_bytes: valid * 100_000 * scale,
        sent_bytes: valid * 20_000 * scale,
        received_packets: valid * 100 * scale,
        sent_packets: valid * 20 * scale,
        receive_errors: 0,
        send_errors: 0,
        receive_drops: 0,
    })
}

/// The battery, which reads the same each second, as a charged battery on power does.
fn publish_battery(continuous_ns: u64) -> BatteryReading {
    BatteryReading {
        source_continuous_ns: continuous_ns,
        charge_current: Some(100),
        charge_maximum: Some(100),
        full_charge_capacity: Some(4_500),
        design_capacity: Some(5_000),
        cycle_count: Some(100),
        charging: Some(false),
        external_power: Some(true),
        voltage_mv: Some(13_000),
        current_ma: Some(0),
        adapter_watts: Some(96),
    }
}

/// The disks' counters after `valid` valid seconds, each disk under identities of its own.
fn publish_disks(
    continuous_ns: u64,
    valid: u64,
) -> Result<DiskSnapshot, std::num::TryFromIntError> {
    let mut records = [DiskReading::default(); DISKS_MAX];
    for (disk, record) in records.iter_mut().take(DISKS).enumerate() {
        let text = format!("disk{disk}");
        let mut name = [0; NAME_BYTES];
        name[..text.len()].copy_from_slice(text.as_bytes());
        let disk = u64::try_from(disk)?;
        let scale = disk + 1;
        *record = DiskReading {
            driver_id: DISK_DRIVER_ID_FIRST + disk,
            media_id: DISK_MEDIA_ID_FIRST + disk,
            name,
            failed: false,
            read_bytes: Some(valid * 65_536 * scale),
            write_bytes: Some(valid * 32_768 * scale),
            read_operations: Some(valid * 16 * scale),
            write_operations: Some(valid * 8 * scale),
            read_errors: Some(0),
            write_errors: Some(0),
            read_time_ns: Some(valid * 1_000_000 * scale),
            write_time_ns: Some(valid * 500_000 * scale),
        };
    }

    Ok(DiskSnapshot {
        source_continuous_ns: continuous_ns,
        state: ScanState::Complete,
        count: DISKS,
        records,
    })
}

/// The system fields, which read the same each second but for the thermal pressure, macOS's
/// level `thermal_pressure`.
fn publish_system(thermal_pressure: u8) -> SystemReading {
    SystemReading {
        load_average: Some(LOAD_AVERAGE),
        open_files: Some(4_096),
        open_files_limit: Some(122_880),
        thermal_pressure: Some(thermal_pressure),
        gpu_memory: Some(GpuMemoryReading {
            in_use_bytes: Some(GIB),
            allocated_bytes: Some(2 * GIB),
        }),
        // One boot, whose continuous clock started at zero.
        boot_wall_ns: Some(ORIGIN_WALL_NS - ORIGIN_CONTINUOUS_NS),
    }
}

/// What the generated Mac is: a chip of `CPUS` cores in two clusters, its super cores a third
/// of them.
fn start_facts() -> Result<MachineFacts, Box<dyn std::error::Error>> {
    let text = |bytes: &[u8]| FactText::new(bytes).ok_or("a fact's text");
    let cpu_cores = u32::try_from(CPUS)?;
    let super_cores = cpu_cores / 3; // exact, as the assertion below the constants checks

    Ok(MachineFacts {
        model: Some(text(b"Reference Mac")?),
        chip: Some(text(b"Reference chip")?),
        architecture: Some(text(b"arm64")?),
        cpu_cores: Some(cpu_cores),
        gpu_cores: Some(20),
        memory_bytes: Some(MEMORY_TOTAL_BYTES),
        levels: Some([
            PerformanceLevel {
                name: text(b"Super")?,
                logical_cpus: super_cores,
            },
            PerformanceLevel {
                name: text(b"Performance")?,
                logical_cpus: cpu_cores - super_cores,
            },
        ]),
    })
}

/// Interns the program of process `process`, which runs from the bundle of App `process` mod 32,
/// under a name of 32 bytes and a bundle path of 96, the lengths the contract fixes.
fn start_program(
    writer: &mut Writer,
    process: u64,
) -> Result<ProgramReference, Box<dyn std::error::Error>> {
    let name = program_name(process)?;
    let path = fixed_path(process % APPS)?;
    let mut reference = None;
    writer
        .intern_program(
            &ProgramBytes {
                executable_name: &name,
                app_path: Some(path.as_bytes()),
            },
            &mut reference,
        )
        .map_err(|availability| format!("program {process}: {availability:?}"))?;

    reference.ok_or_else(|| format!("program {process} was not interned").into())
}

/// The name of process `process`'s program, `PROGRAM_NAME_BYTES` long. The last byte of
/// `NON_UTF8_PROCESS`'s is `NON_UTF8_BYTE`, so each reader of a name meets bytes that are not
/// text, which the ring and the saved tables keep as bytes.
fn program_name(process: u64) -> Result<Vec<u8>, String> {
    let mut name = fixed(
        &format!("reference-process-{process:03}"),
        PROGRAM_NAME_BYTES,
    )?
    .into_bytes();
    if process == NON_UTF8_PROCESS {
        name[PROGRAM_NAME_BYTES - 1] = NON_UTF8_BYTE;
    }

    Ok(name)
}

/// `text` padded with dots to `length` bytes.
fn fixed(text: &str, length: usize) -> Result<String, String> {
    if text.len() > length {
        return Err(format!("{text} is longer than {length} bytes"));
    }

    Ok(format!("{text:.<length$}"))
}

/// The bundle path of App `app`, `PROGRAM_PATH_BYTES` long.
fn fixed_path(app: u64) -> Result<String, String> {
    let bundle = format!("/Reference App {app:02}.app");
    let directory = fixed(
        "/Applications/Reference input",
        PROGRAM_PATH_BYTES - bundle.len(),
    )?;
    let path = format!("{directory}{bundle}");
    assert_eq!(PROGRAM_PATH_BYTES, path.len());

    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn reads_both_options_of_a_generation() {
        // Goal: the two options set the generation. Method: parse them in an order of their own.
        let command = command_from(&words("--seconds 3600 --directory /tmp/input"));

        assert_eq!(
            Ok(Command::Generate {
                directory: PathBuf::from("/tmp/input"),
                seconds: 3_600,
            }),
            command
        );
    }

    #[test]
    fn refuses_an_input_longer_than_the_contracts_hour() {
        // Goal: no generation passes for the contract's input with more than it holds. Method:
        // ask for one second past the hour.
        let command = command_from(&words("--directory /tmp/input --seconds 3601"));

        assert!(command.is_err());
    }

    #[test]
    fn leaves_out_two_seconds_of_each_period_from_the_valid_seconds() {
        // Goal: every period's zero second and missed tick add to no counter, past the first
        // period too. Method: count the valid seconds on each side of the second period's start
        // and of its two special seconds.
        let valid = [599, 600, 900, 902].map(valid_seconds);

        assert_eq!([597, 598, 897, 898], valid);
    }

    #[test]
    fn rotates_each_process_once_a_round_from_its_pairs_turn() {
        // Goal: each rotating process takes a new creation every 127 seconds from its pair's
        // turn, the creations numbered in rotation order, and processes 0 and 1 never rotate.
        // Method: read the creation identities on each side of process 2's first and second
        // turns, at the last pair's first turn, and of process 1 late in the input.
        let identities = [
            (2, 0),
            (2, 1),
            (2, 127),
            (2, 128),
            (255, 126),
            (255, 127),
            (1, 3_599),
        ]
        .map(|(process, second)| creation_at(process, second).identity);

        assert_eq!([3, 257, 257, 511, 256, 510, 2], identities);
    }

    #[test]
    fn splits_each_input_at_the_minute_boundary_before_its_middle() {
        // Goal: the second run starts at half the input on a minute boundary, the contract's 1,800
        // for its hour. Method: split the shortest input, one of an odd number of minutes, and
        // the hour.
        let splits = [SECONDS_MIN, 3_540, SECONDS_MAX].map(run_split);

        assert_eq!([180, 1_740, 1_800], splits);
    }

    #[test]
    fn counts_ten_incident_seconds_in_each_period() {
        // Goal: every period holds the contract's ten incidents, past the first period too.
        // Method: count the incident seconds on each side of the first incident of the first
        // and the second period, and at the end of each.
        let incidents = [9, 10, 599, 609, 610, 1_199].map(incident_seconds);

        assert_eq!([0, 1, 10, 10, 11, 20], incidents);
    }

    #[test]
    fn names_each_program_in_the_contracts_lengths() {
        // Goal: each name and path has the length the contract fixes. Method: build the longest
        // App number's path and the names of the first process and of the non-UTF-8 one.
        let lengths = (
            program_name(0).map(|name| name.len()),
            program_name(NON_UTF8_PROCESS).map(|name| name.len()),
            fixed_path(APPS - 1).map(|path| path.len()),
        );

        assert_eq!((Ok(32), Ok(32), Ok(96)), lengths);
    }

    #[test]
    fn names_only_one_program_in_bytes_that_are_not_utf8() {
        // Goal: exactly one process's program name is not UTF-8, so every other name stays the
        // contract's ASCII. Method: collect the processes whose names do not decode.
        let processes: Vec<u64> = (0..PROCESSES)
            .filter(|process| {
                program_name(*process).is_ok_and(|name| String::from_utf8(name).is_err())
            })
            .collect();

        assert_eq!(vec![NON_UTF8_PROCESS], processes);
    }
}
