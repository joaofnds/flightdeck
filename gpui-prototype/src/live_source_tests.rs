use serde_json::json;

#[test]
fn the_default_app_survives_a_new_first_row_and_an_empty_page() {
    // App history must keep its original target as the loaded list changes.
    let mut request = Request {
        route: "apps".into(),
        second: None,
        identity: None,
        table: "ticks".into(),
        offset: 0,
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
    };
    let request = Request {
        route: "overview".to_owned(),
        second: None,
        identity: None,
        table: "ticks".to_owned(),
        offset: 0,
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
