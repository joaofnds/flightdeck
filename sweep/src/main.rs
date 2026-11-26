//! Removes from a Cargo target directory the build output no build has read for two days. Cargo
//! deletes no unit a newer build supersedes, and a dependency update or a new toolchain gives
//! every unit built on it a new hash, so without this the target directory keeps every copy.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{Duration, SystemTime};

/// A build reads each unit's fingerprint on every run, and APFS refreshes an access time only
/// once it is about a day old, so a unit read today may still show yesterday's read.
const UNUSED_FOR: Duration = Duration::from_hours(48);
/// Cargo holds these in a profile directory while it builds there.
const LOCKS: [&str; 3] = [".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"];
/// The directories of a profile that keep a unit's outputs under the unit's hash.
const UNIT_OUTPUTS: [&str; 3] = ["deps", "build", "examples"];
const HASH_LENGTH: usize = 16;
/// Eighteen times the 452,080 entries of the largest directory a target here held, so a
/// directory past it is no target directory.
const ENTRIES_MAX: usize = 1 << 23;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let Some(target) = target_argument(&arguments) else {
        eprintln!("usage: sweep --target <directory>");
        return ExitCode::FAILURE;
    };

    match sweep(target, SystemTime::now()) {
        Ok(report) => {
            if report.removed_count > 0 {
                println!(
                    "sweep: removed {} entries no build read for two days",
                    report.removed_count
                );
            }
            for profile in &report.busy {
                println!("sweep: left {} alone, a build holds it", profile.display());
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("sweep: {error}");
            ExitCode::FAILURE
        }
    }
}

#[derive(Debug, Default)]
struct Report {
    /// The units, outputs and incremental directories removed.
    removed_count: usize,
    /// The profile directories a build held, which the sweep left alone.
    busy: Vec<PathBuf>,
}

fn target_argument(arguments: &[String]) -> Option<&Path> {
    match arguments {
        [flag, target] if flag == "--target" => Some(Path::new(target)),
        _ => None,
    }
}

/// Sweeps every profile directory of `target` that no build holds, as of `now`.
fn sweep(target: &Path, now: SystemTime) -> io::Result<Report> {
    let cutoff = now
        .checked_sub(UNUSED_FOR)
        .ok_or_else(|| io::Error::other("the clock reads before the sweep's window"))?;

    let mut report = Report::default();
    for profile in sweep_profiles(target)? {
        let Some(locks) = sweep_lock(&profile)? else {
            report.busy.push(profile);
            continue;
        };
        report.removed_count += sweep_profile(&profile, cutoff)?;
        drop(locks);
    }

    Ok(report)
}

/// The directories of `target` Cargo builds a profile in, such as `debug`, and one level deeper,
/// such as `llvm-cov-target/debug`.
fn sweep_profiles(target: &Path) -> io::Result<Vec<PathBuf>> {
    let mut profiles = Vec::new();
    for child in entries(target)? {
        if directory_exists(&child.join(".fingerprint"))? {
            profiles.push(child);
        } else if directory_exists(&child)? {
            for grandchild in entries(&child)? {
                if directory_exists(&grandchild.join(".fingerprint"))? {
                    profiles.push(grandchild);
                }
            }
        }
    }
    profiles.sort();

    Ok(profiles)
}

/// Takes every lock Cargo takes in `profile`, or gives `None` where a build holds one.
fn sweep_lock(profile: &Path) -> io::Result<Option<Vec<File>>> {
    let mut locks = Vec::with_capacity(LOCKS.len());
    for name in LOCKS {
        let path = profile.join(name);
        let lock = OpenOptions::new()
            .append(true)
            .create(true)
            .open(&path)
            .map_err(|error| with_path(&path, &error))?;
        match lock.try_lock() {
            Ok(()) => locks.push(lock),
            Err(TryLockError::WouldBlock) => return Ok(None),
            Err(TryLockError::Error(error)) => return Err(with_path(&path, &error)),
        }
    }

    Ok(Some(locks))
}

/// Removes the units of `profile` no build has read since `cutoff`, the outputs no remaining
/// unit names, and the incremental state no build has written since `cutoff`, and gives how many
/// it removed.
fn sweep_profile(profile: &Path, cutoff: SystemTime) -> io::Result<usize> {
    let mut removed_count = 0;
    let mut live = HashSet::new();
    for unit in entries(&profile.join(".fingerprint"))? {
        if sweep_profile_last_read(&unit)? >= cutoff {
            live.extend(file_name(&unit).and_then(unit_hash).map(str::to_owned));
        } else {
            remove(&unit)?;
            removed_count += 1;
        }
    }

    for output in UNIT_OUTPUTS {
        for entry in entries(&profile.join(output))? {
            if let Some(hash) = file_name(&entry).and_then(unit_hash)
                && !live.contains(hash)
            {
                remove(&entry)?;
                removed_count += 1;
            }
        }
    }

    for crate_path in entries(&profile.join("incremental"))? {
        if sweep_profile_last_write(&crate_path)? < cutoff {
            remove(&crate_path)?;
            removed_count += 1;
        }
    }

    Ok(removed_count)
}

/// When a build last read the unit whose fingerprint directory is `unit`. A build reads the
/// fingerprint's hash file on every run, while tools that inspect a target read the JSON beside
/// it, so a JSON file's access time is no use of the unit.
fn sweep_profile_last_read(unit: &Path) -> io::Result<SystemTime> {
    let mut last = SystemTime::UNIX_EPOCH;
    for file in entries(unit)? {
        let metadata = fs::symlink_metadata(&file).map_err(|error| with_path(&file, &error))?;
        last = last.max(metadata.modified()?);
        if file.extension() != Some(OsStr::new("json")) {
            last = last.max(metadata.accessed()?);
        }
    }

    Ok(last)
}

/// When a build last wrote a crate's incremental directory, `crate_path`, or a session in it.
fn sweep_profile_last_write(crate_path: &Path) -> io::Result<SystemTime> {
    let mut last = modified(crate_path)?;
    for session in entries(crate_path)? {
        last = last.max(modified(&session)?);
    }

    Ok(last)
}

/// The hash Cargo gives the unit `name` belongs to: sixteen lowercase hexadecimal digits after a
/// dash, at the end of the name or before a dot.
fn unit_hash(name: &str) -> Option<&str> {
    for (dash, _) in name.match_indices('-') {
        let start = dash + 1;
        let end = start + HASH_LENGTH;
        let Some(hash) = name.get(start..end) else {
            continue;
        };
        let hexadecimal = hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
        let ends = name
            .get(end..)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('.'));
        if hexadecimal && ends {
            return Some(hash);
        }
    }

    None
}

/// The entries of `directory`, or none where it does not exist.
fn entries(directory: &Path) -> io::Result<Vec<PathBuf>> {
    let listing = match fs::read_dir(directory) {
        Ok(listing) => listing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(with_path(directory, &error)),
    };

    let mut paths = Vec::new();
    for entry in listing {
        if paths.len() == ENTRIES_MAX {
            return Err(io::Error::other(format!(
                "{} holds over {ENTRIES_MAX} entries",
                directory.display()
            )));
        }
        paths.push(entry.map_err(|error| with_path(directory, &error))?.path());
    }

    Ok(paths)
}

/// Whether a directory is at `path`, which none is where the path runs through a file.
fn directory_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) if error.kind() == io::ErrorKind::NotADirectory => Ok(false),
        Err(error) => Err(with_path(path, &error)),
    }
}

fn file_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(OsStr::to_str)
}

fn modified(path: &Path) -> io::Result<SystemTime> {
    fs::symlink_metadata(path)
        .and_then(|metadata| metadata.modified())
        .map_err(|error| with_path(path, &error))
}

fn remove(path: &Path) -> io::Result<()> {
    let removed = if directory_exists(path)? {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };

    removed.map_err(|error| with_path(path, &error))
}

fn with_path(path: &Path, error: &io::Error) -> io::Error {
    io::Error::new(error.kind(), format!("{}: {error}", path.display()))
}

#[cfg(test)]
mod tests;
