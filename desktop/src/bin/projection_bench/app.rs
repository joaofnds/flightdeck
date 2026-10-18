use std::ffi::{OsStr, OsString};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use serde_json::{Value, json};

#[path = "app/command.rs"]
mod command;
#[path = "app/measure.rs"]
mod measure;

pub fn run(arguments: &[OsString]) -> Result<(), Box<dyn std::error::Error>> {
    match command::Command::parse(arguments)? {
        command::Command::Capture(options) => capture(&options)?,
        command::Command::Measure(options) => {
            measure::run(&read(options.input)?, options.iterations, options.output)?;
        }
    }

    Ok(())
}

const INPUT_BYTES_MAX: usize = 8 * 1024 * 1024;
const APP_KEY_BYTES_MAX: usize = 4095;

struct Envelope {
    value: Value,
}

struct Transcript<'values> {
    envelope: &'values Envelope,
    panels: &'values Value,
}

struct IdentityMatch<'values> {
    observed: &'values Value,
    selected: &'values Value,
}

impl Envelope {
    fn value(&self) -> &Value {
        &self.value
    }
}

fn capture(options: &command::Capture<'_>) -> Result<(), Box<dyn std::error::Error>> {
    let mut session = viewer::LiveSession::open(options.recording)?;
    let selection = session.select(options.second, options.alternative)?;
    let history = session.app_history(options.kind, &options.bytes)?;
    let value = json!({"schema_version":1,"route":"apps","app":options.key,
        "selection":selection,"history":history});

    // Both public responses remain intact; validation rejects mismatched or oversized capture.
    let envelope = decode(&serde_json::to_vec(&value)?)?;
    let expected = measure::preflight(&envelope)?;
    write_new(options.output, envelope.value())?;
    let value = envelope.value();
    println!(
        "{}",
        json!({"mode":"capture-app","route":"apps","app":value["app"],
        "selected":value["history"]["selected"],
        "history_points":value["history"]["points"].as_array().map(Vec::len),
        "panels":expected.as_array().map(Vec::len), "coverage":value["history"]["coverage"],
        "scope":"Read-only LiveSession select and selected-App history; no source fill"})
    );

    Ok(())
}

fn capture_key(text: &str) -> io::Result<Vec<u8>> {
    assert!(text.len() <= APP_KEY_BYTES_MAX * 2);
    assert!(text.len().is_multiple_of(2));

    let mut bytes = Vec::with_capacity(text.len() / 2);
    for pair in text.as_bytes().as_chunks::<2>().0 {
        bytes.push(
            u8::from_str_radix(std::str::from_utf8(pair).map_err(io::Error::other)?, 16)
                .map_err(io::Error::other)?,
        );
    }

    Ok(bytes)
}

fn read(path: &Path) -> io::Result<Envelope> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take(u64::try_from(INPUT_BYTES_MAX + 1).map_err(io::Error::other)?)
        .read_to_end(&mut bytes)?;

    decode(&bytes)
}

fn write_new(path: &Path, value: &Value) -> io::Result<()> {
    let mut bytes = serde_json::to_vec(value).map_err(io::Error::other)?;
    bytes.push(b'\n');
    if bytes.len() > INPUT_BYTES_MAX {
        return Err(io::Error::other("App projection output exceeds 8 MiB"));
    }

    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;

    file.write_all(&bytes)
}

fn transcript(input: &Transcript<'_>) -> Value {
    let value = input.envelope.value();
    json!({"schema_version":1,"route":"apps","app":value["app"],
        "selected":value["history"]["selected"],
        "selection_selected":value["selection"]["selected"],"panels":input.panels})
}

fn decode(bytes: &[u8]) -> io::Result<Envelope> {
    if bytes.len() > INPUT_BYTES_MAX {
        return Err(io::Error::other("App projection input exceeds 8 MiB"));
    }

    let value = serde_json::from_slice(bytes).map_err(io::Error::other)?;
    validate(&value)?;

    Ok(Envelope { value })
}

fn iterations(text: &OsStr) -> io::Result<u32> {
    let count = text
        .to_str()
        .ok_or_else(|| io::Error::other("Invalid iteration text"))?
        .parse()
        .map_err(io::Error::other)?;
    if !(1..=100).contains(&count) {
        return Err(io::Error::other("Iterations must be 1..100"));
    }

    Ok(count)
}

fn validate(value: &Value) -> io::Result<()> {
    if value["schema_version"] != 1 {
        return Err(io::Error::other(
            "Unsupported App projection envelope version",
        ));
    }

    if value["route"] != "apps" {
        return Err(io::Error::other("App projection requires the apps route"));
    }

    validate_key(&value["app"])?;
    validate_selected(&value["selection"]["selected"])?;
    integer(&value["selection"]["second"])?;
    let trends = value["selection"]["trends"]
        .as_array()
        .ok_or_else(|| io::Error::other("App selection requires a trends array"))?;
    if trends.len() > crate::data::LIVE_SAMPLE_LIMIT {
        return Err(io::Error::other("App selection exceeds 4096 trend points"));
    }

    for point in trends {
        validate_selected(point)?;
        validate_clock(point, "continuous_ns")?;
        integer(&point["segment"])?;
    }

    validate_history(value)
}

fn validate_key(value: &Value) -> io::Result<viewer::live_app_response::KeyKind> {
    let kind = match value["kind"].as_str() {
        Some("bundle") => viewer::live_app_response::KeyKind::Bundle,
        Some("executable") => viewer::live_app_response::KeyKind::Executable,
        _ => return Err(io::Error::other("Unsupported App key kind")),
    };

    let text = value["bytes_hex"]
        .as_str()
        .ok_or_else(|| io::Error::other("Missing App key bytes"))?;
    if text.len() > APP_KEY_BYTES_MAX * 2 {
        return Err(io::Error::other("App key exceeds 4095 bytes"));
    }

    if !text.len().is_multiple_of(2) {
        return Err(io::Error::other("Incomplete App key hexadecimal byte"));
    }

    if !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(io::Error::other("Invalid App key hexadecimal byte"));
    }

    Ok(kind)
}

fn validate_history(value: &Value) -> io::Result<()> {
    let history = &value["history"];
    validate_observation(&history["selected"])?;
    let points = history["points"]
        .as_array()
        .ok_or_else(|| io::Error::other("App history requires a points array"))?;
    if !(1..=crate::data::LIVE_SAMPLE_LIMIT).contains(&points.len()) {
        return Err(io::Error::other("App history requires 1..4096 points"));
    }

    // Parse every full observation before matching can skip an unrelated publication.
    for point in points {
        validate_observation(&point["observation"])?;
        integer(&point["segment"])?;
    }

    if !identity_matches(&IdentityMatch {
        observed: &value["selection"]["selected"],
        selected: &history["selected"],
    })? {
        return Err(io::Error::other(
            "App history does not match the selected observation",
        ));
    }

    let mut selected_count = 0_u16;
    for point in points {
        if identity_matches(&IdentityMatch {
            observed: &point["observation"],
            selected: &history["selected"],
        })? {
            if point["observation"] != history["selected"] {
                return Err(io::Error::other("App history selected metadata differs"));
            }

            selected_count += 1;
        }
    }

    if selected_count != 1 {
        return Err(io::Error::other(
            "App history requires one exact selected point",
        ));
    }

    Ok(())
}

fn validate_observation(value: &Value) -> io::Result<()> {
    if !value.is_object() {
        return Err(io::Error::other(
            "App history observation requires an object",
        ));
    }

    identity_words(&value["recording"])?;
    validate_stamp(&value["stamp"])?;
    integer(&value["run_tick"])?;
    validate_clock(value, "wall_ns")?;
    validate_clock(&value["stamp"], "continuous_ns")?;
    validate_clock(&value["stamp"], "awake_ns")?;

    Ok(())
}

fn validate_selected(value: &Value) -> io::Result<()> {
    validate_stamp(value)?;
    integer(&value["run_tick"])?;
    integer(&value["wall_ns"])?;

    Ok(())
}

fn validate_stamp(value: &Value) -> io::Result<()> {
    if !value.is_object() {
        return Err(io::Error::other("App observation stamp requires an object"));
    }

    identity(&value["run"])?;
    identity(&value["boot"])?;
    integer(&value["publication"])?;

    Ok(())
}

fn validate_clock(value: &Value, field: &str) -> io::Result<()> {
    let clock = value
        .get(field)
        .ok_or_else(|| io::Error::other(format!("Missing App observation {field}")))?;
    if !clock.is_null() {
        integer(clock)?;
    }

    Ok(())
}

fn identity_matches(input: &IdentityMatch<'_>) -> io::Result<bool> {
    let IdentityMatch { observed, selected } = input;
    let observed_stamp = observed.get("stamp").unwrap_or(observed);
    let selected_stamp = selected.get("stamp").unwrap_or(selected);
    for key in ["run", "boot"] {
        if identity(&observed_stamp[key])? != identity(&selected_stamp[key])? {
            return Ok(false);
        }
    }

    if integer(&observed_stamp["publication"])? != integer(&selected_stamp["publication"])? {
        return Ok(false);
    }

    for key in ["wall_ns", "run_tick"] {
        if integer(&observed[key])? != integer(&selected[key])? {
            return Ok(false);
        }
    }

    Ok(true)
}

fn identity(value: &Value) -> io::Result<[u64; 2]> {
    crate::live_projection::identity(value).map_err(io::Error::other)
}

fn identity_words(value: &Value) -> io::Result<[u64; 2]> {
    let words = value
        .as_array()
        .ok_or_else(|| io::Error::other("Missing App observation identity"))?;
    if words.len() != 2 {
        return Err(io::Error::other(
            "App observation identity requires two words",
        ));
    }

    Ok([integer(&words[0])?, integer(&words[1])?])
}

fn integer(value: &Value) -> io::Result<u64> {
    crate::live_projection::integer(value).map_err(io::Error::other)
}

#[cfg(test)]
#[path = "app/tests.rs"]
mod tests;
