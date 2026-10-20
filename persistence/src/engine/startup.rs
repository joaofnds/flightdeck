use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::{Error, Result};

/// Each Polars thread setting and the value it starts with when the environment lacks it.
pub const THREAD_DEFAULTS: [(&str, &str); 2] = [
    ("POLARS_MAX_THREADS", "1"),
    ("POLARS_ASYNC_THREAD_COUNT", "1"),
];
const THREADS_MAX: u32 = 64;

/// Establishes the application thread defaults before any engine or UI initialization.
/// A same-pid exec supplies missing variables without mutating a live process environment.
/// Explicit settings from 1 through 64 are preserved; other values return an error.
/// Library callers choose their own startup policy.
pub fn initialize_process() -> Result<()> {
    let settings = THREAD_DEFAULTS.map(|(option, _)| std::env::var_os(option));
    for value in settings.iter().flatten() {
        initialize_process_validate(value)?;
    }
    if settings.iter().all(Option::is_some) {
        return Ok(());
    }

    let mut command = Command::new(std::env::current_exe()?);
    command.args(std::env::args_os().skip(1));
    for ((option, default), value) in THREAD_DEFAULTS.into_iter().zip(settings) {
        match value {
            Some(value) => command.env(option, value),
            None => command.env(option, default),
        };
    }

    Err(command.exec().into())
}

fn initialize_process_validate(value: &std::ffi::OsStr) -> Result<()> {
    let invalid = || Error::Bound("Polars thread setting must be 1..=64");
    let count: u32 = value
        .to_str()
        .ok_or_else(invalid)?
        .parse()
        .map_err(|_| invalid())?;
    if !(1..=THREADS_MAX).contains(&count) {
        return Err(invalid());
    }

    Ok(())
}
