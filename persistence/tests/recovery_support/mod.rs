use std::ffi::{CString, c_char, c_int};
use std::fs;
use std::os::fd::{AsRawFd, FromRawFd};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};
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
    old_source: String,
    sources: Vec<String>,
    legacy_identity: Option<collector::ring::RunIdentity>,
    process_birth_ns: u64,
}

static NEXT: AtomicU64 = AtomicU64::new(0);
static SPAWNS: Mutex<()> = Mutex::new(());

#[derive(Clone, Copy)]
pub enum SourceFault {
    Unsupported,
    Corrupt,
    Permission,
    Foreign,
}

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
        let spawns = spawns();
        let (writer, registered) = Registration::start(&root, &place, shape(), 0)
            .unwrap()
            .into_parts();
        registered.unwrap();
        let old_run = identity_text(writer.identity().unwrap().run).to_string();
        let old_source = writer.name().to_owned();
        drop(spawns);
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
            sources: vec![old_source.clone()],
            old_source,
            legacy_identity: None,
            process_birth_ns: 7,
        }
    }

    pub fn leave_an_unsupported_old_source(&mut self) {
        self.leave_a_faulty_old_source(SourceFault::Unsupported);
    }

    pub fn leave_a_faulty_old_source(&mut self, fault: SourceFault) {
        let spawns = spawns();
        self.publish(0, 1_000, 7, false);
        drop(self.writer.take());
        let source = open_source(&self.old_source);
        let name = format!("{}.99", self.old_source.rsplit_once('.').unwrap().0);
        let mut layout = Forgery::default();
        if matches!(fault, SourceFault::Permission) {
            layout.mode = 0o600;
        }
        forge_source(&source, &name, &layout, |_, words| match fault {
            SourceFault::Unsupported => words[1] = 999,
            SourceFault::Corrupt => words[0] = 0,
            SourceFault::Permission | SourceFault::Foreign => {}
        });
        collector::ring::unlink_generation(&self.old_source).unwrap();
        self.sources[0].clone_from(&name);
        self.old_source = name;
        let mut registry = Registry::read(&self.root).unwrap();
        registry.runs[0].ring_name.clone_from(&self.old_source);
        if matches!(fault, SourceFault::Foreign) {
            registry.runs[0].identity.recording = collector::ring::Identity([99, 1]);
        }
        registry.write(&self.root).unwrap();
        fs::write(&self.place.lock_path, &self.old_source).unwrap();
        drop(spawns);
    }

    pub fn assert_old_source_retained(&self) {
        drop(open_source(&self.old_source));
    }

    pub fn new_window_prior_loss_unknown(&self) -> bool {
        persistence::read_committed(&self.root, &self.marker(&self.new_run, 0))
            .unwrap()
            .losses
            .unknown
    }

    pub fn evict_unsealed_history_with_surviving_worker(&mut self) -> persistence::Losses {
        self.record_old_window_and_tail();
        self.wait_for_old_full_window();
        self.restart_collection_with_new_window();
        let retained = self.new_run.clone();
        self.restart_collection_with_new_window();
        self.wait_for_marker(&retained, 0);
        self.wait_for_new_window();
        persistence::read_committed(&self.root, &self.marker(&retained, 0))
            .unwrap()
            .losses
    }

    pub fn record_old_window_and_tail(&mut self) {
        self.publish(0, 1_000, 0, false);
        self.publish(59, 1_059, 59, true);
        self.publish(60, 1_060, 60, false);
        self.restart_worker();
    }

    pub fn record_reused_process_identity(&mut self) {
        self.publish(0, 1_000, 11, false);
        self.process_birth_ns = 8;
        self.publish(1, 1_000, 22, false);
        self.finish_collection_and_drain_start();
    }

    pub fn old_process_identities(&self) -> Vec<(i32, u64, u64)> {
        let manifest =
            persistence::read_committed(&self.root, &self.marker(&self.old_run, 0)).unwrap();
        let path = self
            .root
            .join(manifest.data_directory)
            .join("processes.parquet");
        let connection = duckdb::Connection::open_in_memory().unwrap();
        let mut statement = connection
            .prepare(
                "SELECT pid,awake_ns_at_start,user_ns FROM read_parquet(?) \
            ORDER BY awake_ns_at_start",
            )
            .unwrap();
        statement
            .query_map([path.to_str().unwrap()], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    pub fn record_overwritten_history(&mut self) {
        for tick in 0..=180 {
            self.publish(tick, 1_000 + tick, tick, false);
        }
        self.finish_collection_and_drain_start();
    }

    pub fn assert_exact_publication_loss(&self, publications: u64) {
        let manifest =
            persistence::read_committed(&self.root, &self.marker(&self.old_run, 1)).unwrap();
        assert_eq!(manifest.losses.publications, publications);
        assert_eq!(manifest.losses.events, 0);
        assert!(!manifest.losses.unknown);
        assert_eq!(manifest.publication_first, 62);
        assert_eq!(manifest.publication_end, 121);
    }

    pub fn refuse_storage_then_recover(&mut self) {
        self.publish(0, 1_000, 0, false);
        self.publish(59, 1_059, 59, true);
        self.publish(60, 1_060, 60, false);
        self.restart_collection_with_new_window();
        collector::recording::private::open_private_file(&self.root.join("data"), true).unwrap();
        self.restart_worker();
        let status = wait_status(self.worker.as_mut().unwrap());
        assert_eq!(status.code(), Some(1));
        self.worker = None;
        assert!(!self.marker(&self.new_run, 0).exists());
        assert!(self.current_publication() >= 6);
        fs::remove_file(self.root.join("data")).unwrap();
        self.restart_worker();
        self.wait_for_recovered_tail_and_new_window();
    }

    pub fn record_one_late_tick_before_a_full_scheduled_minute(&mut self) {
        self.publish(0, 1_000, 0, false);
        self.publish_at(59, 1_059, 59, false, 60_000_500_000);
        for tick in 60..120 {
            self.publish_at(
                tick,
                1_000 + tick,
                tick,
                false,
                tick * 1_000_000_000 + 1_000_000,
            );
        }
        self.publish(120, 1_120, 120, false);
        self.finish_collection_and_drain_start();
    }

    fn finish_collection_and_drain_start(&mut self) {
        self.writer.as_mut().unwrap().finish();
        self.start_worker(persistence::resources::WORKING_BYTES_MAX);
    }

    pub fn finish_collection_and_drain(&mut self, generation_value: u64) {
        self.publish(62, 3_062, generation_value, false);
        self.finish_collection_and_drain_start();
        self.wait_for_worker_exit();
    }

    pub fn assert_two_completed_sources_retained_without_loss(&self) {
        let registry = Registry::read(&self.root).unwrap();
        assert_eq!(registry.evictions, 0);
        assert_eq!(registry.runs.len(), 2);
        assert_eq!(self.sources.len(), 4);
        for source in &self.sources[..2] {
            assert!(!source_exists(source));
        }
        for source in &self.sources[2..] {
            assert!(source_exists(source));
        }
    }

    pub fn old_window_tick_count(&self, window: u64) -> u64 {
        let manifest =
            persistence::read_committed(&self.root, &self.marker(&self.old_run, window)).unwrap();
        manifest
            .tables
            .iter()
            .find(|table| table.name == "ticks")
            .unwrap()
            .rows
    }

    pub fn refuse_second_worker(&mut self) {
        let spawns = spawns();
        let mut worker = Command::new(env!("CARGO_BIN_EXE_persister"))
            .args(["--recording", self.root.to_str().unwrap()])
            .spawn()
            .unwrap();
        drop(spawns);
        let status = wait_status(&mut worker);
        self.refused = status.code()
            == Some(i32::from(
                collector::recording::child::OWNERSHIP_EXIT_STATUS,
            ));
    }

    pub fn assert_second_worker_owned(&self) {
        assert!(self.refused);
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

    pub fn record_a_tail_with_a_pid_now_owned_by_another_user(&mut self) {
        let timebase = collector::clock::Timebase::read().unwrap();
        assert!(matches!(
            collector::processes::read_usage(timebase, 1).unwrap(),
            Err(collector::processes::Unreadable::Refused)
        ));
        self.publish(0, 1_000, 7, false);
        let mut registry = Registry::read(&self.root).unwrap();
        registry.runs[0].collector_pid = 1;
        registry.write(&self.root).unwrap();
        self.restart_worker();
    }

    pub fn wait_for_worker_exit(&mut self) {
        let status = wait_status(self.worker.as_mut().unwrap());
        assert!(status.success());
        self.worker = None;
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
        let spawns = spawns();
        drop(self.writer.take());
        let mut changed = shape();
        changed.process_capacity = 2;
        let (writer, registered) = Registration::start(&self.root, &self.place, changed, 0)
            .unwrap()
            .into_parts();
        registered.unwrap();
        self.new_run = identity_text(writer.identity().unwrap().run).to_string();
        self.sources.push(writer.name().to_owned());
        self.writer = Some(writer);
        drop(spawns);
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
        let spawns = spawns();
        self.worker = Some(
            Command::new(env!("CARGO_BIN_EXE_persister"))
                .args([
                    "--recording",
                    self.root.to_str().unwrap(),
                    "--memory-bytes",
                    &memory.to_string(),
                ])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        );
        drop(spawns);
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
        let status = wait_status(self.worker.as_mut().unwrap());
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
        let status = wait_status(self.worker.as_mut().unwrap());
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
        self.wait_for_marker(&self.new_run, 0);
        assert!(
            self.marker(&self.old_run, 1).try_exists().unwrap(),
            "new window became visible before the recoverable old tail"
        );
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
        assert!(registry.runs.len() <= 2);
    }

    pub fn record_and_recover_untimed_tail(&mut self) {
        let spawns = spawns();
        drop(self.writer.take());
        let original = record_legacy_tail(&self.place);
        let name = format!("{}.99", original.rsplit_once('.').unwrap().0);
        forge_source(
            &open_source(&original),
            &name,
            &Forgery {
                removed_slot_words: 3,
                ..Forgery::default()
            },
            forge_legacy_slots,
        );
        remove_source(&original);
        fs::write(&self.place.lock_path, &name).unwrap();
        self.sources.push(name);
        let (mut writer, registered) = Registration::start(&self.root, &self.place, shape(), 0)
            .unwrap()
            .into_parts();
        registered.unwrap();
        self.sources.push(writer.name().to_owned());
        self.legacy_identity = Some(Registry::read(&self.root).unwrap().runs[0].identity);
        writer.finish();
        drop(writer);
        drop(spawns);
        self.start_worker(persistence::resources::WORKING_BYTES_MAX);
        assert!(wait_status(self.worker.as_mut().unwrap()).success());
        self.worker = None;
    }

    pub fn assert_one_untimed_window_with_its_original_reading(&self) {
        let second = persistence::query::processes_at_second(&self.root, 1_059).unwrap();
        assert_eq!(second.unavailable_time_windows, 1);
        let legacy = self.legacy_identity.unwrap();
        assert_eq!(legacy.boot.0, [0, 0]);
        let store = persistence::Store::open(&self.root).unwrap();
        let manifest = store.progress(&legacy).unwrap().unwrap();
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

    pub fn assert_publication_number_continuity(&self) {
        let reader = collector::ring::RingReader::open(&self.place.lock_path).unwrap();
        assert!(reader.publication_newest().unwrap() >= 6);
    }

    fn publish(&mut self, tick: u64, second: u64, user_ns: u64, exits: bool) {
        self.publish_at(tick, second, user_ns, exits, tick * 1_000_000_000);
    }

    fn publish_at(
        &mut self,
        tick: u64,
        second: u64,
        user_ns: u64,
        exits: bool,
        continuous_ns: u64,
    ) {
        let memory = Memory::default();
        let mut process = Process {
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
        process.usage.awake_ns_at_start = self.process_birth_ns;
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
            &ObservationTimes {
                wall_ns: second * 1_000_000_000,
                continuous_ns,
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
        while !self.marker(run, window).try_exists().unwrap() {
            assert!(
                Instant::now() < deadline,
                "seal missing: {}",
                self.marker(run, window).display()
            );
            std::thread::sleep(Duration::from_millis(10));
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
        let spawns = spawns();
        drop(self.writer.take());
        for name in &self.sources {
            remove_source(name);
        }
        drop(spawns);
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

fn open_source(name: &str) -> fs::File {
    open_source_with_flags(name, 0)
}

fn remove_source(name: &str) {
    unsafe extern "C" {
        fn shm_unlink(name: *const c_char) -> c_int;
    }
    let name = CString::new(name).unwrap();
    if unsafe { shm_unlink(name.as_ptr()) } != 0 {
        assert_eq!(std::io::Error::last_os_error().raw_os_error(), Some(2));
    }
}

fn open_source_with_flags(name: &str, flags: c_int) -> fs::File {
    try_open_source_with_flags(name, flags).unwrap()
}

fn source_exists(name: &str) -> bool {
    match try_open_source_with_flags(name, 0) {
        Ok(source) => {
            drop(source);
            true
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => panic!("source status unavailable: {error}"),
    }
}

fn try_open_source_with_flags(name: &str, flags: c_int) -> std::io::Result<fs::File> {
    unsafe extern "C" {
        fn shm_open(name: *const c_char, flags: c_int, ...) -> c_int;
    }
    let name = CString::new(name).unwrap();
    let descriptor = unsafe { shm_open(name.as_ptr(), flags) };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(unsafe { fs::File::from_raw_fd(descriptor) })
}

struct Forgery {
    mode: u16,
    removed_slot_words: usize,
}

impl Default for Forgery {
    fn default() -> Self {
        Self {
            mode: 0o400,
            removed_slot_words: 0,
        }
    }
}

fn forge_source(
    source: &fs::File,
    name: &str,
    layout: &Forgery,
    edit: impl Fn(&[u64], &mut [u64]),
) {
    unsafe extern "C" {
        fn shm_open(name: *const c_char, flags: c_int, ...) -> c_int;
        fn ftruncate(descriptor: c_int, length: i64) -> c_int;
        fn mmap(
            address: *mut std::ffi::c_void,
            length: usize,
            protection: c_int,
            flags: c_int,
            descriptor: c_int,
            offset: i64,
        ) -> *mut std::ffi::c_void;
        fn munmap(address: *mut std::ffi::c_void, length: usize) -> c_int;
    }
    let length = usize::try_from(source.metadata().unwrap().len()).unwrap();
    let name = CString::new(name).unwrap();
    let target_length = length - 120 * layout.removed_slot_words * 8;
    let descriptor = unsafe { shm_open(name.as_ptr(), 0x602, i32::from(layout.mode)) };
    assert!(descriptor >= 0, "{}", std::io::Error::last_os_error());
    let target = unsafe { fs::File::from_raw_fd(descriptor) };
    assert_eq!(
        unsafe { ftruncate(descriptor, i64::try_from(target_length).unwrap()) },
        0
    );
    let from = unsafe { mmap(std::ptr::null_mut(), length, 1, 1, source.as_raw_fd(), 0) };
    assert_ne!(from.addr(), usize::MAX);
    let to = unsafe {
        mmap(
            std::ptr::null_mut(),
            target_length,
            3,
            1,
            target.as_raw_fd(),
            0,
        )
    };
    assert_ne!(to.addr(), usize::MAX);
    unsafe {
        let original = std::slice::from_raw_parts(from.cast::<u64>(), length / 8);
        let words = std::slice::from_raw_parts_mut(to.cast::<u64>(), target_length / 8);
        words.copy_from_slice(&original[..words.len()]);
        edit(original, words);
    }
    assert_eq!(unsafe { munmap(from, length) }, 0);
    assert_eq!(unsafe { munmap(to, target_length) }, 0);
}

fn forge_legacy_slots(source: &[u64], target: &mut [u64]) {
    const LEGACY_WORDS: usize = 39 + 2 + 10 + 1 + 2 + 1 + 18 + 10;
    const CURRENT_WORDS: usize = LEGACY_WORDS + 3;
    target[1] = 2;
    target[17] = 39;
    for slot in 0..120 {
        let source = &source[32 + slot * CURRENT_WORDS..][..CURRENT_WORDS];
        let target = &mut target[32 + slot * LEGACY_WORDS..][..LEGACY_WORDS];
        target[..39].copy_from_slice(&source[..39]);
        target[39..].copy_from_slice(&source[42..]);
    }
    target[32 + 120 * LEGACY_WORDS..].copy_from_slice(&source[32 + 120 * CURRENT_WORDS..]);
}

fn spawns() -> MutexGuard<'static, ()> {
    // The lock protects fork/exec descriptor ownership, not mutable fixture data.
    match SPAWNS.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

fn wait_status(child: &mut Child) -> std::process::ExitStatus {
    for _attempt in 0..2_000 {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    panic!("persister did not finish within its test drain bound");
}

fn record_legacy_tail(place: &Place) -> String {
    let mut legacy = Writer::create(place, shape()).unwrap();
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
    let original = legacy.name().to_owned();
    drop(legacy);
    original
}
