use serde_json::json;

#[path = "live_cpu_history_source_tests.rs"]
mod cpu_history;

#[path = "live_source_hover_tests.rs"]
mod hover;
#[path = "live_process_history_source_tests.rs"]
mod process_history;
#[path = "live_rankings_source_tests.rs"]
mod rankings;

#[test]
fn an_empty_recording_reports_absence_without_requiring_a_live_ring() {
    // Goal: a valid recording with no saved ticks opens without a fabricated observation.
    // Method: create the real empty store and read it through the normal worker.
    let recording = SealedRecording::empty();

    let snapshot = recording.read(SealedRecording::request()).unwrap();

    assert!(snapshot.selection["second"].is_null());
    assert!(snapshot.selection["selected"].is_null());
    assert_eq!(snapshot.selection["trends"], json!([]));
    assert!(snapshot.pages.is_empty());
}

#[test]
fn reader_diagnostics_returns_catalog_without_reading_a_selected_gap() {
    // Goal: diagnostics remains available without projecting a historical selection or trends.
    // Method: use a real sealed store while the committed inspection points into a gap.
    let recording = SealedRecording::new();
    let mut request = SealedRecording::request();
    request.route = "self".into();
    request.second = Some(1001);

    let snapshot = recording.read(request).unwrap();

    assert_eq!(snapshot.catalog["last_second"], 1002);
    assert!(snapshot.selection.is_null());
    assert!(snapshot.pages.is_empty());
    assert!(snapshot.cpu_detail.is_none());
    assert!(snapshot.cpu_history.is_none());
}

#[test]
fn a_delivered_saved_source_does_not_poll_its_recording_while_idle() {
    // Goal: opening saved readings performs no idle data-source work after delivery.
    // Method: make the real store invalid after a successful request and step again.
    let recording = SealedRecording::new();
    let mut reader = SnapshotReader::new();
    assert!(reader.read(&recording).unwrap().is_some());
    std::fs::write(recording.root.join("identity"), b"invalid").unwrap();

    let idle = reader.read(&recording);

    assert!(idle.unwrap().is_none());
}

#[test]
fn a_new_saved_request_revalidates_the_source_after_idle() {
    // Goal: an explicit refresh can report a store that became unreadable after opening.
    // Method: invalidate the real identity and submit the next requested generation.
    let recording = SealedRecording::new();
    let mut reader = SnapshotReader::new();
    assert!(reader.read(&recording).unwrap().is_some());
    std::fs::write(recording.root.join("identity"), b"invalid").unwrap();
    reader.command.generation += 1;

    let refreshed = reader.read(&recording);

    assert!(refreshed.is_err());
}

struct SnapshotReader {
    reader: super::Reader,
    command: super::Command,
    shared: Shared,
}

impl SnapshotReader {
    fn new() -> Self {
        Self {
            reader: super::Reader {
                session: None,
                last_catalog: None,
                last_generation: 0,
                retry_after: std::time::Instant::now(),
            },
            command: super::Command {
                generation: 1,
                epoch: 0,
                mode: crate::live_source_mode::SourceMode::Recorded,
                request: SealedRecording::request(),
            },
            shared: Shared {
                mailbox: Mutex::new(Mailbox {
                    generation: 1,
                    request: None,
                    output: None,
                }),
                stopped: AtomicBool::new(false),
                cancellation: Arc::new(Cancellation::default()),
            },
        }
    }

    fn read(
        &mut self,
        recording: &SealedRecording,
    ) -> Result<Option<Box<super::Snapshot>>, String> {
        super::reader_snapshot(
            &mut self.reader,
            &recording.root,
            &mut self.command,
            &self.shared,
        )
    }
}

#[test]
fn corrupt_saved_history_is_reported_instead_of_becoming_an_empty_recording() {
    // Goal: an unreadable commitment cannot masquerade as valid no-data history.
    // Method: corrupt only this fixture's saved seal and read through the normal worker.
    let recording = SealedRecording::new();
    let seal = recording
        .root
        .join("commits/00000000000000010000000000000002")
        .join("00000000000000000000/00000000000000000000.seal");
    assert!(!std::fs::read(&seal).unwrap().is_empty());
    std::fs::write(seal, b"corrupt").unwrap();

    let result = recording.read(SealedRecording::request());

    assert!(result.is_err());
}

#[test]
fn a_sealed_recording_opens_at_its_latest_saved_observation_without_a_live_ring() {
    // Goal: latest selects the final sibling of the last second without a live collector.
    // Method: request Overview through its worker using a real sealed-only recording.
    let recording = SealedRecording::new();

    let result = recording.read(SealedRecording::request());

    assert!(result.is_ok(), "{}", result.err().unwrap());
    let snapshot = result.unwrap();
    assert_eq!(snapshot.selection["second"], 1002);
    assert_eq!(snapshot.selection["selected"]["publication"], 3);
    assert_eq!(snapshot.selection["summary"]["memory_total_bytes"], 12_288);
    assert_eq!(
        snapshot.pages["fans"]["observation"],
        snapshot.selection["selected"]
    );
}

#[test]
fn a_sealed_recording_keeps_an_explicit_same_second_observation() {
    // Goal: exact inspection can choose an earlier sibling after latest chooses the last.
    // Method: select the earlier literal identity through the real recording worker.
    let recording = SealedRecording::new();
    let mut request = SealedRecording::request();
    request.second = Some(1002);
    request.identity = Some(json!({"run":"00000000000000010000000000000002",
        "boot":"00000000000000030000000000000004", "publication":2}));

    let snapshot = recording.read(request).unwrap();

    assert_eq!(snapshot.selection["selected"]["publication"], 2);
    assert_eq!(snapshot.selection["summary"]["memory_total_bytes"], 8192);
    assert_eq!(
        snapshot.selection["alternatives"].as_array().unwrap().len(),
        2
    );
}

#[test]
fn a_sealed_recording_leaves_a_missing_second_empty() {
    // Goal: a saved gap never borrows either neighboring measured observation.
    // Method: request the unrecorded second between two real encoded ticks.
    let recording = SealedRecording::new();
    let mut request = SealedRecording::request();
    request.second = Some(1001);

    let snapshot = recording.read(request).unwrap();

    assert_eq!(snapshot.selection["second"], 1001);
    assert!(snapshot.selection["selected"].is_null());
    assert!(snapshot.selection["summary"].is_null());
    assert!(snapshot.pages.is_empty());
}

struct SealedRecording {
    root: std::path::PathBuf,
}

impl SealedRecording {
    fn empty() -> Self {
        let token =
            collector::recording::identity_text(collector::recording::random_identity().unwrap());
        let root = std::env::temp_dir().join(format!("flightdeck-sealed-{token}"));
        let recording = Self { root };
        let store = persistence::Store::open(&recording.root).unwrap();
        drop(store);

        recording
    }

    fn new() -> Self {
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
        let mut window = Self::window(identity);
        Self::append(&mut window, 1, 1000);
        Self::append(&mut window, 2, 1002);
        Self::append(&mut window, 3, 1002);
        Self::commit(&store, identity, &window);
        drop(store);

        recording
    }

    fn window(identity: persistence::WindowIdentity) -> persistence::Window {
        persistence::Window::new(
            identity,
            persistence::Limits::from_shape(&Self::shape()).unwrap(),
        )
    }

    fn shape() -> collector::ring::Shape {
        collector::ring::Shape {
            slot_count: 4,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 1,
        }
    }

    fn append(window: &mut persistence::Window, publication: u64, second: u64) {
        let mut tick = persistence::tick_buffer(&Self::shape()).unwrap();
        tick.run_tick = publication - 1;
        tick.memory.total_bytes = 4096 * publication;
        let observation = persistence::Observation {
            publication,
            wall_ns: Some(second * 1_000_000_000),
            continuous_ns: Some(publication * 1_000_000_000),
            awake_ns: Some(publication * 1_000_000_000),
        };
        window.push_tick(&observation, &tick, &[]).unwrap();
    }

    fn commit(
        store: &persistence::Store,
        identity: persistence::WindowIdentity,
        window: &persistence::Window,
    ) {
        let persistence::Begin::Pending(pending) = store.begin(&identity).unwrap() else {
            panic!("Fixture window already committed");
        };
        let tables = persistence::Encoder.encode(window, pending.path()).unwrap();
        let publications = window.publication_range();
        let events = window.event_range();
        store
            .commit(
                pending,
                persistence::Manifest {
                    identity,
                    publication_first: publications.start,
                    publication_end: publications.end,
                    events_first: events.start,
                    events_end: events.end,
                    wall_ns: None,
                    continuous_ns: None,
                    awake_ns: None,
                    losses: persistence::Losses::default(),
                    data_directory: String::new(),
                    tables,
                },
            )
            .unwrap();
    }

    fn request() -> Request {
        Request {
            route: "overview".into(),
            second: None,
            identity: None,
            table: "ticks".into(),
            offset: 0,
            cpu_offset: 0,
            load_resident_history: true,
            load_app_attribution_history: true,
            load_process_history: true,
            executable: None,
            app: None,
            apps_offset: 0,
            members_offset: 0,
        }
    }

    fn read(&self, request: Request) -> Result<Box<super::Snapshot>, String> {
        let mut worker = Worker::start(self.root.clone()).unwrap();
        let generation = worker.request(request)?;
        for _attempt in 0..1000 {
            match worker.take()? {
                Some(Update::Ready(snapshot)) if snapshot.generation == generation => {
                    return Ok(snapshot);
                }
                Some(Update::Failed { message, .. }) => return Err(message),
                _ => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }

        Err("Fixture reader exceeded five seconds".into())
    }
}

impl Drop for SealedRecording {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn the_default_app_survives_a_new_first_row_and_an_empty_page() {
    // App history must keep its original target as the loaded list changes.
    let mut request = Request {
        route: "apps".into(),
        second: None,
        identity: None,
        table: "ticks".into(),
        offset: 0,
        cpu_offset: 0,
        load_resident_history: true,
        load_app_attribution_history: true,
        load_process_history: true,
        executable: None,
        app: None,
        apps_offset: 0,
        members_offset: 0,
    };
    let first = json!({"apps":[{"key":{"kind":"bundle", "bytes_hex":"41"}}]});
    let changed = json!({"apps":[{"key":{"kind":"bundle", "bytes_hex":"42"}}]});

    super::reader_apps_remember(&mut request, &first).unwrap();
    super::reader_apps_remember(&mut request, &changed).unwrap();
    super::reader_apps_remember(&mut request, &json!({"apps":[]})).unwrap();

    assert_eq!(request.app.unwrap().bytes_hex, "41");
}

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use super::{
    Cancellation, Mailbox, Request, Shared, Update, Worker, app_key_bytes, identity_signature,
    identity_validate, selection_alternative, worker_publish,
};

#[test]
fn resolves_the_exact_observation_among_same_second_siblings() {
    // Goal: a scrub keeps its run, boot and publication rather than taking the first sibling.
    // Method: resolve the third of three literal observations sharing one second.
    let selection = json!({"alternatives": [
        {"run": "00000000000000010000000000000002",
            "boot": "00000000000000030000000000000004", "publication": 7},
        {"run": "00000000000000050000000000000006",
            "boot": "00000000000000030000000000000004", "publication": 8},
        {"run": "00000000000000050000000000000006",
            "boot": "0000000000000009000000000000000a", "publication": 8}
    ]});
    let requested = selection["alternatives"][2].clone();

    let alternative = selection_alternative(&selection, &requested).unwrap();

    assert_eq!(alternative, 2);
}

#[test]
fn refuses_a_missing_observation_instead_of_selecting_a_sibling() {
    // Goal: an expired publication cannot silently move to another observation.
    // Method: request publication eight from a response retaining only seven.
    let selection = json!({"alternatives": [{
        "run": "00000000000000010000000000000002",
        "boot": "00000000000000030000000000000004", "publication": 7
    }]});
    let requested = json!({
        "run": "00000000000000010000000000000002",
        "boot": "00000000000000030000000000000004", "publication": 8
    });

    let alternative = selection_alternative(&selection, &requested);

    assert!(alternative.is_err());
}

#[test]
fn matches_live_app_stamp_words_with_the_selected_hex_identity() {
    // Goal: App packets refer to the same observation despite their lossless string encoding.
    // Method: compare decimal stamp words and publication with the live selection format.
    let selected = json!({
        "run": "00000000000000010000000000000002",
        "boot": "00000000000000030000000000000004", "publication": 9
    });
    let observed = json!({"stamp": {
        "run": ["1", "2"], "boot": ["3", "4"], "publication": "9"
    }});

    let validation = identity_validate(&observed, &selected);

    assert!(validation.is_ok());
}

#[test]
fn rejects_app_payloads_from_another_publication() {
    // Goal: mixed-generation App details never reach the UI as a coherent selection.
    // Method: compare two actual wire shapes with different publication numbers.
    let selected = json!({
        "run": "00000000000000010000000000000002",
        "boot": "00000000000000030000000000000004", "publication": 9
    });
    let observed = json!({"stamp": {
        "run": ["1", "2"], "boot": ["3", "4"], "publication": "8"
    }});

    let validation = identity_validate(&observed, &selected);

    assert!(validation.is_err());
}

#[test]
fn refuses_missing_observation_identities() {
    // Goal: absent fields cannot compare equal and admit an unvalidated packet.
    // Method: compare two empty JSON objects.
    assert!(identity_validate(&json!({}), &json!({})).is_err());
}

#[test]
fn decodes_non_utf8_app_key_bytes_without_changing_them() {
    // Goal: a selected App preserves its captured byte key rather than its display text.
    // Method: decode a path ending in two bytes that are not UTF-8.
    let bytes = app_key_bytes("2f4170702fffFe").unwrap();

    assert_eq!(bytes, b"/App/\xff\xfe");
}

#[test]
fn rejects_an_incomplete_app_key_byte() {
    // Goal: truncated hexadecimal keys cannot address another App.
    // Method: decode an odd-length wire key.
    assert!(app_key_bytes("2f1").is_err());
}

#[test]
fn rejects_non_hexadecimal_app_key_bytes() {
    // Goal: malformed bytes cannot be silently replaced with a zero byte.
    // Method: decode a wire key containing an invalid nibble.
    assert!(app_key_bytes("2fg1").is_err());
}

#[test]
fn rejects_app_keys_beyond_the_path_bound() {
    // Goal: a request cannot allocate an unbounded decoded key.
    // Method: exceed the recording format's 4095-byte path limit by one byte.
    assert!(app_key_bytes(&"aa".repeat(4096)).is_err());
}

#[test]
fn keeps_the_same_observation_when_loss_metadata_changes() {
    // Goal: a metadata refresh cannot count as an advancing live publication.
    // Method: compare one identity before and after its reader loss count changes.
    let first = json!({
        "run": "00000000000000010000000000000002",
        "boot": "00000000000000030000000000000004", "publication": 9,
        "reader_losses": {"publications": 0, "events": 0, "unknown": 0}
    });
    let mut updated = first.clone();
    updated["reader_losses"]["publications"] = json!(1);

    let first_identity = identity_signature(&first).unwrap();
    let updated_identity = identity_signature(&updated).unwrap();

    assert_eq!(first_identity, updated_identity);
}

#[test]
fn refuses_superseded_results_after_accepting_a_new_request() {
    // Goal: an old worker result cannot replace the latest requested generation.
    // Method: publish both generations through the real mailbox without a background thread.
    let mut worker = Worker {
        shared: Arc::new(Shared {
            mailbox: Mutex::new(Mailbox {
                generation: 0,
                request: None,
                output: None,
            }),
            stopped: AtomicBool::new(false),
            cancellation: Arc::new(Cancellation::default()),
        }),
        thread: None,
        mode: crate::live_source_mode::SourceMode::Live,
    };
    let request = Request {
        route: "overview".to_owned(),
        second: None,
        identity: None,
        table: "ticks".to_owned(),
        offset: 0,
        cpu_offset: 0,
        load_resident_history: true,
        load_app_attribution_history: true,
        load_process_history: true,
        executable: None,
        app: None,
        apps_offset: 0,
        members_offset: 0,
    };
    let old = worker.request(request.clone()).unwrap();
    let latest = worker.request(request).unwrap();

    worker_publish(
        &worker.shared,
        old,
        Update::Failed {
            generation: old,
            message: "superseded".to_owned(),
        },
    );
    let superseded = worker.take().unwrap();
    worker_publish(
        &worker.shared,
        latest,
        Update::Failed {
            generation: latest,
            message: "latest".to_owned(),
        },
    );
    let accepted = worker.take().unwrap();

    assert!(superseded.is_none());
    assert!(matches!(accepted, Some(Update::Failed { generation, .. }) if generation == latest));
}
