//! An independent source-row reference and disk-only comparison for live acceptance.

#[path = "recording_check/capture.rs"]
mod capture;
#[path = "recording_check/compare.rs"]
mod compare;
#[path = "recording_check/identity_reference.rs"]
mod identity_reference;
#[path = "recording_check/rows.rs"]
mod rows;

use std::path::PathBuf;

use persistence::{Error, Result};

fn main() {
    if let Err(error) = run() {
        eprintln!("recording_check: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    persistence::engine::initialize_process()?;
    let arguments: Vec<String> = std::env::args().collect();
    let mode = arguments
        .get(1)
        .ok_or(Error::Bound("capture or verify mode required"))?;
    let reference = PathBuf::from(argument(&arguments, "--reference")?);
    match mode.as_str() {
        "capture" => {
            let lock = PathBuf::from(argument(&arguments, "--lock-path")?);
            let seconds = argument(&arguments, "--seconds")?
                .parse()
                .map_err(|_| Error::Bound("capture seconds"))?;
            capture::run(&lock, &reference, seconds)
        }
        "verify" => {
            let root = PathBuf::from(argument(&arguments, "--recording")?);
            compare::run(&root, &reference)
        }
        _ => Err(Error::Bound("unknown checker mode")),
    }
}

fn argument<'arguments>(arguments: &'arguments [String], flag: &str) -> Result<&'arguments str> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].as_str())
        .ok_or(Error::Bound("missing checker argument"))
}
