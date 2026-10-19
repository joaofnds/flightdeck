use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use collector::machine::Memory;
use collector::processes::{Process, Task};
use collector::ring::{
    DictionaryCapacity, Dropped, IdentityObservation, IdentityPublication, ObservationTimes, Place,
    ProcessCreation, ProcessIdentity, ProgramBytes, Readings, Shape, UsageAvailability, Writer,
};

use super::usage;

pub(super) struct ResidentFixture {
    pub root: PathBuf,
    writer: Writer,
    publication: u64,
}

pub(super) struct Resident<'key> {
    pub key: &'key [u8],
    pub bytes: Option<u64>,
}

static NEXT: AtomicU64 = AtomicU64::new(0);

impl ResidentFixture {
    pub fn new() -> Self {
        Self::with_capacity(
            10,
            DictionaryCapacity {
                entries: 32,
                arena_bytes: 65_536,
            },
        )
    }

    pub fn with_capacity(population: u64, dictionary: DictionaryCapacity) -> Self {
        assert!(population <= 24_000);
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "flightdeck-resident-{}-{serial}",
            std::process::id()
        ));
        collector::recording::private::create_private_dir(&root).unwrap();
        let place = Place {
            lock_path: root.join("ring.lock"),
            name_prefix: format!("/fdrs{}x{serial}", std::process::id()),
        };
        let shape = Shape {
            slot_count: 2,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: population,
            coalition_capacity: 1,
            event_capacity: 1,
        };
        let (writer, registered) = collector::recording::Registration::start_with_identity(
            &root, &place, shape, dictionary, 0,
        )
        .unwrap()
        .into_parts();
        registered.unwrap();

        Self {
            root,
            writer,
            publication: 0,
        }
    }

    pub fn publish(&mut self, residents: &[Resident<'_>]) {
        self.publish_at(residents, (1000 + self.publication) * 1_000_000_000);
    }

    pub fn publish_at(&mut self, residents: &[Resident<'_>], wall_ns: u64) {
        assert!(residents.len() <= 24_000);
        let mut processes = Vec::with_capacity(residents.len());
        let mut identities = Vec::with_capacity(residents.len());
        for (index, resident) in residents.iter().enumerate() {
            let mut program = None;
            self.writer
                .intern_program(
                    &ProgramBytes {
                        executable_name: b"main",
                        app_path: Some(resident.key),
                    },
                    &mut program,
                )
                .unwrap();
            let pid = i32::try_from(index + 1).unwrap();
            identities.push(IdentityObservation {
                pid,
                usage: if resident.bytes.is_some() {
                    UsageAvailability::Measured
                } else {
                    UsageAvailability::Refused
                },
                identity: ProcessIdentity::Measured {
                    creation: ProcessCreation {
                        unique_id: u64::try_from(pid).unwrap(),
                        pid_version: 1,
                    },
                    program: program.unwrap(),
                },
            });
            if let Some(bytes) = resident.bytes {
                processes.push(Process {
                    pid,
                    usage: usage(bytes),
                    task: Task {
                        threads: 1,
                        faults: 0,
                        context_switches: 0,
                        mach_syscalls: 0,
                        unix_syscalls: 0,
                    },
                });
            }
        }

        self.publish_readings(&processes, &identities, wall_ns);
        self.publication += 1;
    }

    fn publish_readings(
        &mut self,
        processes: &[Process],
        identities: &[IdentityObservation],
        wall_ns: u64,
    ) {
        let memory = Memory::default();
        self.writer
            .publish_with_identity(
                &Readings {
                    run_tick: self.publication,
                    cpus: &[],
                    memory: &memory,
                    interfaces: &[],
                    ioreport: None,
                    smc: None,
                    processes,
                    processes_refused: u32::try_from(identities.len() - processes.len()).unwrap(),
                    processes_gone: 0,
                    coalitions: &[],
                    events: &[],
                    dropped: Dropped::default(),
                },
                Some(&ObservationTimes {
                    wall_ns,
                    continuous_ns: self.publication * 1_000_000_000,
                    awake_ns: self.publication * 1_000_000_000,
                }),
                &IdentityPublication {
                    processes: identities,
                    events: &[],
                },
            )
            .unwrap();
    }
}

impl Drop for ResidentFixture {
    fn drop(&mut self) {
        self.writer.finish();
        collector::ring::unlink_generation(self.writer.name()).unwrap();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}
