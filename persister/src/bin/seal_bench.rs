//! Publishes minutes of a generated Mac to a ring of their own, seals them through the
//! persister's own worker, and prints the CPU time and the elapsed time each window took and how
//! much of it went to deriving the App tables. A change to sealing is measured in seconds, with
//! no collector running, on any Mac.
//!
//! The generated Mac has the shape of an M5 Pro's busy minute on 2026-10-22: about 1,050
//! processes a tick, two thirds of them readable, running 500 programs of 63 apps, 37 forks a
//! second, each with its exec and exit, and 715 coalitions. Its readings change every tick, so
//! every column compresses as a real one does rather than as a constant.

use std::cell::RefCell;
use std::ffi::c_int;
use std::ops::RangeInclusive;
use std::path::Path;
use std::process::ExitCode;
use std::time::Instant;

use collector::coalitions::Coalition;
use collector::interfaces::{Interface, Name};
use collector::machine::{CPUTicks, Memory};
use collector::processes::{Process, Task, Usage};
use collector::recording::Registration;
use collector::ring::{
    DictionaryCapacity, Dropped, IdentityObservation, IdentityPublication, ObservationTimes, Place,
    ProcessCreation, ProcessIdentity, ProgramBytes, ProgramReference, Readings, Shape,
    UsageAvailability, Writer,
};
use collector::tracker::{Event, Noticed};
use persistence::worker::{Derivation, Derive};

const USAGE: &str = "usage: seal_bench --windows N --processes N --derive apps|none";
const TICKS_PER_WINDOW: u64 = 60;
/// Bounds the run, since every tick stays in the ring until the worker seals it.
const WINDOWS_MAX: u64 = 10;
const PROCESSES_MAX: u64 = 4_096;
const PROGRAMS: u64 = 500;
const APPS: u64 = 63;
const FORKS_PER_TICK: u64 = 37;
const COALITIONS: u64 = 715;
const CPUS: u32 = 18;
const INTERFACES: u64 = 34;
/// One process in this many refuses its usage, as those of other users do without root.
const REFUSED_EVERY: u64 = 3;
const CLOCK_PROCESS_CPUTIME_ID: c_int = 12;

fn main() -> ExitCode {
    let options = match collector::arguments("seal_bench").and_then(|words| options_from(&words)) {
        Ok(options) => options,
        Err(usage) => {
            eprintln!("{usage}");
            return ExitCode::from(2);
        }
    };

    match run(&options) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("seal_bench: {error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, PartialEq)]
struct Options {
    windows: u64,
    processes: u64,
    derive: Deriving,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Deriving {
    Apps,
    Nothing,
}

fn options_from(words: &[String]) -> Result<Options, String> {
    let mut windows = None;
    let mut processes = None;
    let mut derive = None;
    for pair in words.chunks(2) {
        let [flag, value] = pair else {
            return Err(USAGE.to_owned());
        };
        match flag.as_str() {
            "--windows" => windows = Some(options_count(value, 2..=WINDOWS_MAX)?),
            "--processes" => {
                processes = Some(options_count(value, FORKS_PER_TICK + 1..=PROCESSES_MAX)?);
            }
            "--derive" => {
                derive = match value.as_str() {
                    "apps" => Some(Deriving::Apps),
                    "none" => Some(Deriving::Nothing),
                    _ => return Err(USAGE.to_owned()),
                }
            }
            _ => return Err(USAGE.to_owned()),
        }
    }

    match (windows, processes, derive) {
        (Some(windows), Some(processes), Some(derive)) => Ok(Options {
            windows,
            processes,
            derive,
        }),
        _ => Err(USAGE.to_owned()),
    }
}

fn options_count(value: &str, bounds: RangeInclusive<u64>) -> Result<u64, String> {
    match value.parse::<u64>() {
        Ok(count) if bounds.contains(&count) => Ok(count),
        _ => Err(format!(
            "{USAGE}\n{value} is not {} to {}",
            bounds.start(),
            bounds.end()
        )),
    }
}

fn run(options: &Options) -> Result<(), Box<dyn std::error::Error>> {
    persistence::engine::initialize_process()?;
    let root = std::env::temp_dir().join(format!("seal_bench-{}", std::process::id()));
    collector::recording::private::create_private_dir(&root)?;

    let place = Place {
        lock_path: root.join("ring.lock"),
        name_prefix: format!("/fdsb{}", std::process::id()),
    };
    let capacity = DictionaryCapacity {
        entries: 4_096,
        arena_bytes: 1024 * 1024,
    };
    let started =
        Registration::start_with_identity(&root, &place, record_shape(options), capacity, 0);

    // The ring outlives this process in shared memory unless it is unlinked, whatever failed.
    let sealed = started.map_err(Into::into).and_then(|started| {
        let (mut writer, registered) = started.into_parts();
        let sealed = registered
            .map_err(Into::into)
            .and_then(|()| run_record(&mut writer, options))
            .and_then(|()| run_seal(&root, options.derive));
        let unlinked = collector::ring::unlink_generation(writer.name());
        sealed.and_then(|marks| unlinked.map(|()| marks).map_err(Into::into))
    });
    std::fs::remove_dir_all(&root)?;

    let marks = sealed?;
    report(options, &marks)
}

/// Publishes every window of the run and marks the run finished, so the worker seals them all
/// and exits.
fn run_record(writer: &mut Writer, options: &Options) -> Result<(), Box<dyn std::error::Error>> {
    let mut machine = Machine::start(writer, options.processes)?;
    for tick in 0..options.windows * TICKS_PER_WINDOW {
        machine.publish(writer, tick)?;
    }
    writer.finish();

    Ok(())
}

fn record_shape(options: &Options) -> Shape {
    let slot_count = options.windows * TICKS_PER_WINDOW;
    Shape {
        slot_count,
        cpu_capacity: u64::from(CPUS),
        interface_capacity: INTERFACES,
        core_capacity: 1,
        state_capacity: 1,
        fan_capacity: 1,
        process_capacity: options.processes + FORKS_PER_TICK,
        coalition_capacity: COALITIONS,
        event_capacity: slot_count * 3 * FORKS_PER_TICK,
    }
}

/// Seals the recording at `root` in this process and returns the CPU time and the clock at the
/// start and the end of each window's derivation.
fn run_seal(root: &Path, deriving: Deriving) -> Result<Vec<Mark>, Box<dyn std::error::Error>> {
    let apps = viewer::SealedApps::default();
    let inner: &dyn Derive = match deriving {
        Deriving::Apps => &apps,
        Deriving::Nothing => &Underived,
    };
    let timed = Timed {
        inner,
        marks: RefCell::new(vec![Mark::take()?]),
    };

    let limit = persistence::worker::default_memory_limit();
    match persistence::worker::run(root, limit, &timed)? {
        persistence::worker::Outcome::Complete => Ok(timed.marks.into_inner()),
        persistence::worker::Outcome::Owned => Err("recording owned by another persister".into()),
    }
}

/// The process's CPU time and the clock at one moment of the run.
#[derive(Clone, Copy)]
struct Mark {
    cpu_ns: u64,
    at: Instant,
}

impl Mark {
    fn take() -> std::io::Result<Self> {
        let mut time = Timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        if unsafe { clock_gettime(CLOCK_PROCESS_CPUTIME_ID, &raw mut time) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let seconds = u64::try_from(time.tv_sec).map_err(std::io::Error::other)?;
        let nanoseconds = u64::try_from(time.tv_nsec).map_err(std::io::Error::other)?;

        Ok(Self {
            cpu_ns: seconds * 1_000_000_000 + nanoseconds,
            at: Instant::now(),
        })
    }
}

#[repr(C)]
struct Timespec {
    tv_sec: i64,
    tv_nsec: i64,
}

unsafe extern "C" {
    fn clock_gettime(clock: c_int, time: *mut Timespec) -> c_int;
}

/// Derives with `inner` and marks the start and the end of each derivation.
struct Timed<'inner> {
    inner: &'inner dyn Derive,
    marks: RefCell<Vec<Mark>>,
}

impl Derive for Timed<'_> {
    fn derive(
        &self,
        derivation: &Derivation<'_>,
    ) -> persistence::Result<Vec<persistence::TableManifest>> {
        self.marks.borrow_mut().push(Mark::take()?);
        let tables = self.inner.derive(derivation);
        self.marks.borrow_mut().push(Mark::take()?);

        tables
    }
}

/// Derives no table, as the persister did before it derived the App tables.
struct Underived;

impl Derive for Underived {
    fn derive(
        &self,
        _derivation: &Derivation<'_>,
    ) -> persistence::Result<Vec<persistence::TableManifest>> {
        Ok(Vec::new())
    }
}

/// What one window cost, from the end of the derivation before it to the end of its own.
struct Window {
    cpu_ns_seal: u64,
    cpu_ns_derive: u64,
    elapsed_ns: u64,
}

/// Prints each window, then the median of all but the first, which pays for the worker's start.
fn report(options: &Options, marks: &[Mark]) -> Result<(), Box<dyn std::error::Error>> {
    // The run's start, then a start and an end for each window's derivation.
    if marks.len() != 1 + 2 * usize::try_from(options.windows)? {
        return Err(format!("{} marks for {} windows", marks.len(), options.windows).into());
    }

    let windows = report_windows(marks)?;
    for (index, window) in windows.iter().enumerate() {
        println!(
            "seal_bench window={index} seal_cpu_ms={} derive_cpu_ms={} elapsed_ms={}",
            milliseconds(window.cpu_ns_seal),
            milliseconds(window.cpu_ns_derive),
            milliseconds(window.elapsed_ns),
        );
    }
    let steady = &windows[1..];
    let footprint = persistence::resources::Footprint::read()?;
    println!(
        "seal_bench windows={} processes={} derive={} seal_cpu_ms_median={} \
        derive_cpu_ms_median={} elapsed_ms_median={} footprint_bytes_max={}",
        options.windows,
        options.processes,
        match options.derive {
            Deriving::Apps => "apps",
            Deriving::Nothing => "none",
        },
        milliseconds(median(steady.iter().map(|window| window.cpu_ns_seal))),
        milliseconds(median(steady.iter().map(|window| window.cpu_ns_derive))),
        milliseconds(median(steady.iter().map(|window| window.elapsed_ns))),
        footprint.lifetime_bytes_max,
    );

    Ok(())
}

fn report_windows(marks: &[Mark]) -> Result<Vec<Window>, Box<dyn std::error::Error>> {
    let mut windows = Vec::new();
    let mut previous = marks[0];
    for pair in marks[1..].chunks(2) {
        let [start, end] = pair else {
            return Err("a derivation without its end".into());
        };
        windows.push(Window {
            cpu_ns_seal: end.cpu_ns - previous.cpu_ns,
            cpu_ns_derive: end.cpu_ns - start.cpu_ns,
            elapsed_ns: u64::try_from(end.at.duration_since(previous.at).as_nanos())?,
        });
        previous = *end;
    }

    Ok(windows)
}

/// The lower middle value, or the only one.
fn median(values: impl Iterator<Item = u64>) -> u64 {
    let mut sorted: Vec<u64> = values.collect();
    assert!(!sorted.is_empty());
    sorted.sort_unstable();

    sorted[(sorted.len() - 1) / 2]
}

/// Nanoseconds as milliseconds with three decimals, which floor.
fn milliseconds(nanoseconds: u64) -> String {
    let microseconds = nanoseconds / 1_000;
    format!("{}.{:03}", microseconds / 1_000, microseconds % 1_000)
}

/// The generated Mac: its live processes in increasing pid order, and what it reads each tick.
struct Machine {
    programs: Vec<ProgramReference>,
    live: Vec<Live>,
    ordinal_next: u64,
    coalitions: Vec<Coalition>,
    cpus: Vec<CPUTicks>,
    interfaces: Vec<Interface>,
}

/// A live process, whose ordinal is its pid and orders every value generated for it.
#[derive(Clone, Copy)]
struct Live {
    ordinal: u64,
    program: ProgramReference,
    process: Process,
}

impl Machine {
    fn start(writer: &mut Writer, processes: u64) -> Result<Self, Box<dyn std::error::Error>> {
        let mut programs = Vec::new();
        for program in 0..PROGRAMS {
            programs.push(start_program(writer, program)?);
        }
        let mut machine = Self {
            programs,
            live: Vec::new(),
            ordinal_next: 100,
            coalitions: (0..COALITIONS).map(start_coalition).collect(),
            cpus: Vec::new(),
            interfaces: (0..INTERFACES)
                .map(start_interface)
                .collect::<Result<_, _>>()?,
        };
        for _process in 0..processes {
            machine.fork()?;
        }

        Ok(machine)
    }

    /// Starts a process with the next pid, which is higher than every live one.
    fn fork(&mut self) -> Result<Live, Box<dyn std::error::Error>> {
        let ordinal = self.ordinal_next;
        self.ordinal_next += 1;
        let program = usize::try_from(ordinal * 7 % PROGRAMS)?;
        let live = Live {
            ordinal,
            program: self.programs[program],
            process: start_process(i32::try_from(ordinal)?, ordinal)?,
        };
        self.live.push(live);

        Ok(live)
    }

    fn publish(
        &mut self,
        writer: &mut Writer,
        tick: u64,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let events = self.publish_churn(tick)?;
        self.publish_advance(u32::try_from(tick)?);

        let measured: Vec<Process> = self
            .live
            .iter()
            .filter(|live| measured(live.ordinal))
            .map(|live| live.process)
            .collect();
        let processes: Vec<IdentityObservation> = self.live.iter().map(identity).collect();
        let identities: Vec<IdentityObservation> = events
            .iter()
            .map(|(event, live)| IdentityObservation {
                usage: match event {
                    Event::Exit { .. } => UsageAvailability::Measured,
                    _ => UsageAvailability::NotApplicable,
                },
                ..identity(live)
            })
            .collect();
        let events: Vec<Event> = events.into_iter().map(|(event, _live)| event).collect();
        let readings = Readings {
            run_tick: tick,
            cpus: &self.cpus,
            memory: &Memory::default(),
            interfaces: &self.interfaces,
            ioreport: None,
            smc: None,
            processes: &measured,
            processes_refused: u32::try_from(processes.len() - measured.len())?,
            processes_gone: 0,
            coalitions: &self.coalitions,
            events: &events,
            dropped: Dropped::default(),
        };
        let times = ObservationTimes {
            wall_ns: 1_800_000_000_000_000_000 + tick * 1_000_000_000,
            continuous_ns: tick * 1_000_000_000,
            awake_ns: tick * 1_000_000_000,
        };
        writer.publish_with_identity(
            &readings,
            Some(&times),
            &IdentityPublication {
                processes: &processes,
                events: &identities,
            },
        )?;

        Ok(())
    }

    /// Ends `FORKS_PER_TICK` processes spread across the pids and starts as many, and returns
    /// each one's events with the process they name.
    fn publish_churn(
        &mut self,
        tick: u64,
    ) -> Result<Vec<(Event, Live)>, Box<dyn std::error::Error>> {
        let mut events = Vec::new();
        for fork in 0..FORKS_PER_TICK {
            let count = collector::u64_from_usize(self.live.len());
            let index = usize::try_from((tick * 7_919 + fork * 104_729) % count)?;
            let ended = self.live.remove(index);
            events.push((
                Event::Exit {
                    pid: ended.process.pid,
                    last: Ok(ended.process.usage),
                    noticed: Noticed::OnExit,
                },
                ended,
            ));
        }
        for _fork in 0..FORKS_PER_TICK {
            let started = self.fork()?;
            let pid = started.process.pid;
            let coalition = Some(started.ordinal % COALITIONS);
            events.push((Event::Start { pid, coalition }, started));
            events.push((Event::Exec { pid }, started));
        }

        Ok(events)
    }

    /// Moves every counter on by an amount of its own, as a second of a busy Mac does.
    fn publish_advance(&mut self, second: u32) {
        for live in &mut self.live {
            let step = live.ordinal % 13 + 1;
            let usage = &mut live.process.usage;
            usage.user_ns += step * 1_000_000;
            usage.system_ns += step * 300_000;
            usage.energy_nj += step * 9_000;
            usage.instructions += step * 4_000_000;
            usage.cycles += step * 3_000_000;
            usage.idle_wakeups += step;
            usage.resident_bytes = (usage.resident_bytes + step * 4_096) % (1 << 32);
            usage.footprint_bytes = usage.resident_bytes / 2;
            usage.disk_read_bytes += u64::from(second) % step * 4_096;
            live.process.task.context_switches += live.process.task.threads * 10;
            live.process.task.unix_syscalls += live.process.task.threads * 50;
        }
        for (index, coalition) in (0_u64..).zip(&mut self.coalitions) {
            coalition.cpu_ns += (index % 17 + 1) * 2_000_000;
            coalition.energy_nj += (index % 17 + 1) * 15_000;
            coalition.idle_wakeups += index % 5;
        }
        self.cpus = (0..CPUS)
            .map(|cpu| CPUTicks {
                user: second * (cpu + 3),
                system: second * 2,
                nice: 0,
                idle: second * 90,
            })
            .collect();
        for interface in &mut self.interfaces {
            interface.received_bytes += u64::from(second % 7) * 1_500;
            interface.sent_bytes += u64::from(second % 5) * 1_500;
        }
    }
}

fn identity(live: &Live) -> IdentityObservation {
    IdentityObservation {
        pid: live.process.pid,
        usage: if measured(live.ordinal) {
            UsageAvailability::Measured
        } else {
            UsageAvailability::Refused
        },
        identity: ProcessIdentity::Measured {
            creation: ProcessCreation {
                unique_id: live.ordinal + 1_000_000,
                pid_version: live.process.pid,
            },
            program: live.program,
        },
    }
}

fn measured(ordinal: u64) -> bool {
    !ordinal.is_multiple_of(REFUSED_EVERY)
}

/// Interns program `program`, which runs from one of the apps, or from no app for one in four.
fn start_program(
    writer: &mut Writer,
    program: u64,
) -> Result<ProgramReference, Box<dyn std::error::Error>> {
    let name = format!("program-{program}");
    let app = format!("/Applications/App {}.app", program % APPS);
    let mut reference = None;
    writer
        .intern_program(
            &ProgramBytes {
                executable_name: name.as_bytes(),
                app_path: if program.is_multiple_of(4) {
                    None
                } else {
                    Some(app.as_bytes())
                },
            },
            &mut reference,
        )
        .map_err(|availability| format!("program {program}: {availability:?}"))?;

    reference.ok_or_else(|| format!("program {program} was not interned").into())
}

fn start_process(pid: i32, ordinal: u64) -> Result<Process, std::num::TryFromIntError> {
    // The kernel counts a task's events in 32 bits.
    let task = i32::try_from(ordinal % 10_000)?;

    Ok(Process {
        pid,
        usage: Usage {
            awake_ns_at_start: ordinal * 1_000_000,
            user_ns: ordinal * 3_000_000,
            system_ns: ordinal * 1_000_000,
            energy_nj: ordinal * 50_000,
            instructions: ordinal * 9_000_000,
            super_instructions: 0,
            cycles: ordinal * 7_000_000,
            idle_wakeups: ordinal,
            interrupt_wakeups: ordinal / 2,
            pageins: ordinal % 11,
            resident_bytes: ordinal % 977 * 65_536,
            footprint_bytes: ordinal % 977 * 32_768,
            disk_read_bytes: ordinal * 4_096,
            disk_written_bytes: ordinal * 1_024,
        },
        task: Task {
            threads: task % 40 + 1,
            faults: task * 13,
            context_switches: task * 100,
            mach_syscalls: task * 70,
            unix_syscalls: task * 90,
        },
    })
}

fn start_coalition(index: u64) -> Coalition {
    Coalition {
        id: index,
        tasks_started: index % 23 + 1,
        tasks_exited: index % 7,
        cpu_ns: index * 40_000_000,
        interrupt_wakeups: index * 3,
        idle_wakeups: index * 11,
        disk_read_bytes: index * 8_192,
        disk_written_bytes: index * 4_096,
        gpu_ns: 0,
        energy_nj: index * 600_000,
    }
}

fn start_interface(index: u64) -> Result<Interface, Box<dyn std::error::Error>> {
    let text = format!("en{index}");
    let mut name = [0; 16];
    name[..text.len()].copy_from_slice(text.as_bytes());

    Ok(Interface {
        index: u16::try_from(index + 1)?,
        name: Name::from_bytes(name, text.len()).ok_or("interface name")?,
        received_bytes: index * 1_000_000,
        sent_bytes: index * 400_000,
        received_packets: index * 1_000,
        sent_packets: index * 400,
        receive_errors: 0,
        send_errors: 0,
        receive_drops: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn reads_every_option() {
        // Goal: the three options set the run. Method: parse them in an order of their own.
        let options = options_from(&words("--derive none --windows 4 --processes 900"));

        assert_eq!(
            Ok(Options {
                windows: 4,
                processes: 900,
                derive: Deriving::Nothing,
            }),
            options
        );
    }

    #[test]
    fn refuses_a_run_too_short_to_leave_a_steady_window() {
        // Goal: a run keeps a window past the first, whose cost holds the worker's start.
        // Method: ask for one window.
        let options = options_from(&words("--windows 1 --processes 900 --derive apps"));

        assert!(options.is_err());
    }

    #[test]
    fn refuses_fewer_processes_than_a_tick_ends() {
        // Goal: every tick has a live process left to end. Method: ask for as many processes
        // as a tick ends.
        let options = options_from(&words("--windows 3 --processes 37 --derive apps"));

        assert!(options.is_err());
    }

    #[test]
    fn refuses_a_missing_option() {
        // Goal: every option is given, so a run never measures a default nobody chose. Method:
        // leave out the derivation.
        let options = options_from(&words("--windows 3 --processes 900"));

        assert!(options.is_err());
    }

    #[test]
    fn takes_the_median_below_the_middle_of_an_even_count() {
        // Goal: the median of the steady windows is one of them. Method: four values.
        let value = median([40, 10, 30, 20].into_iter());

        assert_eq!(20, value);
    }
}
