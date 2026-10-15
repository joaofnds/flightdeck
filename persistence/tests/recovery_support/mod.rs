use std::fs;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use collector::machine::Memory;
use collector::processes::{Process, Task, Usage};
use collector::recording::{Registration, Registry, identity_text};
use collector::ring::{Dropped, ObservationTimes, Place, Readings, Shape, Writer};
use collector::tracker::{Event, Noticed};

pub struct RecordingHarness {
    root: PathBuf,
    place: Place,
    writer: Option<Writer>,
    worker: Option<Child>,
    old_run: String,
    new_run: String,
    refused: bool,
    awake_origin_ns: u64,
}

static NEXT: AtomicU64 = AtomicU64::new(0);

impl RecordingHarness {
    pub fn start() -> Self {
        let ordinal = NEXT.fetch_add(1, Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("fd-recovery-{}-{ordinal}", std::process::id()));
        assert!(!root.exists(), "{} already exists", root.display());
        collector::recording::private::create_private_dir(&root).unwrap();
        let place = Place {
            lock_path: root.join("ring.lock"),
            name_prefix: format!("/fdr{}{ordinal}", std::process::id()),
        };
        let registration = Registration::prepare(&root, &place).unwrap();
        let old_run = identity_text(registration.identity().run).to_string();
        let writer = Writer::create_recording(&place, shape(), registration.identity(), 0).unwrap();
        registration.register(writer.name()).unwrap();
        let awake_origin_ns =
            collector::recording::observation_times(collector::clock::Timebase::read().unwrap())
                .unwrap()
                .awake_ns;
        Self {
            root,
            place,
            writer: Some(writer),
            worker: None,
            old_run,
            new_run: String::new(),
            refused: false,
            awake_origin_ns,
        }
    }

    pub fn record_old_window_and_tail(&mut self) {
        self.publish(0, 1_000, 0, false);
        self.publish(59, 1_059, 59, true);
        self.publish(60, 1_060, 60, false);
        self.restart_worker();
    }

    pub fn record_a_tail_from_a_reused_producer_pid(&mut self) {
        let timebase = collector::clock::Timebase::read().unwrap();
        let usage =
            collector::processes::read_usage(timebase, collector::processes::own_pid().unwrap())
                .unwrap()
                .unwrap();
        self.awake_origin_ns = usage.awake_ns_at_start.checked_sub(1).unwrap();
        self.publish(0, 1_000, 7, false);
        self.restart_worker();
    }

    pub fn wait_for_worker_exit(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(20);
        for _attempt in 0..2_000 {
            if let Some(status) = self.worker.as_mut().unwrap().try_wait().unwrap() {
                assert!(status.success());
                self.worker = None;
                return;
            }
            assert!(
                Instant::now() < deadline,
                "persister did not finish the closed source"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("persister did not finish the closed source");
    }

    pub fn wait_for_old_full_window(&self) {
        self.wait_for_marker(&self.old_run, 0);
    }

    pub fn kill_worker(&mut self) {
        let mut worker = self.worker.take().unwrap();
        worker.kill().unwrap();
        worker.wait().unwrap();
    }

    pub fn restart_collection_with_new_window(&mut self) {
        drop(self.writer.take());
        let registration = Registration::prepare(&self.root, &self.place).unwrap();
        self.new_run = identity_text(registration.identity().run).to_string();
        let mut changed = shape();
        changed.process_capacity = 2;
        let writer =
            Writer::create_recording(&self.place, changed, registration.identity(), 0).unwrap();
        registration.register(writer.name()).unwrap();
        self.writer = Some(writer);
        self.publish(0, 2_000, 101, false);
        self.publish(60, 2_060, 102, false);
    }

    pub fn restart_worker(&mut self) {
        self.start_worker(persistence::resources::WORKING_BYTES_MAX);
        // Publishing after the recovery worker starts cannot wait for its disk commits.
        if !self.new_run.is_empty() {
            self.publish(61, 2_061, 103, false);
        }
    }

    fn start_worker(&mut self, memory: u64) {
        assert!(self.worker.is_none());
        self.worker = Some(
            Command::new(env!("CARGO_BIN_EXE_persister"))
                .args([
                    "--recording",
                    self.root.to_str().unwrap(),
                    "--parent-pid",
                    &std::process::id().to_string(),
                    "--memory-bytes",
                    &memory.to_string(),
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
    }

    pub fn record_backward_wall_clock(&mut self) {
        self.publish(0, 1_000, 0, false);
        self.publish(1, 1_000, 1, false);
        self.publish(2, 999, 2, false);
        self.publish(60, 1_001, 60, false);
        self.restart_worker();
    }

    pub fn record_elapsed_gap_and_finish(&mut self) {
        self.publish(0, 1_000, 0, false);
        self.publish(180, 1_180, 180, false);
        self.writer.as_mut().unwrap().finish();
        self.restart_worker();
        let status = self.worker.as_mut().unwrap().wait().unwrap();
        assert!(status.success());
        self.worker = None;
    }

    pub fn assert_old_windows(&self, expected: &[u64]) {
        let day = self
            .root
            .join("commits")
            .join(&self.old_run)
            .join(format!("{:020}", 0));
        let mut actual: Vec<u64> = fs::read_dir(day)
            .unwrap()
            .map(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_str()
                    .unwrap()
                    .trim_end_matches(".seal")
                    .parse()
                    .unwrap()
            })
            .collect();
        actual.sort_unstable();
        assert_eq!(actual, expected);
    }

    pub fn refuse_worker_with_low_memory_limit(&mut self) {
        self.publish(0, 1_000, 0, false);
        self.publish(60, 1_060, 60, false);
        self.start_worker(1);
        let status = self.worker.as_mut().unwrap().wait().unwrap();
        self.refused = status.code() == Some(74);
        self.worker = None;
        self.publish(61, 1_061, 61, false);
    }

    pub fn assert_worker_refused_without_a_commit(&self) {
        assert!(self.refused);
        assert!(!self.marker(&self.old_run, 0).exists());
    }

    pub fn current_publication(&self) -> u64 {
        collector::ring::RingReader::open(&self.place.lock_path)
            .unwrap()
            .publication_newest()
            .unwrap()
    }

    pub fn wait_for_recovered_tail_and_new_window(&self) {
        self.wait_for_marker(&self.old_run, 1);
        self.wait_for_marker(&self.new_run, 0);
    }

    pub fn wait_for_new_window(&self) {
        self.wait_for_marker(&self.new_run, 0);
    }

    pub fn continue_new_collection(&mut self) {
        self.publish(61, 2_061, 103, false);
    }

    pub fn lose_discovery_lock(&self) {
        fs::remove_file(&self.place.lock_path).unwrap();
    }

    pub fn assert_one_retention_eviction(&self) {
        let registry = Registry::read(&self.root).unwrap();
        assert_eq!(registry.evictions, 1);
        assert_eq!(registry.runs.len(), 2);
    }

    pub fn record_and_recover_untimed_tail(&mut self) {
        drop(self.writer.take());
        let mut legacy = Writer::create(&self.place, shape()).unwrap();
        let memory = Memory::default();
        let process = Process {
            pid: 41,
            usage: usage(77),
            task: Task {
                threads: 1,
                faults: 2,
                context_switches: 3,
                mach_syscalls: 4,
                unix_syscalls: 5,
            },
        };
        legacy.publish(&Readings {
            run_tick: 0,
            cpus: &[],
            memory: &memory,
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &[process],
            processes_refused: 0,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: Dropped::default(),
        });
        drop(legacy);
        drop(Registration::prepare(&self.root, &self.place).unwrap());
        self.start_worker(persistence::resources::WORKING_BYTES_MAX);
        assert!(self.worker.as_mut().unwrap().wait().unwrap().success());
        self.worker = None;
    }

    pub fn assert_one_untimed_window_with_its_original_reading(&self) {
        let second = persistence::query::processes_at_second(&self.root, 1_059).unwrap();
        assert_eq!(second.unavailable_time_windows, 1);
        let registry = Registry::read(&self.root).unwrap();
        let legacy = registry.runs.last().unwrap();
        assert_eq!(legacy.identity.boot.0, [0, 0]);
        let store = persistence::Store::open(&self.root).unwrap();
        let manifest = store.progress(&legacy.identity).unwrap().unwrap();
        assert_eq!(manifest.wall_ns, None);
        let path = self
            .root
            .join(manifest.data_directory)
            .join("processes.parquet");
        let connection = duckdb::Connection::open_in_memory().unwrap();
        let reading: u64 = connection
            .query_row(
                "SELECT user_ns FROM read_parquet(?)",
                [path.to_str().unwrap()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reading, 77);
    }

    pub fn processes_at_second(&self, second: u64) -> Vec<(i32, u64)> {
        query_processes(&self.root, second)
    }

    pub fn assert_old_tail_committed_before_new_window(&self) {
        let old = fs::metadata(self.marker(&self.old_run, 1).parent().unwrap())
            .unwrap()
            .modified()
            .unwrap();
        let new = fs::metadata(self.marker(&self.new_run, 0).parent().unwrap())
            .unwrap()
            .modified()
            .unwrap();
        assert!(old <= new);
    }

    pub fn assert_collection_advanced_during_recovery(&self) {
        let reader = collector::ring::RingReader::open(&self.place.lock_path).unwrap();
        assert!(reader.publication_newest().unwrap() >= 6);
    }

    fn publish(&mut self, tick: u64, second: u64, user_ns: u64, exits: bool) {
        let memory = Memory::default();
        let process = Process {
            pid: 41,
            usage: usage(user_ns),
            task: Task {
                threads: 1,
                faults: 2,
                context_switches: 3,
                mach_syscalls: 4,
                unix_syscalls: 5,
            },
        };
        let event = Event::Exit {
            pid: 42,
            last: Ok(usage(99)),
            noticed: Noticed::OnExit,
        };
        let events = if exits {
            std::slice::from_ref(&event)
        } else {
            &[]
        };
        self.writer.as_mut().unwrap().publish_observed(
            &Readings {
                run_tick: tick,
                cpus: &[],
                memory: &memory,
                interfaces: &[],
                ioreport: None,
                smc: None,
                processes: &[process],
                processes_refused: 0,
                processes_gone: 0,
                coalitions: &[],
                events,
                dropped: Dropped::default(),
            },
            ObservationTimes {
                wall_ns: second * 1_000_000_000,
                continuous_ns: tick * 1_000_000_000,
                awake_ns: self.awake_origin_ns + tick * 1_000_000_000,
            },
        );
    }

    fn marker(&self, run: &str, window: u64) -> PathBuf {
        self.root
            .join("commits")
            .join(run)
            .join(format!("{:020}", window / 1440))
            .join(format!("{window:020}.seal"))
    }

    fn wait_for_marker(&self, run: &str, window: u64) {
        let deadline = Instant::now() + Duration::from_secs(20);
        while !self.marker(run, window).exists() {
            assert!(
                Instant::now() < deadline,
                "seal missing: {}",
                self.marker(run, window).display()
            );
            std::thread::yield_now();
        }
    }
}

impl Drop for RecordingHarness {
    fn drop(&mut self) {
        if let Some(mut worker) = self.worker.take() {
            match worker.try_wait() {
                Ok(Some(_status)) => {}
                Ok(None) => {
                    worker.kill().unwrap();
                    worker.wait().unwrap();
                }
                Err(error) => panic!("worker status: {error}"),
            }
        }
        drop(self.writer.take());
        let registry = Registry::read(&self.root).unwrap();
        for run in registry.runs {
            collector::ring::unlink_generation(&run.ring_name).unwrap();
        }
        fs::remove_dir_all(&self.root).unwrap();
    }
}

fn shape() -> Shape {
    Shape {
        slot_count: 120,
        cpu_capacity: 1,
        interface_capacity: 1,
        core_capacity: 1,
        state_capacity: 1,
        fan_capacity: 1,
        process_capacity: 1,
        coalition_capacity: 1,
        event_capacity: 120,
    }
}

fn usage(user_ns: u64) -> Usage {
    Usage {
        awake_ns_at_start: 7,
        user_ns,
        system_ns: 3,
        energy_nj: 4,
        instructions: 5,
        super_instructions: 6,
        cycles: 7,
        idle_wakeups: 8,
        interrupt_wakeups: 9,
        pageins: 10,
        resident_bytes: 11,
        footprint_bytes: 12,
        disk_read_bytes: 13,
        disk_written_bytes: 14,
    }
}

pub fn query_processes(root: &std::path::Path, second: u64) -> Vec<(i32, u64)> {
    persistence::query::processes_at_second(root, second)
        .unwrap()
        .processes
}
