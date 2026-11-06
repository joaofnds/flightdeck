//! The whole worker ended at every change it makes to a recording, in a process of its own.

use super::super::super::{Outcome, run_paced};
use super::Recording;
use crate::canonical::CanonicalMetadata;
use crate::canonical::metadata::{Body, CanonicalLoss, EndpointEvidence, Lifecycle};
use crate::sealed_apps::SealedApps;
use crate::store::Store;
use crate::transaction::{CanonicalKind, Predecessor};
use collector::recording::Registry;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// More changes than a run of a few minutes makes, so a worker that never stops fails the test.
const CHANGES_MAX: u64 = 1 << 14;

/// What the interposer exits with when it ends a worker.
const KILLED: i32 = 137;

/// What a worker's process exits with when the worker stops with an error, which tells that from
/// a panic.
const STOPPED: i32 = 138;

/// The same bound the store's native children take, for a child that hangs.
const CHILD_WAIT_MAX: Duration = Duration::from_secs(180);

/// How many kills run at once. Each is a process or two, mostly waiting on the disk.
const KILLED_WORKERS: usize = 4;

/// How far apart the first kills of a check's pairs stand.
const KILLED_TWICE_FIRST_STRIDE: u64 = 16;

/// How many changes of each restart a check's pairs end.
const KILLED_TWICE_SECONDS: u64 = 12;

/// More events than one batch holds, so a crashed run's terminal holds two batches.
const CRASHED_RUN_EVENTS: usize = 16_390;

const WORKER_CHILD: KilledChild = KilledChild {
    test: "worker::registered::tests::kill_tests::killed_worker_child",
    root_variable: "FLIGHTDECK_KILLED_WORKER_ROOT",
};

const RETIREMENT_CHILD: KilledChild = KilledChild {
    test: "worker::registered::tests::kill_tests::killed_retirement_child",
    root_variable: "FLIGHTDECK_KILLED_RETIREMENT_ROOT",
};

/// A test that runs in a process of its own when a parent names a recording in its variable.
struct KilledChild {
    test: &'static str,
    root_variable: &'static str,
}

/// Holds these tests to one at a time, so the processes they start never crowd the native tests
/// beside them, which wait on the disk under bounds of their own.
static ONE_KILL_TEST: Mutex<()> = Mutex::new(());

fn one_kill_test() -> MutexGuard<'static, ()> {
    ONE_KILL_TEST.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How a test ends a worker's process, at the numbered change.
#[derive(Clone, Copy, Debug)]
enum Fault {
    Nothing,
    Kill(u64),
    /// Ends the worker just before it creates a file of this name for this many-th time.
    KillCreating(&'static str, u64),
    /// Refuses this change and every one after it, as a full disk does.
    Refuse(u64),
}

/// The interposer, built for the test that asks and removed with it.
struct Interposer {
    directory: PathBuf,
    library: PathBuf,
}

impl Interposer {
    fn build() -> Self {
        let directory = std::env::temp_dir().join(format!(
            "fd-kill-{}-{}",
            std::process::id(),
            super::NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&directory).unwrap();
        let library = directory.join("kill.dylib");
        let source =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("src/worker/registered/kill_fault.rs");
        let log = directory.join("rustc.log");
        let mut build = Command::new("rustc")
            .args(["--edition=2024", "--crate-type=cdylib", "--deny=warnings"])
            .arg(source)
            .arg("-o")
            .arg(&library)
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(&log).unwrap())
            .spawn()
            .unwrap();
        let status = child_wait(&mut build);
        assert!(
            status.success(),
            "{}",
            std::fs::read_to_string(&log).unwrap()
        );

        Self { directory, library }
    }
}

impl Drop for Interposer {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.directory).unwrap();
    }
}

fn child_wait(child: &mut Child) -> std::process::ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if started.elapsed() >= CHILD_WAIT_MAX {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("a worker child outlived its bound");
        }
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn killed_worker_child() {
    // Goal: give the kill tests a worker in a process of its own that they can end.
    // Method: run only when a parent names a recording, which no ordinary test run does.
    let Some(root) = std::env::var_os("FLIGHTDECK_KILLED_WORKER_ROOT") else {
        return;
    };

    let outcome = run_paced(
        Path::new(&root),
        crate::resources::WORKING_BYTES_MAX,
        &SealedApps::default(),
        &mut |_pause| panic!("a finished run gives the worker nothing to wait for"),
        None,
    );

    match outcome {
        Ok(outcome) => assert!(matches!(outcome, Outcome::Complete)),
        Err(error) => {
            eprintln!("the worker stopped: {error}");
            std::process::exit(STOPPED);
        }
    }
}

/// One saved file by name, length and digest, so a difference reads in a line rather than bytes.
type FileDigest = (PathBuf, usize, u64);

/// What one saved proof claims, leaving out what names the recording or the attempt.
#[derive(Debug, PartialEq)]
struct ProofClaim {
    kind: CanonicalKind,
    window: u64,
    canonical_origin: u64,
    original_end: u64,
    evidence: EndpointEvidence,
    lifecycle: Lifecycle,
    loss: CanonicalLoss,
    manifest_losses: Option<crate::Losses>,
}

impl ProofClaim {
    fn read(ready: &Path) -> Self {
        let mut file = std::fs::File::open(ready.join("metadata")).unwrap();
        let metadata = CanonicalMetadata::read(&mut file).unwrap();
        let mut loss = metadata.loss;
        if loss.inherited.predecessor != Predecessor::Genesis {
            // The predecessor names its attempt, which differs between recordings. Its ends do not.
            loss.inherited.predecessor = Predecessor::Genesis;
        }
        let manifest_losses = match metadata.body {
            Body::Ordinary(manifest) => Some(manifest.losses),
            _ => None,
        };

        Self {
            kind: metadata.kind,
            window: metadata.window,
            canonical_origin: metadata.canonical_origin,
            original_end: metadata.original_end,
            evidence: metadata.evidence,
            lifecycle: metadata.lifecycle,
            loss,
            manifest_losses,
        }
    }
}

/// What a run comes to, which a worker that was killed and restarted must come to as well.
#[derive(Debug, PartialEq)]
struct Converged {
    windows: Vec<Vec<FileDigest>>,
    terminals: Vec<Vec<FileDigest>>,
    claims: Vec<ProofClaim>,
    publication_end: u64,
    event_end: u64,
    complete: bool,
    retired: bool,
    interrupted: bool,
}

fn converged(recording: &Recording) -> Converged {
    let run = recording.registered();
    let store = Store::open(&recording.root).unwrap();
    let frontier = recording.frontier(&store).unwrap();
    let terminal_directories: Vec<PathBuf> =
        std::fs::read_dir(recording.root.join("recovery-data"))
            .unwrap()
            .map(|terminal| terminal.unwrap().path().join("ready"))
            .collect();
    let terminals = terminal_directories
        .iter()
        .map(|ready| converged_digests(Recording::saved_files_in(ready)))
        .collect();
    let mut claims: Vec<ProofClaim> = recording
        .saved_windows()
        .iter()
        .map(|window| window.join("ready"))
        .chain(terminal_directories.iter().cloned())
        .map(|ready| ProofClaim::read(&ready))
        .collect();
    claims.sort_by_key(|claim| {
        (
            claim.canonical_origin,
            claim.kind == CanonicalKind::Terminal,
        )
    });

    Converged {
        claims,
        windows: recording
            .saved_files()
            .into_iter()
            .map(converged_digests)
            .collect(),
        terminals,
        publication_end: frontier.publication_end(),
        event_end: frontier.event_end(),
        complete: frontier.complete(),
        retired: Registry::completed(&recording.root, &run.identity, &run.ring_name).unwrap(),
        interrupted: store.interrupted().unwrap().is_some(),
    }
}

fn converged_digests(files: Vec<(PathBuf, Vec<u8>)>) -> Vec<FileDigest> {
    let mut digests: Vec<FileDigest> = files
        .into_iter()
        .map(|(name, bytes)| {
            let mut hasher = DefaultHasher::new();
            bytes.hash(&mut hasher);
            (name, bytes.len(), hasher.finish())
        })
        .collect();
    digests.sort();

    digests
}

/// Runs the worker to its end in a child no kill reaches, since no change is number zero. `after`
/// says what came before it, the first thing a failed restart has to tell.
fn killed_worker_finish(recording: &Recording, interposer: &Interposer, after: &str) {
    killed_worker_run(recording, interposer, Fault::Nothing, after);
}

/// A finished run with one minute of events, whose worker saves it, certifies its end and retires
/// it. A later minute adds no kind of change, and each run of the worker costs its minutes again.
fn killed_worker_finished_run() -> Recording {
    let mut recording = Recording::new();
    recording.publish_events_in_minute(0, 2);
    recording.writer.finish();

    killed_worker_registered(recording)
}

/// A collector that died having published one tick and written the events of the next, so its run
/// holds a window and a terminal of two batches.
fn killed_worker_crashed_run() -> Recording {
    let mut recording =
        Recording::with_event_capacity(collector::u64_from_usize(CRASHED_RUN_EVENTS) + 2);
    recording.publish_events_in_minute(0, 2);
    recording.publish_events_in_minute(0, CRASHED_RUN_EVENTS);
    recording.withhold_publications_past(1);

    killed_worker_registered(recording)
}

/// A collector that overran its ring before it finished, so the minute's proof counts what the ring
/// overwrote, both publications and events.
fn killed_worker_lagging_run() -> Recording {
    let mut recording = Recording::new();
    for _tick in 0..10 {
        let count = recording.writer.event_room().min(3);
        recording.publish_events_in_minute(0, count);
    }
    recording.writer.finish();

    killed_worker_registered(recording)
}

fn killed_worker_registered(recording: Recording) -> Recording {
    let registry = Registry {
        runs: vec![recording.registered()],
        evictions: 0,
    };
    registry.write(&recording.root).unwrap();

    recording
}

/// Runs the whole worker in a child with the fault, and says whether the fault ended it, which it
/// does not once the run makes fewer changes than that.
fn killed_worker_run(
    recording: &Recording,
    interposer: &Interposer,
    fault: Fault,
    after: &str,
) -> bool {
    killed_child_run(&WORKER_CHILD, recording, interposer, fault, after)
}

fn killed_child_run(
    child: &KilledChild,
    recording: &Recording,
    interposer: &Interposer,
    fault: Fault,
    after: &str,
) -> bool {
    let root = recording.root.canonicalize().unwrap();
    let log = recording.base.join("killed-worker.log");
    let output = std::fs::File::create(&log).unwrap();
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", child.test, "--nocapture"])
        .env("DYLD_INSERT_LIBRARIES", &interposer.library)
        .env(child.root_variable, &root)
        .env("FLIGHTDECK_KILL_ROOT", &root)
        .stdin(Stdio::null())
        .stdout(output.try_clone().unwrap())
        .stderr(output);
    let expected = match fault {
        Fault::Nothing => None,
        Fault::Kill(number) => {
            command.env("FLIGHTDECK_KILL_AT", number.to_string());
            Some(KILLED)
        }
        Fault::KillCreating(name, number) => {
            command
                .env("FLIGHTDECK_KILL_AT", number.to_string())
                .env("FLIGHTDECK_KILL_CREATING", name);
            Some(KILLED)
        }
        Fault::Refuse(number) => {
            command
                .env("FLIGHTDECK_KILL_AT", number.to_string())
                .env("FLIGHTDECK_KILL_REFUSES", "1");
            Some(STOPPED)
        }
    };
    let mut child = command.spawn().unwrap();
    let status = child_wait(&mut child);

    if expected.is_some() && status.code() == expected {
        return true;
    }
    assert!(
        status.success(),
        "{after}: {status:?} signal {:?}\n{}",
        status.signal(),
        std::fs::read_to_string(&log).unwrap()
    );
    false
}

#[test]
fn killed_retirement_child() {
    // Goal: give the kill tests passes of physical removal in a process of their own to end.
    // Method: run only when a parent names a recording, which no ordinary test run does.
    let Some(root) = std::env::var_os(RETIREMENT_CHILD.root_variable) else {
        return;
    };

    match killed_retirement_passes(Path::new(&root)) {
        Ok(()) => {}
        Err(error) => {
            eprintln!("the pass stopped: {error}");
            std::process::exit(STOPPED);
        }
    }
}

/// Passes at a clock past everything saved until one removes nothing.
fn killed_retirement_passes(root: &Path) -> std::io::Result<()> {
    let store = Store::open(root)?;
    let budget = Arc::new(crate::resources::ByteBudget::new(256 * 1024 * 1024));
    let now = crate::retention::ClockReading {
        boot: collector::ring::Identity([9, 9]),
        continuous_ns: 1 << 62,
        wall_ns: 1 << 62,
    };
    for _pass in 0..16 {
        if store.retire_expired(&now, &[], &budget)? == 0 {
            return Ok(());
        }
    }
    panic!("removal did not finish");
}

/// A run the worker has saved whole and retired, a window and a terminal, with nothing left of it
/// but what a pass removes.
fn killed_retirement_recording(interposer: &Interposer) -> Recording {
    let recording = killed_worker_finished_run();
    killed_worker_finish(&recording, interposer, "the save");

    recording
}

/// Every path in the recording, which removal leaves the same whichever way it was ended. A retired
/// run's receipt takes its name from the ring, which each recording names at random.
fn killed_retirement_paths(root: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut directories = vec![root.to_path_buf()];
    for _directory in 0..1 << 16 {
        let Some(directory) = directories.pop() else {
            paths.sort();
            return paths;
        };
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                directories.push(path.clone());
            }
            let relative = path.strip_prefix(root).unwrap();
            if relative.starts_with("retired") && !path.is_dir() {
                paths.push(PathBuf::from("retired/receipt"));
            } else {
                paths.push(relative.to_path_buf());
            }
        }
    }
    panic!("a recording of more directories than this test saves");
}

/// What a reader meets where a pass ended: what the run saved, or less of it, or a wait. A window
/// with only some of its names is one a reader could misread, so only a Busy may leave one.
fn killed_retirement_reader(recording: &Recording, saved_windows: usize) {
    let budget = Arc::new(crate::resources::ByteBudget::new(256 * 1024 * 1024));
    let catalog = crate::canonical::catalog::Catalog::open(&recording.root, &budget).unwrap();
    let run = recording.registered().identity;
    match catalog.windows_of(&run, &mut crate::canonical::ReadWork::default()) {
        Ok(windows) => assert!(windows.len() <= saved_windows),
        Err(error) => assert_eq!(error.kind(), std::io::ErrorKind::WouldBlock),
    }

    let names_of_readings = killed_retirement_names(&recording.root, "observations-known");
    let names_of_windows = killed_retirement_names(&recording.root, "ordinary");
    if names_of_readings != names_of_windows {
        let state = std::fs::read(recording.root.join("namespace-state")).unwrap();
        assert!(matches!(
            collector::recording::bootstrap::codec::decode_state(&state, run.recording).unwrap(),
            collector::recording::bootstrap::codec::NamespaceState::Busy { .. }
        ));
    }
}

/// How many names a namespace holds, whichever nodes route to them.
fn killed_retirement_names(root: &Path, namespace: &str) -> usize {
    killed_retirement_paths(&root.join(namespace))
        .iter()
        .filter(|path| !root.join(namespace).join(path).is_dir())
        .count()
}

fn killed_retirement_converges(stride: u64, fault: fn(u64) -> Fault) {
    let interposer = Interposer::build();
    let (uninterrupted, saved_windows) = {
        let recording = killed_retirement_recording(&interposer);
        let saved_windows = recording.saved_windows().len();
        killed_child_run(
            &RETIREMENT_CHILD,
            &recording,
            &interposer,
            Fault::Nothing,
            "never ended",
        );
        (killed_retirement_paths(&recording.root), saved_windows)
    };
    assert!(saved_windows > 0);
    let saved = killed_retirement_recording(&interposer);
    // One reading a window, so a window's names are whole when these two counts agree.
    assert_eq!(
        killed_retirement_names(&saved.root, "observations-known"),
        killed_retirement_names(&saved.root, "ordinary")
    );
    drop(saved);
    let next = AtomicU64::new(1);
    let changes = AtomicU64::new(0);

    std::thread::scope(|scope| {
        for _worker in 0..KILLED_WORKERS {
            scope.spawn(|| {
                for _attempt in 0..CHANGES_MAX {
                    let number = next.fetch_add(stride, Ordering::Relaxed);
                    let recording = killed_retirement_recording(&interposer);
                    let ended = killed_child_run(
                        &RETIREMENT_CHILD,
                        &recording,
                        &interposer,
                        fault(number),
                        "first",
                    );
                    let after = format!("ended at change {number}");
                    if ended {
                        killed_retirement_reader(&recording, saved_windows);
                    }
                    killed_child_run(
                        &RETIREMENT_CHILD,
                        &recording,
                        &interposer,
                        Fault::Nothing,
                        &after,
                    );

                    assert_eq!(
                        killed_retirement_paths(&recording.root),
                        uninterrupted,
                        "{after}"
                    );
                    if !ended {
                        return;
                    }
                    changes.fetch_max(number, Ordering::Relaxed);
                }
            });
        }
    });

    let changes = changes.into_inner();
    assert!(changes > 0, "no change was numbered");
    assert!(changes < CHANGES_MAX, "removal kept making changes");
}

#[test]
fn a_pass_killed_at_each_change_of_a_removal_comes_to_the_same_end() {
    // Goal: no change a removal makes leaves a recording that a reader misreads or a restart cannot
    // finish the same way.
    // Method: for each numbered change, end a pass there in its own process, read the run as a
    // reader would, restart, and compare what is left with a pass that was never ended.
    let _one = one_kill_test();
    killed_retirement_converges(1, Fault::Kill);
}

#[test]
fn a_pass_refused_a_change_comes_to_the_same_end_once_the_disk_has_room() {
    // Goal: a change the file system refuses is an error, never a panic or a state a restart cannot
    // finish.
    // Method: refuse each fourth change and all after it, then restart with nothing refused.
    let _one = one_kill_test();
    killed_retirement_converges(4, Fault::Refuse);
}

#[test]
fn a_worker_killed_at_each_change_of_a_finished_run_comes_to_the_same_end() {
    // Goal: no change the worker makes to a recording leaves one that a restart cannot finish
    // the same way.
    // Method: for each numbered change, end a worker there in its own process, restart it, and
    // compare what the run comes to with a worker that was never ended.
    let _one = one_kill_test();
    killed_worker_converges(killed_worker_finished_run, 1, Fault::Kill);
}

#[test]
fn a_worker_killed_during_a_crashed_run_comes_to_the_same_end() {
    // Goal: the same for a dead writer's window and a terminal of two batches, which a finished
    // run does not reach.
    // Method: as above at every third change, since much of the run is what the finished run makes.
    let _one = one_kill_test();
    killed_worker_converges(killed_worker_crashed_run, 3, Fault::Kill);
}

#[test]
fn a_worker_killed_during_a_run_that_overran_its_ring_comes_to_the_same_end() {
    // Goal: what the ring overwrote is counted once whatever a restart finds saved.
    // Method: as above at every third change, over a run with publications and events lost.
    let _one = one_kill_test();
    killed_worker_converges(killed_worker_lagging_run, 3, Fault::Kill);
}

#[test]
fn a_worker_refused_a_change_comes_to_the_same_end_once_the_disk_has_room() {
    // Goal: a change the file system refuses is an error, never a panic or a saved state a restart
    // cannot finish.
    // Method: refuse each fourth change and all after it, then restart with nothing refused.
    let _one = one_kill_test();
    killed_worker_converges(killed_worker_finished_run, 4, Fault::Refuse);
}

fn killed_worker_converges(recording_new: fn() -> Recording, stride: u64, fault: fn(u64) -> Fault) {
    let interposer = Interposer::build();
    let uninterrupted = {
        let recording = recording_new();
        killed_worker_finish(&recording, &interposer, "never ended");
        converged(&recording)
    };
    let next = AtomicU64::new(1);
    let changes = AtomicU64::new(0);

    std::thread::scope(|scope| {
        for _worker in 0..KILLED_WORKERS {
            scope.spawn(|| {
                for _attempt in 0..CHANGES_MAX {
                    let number = next.fetch_add(stride, Ordering::Relaxed);
                    let recording = recording_new();
                    let ended = killed_worker_run(&recording, &interposer, fault(number), "first");
                    let after = format!("ended at change {number}");
                    if ended {
                        killed_worker_finish(&recording, &interposer, &after);
                    }

                    assert_eq!(converged(&recording), uninterrupted, "{after}");
                    if !ended {
                        return;
                    }
                    changes.fetch_max(number, Ordering::Relaxed);
                }
            });
        }
    });

    let changes = changes.into_inner();
    assert!(changes > 0, "no change was numbered");
    assert!(changes < CHANGES_MAX, "the worker kept making changes");
}

#[test]
fn a_worker_killed_again_while_it_recovers_comes_to_the_same_end() {
    // Goal: the recovery of a killed worker can itself be killed.
    // Method: end a worker at a first change, end its restart at a second, restart it again, and
    // compare with a worker that was never ended, over the pairs a check can afford. A restart
    // makes its recovery's own changes first, and past them it is a run the single kills cover.
    let _one = one_kill_test();
    let pairs = killed_twice_converges(
        killed_worker_finished_run,
        KILLED_TWICE_FIRST_STRIDE,
        KILLED_TWICE_SECONDS,
        Fault::Kill,
    );

    assert!(pairs > 0, "no restart was ended");
}

#[test]
fn a_worker_killed_again_while_it_gives_up_a_terminal_comes_to_the_same_end() {
    // Goal: the same over a terminal of two batches, which a restart removes and writes again.
    // Method: as above, over a crashed run.
    let _one = one_kill_test();
    let pairs = killed_twice_converges(
        killed_worker_crashed_run,
        KILLED_TWICE_FIRST_STRIDE,
        KILLED_TWICE_SECONDS,
        Fault::Kill,
    );

    assert!(pairs > 0, "no restart was ended");
}

#[test]
fn a_worker_killed_again_while_it_removes_a_terminals_batches_comes_to_the_same_end() {
    // Goal: a terminal's batches go newest first, so a process that ends between two removals
    // leaves the order the writer leaves, which the sampled kills above seldom reach.
    // Method: end a worker just before it creates the terminal's proof, the only time both its
    // batches are whole with nothing after them, then end its restart at each of the first
    // changes, which take in both removals.
    let _one = one_kill_test();
    let interposer = Interposer::build();
    let uninterrupted = {
        let recording = killed_worker_crashed_run();
        killed_worker_finish(&recording, &interposer, "never ended");
        converged(&recording)
    };
    let pair = KilledTwice {
        recording_new: killed_worker_crashed_run,
        first: Fault::KillCreating("metadata", 2),
        seconds: 6,
        second: Fault::Kill,
        interposer: &interposer,
        uninterrupted: &uninterrupted,
    };

    let ended = pair.converges();

    assert_eq!(ended, Some(6));
}

#[test]
fn a_worker_refused_a_change_while_it_recovers_comes_to_the_same_end() {
    // Goal: a failure during recovery removes nothing a later restart needs, so it finishes once
    // the disk has room.
    // Method: end a worker at a first change, refuse its restart from a second on, restart it with
    // nothing refused, and compare with a worker that was never ended.
    let _one = one_kill_test();
    let pairs = killed_twice_converges(
        killed_worker_finished_run,
        KILLED_TWICE_FIRST_STRIDE,
        KILLED_TWICE_SECONDS,
        Fault::Refuse,
    );

    assert!(pairs > 0, "no restart was refused");
}

#[cfg(feature = "kill-every-pair")]
#[test]
fn a_worker_ended_twice_at_every_change_comes_to_the_same_end() {
    // Goal: the same over every pair of changes of each kind of run, which takes hours.
    // Method: as above with nothing left out. Run it with `--features kill-every-pair`.
    let _one = one_kill_test();
    killed_twice_converges(killed_worker_finished_run, 1, CHANGES_MAX, Fault::Kill);
    killed_twice_converges(killed_worker_crashed_run, 1, CHANGES_MAX, Fault::Kill);
    killed_twice_converges(killed_worker_lagging_run, 1, CHANGES_MAX, Fault::Kill);
    killed_twice_converges(killed_worker_finished_run, 1, CHANGES_MAX, Fault::Refuse);
}

/// Ends a worker at first changes `first_stride` apart, faults its restart at each of `seconds`,
/// and says how many restarts the fault reached.
fn killed_twice_converges(
    recording_new: fn() -> Recording,
    first_stride: u64,
    seconds: u64,
    second: fn(u64) -> Fault,
) -> u64 {
    let interposer = Interposer::build();
    let uninterrupted = {
        let recording = recording_new();
        killed_worker_finish(&recording, &interposer, "never ended");
        converged(&recording)
    };
    let next_first = AtomicU64::new(1);
    let pairs = AtomicU64::new(0);

    std::thread::scope(|scope| {
        for _worker in 0..KILLED_WORKERS {
            scope.spawn(|| {
                for _first in 0..CHANGES_MAX {
                    let first = next_first.fetch_add(first_stride, Ordering::Relaxed);
                    let pair = KilledTwice {
                        recording_new,
                        first: Fault::Kill(first),
                        seconds,
                        second,
                        interposer: &interposer,
                        uninterrupted: &uninterrupted,
                    };
                    let Some(ended) = pair.converges() else {
                        return;
                    };
                    pairs.fetch_add(ended, Ordering::Relaxed);
                }
            });
        }
    });

    pairs.into_inner()
}

/// One first kill and the faults of the restart after it.
struct KilledTwice<'test> {
    recording_new: fn() -> Recording,
    first: Fault,
    seconds: u64,
    second: fn(u64) -> Fault,
    interposer: &'test Interposer,
    uninterrupted: &'test Converged,
}

impl KilledTwice<'_> {
    /// Tries the faults after the first kill, and says how many of them reached the restart, or
    /// none at all when the first kill was not reached.
    fn converges(&self) -> Option<u64> {
        let mut ended = 0;
        for second in 1..=self.seconds {
            let recording = (self.recording_new)();
            if !killed_worker_run(&recording, self.interposer, self.first, "first") {
                return None;
            }
            let after = format!("ended by {:?}", self.first);
            let faulted = (self.second)(second);
            let reached = killed_worker_run(&recording, self.interposer, faulted, &after);
            let after = format!("{after}, then at change {second}");
            if reached {
                killed_worker_finish(&recording, self.interposer, &after);
            }

            assert_eq!(converged(&recording), *self.uninterrupted, "{after}");
            if !reached {
                return Some(ended);
            }
            ended += 1;
        }

        Some(ended)
    }
}
