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

use super::{Detail, Page, Sample};
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

#[test]
fn owns_exact_disk_deltas_and_rates_after_the_ring_is_retired() {
    // Goal: cumulative counters above float precision retain exact interval disk amounts.
    // Method: retire two real identity-ring publications, then inspect owned App/process JSON.
    let mut fixture = Fixture::new();
    let previous = fixture.frame(1);
    let selected = fixture.frame(2);
    let mut sample = None;
    Sample::capture(&selected, Some(&previous), &mut sample).unwrap();
    drop(selected);
    drop(previous);
    drop(fixture);
    let sample = sample.unwrap();

    let page = sample
        .apps_page(Page {
            offset: 0,
            limit: 512,
        })
        .unwrap();
    let detail = sample
        .app_detail(&Detail {
            selected: &sample.selected,
            app_index: 0,
            page: Page {
                offset: 0,
                limit: 512,
            },
        })
        .unwrap();

    assert_eq!(
        page["apps"][0]["disk_read_bytes"],
        serde_json::json!({
            "known":"4096", "measured_members":1, "unavailable_members":0, "overflowed":false
        })
    );
    assert_eq!(detail["app"], page["apps"][0]);
    assert_eq!(detail["app"]["disk_written_bytes"]["known"], "1024");
    assert_eq!(detail["app"]["disk_io_bytes"]["known"], "5120");
    assert_eq!(detail["app"]["disk_read_bps"], 4096.0);
    assert_eq!(detail["app"]["disk_written_bps"], 1024.0);
    assert_eq!(detail["app"]["disk_io_bps"], 5120.0);
    assert_eq!(
        detail["processes"][0]["disk_read_bytes"],
        serde_json::json!({
            "known":"4096", "unavailable":null
        })
    );
    assert_eq!(
        detail["processes"][0]["disk_written_bytes"]["known"],
        "1024"
    );
    assert_eq!(detail["processes"][0]["disk_io_bytes"]["known"], "5120");
    assert_eq!(detail["processes"][0]["disk_read_bps"], 4096.0);
    assert_eq!(detail["processes"][0]["disk_written_bps"], 1024.0);
    assert_eq!(detail["processes"][0]["disk_io_bps"], 5120.0);
}

#[test]
fn retains_a_full_rolling_disk_history_from_real_ring_publications() {
    // Goal: compact disk history retains exactly the latest 900 elapsed seconds after rollover.
    // Method: capture 901 actual identity publications, retire sources and serialize the trace.
    let mut fixture = Fixture::new();
    let mut previous = None;
    let mut sample = None;
    let mut history = crate::live_app_history::History::new();
    for second in 1..=901 {
        let selected = fixture.frame(second);
        Sample::capture(&selected, previous.as_deref(), &mut sample).unwrap();
        let mut point = None;
        history.capture(sample.as_deref().unwrap(), 0, &mut point);
        let first = (1_800_000_000 + second).saturating_sub(899) * 1_000_000_000;
        history.push(point.unwrap(), first);
        previous = Some(selected);
    }
    let selected = sample.unwrap().selected;
    drop(previous);
    drop(fixture);
    let cancellation = std::sync::Arc::new(crate::Cancellation::default());
    let operation = cancellation.operation(0).unwrap();

    let response = history
        .response(
            &crate::live_app_response::Selected::ending_at(selected).unwrap(),
            &crate::live_app_history::Target {
                kind: super::KeyKind::Bundle,
                bytes: b"/A.app",
            },
            &operation,
        )
        .unwrap();

    let points = response["points"].as_array().unwrap();
    assert_eq!(points.len(), 900);
    assert_eq!(points[0]["observation"]["stamp"]["publication"], "2");
    assert_eq!(points[899]["observation"]["stamp"]["publication"], "901");
    assert_eq!(points[0]["disk_io_bytes"]["known"], "5120");
    assert_eq!(points[899]["disk_read_bps"], 4096.0);
    assert_eq!(points[899]["disk_written_bps"], 1024.0);
    assert_eq!(points[899]["disk_io_bps"], 5120.0);
    assert_eq!(response["coverage"]["retention_limited"], false);
    assert_eq!(response["coverage"]["cache_bytes_max"], 67_108_864);
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
                    wall_ns: 1_800_000_000_000_000_000 + second * 1_000_000_000,
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
    let mut usage =
        super::tests::fixture::usage(200_000_000 * second, 1_000_000_000 * second, 104_857_600);
    usage.disk_read_bytes = 9_007_199_254_740_993 + second * 4096;
    usage.disk_written_bytes = 9_007_199_254_741_993 + second * 1024;
    Process {
        pid: 42,
        usage,
        task: Task {
            threads: 1,
            faults: 0,
            context_switches: 0,
            mach_syscalls: 0,
            unix_syscalls: 0,
        },
    }
}
