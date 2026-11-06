use super::*;
use crate::canonical::catalog::{
    Catalog, EventObject, EventObjectKind, KnownReading, PAGE_READINGS_MAX, UnknownReading,
};
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

#[test]
fn a_query_of_a_second_refuses_a_window_holding_a_stray_file() {
    // Goal: a query reads a window's files only after checking them, as every reader does.
    // Method: query a saved second, put a private file in its window's directory and query the
    // same second again, so the file is all that changed.
    let recording = catalog_tests_two_saved_minutes();
    let readable = crate::query::processes_at_second(&recording.root, 0);
    let directory = recording
        .root
        .join(&recording.saved_manifests()[0].data_directory);
    private::atomic_write(&directory, "stray", b"stray").unwrap();

    let refused = crate::query::processes_at_second(&recording.root, 0);

    assert!(readable.is_ok());
    assert!(matches!(
        refused,
        Err(crate::Error::Io(error)) if error.kind() == ErrorKind::InvalidData
    ));
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
    let engine = crate::engine::Engine::new();
    let events = engine
        .event_source(&recording.root, &manifest, &budget)
        .unwrap();
    let count = engine
        .query(&format!("SELECT count(*) FROM {events}"))
        .unwrap();

    assert_eq!(read, manifest);
    assert_eq!(reread, manifest);
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

/// Every saved object of events, reading `objects_max` at a time.
fn catalog_tests_event_objects(catalog: &Catalog, objects_max: usize) -> Vec<EventObject> {
    let mut objects = Vec::new();
    let mut after = None;
    for _page in 0..16 {
        let page = catalog
            .event_objects_page(after.as_ref(), objects_max, &mut ReadWork::default())
            .unwrap();
        objects.extend(page.objects);
        if page.next.is_none() {
            return objects;
        }
        after = page.next;
    }
    panic!("a page of event objects that never ends");
}

/// A finished run that saved only events its writer never published.
fn catalog_tests_terminal_only(recording: &mut Recording) {
    recording.publish_events_in_minute(0, 2);
    recording.withhold_publications_past(0);
    let store = Store::open(&recording.root).unwrap();
    assert!(matches!(
        recording.persist(&store, &mut RunState::default()),
        Progress::Complete
    ));
}

#[test]
fn a_recording_with_no_minute_lists_the_events_of_its_terminal() {
    // Goal: events a run saved apart from any minute are found with no window to lead to them, and
    // what they cover comes with them (P02.1).
    // Method: save only a terminal, and read the event objects and the known readings.
    let mut recording = Recording::new();
    catalog_tests_terminal_only(&mut recording);
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();

    let objects = catalog_tests_event_objects(&catalog, 8);
    let newest = catalog.newest_known(&mut ReadWork::default()).unwrap();

    assert_eq!(objects.len(), 1);
    assert_eq!(objects[0].kind, EventObjectKind::Terminal);
    assert_eq!(objects[0].recovery.run, recording.run);
    assert_eq!(objects[0].recovery.rows, 2);
    assert_eq!(newest, None);
}

#[test]
fn a_run_the_registry_forgot_keeps_its_event_objects() {
    // Goal: an evicted run's saved events are reached through the recording and never through the
    // registry that named its source (P02.3).
    // Method: save only a terminal, empty the registry as an eviction does, and list the objects.
    let mut recording = Recording::new();
    catalog_tests_terminal_only(&mut recording);
    Registry {
        runs: Vec::new(),
        evictions: 1,
    }
    .write(&recording.root)
    .unwrap();
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();

    let objects = catalog_tests_event_objects(&catalog, 8);

    assert_eq!(objects.len(), 1);
    assert_eq!(objects[0].recovery.run, recording.run);
}

#[test]
fn event_objects_continue_page_by_page_with_none_lost_or_repeated() {
    // Goal: a page is a bounded part of the objects, and its continuation reaches the rest (P02.2).
    // Method: save a terminal for each of two runs, and read them one to a page and then together.
    let mut recording = Recording::new();
    catalog_tests_terminal_only(&mut recording);
    let mut other = recording.another_run();
    other.publish_in_minute(0);
    other.writer.finish();
    let store = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();
    assert!(matches!(
        other.persist(&store, &mut state),
        Progress::Sealed
    ));
    assert!(matches!(
        other.persist(&store, &mut state),
        Progress::Complete
    ));
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();

    let paged = catalog_tests_event_objects(&catalog, 1);
    let whole = catalog_tests_event_objects(&catalog, 8);

    assert_eq!(paged.len(), 2);
    assert_ne!(paged[0].recovery.run, paged[1].recovery.run);
    assert_eq!(paged, whole);
}

#[test]
fn a_damaged_event_object_is_explicit_rather_than_a_shorter_list() {
    // Goal: a partial inventory is never presented as the whole of it (P02.3).
    // Method: save a terminal, damage its proof, and read the objects.
    let mut recording = Recording::new();
    catalog_tests_terminal_only(&mut recording);
    let mut objects = std::fs::read_dir(recording.root.join("recovery-data")).unwrap();
    let object = objects.next().unwrap().unwrap().path();
    std::fs::write(object.join("ready/metadata"), b"damage").unwrap();
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();

    let page = catalog.event_objects_page(None, 8, &mut ReadWork::default());

    assert!(page.is_err());
}

#[test]
fn a_proof_under_another_objects_name_is_refused() {
    // Goal: a gate moved onto another key never lists its proof as that key's object (P02.3).
    // Method: save a terminal, rename its gate so the name holds another run, and read the objects.
    let mut recording = Recording::new();
    catalog_tests_terminal_only(&mut recording);
    let mut leaves = std::fs::read_dir(recording.root.join("terminal")).unwrap();
    let leaf = leaves.next().unwrap().unwrap().path();
    let mut gates = std::fs::read_dir(&leaf).unwrap();
    let gate = gates.next().unwrap().unwrap().file_name();
    let gate = gate.to_str().unwrap();
    let digit_last = gate.len() - ".seal".len() - 1;
    let digit_other = if gate.as_bytes()[digit_last] == b'0' {
        "1"
    } else {
        "0"
    };
    let moved = format!("{}{digit_other}.seal", &gate[..digit_last]);
    std::fs::rename(leaf.join(gate), leaf.join(moved)).unwrap();
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();

    let page = catalog.event_objects_page(None, 8, &mut ReadWork::default());

    assert!(page.is_err());
}

/// Publishes one tick of the given minute with no clocks, as a source without clocks leaves it.
fn catalog_tests_publish_without_clock(recording: &mut Recording, minute: u64) {
    let memory = collector::machine::Memory::default();
    recording.tick += 1;
    recording.writer.publish(&collector::ring::Readings {
        run_tick: minute * 60,
        cpus: &[],
        memory: &memory,
        interfaces: &[],
        ioreport: None,
        smc: None,
        processes: &[],
        processes_refused: 0,
        processes_gone: 0,
        coalitions: &[],
        events: &[],
        dropped: collector::ring::Dropped::default(),
    });
}

/// The window numbers of `manifests`, in their order.
fn catalog_tests_window_numbers(manifests: &[crate::Manifest]) -> Vec<u64> {
    manifests
        .iter()
        .map(|manifest| manifest.identity.window)
        .collect()
}

/// The kind of I/O error `read` ended in.
fn catalog_tests_error_kind<T>(read: crate::Result<T>) -> ErrorKind {
    match read {
        Err(crate::Error::Io(error)) => error.kind(),
        Err(error) => panic!("not an I/O error: {error}"),
        Ok(_) => panic!("the read succeeded"),
    }
}

/// The unknown readings of publications `publications` of `recording`'s run.
fn catalog_tests_unknown_readings(
    recording: &Recording,
    publications: std::ops::Range<u64>,
) -> Vec<UnknownReading> {
    publications
        .map(|publication| UnknownReading {
            run: recording.run,
            publication,
        })
        .collect()
}

/// Links each reading into the unknown namespace in the writer's grammar.
fn catalog_tests_link_unknown(recording: &Recording, readings: &[UnknownReading]) {
    let keys: Vec<CanonicalKey> = readings
        .iter()
        .map(|reading| reading.key().unwrap())
        .collect();
    catalog_tests_link(recording, Namespace::Unknown, &keys);
}

#[test]
fn a_page_of_unknown_readings_continues_after_its_last_reading() {
    // Goal: readings with no clock past one page are reached through its continuation and none is
    // lost or repeated (P02.3).
    // Method: link one reading more than a page holds and read it page by page.
    let recording = Recording::new();
    let readings_total = u64::try_from(PAGE_READINGS_MAX).unwrap() + 1;
    let expected = catalog_tests_unknown_readings(&recording, 1..readings_total + 1);
    catalog_tests_link_unknown(&recording, &expected);
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();
    let mut work = ReadWork::default();

    let first = catalog.unknown_page(None, &mut work).unwrap();
    let second = catalog.unknown_page(first.next, &mut work).unwrap();

    assert_eq!(first.readings, expected[..PAGE_READINGS_MAX]);
    assert_eq!(first.next, Some(expected[PAGE_READINGS_MAX - 1]));
    assert_eq!(second.readings, expected[PAGE_READINGS_MAX..]);
    assert_eq!(second.next, None);
}

#[test]
fn a_page_ending_at_the_last_unknown_reading_names_no_next_page() {
    // Goal: a page that happens to hold the last reading ends the listing, so a reader never asks
    // for a page that cannot exist.
    // Method: link exactly a page of readings and read it.
    let recording = Recording::new();
    let readings_total = u64::try_from(PAGE_READINGS_MAX).unwrap();
    let expected = catalog_tests_unknown_readings(&recording, 1..readings_total + 1);
    catalog_tests_link_unknown(&recording, &expected);
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();

    let page = catalog
        .unknown_page(None, &mut ReadWork::default())
        .unwrap();

    assert_eq!(page.readings, expected);
    assert_eq!(page.next, None);
}

#[test]
fn an_unknown_name_with_bytes_past_its_publication_is_refused() {
    // Goal: a name no writer makes is damage rather than a reading (P02.3).
    // Method: link a reading's name with its last padding byte set and read the page.
    let recording = Recording::new();
    let reading = UnknownReading {
        run: recording.run,
        publication: 1,
    };
    let mut bytes = [0; 48];
    bytes[..48].copy_from_slice(&reading.key().unwrap().bytes[..48]);
    bytes[47] = 1;
    let key = CanonicalKey::from_bytes(Namespace::Unknown, &bytes).unwrap();
    catalog_tests_link(&recording, Namespace::Unknown, &[key]);
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();

    let page = catalog.unknown_page(None, &mut ReadWork::default());

    assert_eq!(
        catalog_tests_error_kind(page.map_err(Into::into)),
        ErrorKind::InvalidData
    );
}

#[test]
fn a_window_with_no_clock_is_listed_once_and_a_clock_range_never_lists_it() {
    // Goal: a window with no clock is reachable with no clock range, once however many readings it
    // holds, and the windows a clock range reaches stay out of the list (P02.3).
    // Method: save a window of two readings with no clock and a window with clocks, and read both
    // lists.
    let mut recording = Recording::new();
    catalog_tests_publish_without_clock(&mut recording, 0);
    catalog_tests_publish_without_clock(&mut recording, 0);
    recording.publish_in_minute(1);
    recording.publish_in_minute(2);
    let store = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();
    recording.persist(&store, &mut state);
    recording.persist(&store, &mut state);
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();
    let mut work = ReadWork::default();

    let without_clock = catalog.windows_without_clock(&mut work).unwrap();
    let in_range = catalog.windows_in(0, u64::MAX, &mut work).unwrap();

    assert_eq!(catalog_tests_window_numbers(&without_clock), [0]);
    assert_eq!(catalog_tests_window_numbers(&in_range), [1]);
}

#[test]
fn a_window_with_one_clock_among_its_readings_is_left_to_the_clock_range() {
    // Goal: a window one of whose readings has a clock is the clock range's to find, so listing it
    // here as well would show it twice (P02.3).
    // Method: save a window of a reading with a clock and one without, and read both lists.
    let mut recording = Recording::new();
    recording.publish_in_minute(0);
    catalog_tests_publish_without_clock(&mut recording, 0);
    recording.publish_in_minute(1);
    let store = Store::open(&recording.root).unwrap();
    recording.persist(&store, &mut RunState::default());
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();
    let mut work = ReadWork::default();

    let unknown = catalog.unknown_page(None, &mut work).unwrap();
    let without_clock = catalog.windows_without_clock(&mut work).unwrap();
    let in_range = catalog.windows_in(0, u64::MAX, &mut work).unwrap();

    assert_eq!(
        unknown.readings,
        catalog_tests_unknown_readings(&recording, 2..3)
    );
    assert!(without_clock.is_empty());
    assert_eq!(catalog_tests_window_numbers(&in_range), [0]);
}

#[test]
fn windows_with_no_clock_are_reached_past_one_page_of_their_readings() {
    // Goal: the list holds a window whose readings all lie past the first page (P02.3).
    // Method: save four minutes of 60 readings with no clock each, which leaves the last window
    // wholly on the second page of 128, and read the list.
    let mut recording = Recording::create(
        false,
        Shape {
            slot_count: 256,
            ..shape()
        },
    );
    for minute in 0..4 {
        for _reading in 0..60 {
            catalog_tests_publish_without_clock(&mut recording, minute);
        }
    }
    recording.publish_in_minute(4);
    let store = Store::open(&recording.root).unwrap();
    let mut state = RunState::default();
    for _minute in 0..4 {
        recording.persist(&store, &mut state);
    }
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();

    let without_clock = catalog
        .windows_without_clock(&mut ReadWork::default())
        .unwrap();

    assert_eq!(catalog_tests_window_numbers(&without_clock), [0, 1, 2, 3]);
}

#[test]
fn a_reading_with_no_clock_and_no_window_is_damage() {
    // Goal: a reading's window outlives its links, so one missing is explicit rather than a
    // shorter list (P02.3).
    // Method: link a reading no window holds and read the list.
    let recording = Recording::new();
    let reading = catalog_tests_unknown_readings(&recording, 1..2);
    catalog_tests_link_unknown(&recording, &reading);
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();

    let without_clock = catalog.windows_without_clock(&mut ReadWork::default());

    assert_eq!(
        catalog_tests_error_kind(without_clock),
        ErrorKind::InvalidData
    );
}

#[test]
fn a_reading_of_another_run_is_never_taken_for_the_window_before_it() {
    // Goal: a window found for one run answers only for that run, although another run can use
    // the same publication numbers (P02.3).
    // Method: save a window with no clock, link a reading of another run that no window holds under
    // the same publication, and read the list.
    let mut recording = Recording::new();
    catalog_tests_publish_without_clock(&mut recording, 0);
    recording.publish_in_minute(1);
    let store = Store::open(&recording.root).unwrap();
    recording.persist(&store, &mut RunState::default());
    let other = UnknownReading {
        run: RunIdentity {
            run: Identity([u64::MAX, 101]),
            ..recording.run
        },
        publication: 1,
    };
    catalog_tests_link_unknown(&recording, &[other]);
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();

    let without_clock = catalog.windows_without_clock(&mut ReadWork::default());

    assert_eq!(
        catalog_tests_error_kind(without_clock),
        ErrorKind::InvalidData
    );
}

#[test]
fn a_short_proof_behind_a_window_gate_is_damage() {
    // Goal: a gate names a proof only once it is whole, so a short one reads as damage and never
    // as the end of a file still being written (P02.3).
    // Method: save a window, cut its proof short through its gate, and find the window.
    let recording = catalog_tests_two_saved_minutes();
    let proof = std::fs::File::options()
        .write(true)
        .open(catalog_tests_first_window_proof(&recording))
        .unwrap();
    proof.set_len(8).unwrap();
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();

    let window = catalog.window_holding(&recording.run, 1, &mut ReadWork::default());

    assert_eq!(
        catalog_tests_error_kind(window.map_err(Into::into)),
        ErrorKind::InvalidData
    );
}

#[test]
fn a_short_proof_behind_an_event_object_gate_is_damage() {
    // Goal: as for a window, a short proof of an event object is damage and not a write going on
    // (P02.3).
    // Method: save a terminal, cut its proof short, and read the objects.
    let mut recording = Recording::new();
    catalog_tests_terminal_only(&mut recording);
    let mut objects = std::fs::read_dir(recording.root.join("recovery-data")).unwrap();
    let object = objects.next().unwrap().unwrap().path();
    let proof = std::fs::File::options()
        .write(true)
        .open(object.join("ready/metadata"))
        .unwrap();
    proof.set_len(8).unwrap();
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();

    let page = catalog.event_objects_page(None, 8, &mut ReadWork::default());

    assert_eq!(
        catalog_tests_error_kind(page.map_err(Into::into)),
        ErrorKind::InvalidData
    );
}

#[test]
fn a_window_with_no_clock_and_lost_readings_is_still_listed() {
    // Goal: a reading the source overwrote is no reading with a clock, so a lost reading leaves
    // the window on the list (P02.3).
    // Method: publish more readings with no clock than the source holds, save the window, and read
    // the list.
    let mut recording = Recording::new();
    for _reading in 0..12 {
        catalog_tests_publish_without_clock(&mut recording, 0);
    }
    recording.publish_in_minute(1);
    let store = Store::open(&recording.root).unwrap();
    recording.persist(&store, &mut RunState::default());
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();

    let without_clock = catalog
        .windows_without_clock(&mut ReadWork::default())
        .unwrap();

    assert_eq!(catalog_tests_window_numbers(&without_clock), [0]);
    assert!(without_clock[0].losses.publications > 0);
}

#[test]
fn a_window_with_no_clock_and_a_gap_among_its_readings_is_still_listed() {
    // Goal: the readings a window lost in its middle leave it on the list, since its publication
    // range is wider than what it saved (P02.3).
    // Method: let the source overwrite the second reading of a window as it is being read, save the
    // window, and read the list.
    let mut recording = Recording::new();
    catalog_tests_publish_without_clock(&mut recording, 0);
    catalog_tests_publish_without_clock(&mut recording, 0);
    let registered = recording.registered();
    let store = Store::open(&recording.root).unwrap();
    super::super::seal_first_window_overrun(
        &store,
        &registered,
        &SealedApps::default(),
        &mut || {
            for _reading in 0..7 {
                catalog_tests_publish_without_clock(&mut recording, 0);
            }
            recording.publish_in_minute(1);
        },
    )
    .unwrap();
    let catalog = Catalog::open(&recording.root, &catalog_tests_budget()).unwrap();

    let without_clock = catalog
        .windows_without_clock(&mut ReadWork::default())
        .unwrap();

    assert_eq!(catalog_tests_window_numbers(&without_clock), [0]);
    assert_eq!(without_clock[0].publication_first, 1);
    assert_eq!(without_clock[0].losses.publications, 1);
}
