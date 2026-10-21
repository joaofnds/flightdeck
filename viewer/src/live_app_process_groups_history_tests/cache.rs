use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use collector::processes::Usage;

use super::{BEFORE_PROJECT, BYTES_MAX, History, Query, Reduction, Target};
use crate::app_process_groups_history::{AppProcessGroupsHistory, ProcessGroup};
use crate::apps::Stamp;
use crate::live::identity_fixture::Fixture;
use crate::live_app_response::{KeyKind, Observation};
use crate::{Cancellation, Session};

use crate::live::app_process_groups_history_tests::{equal, named};

struct Cold {
    fixture: Fixture,
    saved: Session,
    history: History,
    cancellation: Arc<Cancellation>,
}

#[test]
fn the_open_pages_groups_displace_other_owners_under_pressure() {
    // Goal: Apps keeps its process group cache when other pages' saved caches fill the budget.
    // Method: fill all three owners, then answer with room for the others but not the groups.
    let cold = cold();
    let full = call(&cold, 2, BYTES_MAX).unwrap();
    other_owners(&cold);
    let others = without_groups(&cold);
    assert!(cold.history.saved_residents.borrow().bytes() > others);

    let pressed = call(&cold, 2, others).unwrap();

    equal(&full, &pressed);
    assert_eq!(without_groups(&cold), 0);
    let mut resident = cold.history.saved_residents.borrow_mut();
    let groups = resident
        .take_groups()
        .expect("the open page keeps its group cache");
    assert!(groups.points().next().is_some());
    assert!(groups.bytes() <= others);
}

#[test]
fn warm_pressure_rebuilds_old_excluded_points() {
    // Goal: a group cache that outgrows its budget answers exactly as the uncached fallback.
    // Method: cache two saved points, then add a third with room for only the former cache.
    let mut cold = cold();
    let first = call(&cold, 2, BYTES_MAX).unwrap();
    let before = cold.history.saved_residents.borrow().bytes();
    advance(&mut cold);

    let limited = call(&cold, 3, before).unwrap();
    let fallback = call(&cold, 3, 0).unwrap();

    equal(&limited, &fallback);
    assert_eq!(limited.points.len(), 3);
    assert_eq!(limited.points[0].readings[0].measures[5].known, 1000);
    assert_eq!(limited.points[2].readings[0].measures[2].known, 1440);
    assert_eq!(first.points[0].observation, limited.points[0].observation);
    let mut resident = cold.history.saved_residents.borrow_mut();
    assert!(resident.take_groups().is_none());
    assert!(cold.history.bytes + resident.bytes() <= BYTES_MAX);
}

#[test]
fn zero_budget_fallback_matches_every_typed_value_including_other() {
    // Nine raw names exercise both leader ordering and Other in the complete uncached stream.
    let mut cold = cold();
    let full = call(&cold, 2, BYTES_MAX).unwrap();
    let limited = call(&cold, 2, 0).unwrap();

    equal(&full, &limited);
    assert_eq!(limited.named_group_count, 9);
    assert!(matches!(limited.groups[7], ProcessGroup::Other));
    assert_eq!(limited.points[1].readings[7].members, 2);
    assert!(
        cold.history
            .saved_residents
            .borrow_mut()
            .take_groups()
            .is_none()
    );
    cold.history = History::new();
    let fresh_limited = call(&cold, 2, 0).unwrap();
    equal(&full, &fresh_limited);
}

#[test]
fn new_saved_endpoint_and_replacement_session_keep_old_validated_facts() {
    // History survives a new seal and Session replacement; only the newly captured point is added.
    let mut cold = cold();
    let decoded = crate::session::ProcessGroupsReplay::test_frame_decodes();
    call(&cold, 2, BYTES_MAX).unwrap();
    assert_eq!(
        crate::session::ProcessGroupsReplay::test_frame_decodes() - decoded,
        2
    );
    advance(&mut cold);
    let decoded = crate::session::ProcessGroupsReplay::test_frame_decodes();
    let reached = Arc::new(AtomicBool::new(false));
    let checkpoint = Arc::clone(&reached);
    cold.saved
        .after_process_groups_frame
        .replace(Some(Box::new(move || {
            checkpoint.store(true, Ordering::Release);
        })));

    let advanced = call(&cold, 3, BYTES_MAX).unwrap();
    assert_eq!(
        crate::session::ProcessGroupsReplay::test_frame_decodes() - decoded,
        2
    );
    assert!(reached.load(Ordering::Acquire));
    assert_eq!(advanced.points.len(), 3);
    assert_eq!(advanced.points[2].elapsed_ns, Some(1_000_000_000));
    assert_eq!(advanced.points[2].readings[0].measures[2].known, 1440);
    assert_eq!(advanced.points[2].readings[0].measures[3].known, 720);
    let reached = Arc::new(AtomicBool::new(false));
    let checkpoint = Arc::clone(&reached);
    cold.saved
        .after_process_groups_frame
        .replace(Some(Box::new(move || {
            checkpoint.store(true, Ordering::Release);
        })));
    let repeated = call(&cold, 3, BYTES_MAX).unwrap();
    assert_eq!(
        crate::session::ProcessGroupsReplay::test_frame_decodes() - decoded,
        2
    );
    equal(&advanced, &repeated);
    assert!(!reached.load(Ordering::Acquire));
}

#[test]
fn late_predecessor_replaces_a_cached_unavailable_interval() {
    // Capture publication 3 before 2 is sealed, then add its real predecessor in a new window.
    let mut cold = cold();
    cold.history = History::new();
    let manifest = &cold.saved.windows[0];
    let marker = store_marker(&cold.fixture, manifest);
    std::fs::remove_file(marker).unwrap();
    publish(&mut cold.fixture, 3);
    commit(&cold.fixture, 3, 2);
    cold.saved = Session::open(&cold.fixture.root).unwrap();
    let absent = call(&cold, 3, BYTES_MAX).unwrap();
    assert_eq!(absent.points.len(), 1);
    assert_eq!(absent.points[0].elapsed_ns, None);
    assert_eq!(absent.points[0].readings[0].measures[2].measured_members, 0);
    commit(&cold.fixture, 2, 1);
    cold.saved = Session::open(&cold.fixture.root).unwrap();

    let repaired = call(&cold, 3, BYTES_MAX).unwrap();

    assert_eq!(repaired.points.len(), 2);
    assert_eq!(repaired.points[1].elapsed_ns, Some(1_000_000_000));
    assert_eq!(repaired.points[1].readings[0].measures[2].known, 1440);
    assert_eq!(repaired.points[1].readings[0].measures[3].known, 720);
}

#[test]
fn warm_replaced_source_is_refused_and_unrelated_owners_survive() {
    // Identical bytes in a new inode invalidate retained facts before any stale response escapes.
    let cold = cold();
    call(&cold, 2, BYTES_MAX).unwrap();
    other_owners(&cold);
    let others = without_groups(&cold);
    let path = cold
        .fixture
        .root
        .join(&cold.saved.windows[0].data_directory)
        .join("processes.parquet");
    let original = cold.fixture.root.join("old-processes.parquet");
    std::fs::rename(&path, &original).unwrap();
    assert!(std::fs::copy(&original, &path).unwrap() > 0);

    let result = call(&cold, 2, BYTES_MAX);

    assert!(matches!(
        result,
        Err(persistence::Error::Bound(
            "changed committed App history source"
        ))
    ));
    assert_eq!(without_groups(&cold), others);
    assert!(
        cold.history
            .saved_residents
            .borrow_mut()
            .take_groups()
            .is_none()
    );
}

#[test]
fn cancellation_between_warm_observe_and_project_discards_groups_only() {
    // Interrupt the no-decode warm path after choosing leaders, while preserving other owners.
    let cold = cold();
    call(&cold, 2, BYTES_MAX).unwrap();
    other_owners(&cold);
    let others = without_groups(&cold);
    let cancellation = Arc::clone(&cold.cancellation);
    BEFORE_PROJECT.with(|hook| {
        assert!(
            hook.replace(Some(Box::new(move || cancellation.cancel().unwrap())))
                .is_none()
        );
    });

    let result = call(&cold, 2, BYTES_MAX);

    assert!(matches!(result, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
    assert_eq!(without_groups(&cold), others);
    assert!(
        cold.history
            .saved_residents
            .borrow_mut()
            .take_groups()
            .is_none()
    );
}

#[test]
fn cancellation_during_cold_fill_restores_resident_and_attribution_owners() {
    // Cancel after a real first saved frame entered Observe; unrelated caches stay byte-identical.
    let cold = cold();
    other_owners(&cold);
    let others = without_groups(&cold);
    let cancellation = Arc::clone(&cold.cancellation);
    assert!(
        cold.saved
            .after_process_groups_frame
            .replace(Some(Box::new(move || {
                cancellation.cancel().unwrap();
            })))
            .is_none()
    );

    let result = call(&cold, 2, BYTES_MAX);

    assert!(matches!(result, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
    assert_eq!(without_groups(&cold), others);
    assert!(
        cold.history
            .saved_residents
            .borrow_mut()
            .take_groups()
            .is_none()
    );
}

#[test]
fn removed_warm_commitment_is_refused_without_a_partial_cohort() {
    // A retained source pin requires its publication marker on every warm reuse.
    let cold = cold();
    call(&cold, 2, BYTES_MAX).unwrap();
    std::fs::remove_file(store_marker(&cold.fixture, &cold.saved.windows[0])).unwrap();

    let result = call(&cold, 2, BYTES_MAX);

    assert!(result.is_err());
    assert!(
        cold.history
            .saved_residents
            .borrow_mut()
            .take_groups()
            .is_none()
    );
}

#[test]
fn replacement_between_warm_observe_and_project_is_refused_after_projection() {
    // The second pin check detects a source replaced after the initial warm validation.
    let cold = cold();
    call(&cold, 2, BYTES_MAX).unwrap();
    let path = cold
        .fixture
        .root
        .join(&cold.saved.windows[0].data_directory)
        .join("processes.parquet");
    let original = cold.fixture.root.join("old-processes.parquet");
    BEFORE_PROJECT.with(|hook| {
        assert!(
            hook.replace(Some(Box::new(move || {
                std::fs::rename(&path, &original).unwrap();
                assert!(std::fs::copy(&original, &path).unwrap() > 0);
            })))
            .is_none()
        );
    });

    let result = call(&cold, 2, BYTES_MAX);

    assert!(matches!(
        result,
        Err(persistence::Error::Bound(
            "changed committed App history source"
        ))
    ));
    assert!(
        cold.history
            .saved_residents
            .borrow_mut()
            .take_groups()
            .is_none()
    );
}

#[test]
fn refuses_the_parts_of_a_window_whose_groups_were_replayed() {
    // Goal: a window gives an App's groups from its parts or from a replay of its minute, never
    // both, so parts that become readable while the groups are read are refused.
    // Method: seal two observations with their Apps' parts of a version the session does not
    // read, make those parts readable before the groups are projected, and ask for the groups.
    let cold = kept_cold();
    let path = cold
        .fixture
        .root
        .join(&cold.saved.windows[0].data_directory)
        .join("app_parts.parquet");
    let readable = std::fs::read(&path).unwrap();
    cold.saved.unread_kept_parts();
    BEFORE_PROJECT.with(|hook| {
        assert!(
            hook.replace(Some(Box::new(
                move || std::fs::write(&path, readable).unwrap()
            )))
            .is_none()
        );
    });

    let result = call(&cold, 2, BYTES_MAX);

    assert!(matches!(
        result,
        Err(persistence::Error::Bound("process group kept windows"))
    ));
}

#[test]
fn refuses_a_window_whose_parts_become_unreadable_while_its_groups_are_read() {
    // Goal: a window chosen to give an App's groups from its parts gives them, so parts that
    // become unreadable while the groups are read are refused, not read as groups with no
    // members.
    // Method: seal two observations with their Apps' parts, turn those parts to a version the
    // session does not read before the groups are projected, and ask for the groups.
    let cold = kept_cold();
    let path = cold
        .fixture
        .root
        .join(&cold.saved.windows[0].data_directory)
        .join("app_parts.parquet");
    let readable = std::fs::read(&path).unwrap();
    cold.saved.unread_kept_parts();
    let unreadable = std::fs::read(&path).unwrap();
    std::fs::write(&path, readable).unwrap();
    BEFORE_PROJECT.with(|hook| {
        assert!(
            hook.replace(Some(Box::new(
                move || std::fs::write(&path, unreadable).unwrap()
            )))
            .is_none()
        );
    });

    let result = call(&cold, 2, BYTES_MAX);

    assert!(matches!(
        result,
        Err(persistence::Error::Bound("process group kept windows"))
    ));
}

/// A recording of two observations of an App running two executables, sealed with its Apps'
/// parts.
fn kept_cold() -> Box<Cold> {
    let mut fixture = Fixture::new();
    publish_workers(&mut fixture, 1, 2);
    publish_workers(&mut fixture, 2, 2);
    fixture.seal_derived(&[], &crate::SealedApps);
    let cancellation = Arc::new(Cancellation::default());
    let saved = Session::open_with(&fixture.root, Arc::clone(&cancellation), 0).unwrap();
    Box::new(Cold {
        fixture,
        saved,
        history: History::new(),
        cancellation,
    })
}

fn cold() -> Box<Cold> {
    let mut fixture = named::dense();
    publish(&mut fixture, 1);
    publish(&mut fixture, 2);
    named::seal_selected(&fixture, &[1, 2]);
    let cancellation = Arc::new(Cancellation::default());
    let saved = Session::open_with(&fixture.root, Arc::clone(&cancellation), 0).unwrap();
    Box::new(Cold {
        fixture,
        saved,
        history: History::new(),
        cancellation,
    })
}

fn publish(fixture: &mut Fixture, publication: u64) {
    publish_workers(fixture, publication, 9);
}

/// Publishes `count` processes of one App, each running an executable of its own.
fn publish_workers(fixture: &mut Fixture, publication: u64, count: usize) {
    let names: Vec<_> = (1..=count).map(|index| format!("worker{index}")).collect();
    let rows: Vec<_> = names
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let amount = u64::try_from(index + 1).unwrap() * (publication - 1);
            let mut row = named::member(
                i32::try_from(index + 1).unwrap(),
                name.as_bytes(),
                b"/A.app",
                Usage {
                    awake_ns_at_start: 0,
                    user_ns: amount * 1_000_000,
                    system_ns: 0,
                    energy_nj: amount * 1_000_000,
                    instructions: 0,
                    super_instructions: 0,
                    cycles: 0,
                    idle_wakeups: amount * 6,
                    interrupt_wakeups: 0,
                    pageins: amount * 4,
                    resident_bytes: 1000,
                    footprint_bytes: 9_007_199_254_740_993,
                    disk_read_bytes: amount * 160,
                    disk_written_bytes: amount * 80,
                },
            );
            row.threads = i32::try_from(index + 1).unwrap();
            row
        })
        .collect();
    named::publish(fixture, publication, &rows);
}

fn observation(fixture: &Fixture, publication: u64) -> Observation {
    let run = fixture.writer.identity().unwrap();
    Observation {
        recording: run.recording.0,
        stamp: Stamp {
            boot: run.boot.0,
            run: run.run.0,
            publication,
            continuous_ns: Some(publication * 1_000_000_000),
            awake_ns: Some(publication * 1_000_000_000),
        },
        run_tick: publication,
        wall_ns: Some((1000 + publication) * 1_000_000_000),
    }
}

fn call(
    cold: &Cold,
    publication: u64,
    available: usize,
) -> persistence::Result<Box<AppProcessGroupsHistory>> {
    let operation = Arc::clone(&cold.cancellation).operation(0)?;
    let selected = observation(&cold.fixture, publication);
    let target = Target {
        kind: KeyKind::Bundle,
        bytes: b"/A.app",
    };
    let reduction = Reduction::new(
        &crate::live_app_response::Selected::ending_at(selected).unwrap(),
        target.kind,
        target.bytes,
        &operation,
    )?;
    cold.history.process_groups_response_saved(
        &Query {
            selected: &crate::live_app_response::Selected::ending_at(selected).unwrap(),
            target: &target,
            saved: &cold.saved,
            context: &[],
            operation: &operation,
            available,
        },
        reduction,
    )
}

fn advance(cold: &mut Cold) {
    publish(&mut cold.fixture, 3);
    commit(&cold.fixture, 3, 1);
    cold.saved = Session::open_with(&cold.fixture.root, Arc::clone(&cold.cancellation), 0).unwrap();
}

fn without_groups(cold: &Cold) -> usize {
    let mut resident = cold.history.saved_residents.borrow_mut();
    let groups = resident.take_groups();
    let bytes = resident.bytes();
    resident.install_groups(groups);
    bytes
}

fn other_owners(cold: &Cold) {
    let operation = Arc::clone(&cold.cancellation).operation(0).unwrap();
    let selected = observation(&cold.fixture, 2);
    cold.history
        .resident_response(
            &crate::live_app_response::Selected::ending_at(selected).unwrap(),
            Some(&cold.saved),
            &operation,
        )
        .unwrap();
    let rates = cold
        .history
        .attribution_batch_response(
            &crate::live_app_response::Selected::ending_at(selected).unwrap(),
            Some(&cold.saved),
            &operation,
        )
        .unwrap();
    assert!(rates.cpu.is_ok());
    assert!(rates.power.is_ok());
    assert!(rates.disk.is_ok());
}

fn commit(fixture: &Fixture, publication: u64, window: u64) {
    let mut reader = collector::ring::RingReader::open_generation(fixture.writer.name()).unwrap();
    let shape = reader.shape();
    let mut tick = persistence::tick_buffer(&shape).unwrap();
    let mut identities = Vec::with_capacity(600);
    for _attempt in 0..8 {
        let collector::ring::Found::Publication {
            publication: found, ..
        } = reader
            .read_identity_into(&mut tick, &mut identities)
            .unwrap()
        else {
            continue;
        };
        if found != publication {
            continue;
        }
        let mut selected = persistence::Window::new(
            persistence::WindowIdentity {
                run: fixture.writer.identity().unwrap(),
                window,
            },
            persistence::Limits::from_shape(&shape).unwrap(),
        );
        selected
            .push_tick_with_identity(
                &persistence::Observation {
                    publication,
                    wall_ns: Some((1000 + publication) * 1_000_000_000),
                    continuous_ns: Some(publication * 1_000_000_000),
                    awake_ns: Some(publication * 1_000_000_000),
                },
                &tick,
                &[],
                &persistence::IdentitySource {
                    reader: &reader,
                    processes: &identities,
                    events: &[],
                },
            )
            .unwrap();
        seal(fixture, &selected, publication);
        return;
    }
    panic!("missing fixture publication");
}

fn seal(fixture: &Fixture, selected: &persistence::Window, publication: u64) {
    let store = persistence::Store::open(&fixture.root).unwrap();
    let persistence::Begin::Pending(pending) = store.begin(&selected.identity).unwrap() else {
        panic!("fixture window already committed");
    };
    let tables = persistence::Encoder
        .encode(selected, pending.path())
        .unwrap();
    store
        .commit(
            pending,
            persistence::Manifest {
                identity: selected.identity,
                publication_first: publication,
                publication_end: publication + 1,
                events_first: 0,
                events_end: 0,
                wall_ns: Some(persistence::TimeBounds {
                    min: (1000 + publication) * 1_000_000_000,
                    max: (1000 + publication) * 1_000_000_000,
                }),
                continuous_ns: None,
                awake_ns: None,
                losses: persistence::Losses::default(),
                data_directory: String::new(),
                tables,
            },
        )
        .unwrap();
}

fn store_marker(fixture: &Fixture, manifest: &persistence::Manifest) -> std::path::PathBuf {
    persistence::Store::open(&fixture.root)
        .unwrap()
        .marker_path(&manifest.identity)
}

#[test]
fn warm_reuse_and_zero_budget_keep_nonzero_added_measures_including_other() {
    // A warm cached result and complete fallback retain all ten readings without decoding again.
    let cold = cold();
    let first = call(&cold, 2, BYTES_MAX).unwrap();
    let decoded = crate::session::ProcessGroupsReplay::test_frame_decodes();

    let warm = call(&cold, 2, BYTES_MAX).unwrap();
    let after_warm = crate::session::ProcessGroupsReplay::test_frame_decodes();
    let uncached = call(&cold, 2, 0).unwrap();

    equal(&first, &warm);
    equal(&warm, &uncached);
    assert_eq!(after_warm, decoded);
    assert_eq!(
        warm.points[1].readings[0].measures.get(7).map(|m| m.known),
        Some(54)
    );
    assert_eq!(
        warm.points[1].readings[0].measures.get(8).map(|m| m.known),
        Some(36)
    );
    assert_eq!(
        warm.points[1].readings[0].measures.get(9).map(|m| m.known),
        Some(9)
    );
    assert_eq!(
        warm.points[1].readings[7].measures.get(7).map(|m| m.known),
        Some(18)
    );
    assert_eq!(
        warm.points[1].readings[7].measures.get(8).map(|m| m.known),
        Some(12)
    );
    assert_eq!(
        warm.points[1].readings[7].measures.get(9).map(|m| m.known),
        Some(3)
    );
}
