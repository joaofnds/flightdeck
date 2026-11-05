//! The whole worker ended at every change it makes to a recording, in a process of its own.

use super::super::super::{Outcome, run_paced};
use super::{Recording, recording_with_withheld_events};
use crate::sealed_apps::SealedApps;
use crate::store::Store;
use collector::recording::Registry;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// More changes than a run of a few minutes makes, so a worker that never stops fails the test.
const CHANGES_MAX: u64 = 1 << 14;

/// What the interposer exits with when it ends a worker.
const KILLED: i32 = 137;

/// The same bound the store's native children take, for a child that hangs.
const CHILD_WAIT_MAX: Duration = Duration::from_secs(180);

/// How many kills run at once. Each is a process or two, mostly waiting on the disk.
const KILLED_WORKERS: usize = 4;

/// How far apart the first kills of a check's pairs stand.
const KILLED_TWICE_FIRST_STRIDE: u64 = 8;

/// How many changes of each restart a check's pairs kill.
const KILLED_TWICE_SECONDS: u64 = 16;

const CHILD_TEST: &str = "worker::registered::tests::kill_tests::killed_worker_child";

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
    )
    .unwrap();

    assert!(matches!(outcome, Outcome::Complete));
}

/// One saved file by name, length and digest, so a difference reads in a line rather than bytes.
type FileDigest = (PathBuf, usize, u64);

/// What a run comes to, which a worker that was killed and restarted must come to as well.
#[derive(Debug, PartialEq)]
struct Converged {
    windows: Vec<Vec<FileDigest>>,
    terminals: Vec<Vec<FileDigest>>,
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
    let terminals = std::fs::read_dir(recording.root.join("recovery-data"))
        .unwrap()
        .map(|terminal| {
            let ready = terminal.unwrap().path().join("ready");
            converged_digests(Recording::saved_files_in(&ready))
        })
        .collect();

    Converged {
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
    let killed = killed_worker_run(recording, interposer, 0, after);

    assert!(!killed, "{after}: a worker that names no change was killed");
}

/// A finished run with one minute of events, whose worker saves it, certifies its end and retires
/// it. A later minute adds no kind of change, and each run of the worker costs its minutes again.
fn killed_worker_finished_run() -> Recording {
    let mut recording = Recording::new();
    recording.publish_events_in_minute(0, 2);
    recording.writer.finish();

    killed_worker_registered(recording)
}

/// A collector that died with events its tick never published, so its terminal holds rows.
fn killed_worker_crashed_run() -> Recording {
    killed_worker_registered(recording_with_withheld_events())
}

fn killed_worker_registered(recording: Recording) -> Recording {
    let registry = Registry {
        runs: vec![recording.registered()],
        evictions: 0,
    };
    registry.write(&recording.root).unwrap();

    recording
}

/// Runs the whole worker in a child that the interposer ends at change `number`, and says
/// whether it did, which it does not once the run makes fewer changes than that.
fn killed_worker_run(
    recording: &Recording,
    interposer: &Interposer,
    number: u64,
    after: &str,
) -> bool {
    let root = recording.root.canonicalize().unwrap();
    let log = recording.base.join("killed-worker.log");
    let output = std::fs::File::create(&log).unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", CHILD_TEST, "--nocapture"])
        .env("DYLD_INSERT_LIBRARIES", &interposer.library)
        .env("FLIGHTDECK_KILLED_WORKER_ROOT", &root)
        .env("FLIGHTDECK_KILL_ROOT", &root)
        .env("FLIGHTDECK_KILL_AT", number.to_string())
        .stdin(Stdio::null())
        .stdout(output.try_clone().unwrap())
        .stderr(output)
        .spawn()
        .unwrap();
    let status = child_wait(&mut child);

    if status.code() == Some(KILLED) {
        return true;
    }
    assert!(
        status.success(),
        "{after}, change {number}: {status:?} signal {:?}\n{}",
        status.signal(),
        std::fs::read_to_string(&log).unwrap()
    );
    false
}

#[test]
fn a_worker_killed_at_each_change_of_a_finished_run_comes_to_the_same_end() {
    // Goal: no change the worker makes to a recording leaves one that a restart cannot finish
    // the same way (P02.3).
    // Method: for each numbered change, end a worker there in its own process, restart it, and
    // compare what the run comes to with a worker that was never ended.
    killed_worker_converges(killed_worker_finished_run);
}

#[test]
fn a_worker_killed_at_each_change_of_a_crashed_run_comes_to_the_same_end() {
    // Goal: the same for a terminal that holds rows, which a finished run's does not (P02.3).
    // Method: as above, over a source with events its writer never published.
    killed_worker_converges(killed_worker_crashed_run);
}

fn killed_worker_converges(recording_new: fn() -> Recording) {
    let interposer = Interposer::build();
    let uninterrupted = {
        let recording = recording_new();
        killed_worker_finish(&recording, &interposer, "never killed");
        converged(&recording)
    };
    let next = AtomicU64::new(1);
    let changes = AtomicU64::new(0);

    std::thread::scope(|scope| {
        for _worker in 0..KILLED_WORKERS {
            scope.spawn(|| {
                for _attempt in 0..CHANGES_MAX {
                    let number = next.fetch_add(1, Ordering::Relaxed);
                    let recording = recording_new();
                    let killed = killed_worker_run(&recording, &interposer, number, "first run");
                    let after = format!("killed at change {number}");
                    if killed {
                        killed_worker_finish(&recording, &interposer, &after);
                    }

                    assert_eq!(converged(&recording), uninterrupted, "{after}");
                    if !killed {
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
    // Goal: the recovery of a killed worker can itself be killed (P02.3).
    // Method: end a worker at a first change, end its restart at a second, restart it again, and
    // compare with a worker that was never ended, over the pairs a check can afford. A restart
    // makes its recovery's own changes first, and past them it is a run the single kills cover.
    killed_twice_converges(KILLED_TWICE_FIRST_STRIDE, KILLED_TWICE_SECONDS);
}

#[cfg(feature = "kill-every-pair")]
#[test]
fn a_worker_killed_again_at_every_change_comes_to_the_same_end() {
    // Goal: the same over every pair of changes, which takes about an hour.
    // Method: as above with nothing left out. Run it with `--features kill-every-pair`.
    killed_twice_converges(1, CHANGES_MAX);
}

fn killed_twice_converges(first_stride: u64, seconds: u64) {
    let interposer = Interposer::build();
    let uninterrupted = {
        let recording = killed_worker_finished_run();
        killed_worker_finish(&recording, &interposer, "never killed");
        converged(&recording)
    };
    let next_first = AtomicU64::new(1);

    std::thread::scope(|scope| {
        for _worker in 0..KILLED_WORKERS {
            scope.spawn(|| {
                for _first in 0..CHANGES_MAX {
                    let first = next_first.fetch_add(first_stride, Ordering::Relaxed);
                    if !killed_twice_at(first, seconds, &interposer, &uninterrupted) {
                        return;
                    }
                }
            });
        }
    });
}

/// Tries the second kills after the first, and says whether the first kill was reached at all.
fn killed_twice_at(
    first: u64,
    seconds: u64,
    interposer: &Interposer,
    uninterrupted: &Converged,
) -> bool {
    for second in 1..=seconds {
        let recording = killed_worker_finished_run();
        if !killed_worker_run(&recording, interposer, first, "first run") {
            return false;
        }
        let after = format!("killed at change {first}");
        let killed = killed_worker_run(&recording, interposer, second, &after);
        let after = format!("{after}, then at change {second}");
        if killed {
            killed_worker_finish(&recording, interposer, &after);
        }

        assert_eq!(converged(&recording), *uninterrupted, "{after}");
        if !killed {
            return true;
        }
    }

    true
}
