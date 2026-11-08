//! Materializes the fixed reference input of P00's contract for W10, a generated Mac's hour,
//! into a recording of its own: the collector's ring writer publishes each second and the
//! persister's seal worker saves each minute, as a recording's collector and persister do. Every
//! reading is a function of the second and the process, so two generations read the same.
//!
//! The recording is left as a running collector leaves it, its last run live with the minute it
//! ends in still in its ring, which stays registered for readers until `--unlink` removes it.

use std::fs::DirBuilder;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::thread::JoinHandle;
use std::time::Duration;

use collector::machine::{CPUTicks, Memory};
use collector::processes::{Process, Task, Usage};
use collector::recording::control::{Control, Failure, Role, State};
use collector::recording::private::PrivateDirectory;
use collector::recording::{Registration, Registry};
use collector::ring::{
    DictionaryCapacity, Dropped, Identity, IdentityObservation, IdentityPublication,
    ObservationTimes, Place, ProcessCreation, ProcessIdentity, ProgramBytes, ProgramReference,
    Readings, Shape, SupplementalReadings, UsageAvailability, Writer,
};
use persistence::retention::Period;

const USAGE: &str =
    "usage: reference_input --directory PATH --seconds N | reference_input --unlink PATH";
/// The contract's input lasts an hour.
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
    // Each CPU idles for what it does not spend, so no CPU's idle ticks fall below zero.
    assert!(CPUS < 85);
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
    Unlink { directory: PathBuf },
}

fn command_from(words: &[String]) -> Result<Command, String> {
    match words {
        [unlink, directory] if unlink == "--unlink" => Ok(Command::Unlink {
            directory: PathBuf::from(directory),
        }),
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
    drop(persistence::Store::open(directory)?);
    let period = Period::from_seconds(RETENTION_SECONDS).ok_or("retention period")?;
    period.write(&PrivateDirectory::open(directory)?)?;
    // The worker reports each pass here, as a recorder's persister does.
    let control = Control::create(directory)?;

    let place = Place {
        lock_path: directory.join("ring.lock"),
        name_prefix: format!("/fdri{}", std::process::id()),
    };
    let started = Registration::start_with_system(
        directory,
        &place,
        RUN_SHAPE,
        DictionaryCapacity {
            entries: 65_536,
            arena_bytes: 64 * 1024 * 1024,
        },
        ORIGIN_CONTINUOUS_NS,
    )?;
    let (mut writer, registered) = started.into_parts();
    registered?;
    let run = writer.identity().ok_or("the run has no identity")?;
    // The worker starts once the run is registered, since a worker that finds no run exits.
    let worker = generate_worker(directory, control.authority())?;

    let mut machine = Machine::start(&mut writer)?;
    let mut paced = Paced {
        control: &control,
        run: run.run,
        worker: &worker,
        saved_end: writer.publication_newest() + 1,
        heartbeat_ns: 0,
    };
    let last_minute_first = generate_seconds(&mut writer, &mut machine, &mut paced, seconds)?;
    paced.wait_for_saved(last_minute_first)?;
    // A pass that finds nothing to save ends in the worker's pause, so the process ends with the
    // worker asleep rather than part way through a save.
    paced.wait_for_pass()?;

    println!(
        "reference_input directory={} seconds={seconds} ring={} run={} saved_end={} \
        publication_newest={}",
        directory.display(),
        writer.name(),
        collector::recording::identity_text(run.run),
        paced.saved_end,
        writer.publication_newest(),
    );
    // The process ends with the run live and the minute it ends in in its ring, as a recording
    // stands while its collector and persister run.
    Ok(())
}

/// Publishes the input's `seconds`, waiting on the worker wherever the ring would otherwise
/// overwrite a publication it has yet to save, and returns the first publication of the minute
/// the input ends in.
fn generate_seconds(
    writer: &mut Writer,
    machine: &mut Machine,
    paced: &mut Paced<'_>,
    seconds: u64,
) -> Result<u64, Box<dyn std::error::Error>> {
    let mut last_minute_first = None;
    for second in 0..seconds {
        // A missed tick publishes nothing, so its second has no readings.
        if second % PERIOD_SECONDS == MISSED_SECOND {
            continue;
        }

        let publication = writer.publication_newest() + 1;
        paced.wait_for_room(publication)?;
        // Both divisions floor, to the minute a second falls in.
        if second / SECONDS_PER_MINUTE == (seconds - 1) / SECONDS_PER_MINUTE {
            last_minute_first.get_or_insert(publication);
        }
        machine.publish(writer, second)?;
    }

    last_minute_first.ok_or_else(|| "no last minute".into())
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
fn unlink(directory: &Path) -> Result<(), Box<dyn std::error::Error>> {
    // A path that names no directory refuses here, rather than reading as a recording of no run.
    drop(PrivateDirectory::open(directory)?);
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
    worker: &'a JoinHandle<persistence::Result<persistence::worker::Outcome>>,
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

/// The generated Mac: its processes in increasing pid order, and the programs they run.
struct Machine {
    programs: Vec<ProgramReference>,
}

impl Machine {
    fn start(writer: &mut Writer) -> Result<Self, Box<dyn std::error::Error>> {
        let mut programs = Vec::new();
        for process in 0..PROCESSES {
            programs.push(start_program(writer, process)?);
        }

        Ok(Self { programs })
    }

    fn publish(
        &mut self,
        writer: &mut Writer,
        second: u64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let processes: Vec<Process> = (0..PROCESSES)
            .map(|process| publish_process(process, second))
            .collect::<Result<_, _>>()?;
        let identities: Vec<IdentityObservation> = (0..PROCESSES)
            .zip(&self.programs)
            .map(|(process, program)| publish_identity(process, *program))
            .collect::<Result<_, _>>()?;
        let cpus: Vec<CPUTicks> = (0..CPUS)
            .map(|cpu| publish_cpu(cpu, second))
            .collect::<Result<_, _>>()?;
        let readings = Readings {
            run_tick: second,
            cpus: &cpus,
            memory: &Memory::default(),
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &processes,
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: Dropped::default(),
        };
        let times = ObservationTimes {
            wall_ns: ORIGIN_WALL_NS + second * NANOSECONDS_PER_SECOND,
            continuous_ns: ORIGIN_CONTINUOUS_NS + second * NANOSECONDS_PER_SECOND,
            awake_ns: ORIGIN_CONTINUOUS_NS + second * NANOSECONDS_PER_SECOND,
        };
        writer.publish_with_disks(
            &readings,
            Some(&times),
            &IdentityPublication {
                processes: &identities,
                events: &[],
            },
            &SupplementalReadings::default(),
        )?;

        Ok(())
    }
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

/// Process `process`'s readings at `second`, as the contract gives them.
fn publish_process(process: u64, second: u64) -> Result<Process, std::num::TryFromIntError> {
    let valid = valid_seconds(second);

    Ok(Process {
        pid: PID_FIRST + i32::try_from(process)?,
        usage: Usage {
            awake_ns_at_start: ORIGIN_CONTINUOUS_NS - NANOSECONDS_PER_SECOND,
            user_ns: CPU_BASE_NS + 1_000 * process + valid * 1_000_000 * (process % 8 + 1),
            system_ns: 0,
            energy_nj: valid * 1_000 * (process + 1),
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
        },
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
    program: ProgramReference,
) -> Result<IdentityObservation, std::num::TryFromIntError> {
    let creation = process + 1;

    Ok(IdentityObservation {
        pid: PID_FIRST + i32::try_from(process)?,
        usage: UsageAvailability::Measured,
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

/// Interns the program of process `process`, which runs from the bundle of App `process` mod 32,
/// under a name of 32 bytes and a bundle path of 96, the lengths the contract fixes.
fn start_program(
    writer: &mut Writer,
    process: u64,
) -> Result<ProgramReference, Box<dyn std::error::Error>> {
    let name = fixed(
        &format!("reference-process-{process:03}"),
        PROGRAM_NAME_BYTES,
    )?;
    let path = fixed_path(process % APPS)?;
    let mut reference = None;
    writer
        .intern_program(
            &ProgramBytes {
                executable_name: name.as_bytes(),
                app_path: Some(path.as_bytes()),
            },
            &mut reference,
        )
        .map_err(|availability| format!("program {process}: {availability:?}"))?;

    reference.ok_or_else(|| format!("program {process} was not interned").into())
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
    fn names_each_program_in_the_contracts_lengths() {
        // Goal: each name and path has the length the contract fixes. Method: build the longest
        // App number's path and a process's name.
        let lengths = (
            fixed("reference-process-255", PROGRAM_NAME_BYTES).map(|name| name.len()),
            fixed_path(APPS - 1).map(|path| path.len()),
        );

        assert_eq!((Ok(32), Ok(96)), lengths);
    }
}
