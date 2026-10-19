//! Sends commands to a running Flightdeck's control socket and prints each JSON reply.
//! Start the app with `flightdeck --control SOCKET` from a build with the `automation`
//! feature, then run `flightdeck-control --socket SOCKET --command state`.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

/// The commands one run may send, which bounds its work.
const COMMANDS_MAX: usize = 64;
/// The longest reply read, enough for a page's rows.
const REPLY_LIMIT: u64 = 4 << 20;

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("flightdeck-control: {error}");
            ExitCode::from(2)
        }
    }
}

/// Sends every command in order and reports whether the window accepted all of them.
fn run() -> Result<bool, String> {
    let (socket, commands) = arguments()?;
    let stream = UnixStream::connect(&socket).map_err(|error| format!("{socket}: {error}"))?;
    let mut writer = stream.try_clone().map_err(|error| error.to_string())?;
    let mut reader = BufReader::new(stream);
    let mut accepted = true;
    for command in &commands {
        writeln!(writer, "{command}").map_err(|error| error.to_string())?;
        let mut reply = String::new();
        let read = (&mut reader)
            .take(REPLY_LIMIT)
            .read_line(&mut reply)
            .map_err(|error| error.to_string())?;
        if read == 0 {
            return Err(format!(
                "the app closed the socket before answering {command:?}"
            ));
        }
        print!("{reply}");
        if !reply.starts_with("{\"ok\":true") {
            accepted = false;
        }
    }
    Ok(accepted)
}

fn arguments() -> Result<(String, Vec<String>), String> {
    let mut socket = None;
    let mut commands = Vec::new();
    let mut arguments = std::env::args().skip(1).take(2 * COMMANDS_MAX + 2);
    while let Some(flag) = arguments.next() {
        let value = arguments
            .next()
            .ok_or_else(|| format!("{flag} needs a value"))?;
        match flag.as_str() {
            "--socket" => socket = Some(value),
            "--command" => commands.push(value),
            _ => {
                return Err(format!(
                    "unknown flag {flag}; use --socket PATH --command LINE"
                ));
            }
        }
    }
    let socket = socket.ok_or("--socket is required")?;
    if commands.is_empty() {
        return Err("give at least one --command".into());
    }
    if commands.len() > COMMANDS_MAX {
        return Err(format!("at most {COMMANDS_MAX} commands per run"));
    }
    Ok((socket, commands))
}
