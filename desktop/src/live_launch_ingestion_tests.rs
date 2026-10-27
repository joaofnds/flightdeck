use std::path::PathBuf;
use std::time::{Duration, Instant};

use collector::recording::{identity_text, random_identity};
use persistence::work::ingestion::{Event, Key, Kind, Producer, Trace};
use serde_json::json;

use super::Backend;
use crate::live_source::{Request, Snapshot, Update};
use crate::live_source_mode::SourceMode;

#[path = "../../viewer/src/bin/compatibility_fixture/source.rs"]
mod source;

#[path = "live_launch_ingestion_output.rs"]
mod output;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceLayout {
    Legacy,
    Identity,
}

#[derive(Clone, Copy)]
enum Input {
    Sealed(SourceLayout),
    NonfinitePower,
    DuplicatePid,
}

struct Harness {
    backend: Option<Backend>,
    trace: Trace,
    scope: Option<persistence::work::ingestion::Scope>,
    root: PathBuf,
    input: Option<source::Source>,
}

impl Harness {
    fn baseline(output: &mut Option<Self>) {
        match std::env::var_os("FLIGHTDECK_ITEM2_RECORDING") {
            Some(root) => Self::open(std::path::Path::new(&root), output),
            None => Self::setup(Input::Sealed(SourceLayout::Identity), output),
        }
    }

    fn setup(input: Input, output: &mut Option<Self>) {
        assert!(output.is_none());
        let root = std::env::temp_dir().join(format!(
            "flightdeck-launch-ingestion-{}",
            identity_text(random_identity().unwrap())
        ));
        let store = persistence::Store::open(&root).unwrap();
        let mut captured = None;
        match input {
            Input::Sealed(layout) => {
                source::create(&root, store.recording(), layout, 0, &mut captured).unwrap();
            }
            Input::NonfinitePower | Input::DuplicatePid => {
                source::create_custom(
                    &root,
                    store.recording(),
                    SourceLayout::Legacy,
                    0,
                    &mut captured,
                    |readings| customize(input, readings),
                )
                .unwrap();
            }
        }
        drop(store);
        if matches!(input, Input::Sealed(_)) {
            persistence::worker::run(
                &root,
                2 * 1024 * 1024 * 1024,
                &persistence::sealed_apps::SealedApps::default(),
            )
            .unwrap();
        }
        let input = captured.unwrap();
        let trace = Trace::new(input.identity.recording);
        let scope = Some(trace.enter());
        let mut backend = None;
        Backend::start(&root, SourceMode::Live, &mut backend).unwrap();
        *output = Some(Self {
            backend,
            trace,
            scope,
            root,
            input: Some(input),
        });
    }

    fn open(root: &std::path::Path, output: &mut Option<Self>) {
        assert!(output.is_none());
        let recording = collector::recording::read_identity(root).unwrap();
        let registry = collector::recording::Registry::read(root).unwrap();
        assert_eq!(registry.runs.len(), 1);
        let run = &registry.runs[0];
        assert_eq!(run.identity.recording, recording);
        let reader = collector::ring::RingReader::open_generation(&run.ring_name).unwrap();
        assert!(reader.mapped_size_bytes() <= 65_536);
        assert_eq!(reader.identity(), Some(run.identity));
        assert!(reader.completed());
        assert!(reader.dictionary_capacity().is_some());
        assert_eq!(reader.publication_first(), 1);
        assert_eq!(reader.publication_newest().unwrap(), 2);
        drop(reader);

        let trace = Trace::new(recording);
        let scope = Some(trace.enter());
        let mut backend = None;
        Backend::start(root, SourceMode::Live, &mut backend).unwrap();
        *output = Some(Self {
            backend,
            trace,
            scope,
            root: root.to_owned(),
            input: None,
        });
    }

    fn read(&mut self, route: &str) -> Box<Snapshot> {
        self.read_result(route).unwrap()
    }

    fn read_result(&mut self, route: &str) -> Result<Box<Snapshot>, String> {
        self.read_request(request(route))
    }

    fn read_request(&mut self, request: Request) -> Result<Box<Snapshot>, String> {
        let generation = self.backend.as_mut().unwrap().worker.request(request)?;
        self.wait(generation)
    }

    fn inspect_request(&mut self, request: Request) -> Result<Box<Snapshot>, String> {
        let generation = self.backend.as_mut().unwrap().worker.inspect(request)?;
        self.wait(generation)
    }

    fn wait(&mut self, generation: u64) -> Result<Box<Snapshot>, String> {
        let worker = &mut self.backend.as_mut().unwrap().worker;
        let deadline = Instant::now() + Duration::from_secs(10);
        for _step in 0..2_000 {
            match worker.take()? {
                Some(Update::Ready(snapshot)) => {
                    assert_eq!(snapshot.generation, generation);
                    if worker.last_output_complete() {
                        assert!(!snapshot.histories_pending);
                        return Ok(snapshot);
                    }
                }
                Some(Update::Failed {
                    generation: failed,
                    message,
                }) => {
                    assert_eq!(failed, generation);
                    return Err(message);
                }
                None => {}
            }
            assert!(
                Instant::now() < deadline,
                "complete launch request timed out"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("complete launch request exceeded its step bound");
    }

    fn view(&self) -> std::sync::Arc<viewer::store::View> {
        let store = self.backend.as_ref().unwrap().store.as_ref().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        for _step in 0..2_000 {
            if let Some(view) = store.take() {
                if view
                    .seconds
                    .last()
                    .is_some_and(|second| second.publication == 2)
                {
                    if view.ring_complete {
                        return view;
                    }
                }
            }
            assert!(Instant::now() < deadline, "typed launch View timed out");
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("typed launch View exceeded its step bound");
    }

    fn events(&self) -> Vec<Event> {
        assert!(self.backend.is_none());
        let mut events = Vec::new();
        self.trace.snapshot(&mut events).unwrap();
        for event in &events {
            if matches!(event.kind, Kind::NativeCollect | Kind::ProjectionDecode) {
                assert!(!matches!(event.key, Key::Unattributed { .. }), "{event:?}");
            }
        }
        events
    }

    fn wait_for_failure(&self, publication: u64) -> usize {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut events = Vec::new();
        for _step in 0..2_000 {
            events.clear();
            self.trace.snapshot(&mut events).unwrap();
            let failures = count(&events, Producer::Worker, Kind::AttemptFailed, publication);
            if failures > 0 {
                return failures;
            }

            assert!(
                Instant::now() < deadline,
                "initial following failure timed out"
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        panic!("initial following failure exceeded its step bound");
    }

    fn teardown(&mut self) {
        let backend = self.backend.take().unwrap();
        let mut thread = backend.worker.retire();
        crate::live_probe::matched_tests::join_observed(
            &mut thread,
            Instant::now() + Duration::from_secs(10),
        )
        .unwrap();
        drop(backend.store);
        drop(self.scope.take());
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        if self.backend.is_some() {
            self.teardown();
        }
        if let Some(input) = &self.input {
            collector::ring::unlink_generation(&input.ring).unwrap();
            std::fs::remove_dir_all(&self.root).unwrap();
        }
    }
}

fn request(route: &str) -> Request {
    Request {
        route: route.into(),
        second: None,
        identity: None,
        table: "ticks".into(),
        offset: 0,
        app: None,
        apps_offset: 0,
        members_offset: 0,
        cpu_offset: 0,
        load_resident_history: true,
        load_app_attribution_history: true,
        load_process_history: true,
        earlier: crate::live_source::Earlier::Load,
        executable: None,
    }
}

fn count(events: &[Event], producer: Producer, kind: Kind, publication: u64) -> usize {
    events
        .iter()
        .filter(|event| event.producer == producer)
        .filter(|event| event.kind == kind)
        .filter(|event| match event.key {
            Key::Publication {
                publication: found, ..
            } => found == publication,
            Key::Unattributed { .. } | Key::Minute { .. } => false,
        })
        .count()
}

fn assert_worker_sealed_work(events: &[Event], run: &collector::ring::RunIdentity) {
    let collected: Vec<_> = events
        .iter()
        .filter(|event| event.producer == Producer::Worker && event.kind == Kind::NativeCollect)
        .collect();
    assert!(!collected.is_empty());
    for event in collected {
        assert_eq!(
            event.key,
            Key::Minute {
                run: *run,
                first: 1,
                end: 3
            }
        );
    }
}

#[test]
fn characterizes_duplicate_capture_through_the_live_launch_constructor() {
    // Goal: stage 1 records actual duplicate work before the one-owner implementation.
    // Method: use the real constructor, populated registered ring, seal and completed consumers.
    let mut harness = None;
    Harness::setup(Input::Sealed(SourceLayout::Identity), &mut harness);
    let mut harness = harness.unwrap();

    let snapshot = harness.read("overview");
    let view = harness.view();
    let mut payloads = std::collections::BTreeMap::new();
    harness
        .backend
        .as_ref()
        .unwrap()
        .worker
        .frozen_payloads(&mut payloads);
    harness.teardown();
    let events = harness.events();
    let input = harness.input.as_ref().unwrap();

    assert_worker_sealed_work(&events, &input.identity);
    assert_eq!(snapshot.selection["selected"]["publication"], 2);
    assert_eq!(view.seconds.last().unwrap().publication, 2);
    assert_eq!(
        snapshot.selection["summary"]["memory_total_bytes"],
        8_589_934_592_u64
    );
    assert_eq!(count(&events, Producer::Worker, Kind::FrameCapture, 2), 1);
    assert_eq!(count(&events, Producer::Store, Kind::FrameCapture, 2), 1);
    assert_eq!(count(&events, Producer::Worker, Kind::RawCapture, 2), 1);
    println!(
        "{}",
        json!({"selected": snapshot.selection["selected"], "payloads": payloads,
        "events": events.iter().map(output::event).collect::<Vec<_>>(),
        "trace_capacity_bytes": harness.trace.capacity_bytes(),
        "input": {"population": input.population,
            "layout": format!("{:?}", input.layout), "lock": input.lock,
            "second_first": input.second_first, "mapped_bytes": input.mapped_bytes}})
    );
    assert_eq!(
        count(&events, Producer::Worker, Kind::AppDerivation, 2)
            + count(&events, Producer::Store, Kind::AppDerivation, 2),
        2 // Tighten this baseline to one in the ownership switch; the first red is retained.
    );
}

fn customize(input: Input, readings: &mut collector::ring::Readings<'_>) {
    if readings.run_tick != 1 {
        return;
    }
    match input {
        Input::Sealed(_) => {}
        Input::NonfinitePower => {
            readings.smc = Some(collector::ring::SmcReadings {
                power_watts: f32::NAN,
                soc_celsius: 40.0,
                cpu_celsius: None,
                gpu_celsius: None,
                fan_rpm: &[],
            });
        }
        Input::DuplicatePid => {
            const PROCESSES: [collector::processes::Process; 2] = [source::PROCESS; 2];
            readings.processes = &PROCESSES;
        }
    }
}

#[test]
fn characterizes_raw_validation_refusal_beside_available_store_machine_readings() {
    // Goal: canonicalization must account for today's differing nonfinite-power policies.
    // Method: send identical malformed raw facts through the two launch-created readers.
    let mut harness = None;
    Harness::setup(Input::NonfinitePower, &mut harness);
    let mut harness = harness.unwrap();
    let initial_failures = harness.wait_for_failure(2);

    let result = harness.read_result("overview");
    let repeated = harness.read_result("overview");
    let view = harness.view();

    assert!(result.is_err());
    assert!(repeated.is_err());
    assert_eq!(view.seconds.last().unwrap().power_watts, None);
    assert_eq!(view.apps.last().unwrap().missing, None);
    harness.teardown();
    let events = harness.events();
    assert!(count(&events, Producer::Worker, Kind::AttemptFailed, 2) >= initial_failures + 2);
    assert!(count(&events, Producer::Worker, Kind::FrameCapture, 2) >= initial_failures + 2);
    assert_eq!(count(&events, Producer::Worker, Kind::RawCapture, 2), 0);
    output::record_refusal(
        "nonfinite-power",
        &harness,
        &view,
        &[
            result.as_ref().err().unwrap(),
            repeated.as_ref().err().unwrap(),
        ],
    );
}

#[test]
fn characterizes_app_refusal_beside_available_store_machine_readings() {
    // Goal: canonicalization must retain the current independently unavailable App result.
    // Method: supply duplicate numeric PIDs without changing the valid machine reading.
    let mut harness = None;
    Harness::setup(Input::DuplicatePid, &mut harness);
    let mut harness = harness.unwrap();

    let result = harness.read_result("overview");
    let view = harness.view();

    assert!(result.is_err());
    assert!(matches!(
        view.apps.last().unwrap().missing,
        Some(viewer::store::Missing::Refused(_))
    ));
    harness.teardown();
    let events = harness.events();
    assert_eq!(count(&events, Producer::Worker, Kind::FrameCapture, 2), 0);
    assert_eq!(count(&events, Producer::Store, Kind::FrameCapture, 2), 0);
    assert!(count(&events, Producer::Worker, Kind::FrameCaptureAttempt, 2) >= 1);
    assert_eq!(
        count(&events, Producer::Store, Kind::FrameCaptureAttempt, 2),
        1
    );
    output::record_refusal(
        "duplicate-pid",
        &harness,
        &view,
        &[result.as_ref().err().unwrap()],
    );
}

#[test]
fn keeps_inactive_source_accounting_free_of_rust_allocations() {
    // Goal: ordinary source work allocates no trace storage or per-operation recorder state.
    // Method: warm TLS, then count Rust allocations around all inactive recorder operations.
    use persistence::work::ingestion;
    assert!(!ingestion::is_active());
    let run = collector::ring::RunIdentity {
        recording: collector::ring::Identity([1, 2]),
        run: collector::ring::Identity([3, 4]),
        boot: collector::ring::Identity([5, 6]),
    };

    let ((), allocations) = crate::allocation_tests::measure(|| {
        let inherited = ingestion::inherit(Producer::Store).enter();
        let purpose = ingestion::purpose(ingestion::Purpose::Raw);
        ingestion::record_publication(run, u64::MAX, Kind::RingRead);
        ingestion::record_content(Kind::ContentCapture, u64::MAX);
        drop(purpose);
        drop(inherited);
    });

    assert_eq!(allocations.calls, 0);
    assert_eq!(allocations.bytes, 0);
    assert!(!ingestion::is_active());
}

macro_rules! route_oracle {
    ($name:ident, $route:literal) => {
        #[test]
        fn $name() {
            // Goal: retain a complete route oracle before changing its ingestion owner.
            // Method: complete the production backend and presentation over finite native input.
            output::record_route($route);
        }
    };
}

route_oracle!(freezes_overview_launch_output, "overview");
route_oracle!(freezes_cpu_launch_output, "cpu");
route_oracle!(freezes_apps_launch_output, "apps");
route_oracle!(freezes_memory_launch_output, "memory");
route_oracle!(freezes_gpu_launch_output, "gpu");
route_oracle!(freezes_storage_launch_output, "storage");
route_oracle!(freezes_network_launch_output, "network");
route_oracle!(freezes_energy_launch_output, "energy");
route_oracle!(freezes_battery_launch_output, "battery");
route_oracle!(freezes_computer_launch_output, "computer");
route_oracle!(freezes_self_launch_output, "self");

#[test]
fn keeps_legacy_numeric_sources_available_to_both_launch_readers() {
    // Goal: the constructor keeps pre-dictionary recordings available during consolidation.
    // Method: complete registered legacy input through both launch-created consumers.
    let mut harness = None;
    Harness::setup(Input::Sealed(SourceLayout::Legacy), &mut harness);
    let mut harness = harness.unwrap();

    let snapshot = harness.read("network");
    let view = harness.view();

    assert_eq!(snapshot.selection["selected"]["publication"], 2);
    assert_eq!(view.seconds.last().unwrap().publication, 2);
    harness.teardown();
}

#[test]
fn leaves_borrowed_frozen_recording_and_ring_owned_by_the_fixture() {
    // Goal: baseline and candidate may reopen identical native input without destroying it.
    // Method: finish the owning backend, open a borrowed backend, then verify retained input.
    let mut owner = None;
    Harness::setup(Input::Sealed(SourceLayout::Identity), &mut owner);
    let mut owner = owner.unwrap();
    owner.teardown();
    let registry = std::fs::read(owner.root.join("runs")).unwrap();
    let mut borrowed = None;
    Harness::open(&owner.root, &mut borrowed);
    let mut borrowed = borrowed.unwrap();

    let snapshot = borrowed.read("network");
    assert_eq!(snapshot.selection["selected"]["publication"], 2);
    borrowed.teardown();
    drop(borrowed);

    assert_eq!(std::fs::read(owner.root.join("runs")).unwrap(), registry);
    let ring = &owner.input.as_ref().unwrap().ring;
    let reader = collector::ring::RingReader::open_generation(ring).unwrap();
    assert_eq!(reader.publication_newest().unwrap(), 2);
    drop(reader);
}

#[test]
fn preserves_pinned_hover_and_resumed_outputs_beside_the_live_store() {
    // Goal: pin and kept-context hover keep their exact reading while Store stays current.
    // Method: complete two native readings through Backend, inspect the first, then resume.
    let mut harness = None;
    Harness::baseline(&mut harness);
    let mut harness = harness.unwrap();
    let latest = harness.read("overview");
    let identity =
        crate::live_projection::inspection_identity(&latest.selection, source::SECOND_FIRST)
            .unwrap();
    let mut pinned = request("overview");
    pinned.second = Some(source::SECOND_FIRST);
    pinned.identity = Some(identity);

    let selected = harness.read_request(pinned.clone()).unwrap();
    pinned.load_resident_history = false;
    pinned.load_app_attribution_history = false;
    pinned.load_process_history = false;
    pinned.earlier = crate::live_source::Earlier::Skip;
    let hovered = harness.inspect_request(pinned).unwrap();
    let resumed = harness.read("overview");
    let view = harness.view();
    harness.teardown();

    assert_eq!(selected.selection["selected"]["publication"], 1);
    crate::live_source::identity_validate(
        &hovered.selection["selected"],
        &selected.selection["selected"],
    )
    .unwrap();
    assert_eq!(
        hovered.selection["selected"]["run_tick"],
        selected.selection["selected"]["run_tick"]
    );
    assert_eq!(
        hovered.selection["selected"]["wall_ns"],
        selected.selection["selected"]["wall_ns"]
    );
    assert!(hovered.selection.get("trends").is_none());
    assert_eq!(resumed.selection["selected"], latest.selection["selected"]);
    assert_eq!(view.seconds.last().unwrap().publication, 2);
    output::record_selections(&harness, [&latest, &selected, &hovered, &resumed]);
}
