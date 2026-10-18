use std::ffi::OsString;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use serde_json::{Value, json};

use super::super::command::Command;
use super::super::{Transcript, decode, read, transcript, write_new};

#[test]
fn parses_an_explicit_app_capture_without_reading_its_recording() {
    // Goal: capture chooses one explicit observation and immutable App key.
    // Method: parse all twelve long-flag arguments without opening any source.
    let arguments = arguments(&[
        "--capture-app",
        "recording",
        "--second",
        "1000",
        "--alternative",
        "2",
        "--kind",
        "bundle",
        "--bytes-hex",
        "41ff",
        "--output",
        "capture.json",
    ]);

    let Command::Capture(capture) = Command::parse(&arguments).unwrap() else {
        panic!("Capture arguments selected the wrong command");
    };

    assert_eq!(capture.second, 1000);
    assert_eq!(capture.alternative, 2);
    assert_eq!(capture.key, json!({"kind":"bundle","bytes_hex":"41ff"}));
    assert_eq!(capture.kind, viewer::live_app_response::KeyKind::Bundle);
    assert_eq!(capture.bytes, [0x41, 0xff]);
    assert_eq!(capture.recording, std::path::Path::new("recording"));
    assert_eq!(capture.output, std::path::Path::new("capture.json"));
}

#[test]
fn parses_the_bounded_app_measurement_route() {
    // Goal: measurement accepts only the narrow App-input command.
    // Method: parse its six long-flag arguments without decoding or projecting input.
    let arguments = arguments(&[
        "--app-input",
        "capture.json",
        "--iterations",
        "100",
        "--output",
        "panels.json",
    ]);

    let Command::Measure(measure) = Command::parse(&arguments).unwrap() else {
        panic!("Measurement arguments selected the wrong command");
    };

    assert_eq!(measure.iterations, 100);
    assert_eq!(measure.input, std::path::Path::new("capture.json"));
    assert_eq!(measure.output, std::path::Path::new("panels.json"));
}

#[test]
fn refuses_the_capture_argument_sentinel() {
    // Goal: extra arguments cannot be silently ignored by the parent dispatch.
    // Method: append a thirteenth argument to a complete twelve-argument capture command.
    let arguments = arguments(&[
        "--capture-app",
        "recording",
        "--second",
        "1000",
        "--alternative",
        "0",
        "--kind",
        "bundle",
        "--bytes-hex",
        "41",
        "--output",
        "capture.json",
        "extra",
    ]);

    assert!(Command::parse(&arguments).is_err());
}

#[test]
fn refuses_scalar_route_flags_in_the_app_command() {
    // Goal: the narrow command cannot mislabel a scalar-route workload as App history.
    // Method: substitute the scalar route flag for the required iteration flag.
    let arguments = arguments(&[
        "--app-input",
        "capture.json",
        "--route",
        "overview",
        "--output",
        "panels.json",
    ]);

    assert!(Command::parse(&arguments).is_err());
}

#[test]
fn refuses_unknown_command_modes() {
    // Goal: parent dispatch mistakes fail before any input or recording is opened.
    // Method: parse an unsupported first flag with an otherwise valid measurement shape.
    let arguments = arguments(&[
        "--input",
        "capture.json",
        "--iterations",
        "1",
        "--output",
        "panels.json",
    ]);

    assert!(Command::parse(&arguments).is_err());
}

#[test]
fn retains_selected_identity_and_all_panels_in_the_output() {
    // Goal: bitwise comparisons remain attributable to the actual selected App and observation.
    // Method: build a transcript from the decoded synthetic contract fixture and all five panels.
    let value = super::fixture();
    let envelope = decode(&serde_json::to_vec(&value).unwrap()).unwrap();
    let panels =
        crate::live_projection::panels("apps", &value["selection"], Some(&value["history"]))
            .unwrap();
    let bits = crate::projection_bench::bits(&panels);

    let output = transcript(&Transcript {
        envelope: &envelope,
        panels: &bits,
    });

    assert_eq!(output["selected"], value["history"]["selected"]);
    assert_eq!(output["selection_selected"], value["selection"]["selected"]);
    assert_eq!(output["app"], value["app"]);
    assert_eq!(output["panels"], bits);
}

#[test]
fn refuses_to_overwrite_a_benchmark_artifact() {
    // Goal: later captures or measurements cannot destroy earlier evidence.
    // Method: write twice to the same new-file path and inspect the original bytes.
    let directory = Directory::new();
    let path = directory.0.join("evidence.json");
    write_new(&path, &json!({"original":true})).unwrap();
    let before = fs::read(&path).unwrap();

    let error = write_new(&path, &Value::Null).unwrap_err();

    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
    assert_eq!(fs::read(&path).unwrap(), before);
}

#[test]
fn writes_actual_identity_artifacts_with_user_private_permissions() {
    // Goal: captured process and App identities are readable only by their owner.
    // Method: inspect the mode of the newly created artifact rather than an existing path.
    let directory = Directory::new();
    let path = directory.0.join("capture.json");

    write_new(&path, &super::fixture()).unwrap();

    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn refuses_oversized_output_before_creating_a_file() {
    // Goal: the complete evidence artifact stays within its independent 8 MiB bound.
    // Method: serialize an oversized value and inspect whether its output path was created.
    let directory = Directory::new();
    let path = directory.0.join("oversized.json");

    let result = write_new(&path, &json!("x".repeat(8 * 1024 * 1024)));

    assert!(result.is_err());
    assert!(!path.exists());
}

#[test]
fn round_trips_the_complete_private_capture_envelope() {
    // Goal: capture serialization preserves real-shaped identity, source gaps and coverage.
    // Method: write and reread a bounded envelope through the same file helpers as the CLI.
    let directory = Directory::new();
    let path = directory.0.join("capture.json");
    let value = super::fixture();
    write_new(&path, &value).unwrap();

    let decoded = read(&path).unwrap();

    assert_eq!(decoded.value(), &value);
}

#[test]
fn refuses_to_read_a_file_one_byte_above_the_input_bound() {
    // Goal: the file reader retains a sentinel byte so oversized valid JSON is rejected.
    // Method: append whitespace beyond 8 MiB and read through the actual bounded file helper.
    let directory = Directory::new();
    let path = directory.0.join("oversized.json");
    let mut bytes = serde_json::to_vec(&super::fixture()).unwrap();
    bytes.resize(8 * 1024 * 1024 + 1, b' ');
    fs::write(&path, bytes).unwrap();

    let result = read(&path);

    assert!(result.is_err());
}

fn arguments(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "flightdeck-app-projection-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}
