use std::path::Path;

use serde_json::Value;

use super::{SnapshotReader, read_complete};

pub(super) fn configure(reader: &mut SnapshotReader, root: &Path, expected: &Value, kind: &str) {
    assert!(reader.reader.session.is_none());
    match kind {
        "follow" => return,
        "pin" => {}
        "hover" => {
            let (snapshot, _) = read_complete(reader, root);
            super::payloads::validate_context(
                &snapshot.selection["trends"],
                &expected["kept_observations"],
            )
            .unwrap();
            reader.command.generation += 1;
            reader.command.context = crate::live_source::Context::Keep;
            reader.command.request.load_resident_history = false;
            reader.command.request.load_app_attribution_history = false;
            reader.command.request.load_process_history = false;
        }
        _ => panic!("selection must be follow, pin or hover"),
    }

    reader.command.request.second =
        Some(expected["selected"]["wall_ns"].as_u64().unwrap() / 1_000_000_000);
    reader.command.request.identity = Some(expected["selected"].clone());
}

#[test]
fn keeps_the_followed_context_while_reading_an_exact_older_hover_identity() {
    // Goal: a baseline correctness oracle can freeze hover availability without candidate output.
    // Method: warm the existing real reader, inspect its first identity and retain its context.
    let mut fixture = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    fixture.publish_resources();
    fixture.publish_resources();
    let mut reader = SnapshotReader::new();
    reader.command.mode = crate::live_source_mode::SourceMode::Live;
    reader.command.request.route = "network".into();
    let (latest, _) = read_complete(&mut reader, &fixture.root);
    let expected = serde_json::json!({"selected": latest.selection["trends"][0],
        "context_mode": "kept", "observations": null,
        "kept_observations": latest.selection["trends"]});
    drop(reader);
    let mut reader = SnapshotReader::new();
    reader.command.mode = crate::live_source_mode::SourceMode::Live;
    reader.command.request.route = "network".into();
    configure(&mut reader, &fixture.root, &expected, "hover");

    let (hover, _) = read_complete(&mut reader, &fixture.root);

    super::payloads::validate_input(&hover, &expected).unwrap();
    assert_eq!(hover.selection["selected"]["publication"], 1);
    assert!(hover.app_attribution_history.is_none());
}
