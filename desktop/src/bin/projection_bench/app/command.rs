use std::ffi::{OsStr, OsString};
use std::io;
use std::path::Path;

use serde_json::{Value, json};
use viewer::live_app_response::KeyKind;

pub(super) enum Command<'arguments> {
    Capture(Capture<'arguments>),
    Measure(Measure<'arguments>),
}

pub(super) struct Capture<'arguments> {
    pub recording: &'arguments Path,
    pub second: u64,
    pub alternative: usize,
    pub key: Value,
    pub kind: KeyKind,
    pub bytes: Vec<u8>,
    pub output: &'arguments Path,
}

pub(super) struct Measure<'arguments> {
    pub input: &'arguments Path,
    pub iterations: u32,
    pub output: &'arguments Path,
}

impl<'arguments> Command<'arguments> {
    pub(super) fn parse(arguments: &'arguments [OsString]) -> io::Result<Self> {
        match arguments.first().and_then(|value| value.to_str()) {
            Some("--capture-app") => capture(arguments).map(Self::Capture),
            Some("--app-input") => measure(arguments).map(Self::Measure),
            _ => Err(io::Error::other("Expected --capture-app or --app-input")),
        }
    }
}

fn capture(arguments: &[OsString]) -> io::Result<Capture<'_>> {
    if arguments.len() != 12 {
        return Err(io::Error::other(
            "--capture-app RECORDING --second SECOND --alternative INDEX \
             --kind bundle|executable --bytes-hex HEX --output NEW_FILE",
        ));
    }

    flags(
        arguments,
        &[
            (2, "--second"),
            (4, "--alternative"),
            (6, "--kind"),
            (8, "--bytes-hex"),
            (10, "--output"),
        ],
    )?;
    let bytes_hex = text(&arguments[9])?;
    let key = json!({"kind":text(&arguments[7])?,"bytes_hex":bytes_hex});
    let kind = super::validate_key(&key)?;
    let bytes = super::capture_key(bytes_hex)?;
    let alternative = usize::try_from(unsigned(&arguments[5])?).map_err(io::Error::other)?;
    if alternative >= crate::data::LIVE_SAMPLE_LIMIT {
        return Err(io::Error::other("Observation alternative exceeds 4095"));
    }

    Ok(Capture {
        recording: Path::new(&arguments[1]),
        second: unsigned(&arguments[3])?,
        alternative,
        key,
        kind,
        bytes,
        output: Path::new(&arguments[11]),
    })
}

fn measure(arguments: &[OsString]) -> io::Result<Measure<'_>> {
    if arguments.len() != 6 {
        return Err(io::Error::other(
            "--app-input FILE --iterations 1..100 --output NEW_FILE",
        ));
    }

    flags(arguments, &[(2, "--iterations"), (4, "--output")])?;

    Ok(Measure {
        input: Path::new(&arguments[1]),
        iterations: super::iterations(&arguments[3])?,
        output: Path::new(&arguments[5]),
    })
}

fn flags(arguments: &[OsString], expected: &[(usize, &str)]) -> io::Result<()> {
    assert!(expected.len() <= 5);
    for (index, flag) in expected {
        if arguments[*index] != *flag {
            return Err(io::Error::other(format!("Expected {flag}")));
        }
    }

    Ok(())
}

fn text(value: &OsStr) -> io::Result<&str> {
    value
        .to_str()
        .ok_or_else(|| io::Error::other("Argument is not Unicode"))
}

fn unsigned(value: &OsStr) -> io::Result<u64> {
    let value = text(value)?;
    if value.len() > 20 {
        return Err(io::Error::other("Unsigned argument exceeds its bound"));
    }

    value.parse().map_err(io::Error::other)
}
