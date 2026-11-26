use std::fs::{self, File, FileTimes};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use super::*;

const DAY: Duration = Duration::from_hours(24);
const HASH: &str = "0123456789abcdef";
const OTHER_HASH: &str = "fedcba9876543210";

static SERIAL: AtomicUsize = AtomicUsize::new(0);

/// A target directory of one test's own, with an empty `debug` profile, removed when it ends.
struct Target {
    root: PathBuf,
    profile: PathBuf,
}

impl Target {
    fn new() -> Self {
        let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("fd-sweep-{}-{serial}", std::process::id()));
        let profile = root.join("debug");
        fs::create_dir_all(profile.join(".fingerprint")).unwrap();

        Self { root, profile }
    }
}

impl Drop for Target {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn removes_a_unit_no_build_has_read_for_two_days() {
    // Goal: a unit whose fingerprint no build has read for two days goes, with its outputs.
    // Method: date the read of the fingerprint's hash file three days back and sweep.
    let target = Target::new();
    let unit = fingerprint(&target.profile, days_ago(30), days_ago(3));
    let outputs = outputs(&target.profile, HASH);

    sweep(&target.root, now()).unwrap();

    assert!(!unit.exists());
    assert_eq!(existing(&outputs), Vec::<&PathBuf>::new());
}

#[test]
fn keeps_a_unit_a_build_read_within_two_days() {
    // Goal: a unit a build still reads stays, with its outputs, however old its build is.
    // Method: date the read of the fingerprint's hash file a day back and sweep.
    let target = Target::new();
    let unit = fingerprint(&target.profile, days_ago(30), days_ago(1));
    let outputs = outputs(&target.profile, HASH);

    sweep(&target.root, now()).unwrap();

    assert!(unit.exists());
    assert_eq!(missing(&outputs), Vec::<&PathBuf>::new());
}

#[test]
fn keeps_a_unit_built_within_two_days() {
    // Goal: a unit built a day ago stays, whatever its files' access times say.
    // Method: date the fingerprint's writes a day back and sweep.
    let target = Target::new();
    let unit = fingerprint(&target.profile, days_ago(1), days_ago(1));

    sweep(&target.root, now()).unwrap();

    assert!(unit.exists());
}

#[test]
fn a_read_of_the_fingerprint_description_is_no_use() {
    // Goal: a tool that reads the JSON beside a fingerprint keeps no unit alive.
    // Method: read the JSON now and the hash file three days back, and sweep.
    let target = Target::new();
    let unit = fingerprint(&target.profile, days_ago(30), days_ago(3));
    set_times(&unit.join("lib-flightdeck.json"), now(), days_ago(30));

    sweep(&target.root, now()).unwrap();

    assert!(!unit.exists());
}

#[test]
fn removes_the_outputs_of_a_unit_whose_fingerprint_is_gone() {
    // Goal: outputs no fingerprint names, which Cargo can never use again, go.
    // Method: keep a fresh unit under one hash beside outputs under another, and sweep.
    let target = Target::new();
    fingerprint(&target.profile, now(), now());
    let orphans = outputs(&target.profile, OTHER_HASH);

    sweep(&target.root, now()).unwrap();

    assert_eq!(existing(&orphans), Vec::<&PathBuf>::new());
}

#[test]
fn leaves_the_files_cargo_copies_out_of_deps() {
    // Goal: the binaries and libraries a build names without a hash stay.
    // Method: write old copies at the profile's top and in examples, and sweep.
    let target = Target::new();
    let copies = [
        write(&target.profile.join("collector"), days_ago(30)),
        write(&target.profile.join("libcollector.rlib"), days_ago(30)),
        write(&target.profile.join("examples/probe"), days_ago(30)),
    ];

    sweep(&target.root, now()).unwrap();

    assert_eq!(missing(&copies), Vec::<&PathBuf>::new());
}

#[test]
fn removes_incremental_state_not_written_for_two_days() {
    // Goal: a crate's incremental state goes once no build has written it for two days.
    // Method: write one crate's session three days back and another's a day back, and sweep.
    let target = Target::new();
    let stale = incremental(&target.profile, "collector-004ehrv3ezmer", days_ago(3));
    let fresh = incremental(&target.profile, "persister-02e8k8omfmofq", days_ago(1));

    sweep(&target.root, now()).unwrap();

    assert!(!stale.exists());
    assert!(fresh.exists());
}

#[test]
fn sweeps_a_profile_one_directory_deeper() {
    // Goal: a profile under a directory of its own, as cargo llvm-cov builds one, is swept.
    // Method: put an unused unit in llvm-cov-target/debug and sweep the target.
    let target = Target::new();
    let profile = target.root.join("llvm-cov-target/debug");
    let unit = fingerprint(&profile, days_ago(30), days_ago(3));

    sweep(&target.root, now()).unwrap();

    assert!(!unit.exists());
}

#[test]
fn passes_over_the_files_beside_the_profiles() {
    // Goal: the files Cargo writes beside the profiles, and those one directory deeper, are
    // neither profiles nor a reason to stop.
    // Method: write a file at the target's top and one in a directory under it, and sweep.
    let target = Target::new();
    let files = [
        write(&target.root.join(".rustc_info.json"), days_ago(30)),
        write(&target.root.join("tmp/scratch"), days_ago(30)),
    ];

    sweep(&target.root, now()).unwrap();

    assert_eq!(missing(&files), Vec::<&PathBuf>::new());
}

#[test]
fn leaves_a_profile_alone_while_a_build_holds_its_lock() {
    // Goal: the sweep removes nothing a running build may read or write.
    // Method: hold the profile's build lock, as Cargo does while it builds, and sweep.
    let target = Target::new();
    let unit = fingerprint(&target.profile, days_ago(30), days_ago(3));
    let lock = File::create(target.profile.join(".cargo-build-lock")).unwrap();
    lock.lock().unwrap();

    let report = sweep(&target.root, now()).unwrap();

    assert!(unit.exists());
    assert_eq!(report.busy, std::slice::from_ref(&target.profile));
}

#[test]
fn reads_the_target_from_its_flag() {
    // Goal: the one argument the tool takes is the target directory, behind its flag.
    // Method: pass the flag and a directory.
    let arguments = ["--target".to_owned(), "target".to_owned()];

    assert_eq!(target_argument(&arguments), Some(Path::new("target")));
}

#[test]
fn refuses_a_directory_without_the_flag() {
    // Goal: a bare directory is refused, so a mistyped call sweeps nothing.
    // Method: pass the directory alone.
    let arguments = ["target".to_owned()];

    assert_eq!(target_argument(&arguments), None);
}

fn now() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(2_000_000_000)
}

fn days_ago(days: u32) -> SystemTime {
    now() - DAY * days
}

/// A unit's fingerprint in `profile` as Cargo leaves it, written at `built`, whose hash file a
/// build last read at `read`.
fn fingerprint(profile: &Path, built: SystemTime, read: SystemTime) -> PathBuf {
    let unit = profile.join(format!(".fingerprint/flightdeck-{HASH}"));
    write(&unit.join("lib-flightdeck.json"), built);
    write(&unit.join("invoked.timestamp"), built);
    write(&unit.join("dep-lib-flightdeck"), built);
    let hash_file = write(&unit.join("lib-flightdeck"), built);
    set_times(&hash_file, read, built);

    unit
}

/// The outputs a unit built under `hash` leaves in `profile`, each a month old.
fn outputs(profile: &Path, hash: &str) -> [PathBuf; 5] {
    [
        write(
            &profile.join(format!("deps/libflightdeck-{hash}.rlib")),
            days_ago(30),
        ),
        write(
            &profile.join(format!("deps/flightdeck-{hash}.d")),
            days_ago(30),
        ),
        write(
            &profile.join(format!("deps/flightdeck-{hash}.cgu.01.rcgu.o")),
            days_ago(30),
        ),
        write(
            &profile.join(format!("build/flightdeck-{hash}/out/generated.rs")),
            days_ago(30),
        ),
        write(
            &profile.join(format!("examples/flightdeck-{hash}")),
            days_ago(30),
        ),
    ]
}

/// A crate's incremental directory in `profile` holding one session, all written at `written`.
fn incremental(profile: &Path, crate_directory: &str, written: SystemTime) -> PathBuf {
    let crate_path = profile.join("incremental").join(crate_directory);
    let session = crate_path.join("s-session");
    write(&session.join("query-cache.bin"), written);
    set_times(&session, written, written);
    set_times(&crate_path, written, written);

    crate_path
}

fn write(path: &Path, at: SystemTime) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"artifact").unwrap();
    set_times(path, at, at);

    path.to_owned()
}

fn set_times(path: &Path, accessed: SystemTime, modified: SystemTime) {
    let times = FileTimes::new()
        .set_accessed(accessed)
        .set_modified(modified);
    File::open(path).unwrap().set_times(times).unwrap();
}

fn existing(paths: &[PathBuf]) -> Vec<&PathBuf> {
    paths.iter().filter(|path| path.exists()).collect()
}

fn missing(paths: &[PathBuf]) -> Vec<&PathBuf> {
    paths.iter().filter(|path| !path.exists()).collect()
}
