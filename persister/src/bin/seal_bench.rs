//! Publishes minutes of a generated Mac to a ring of their own, seals them through the
//! persister's own worker, and prints the CPU time and the elapsed time each window took and how
//! much of it went to deriving the App tables. A change to sealing is measured in seconds, with
//! no collector running, on any Mac.
//!
//! The generated Mac has the shape of an M5 Pro's busy minute on 2026-10-22: about 1,050
//! processes a tick, two thirds of them readable, running 500 programs of 63 apps, 37 forks a
//! second, each with its exec and exit, and 715 coalitions. Its readings change every tick, so
//! every column compresses as a real one does rather than as a constant.
//!
//! Given a persister in place of a derivation, it leaves the sealing to that binary as a process
//! of its own, as the recorder runs it, and prints what the process cost. The run fails unless
//! the persister sealed every tick and held no more than the memory it was given, which is what
//! the recorder gives one where the run names none, so a seal that outgrows the recorder's limit
//! at some count of processes is found before a Mac that busy is.
//! Given a restart as well, it kills that persister once it has sealed some of the run's windows
//! and starts it again for the rest, and holds both to the memory, since a persister started
//! again reads back a minute it did not seal itself.

use std::cell::RefCell;
use std::ffi::c_int;
use std::ops::{Range, RangeInclusive};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitCode, ExitStatus};
use std::sync::Arc;
use std::time::{Duration, Instant};

use collector::clock::Timebase;
use collector::coalitions::Coalition;
use collector::interfaces::{Interface, Name};
use collector::machine::{CPUTicks, Memory};
use collector::processes::{Process, Task, Usage};
use collector::recording::Registration;
use collector::recording::child::{PERSISTER_MEMORY_BYTES, ReapedChild};
use collector::ring::{
    DictionaryCapacity, Dropped, IdentityObservation, IdentityPublication, ObservationTimes, Place,
    ProcessCreation, ProcessIdentity, ProgramBytes, ProgramReference, Readings, RunIdentity, Shape,
    UsageAvailability, Writer,
};
use collector::tracker::{Event, Noticed};
use persistence::canonical::ReadWork;
use persistence::canonical::catalog::Catalog;
use persistence::resources::ByteBudget;
use persistence::schema::Table;
use persistence::worker::{Derivation, Derive};
use persistence::{Losses, Manifest};

const USAGE: &str = concat!(
    "usage: seal_bench --windows N --processes N --derive apps|restarted|none\n",
    "       seal_bench --windows N --processes N --persister PATH [--memory-bytes N] ",
    "[--restart-after-windows N]",
);
const TICKS_PER_WINDOW: u64 = 60;
/// A run's first window is not a steady one, so it has a second.
const WINDOWS_MIN: u64 = 2;
const WINDOWS_MAX: u64 = 10;
/// Above the 12,000 processes `kern.maxproc` lets an M5 Pro list.
const PROCESSES_MAX: u64 = 16_384;
/// The most processes a run's windows hold between them, its windows times its processes a
/// tick, which bounds the ring, since the ring holds every process of every tick until the run
/// is sealed.
const RUN_PROCESSES_MAX: u64 = WINDOWS_MAX * 4_096;
const PERSISTER_POLL_MS: u64 = 10;
const PERSISTER_POLL: Duration = Duration::from_millis(PERSISTER_POLL_MS);
/// Bounds each wait for the persister at three minutes, where the longest run of the process
/// capacity check took eight seconds on an M5 Pro, so a persister that hangs fails the run and a
/// slower Mac still finishes it.
const PERSISTER_POLLS_MAX: u64 = 18_000;
/// What reading the recording's list of windows may hold.
const CATALOG_BYTES_MAX: usize = 1 << 28;
const PROGRAMS: u64 = 500;
const APPS: u64 = 63;
const FORKS_PER_TICK: u64 = 37;
const COALITIONS: u64 = 715;
const CPUS: u32 = 18;
const INTERFACES: u64 = 34;
/// One process in this many refuses its usage, as those of other users do without root.
const REFUSED_EVERY: u64 = 3;
const CLOCK_PROCESS_CPUTIME_ID: c_int = 12;
const SIGKILL: i32 = 9;

const _: () = {
    // A run of the fewest windows may have the most processes a tick.
    assert!(WINDOWS_MIN * PROCESSES_MAX <= RUN_PROCESSES_MAX);
    // A run of the most windows may have the processes the capacity check gives one.
    assert!(WINDOWS_MAX * 4_096 <= RUN_PROCESSES_MAX);
    assert!(PERSISTER_POLLS_MAX * PERSISTER_POLL_MS == 3 * 60 * 1_000);
};

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
    sealing: Sealing,
}

/// What seals the run's windows.
#[derive(Debug, PartialEq)]
enum Sealing {
    /// The worker, in this process.
    Worker(Deriving),
    /// The persister, as a process of its own.
    Persister(PersisterOptions),
}

#[derive(Debug, PartialEq)]
struct PersisterOptions {
    path: PathBuf,
    /// What the process may hold.
    memory_bytes: u64,
    /// How many sealed windows into the run the persister is killed and started again, where
    /// it is.
    restart_after_windows: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Deriving {
    Apps,
    /// The App tables, as a persister restarted before each window derives them.
    Restarted,
    Nothing,
}

fn options_from(words: &[String]) -> Result<Options, String> {
    let mut windows = None;
    let mut processes = None;
    let mut derive = None;
    let mut persister = None;
    let mut memory_bytes = None;
    let mut restart_after_windows = None;
    for pair in words.chunks(2) {
        let [flag, value] = pair else {
            return Err(USAGE.to_owned());
        };
        match flag.as_str() {
            "--windows" => windows = Some(options_count(value, WINDOWS_MIN..=WINDOWS_MAX)?),
            "--processes" => {
                processes = Some(options_count(value, FORKS_PER_TICK + 1..=PROCESSES_MAX)?);
            }
            "--derive" => {
                derive = match value.as_str() {
                    "apps" => Some(Deriving::Apps),
                    "restarted" => Some(Deriving::Restarted),
                    "none" => Some(Deriving::Nothing),
                    _ => return Err(USAGE.to_owned()),
                }
            }
            "--persister" => persister = Some(PathBuf::from(value)),
            "--memory-bytes" => {
                let bounds = 1..=persistence::resources::WORKING_BYTES_MAX;
                memory_bytes = Some(options_count(value, bounds)?);
            }
            "--restart-after-windows" => {
                restart_after_windows = Some(options_count(value, 1..=WINDOWS_MAX - 1)?);
            }
            _ => return Err(USAGE.to_owned()),
        }
    }

    let sealing = match (derive, persister, memory_bytes) {
        (Some(derive), None, None) if restart_after_windows.is_none() => Sealing::Worker(derive),
        (None, Some(path), memory_bytes) => Sealing::Persister(PersisterOptions {
            path,
            memory_bytes: memory_bytes.unwrap_or(PERSISTER_MEMORY_BYTES),
            restart_after_windows,
        }),
        _ => return Err(USAGE.to_owned()),
    };
    let (Some(windows), Some(processes)) = (windows, processes) else {
        return Err(USAGE.to_owned());
    };
    if windows * processes > RUN_PROCESSES_MAX {
        return Err(format!(
            "{USAGE}\n{windows} windows of {processes} processes are more than \
            {RUN_PROCESSES_MAX} in all"
        ));
    }
    if restart_after_windows.is_some_and(|sealed| sealed >= windows) {
        return Err(format!(
            "{USAGE}\na restart leaves the persister started again none of {windows} windows"
        ));
    }

    Ok(Options {
        windows,
        processes,
        sealing,
    })
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
    drop(persistence::Store::open(&root)?);

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
            .and_then(|()| run_sealing(&mut writer, &root, options));
        let unlinked = collector::ring::unlink_generation(writer.name());
        sealed.and_then(|sealed| unlinked.map(|()| sealed).map_err(Into::into))
    });
    std::fs::remove_dir_all(&root)?;

    match sealed? {
        Sealed::Worker(deriving, marks) => report(options, deriving, &marks),
        Sealed::Persister { killed, finished } => {
            run_report_persister(options, killed.as_ref(), &finished);
            Ok(())
        }
    }
}

/// What a sealed run leaves to report.
enum Sealed {
    /// The CPU time and the clock at the start and the end of each window's derivation.
    Worker(Deriving, Vec<Mark>),
    /// What the persister cost that sealed the run to its end, and the one killed before it
    /// where the run killed one.
    Persister {
        killed: Option<PersisterRun>,
        finished: PersisterRun,
    },
}

/// Publishes every window of the run to the recording at `root` and has what `options` names
/// seal them. The run is marked finished once its last tick is published, so what seals it
/// seals every window and exits.
fn run_sealing(
    writer: &mut Writer,
    root: &Path,
    options: &Options,
) -> Result<Sealed, Box<dyn std::error::Error>> {
    let mut machine = Machine::start(writer, options.processes)?;
    let ticks = options.windows * TICKS_PER_WINDOW;
    match &options.sealing {
        Sealing::Worker(deriving) => {
            run_record(&mut machine, writer, 0..ticks)?;
            writer.finish();
            let marks = run_seal(root, *deriving)?;

            Ok(Sealed::Worker(*deriving, marks))
        }
        Sealing::Persister(persister) => {
            let run = writer.identity().ok_or("the ring names no run")?;
            let mut ticks_published = 0;
            let mut killed = None;
            if let Some(windows) = persister.restart_after_windows {
                // A persister seals a window once it reads a tick of the next, so the first
                // persister is given one tick past the windows it is to seal and no more.
                ticks_published = windows * TICKS_PER_WINDOW + 1;
                run_record(&mut machine, writer, 0..ticks_published)?;
                killed = Some(run_persister_killed(root, persister, &run, windows)?);
            }
            run_record(&mut machine, writer, ticks_published..ticks)?;
            writer.finish();
            let finished = run_persister(root, persister)?;
            run_persister_sealed(root, &run, options)?;

            Ok(Sealed::Persister { killed, finished })
        }
    }
}

/// Publishes `ticks` of the run, each a tick of `machine`.
fn run_record(
    machine: &mut Machine,
    writer: &mut Writer,
    ticks: Range<u64>,
) -> Result<(), Box<dyn std::error::Error>> {
    for tick in ticks {
        machine.publish(writer, tick)?;
    }

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
    let apps = persistence::sealed_apps::SealedApps::default();
    let inner: &dyn Derive = match deriving {
        Deriving::Apps => &apps,
        Deriving::Restarted => &Restarted,
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

/// What the persister was given and what it cost as a process of its own, from its start to
/// its exit.
#[derive(Debug, PartialEq)]
struct PersisterRun {
    memory_bytes: u64,
    cpu_ns: u64,
    elapsed_ns: u64,
    footprint_bytes_max: u64,
}

/// Seals the recording at `root` with the persister `options` names, which exits once it has
/// sealed every tick of the finished run, and returns what that process cost. A persister that
/// failed or held more than its memory is an error.
fn run_persister(
    root: &Path,
    options: &PersisterOptions,
) -> Result<PersisterRun, Box<dyn std::error::Error>> {
    let mut persister = PersisterChild::start(root, options)?;
    let Some(reaped) = run_persister_reap(&mut persister)? else {
        return Err(format!("persister still ran after {PERSISTER_POLLS_MAX} polls").into());
    };
    let elapsed = persister.started.elapsed();

    if !reaped.finished.successful() {
        return Err(run_persister_failure(&reaped, options.memory_bytes).into());
    }

    run_persister_cost(reaped, options.memory_bytes, elapsed)
}

/// Starts the persister `options` names on the recording at `root`, kills it once the recording
/// lists `windows` sealed windows of `run`, and returns what that process cost. A persister that
/// ended before the kill or held more than its memory is an error.
fn run_persister_killed(
    root: &Path,
    options: &PersisterOptions,
    run: &RunIdentity,
    windows: u64,
) -> Result<PersisterRun, Box<dyn std::error::Error>> {
    let mut persister = PersisterChild::start(root, options)?;
    match run_persister_killed_wait(&mut persister, root, run, windows)? {
        SealedWait::Sealed => {}
        SealedWait::Exited(reaped) => {
            return Err(run_persister_failure(&reaped, options.memory_bytes).into());
        }
        SealedWait::Unsealed => {
            return Err(format!(
                "persister sealed fewer than {windows} windows in {PERSISTER_POLLS_MAX} polls"
            )
            .into());
        }
    }
    persister.child.kill()?;
    let Some(reaped) = run_persister_reap(&mut persister)? else {
        return Err(format!("killed persister still ran after {PERSISTER_POLLS_MAX} polls").into());
    };
    let elapsed = persister.started.elapsed();

    if ExitStatus::from_raw(reaped.finished.status).signal() != Some(SIGKILL) {
        return Err(run_persister_failure(&reaped, options.memory_bytes).into());
    }

    run_persister_cost(reaped, options.memory_bytes, elapsed)
}

/// What a wait for the windows a running persister seals ended on.
enum SealedWait {
    /// The recording lists the windows.
    Sealed,
    /// The persister exited first.
    Exited(ReapedChild),
    /// Neither came within the polls.
    Unsealed,
}

/// Waits until the recording at `root` lists `windows` sealed windows of `run`, the persister
/// exits or the polls run out.
fn run_persister_killed_wait(
    persister: &mut PersisterChild,
    root: &Path,
    run: &RunIdentity,
    windows: u64,
) -> Result<SealedWait, Box<dyn std::error::Error>> {
    let budget = Arc::new(ByteBudget::new(CATALOG_BYTES_MAX));
    for _poll in 0..PERSISTER_POLLS_MAX {
        if let Some(reaped) = persister.reap()? {
            return Ok(SealedWait::Exited(reaped));
        }
        let listed = run_persister_killed_listed(root, run, &budget)?;
        if listed.is_some_and(|listed| listed >= windows) {
            return Ok(SealedWait::Sealed);
        }
        std::thread::sleep(PERSISTER_POLL);
    }

    Ok(SealedWait::Unsealed)
}

/// How many sealed windows of `run` the recording at `root` lists, or nothing where a window
/// the persister is publishing stands in the way, since a reader that meets one has learned
/// nothing of what the recording lists.
fn run_persister_killed_listed(
    root: &Path,
    run: &RunIdentity,
    budget: &Arc<ByteBudget>,
) -> std::io::Result<Option<u64>> {
    let listed = Catalog::open(root, budget)
        .and_then(|catalog| catalog.windows_of(run, &mut ReadWork::default()));
    match listed {
        Ok(windows) => Ok(Some(collector::u64_from_usize(windows.len()))),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error),
    }
}

/// Takes the status of `persister` once it exits, with what it cost, or nothing where it still
/// runs after the last poll.
fn run_persister_reap(persister: &mut PersisterChild) -> std::io::Result<Option<ReapedChild>> {
    for _poll in 0..PERSISTER_POLLS_MAX {
        let reaped = persister.reap()?;
        if reaped.is_some() {
            return Ok(reaped);
        }
        std::thread::sleep(PERSISTER_POLL);
    }

    Ok(None)
}

/// How a persister ended that should not have ended so, with the most it held where that
/// read, since a persister over its memory exits on its own.
fn run_persister_failure(reaped: &ReapedChild, memory_bytes: u64) -> String {
    let status = ExitStatus::from_raw(reaped.finished.status);
    match &reaped.footprint {
        Ok(cost) => format!(
            "persister {status} footprint_bytes_max={} memory_bytes={memory_bytes}",
            cost.footprint_bytes_max
        ),
        Err(error) => format!("persister {status} and its footprint did not read: {error}"),
    }
}

/// What the reaped persister cost over `elapsed`, refused where it held more than the
/// `memory_bytes` it was given. The persister's watchdog misses what a persister held that exits
/// between two of its checks.
fn run_persister_cost(
    reaped: ReapedChild,
    memory_bytes: u64,
    elapsed: Duration,
) -> Result<PersisterRun, Box<dyn std::error::Error>> {
    let footprint_bytes_max = reaped.footprint?.footprint_bytes_max;
    if footprint_bytes_max > memory_bytes {
        return Err(format!(
            "persister held {footprint_bytes_max} bytes of the {memory_bytes} it was given"
        )
        .into());
    }

    Ok(PersisterRun {
        memory_bytes,
        cpu_ns: reaped.finished.cpu_ns,
        elapsed_ns: u64::try_from(elapsed.as_nanos())?,
        footprint_bytes_max,
    })
}

/// A persister this run started, which nothing else waits for. Dropping one not yet reaped ends
/// it, so no path leaves it reading a ring the run has unlinked.
struct PersisterChild {
    child: Child,
    timebase: Timebase,
    started: Instant,
    /// Whether the persister's pid is still this run's to signal. It is until a reap takes the
    /// persister's status or fails, since a reap that fails may have taken the status first.
    unreaped: bool,
}

impl PersisterChild {
    fn start(root: &Path, options: &PersisterOptions) -> Result<Self, Box<dyn std::error::Error>> {
        let timebase = Timebase::read()?;
        let started = Instant::now();
        let path = &options.path;
        let child = Command::new(path)
            .arg("--recording")
            .arg(root)
            .arg("--memory-bytes")
            .arg(options.memory_bytes.to_string())
            .spawn()
            .map_err(|error| format!("persister {} did not start: {error}", path.display()))?;

        Ok(Self {
            child,
            timebase,
            started,
            unreaped: true,
        })
    }

    /// Takes the persister's status, with what it cost, if it has exited.
    fn reap(&mut self) -> std::io::Result<Option<ReapedChild>> {
        let reaped = collector::recording::child::reap_owned(&self.child, self.timebase);
        self.unreaped = matches!(reaped, Ok(None));

        reaped
    }
}

impl Drop for PersisterChild {
    fn drop(&mut self) {
        if self.unreaped {
            let ended = self.child.kill().and_then(|()| self.child.wait());
            if let Err(error) = ended {
                eprintln!(
                    "seal_bench: persister {} was not ended: {error}",
                    self.child.id()
                );
            }
        }
    }
}

/// Refuses the recording at `root` unless the windows it lists of `run` hold every tick the run
/// published and every process of each, since a persister can exit well and have left one
/// unsealed.
fn run_persister_sealed(
    root: &Path,
    run: &RunIdentity,
    options: &Options,
) -> Result<(), Box<dyn std::error::Error>> {
    let budget = Arc::new(ByteBudget::new(CATALOG_BYTES_MAX));
    let windows = Catalog::open(root, &budget)?.windows_of(run, &mut ReadWork::default())?;

    Ok(run_persister_sealed_windows(&windows, options)?)
}

/// Refuses `windows` unless together they hold every tick of the run, each window with a row for
/// every tick its range spans, a row for every process of each and no loss. A range alone
/// spans a tick the persister lost inside it.
fn run_persister_sealed_windows(windows: &[Manifest], options: &Options) -> Result<(), String> {
    let mut ticks_sealed = 0_u64;
    for manifest in windows {
        let window = manifest.identity.window;
        // The windows are what another process wrote, so a count that overflows is an error
        // here and never a panic.
        let ticks = manifest
            .publication_end
            .checked_sub(manifest.publication_first)
            .ok_or(format!("persister sealed window {window} with no range"))?;
        let processes = ticks
            .checked_mul(options.processes)
            .ok_or(format!("persister sealed window {window} of {ticks} ticks"))?;
        if manifest.losses != Losses::default() {
            return Err(format!(
                "persister sealed window {window} with {:?}",
                manifest.losses
            ));
        }
        for (table, rows) in [(Table::Ticks, ticks), (Table::Processes, processes)] {
            let sealed = run_persister_sealed_rows(manifest, table);
            if sealed != Some(rows) {
                return Err(format!(
                    "persister sealed window {window} with {sealed:?} rows of {} where the run \
                    published {rows}",
                    table.name()
                ));
            }
        }
        ticks_sealed = ticks_sealed.checked_add(ticks).ok_or(format!(
            "persister sealed more ticks than count at window {window}"
        ))?;
    }
    let ticks_published = options.windows * TICKS_PER_WINDOW;
    if ticks_sealed != ticks_published {
        return Err(format!(
            "persister sealed {ticks_sealed} of {ticks_published} ticks"
        ));
    }

    Ok(())
}

/// The rows the sealed window `manifest` says its `table` holds, if it lists that table.
fn run_persister_sealed_rows(manifest: &Manifest, table: Table) -> Option<u64> {
    manifest
        .tables
        .iter()
        .find(|sealed| sealed.name == table.name())
        .map(|sealed| sealed.rows)
}

/// Prints what the persister cost that sealed the run to its end, and on the same line what
/// the one killed before it cost, where the run killed one.
fn run_report_persister(options: &Options, killed: Option<&PersisterRun>, finished: &PersisterRun) {
    print!(
        "seal_bench windows={} processes={} memory_bytes={} persister_cpu_ms={} elapsed_ms={} \
        footprint_bytes_max={}",
        options.windows,
        options.processes,
        finished.memory_bytes,
        milliseconds(finished.cpu_ns),
        milliseconds(finished.elapsed_ns),
        finished.footprint_bytes_max,
    );
    if let Some(killed) = killed {
        print!(
            " killed_persister_cpu_ms={} killed_elapsed_ms={} killed_footprint_bytes_max={}",
            milliseconds(killed.cpu_ns),
            milliseconds(killed.elapsed_ns),
            killed.footprint_bytes_max,
        );
    }
    println!();
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

/// Derives through a derivation of its own each time.
struct Restarted;

impl Derive for Restarted {
    fn derive(
        &self,
        derivation: &Derivation<'_>,
    ) -> persistence::Result<Vec<persistence::TableManifest>> {
        persistence::sealed_apps::SealedApps::default().derive(derivation)
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
fn report(
    options: &Options,
    deriving: Deriving,
    marks: &[Mark],
) -> Result<(), Box<dyn std::error::Error>> {
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
        match deriving {
            Deriving::Apps => "apps",
            Deriving::Restarted => "restarted",
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

/// The awake clock at this process's start, as macOS reports it.
fn start_awake_ns() -> Result<u64, Box<dyn std::error::Error>> {
    let own =
        collector::processes::read_usage(Timebase::read()?, collector::processes::own_pid()?)?
            .map_err(|_| "this process's own start did not read")?;

    Ok(own.awake_ns_at_start)
}

/// The generated Mac: its live processes in increasing pid order, and what it reads each tick.
struct Machine {
    programs: Vec<ProgramReference>,
    live: Vec<Live>,
    ordinal_next: u64,
    coalitions: Vec<Coalition>,
    cpus: Vec<CPUTicks>,
    interfaces: Vec<Interface>,
    /// The awake clock at this process's start, which the first tick is stamped with.
    awake_ns_start: u64,
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
            awake_ns_start: start_awake_ns()?,
        };
        for _process in 0..processes {
            machine.fork()?;
        }

        Ok(machine)
    }

    /// The clocks of `tick`, a second after the tick before it. A persister takes a run's
    /// collector for gone once a tick's awake time is before the start of the process registered
    /// for the run, and then seals the run to its end however few ticks it holds, so the awake
    /// clock runs on from this process's start.
    fn publish_times(awake_ns_start: u64, tick: u64) -> ObservationTimes {
        ObservationTimes {
            wall_ns: 1_800_000_000_000_000_000 + tick * 1_000_000_000,
            continuous_ns: tick * 1_000_000_000,
            awake_ns: awake_ns_start + tick * 1_000_000_000,
        }
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
        let times = Self::publish_times(self.awake_ns_start, tick);
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
                sealing: Sealing::Worker(Deriving::Nothing),
            }),
            options
        );
    }

    #[test]
    fn reads_a_persister_and_its_memory() {
        // Goal: a run names the persister that seals it and the memory that persister is given.
        // Method: parse both in place of a derivation, at the most processes a run takes.
        let options = options_from(&words(
            "--windows 2 --processes 16384 --persister /build/persister --memory-bytes 268435456",
        ));

        assert_eq!(
            Ok(Options {
                windows: 2,
                processes: 16_384,
                sealing: Sealing::Persister(PersisterOptions {
                    path: PathBuf::from("/build/persister"),
                    memory_bytes: 268_435_456,
                    restart_after_windows: None,
                }),
            }),
            options
        );
    }

    #[test]
    fn reads_a_restart_of_the_persister() {
        // Goal: a run names how many sealed windows in it kills the persister and starts it
        // again. Method: parse a restart after the first of three windows.
        let options = options_from(&words(
            "--windows 3 --processes 12000 --persister /build/persister --memory-bytes 268435456 \
            --restart-after-windows 1",
        ));

        assert_eq!(
            Ok(Options {
                windows: 3,
                processes: 12_000,
                sealing: Sealing::Persister(PersisterOptions {
                    path: PathBuf::from("/build/persister"),
                    memory_bytes: 268_435_456,
                    restart_after_windows: Some(1),
                }),
            }),
            options
        );
    }

    #[test]
    fn refuses_a_restart_after_the_last_window() {
        // Goal: the persister started again has a window left to seal, or the run shows nothing
        // of a restart. Method: ask for a restart after both of two windows.
        let options = options_from(&words(
            "--windows 2 --processes 900 --persister /build/persister --memory-bytes 268435456 \
            --restart-after-windows 2",
        ));

        assert!(options.is_err());
    }

    #[test]
    fn refuses_a_restart_of_the_worker() {
        // Goal: only a persister is a process a run can kill. Method: name a restart beside a
        // derivation.
        let options = options_from(&words(
            "--windows 2 --processes 900 --derive apps --restart-after-windows 1",
        ));

        assert!(options.is_err());
    }

    #[test]
    fn refuses_a_persister_given_a_derivation() {
        // Goal: a persister derives as it ships, so a run names a derivation or a persister and
        // never both. Method: name both.
        let options = options_from(&words(
            "--windows 2 --processes 900 --derive apps --persister /build/persister \
            --memory-bytes 268435456",
        ));

        assert!(options.is_err());
    }

    #[test]
    fn gives_a_persister_the_memory_the_recorder_gives_it_where_a_run_names_none() {
        // Goal: a run holds its persister to what the recorder gives one, so a check that names
        // no memory cannot fall out of step with the recorder. Method: name a persister alone.
        let options = options_from(&words(
            "--windows 2 --processes 900 --persister /build/persister",
        ));

        assert_eq!(
            Ok(Options {
                windows: 2,
                processes: 900,
                sealing: Sealing::Persister(PersisterOptions {
                    path: PathBuf::from("/build/persister"),
                    memory_bytes: PERSISTER_MEMORY_BYTES,
                    restart_after_windows: None,
                }),
            }),
            options
        );
    }

    #[test]
    fn refuses_a_worker_given_memory() {
        // Goal: the memory is a persister's, since the worker seals inside the benchmark's own
        // process. Method: name memory beside a derivation.
        let options = options_from(&words(
            "--windows 2 --processes 900 --derive apps --memory-bytes 268435456",
        ));

        assert!(options.is_err());
    }

    #[test]
    fn takes_a_run_that_fills_the_ring_to_its_bound() {
        // Goal: the bound on the ring leaves the longest run 4,096 processes a tick, which
        // the process capacity check runs. Method: ask for ten windows of them.
        let options = options_from(&words("--windows 10 --processes 4096 --derive apps"));

        assert!(options.is_ok());
    }

    #[test]
    fn refuses_a_run_that_fills_the_ring_past_its_bound() {
        // Goal: more processes leave fewer windows, so the ring that holds every tick of the
        // run stays bounded. Method: ask for ten windows of one process more than fits.
        let options = options_from(&words("--windows 10 --processes 4097 --derive apps"));

        assert!(options.is_err());
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

    /// A persister reaped after it exited well, having held `footprint_bytes_max` at most.
    fn reaped(footprint_bytes_max: u64) -> ReapedChild {
        ReapedChild {
            finished: collector::recording::child::Finished {
                cpu_ns: 7,
                status: 0,
                resident_bytes_max: 0,
            },
            footprint: Ok(collector::recording::child::ChildCost {
                cpu_ns: 7,
                footprint_bytes: 0,
                footprint_bytes_max,
            }),
        }
    }

    #[test]
    fn costs_a_persister_that_held_all_it_was_given() {
        // Goal: the memory a persister is given is the most it may hold, so one that held all
        // of it passes. Method: cost a persister reaped at exactly its limit.
        let cost = run_persister_cost(reaped(1_000), 1_000, Duration::from_nanos(9));

        assert_eq!(
            Some(PersisterRun {
                memory_bytes: 1_000,
                cpu_ns: 7,
                elapsed_ns: 9,
                footprint_bytes_max: 1_000,
            }),
            cost.ok()
        );
    }

    #[test]
    fn refuses_a_persister_that_held_a_byte_more_than_it_was_given() {
        // Goal: a persister that exits between two checks of its own watchdog is still held to
        // its limit. Method: cost one that exited well a byte over.
        let cost = run_persister_cost(reaped(1_001), 1_000, Duration::from_nanos(9));

        assert!(cost.is_err());
    }

    #[test]
    fn refuses_a_persister_whose_footprint_did_not_read() {
        // Goal: a run never passes on a footprint it did not read. Method: cost a persister
        // that exited well and whose footprint macOS refused.
        let unread = ReapedChild {
            footprint: Err(std::io::Error::from(std::io::ErrorKind::NotFound)),
            ..reaped(0)
        };

        let cost = run_persister_cost(unread, 1_000, Duration::from_nanos(9));

        assert!(cost.is_err());
    }

    /// A sealed window of one run, with the rows its tables of ticks and of processes hold.
    fn sealed(window: u64, publications: Range<u64>, rows: [u64; 2]) -> Manifest {
        Manifest {
            identity: persistence::WindowIdentity {
                run: RunIdentity {
                    recording: collector::ring::Identity([1, 2]),
                    run: collector::ring::Identity([3, 4]),
                    boot: collector::ring::Identity([5, 6]),
                },
                window,
            },
            publication_first: publications.start,
            publication_end: publications.end,
            events_first: 0,
            events_end: 0,
            wall_ns: None,
            continuous_ns: None,
            awake_ns: None,
            losses: Losses::default(),
            data_directory: String::new(),
            tables: vec![
                persistence::TableManifest {
                    name: Table::Ticks.name(),
                    rows: rows[0],
                },
                persistence::TableManifest {
                    name: Table::Processes.name(),
                    rows: rows[1],
                },
            ],
            event_batches: 0,
        }
    }

    fn two_windows_of_900_processes() -> Options {
        options_from(&words("--windows 2 --processes 900 --derive none")).unwrap()
    }

    #[test]
    fn takes_windows_that_hold_every_tick_and_every_process() {
        // Goal: a run whose every tick and process was sealed passes. Method: two windows of 60
        // ticks, newest first as the catalog lists them, each with a row for every tick and for
        // every process of every tick.
        let windows = [
            sealed(1, 61..121, [60, 54_000]),
            sealed(0, 1..61, [60, 54_000]),
        ];

        let held = run_persister_sealed_windows(&windows, &two_windows_of_900_processes());

        assert_eq!(Ok(()), held);
    }

    #[test]
    fn refuses_a_window_that_lacks_a_tick_inside_its_range() {
        // Goal: a tick lost inside a window fails the run, although the window's range still
        // spans it. Method: a window of 60 publications whose table of ticks holds 59 rows.
        let windows = [
            sealed(1, 61..121, [59, 54_000]),
            sealed(0, 1..61, [60, 54_000]),
        ];

        let held = run_persister_sealed_windows(&windows, &two_windows_of_900_processes());

        assert!(held.is_err());
    }

    #[test]
    fn refuses_a_window_that_lacks_a_process_of_a_tick() {
        // Goal: a process lost from a tick fails the run. Method: a window whose table of
        // processes holds one row fewer than 60 ticks of 900 processes.
        let windows = [
            sealed(1, 61..121, [60, 54_000]),
            sealed(0, 1..61, [60, 53_999]),
        ];

        let held = run_persister_sealed_windows(&windows, &two_windows_of_900_processes());

        assert!(held.is_err());
    }

    #[test]
    fn refuses_a_window_sealed_with_a_loss() {
        // Goal: a window the persister sealed while saying it lost a tick fails the run. Method:
        // a window whose rows count but whose losses name one publication.
        let mut lossy = sealed(1, 61..121, [60, 54_000]);
        lossy.losses.publications = 1;
        let windows = [lossy, sealed(0, 1..61, [60, 54_000])];

        let held = run_persister_sealed_windows(&windows, &two_windows_of_900_processes());

        assert!(held.is_err());
    }

    #[test]
    fn refuses_windows_that_end_before_the_run_does() {
        // Goal: a persister that exited well with the last minute unsealed fails the run. Method:
        // the first window of a run of two, whole.
        let windows = [sealed(0, 1..61, [60, 54_000])];

        let held = run_persister_sealed_windows(&windows, &two_windows_of_900_processes());

        assert!(held.is_err());
    }

    #[test]
    fn stamps_no_tick_before_this_process_started() {
        // Goal: a persister takes a run's collector for gone once a tick's awake time is before
        // the start of the process registered for the run, and then seals the run to its end
        // however few ticks it holds. Method: compare the first tick's awake time with the start
        // macOS reports for this process.
        let own = collector::processes::read_usage(
            Timebase::read().unwrap(),
            collector::processes::own_pid().unwrap(),
        )
        .unwrap()
        .unwrap();

        let times = Machine::publish_times(start_awake_ns().unwrap(), 0);

        assert!(times.awake_ns >= own.awake_ns_at_start);
    }

    #[test]
    fn takes_the_median_below_the_middle_of_an_even_count() {
        // Goal: the median of the steady windows is one of them. Method: four values.
        let value = median([40, 10, 30, 20].into_iter());

        assert_eq!(20, value);
    }
}
