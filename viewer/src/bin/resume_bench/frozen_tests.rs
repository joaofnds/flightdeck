use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;

use collector::machine::Memory;
use collector::processes::{Process, Task, Usage};
use collector::recording::{
    RegisteredRun, Registry, new_run_identity, private, recording_identity,
};
use collector::ring::{
    DictionaryCapacity, Dropped, Found, IdentityObservation, IdentityPublication, ObservationTimes,
    Place, ProcessCreation, ProcessIdentity, ProgramBuffers, ProgramBytes, ProgramReference,
    Readings, RingReader, Shape, UsageAvailability, Writer,
};
use persistence::{Begin, Limits, Losses, Manifest, Observation, Window, WindowIdentity};

use super::FrozenRecording;

struct Fixture {
    root: PathBuf,
    target: PathBuf,
    writer: Writer,
    program: ProgramReference,
}

const SHAPE: Shape = Shape {
    slot_count: 120,
    cpu_capacity: 1,
    interface_capacity: 1,
    core_capacity: 1,
    state_capacity: 1,
    fan_capacity: 1,
    process_capacity: 1,
    coalition_capacity: 1,
    event_capacity: 1,
};

#[test]
fn captures_real_identity_rows_and_restores_covered_and_overwritten_gaps() {
    // Goal: all gaps reuse one real endpoint and leave the original recording untouched.
    // Method: seal two real windows, overwrite the ring, then checkpoint and advance copies.
    let mut source = Fixture::new();
    source.populate();
    let frozen = FrozenRecording::capture(&super::Directories {
        source: &source.root,
        target: &source.target,
    })
    .unwrap();
    assert_eq!(frozen.publication_newest(), 1900);
    assert_eq!(frozen.retained_publications(), 120);
    assert_eq!(frozen.committed_windows(), 2);
    let registry = Registry::read(frozen.root()).unwrap();
    let run = &registry.runs[0];
    let reader = RingReader::open_generation(&run.ring_name).unwrap();
    assert_eq!(reader.identity(), source.writer.identity());
    assert_eq!(reader.source_layout_version(), 4);
    assert_ne!(run.ring_name, source.writer.name());
    assert_eq!(
        reader.dictionary_usage().unwrap(),
        source.writer.dictionary_usage().unwrap()
    );
    source.publish(1901);
    assert_eq!(reader.publication_newest().unwrap(), 1900);
    for gap in [1, 30, 60, 1800] {
        assert_checkpoint(&frozen, run, gap);
        frozen.advance().unwrap();
        assert_eq!(reader.publication_newest().unwrap(), 1900);
        assert!(marker(frozen.root(), run.identity.run, 30).exists());
    }
    assert!(frozen.checkpoint(1901).is_err());
    assert_latest_row(&run.ring_name, &source.program);
    let original = marker(&source.root, run.identity.run, 0);
    let captured = marker(frozen.root(), run.identity.run, 0);
    assert_eq!(
        fs::metadata(&original).unwrap().ino(),
        fs::metadata(&captured).unwrap().ino()
    );
    frozen.checkpoint(1800).unwrap();
    drop(reader);
    drop(frozen);
    assert!(source.target.exists());
    assert!(RingReader::open_generation(&run.ring_name).is_err());
    assert_eq!(
        persistence::read_committed(&source.target, &captured)
            .unwrap()
            .identity
            .window,
        0
    );
    assert_eq!(
        RingReader::open_generation(source.writer.name())
            .unwrap()
            .publication_newest()
            .unwrap(),
        1901
    );
}

fn assert_checkpoint(frozen: &FrozenRecording, run: &RegisteredRun, gap: u64) {
    frozen.checkpoint(gap).unwrap();
    let mut reader = RingReader::open_generation(&run.ring_name).unwrap();
    assert_eq!(reader.publication_newest().unwrap(), 1900 - gap);
    assert_eq!(
        marker(frozen.root(), run.identity.run, 30).exists(),
        gap < 60
    );
    let prefix = marker(frozen.root(), run.identity.run, 0);
    assert_eq!(
        persistence::read_committed(frozen.root(), &prefix)
            .unwrap()
            .publication_end,
        61
    );
    if gap > frozen.retained_publications() {
        let mut tick = persistence::tick_buffer(&SHAPE).unwrap();
        assert!(matches!(
            reader.read_into(&mut tick).unwrap(),
            Found::Missed { .. }
        ));
    }
}

impl Fixture {
    fn new() -> Self {
        let nonce = collector::recording::random_identity().unwrap().0[0];
        let root = std::env::temp_dir().join(format!("flightdeck-frozen-{nonce:016x}"));
        private::create_private_dir(&root).unwrap();
        let target = root.join("captured");
        let identity = new_run_identity(recording_identity(&root).unwrap()).unwrap();
        let mut writer = Writer::create_with_identity(
            &Place {
                lock_path: root.join("ring.lock"),
                name_prefix: "/ftest".to_owned(),
            },
            SHAPE,
            DictionaryCapacity {
                entries: 1,
                arena_bytes: 128,
            },
            &identity,
            0,
        )
        .unwrap();
        Registry {
            runs: vec![RegisteredRun {
                identity,
                ring_name: writer.name().to_owned(),
                collector_pid: std::process::id(),
            }],
            evictions: 0,
        }
        .write(&root)
        .unwrap();
        let mut program = None;
        writer
            .intern_program(
                &ProgramBytes {
                    executable_name: b"recorded-helper",
                    app_path: Some(b"/Applications/Recorded.app"),
                },
                &mut program,
            )
            .unwrap();
        Self {
            root,
            target,
            writer,
            program: program.unwrap(),
        }
    }

    fn populate(&mut self) {
        self.populate_window(0);
        for publication in 61..=1800 {
            self.publish(publication);
        }
        self.populate_window(30);
        for publication in 1861..=1900 {
            self.publish(publication);
        }
    }

    fn populate_window(&mut self, number: u64) {
        let identity = WindowIdentity {
            run: self.writer.identity().unwrap(),
            window: number,
        };
        let mut window = Window::new(identity, Limits::from_shape(&SHAPE).unwrap());
        let mut reader = RingReader::open_generation(self.writer.name()).unwrap();
        let mut tick = persistence::tick_buffer(&SHAPE).unwrap();
        let mut identities = Vec::with_capacity(1);
        reader.seek_publication(number * 60 + 1).unwrap();
        for publication in number * 60 + 1..=number * 60 + 60 {
            self.publish(publication);
            assert!(matches!(
                reader
                    .read_identity_into(&mut tick, &mut identities)
                    .unwrap(),
                Found::Publication { .. }
            ));
            window
                .push_tick_with_identity(
                    &Observation {
                        publication,
                        wall_ns: Some(publication * 1_000_000_000),
                        continuous_ns: Some(publication * 1_000_000_000),
                        awake_ns: Some(publication * 1_000_000_000),
                    },
                    &tick,
                    &[],
                    &persistence::IdentitySource {
                        reader: &reader,
                        processes: &identities,
                        events: &[],
                    },
                )
                .unwrap();
        }
        seal(&self.root, &window);
    }

    fn publish(&mut self, publication: u64) {
        let process = process(publication);
        let observation = IdentityObservation {
            pid: process.pid,
            usage: UsageAvailability::Measured,
            identity: ProcessIdentity::Measured {
                creation: ProcessCreation {
                    unique_id: 23,
                    pid_version: 7,
                },
                program: self.program,
            },
        };
        self.writer
            .publish_with_identity(
                &Readings {
                    run_tick: publication - 1,
                    cpus: &[],
                    memory: &Memory::default(),
                    interfaces: &[],
                    ioreport: None,
                    smc: None,
                    processes: std::slice::from_ref(&process),
                    processes_refused: 0,
                    processes_gone: 0,
                    coalitions: &[],
                    events: &[],
                    dropped: Dropped::default(),
                },
                Some(&ObservationTimes {
                    wall_ns: publication * 1_000_000_000,
                    continuous_ns: publication * 1_000_000_000,
                    awake_ns: publication * 1_000_000_000,
                }),
                &IdentityPublication {
                    processes: &[observation],
                    events: &[],
                },
            )
            .unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Err(error) = collector::ring::unlink_generation(self.writer.name()) {
            eprintln!("test fixture: unlink failed: {error}");
        }
        if let Err(error) = fs::remove_dir_all(&self.root) {
            eprintln!("test fixture: remove failed: {error}");
        }
    }
}

fn seal(root: &std::path::Path, window: &Window) {
    let store = persistence::Store::open(root).unwrap();
    let Begin::Pending(pending) = store.begin(&window.identity).unwrap() else {
        panic!("test window already committed");
    };
    let tables = persistence::Encoder.encode(window, pending.path()).unwrap();
    let publications = window.publication_range();
    let events = window.event_range();
    store
        .commit(
            pending,
            Manifest {
                identity: window.identity,
                publication_first: publications.start,
                publication_end: publications.end,
                events_first: events.start,
                events_end: events.end,
                wall_ns: Some(persistence::TimeBounds {
                    min: publications.start * 1_000_000_000,
                    max: (publications.end - 1) * 1_000_000_000,
                }),
                continuous_ns: None,
                awake_ns: None,
                losses: Losses::default(),
                data_directory: String::new(),
                tables,
            },
        )
        .unwrap();
}

fn process(publication: u64) -> Process {
    Process {
        pid: 23,
        usage: Usage {
            awake_ns_at_start: 0,
            user_ns: 9_007_199_254_740_993 + publication,
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
        },
        task: Task {
            threads: 1,
            faults: 2,
            context_switches: 3,
            mach_syscalls: 4,
            unix_syscalls: 5,
        },
    }
}

fn marker(root: &std::path::Path, run: collector::ring::Identity, window: u64) -> PathBuf {
    persistence::layout::WindowKey { run, window }.marker(root)
}

fn assert_latest_row(name: &str, original_program: &ProgramReference) {
    let mut reader = RingReader::open_generation(name).unwrap();
    let mut tick = persistence::tick_buffer(&SHAPE).unwrap();
    let mut identities = Vec::with_capacity(1);
    reader.seek_publication(1900).unwrap();
    assert_eq!(
        reader
            .read_identity_into(&mut tick, &mut identities)
            .unwrap(),
        Found::Publication {
            publication: 1900,
            tick: 1899
        }
    );
    assert_eq!(tick.processes, vec![process(1900)]);
    let program = identities[0].program().unwrap();
    assert_eq!(program.number(), original_program.number());
    let mut executable_name = [0; collector::ring::PROGRAM_PATH_BYTES_MAX];
    let mut app_path = [0; collector::ring::PROGRAM_PATH_BYTES_MAX];
    let (name_length, app_length) = reader
        .copy_program(
            program,
            &mut ProgramBuffers {
                executable_name: &mut executable_name,
                app_path: &mut app_path,
            },
        )
        .unwrap();
    assert_eq!(&executable_name[..name_length], b"recorded-helper");
    assert_eq!(&app_path[..app_length], b"/Applications/Recorded.app");
}
