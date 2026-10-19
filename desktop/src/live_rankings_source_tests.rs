use collector::machine::Memory;
use collector::processes::{Process, Task, Usage};
use collector::ring::{
    DictionaryCapacity, Dropped, Found, IdentityObservation, IdentityPublication, ObservationTimes,
    Place, ProcessCreation, ProcessIdentity, ProgramBytes, Readings, RingReader, UsageAvailability,
    Writer,
};
use serde_json::json;

use super::SealedRecording;
use crate::live_rankings::{Rankings, TopApps};
use crate::live_source::{Request, Snapshot};

#[test]
fn real_saved_energy_rankings_reach_later_pages_at_the_exact_sibling() {
    // Read the actual Energy route from 600 sealed Apps after its source ring retires.
    let recording = SealedRecording::with_ranked_apps();
    let request = exact_ranked_request("energy");

    let snapshot = recording.read(request).unwrap();

    let rankings = assert_exact_ranked_snapshot(&snapshot);
    assert_eq!(rankings.metrics[2].entries[0].app.name, "App00600");
    assert_eq!(rankings.metrics[2].entries[0].amount, 600_000);
}

#[test]
fn real_saved_cpu_rankings_reach_later_pages_at_the_exact_sibling() {
    // Read the actual CPU route from 600 sealed Apps after its source ring retires.
    let recording = SealedRecording::with_ranked_apps();
    let request = exact_ranked_request("cpu");

    let snapshot = recording.read(request).unwrap();

    let rankings = assert_exact_ranked_snapshot(&snapshot);
    assert_eq!(rankings.metrics[0].entries[0].app.name, "App00600");
    assert_eq!(rankings.metrics[0].entries[0].amount, 600);
}

#[test]
fn real_saved_memory_rankings_reach_later_pages_at_the_exact_sibling() {
    // Read the actual Memory route from 600 sealed Apps after its source ring retires.
    let recording = SealedRecording::with_ranked_apps();
    let request = exact_ranked_request("memory");

    let snapshot = recording.read(request).unwrap();

    let rankings = assert_exact_ranked_snapshot(&snapshot);
    assert_eq!(rankings.metrics[1].entries[0].app.name, "App00600");
    assert_eq!(rankings.metrics[1].entries[0].amount, 1_228_800);
}

#[test]
fn real_saved_app_rankings_reach_later_pages_and_preserve_the_exact_sibling() {
    // Read 600 captured App identities through the real worker after their source ring retires.
    let recording = SealedRecording::with_ranked_apps();
    let mut request = SealedRecording::request();
    request.second = Some(1001);
    request.identity = Some(json!({"run":"00000000000000010000000000000002",
        "boot":"00000000000000030000000000000004","publication":2}));

    let snapshot = recording.read(request).unwrap();

    let Rankings::Ready(rankings) = snapshot.rankings.as_deref().unwrap() else {
        panic!("{:?}", snapshot.rankings);
    };
    crate::live_source::identity_validate(&rankings.selected, &snapshot.selection["selected"])
        .unwrap();
    assert_eq!(rankings.selected["publication"], 2);
    assert_eq!(rankings.app_count, 600);
    assert_eq!(rankings.metrics[0].entries[0].app.name, "App00600");
    assert_eq!(rankings.metrics[0].entries[0].amount, 600);
    assert_eq!(rankings.metrics[1].entries[0].amount, 1_228_800);
    assert_eq!(rankings.metrics[2].entries[0].amount, 600_000);
    assert_eq!(rankings.metrics[3].entries[0].app.name, "App00600");
    assert_eq!(rankings.metrics[3].entries[0].amount, 900_000_000);
    assert_eq!(rankings.metrics[3].entries[0].rate, Some(900_000_000.0));
}

#[test]
fn real_saved_storage_rankings_reach_later_pages_at_the_exact_sibling() {
    // The actual Storage request must hydrate its disk ranks through the worker, not a UI seed.
    let recording = SealedRecording::with_ranked_apps();
    let mut request = SealedRecording::request();
    request.route = "storage".into();
    request.second = Some(1001);
    request.identity = Some(json!({"run":"00000000000000010000000000000002",
        "boot":"00000000000000030000000000000004","publication":2}));

    let snapshot = recording.read(request).unwrap();

    let Rankings::Ready(rankings) = snapshot.rankings.as_deref().unwrap() else {
        panic!("{:?}", snapshot.rankings);
    };
    crate::live_source::identity_validate(&rankings.selected, &snapshot.selection["selected"])
        .unwrap();
    assert_eq!(snapshot.selection["second"], 1001);
    assert_eq!(rankings.selected["publication"], 2);
    assert_eq!(rankings.app_count, 600);
    assert_eq!(rankings.metrics[3].entries[0].app.name, "App00600");
    assert_eq!(rankings.metrics[3].entries[0].amount, 900_000_000);
    assert_eq!(rankings.metrics[3].entries[0].rate, Some(900_000_000.0));
}

#[test]
fn legacy_identity_absence_keeps_overview_scalar_readings_available() {
    // A valid old source has no App identity extension and must not hide its memory reading.
    let recording = SealedRecording::new();

    let snapshot = recording.read(SealedRecording::request()).unwrap();

    assert_eq!(snapshot.selection["summary"]["memory_total_bytes"], 12_288);
    let Rankings::Ready(rankings) = snapshot.rankings.as_deref().unwrap() else {
        panic!("{:?}", snapshot.rankings);
    };
    assert_eq!(rankings.app_count, 0);
    assert!(rankings.metrics[0].entries.is_empty());
}

#[test]
fn a_saved_gap_has_no_app_ranking_payload() {
    // No selected observation means no App query and no borrowed ranking from a neighboring tick.
    let recording = SealedRecording::with_ranked_apps();
    let mut request = SealedRecording::request();
    request.second = Some(1002);

    let snapshot = recording.read(request).unwrap();

    assert!(snapshot.selection["selected"].is_null());
    assert!(snapshot.rankings.is_none());
}

#[test]
fn failed_ranked_app_setup_retires_its_created_generation() {
    // Panic just after creation, then check the exact generation through the POSIX reader API.
    let mut generation_name = None;
    let setup = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        SealedRecording::with_ranked_apps_after_create(|name| {
            generation_name = Some(name.to_owned());
            panic!("injected ranked App fixture setup failure");
        })
    }));
    assert!(setup.is_err());
    let name = generation_name.unwrap();
    let error_kind = match RingReader::open_generation(&name) {
        Ok(reader) => {
            drop(reader);
            collector::ring::unlink_generation(&name).unwrap();
            eprintln!("cleaned leaked baseline generation {name}");
            None
        }
        Err(error) => Some(error.kind()),
    };

    assert_eq!(error_kind, Some(std::io::ErrorKind::NotFound), "{name}");
}

fn exact_ranked_request(route: &str) -> Request {
    let mut request = SealedRecording::request();
    request.route = route.into();
    request.second = Some(1001);
    request.identity = Some(json!({"run":"00000000000000010000000000000002",
        "boot":"00000000000000030000000000000004","publication":2}));

    request
}

fn assert_exact_ranked_snapshot(snapshot: &Snapshot) -> &TopApps {
    assert!(snapshot.rankings.is_some());
    let Rankings::Ready(rankings) = snapshot.rankings.as_deref().unwrap() else {
        panic!("{:?}", snapshot.rankings);
    };
    crate::live_source::identity_validate(&rankings.selected, &snapshot.selection["selected"])
        .unwrap();
    assert_eq!(snapshot.selection["second"], 1001);
    assert_eq!(rankings.selected["publication"], 2);
    assert_eq!(rankings.app_count, 600);

    rankings
}

impl SealedRecording {
    fn with_ranked_apps() -> Self {
        Self::with_ranked_apps_after_create(|_| {})
    }

    fn with_ranked_apps_after_create(after_create: impl FnOnce(&str)) -> Self {
        let recording = Self::empty();
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
            process_capacity: 600,
            ..Self::shape()
        };
        let place = Place {
            lock_path: recording.root.join("rankings.lock"),
            name_prefix: "fdrank".into(),
        };
        let mut generation = RankedGeneration {
            writer: Writer::create_with_identity(
                &place,
                shape,
                DictionaryCapacity {
                    entries: 600,
                    arena_bytes: 32_768,
                },
                &identity.run,
                0,
            )
            .unwrap(),
        };
        after_create(generation.writer.name());
        let (mut processes, identities) = ranked_processes(&mut generation.writer);
        let mut window =
            persistence::Window::new(identity, persistence::Limits::from_shape(&shape).unwrap());
        let mut reader = RingReader::open(&place.lock_path).unwrap();
        for publication in 1..=3_u64 {
            Self::with_ranked_apps_append(
                &mut generation.writer,
                &mut reader,
                &mut window,
                &mut processes,
                &identities,
                publication,
            );
        }
        Self::commit(&store, identity, &window);
        drop(reader);
        drop(generation);
        drop(store);

        recording
    }

    fn with_ranked_apps_append(
        writer: &mut Writer,
        reader: &mut RingReader,
        window: &mut persistence::Window,
        processes: &mut [Process],
        identities: &[IdentityObservation],
        publication: u64,
    ) {
        for process in &mut *processes {
            let pid = u64::try_from(process.pid).unwrap();
            process.usage.user_ns = pid * publication;
            process.usage.energy_nj = pid * publication * 1000;
            process.usage.resident_bytes = pid * publication * 1024;
            process.usage.disk_read_bytes = pid * publication * 1_000_000;
            process.usage.disk_written_bytes = pid * publication * 500_000;
        }
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
        let counter = u32::try_from(publication).unwrap();
        let cpus = [collector::machine::CPUTicks {
            user: counter * 10,
            system: 0,
            nice: 0,
            idle: counter * 90,
        }];
        writer
            .publish_with_identity(
                &Readings {
                    run_tick: publication - 1,
                    cpus: &cpus,
                    memory: &Memory::default(),
                    interfaces: &[],
                    ioreport: None,
                    smc: None,
                    processes,
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
        Self::with_ranked_apps_stage(reader, window, &times, publication);
    }

    fn with_ranked_apps_stage(
        reader: &mut RingReader,
        window: &mut persistence::Window,
        times: &ObservationTimes,
        publication: u64,
    ) {
        let mut tick = persistence::tick_buffer(&reader.shape()).unwrap();
        let mut captured = Vec::with_capacity(600);
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
}

struct RankedGeneration {
    writer: Writer,
}

impl Drop for RankedGeneration {
    fn drop(&mut self) {
        collector::ring::unlink_generation(self.writer.name()).unwrap();
    }
}

fn ranked_processes(writer: &mut Writer) -> (Vec<Process>, Vec<IdentityObservation>) {
    let mut processes = Vec::with_capacity(600);
    let mut identities = Vec::with_capacity(600);
    for pid in 1..=600_i32 {
        let name = format!("App{pid:05}");
        let mut program = None;
        writer
            .intern_program(
                &ProgramBytes {
                    executable_name: name.as_bytes(),
                    app_path: None,
                },
                &mut program,
            )
            .unwrap();
        processes.push(process(pid));
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

    (processes, identities)
}

fn process(pid: i32) -> Process {
    Process {
        pid,
        usage: Usage {
            awake_ns_at_start: 0,
            user_ns: 0,
            system_ns: 0,
            energy_nj: 0,
            instructions: 0,
            super_instructions: 0,
            cycles: 0,
            idle_wakeups: 0,
            interrupt_wakeups: 0,
            pageins: 0,
            resident_bytes: 0,
            footprint_bytes: 0,
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
    }
}

#[test]
fn real_cpu_worker_restores_attribution_and_installs_its_exact_selected_plot() {
    // Pass 600 saved Apps through the worker, rate decoder and installed panel preparation.
    assert_real_rate_attribution(crate::live_app_attribution_history::Metric::Cpu, 0.000_06);
}

#[test]
fn real_power_worker_restores_attribution_and_installs_its_exact_selected_plot() {
    // Preserve fractional process watts from captured counters at the earlier same-second sibling.
    assert_real_rate_attribution(crate::live_app_attribution_history::Metric::Power, 0.000_6);
}

fn assert_real_rate_attribution(
    metric: crate::live_app_attribution_history::Metric,
    expected: f32,
) {
    let route = match metric {
        crate::live_app_attribution_history::Metric::Cpu => "cpu",
        crate::live_app_attribution_history::Metric::Power => "energy",
        crate::live_app_attribution_history::Metric::Disk => "storage",
    };
    let recording = SealedRecording::with_ranked_apps();
    let request = exact_ranked_request(route);

    let snapshot = recording.read(request.clone()).unwrap();

    assert_eq!(snapshot.selection["selected"]["publication"], 2);
    let presentation = crate::live_acceptance::Presentation::prepare(&request, snapshot).unwrap();
    let panel = presentation
        .panels
        .iter()
        .find(|panel| panel.id == metric.panel_id())
        .expect("Real App counters must produce the attribution plot");
    assert_eq!(panel.lines.len(), 9);
    assert_eq!(panel.lines[0].name, "Other");
    assert_eq!(panel.lines[1].name, "App00600");
    assert_eq!(panel.selected_index, Some(1));
    assert_eq!(panel.lines[1].values[1], Some(expected));
}

#[test]
fn real_memory_worker_restores_resident_history_and_installs_its_chart() {
    // Load saved App residents through the actual Memory worker and prepare its visible panel.
    let recording = SealedRecording::with_ranked_apps();
    let request = exact_ranked_request("memory");

    let snapshot = recording.read(request.clone()).unwrap();

    assert!(snapshot.resident_history.is_some());
    let presentation = crate::live_acceptance::Presentation::prepare(&request, snapshot).unwrap();
    let panel = presentation
        .panels
        .iter()
        .find(|panel| panel.id == crate::live_resident_history::PANEL_ID)
        .expect("Real Memory worker must install the resident-by-App plot");
    assert_eq!(panel.lines.len(), 9);
    assert_eq!(panel.lines[0].name, "Other");
    assert_eq!(panel.lines[1].name, "App00600");
    assert_eq!(panel.selected_index, Some(1));
}

#[test]
fn real_memory_preview_reuses_the_accepted_resident_context_without_reloading_it() {
    // Keep accepted resident ownership while the real worker reads only exact preview details.
    let recording = SealedRecording::with_ranked_apps();
    let request = exact_ranked_request("memory");
    let accepted = recording.read(request.clone()).unwrap();
    let target = crate::live_hover::Target::exact(
        1000,
        &json!({"run":"00000000000000010000000000000002",
            "boot":"00000000000000030000000000000004","publication":1}),
    )
    .unwrap();
    let preview = crate::live_hover::Preview::resident(&accepted, &request, target);
    assert!(!preview.request.load_resident_history);
    assert!(std::sync::Arc::ptr_eq(
        accepted.resident_history.as_ref().unwrap(),
        preview.reading.resident_history.as_ref().unwrap()
    ));

    let exact = recording.read(preview.request).unwrap();

    assert!(exact.resident_history.is_none());
    assert_eq!(exact.selection["selected"]["publication"], 1);
    assert!(exact.rankings.is_some());
    assert!(
        accepted
            .resident_history
            .as_ref()
            .unwrap()
            .panel()
            .is_some()
    );
}

#[test]
fn real_disk_worker_restores_attribution_and_keeps_exact_interval_bytes() {
    // Carry 600 sealed Apps through the actual Storage worker and public desktop decoder.
    use crate::live_app_attribution_history::Metric;
    let recording = SealedRecording::with_ranked_apps();
    let request = exact_ranked_request("storage");
    let snapshot = recording.read(request.clone()).unwrap();

    let presentation = crate::live_acceptance::Presentation::prepare(&request, snapshot).unwrap();

    assert_eq!(
        presentation.snapshot.selection["selected"]["publication"],
        2
    );
    let history = presentation
        .snapshot
        .app_attribution_history
        .as_ref()
        .unwrap();
    let panel = presentation
        .panels
        .iter()
        .find(|panel| panel.id == Metric::Disk.panel_id())
        .unwrap();
    assert_eq!(panel.lines.len(), 9);
    assert_eq!(panel.lines[0].name, "Other");
    assert_eq!(panel.lines[1].name, "App00600");
    assert_eq!(panel.selected_index, Some(1));
    assert_eq!(panel.lines[1].values[1], Some(900.0));
    assert_eq!(history.captured_amount(1, Some(1)), Some(900_000_000));
    assert_eq!(
        history.reading(1, Some(1)),
        "900.0 MB/s · 900000000 B known subtotal · 1 measured · 0 unavailable"
    );
}

#[test]
fn real_disk_preview_reuses_the_context_while_loading_exact_ranks() {
    // Hover keeps the accepted byte cohort while the worker reads only exact historical Apps.
    let recording = SealedRecording::with_ranked_apps();
    let request = exact_ranked_request("storage");
    let accepted = recording.read(request.clone()).unwrap();
    let target =
        crate::live_hover::Target::exact(1000, &json!({"run":[1,2],"boot":[3,4],"publication":1}))
            .unwrap();
    let preview = crate::live_hover::Preview::resident(&accepted, &request, target);
    assert!(!preview.request.load_app_attribution_history);
    assert!(std::sync::Arc::ptr_eq(
        accepted.app_attribution_history.as_ref().unwrap(),
        preview.reading.app_attribution_history.as_ref().unwrap()
    ));

    let exact = recording.read(preview.request).unwrap();

    assert!(exact.app_attribution_history.is_none());
    assert_eq!(exact.selection["selected"]["publication"], 1);
    assert!(exact.rankings.is_some());
}
