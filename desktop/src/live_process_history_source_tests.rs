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
    assert_eq!(group.len(), 8);
    assert_eq!(group[0].selected_index, Some(1));
    assert_eq!(group[0].lines[0].values[1], Some(120.0));
    assert_eq!(group[3].lines[0].values[1], Some(0.000_08));
    assert_eq!(group[4].lines[0].values[1], Some(1.25));
    let total = presentation
        .panels
        .iter()
        .find(|panel| panel.id == "live-app-cpu")
        .unwrap();
    assert_eq!(total.lines[0].values[1], Some(360.0));
}

#[test]
fn a_named_process_group_reads_in_the_prototypes_by_process_charts() {
    // Goal: as in the prototype, choosing one group keeps the same charts with only its
    // series. Method: project the named group a and read the charts' titles and lines.
    let recording = named_recording();
    let request = named_request();
    let snapshot = recording.read(request.clone()).unwrap();

    let presentation = crate::live_acceptance::Presentation::prepare(&request, snapshot).unwrap();

    let group: Vec<_> = presentation
        .panels
        .iter()
        .filter(|panel| panel.id.starts_with("live-process-group-"))
        .collect();
    let titles: Vec<_> = group.iter().map(|panel| panel.title.as_str()).collect();
    assert_eq!(
        titles,
        [
            "CPU by process",
            "Resident memory by process",
            "Footprint by process",
            "Disk by process",
            "Power by process",
            "Idle wakeups by process",
            "Page-ins by process",
            "Threads by process"
        ]
    );
    let lines: Vec<_> = group
        .iter()
        .flat_map(|panel| panel.lines.iter())
        .map(|line| (line.name.as_str(), line.negative))
        .collect();
    let read = ("a", Some(false));
    assert_eq!(
        lines,
        [
            read,
            read,
            read,
            ("a read", Some(false)),
            ("a written", Some(true)),
            read,
            read,
            read,
            read
        ]
    );
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
fn an_apps_hover_read_brings_the_apps_detail_without_its_history() {
    // Goal: a hover on the Apps page reads the hovered second's App detail and leaves the App's
    // history unread, since the page's charts already hold it.
    // Method: read the second of three saved observations of a named App as a hover's read
    // asks for it, through the worker over a real sealed recording.
    let recording = named_recording();
    let mut request = named_request();
    request.load_resident_history = false;
    request.load_app_attribution_history = false;
    request.load_process_history = false;

    let snapshot = recording.inspect(request).unwrap();

    assert_eq!(snapshot.selection["selected"]["publication"], 2);
    let detail = snapshot.detail.as_ref().unwrap();
    assert_eq!(detail["processes"].as_array().unwrap().len(), 2);
    assert!(snapshot.history.is_none());
    assert!(snapshot.process_history.is_none());
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

#[test]
fn default_all_process_groups_loads_twelve_histories_before_app_totals() {
    // Read a sealed two-name recording through the real worker with the default group selection.
    let recording = named_recording();
    let mut request = named_request();
    request.executable = None;

    let snapshot = recording.read(request.clone()).unwrap();

    assert!(
        snapshot.process_history.is_some(),
        "Default All process groups loads captured history"
    );
    let presentation = crate::live_acceptance::Presentation::prepare(&request, snapshot).unwrap();
    let panels: Vec<_> = presentation
        .panels
        .iter()
        .filter(|panel| panel.id.starts_with("live-process-groups-"))
        .collect();
    assert_eq!(
        panels
            .iter()
            .map(|panel| panel.id.as_str())
            .collect::<Vec<_>>(),
        [
            "live-process-groups-cpu",
            "live-process-groups-resident",
            "live-process-groups-footprint",
            "live-process-groups-network",
            "live-process-groups-disk",
            "live-process-groups-gpu",
            "live-process-groups-power",
            "live-process-groups-wakeups",
            "live-process-groups-pageins",
            "live-process-groups-threads",
            "live-process-groups-files",
            "live-process-groups-sockets"
        ]
    );
    assert_eq!(panels[0].lines.len(), 2);
    assert_eq!(panels[4].lines.len(), 4);
    assert_eq!(panels[0].selected_index, Some(1));
    assert_eq!(panels[0].lines[1].values[1], Some(240.0));
    assert_eq!(presentation.panels[0].id, "live-process-groups-cpu");
}

#[test]
fn all_groups_disk_draws_written_bytes_below_zero() {
    // Goal: as in the prototype, each group's reads rise above the axis and its writes fall
    // below it. Method: project the two-name recording and read the disk lines' directions.
    let recording = named_recording();
    let mut request = named_request();
    request.executable = None;
    let snapshot = recording.read(request.clone()).unwrap();

    let presentation = crate::live_acceptance::Presentation::prepare(&request, snapshot).unwrap();

    let disk = presentation
        .panels
        .iter()
        .find(|panel| panel.id == "live-process-groups-disk")
        .unwrap();
    let directions: Vec<_> = disk.lines.iter().map(|line| line.negative).collect();
    assert_eq!(
        directions,
        [Some(false), Some(false), Some(true), Some(true)]
    );
    assert_eq!(disk.minimum, None);
}

#[test]
fn all_groups_inspector_uses_the_exact_shared_same_second_observation() {
    // Two observations share a second; all panels and exact Disk values use publication two.
    let recording = named_recording();
    let mut request = named_request();
    request.executable = None;
    let snapshot = recording.read(request).unwrap();
    let history = snapshot.process_history.as_ref().unwrap().all().unwrap();
    let target =
        crate::live_hover::Target::exact(1001, &json!({"run":[1,2],"boot":[3,4],"publication":2}))
            .unwrap();

    let index = crate::live_process_history::groups::index(history, &target);
    let reading =
        crate::live_process_history::groups::reading(history, "live-process-groups-disk", 1, index);
    let written =
        crate::live_process_history::groups::reading(history, "live-process-groups-disk", 3, index);

    assert_eq!(index, Some(1));
    assert_eq!(reading, "160 B/s");
    assert_eq!(written, "80 B/s");
    assert_eq!(
        history.points[index.unwrap()].observation.stamp.publication,
        2
    );
}

#[test]
fn all_groups_preview_retains_the_accepted_context_without_reloading_it() {
    // Exact preview detail loading keeps the same accepted cohort Arc used by the charts.
    let recording = named_recording();
    let mut request = named_request();
    request.executable = None;
    let snapshot = recording.read(request.clone()).unwrap();
    let target =
        crate::live_hover::Target::exact(1000, &json!({"run":[1,2],"boot":[3,4],"publication":1}))
            .unwrap();

    let preview = crate::live_hover::Preview::resident(&snapshot, &request, target);

    assert!(preview.request.executable.is_none());
    assert!(!preview.request.load_process_history);
    assert!(std::sync::Arc::ptr_eq(
        snapshot.process_history.as_ref().unwrap(),
        preview.reading.process_history.as_ref().unwrap()
    ));
}

#[test]
fn all_groups_history_ignores_the_loaded_member_page() {
    // A page containing only b cannot remove a from the all-groups cohort.
    let recording = named_recording_with_members(513);
    let mut request = named_request();
    request.executable = None;
    request.members_offset = 512;

    let snapshot = recording.read(request).unwrap();

    assert_eq!(
        snapshot.detail.as_ref().unwrap()["processes"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        snapshot
            .process_history
            .as_ref()
            .unwrap()
            .all()
            .unwrap()
            .named_group_count,
        2
    );
}

#[test]
fn all_groups_readouts_use_historical_rates() {
    // Goal: each reading is a rate over its own observation's interval.
    // Method: give an earlier observation a longer interval and read both observations.
    let mut history = all_history();
    let selected = crate::live_process_history::groups::reading(
        &history,
        "live-process-groups-cpu",
        1,
        Some(1),
    );
    assert_eq!(selected, "240%");
    history.points[0].readings[0] = history.points[1].readings[0];
    history.points[0].elapsed_ns = Some(2_000_000_000);
    let historical = crate::live_process_history::groups::reading(
        &history,
        "live-process-groups-cpu",
        1,
        Some(0),
    );
    assert_eq!(historical, "120%");
    history.points[0].readings[0].measures[2].known = 9_007_199_254_740_993;
    let disk = crate::live_process_history::groups::reading(
        &history,
        "live-process-groups-disk",
        1,
        Some(0),
    );
    assert_eq!(disk, "4,503,599,616 MB/s");
    history.points[0].readings[0].measures[3].known = 9_007_199_254_740_995;
    let written = crate::live_process_history::groups::reading(
        &history,
        "live-process-groups-disk",
        history.groups.len() + 1,
        Some(0),
    );
    assert_eq!(written, "4,503,599,616 MB/s");
    let power = crate::live_process_history::groups::reading(
        &history,
        "live-process-groups-power",
        1,
        Some(1),
    );
    assert_eq!(power, "2.5 W");
}

#[test]
fn all_groups_unavailable_elapsed_keeps_known_bytes_and_partial_coverage() {
    // Missing timing prevents a rate, while known bytes and honest member coverage survive.
    let mut history = all_history();
    history.points[1].elapsed_ns = None;
    let disk = crate::live_process_history::groups::reading(
        &history,
        "live-process-groups-disk",
        1,
        Some(1),
    );
    assert_eq!(disk, "Unavailable");
    history.points[1].readings[0].measures[2].unavailable_members = 2;
    let partial = crate::live_process_history::groups::reading(
        &history,
        "live-process-groups-disk",
        1,
        Some(1),
    );
    assert_eq!(
        partial,
        "Unavailable · known subtotal · 1 measured · 2 unavailable"
    );
}

#[test]
fn all_groups_small_nonzero_readouts_preserve_visible_rates() {
    // Use the shared quantity formatter so small captured rates cannot display as zero.
    let mut history = all_history();
    history.points[1].readings[0].measures[1].known = 1_000_000;
    history.points[1].readings[0].measures[0].known = 10_000;
    let power = crate::live_process_history::groups::reading(
        &history,
        "live-process-groups-power",
        1,
        Some(1),
    );
    assert_eq!(power, "1 mW");
    let cpu = crate::live_process_history::groups::reading(
        &history,
        "live-process-groups-cpu",
        1,
        Some(1),
    );
    assert_eq!(cpu, "0.001%");
}

pub(crate) fn all_history() -> Box<viewer::app_process_groups_history::AppProcessGroupsHistory> {
    let recording = named_recording();
    let mut request = named_request();
    request.executable = None;
    let snapshot = recording.read(request).unwrap();
    match std::sync::Arc::try_unwrap(snapshot.process_history.unwrap()) {
        Ok(crate::live_process_history::ProcessHistory::All { payload, .. }) => payload,
        _ => panic!("Expected uniquely owned All history"),
    }
}

#[test]
fn captured_process_sources_project_all_history_cards() {
    // The real saved worker supplies eight captured cards for named groups and twelve for All
    // groups.
    let recording = named_recording();
    let named = named_request();
    let mut all = named.clone();
    all.executable = None;

    let named_snapshot = recording.read(named.clone()).unwrap();
    let all_snapshot = recording.read(all.clone()).unwrap();
    let named_view = crate::live_acceptance::Presentation::prepare(&named, named_snapshot).unwrap();
    let all_view = crate::live_acceptance::Presentation::prepare(&all, all_snapshot).unwrap();

    assert_eq!(
        named_view
            .panels
            .iter()
            .filter(|panel| panel.id.starts_with("live-process-group-"))
            .count(),
        8
    );
    assert_eq!(
        all_view
            .panels
            .iter()
            .filter(|panel| panel.id.starts_with("live-process-groups-"))
            .count(),
        12
    );
    assert!(
        all_view
            .panels
            .iter()
            .any(|panel| panel.id == "live-process-groups-wakeups")
    );
    assert!(
        all_view
            .panels
            .iter()
            .any(|panel| panel.id == "live-process-groups-pageins")
    );
    assert!(
        all_view
            .panels
            .iter()
            .any(|panel| panel.id == "live-process-groups-threads")
    );
}

#[test]
fn added_process_readings_need_their_interval() {
    // Goal: a wakeup rate reads only where its observation has an interval to divide by.
    // Method: read one large count with its interval, then without it.
    let mut history = all_history();
    let reading = &mut history.points[1].readings[0];
    reading.measures.get_mut(7).unwrap().known = 9_007_199_254_740_993;

    let wakeups = crate::live_process_history::groups::reading(
        &history,
        "live-process-groups-wakeups",
        1,
        Some(1),
    );
    history.points[1].elapsed_ns = None;
    let unavailable = crate::live_process_history::groups::reading(
        &history,
        "live-process-groups-wakeups",
        1,
        Some(1),
    );

    assert!(wakeups.ends_with("/s"), "{wakeups}");
    assert_eq!(unavailable, "Unavailable");
}

#[test]
fn all_thread_gauge_preserves_one_exact_quantity_without_elapsed() {
    // The integer gauge needs neither an elapsed interval nor a rounded duplicate.
    let mut history = all_history();
    history.points[1].elapsed_ns = None;
    let reading = &mut history.points[1].readings[0];
    reading.measures[9] = reading.measures[0];
    reading.measures[9].known = 9_007_199_254_740_993;

    let actual = crate::live_process_history::groups::reading(
        &history,
        "live-process-groups-threads",
        1,
        Some(1),
    );

    assert_eq!(actual, "9007199254740993 threads");
}
