use collector::machine::Memory;
use collector::processes::{Process, Task, Usage};
use collector::ring::{
    DictionaryCapacity, Dropped, Found, IdentityObservation, IdentityPublication, ObservationTimes,
    Place, ProcessCreation, ProcessIdentity, ProgramBytes, Readings, RingReader, UsageAvailability,
    Writer,
};
use serde_json::json;

use super::SealedRecording;
use crate::live_source::{AppKey, Request};

#[test]
fn real_named_process_worker_preserves_scope_and_exact_same_second_history() {
    // Seal two executable names in one App and carry the selected name through the public worker.
    let recording = named_recording();
    let request = named_request();

    let snapshot = recording.read(request.clone()).unwrap();

    assert_eq!(snapshot.selection["selected"]["publication"], 2);
    let presentation = crate::live_acceptance::Presentation::prepare(&request, snapshot).unwrap();
    let history = presentation.snapshot.process_history.as_ref().unwrap();
    let payload = history.payload().unwrap();
    assert_eq!(payload["scope"], "selected_named_process_group");
    assert_eq!(payload["executable_name"]["bytes_hex"], "61");
    assert_eq!(payload["selected"]["stamp"]["publication"], "2");
    let group = presentation
        .panels
        .iter()
        .filter(|panel| panel.id.starts_with("live-process-group-"))
        .collect::<Vec<_>>();
    assert_eq!(group.len(), 5);
    assert_eq!(group[0].selected_index, Some(1));
    assert_eq!(group[0].lines[0].values[1], Some(120.0));
    assert_eq!(group[3].lines[0].values[1], Some(1.25));
    assert_eq!(group[4].lines[0].values[1], Some(0.000_08));
    let total = presentation
        .panels
        .iter()
        .find(|panel| panel.id == "live-app-cpu")
        .unwrap();
    assert_eq!(total.lines[0].values[1], Some(360.0));
}

#[test]
fn real_named_process_preview_retains_context_without_reloading_the_group() {
    // Exact preview details use the accepted group Arc while the worker omits its costly context.
    let recording = named_recording();
    let request = named_request();
    let accepted = recording.read(request.clone()).unwrap();
    let target =
        crate::live_hover::Target::exact(1000, &json!({"run":[1,2],"boot":[3,4],"publication":1}))
            .unwrap();

    let preview = crate::live_hover::Preview::resident(&accepted, &request, target);

    assert!(!preview.request.load_process_history);
    assert_eq!(preview.request.executable.as_deref(), Some("hex:61"));
    assert!(std::sync::Arc::ptr_eq(
        accepted.process_history.as_ref().unwrap(),
        preview.reading.process_history.as_ref().unwrap()
    ));
    let exact = recording.read(preview.request).unwrap();
    assert!(exact.process_history.is_none());
    assert_eq!(exact.selection["selected"]["publication"], 1);
}

#[test]
fn real_named_process_history_includes_members_outside_the_loaded_page() {
    // Read page two containing only b, while a's captured history comes from page one.
    let recording = named_recording_with_members(513);
    let mut request = named_request();
    request.members_offset = 512;

    let snapshot = recording.read(request).unwrap();

    let detail = snapshot.detail.as_ref().unwrap();
    assert_eq!(detail["page"]["offset"], 512);
    assert_eq!(detail["page"]["total"], 513);
    assert_eq!(detail["processes"].as_array().unwrap().len(), 1);
    assert_eq!(detail["processes"][0]["executable_name"]["bytes_hex"], "62");
    let payload = snapshot
        .process_history
        .as_ref()
        .unwrap()
        .payload()
        .unwrap();
    assert_eq!(payload["selected"]["stamp"]["publication"], "2");
    assert_eq!(payload["executable_name"]["bytes_hex"], "61");
    let point = &payload["points"][1];
    assert_eq!(point["cpu_ns"]["known"], "1200000000");
    assert_eq!(point["cpu_percent_one_core"], 120.0);
    assert_eq!(point["resident_bytes"]["known"], "2048");
    assert_eq!(point["footprint_bytes"]["known"], "4096");
    assert_eq!(point["energy_watts"], 1.25);
    assert_eq!(point["disk_read_bps"], 80.0);
    assert_eq!(point["disk_written_bps"], 40.0);
}

fn named_request() -> Request {
    let mut request = SealedRecording::request();
    request.route = "apps".into();
    request.second = Some(1001);
    request.identity = Some(json!({"run":"00000000000000010000000000000002",
        "boot":"00000000000000030000000000000004","publication":2}));
    request.app = Some(AppKey {
        kind: "bundle".into(),
        bytes_hex: "2f4170706c69636174696f6e732f4578616d706c652e617070".into(),
    });
    request.executable = Some("hex:61".into());
    request.load_process_history = true;
    request
}

fn named_recording() -> SealedRecording {
    named_recording_with_members(2)
}

fn named_recording_with_members(members: u32) -> SealedRecording {
    assert!(members <= 1024);
    let recording = SealedRecording::empty();
    let store = persistence::Store::open(&recording.root).unwrap();
    let identity = persistence::WindowIdentity {
        run: collector::ring::RunIdentity {
            recording: store.recording(),
            run: collector::ring::Identity([1, 2]),
            boot: collector::ring::Identity([3, 4]),
        },
        window: 0,
    };
    let shape = collector::ring::Shape {
        process_capacity: u64::from(members),
        ..SealedRecording::shape()
    };
    let place = Place {
        lock_path: recording.root.join("named.lock"),
        name_prefix: "fdnamed".into(),
    };
    let mut generation = NamedGeneration(
        Writer::create_with_identity(
            &place,
            shape,
            DictionaryCapacity {
                entries: 2,
                arena_bytes: 8192,
            },
            &identity.run,
            0,
        )
        .unwrap(),
    );
    let identities = named_identities(&mut generation.0, members);
    let mut reader = RingReader::open(&place.lock_path).unwrap();
    let mut window =
        persistence::Window::new(identity, persistence::Limits::from_shape(&shape).unwrap());
    for publication in 1..=3 {
        named_append(
            &mut generation.0,
            &mut reader,
            &mut window,
            &identities,
            publication,
        );
    }
    SealedRecording::commit(&store, identity, &window);
    drop(reader);
    drop(generation);
    drop(store);
    recording
}

struct NamedGeneration(Writer);

impl Drop for NamedGeneration {
    fn drop(&mut self) {
        collector::ring::unlink_generation(self.0.name()).unwrap();
    }
}

fn named_identities(writer: &mut Writer, members: u32) -> Vec<IdentityObservation> {
    assert!(members <= 1024);
    let mut identities = Vec::with_capacity(usize::try_from(members).unwrap());
    for pid in 1..=i32::try_from(members).unwrap() {
        let name = if pid == 1 { b"a" } else { b"b" };
        let mut program = None;
        writer
            .intern_program(
                &ProgramBytes {
                    executable_name: name,
                    app_path: Some(b"/Applications/Example.app"),
                },
                &mut program,
            )
            .unwrap();
        identities.push(IdentityObservation {
            pid,
            usage: UsageAvailability::Measured,
            identity: ProcessIdentity::Measured {
                creation: ProcessCreation {
                    unique_id: u64::try_from(pid).unwrap(),
                    pid_version: 1,
                },
                program: program.unwrap(),
            },
        });
    }
    identities
}

fn named_append(
    writer: &mut Writer,
    reader: &mut RingReader,
    window: &mut persistence::Window,
    identities: &[IdentityObservation],
    publication: u64,
) {
    let processes: Vec<_> = identities
        .iter()
        .map(|identity| named_process(identity.pid, publication))
        .collect();
    let cpus = named_cpus(publication);
    let times = ObservationTimes {
        wall_ns: match publication {
            1 => 1_000_000_000_000,
            2 => 1_001_100_000_000,
            3 => 1_001_900_000_000,
            _ => unreachable!(),
        },
        continuous_ns: publication * 1_000_000_000,
        awake_ns: publication * 1_000_000_000,
    };
    writer
        .publish_with_identity(
            &Readings {
                run_tick: publication - 1,
                cpus: &cpus,
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
            Some(&times),
            &IdentityPublication {
                processes: identities,
                events: &[],
            },
        )
        .unwrap();
    let mut tick = persistence::tick_buffer(&reader.shape()).unwrap();
    let mut captured =
        Vec::with_capacity(usize::try_from(reader.shape().process_capacity).unwrap());
    assert!(matches!(
        reader.read_identity_into(&mut tick, &mut captured).unwrap(),
        Found::Publication { .. }
    ));
    window
        .push_tick_with_identity(
            &persistence::Observation {
                publication,
                wall_ns: Some(times.wall_ns),
                continuous_ns: Some(times.continuous_ns),
                awake_ns: Some(times.awake_ns),
            },
            &tick,
            &[],
            &persistence::IdentitySource {
                reader,
                processes: &captured,
                events: &[],
            },
        )
        .unwrap();
}

fn named_cpus(publication: u64) -> [collector::machine::CPUTicks; 1] {
    let counter = u32::try_from(publication).unwrap();
    [collector::machine::CPUTicks {
        user: counter * 10,
        system: 0,
        nice: 0,
        idle: counter * 90,
    }]
}

fn named_process(pid: i32, publication: u64) -> Process {
    let factor = u64::try_from(pid).unwrap() * publication;
    Process {
        pid,
        usage: Usage {
            awake_ns_at_start: 0,
            user_ns: factor * 1_200_000_000,
            system_ns: 0,
            energy_nj: factor * 1_250_000_000,
            instructions: 0,
            super_instructions: 0,
            cycles: 0,
            idle_wakeups: 0,
            interrupt_wakeups: 0,
            pageins: 0,
            resident_bytes: factor * 1024,
            footprint_bytes: factor * 2048,
            disk_read_bytes: factor * 80,
            disk_written_bytes: factor * 40,
        },
        task: Task {
            threads: 1,
            faults: 0,
            context_switches: 0,
            mach_syscalls: 0,
            unix_syscalls: 0,
        },
    }
}
