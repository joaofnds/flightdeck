//! Owns a frozen endpoint while separate benchmark executables attach to it.
use std::io::{self, BufRead, Read, Write};
use std::path::Path;

use serde_json::json;

use super::frozen::{Directories, FrozenRecording};

const COMMANDS_MAX: u32 = 1024;
const COMMAND_BYTES_MAX: u64 = 128;

pub(super) fn run() -> Result<(), Box<dyn std::error::Error>> {
    let words: Vec<_> = std::env::args().skip(1).take(5).collect();
    if words.len() != 4 {
        return Err(io::Error::other("--freeze SOURCE --directory TARGET").into());
    }
    if words[2] != "--directory" {
        return Err(io::Error::other("expected --directory").into());
    }
    let fixture = FrozenRecording::capture(&Directories {
        source: Path::new(&words[1]),
        target: Path::new(&words[3]),
    })?;
    println!(
        "{}",
        json!({
            "root": fixture.root(), "publication_newest": fixture.publication_newest(),
            "retained_publications": fixture.retained_publications(),
            "committed_windows": fixture.committed_windows(),
        })
    );
    io::stdout().flush()?;
    let mut input = io::stdin().lock();
    for _ in 0..COMMANDS_MAX {
        let mut line = String::new();
        let read = input
            .by_ref()
            .take(COMMAND_BYTES_MAX + 1)
            .read_line(&mut line)?;
        if read == 0 {
            return Ok(());
        }
        if u64::try_from(read)? > COMMAND_BYTES_MAX {
            return Err(io::Error::other("fixture command length bound").into());
        }
        if line.trim() == "quit" {
            return Ok(());
        }
        let result = command(&fixture, line.trim());
        match result {
            Ok(()) => println!("{}", json!({"ok":true})),
            Err(error) => println!("{}", json!({"ok":false,"error":error.to_string()})),
        }
        io::stdout().flush()?;
    }
    Err(io::Error::other("fixture command count bound").into())
}

fn command(fixture: &FrozenRecording, line: &str) -> io::Result<()> {
    if line == "advance" {
        return fixture.advance();
    }
    if let Some(gap) = line.strip_prefix("checkpoint ") {
        return fixture.checkpoint(gap.parse().map_err(io::Error::other)?);
    }

    Err(io::Error::other("expected checkpoint N, advance, or quit"))
}
