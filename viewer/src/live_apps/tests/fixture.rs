use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use collector::machine::Memory;
use collector::processes::{Process, Task, Usage};
use collector::recording::private;
use collector::ring::{
    DictionaryCapacity, Dropped, Found, Identity, IdentityObservation, IdentityPublication,
    ObservationTimes, Place, ProcessCreation, ProcessIdentity, ProgramBytes, RingReader,
    RunIdentity, Shape, Tick, UsageAvailability, Writer,
};

use super::super::{Frame, Publication};

static NEXT: AtomicU64 = AtomicU64::new(0);
pub const SHAPE: Shape = Shape {
    slot_count: 2,
    cpu_capacity: 1,
    interface_capacity: 1,
    core_capacity: 1,
    state_capacity: 1,
    fan_capacity: 1,
    process_capacity: 5,
    coalition_capacity: 1,
    event_capacity: 1,
};
const PATHS: [&[u8]; 5] = [
    b"/Applications/A.app/Contents/Helpers/Nested.app/Contents/MacOS/helper\xff",
    b"/Applications/A.app/Contents/MacOS/main",
    b"/usr/bin/name\xff",
    b"/usr/bin/name\\xff",
    b"/Applications/A.app/Contents/MacOS/refused",
];

pub struct Fixture {
    root: PathBuf,
    ring: String,
    writer: Option<Writer>,
    pub reader: Option<RingReader>,
    pub tick: Tick,
    pub identities: Vec<IdentityObservation>,
    identity: bool,
}

#[derive(Clone, Copy)]
pub enum Change {
    None,
    PidReuse,
    Exec,
}

impl Fixture {
    pub fn initialize(output: &mut Option<Box<Self>>, identity: bool) {
        Self::initialize_with_shape(output, identity, &SHAPE);
    }

    pub fn initialize_with_shape(output: &mut Option<Box<Self>>, identity: bool, shape: &Shape) {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "flightdeck-live-app-frame-{}-{serial}",
            std::process::id(),
        ));
        private::create_private_dir(&root).unwrap();
        let place = Place {
            lock_path: root.join("ring.lock"),
            name_prefix: format!("/fdlaf{}x{serial}", std::process::id()),
        };
        let run = RunIdentity {
            recording: Identity([1, 2]),
            run: Identity([3, 4]),
            boot: Identity([5, 6]),
        };
        let writer = if identity {
            Writer::create_with_identity(
                &place,
                *shape,
                DictionaryCapacity {
                    entries: 5,
                    arena_bytes: 4096,
                },
                &run,
                0,
            )
            .unwrap()
        } else {
            Writer::create_recording(&place, *shape, &run, 0).unwrap()
        };
        let ring = writer.name().to_owned();
        let reader = RingReader::open_generation(&ring).unwrap();
        *output = Some(Box::new(Self {
            root,
            ring,
            writer: Some(writer),
            reader: Some(reader),
            tick: persistence::tick_buffer(shape).unwrap(),
            identities: Vec::with_capacity(usize::try_from(shape.process_capacity).unwrap()),
            identity,
        }));
    }

    pub fn publish(&mut self, second: u64, change: Change) {
        let mut processes = Vec::with_capacity(4);
        let mut identities = Vec::with_capacity(5);
        self.publish_members(second, change, &mut processes, &mut identities);
        let writer = self.writer.as_mut().unwrap();
        let memory = Memory::default();
        let readings = collector::ring::Readings {
            run_tick: second,
            cpus: &[],
            memory: &memory,
            interfaces: &[],
            ioreport: None,
            smc: None,
            processes: &processes,
            processes_refused: 1,
            processes_gone: 0,
            coalitions: &[],
            events: &[],
            dropped: Dropped::default(),
        };
        let times = ObservationTimes {
            wall_ns: 1_800_000_000_000_000_000 + second * 1_000_000_000,
            continuous_ns: (second + 1) * 1_000_000_000,
            awake_ns: (second + 1) * 1_000_000_000,
        };
        if self.identity {
            writer
                .publish_with_identity(
                    &readings,
                    Some(&times),
                    &IdentityPublication {
                        processes: &identities,
                        events: &[],
                    },
                )
                .unwrap();
        } else {
            writer.publish_observed(&readings, &times);
        }
    }

    fn publish_members(
        &mut self,
        second: u64,
        change: Change,
        processes: &mut Vec<Process>,
        identities: &mut Vec<IdentityObservation>,
    ) {
        let writer = self.writer.as_mut().unwrap();
        for (index, path) in PATHS.iter().enumerate() {
            let pid = i32::try_from(index + 7).unwrap();
            if index < 4 {
                process(index, second, processes);
            }
            if self.identity {
                let mut captured = None;
                collector::programs::split_program(path, &mut captured).unwrap();
                let mut program = None;
                writer
                    .intern_program(&captured.unwrap(), &mut program)
                    .unwrap();
                identities.push(IdentityObservation {
                    pid,
                    usage: if index == 4 {
                        UsageAvailability::Refused
                    } else {
                        UsageAvailability::Measured
                    },
                    identity: ProcessIdentity::Measured {
                        creation: ProcessCreation {
                            unique_id: if matches!(change, Change::PidReuse) {
                                u64::try_from(pid).unwrap() * 100 + 1
                            } else {
                                u64::try_from(pid).unwrap() * 100
                            },
                            pid_version: if matches!(change, Change::Exec) { 2 } else { 1 },
                        },
                        program: program.unwrap(),
                    },
                });
            }
        }
    }

    pub fn publish_refused_members(&mut self, count: u32, name: &[u8]) {
        let writer = self.writer.as_mut().unwrap();
        let mut program = None;
        writer
            .intern_program(
                &ProgramBytes {
                    executable_name: name,
                    app_path: None,
                },
                &mut program,
            )
            .unwrap();
        let identities: Vec<_> = (1..=count)
            .map(|pid| IdentityObservation {
                pid: i32::try_from(pid).unwrap(),
                usage: UsageAvailability::Refused,
                identity: ProcessIdentity::Measured {
                    creation: ProcessCreation {
                        unique_id: u64::from(pid),
                        pid_version: 1,
                    },
                    program: program.unwrap(),
                },
            })
            .collect();
        writer
            .publish_with_identity(
                &collector::ring::Readings {
                    run_tick: 0,
                    cpus: &[],
                    memory: &Memory::default(),
                    interfaces: &[],
                    ioreport: None,
                    smc: None,
                    processes: &[],
                    processes_refused: count,
                    processes_gone: 0,
                    coalitions: &[],
                    events: &[],
                    dropped: Dropped::default(),
                },
                Some(&ObservationTimes {
                    wall_ns: 1,
                    continuous_ns: 1,
                    awake_ns: 1,
                }),
                &IdentityPublication {
                    processes: &identities,
                    events: &[],
                },
            )
            .unwrap();
    }

    pub fn read(&mut self) -> u64 {
        let reader = self.reader.as_mut().unwrap();
        let found = if self.identity {
            reader.read_identity_into(&mut self.tick, &mut self.identities)
        } else {
            reader.read_into(&mut self.tick)
        }
        .unwrap();
        let Found::Publication { publication, .. } = found else {
            panic!("publication absent");
        };
        publication
    }

    pub fn capture(&mut self, output: &mut Option<Box<Frame>>) {
        let publication = self.read();
        Frame::capture(
            &Publication {
                reader: self.reader.as_ref().unwrap(),
                tick: &self.tick,
                identities: &self.identities,
                publication,
            },
            output,
        )
        .unwrap();
    }

    pub fn finish(&mut self) {
        self.writer.as_mut().unwrap().finish();
        drop(self.reader.take());
        drop(self.writer.take());
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        drop(self.reader.take());
        drop(self.writer.take());
        collector::ring::unlink_generation(&self.ring).unwrap();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn process(index: usize, second: u64, output: &mut Vec<Process>) {
    output.push(Process {
        pid: i32::try_from(index + 7).unwrap(),
        usage: Usage {
            awake_ns_at_start: 10,
            user_ns: 9_007_199_254_740_993 + [200_000_000, 300_000_000, 10, 20][index] * second,
            system_ns: 9_007_199_254_740_000,
            energy_nj: 9_007_199_254_741_993
                + [1_000_000_000, 2_000_000_000, 100, 200][index] * second,
            instructions: 0,
            super_instructions: 0,
            cycles: 0,
            idle_wakeups: 0,
            interrupt_wakeups: 0,
            pageins: 0,
            resident_bytes: [104_857_600, 52_428_800, 1000, 2000][index],
            footprint_bytes: [110_000_000, 55_000_000, 2000, 3000][index],
            disk_read_bytes: 0,
            disk_written_bytes: 0,
        },
        task: Task {
            threads: 1,
            faults: 0,
            context_switches: 0,
            mach_syscalls: 0,
            unix_syscalls: 0,
        },
    });
}
