#![cfg(test)]

use std::ffi::{OsStr, OsString};
use std::fmt::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::Command;

const PROBE_DIRECTORY: &str = "FLIGHTDECK_ENGINE_STARTUP_TEST";
/// The thread counts a child passes its initializer, as "maximum asynchronous", where the
/// case names them.
const DEFAULTS: &str = "FLIGHTDECK_ENGINE_STARTUP_DEFAULTS";

#[test]
fn missing_settings_preserve_the_process_and_arguments() {
    // Goal: both defaults retain the process and every argument across one re-exec.
    // Method: isolated children compare parent-written raw arguments, pid and entry count.
    run_case(
        "missing_settings_preserve_the_process_and_arguments",
        &Settings {
            maximum: None,
            asynchronous: None,
            valid: true,
        },
    );
}

#[test]
fn explicit_settings_preserve_the_process_and_arguments_without_reexecution() {
    // Goal: valid overrides retain their values without re-executing the process.
    // Method: an isolated child compares raw arguments, settings, pid and entry count.
    run_case(
        "explicit_settings_preserve_the_process_and_arguments_without_reexecution",
        &Settings {
            maximum: Some("2"),
            asynchronous: Some("2"),
            valid: true,
        },
    );
}

#[test]
fn missing_async_setting_preserves_the_explicit_maximum_and_arguments() {
    // Goal: filling the async default preserves the maximum override and argument vector.
    // Method: an isolated child checks parent-written arguments and one same-pid re-exec.
    run_case(
        "missing_async_setting_preserves_the_explicit_maximum_and_arguments",
        &Settings {
            maximum: Some("2"),
            asynchronous: None,
            valid: true,
        },
    );
}

#[test]
fn missing_maximum_setting_preserves_the_explicit_async_count_and_arguments() {
    // Goal: filling the maximum default preserves the async override and argument vector.
    // Method: an isolated child checks parent-written arguments and one same-pid re-exec.
    run_case(
        "missing_maximum_setting_preserves_the_explicit_async_count_and_arguments",
        &Settings {
            maximum: None,
            asynchronous: Some("2"),
            valid: true,
        },
    );
}

#[test]
fn missing_settings_take_the_caller_s_defaults() {
    // Goal: a caller that starts with defaults of its own, as the app does, gets its own counts
    // for missing settings rather than the library's.
    // Method: an isolated child passes defaults of 3 and 2 with neither setting present, and
    // checks both settings after its one re-exec.
    run_case_with_defaults(
        "missing_settings_take_the_caller_s_defaults",
        &Settings {
            maximum: None,
            asynchronous: None,
            valid: true,
        },
        Some(persistence::engine::ThreadCounts {
            maximum: 3,
            asynchronous: 2,
        }),
    );
}

#[test]
#[should_panic(expected = "1 <= count")]
fn a_caller_s_zero_default_is_refused_before_reading_the_environment() {
    // Goal: a caller's default outside the bound is the caller's bug, so it stops the program
    // rather than re-executing into an error that blames the environment.
    // Method: pass a maximum default of zero in process, where the assertion fires first.
    let defaults = persistence::engine::ThreadCounts {
        maximum: 0,
        asynchronous: 1,
    };

    persistence::engine::initialize_process_with(defaults).unwrap();
}

#[test]
fn zero_maximum_is_rejected_without_reexecution() {
    // Goal: a zero native pool size fails before any re-exec.
    // Method: an isolated child observes the bounded error and its sole process entry.
    run_case(
        "zero_maximum_is_rejected_without_reexecution",
        &Settings {
            maximum: Some("0"),
            asynchronous: Some("1"),
            valid: false,
        },
    );
}

#[test]
fn nonnumeric_async_count_is_rejected_without_reexecution() {
    // Goal: a nonnumeric async pool size fails before any re-exec.
    // Method: an isolated child observes the bounded error and its sole process entry.
    run_case(
        "nonnumeric_async_count_is_rejected_without_reexecution",
        &Settings {
            maximum: Some("1"),
            asynchronous: Some("invalid"),
            valid: false,
        },
    );
}

#[test]
fn excessive_async_count_is_rejected_without_reexecution() {
    // Goal: an async pool size above the supported bound fails before any re-exec.
    // Method: an isolated child observes the bounded error and its sole process entry.
    run_case(
        "excessive_async_count_is_rejected_without_reexecution",
        &Settings {
            maximum: Some("1"),
            asynchronous: Some("65"),
            valid: false,
        },
    );
}

#[test]
fn overflowing_maximum_is_rejected_before_filling_the_async_default() {
    // Goal: an overflowing maximum fails before a missing setting causes re-exec.
    // Method: an isolated child observes the bounded error and its sole process entry.
    run_case(
        "overflowing_maximum_is_rejected_before_filling_the_async_default",
        &Settings {
            maximum: Some("18446744073709551615"),
            asynchronous: None,
            valid: false,
        },
    );
}

struct Settings {
    maximum: Option<&'static str>,
    asynchronous: Option<&'static str>,
    valid: bool,
}

fn run_case(name: &str, settings: &Settings) {
    run_case_with_defaults(name, settings, None);
}

/// Runs the case `name` in a child whose initializer takes `defaults`, or the library's
/// defaults where it is `None`.
fn run_case_with_defaults(
    name: &str,
    settings: &Settings,
    defaults: Option<persistence::engine::ThreadCounts>,
) {
    if let Some(directory) = std::env::var_os(PROBE_DIRECTORY) {
        assert_child(&PathBuf::from(directory));
        return;
    }

    let directory = Directory::new(name);
    let executable = std::env::current_exe().unwrap();
    let arguments = [
        executable.as_os_str(),
        OsStr::new("--exact"),
        OsStr::new(name),
        OsStr::new("--nocapture"),
        OsStr::new("--test-threads=1"),
    ];
    let expected: Vec<u8> = arguments
        .iter()
        .flat_map(|argument| argument.as_bytes().iter().copied().chain([0]))
        .collect();
    std::fs::write(directory.0.join("arguments"), expected).unwrap();
    let [(_, maximum), (_, asynchronous)] = defaults
        .unwrap_or(persistence::engine::THREAD_DEFAULTS)
        .environment();
    let mut command = Command::new(&executable);
    command
        .args(&arguments[1..])
        .env(PROBE_DIRECTORY, &directory.0)
        .env(
            "FLIGHTDECK_ENGINE_STARTUP_VALID",
            settings.valid.to_string(),
        )
        .env(
            "FLIGHTDECK_ENGINE_STARTUP_MAXIMUM",
            settings.maximum.map_or(maximum, String::from),
        )
        .env(
            "FLIGHTDECK_ENGINE_STARTUP_ASYNC",
            settings.asynchronous.map_or(asynchronous, String::from),
        );
    if let Some(defaults) = defaults {
        let counts = format!("{} {}", defaults.maximum, defaults.asynchronous);
        command.env(DEFAULTS, counts);
    }

    run_case_settings(&mut command, settings);
    let output = command.output().unwrap();
    assert!(output.status.success(), "{output:?}");
}

fn run_case_settings(command: &mut Command, settings: &Settings) {
    let mut entries = 1;
    for (option, value) in [
        ("POLARS_MAX_THREADS", settings.maximum),
        ("POLARS_ASYNC_THREAD_COUNT", settings.asynchronous),
    ] {
        command.env_remove(option);
        if let Some(value) = value {
            command.env(option, value);
        } else {
            if settings.valid {
                entries = 2;
            }
        }
    }
    command.env("FLIGHTDECK_ENGINE_STARTUP_ENTRIES", entries.to_string());
}

fn assert_child(directory: &Path) {
    assert_child_arguments(directory);
    let path = directory.join("entries");
    let mut entries = if path.try_exists().unwrap() {
        std::fs::read_to_string(&path).unwrap()
    } else {
        String::new()
    };
    // A broken initializer cannot re-exec indefinitely inside this isolated test.
    assert!(entries.lines().count() < 2);
    writeln!(entries, "{}", std::process::id()).unwrap();
    std::fs::write(&path, &entries).unwrap();
    let result = match std::env::var(DEFAULTS) {
        Ok(defaults) => {
            persistence::engine::initialize_process_with(assert_child_defaults(&defaults))
        }
        Err(std::env::VarError::NotPresent) => persistence::engine::initialize_process(),
        Err(error) => panic!("{error}"),
    };
    let valid: bool = std::env::var("FLIGHTDECK_ENGINE_STARTUP_VALID")
        .unwrap()
        .parse()
        .unwrap();
    if !valid {
        assert!(matches!(result, Err(persistence::Error::Bound(_))));
        assert_eq!(entries.lines().count(), 1);
        return;
    }
    result.unwrap();
    assert_child_arguments(directory);

    let expected: usize = std::env::var("FLIGHTDECK_ENGINE_STARTUP_ENTRIES")
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(entries.lines().count(), expected);
    for previous in entries.lines() {
        assert_eq!(previous.parse::<u32>().unwrap(), std::process::id());
    }
    assert_eq!(
        std::env::var("POLARS_MAX_THREADS").unwrap(),
        std::env::var("FLIGHTDECK_ENGINE_STARTUP_MAXIMUM").unwrap()
    );
    assert_eq!(
        std::env::var("POLARS_ASYNC_THREAD_COUNT").unwrap(),
        std::env::var("FLIGHTDECK_ENGINE_STARTUP_ASYNC").unwrap()
    );
}

fn assert_child_defaults(text: &str) -> persistence::engine::ThreadCounts {
    let (maximum, asynchronous) = text.split_once(' ').unwrap();
    persistence::engine::ThreadCounts {
        maximum: maximum.parse().unwrap(),
        asynchronous: asynchronous.parse().unwrap(),
    }
}

fn assert_child_arguments(directory: &Path) {
    let bytes = std::fs::read(directory.join("arguments")).unwrap();
    let expected: Vec<OsString> = bytes
        .split_inclusive(|byte| *byte == 0)
        .map(|argument| {
            assert_eq!(argument.last(), Some(&0));
            OsString::from_vec(argument[..argument.len() - 1].to_vec())
        })
        .collect();

    assert_eq!(std::env::args_os().collect::<Vec<_>>(), expected);
}

struct Directory(PathBuf);

impl Directory {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "flightdeck-engine-startup-{}-{name}",
            std::process::id()
        ));
        std::fs::create_dir(&path).unwrap();
        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
