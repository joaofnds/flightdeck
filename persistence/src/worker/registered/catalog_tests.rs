use super::*;
use crate::canonical::catalog::{Catalog, KnownReading, PAGE_READINGS_MAX};
use crate::canonical::{CanonicalKey, Namespace, ReadWork};

/// Two minutes saved and a third still open: readings at wall 1 and 2 in the first window and at
/// the first second of the next minute plus three in the second.
fn catalog_tests_two_saved_minutes() -> Recording {
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let store = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();
    recording.persist(&store, &mut state);
    recording.persist(&store, &mut state);
    recording
}

fn catalog_tests_budget() -> Arc<ByteBudget> {
    Arc::new(ByteBudget::new(256 * 1024 * 1024))
}

fn catalog_tests_reading(recording: &Recording, wall_ns: u64, publication: u64) -> KnownReading {
    KnownReading {
        wall_ns,
        run: recording.run,
        publication,
    }
}

#[test]
fn a_recording_with_nothing_saved_has_no_readings() {
    // Goal: an empty recording answers as empty, which is the end of the question and not an
    // absence of the catalog (P02.3).
    // Method: open a catalog over a recording no minute reached, and read both ends and a page.
    let recording = Recording::new();
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();
    let mut work = ReadWork::default();

    let newest = catalog.newest_known(&mut work).unwrap();
    let oldest = catalog.oldest_known(&mut work).unwrap();
    let page = catalog.known_page(0, u64::MAX, None, &mut work).unwrap();

    assert_eq!(newest, None);
    assert_eq!(oldest, None);
    assert!(page.readings.is_empty());
    assert_eq!(page.next, None);
}

#[test]
fn the_ends_of_a_recording_name_their_readings_exactly() {
    // Goal: the oldest and newest saved readings come with their full identity (P02.1).
    // Method: save two minutes and read both ends.
    let recording = catalog_tests_two_saved_minutes();
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();
    let mut work = ReadWork::default();

    let oldest = catalog.oldest_known(&mut work).unwrap();
    let newest = catalog.newest_known(&mut work).unwrap();

    assert_eq!(oldest, Some(catalog_tests_reading(&recording, 1, 1)));
    assert_eq!(
        newest,
        Some(catalog_tests_reading(&recording, 60_000_000_003, 3))
    );
}

#[test]
fn a_page_holds_only_the_readings_of_its_range() {
    // Goal: a known range returns the readings whose clock lies in it, its end excluded (P02.5).
    // Method: save two minutes and ask for a range that starts on a reading and ends on another.
    let recording = catalog_tests_two_saved_minutes();
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();
    let mut work = ReadWork::default();

    let page = catalog
        .known_page(2, 60_000_000_003, None, &mut work)
        .unwrap();

    assert_eq!(page.readings, [catalog_tests_reading(&recording, 2, 2)]);
    assert_eq!(page.next, None);
}

#[test]
fn a_full_page_continues_after_its_last_reading() {
    // Goal: readings past one page are reached through its continuation and none is lost or
    // repeated, including readings that share a clock across runs (P02.1, P02.5).
    // Method: make a known range of two pages of hand-made links in the namespace, half of them
    // two runs on one second, and read it page by page.
    let recording = Recording::new();
    let other = RunIdentity {
        run: Identity([100, 101]),
        ..recording.run
    };
    let readings_total = PAGE_READINGS_MAX + 3;
    let mut expected = Vec::new();
    for index in 0..u64::try_from(readings_total / 2).unwrap() {
        for run in [recording.run, other] {
            expected.push(KnownReading {
                wall_ns: 5_000_000_000 + index,
                run,
                publication: index + 1,
            });
        }
    }
    catalog_tests_link_known(&recording, &expected);
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();
    let mut work = ReadWork::default();

    let first = catalog.known_page(0, u64::MAX, None, &mut work).unwrap();
    let last = first.next.unwrap();
    let second = catalog
        .known_page(0, u64::MAX, Some(last), &mut work)
        .unwrap();

    let read: Vec<_> = first.readings.iter().chain(&second.readings).collect();
    assert_eq!(first.readings.len(), PAGE_READINGS_MAX);
    assert_eq!(last, first.readings[PAGE_READINGS_MAX - 1]);
    assert_eq!(second.next, None);
    assert_eq!(read, expected.iter().collect::<Vec<_>>());
}

/// Links each reading into the known namespace in the writer's grammar.
fn catalog_tests_link_known(recording: &Recording, readings: &[KnownReading]) {
    let keys: Vec<CanonicalKey> = readings
        .iter()
        .map(|reading| reading.key().unwrap())
        .collect();
    catalog_tests_link(recording, Namespace::Known, &keys);
}

/// Links sorted `keys` into a namespace as nodes of the writer's grammar: a leaf of at most 128
/// links, or a branch of at most 16 children that differ in the nibble after their common prefix.
fn catalog_tests_link(recording: &Recording, namespace: Namespace, keys: &[CanonicalKey]) {
    let target = recording.root.join("catalog-test-link-target");
    std::fs::write(&target, []).unwrap();
    let names: Vec<String> = keys.iter().map(CanonicalKey::file_name).collect();
    let mut branches = vec![(recording.root.join(namespace.name()), 0, &names[..])];
    while let Some((directory, nibbles, names)) = branches.pop() {
        for group in
            names.chunk_by(|left, right| left.as_bytes()[nibbles] == right.as_bytes()[nibbles])
        {
            let first = &group[0];
            let last = &group[group.len() - 1];
            let shared = first
                .bytes()
                .zip(last.bytes())
                .take_while(|(left, right)| left == right && *left != b'.')
                .count();
            let child = directory.join(&first[nibbles..shared]);
            private::create_private_dir(&child).unwrap();
            if group.len() <= 128 {
                for name in group {
                    std::fs::hard_link(&target, child.join(name)).unwrap();
                }
            } else {
                branches.push((child, shared, group));
            }
        }
    }
    std::fs::remove_file(target).unwrap();
}

#[test]
fn a_window_is_found_by_the_publication_it_holds() {
    // Goal: exact identity reaches its window with no clock and no window number (P02.2).
    // Method: save two minutes, then ask for a publication in each and for one past them both.
    let recording = catalog_tests_two_saved_minutes();
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();
    let mut work = ReadWork::default();

    let first = catalog
        .window_holding(&recording.run, 2, &mut work)
        .unwrap();
    let second = catalog
        .window_holding(&recording.run, 3, &mut work)
        .unwrap();
    let past = catalog
        .window_holding(&recording.run, 4, &mut work)
        .unwrap();
    let before = catalog
        .window_holding(&recording.run, 0, &mut work)
        .unwrap();

    assert_eq!(first.unwrap().identity.window, 0);
    assert_eq!(second.unwrap().identity.window, 1);
    assert!(past.is_none());
    assert!(before.is_none());
}

#[test]
fn a_lookup_leaves_every_other_window_unread() {
    // Goal: finding a window admits that window alone, so an unavailable old file elsewhere
    // cannot stop a reader of a newer one (P02.3).
    // Method: save two minutes, damage the first window's proof, and find the second.
    let recording = catalog_tests_two_saved_minutes();
    let first = catalog_tests_first_window_proof(&recording);
    std::fs::write(&first, b"damage").unwrap();
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();
    let mut work = ReadWork::default();

    let second = catalog
        .window_holding(&recording.run, 3, &mut work)
        .unwrap();
    let damaged = catalog.window_holding(&recording.run, 1, &mut work);

    assert_eq!(second.unwrap().identity.window, 1);
    assert!(damaged.is_err());
}

#[test]
fn a_clock_range_finds_each_window_it_touches_once() {
    // Goal: a range of the clock reaches every window holding a reading in it, and none other,
    // however many of a window's readings fall in it (P02.1, P02.2).
    // Method: save two minutes and ask for ranges over both, over the second alone and past them.
    let recording = catalog_tests_two_saved_minutes();
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();
    let mut work = ReadWork::default();

    let both = catalog.windows_in(0, 60_000_000_004, &mut work).unwrap();
    let second = catalog.windows_in(3, 60_000_000_004, &mut work).unwrap();
    let past = catalog
        .windows_in(60_000_000_004, u64::MAX, &mut work)
        .unwrap();

    let numbers = |windows: &[crate::Manifest]| -> Vec<u64> {
        windows
            .iter()
            .map(|window| window.identity.window)
            .collect()
    };
    assert_eq!(numbers(&both), [0, 1]);
    assert_eq!(numbers(&second), [1]);
    assert!(past.is_empty());
}

#[test]
fn a_clock_range_leaves_the_windows_outside_it_unread() {
    // Goal: damage outside a range stops no reader of the range, and damage inside it is explicit
    // rather than a window quietly missing (P02.3).
    // Method: save two minutes, damage the first window's proof, and ask for a range over the
    // second and one over both.
    let recording = catalog_tests_two_saved_minutes();
    std::fs::write(catalog_tests_first_window_proof(&recording), b"damage").unwrap();
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();
    let mut work = ReadWork::default();

    let second = catalog.windows_in(3, 60_000_000_004, &mut work).unwrap();
    let both = catalog.windows_in(0, 60_000_000_004, &mut work);

    assert_eq!(second.len(), 1);
    assert_eq!(second[0].identity.window, 1);
    assert!(both.is_err());
}

/// The proof of the recording's first window, found by its publication's gate.
fn catalog_tests_first_window_proof(recording: &Recording) -> PathBuf {
    let name = CanonicalKey::source(Namespace::Ordinary, &recording.run, 1)
        .unwrap()
        .file_name();
    let mut pending = vec![recording.root.join(Namespace::Ordinary.name())];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().unwrap() == name.as_str() {
                return path;
            }
        }
    }
    panic!("no gate for the first window");
}

#[test]
fn a_window_a_catalog_finds_reads_like_any_committed_window() {
    // Goal: the reads a commitment guards take a registered window as they take any other, so a
    // reader needs one path for its files and its events (P02.5).
    // Method: save two minutes, find the first through the catalog, read its commitment by every
    // route, and count its events through the engine.
    let recording = catalog_tests_two_saved_minutes();
    let budget = catalog_tests_budget();
    let catalog = Catalog::open(&recording.root, &budget).unwrap();
    let manifest = catalog
        .window_holding(&recording.run, 1, &mut ReadWork::default())
        .unwrap()
        .unwrap();
    let path = crate::commitment_path(&recording.root, &manifest);

    let read = crate::read_committed(&recording.root, &path).unwrap();
    let reread = crate::reread_committed(&recording.root, &path).unwrap();
    let listed = crate::list_committed(&recording.root, &path, recording.run.recording).unwrap();
    let engine = crate::engine::Engine::new();
    let events = engine
        .event_source(&recording.root, &manifest, &budget)
        .unwrap();
    let count = engine
        .query(&format!("SELECT count(*) FROM {events}"))
        .unwrap();

    assert_eq!(read, manifest);
    assert_eq!(reread, manifest);
    assert_eq!(listed, manifest);
    assert_eq!(
        crate::engine::Row::new(&count, 0)
            .unwrap()
            .get::<u64>(0)
            .unwrap(),
        3
    );
}

#[test]
fn a_commitment_refuses_a_stray_file_in_its_window() {
    // Goal: the first read of a registered window holds its files to its manifest, as a legacy
    // window's does (P02.5).
    // Method: save two minutes, add a file beside the first window's tables, and read it.
    let recording = catalog_tests_two_saved_minutes();
    let budget = catalog_tests_budget();
    let catalog = Catalog::open(&recording.root, &budget).unwrap();
    let manifest = catalog
        .window_holding(&recording.run, 1, &mut ReadWork::default())
        .unwrap()
        .unwrap();
    let path = crate::commitment_path(&recording.root, &manifest);
    std::fs::write(path.with_file_name("stray.parquet"), []).unwrap();

    let read = crate::read_committed(&recording.root, &path);

    assert_eq!(read.unwrap_err().kind(), ErrorKind::InvalidData);
}

/// What `read` found, none where a publication told it to wait. Anything else is a failure, since a
/// reader that meets a publication may only wait or read what is saved.
fn catalog_tests_found_or_waited<T>(read: crate::Result<T>) -> Option<T> {
    match read {
        Err(crate::Error::Io(error)) if error.kind() == ErrorKind::WouldBlock => None,
        result => Some(result.unwrap()),
    }
}

#[test]
fn a_read_during_a_publication_waits_or_finds_each_window_whole() {
    // Goal: a reader that meets a publication at any of its steps learns nothing false, and finds
    // the minute once the publication ends (P02.3, P02.5).
    // Method: save a minute, stop the next one's publication before its first step, and read the
    // windows of every clock between its steps.
    let recording = catalog_tests_two_saved_minutes_and_one_publishing();
    let budget = catalog_tests_budget();
    let store = Store::open(&recording.root).unwrap();
    let catalog = Catalog::open(&recording.root, &budget).unwrap();
    let mut publishing = store.resume_publication(&budget).unwrap();
    let mut seen = Vec::new();

    for _step in 0..1 << 10 {
        let read = catalog.windows_in(0, u64::MAX, &mut ReadWork::default());
        seen.push(catalog_tests_found_or_waited(read).map(|windows| {
            windows
                .iter()
                .map(|window| window.identity.window)
                .collect::<Vec<u64>>()
        }));
        if publishing.step().unwrap() == PublicationStep::Idle {
            break;
        }
    }
    let after = catalog
        .windows_in(0, u64::MAX, &mut ReadWork::default())
        .unwrap();

    let found: Vec<&Vec<u64>> = seen.iter().flatten().collect();
    let first_whole = found.iter().position(|windows| windows.len() == 2);
    let before_it = &found[..first_whole.unwrap_or(found.len())];
    assert!(before_it.iter().all(|windows| **windows == [0]), "{seen:?}");
    assert!(
        found[before_it.len()..]
            .iter()
            .all(|windows| **windows == [0, 1]),
        "{seen:?}"
    );
    assert_eq!(after.len(), 2);
}

/// One minute saved and the publication of the next stopped before its first step.
fn catalog_tests_two_saved_minutes_and_one_publishing() -> Recording {
    let mut recording = Recording::new();
    publish_three_minutes(&mut recording);
    let store = Store::open(&recording.root).unwrap();
    recording.persist(&store, &mut RunState::default());
    let second = InterruptedWindow {
        number: 1,
        publication: 3,
        publications_lost: 0,
        unknown: false,
    };
    recording.interrupted_seal_of(&store, &second, Interruption::PublicationSteps(0));
    recording
}
