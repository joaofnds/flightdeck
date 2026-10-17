use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use collector::machine::Memory;
use collector::processes::{Process, Task};
use collector::recording::private;
use collector::ring::{
    DictionaryCapacity, Dropped, Found, Identity, IdentityObservation, IdentityPublication,
    ObservationTimes, Place, ProcessCreation, ProcessIdentity, ProgramBytes, RingReader,
    RunIdentity, Shape, Tick, UsageAvailability, Writer,
};

use super::{Page, Sample};
use crate::live_apps::{Frame, Publication};

#[test]
fn owns_ring_facts_after_source_and_frames_are_retired() {
    // Goal: the presentation sample outlives the ring, scratch and borrowed aggregate.
    // Method: capture two real identity publications, retire all sources, then serialize.
    let mut fixture = Fixture::new();
    let previous = fixture.frame(1);
    let selected = fixture.frame(2);
    let mut sample = None;
    Sample::capture(&selected, Some(&previous), &mut sample).unwrap();
    drop(selected);
    drop(previous);
    drop(fixture);

    let sample = sample.unwrap();
    let result = sample
        .apps_page(Page {
            offset: 0,
            limit: 512,
        })
        .unwrap();

    assert_eq!(result["selected"]["stamp"]["publication"], "2");
    assert_eq!(result["previous"]["publication"], "1");
    assert_eq!(result["apps"][0]["cpu_ns"]["known"], "200000000");
    assert_eq!(result["apps"][0]["energy_nj"]["known"], "1000000000");
    assert_eq!(result["apps"][0]["resident_bytes"]["known"], "104857600");
    assert_eq!(result["apps"][0]["membership_complete"], false);
    assert_eq!(result["apps"][0]["key"]["bytes_hex"], "2f412e617070");
}

struct Fixture {
    root: PathBuf,
    ring: String,
    writer: Option<Writer>,
    reader: Option<RingReader>,
    tick: Tick,
    identities: Vec<IdentityObservation>,
}

static NEXT: AtomicU64 = AtomicU64::new(0);
const SHAPE: Shape = Shape {
    slot_count: 2,
    cpu_capacity: 1,
    interface_capacity: 1,
    core_capacity: 1,
    state_capacity: 1,
    fan_capacity: 1,
    process_capacity: 1,
    coalition_capacity: 1,
    event_capacity: 1,
};

impl Fixture {
    fn new() -> Self {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "flightdeck-live-app-response-{}-{serial}",
            std::process::id()
        ));
        private::create_private_dir(&root).unwrap();
        let writer = Writer::create_with_identity(
            &Place {
                lock_path: root.join("ring.lock"),
                name_prefix: format!("/fdlar{}x{serial}", std::process::id()),
            },
            SHAPE,
            DictionaryCapacity {
                entries: 1,
                arena_bytes: 64,
            },
            &RunIdentity {
                recording: Identity([1, 2]),
                run: Identity([3, 4]),
                boot: Identity([5, 6]),
            },
            0,
        )
        .unwrap();
        let ring = writer.name().to_owned();
        let reader = RingReader::open_generation(&ring).unwrap();

        Self {
            root,
            ring,
            writer: Some(writer),
            reader: Some(reader),
            tick: persistence::tick_buffer(&SHAPE).unwrap(),
            identities: Vec::with_capacity(1),
        }
    }

    fn frame(&mut self, second: u64) -> Box<Frame> {
        let writer = self.writer.as_mut().unwrap();
        let mut program = None;
        writer
            .intern_program(
                &ProgramBytes {
                    executable_name: b"main\xff",
                    app_path: Some(b"/A.app"),
                },
                &mut program,
            )
            .unwrap();

        let processes = [frame_process(second)];
        let identities = [IdentityObservation {
            pid: 42,
            usage: UsageAvailability::Measured,
            identity: ProcessIdentity::Measured {
                creation: ProcessCreation {
                    unique_id: 9_007_199_254_740_993,
                    pid_version: 1,
                },
                program: program.unwrap(),
            },
        }];

        writer
            .publish_with_identity(
                &collector::ring::Readings {
                    run_tick: second,
                    cpus: &[],
                    memory: &Memory::default(),
                    interfaces: &[],
                    ioreport: None,
                    smc: None,
                    processes: &processes,
                    processes_refused: 0,
                    processes_gone: 0,
                    coalitions: &[],
                    events: &[],
                    dropped: Dropped::default(),
                },
                Some(&ObservationTimes {
                    wall_ns: 1_800_000_000_000_000_000 + second,
                    continuous_ns: second * 1_000_000_000,
                    awake_ns: second * 1_000_000_000,
                }),
                &IdentityPublication {
                    processes: &identities,
                    events: &[],
                },
            )
            .unwrap();

        self.frame_read()
    }

    fn frame_read(&mut self) -> Box<Frame> {
        let reader = self.reader.as_mut().unwrap();
        let Found::Publication { publication, .. } = reader
            .read_identity_into(&mut self.tick, &mut self.identities)
            .unwrap()
        else {
            panic!("publication absent")
        };
        let mut frame = None;
        Frame::capture(
            &Publication {
                reader,
                tick: &self.tick,
                identities: &self.identities,
                publication,
            },
            &mut frame,
        )
        .unwrap();

        frame.unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.writer.as_mut().unwrap().finish();
        drop(self.reader.take());
        drop(self.writer.take());
        collector::ring::unlink_generation(&self.ring).unwrap();
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

fn frame_process(second: u64) -> Process {
    Process {
        pid: 42,
        usage: super::tests::fixture::usage(
            200_000_000 * second,
            1_000_000_000 * second,
            104_857_600,
        ),
        task: Task {
            threads: 1,
            faults: 0,
            context_switches: 0,
            mach_syscalls: 0,
            unix_syscalls: 0,
        },
    }
}
