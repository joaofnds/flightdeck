use std::os::unix::process::CommandExt;
use std::process::Command;

use crate::{Error, Result};

/// The thread counts of the two Polars pools, the compute pool's maximum and the async pool's.
#[derive(Clone, Copy)]
pub struct ThreadCounts {
    pub maximum: u32,
    pub asynchronous: u32,
}

impl ThreadCounts {
    /// Each Polars setting with its count, as the environment carries them.
    #[must_use]
    pub fn environment(self) -> [(&'static str, String); 2] {
        [
            ("POLARS_MAX_THREADS", self.maximum.to_string()),
            ("POLARS_ASYNC_THREAD_COUNT", self.asynchronous.to_string()),
        ]
    }
}

/// The thread counts a process starts with when the environment lacks a setting.
pub const THREAD_DEFAULTS: ThreadCounts = ThreadCounts {
    maximum: 1,
    asynchronous: 1,
};
const THREADS_MAX: u32 = 64;

/// Establishes the application thread defaults before any engine or UI initialization.
/// A same-pid exec supplies missing variables without mutating a live process environment.
/// Explicit settings from 1 through 64 are preserved; other values return an error.
/// Library callers choose their own startup policy.
pub fn initialize_process() -> Result<()> {
    initialize_process_with(THREAD_DEFAULTS)
}

/// Establishes thread defaults as `initialize_process` does, with `defaults` in place of the
/// library's.
pub fn initialize_process_with(defaults: ThreadCounts) -> Result<()> {
    for count in [defaults.maximum, defaults.asynchronous] {
        assert!(1 <= count);
        assert!(count <= THREADS_MAX);
    }

    let environment = defaults.environment();
    let settings = environment
        .each_ref()
        .map(|(option, _)| std::env::var_os(option));
    for value in settings.iter().flatten() {
        initialize_process_validate(value)?;
    }
    if settings.iter().all(Option::is_some) {
        return Ok(());
    }

    let mut command = Command::new(std::env::current_exe()?);
    command.args(std::env::args_os().skip(1));
    for ((option, default), value) in environment.into_iter().zip(settings) {
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
