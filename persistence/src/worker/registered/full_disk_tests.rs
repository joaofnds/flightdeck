//! What a recording does when its disk has no room left, on a small volume of its own.
use std::fs::File;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::*;

/// Large enough for the whole working files of a few saved minutes.
const VOLUME_SIZE: &str = "256m";
const CHUNK_BYTES: usize = 1 << 20;
const FILES_MAX: usize = 1 << 10;
/// How many times a test asks for its volume to go, a second apart, since another process can hold
/// the disk open after the volume unmounted. A holder that outlasts this fails the test, named.
const DETACH_ATTEMPTS: usize = 30;

/// A small APFS volume mounted for one test.
struct Volume {
    directory: PathBuf,
    mount: PathBuf,
    /// The image's whole disk, which still names it once the volume has unmounted.
    device: String,
}

impl Volume {
    fn new(name: &str) -> Self {
        let directory =
            std::env::temp_dir().join(format!("fd-volume-{}-{name}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let image = directory.join("volume.dmg");
        let mount = directory.join("mount");
        std::fs::create_dir(&mount).unwrap();
        volume_command(
            &[
                "create",
                "-quiet",
                "-fs",
                "APFS",
                "-volname",
                "fd",
                "-size",
                VOLUME_SIZE,
            ],
            &image,
        );
        let attached = Command::new("hdiutil")
            .args(["attach", "-nobrowse", "-noverify", "-mountpoint"])
            .arg(&mount)
            .arg(&image)
            .output()
            .unwrap();
        assert!(attached.status.success());
        let listed = String::from_utf8(attached.stdout).unwrap();
        let device = listed.split_whitespace().next().unwrap().to_owned();
        assert!(device.starts_with("/dev/disk"), "{listed}");

        Self {
            directory,
            mount,
            device,
        }
    }

    /// Writes files of smaller and smaller sizes until the volume refuses the smallest, and gives
    /// their directory, whose removal gives the room back.
    fn fill(&self) -> PathBuf {
        let directory = self.mount.join("filler");
        std::fs::create_dir(&directory).unwrap();
        let chunk = vec![0x5a_u8; CHUNK_BYTES];
        let mut written = 0_usize;
        for size in [CHUNK_BYTES, CHUNK_BYTES >> 4, CHUNK_BYTES >> 8] {
            for _file in 0..FILES_MAX {
                let path = directory.join(written.to_string());
                written += 1;
                if !volume_file_written(&path, &chunk[..size]) {
                    break;
                }
            }
        }

        directory
    }
}

/// Whether the volume took a new file of `bytes`.
fn volume_file_written(path: &Path, bytes: &[u8]) -> bool {
    let written = File::create(path).and_then(|mut file| {
        file.write_all(bytes)?;
        file.sync_all()
    });
    match written {
        Ok(()) => true,
        Err(error) if error.kind() == ErrorKind::StorageFull => false,
        Err(error) => panic!("filling the volume: {error}"),
    }
}

impl Drop for Volume {
    fn drop(&mut self) {
        let mut refusal = String::new();
        for attempt in 0..DETACH_ATTEMPTS {
            let detached = Command::new("hdiutil")
                .args(["detach", "-force", &self.device])
                .output()
                .unwrap();
            if detached.status.success() {
                std::fs::remove_dir_all(&self.directory).unwrap();
                return;
            }
            refusal = String::from_utf8_lossy(&detached.stderr).into_owned();
            if attempt == 0 {
                eprintln!("{refusal}{}", volume_holders());
            }
            std::thread::sleep(Duration::from_secs(1));
        }
        panic!("{refusal}{}", volume_holders());
    }
}

/// What this user can see holding a disk open, and the system's processes that open disks, whose
/// files only root can see.
fn volume_holders() -> String {
    let files = Command::new("lsof").args(["-n", "-P"]).output().unwrap();
    let processes = Command::new("ps")
        .args(["-axo", "pid,etime,command"])
        .output()
        .unwrap();
    let files = String::from_utf8_lossy(&files.stdout).into_owned();
    let processes = String::from_utf8_lossy(&processes.stdout).into_owned();
    let disks = files
        .lines()
        .filter(|line| line.contains("/dev/disk") || line.contains("/dev/rdisk"));
    let tools = processes.lines().filter(|line| {
        [
            "fsck",
            "/mds",
            "mdworker",
            "diskimages",
            "hdiutil",
            "diskutil",
        ]
        .iter()
        .any(|tool| line.contains(tool))
    });

    disks.chain(tools).collect::<Vec<_>>().join("\n")
}

fn volume_command(arguments: &[&str], path: &Path) {
    let status = Command::new("hdiutil")
        .args(arguments)
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn a_save_the_disk_refuses_leaves_what_was_saved_and_succeeds_once_there_is_room() {
    // Goal: a full disk stops a save and costs no reading, saved or still in the ring.
    // Method: save a minute, fill the volume, ask for the next minute, give the room back, and
    // restart the worker.
    let volume = Volume::new("refused-save");
    let mut recording = Recording::create_in(&volume.mount, false, shape());
    recording.publish_in_minute(0);
    recording.publish_in_minute(1);
    let store = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();
    assert!(matches!(
        recording.persist_live(&store, &mut state),
        Progress::Sealed
    ));
    assert_eq!(recording.saved_manifests().len(), 1);
    let filler = volume.fill();
    recording.publish_in_minute(2);

    let refused = recording.persist_live_result(&store, &mut state);
    let saved_while_full = recording.saved_manifests().len();
    std::fs::remove_dir_all(&filler).unwrap();
    drop(store);
    let store = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();
    let saved_with_room = recording.persist_live(&store, &mut state);

    assert!(matches!(
        refused,
        Err(crate::Error::Io(error)) if error.kind() == ErrorKind::StorageFull
    ));
    assert_eq!(saved_while_full, 1);
    assert!(matches!(saved_with_room, Progress::Sealed));
    let saved = recording.saved_manifests();
    assert_eq!(saved.len(), 2);
    assert_eq!(saved[0].publication_end, saved[1].publication_first);
}

const CLOCK_NS: u64 = 1 << 62;
const PERIOD_SECONDS: u64 = 60;

/// A recording on `volume` whose clock stands still at `CLOCK_NS`, and whose period is a minute, so
/// a reading at continuous `removal_cutoff_ns` is the first that removal does not take. Its first
/// pass has set room aside, and it holds three saved minutes.
fn saved_on_placed_clock(
    volume: &Volume,
    removal_cutoff_ns: u64,
) -> (Recording, Store, ClockReading) {
    let (recording, store, now) = published_on_placed_clock(volume, removal_cutoff_ns);
    let mut state = RunState::default();
    persist_until_quiet(&recording, &store, &mut state);
    assert_eq!(recording.saved_manifests().len(), 3);

    (recording, store, now)
}

/// As above with three minutes published and none saved yet.
fn published_on_placed_clock(
    volume: &Volume,
    removal_cutoff_ns: u64,
) -> (Recording, Store, ClockReading) {
    const GRACE_SECONDS: u64 = 60;
    let mut recording = Recording::create_in(&volume.mount, false, shape());
    let root = PrivateDirectory::open(&recording.root).unwrap();
    let removal_ns = CLOCK_NS - (PERIOD_SECONDS + GRACE_SECONDS) * 1_000_000_000;
    Period::from_seconds(PERIOD_SECONDS)
        .unwrap()
        .write(&root)
        .unwrap();
    boot_offset_save(&root, recording.run.boot, removal_ns - removal_cutoff_ns).unwrap();
    let now = ClockReading {
        boot: Identity([9, 9]),
        continuous_ns: 0,
        wall_ns: CLOCK_NS,
    };
    let store = Store::open(&recording.root).unwrap();
    let first = store.retire_expired(&now, &[], &super::super::persist_budget(0).unwrap());
    assert_eq!(first.unwrap(), 0);
    recording.publish_in_minute(0);
    recording.publish_in_minute(1);
    recording.publish_in_minute(1);
    recording.publish_in_minute(2);

    (recording, store, now)
}

fn volume_refuses_a_small_file(filler: &Path) -> bool {
    !volume_file_written(&filler.join("refused"), &[0; 4096])
}

fn reserve_bytes(recording: &Recording) -> Option<u64> {
    let reserve = recording.root.join("retention/reserve");

    std::fs::metadata(reserve).ok().map(|reserve| reserve.len())
}

#[test]
fn a_pass_on_a_full_disk_still_removes_what_expired() {
    // Goal: the one thing that can make room works without any.
    // Method: save three minutes on a clock that has expired the first, fill the volume, and take
    // a pass of physical removal.
    let volume = Volume::new("full-pass");
    let (recording, store, now) = saved_on_placed_clock(&volume, 60_000_000_003);
    let filler = volume.fill();
    assert!(volume_refuses_a_small_file(&filler));

    let removed = store.retire_expired(&now, &[], &super::super::persist_budget(0).unwrap());

    assert_eq!(removed.unwrap(), 1);
    assert_eq!(recording.saved_manifests().len(), 2);
}

#[test]
fn a_full_disk_with_nothing_to_remove_keeps_its_room_aside() {
    // Goal: room set aside waits for a removal, since a save would take it and leave none.
    // Method: as above with a clock that has expired nothing.
    let volume = Volume::new("nothing-expired");
    let (recording, store, now) = saved_on_placed_clock(&volume, 0);
    let filler = volume.fill();
    assert!(volume_refuses_a_small_file(&filler));

    let removed = store.retire_expired(&now, &[], &super::super::persist_budget(0).unwrap());

    assert_eq!(removed.unwrap(), 0);
    assert_eq!(reserve_bytes(&recording), Some(1 << 20));
}

#[test]
fn room_given_up_is_set_aside_again_once_there_is_some() {
    // Goal: one full disk does not use up the way out of the next.
    // Method: as in the first test, then take the pass after it, with the filler gone.
    let volume = Volume::new("set-aside-again");
    let (recording, store, now) = saved_on_placed_clock(&volume, 60_000_000_003);
    let budget = super::super::persist_budget(0).unwrap();
    let filler = volume.fill();

    store.retire_expired(&now, &[], &budget).unwrap();
    let reserve_after_removal = reserve_bytes(&recording);
    std::fs::remove_dir_all(&filler).unwrap();
    store.retire_expired(&now, &[], &budget).unwrap();

    assert_eq!(reserve_after_removal, None);
    assert_eq!(reserve_bytes(&recording), Some(1 << 20));
}

#[test]
fn a_full_disk_with_no_room_aside_says_the_disk_is_full() {
    // Goal: a pass with no room to give up reports the cause, and what no room can be found for
    // waits for the next pass.
    // Method: as in the first test, with the room aside taken away before the volume fills.
    let volume = Volume::new("no-room-aside");
    let (recording, store, now) = saved_on_placed_clock(&volume, 60_000_000_003);
    std::fs::remove_file(recording.root.join("retention/reserve")).unwrap();
    let filler = volume.fill();
    assert!(volume_refuses_a_small_file(&filler));

    let refused = store.retire_expired(&now, &[], &super::super::persist_budget(0).unwrap());

    assert_eq!(refused.unwrap_err().kind(), ErrorKind::StorageFull);
    assert_eq!(recording.saved_manifests().len(), 3);
}

#[test]
fn a_worker_restarted_on_a_full_disk_makes_room_and_saves_what_the_disk_refused() {
    // Goal: the save a full disk refused goes through at the next start with nobody freeing
    // anything, since a removal pass comes before the save resumes.
    // Method: save minutes on a clock that has expired the first, let a full volume refuse the next
    // save, and run the whole worker over the finished ring with the volume still full.
    let volume = Volume::new("restart-full");
    let (mut recording, store, _now) = published_on_placed_clock(&volume, 60_000_000_003);
    let mut state = RunState::default();
    recording.persist_live(&store, &mut state);
    recording.persist_live(&store, &mut state);
    assert_eq!(recording.saved_manifests().len(), 2);
    recording.publish_in_minute(3);
    recording.writer.finish();
    let registry = Registry {
        runs: vec![recording.registered()],
        evictions: 0,
    };
    registry.write(&recording.root).unwrap();
    let filler = volume.fill();
    assert!(volume_refuses_a_small_file(&filler));
    let refused = recording.persist_live_result(&store, &mut state);
    drop(store);

    let outcome = run_paced(
        &recording.root,
        crate::resources::WORKING_BYTES_MAX,
        &SealedApps::default(),
        &mut |_pause| panic!("the ring is finished"),
        None,
    );

    assert!(matches!(
        refused,
        Err(crate::Error::Io(error)) if error.kind() == ErrorKind::StorageFull
    ));
    assert!(matches!(outcome, Ok(Outcome::Complete)));
    let newest: Vec<_> = recording
        .saved_manifests()
        .iter()
        .map(|manifest| manifest.continuous_ns.unwrap().max)
        .collect();
    assert_eq!(newest, [60_000_000_003, 120_000_000_004, 180_000_000_005]);
}
