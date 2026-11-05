//! Opt-in native event-tail fidelity and the unchanged 256 MiB persister ceiling.
use std::fs::{self, File};
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use collector::recording::private::{FileLock, PrivateDirectory};
use collector::recording::{RegisteredRun, Registration, Registry, bootstrap};
use collector::ring::{self, IdentityObservation, IdentityPublication, ObservationTimes};
use persistence::canonical::ReadWork;
use persistence::resources::ByteBudget;

static NEXT: AtomicU64 = AtomicU64::new(0);
const ROWS_MAX: u64 = 1_966_080;
const MEMORY_BYTES: usize = 256 * 1024 * 1024;
const PROGRAM_BYTES: usize = ring::PROGRAM_PATH_BYTES_MAX;

#[path = "source.rs"]
mod source;

#[path = "managed.rs"]
mod managed;

struct Candidate {
    root: PathBuf,
    writer: Option<ring::Writer>,
    source: String,
    sources: Vec<String>,
    run: ring::RunIdentity,
    child: Option<Child>,
    managed: Option<managed::Recorder>,
}

#[test]
#[ignore = "opt-in native managed recovery retry-phase and cleanup proof"]
fn managed_recovery_observes_the_full_retry_phase_before_exact_terminal_commit() {
    // Goal: the managed timer includes a full launch backoff and ends only at canonical recovery.
    // Method: restore an owned withheld helper just after a five-second retry over2,048events.
    observe_managed(2_048, true);
}

#[test]
#[ignore = "opt-in maximum ordinary full-byte managed recovery deadline"]
fn managed_maximum_ordinary_tail_is_canonical_within_thirty_seconds() {
    // Goal: maximum ordinary recovery includes detection/backoff/launch in its30second gate.
    // Method: clear a late-phase helper failure and verify all retained bytes before the timer.
    observe_managed(ROWS_MAX, false);
}

#[test]
#[ignore = "opt-in maximum terminal full-byte managed recovery deadline"]
fn managed_maximum_terminal_tail_is_canonical_within_thirty_seconds() {
    // Goal: zero-publication recovery meets the same end-to-end deadline at admitted capacity.
    // Method: clear a late-phase helper failure and verify canonical terminal rows and bytes.
    observe_managed(ROWS_MAX, true);
}

fn observe_managed(rows: u64, terminal: bool) {
    let mut candidate = Candidate::new(rows);
    candidate.publish(rows);
    if terminal {
        candidate.withhold_publications(rows);
    }
    let binary = std::env::var_os("FLIGHTDECK_PERSISTER_BINARY").map_or_else(
        || PathBuf::from(env!("CARGO_BIN_EXE_persister")),
        PathBuf::from,
    );
    candidate.managed = Some(managed::Recorder::new(&candidate.root, &binary).unwrap());
    let recorder = candidate.managed.as_mut().unwrap();
    recorder.launch().unwrap();
    let result = recorder.recover(&candidate.run, &candidate.source);
    if result.is_err() {
        recorder.report().unwrap();
    }
    let recovery = result.unwrap();
    recorder.stop().unwrap();
    // Reported before the assertion, since its panic removes the root that holds this log.
    recorder.report().unwrap();
    assert!(recorder.exit_status().unwrap().unwrap().success());
    eprintln!(
        "managed_recovery rows={rows} terminal={terminal} clear_ns={} restored_ns={} due_ns={} \
        worker_seen_ns={} canonical_ns={} elapsed_ns={} worker_previous_poll_ns={} \
        worker_poll_interval_ns={} worker={:?}",
        recovery.cause_clear_ns,
        recovery.restored_ns,
        recovery.due_ns,
        recovery.worker_seen_ns,
        recovery.canonical_ns,
        recovery.canonical_ns - recovery.cause_clear_ns,
        recovery.worker_previous_poll_ns,
        recovery.worker_seen_ns - recovery.worker_previous_poll_ns,
        recovery.worker
    );
    if terminal {
        candidate.verify_terminal(rows);
    } else {
        candidate.verify_ordinary(rows);
    }
    assert!(recovery.worker_seen_ns >= recovery.due_ns);
    assert_eq!(
        recovery.worker.state,
        collector::recording::control::State::Stopped
    );
    assert!(
        recovery.canonical_ns - recovery.cause_clear_ns <= 30_000_000_000,
        "managed canonical recovery exceeded30seconds after cause clearance"
    );
}

#[test]
fn managed_helper_copy_preserves_the_explicitly_selected_persister_file() {
    // Goal: managed and standalone observations use the exact selected persister executable.
    // Method: supply distinguishable sibling filenames and inspect the copied withheld input.
    let candidate = Candidate::new(8);
    let cohort = candidate.root.join("cohort");
    collector::recording::private::create_private_dir(&cohort).unwrap();
    for name in ["collector", "recorder", "persister"] {
        fs::write(cohort.join(name), name.as_bytes()).unwrap();
    }
    let selected = cohort.join("persister-probe");
    fs::write(&selected, b"explicit selected persister").unwrap();

    let recorder = managed::Recorder::new(&candidate.root, &selected).unwrap();

    assert_eq!(
        fs::read(candidate.root.join("helpers/persister.withheld")).unwrap(),
        b"explicit selected persister"
    );
    drop(recorder);
}

#[test]
#[ignore = "opt-in native full-width event payload and 256 MiB worker observation"]
fn ordinary_full_width_batches_keep_every_native_byte() {
    // Goal: byte-limited physical parts fit the production worker ceiling without data loss.
    // Method: publish2,048full-width identities, retain8observations and query every saved row.
    observe(2_048, false);
}

#[test]
#[ignore = "opt-in native full-width terminal payload and 256 MiB worker observation"]
fn terminal_full_width_batches_keep_every_native_byte() {
    // Goal: event-only history uses the same bounded codec and native memory ceiling.
    // Method: close an owned event-before-observation source and page all native bytes.
    observe(2_048, true);
}

#[test]
#[ignore = "opt-in admitted maximum event capacity and full-width native payload"]
fn ordinary_maximum_event_tail_keeps_every_native_byte() {
    // Goal: physical batching does not lower admitted source capacity or native fidelity.
    // Method: save the largest admitted retained log under the unchanged256MiBworker ceiling.
    observe(ROWS_MAX, false);
}

#[test]
#[ignore = "opt-in admitted maximum terminal event capacity and full-width native payload"]
fn terminal_maximum_event_tail_keeps_every_native_byte() {
    // Goal: a run with no observed publication remains complete at admitted source capacity.
    // Method: recover a maximum full-width event-only log, discover it and page every row.
    observe(ROWS_MAX, true);
}

#[test]
#[ignore = "opt-in admitted maximum event capacity through a registered recording"]
fn registered_ordinary_maximum_event_tail_is_saved_within_its_memory() {
    // Goal: the registered Store and worker save the largest admitted log within 256 MiB, and say
    // how long that took on this machine. The managed tests hold the 30 second bound to this route
    // once creators write its format, which waits on that number (P02.4).
    // Method: run the persister binary over a registered root with a finished maximum ring.
    observe_registered(ROWS_MAX, false);
}

#[test]
#[ignore = "opt-in admitted maximum terminal event capacity through a registered recording"]
fn registered_terminal_maximum_event_tail_is_saved_within_its_memory() {
    // Goal: a registered run with no observed publication does the same (P02.4).
    // Method: hide every publication of a maximum ring and run the persister binary over it.
    observe_registered(ROWS_MAX, true);
}

fn observe_registered(rows: u64, terminal: bool) {
    let mut candidate = Candidate::registered(rows);
    candidate.publish(rows);
    if terminal {
        candidate.withhold_publications(rows);
    }
    let started = Instant::now();

    candidate.run_worker();
    let elapsed = started.elapsed();

    candidate.verify_registered(rows, terminal);
    eprintln!("event_stream registered rows={rows} terminal={terminal} elapsed={elapsed:?}");
}

fn observe(rows: u64, terminal: bool) {
    let mut candidate = Candidate::new(rows);
    candidate.publish(rows);
    if terminal {
        candidate.withhold_publications(rows);
    }
    candidate.run_worker();
    if terminal {
        candidate.verify_terminal(rows);
    } else {
        candidate.verify_ordinary(rows);
    }
}

impl Candidate {
    fn new(rows: u64) -> Self {
        let (root, place) = Self::new_place(rows);
        let (writer, registration) = Registration::start_with_identity(
            &root,
            &place,
            Self::new_shape(rows),
            Self::new_dictionary(),
            1,
        )
        .unwrap()
        .into_parts();
        registration.unwrap();
        let run = writer.identity().unwrap();

        Self::new_with(root, writer, run)
    }

    /// A candidate on a recording of the registered format, which no creator makes yet.
    fn registered(rows: u64) -> Self {
        let (root, place) = Self::new_place(rows);
        let directory = PrivateDirectory::open(&root).unwrap();
        let lock = FileLock::try_lock_in(&directory, "registration.lock").unwrap();
        let recording = bootstrap::initialize(&directory, &lock, &mut [0; 4097]).unwrap();
        drop(lock);
        let run = ring::RunIdentity {
            recording,
            run: ring::Identity([8, 9]),
            boot: ring::Identity([10, 11]),
        };
        let writer = ring::Writer::create_with_identity(
            &place,
            Self::new_shape(rows),
            Self::new_dictionary(),
            &run,
            1,
        )
        .unwrap();
        let registry = Registry {
            runs: vec![RegisteredRun {
                identity: run,
                ring_name: writer.name().to_owned(),
                collector_pid: 0,
            }],
            evictions: 0,
        };
        registry.write(&root).unwrap();

        Self::new_with(root, writer, run)
    }

    fn new_place(rows: u64) -> (PathBuf, ring::Place) {
        assert!(rows <= ROWS_MAX);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("fd-event-stream-{}-{serial}", std::process::id()));
        assert!(!root.exists());
        collector::recording::private::create_private_dir(&root).unwrap();
        let place = ring::Place {
            lock_path: root.join("ring.lock"),
            name_prefix: format!("/fdes{}{serial}", std::process::id()),
        };

        (root, place)
    }

    fn new_shape(rows: u64) -> ring::Shape {
        ring::Shape {
            slot_count: 8,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: rows,
        }
    }

    fn new_dictionary() -> ring::DictionaryCapacity {
        ring::DictionaryCapacity {
            entries: 1,
            arena_bytes: 8192,
        }
    }

    fn new_with(root: PathBuf, writer: ring::Writer, run: ring::RunIdentity) -> Self {
        let source = writer.name().to_owned();
        let sources = vec![source.clone()];

        Self {
            root,
            writer: Some(writer),
            source,
            sources,
            run,
            child: None,
            managed: None,
        }
    }

    fn publish(&mut self, rows: u64) {
        let writer = self.writer.as_mut().unwrap();
        let name = vec![0xff; PROGRAM_BYTES];
        let mut app = vec![0xfe; PROGRAM_BYTES];
        app[0] = b'/';
        app[PROGRAM_BYTES - 4..].copy_from_slice(b".app");
        let mut program = None;
        writer
            .intern_program(
                &ring::ProgramBytes {
                    executable_name: &name,
                    app_path: Some(&app),
                },
                &mut program,
            )
            .unwrap();
        let identity = IdentityObservation {
            pid: 41,
            usage: ring::UsageAvailability::NotApplicable,
            identity: ring::ProcessIdentity::Measured {
                creation: ring::ProcessCreation {
                    unique_id: u64::MAX,
                    pid_version: i32::MIN,
                },
                program: program.unwrap(),
            },
        };
        for number in 1..=rows {
            let memory = collector::machine::Memory::default();
            let event = collector::tracker::Event::Exec { pid: 41 };
            let readings = ring::Readings {
                run_tick: number,
                cpus: &[],
                memory: &memory,
                interfaces: &[],
                ioreport: None,
                smc: None,
                processes: &[],
                processes_refused: 0,
                processes_gone: 0,
                coalitions: &[],
                events: &[event],
                dropped: ring::Dropped::default(),
            };
            writer
                .publish_with_identity(
                    &readings,
                    Some(&ObservationTimes {
                        wall_ns: 1_000_000_000_000 + number,
                        continuous_ns: 1 + number,
                        awake_ns: 1 + number,
                    }),
                    &IdentityPublication {
                        processes: &[],
                        events: &[identity],
                    },
                )
                .unwrap();
        }
        writer.finish();
        drop(self.writer.take());
    }

    fn withhold_publications(&mut self, expected: u64) {
        // Both names belong to this fixture; cleanup owns the copy even if construction fails.
        let target = format!("{}.99", self.source.rsplit_once('.').unwrap().0);
        self.sources.push(target.clone());
        source::withhold(&self.source, &target, expected);
        let mut registry = Registry::read(&self.root).unwrap();
        assert_eq!(registry.runs.len(), 1);
        registry.runs[0].ring_name.clone_from(&target);
        registry.write(&self.root).unwrap();
        ring::unlink_generation(&self.source).unwrap();
        self.source = target;
    }

    fn run_worker(&mut self) {
        let binary = std::env::var_os("FLIGHTDECK_PERSISTER_BINARY").map_or_else(
            || PathBuf::from(env!("CARGO_BIN_EXE_persister")),
            PathBuf::from,
        );
        let diagnostics = self.root.join("worker.log");
        let log = File::create(&diagnostics).unwrap();
        let timebase = collector::clock::Timebase::read().unwrap();
        let started = Instant::now();
        self.child = Some(
            Command::new(&binary)
                .args(["--recording"])
                .arg(&self.root)
                .args(["--memory-bytes", "268435456"])
                .stdout(Stdio::null())
                .stderr(log)
                .spawn()
                .unwrap(),
        );
        for _step in 0..18_000 {
            if let Some(reaped) =
                collector::recording::child::reap_owned(self.child.as_ref().unwrap(), timebase)
                    .unwrap()
            {
                let elapsed = started.elapsed();
                drop(self.child.take());
                let mut log = String::new();
                File::open(&diagnostics)
                    .unwrap()
                    .take(1024 * 1024 + 1)
                    .read_to_string(&mut log)
                    .unwrap();
                assert!(log.len() <= 1024 * 1024);
                eprintln!(
                    "event_stream binary={} root={} status={} child_wall_ns={} child_cpu_ns={} \
                    resident_bytes_max={} final_footprint={:?}\n{log}",
                    binary.display(),
                    self.root.display(),
                    reaped.finished.status,
                    elapsed.as_nanos(),
                    reaped.finished.cpu_ns,
                    reaped.finished.resident_bytes_max,
                    reaped.footprint,
                );
                assert!(reaped.finished.successful());
                let footprint = reaped.footprint.unwrap();
                assert!(footprint.footprint_bytes_max <= collector::u64_from_usize(MEMORY_BYTES));
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // The production watchdog independently enforces120s for each operation.
        panic!("owned event worker exceeded180second fixture observation deadline");
    }

    /// What the canonical frontier says of the run, whose read admits every row of its newest
    /// window. The worker's own tests hold the bytes of those rows.
    fn verify_registered(&self, rows: u64, terminal: bool) {
        let store = persistence::Store::open(&self.root).unwrap();
        let budget = Arc::new(ByteBudget::new(MEMORY_BYTES));
        let frontier = store
            .source_frontier(&self.run, &budget, &mut ReadWork::default())
            .unwrap()
            .unwrap();

        assert!(frontier.complete());
        assert_eq!(frontier.event_end(), rows + 1);
        let publication_end = if terminal { 1 } else { rows + 1 };
        assert_eq!(frontier.publication_end(), publication_end);
    }

    fn verify_ordinary(&self, rows: u64) {
        let store = persistence::Store::open(&self.root).unwrap();
        let manifest = store.progress(&self.run).unwrap().unwrap();
        assert_eq!(
            manifest.publication_first..manifest.publication_end,
            rows - 7..rows + 1
        );
        assert_eq!(manifest.losses.publications, rows - 8);
        assert_eq!(manifest.losses.events, 0);
        let budget = Arc::new(ByteBudget::new(MEMORY_BYTES));
        let table =
            persistence::event_table::EventTable::open_budgeted(&self.root, &manifest, &budget)
                .unwrap();
        let mut expected = 1;
        let mut associated = 0;
        for index in 0..table.len() {
            let part = table.part(index).unwrap();
            let frame = part.read_slice(0, 16_384, &budget).unwrap();
            associated +=
                frame.rows().height() - frame.rows().column("publication").unwrap().null_count();
            verify_rows(frame.rows(), &mut expected);
        }
        assert_eq!(associated, 8);
        assert_eq!(expected, rows + 1);
        eprintln!(
            "event_stream ordinary rows={rows} parts={} ledger_peak={}",
            table.len(),
            budget.used_peak()
        );
        drop(table);
        assert_eq!(budget.used(), 0);
    }

    fn verify_terminal(&self, rows: u64) {
        let store = persistence::Store::open(&self.root).unwrap();
        assert!(store.progress(&self.run).unwrap().is_none());
        let pages = persistence::event_recovery::Pages::open(&self.root, &self.run).unwrap();
        let mut expected = 1;
        for _page in 0..ROWS_MAX {
            let page = pages.read(expected, 16_384).unwrap();
            assert_eq!(
                page.rows.column("publication").unwrap().null_count(),
                page.rows.height()
            );
            assert_eq!(page.losses, persistence::Losses::default());
            verify_rows(&page.rows, &mut expected);
            if page.next.is_none() {
                assert_eq!(expected, rows + 1);
                eprintln!("event_stream terminal rows={rows}");
                return;
            }
            assert_eq!(page.next, Some(expected));
        }
        panic!("bounded terminal page count");
    }
}

fn verify_rows(frame: &polars::prelude::DataFrame, expected: &mut u64) {
    let numbers = frame.column("event").unwrap().u64().unwrap();
    let name = vec![0xff; PROGRAM_BYTES];
    let mut app = vec![0xfe; PROGRAM_BYTES];
    app[0] = b'/';
    app[PROGRAM_BYTES - 4..].copy_from_slice(b".app");
    for row in 0..frame.height() {
        assert_eq!(numbers.get(row), Some(*expected));
        for (column, expected) in [("executable_name", &name), ("app_path", &app)] {
            let value = frame
                .column(column)
                .unwrap()
                .binary()
                .unwrap()
                .get(row)
                .unwrap();
            assert_eq!(value.len(), PROGRAM_BYTES);
            assert_eq!(value, expected);
        }
        assert_eq!(
            frame
                .column("process_unique_id")
                .unwrap()
                .u64()
                .unwrap()
                .get(row),
            Some(u64::MAX)
        );
        assert_eq!(
            frame.column("pid_version").unwrap().i32().unwrap().get(row),
            Some(i32::MIN)
        );
        *expected += 1;
    }
}

impl Drop for Candidate {
    fn drop(&mut self) {
        if let Some(managed) = &mut self.managed {
            if let Err(error) = managed.stop() {
                eprintln!(
                    "owned managed recording retained after cleanup failure: {}: {error}",
                    self.root.display()
                );
                if let Err(report) = managed.report() {
                    eprintln!("owned cleanup report failed: {report}");
                }
                return;
            }
        }
        if let Some(child) = &mut self.child {
            if child.try_wait().unwrap().is_none() {
                child.kill().unwrap();
                stop_child(child);
            }
        }
        drop(self.writer.take());
        assert!(self.sources.len() <= 2);
        for name in &self.sources {
            if let Err(error) = ring::unlink_generation(name) {
                assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
            }
        }
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn stop_child(child: &mut Child) {
    for _step in 0..1_000 {
        if child.try_wait().unwrap().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("owned killed event worker did not reap within10seconds");
}
