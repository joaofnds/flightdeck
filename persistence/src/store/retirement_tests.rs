//! Physical removal through the real publication route, with the clock given to it.

use super::*;
use crate::retention::{
    Clock, ClockReading, Period, boot_offset_ns, boot_offset_save, order_entry_names,
};

const PERIOD_SECONDS: u64 = 60;
const GRACE_SECONDS: u64 = 60;
const SECOND_NS: u64 = 1_000_000_000;
/// A clock reading far past anything these fixtures save, where everything has expired.
const FAR_NS: u64 = 1 << 62;

/// A store over a recording that keeps a minute, whose readings lie on the retention clock at
/// their own continuous clock.
fn retirement_store(fixture: &Fixture) -> Store {
    let store = Store::open(&fixture.root).unwrap();
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    Period::from_seconds(PERIOD_SECONDS)
        .unwrap()
        .write(&root)
        .unwrap();
    boot_offset_save(&root, fixture.run.boot, 0).unwrap();
    Clock::started_at(&retirement_at(fixture, 0))
        .write(&root)
        .unwrap();
    store
}

fn retirement_windows(fixture: &mut Fixture, store: &Store, count: u64) {
    let memory = registration_budget();
    for window in 0..count {
        publish_counted_window(fixture, store, window, &memory);
    }
}

/// The retained times of what the order holds, oldest first.
fn retirement_places(fixture: &Fixture) -> Vec<u64> {
    let mut places: Vec<u64> = order_entry_names(&fixture.root)
        .iter()
        .map(|name| name.split_once('-').unwrap().0.parse().unwrap())
        .collect();
    places.sort_unstable();
    places
}

fn retirement_at(fixture: &Fixture, clock_ns: u64) -> ClockReading {
    ClockReading {
        boot: fixture.run.boot,
        continuous_ns: clock_ns,
        wall_ns: clock_ns,
    }
}

/// The clock at which everything before `place` has outlived the period and the grace.
fn retirement_clock_keeping(place: u64) -> u64 {
    place + (PERIOD_SECONDS + GRACE_SECONDS) * SECOND_NS
}

/// Passes until one removes nothing, which a recording's size bounds.
fn retirement_drain(fixture: &Fixture, store: &Store, runs: &[RunIdentity], clock_ns: u64) {
    for _pass in 0..256 {
        let retired = store
            .retire_expired(
                &retirement_at(fixture, clock_ns),
                runs,
                &registration_budget(),
            )
            .unwrap();
        if retired == 0 {
            return;
        }
    }
    panic!("removal did not finish");
}

/// A reading of the clock in a boot that no publication of the fixture ran in.
fn retirement_in_another_boot(clock_ns: u64) -> ClockReading {
    ClockReading {
        boot: Identity([5, 5]),
        continuous_ns: clock_ns,
        wall_ns: clock_ns,
    }
}

fn retirement_names_under(fixture: &Fixture, directory: &str) -> usize {
    fs::read_dir(fixture.root.join(directory)).unwrap().count()
}

fn retirement_pass(
    fixture: &Fixture,
    store: &Store,
    runs: &[RunIdentity],
    clock_ns: u64,
) -> io::Result<u64> {
    store.retire_expired(
        &retirement_at(fixture, clock_ns),
        runs,
        &registration_budget(),
    )
}

/// The raw directory of the window whose entry lies at `place`.
fn retirement_raw_of(fixture: &Fixture, place: u64) -> PathBuf {
    let name = order_entry_names(&fixture.root)
        .into_iter()
        .find(|name| name.starts_with(&format!("{place}-")))
        .unwrap();

    fixture
        .root
        .join("data")
        .join(name.split('-').nth(1).unwrap())
        .join("ready")
}

#[test]
fn a_pass_removes_what_outlived_the_period_and_keeps_the_rest() {
    // Goal: saved readings go once they have outlived the period, and nothing younger goes.
    // Method: save four one-reading windows, and pass at the clock that puts the cutoff just past
    // the third, which then waits for the fourth, whose first reading has not expired.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 4);
    let places = retirement_places(&fixture);

    let retired = store
        .retire_expired(
            &retirement_at(&fixture, retirement_clock_keeping(places[2] + 1)),
            &[fixture.run],
            &registration_budget(),
        )
        .unwrap();

    assert_eq!(retired, 2);
    assert_eq!(retirement_places(&fixture), places[2..]);
    assert_eq!(retirement_names_under(&fixture, "data"), 2);
    let catalog =
        crate::canonical::catalog::Catalog::open(&fixture.root, &registration_budget()).unwrap();
    let kept = catalog
        .windows_of(&fixture.run, &mut ReadWork::default())
        .unwrap();
    assert_eq!(kept.len(), 2);
}

#[test]
fn a_pass_before_the_period_and_the_grace_are_up_removes_nothing() {
    // Goal: a reading keeps its whole period and the grace a read under way has, which only a
    // window with no reading after it depends on, since the reading after one waits longer.
    // Method: pass at the last clock at which a lone window's retained time is not below the
    // cutoff, and at the next.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 1);
    let places = retirement_places(&fixture);
    let last_clock = retirement_clock_keeping(places[0]);
    let memory = registration_budget();

    let at_the_last = store
        .retire_expired(&retirement_at(&fixture, last_clock), &[], &memory)
        .unwrap();
    let at_the_next = store
        .retire_expired(&retirement_at(&fixture, last_clock + 1), &[], &memory)
        .unwrap();

    assert_eq!(at_the_last, 0);
    assert_eq!(at_the_next, 1);
}

#[test]
fn a_registered_run_keeps_its_newest_publication_however_old_it_is() {
    // Goal: the next save of a registered run still finds its predecessor.
    // Method: expire every window of a registered run, and read its frontier afterwards.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 3);
    let memory = registration_budget();
    let before = store
        .source_frontier(&fixture.run, &memory, &mut ReadWork::default())
        .unwrap()
        .unwrap();

    retirement_drain(&fixture, &store, &[fixture.run], FAR_NS);

    let after = store
        .source_frontier(&fixture.run, &memory, &mut ReadWork::default())
        .unwrap()
        .unwrap();
    assert_eq!(after.predecessor(), before.predecessor());
    assert_eq!(after.publication_end(), before.publication_end());
    assert_eq!(order_entry_names(&fixture.root).len(), 1);
}

#[test]
fn a_run_no_longer_registered_goes_whole_with_the_directories_it_grew() {
    // Goal: 30 days of history leaves no directory behind, and later saves route around what went.
    // Method: save enough windows to split a leaf, expire them all, then save a new run.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 129);

    retirement_drain(&fixture, &store, &[], FAR_NS);

    assert_eq!(order_entry_names(&fixture.root).len(), 0);
    for directory in ["data", "ordinary", "observations-known", "retention/order"] {
        assert_eq!(
            retirement_names_under(&fixture, directory),
            0,
            "{directory}"
        );
    }
    let memory = registration_budget();
    let mut second = source_for_root(&fixture, Identity([8, 10]));
    second.writer.finish();
    let attempt = store
        .register_attempt(&second.input(false), &mut second.reader(), &memory)
        .unwrap();
    let (_, attempt) = encode_original(attempt, &mut second, 1, &store);
    finish_observe_wrapper(&second, &mut attempt.normal_publication(&memory).unwrap());

    assert_eq!(retirement_names_under(&fixture, "data"), 1);
}

#[test]
fn a_crash_under_a_removal_busy_settles_at_the_next_pass() {
    // Goal: a removal cut short cannot stop readers or saves for good.
    // Method: leave the namespace as a crash would, reopen the root and pass.
    let fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    drop(store);
    let mut bytes = [0; 256];
    codec::encode_state(
        fixture.run.recording,
        &codec::NamespaceState::Busy {
            sequence: 1,
            transaction: codec::RETIREMENT_TRANSACTION,
        },
        &mut bytes,
    )
    .unwrap();
    fs::write(fixture.root.join("namespace-state"), bytes).unwrap();

    let store = Store::open(&fixture.root).unwrap();
    store
        .retire_expired(
            &retirement_at(&fixture, SECOND_NS),
            &[],
            &registration_budget(),
        )
        .unwrap();

    let state = fs::read(fixture.root.join("namespace-state")).unwrap();
    assert_eq!(
        codec::decode_state(&state, fixture.run.recording).unwrap(),
        codec::NamespaceState::Quiet { sequence: 2 }
    );
}

#[test]
fn a_proof_that_does_not_read_stops_no_removal() {
    // Goal: damage to something expired cannot keep the recording from shrinking.
    // Method: damage the oldest window's proof, whose hard links all share it, and pass.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 3);
    let places = retirement_places(&fixture);
    let name = order_entry_names(&fixture.root)
        .into_iter()
        .find(|name| name.starts_with(&format!("{}-", places[0])))
        .unwrap();
    let transaction = name.split('-').nth(1).unwrap().to_owned();
    fs::write(
        fixture
            .root
            .join("data")
            .join(&transaction)
            .join("ready/metadata"),
        b"damaged",
    )
    .unwrap();

    let retired = store
        .retire_expired(
            &retirement_at(&fixture, retirement_clock_keeping(places[1])),
            &[fixture.run],
            &registration_budget(),
        )
        .unwrap();

    assert_eq!(retired, 1);
    assert_eq!(retirement_places(&fixture), places[1..]);
    assert!(!fixture.root.join("data").join(&transaction).exists());
}

#[test]
fn a_reading_removed_after_it_was_listed_asks_for_the_list_again() {
    // Goal: a removal between a reader's list and its lookup never reads as damage.
    // Method: list the readings, pass, and look up the window of the oldest of them.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 4);
    let places = retirement_places(&fixture);
    let catalog =
        crate::canonical::catalog::Catalog::open(&fixture.root, &registration_budget()).unwrap();
    let state = catalog.namespace_state().unwrap();
    let page = catalog
        .known_page(0, u64::MAX / 2, None, &mut ReadWork::default())
        .unwrap();
    store
        .retire_expired(
            &retirement_at(&fixture, retirement_clock_keeping(places[2])),
            &[fixture.run],
            &registration_budget(),
        )
        .unwrap();

    let lookup = catalog.window_of_listed(
        &page.readings[0].run,
        page.readings[0].publication,
        &state,
        &mut ReadWork::default(),
    );

    assert_eq!(lookup.unwrap_err().kind(), io::ErrorKind::WouldBlock);
}

#[test]
fn a_reader_waits_while_a_removal_holds_the_namespace() {
    // Goal: names vanishing under a reader are a wait and never damage.
    // Method: leave the namespace as a removal under way does, and read the recording's end.
    let fixture = Fixture::new(false);
    drop(retirement_store(&fixture));
    let mut bytes = [0; 256];
    codec::encode_state(
        fixture.run.recording,
        &codec::NamespaceState::Busy {
            sequence: 1,
            transaction: codec::RETIREMENT_TRANSACTION,
        },
        &mut bytes,
    )
    .unwrap();
    fs::write(fixture.root.join("namespace-state"), bytes).unwrap();
    let catalog =
        crate::canonical::catalog::Catalog::open(&fixture.root, &registration_budget()).unwrap();

    let read = catalog.newest_known(&mut ReadWork::default());

    assert_eq!(read.unwrap_err().kind(), io::ErrorKind::WouldBlock);
}

#[test]
fn a_publication_past_its_period_still_finishes_beside_a_pass() {
    // Goal: a publication held up past the period keeps its raw data and its Busy, and what it
    // reads as its predecessor.
    // Method: pass at a far clock before a publication takes the namespace and after each step
    // once it has, then read what it saved.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 2);
    let memory = registration_budget();
    let input = ordinary_input(&fixture, &store, 2, &memory);
    let attempt = store
        .register_attempt(&input, &mut fixture.reader(), &memory)
        .unwrap();
    let (_, attempt) = encode_original(attempt, &mut fixture, 3, &store);
    let mut publication = attempt.normal_publication(&memory).unwrap();
    let at = retirement_at(&fixture, FAR_NS);

    let before_the_namespace_is_taken = store.retire_expired(&at, &[], &memory).unwrap();

    assert_eq!(before_the_namespace_is_taken, 1);
    let mut passes_after = 0;
    for _step in 0..512 {
        if publication.step().unwrap() == PublicationStep::Idle {
            break;
        }
        let slot = fixture.slot();
        let held =
            crate::transaction::normal::NormalState::decode(&slot, fixture.run.recording).unwrap();
        if held.sequence > 0 {
            let state = fs::read(fixture.root.join("namespace-state")).unwrap();
            assert_eq!(store.retire_expired(&at, &[], &memory).unwrap(), 0);
            assert_eq!(
                fs::read(fixture.root.join("namespace-state")).unwrap(),
                state
            );
            passes_after += 1;
        }
    }

    assert!(passes_after > 0);
    let catalog = crate::canonical::catalog::Catalog::open(&fixture.root, &memory).unwrap();
    let kept = catalog
        .windows_of(&fixture.run, &mut ReadWork::default())
        .unwrap();
    assert_eq!(kept.len(), 2);
    assert_eq!(order_entry_names(&fixture.root).len(), 2);
    assert_eq!(retirement_names_under(&fixture, "data"), 2);
}

#[test]
fn a_pass_removes_no_more_than_its_bound() {
    // Goal: a pass costs its bound however much has expired, so a shortened period cannot stall
    // the saves that share its thread.
    // Method: expire more windows than a pass removes, and pass twice.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 17);
    let memory = registration_budget();
    let at = retirement_at(&fixture, FAR_NS);

    let first = store.retire_expired(&at, &[], &memory).unwrap();
    let second = store.retire_expired(&at, &[], &memory).unwrap();

    assert_eq!(first, 16);
    assert_eq!(second, 1);
}

#[test]
fn a_pass_saves_the_clock_it_read() {
    // Goal: the clock moves on whether or not anything expired, so a later read cannot run behind
    // a pass.
    // Method: pass at one clock on an empty recording, and read the saved clock from an earlier
    // one.
    let fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    let later = retirement_at(&fixture, 2 * FAR_NS / 4);

    store
        .retire_expired(&later, &[], &registration_budget())
        .unwrap();

    let root = PrivateDirectory::open(&fixture.root).unwrap();
    let earlier = retirement_at(&fixture, FAR_NS / 4);
    assert_eq!(
        Clock::read(&root, &earlier).progress_ns(),
        later.continuous_ns
    );
}

#[test]
fn a_window_waits_for_a_successor_inside_the_period() {
    // Goal: the reading after a window takes its rates from that window, so the window goes only
    // once that reading has expired. A reading off the clock holds nothing back, since a row of
    // them would hold every window behind it for good.
    // Method: compare each kind of successor with a cutoff at a known time.
    let cutoff = Clock::started_at(&ClockReading {
        boot: Identity([1, 2]),
        continuous_ns: 0,
        wall_ns: 1_000 * SECOND_NS,
    })
    .cutoff(Period::from_seconds(PERIOD_SECONDS).unwrap());
    let first = 940 * SECOND_NS;
    let bounds = |min| Some(crate::metadata::TimeBounds { min, max: min });

    let rows = [
        ("inside the period", bounds(first), Some(0), true),
        ("expired", bounds(first - 1), Some(0), false),
        (
            "inside the period by its offset",
            bounds(first - 1),
            Some(1),
            true,
        ),
        ("no clock", None, Some(0), false),
        ("no boot record", bounds(0), None, false),
    ];

    for (name, successor, offset_ns, waits) in rows {
        assert_eq!(
            retirement::retire_one_waits_for(successor, offset_ns, cutoff),
            waits,
            "{name}"
        );
    }
}

#[test]
fn a_window_stays_through_the_grace_of_the_reading_after_it() {
    // Goal: a read of the first retained reading loads the window before it, so that window lasts
    // as long as a read under way may go on returning the reading.
    // Method: pass where the second window's first reading has only just outlived the period, and
    // again once the grace has passed too.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 3);
    let successor_first_ns = retirement_places(&fixture)[1];
    let just_expired_ns = successor_first_ns + 1 + PERIOD_SECONDS * SECOND_NS;
    let no_runs: &[RunIdentity] = &[];

    let within_the_grace = store
        .retire_expired(
            &retirement_at(&fixture, just_expired_ns + GRACE_SECONDS * SECOND_NS - 1),
            no_runs,
            &registration_budget(),
        )
        .unwrap();
    let past_the_grace = store
        .retire_expired(
            &retirement_at(&fixture, just_expired_ns + GRACE_SECONDS * SECOND_NS),
            no_runs,
            &registration_budget(),
        )
        .unwrap();

    assert_eq!(within_the_grace, 0);
    assert_eq!(past_the_grace, 1);
}

#[test]
fn a_successor_that_does_not_read_stops_no_removal() {
    // Goal: damage to what comes after an expired window cannot keep it from going.
    // Method: damage the second window's proof and pass where both have expired.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 2);
    let places = retirement_places(&fixture);
    let name = order_entry_names(&fixture.root)
        .into_iter()
        .find(|name| name.starts_with(&format!("{}-", places[1])))
        .unwrap();
    let transaction = name.split('-').nth(1).unwrap().to_owned();
    let proof = fixture
        .root
        .join("data")
        .join(&transaction)
        .join("ready/metadata");
    fs::write(proof, b"damaged").unwrap();

    let retired = store
        .retire_expired(
            &retirement_at(&fixture, FAR_NS),
            &[],
            &registration_budget(),
        )
        .unwrap();

    assert_eq!(retired, 2);
    assert_eq!(order_entry_names(&fixture.root).len(), 0);
}

#[test]
fn a_clock_that_lost_its_record_stands_no_lower_than_the_newest_entry() {
    // Goal: a lost clock record cannot make a saved publication look young, however far back the
    // wall clock stands.
    // Method: lose the record, take a pass with the wall clock at zero, and see that the newest
    // entry's place expired the oldest.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 5);
    let places = retirement_places(&fixture);
    fs::remove_file(fixture.root.join("retention/clock")).unwrap();

    store
        .retire_expired(&retirement_at(&fixture, 0), &[], &registration_budget())
        .unwrap();

    assert_eq!(retirement_places(&fixture), places[1..]);
}

#[test]
fn a_saved_clock_behind_the_newest_entry_stays_where_it_is() {
    // Goal: the floor applies only where the record is lost, so a saved clock's value decides.
    // Method: take a pass with a saved clock at zero, behind every entry.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 4);
    let places = retirement_places(&fixture);

    store
        .retire_expired(&retirement_at(&fixture, 0), &[], &registration_budget())
        .unwrap();

    assert_eq!(retirement_places(&fixture), places);
}

#[test]
fn a_boot_record_stays_until_nothing_of_that_boot_can_need_it() {
    // Goal: boot records plateau, and a record outlives its publications, since a reader shows a
    // reading whose boot has no record.
    // Method: run a pass in another boot, expire all but the entry at the value that pass recorded,
    // which a registered run holds, and then that one too.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    retirement_windows(&mut fixture, &store, 2);
    let newest = *retirement_places(&fixture).last().unwrap();
    let running = Identity([5, 5]);
    boot_offset_save(&root, running, 7).unwrap();
    Clock::started_at(&retirement_at(&fixture, newest))
        .write(&root)
        .unwrap();
    let budget = registration_budget();

    let keeping = retirement_clock_keeping(newest + 1);
    for clock_ns in [newest, keeping, keeping] {
        store
            .retire_expired(
                &retirement_in_another_boot(clock_ns),
                &[fixture.run],
                &budget,
            )
            .unwrap();
    }
    assert_eq!(retirement_places(&fixture), [newest]);
    let beside_the_last_entry = boot_offset_ns(&root, fixture.run.boot).unwrap();
    for clock_ns in [keeping + 1, keeping + 2] {
        store
            .retire_expired(&retirement_in_another_boot(clock_ns), &[], &budget)
            .unwrap();
    }

    assert_eq!(beside_the_last_entry, Some(0));
    assert_eq!(retirement_places(&fixture), [0_u64; 0]);
    assert_eq!(boot_offset_ns(&root, fixture.run.boot).unwrap(), None);
    assert_eq!(boot_offset_ns(&root, running).unwrap(), Some(7));
}

#[test]
fn a_boot_record_that_does_not_read_goes_without_stopping_the_pass() {
    // Goal: a damaged record, which places no reading, neither stays forever nor stops removal.
    // Method: damage a boot's record, and take one pass past every entry in another boot, which
    // takes both windows, since neither waits for a successor no record can place.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    retirement_windows(&mut fixture, &store, 2);
    let newest = *retirement_places(&fixture).last().unwrap();
    let name = format!(
        "retention/boot-{}",
        collector::recording::identity_text(fixture.run.boot)
    );
    fs::write(fixture.root.join(name), b"damaged").unwrap();
    let far = retirement_clock_keeping(newest) + 1;

    store
        .retire_expired(
            &retirement_in_another_boot(far),
            &[],
            &registration_budget(),
        )
        .unwrap();

    assert_eq!(boot_offset_ns(&root, fixture.run.boot).unwrap(), None);
    assert_eq!(retirement_places(&fixture), [0_u64; 0]);
}

#[test]
fn a_period_that_does_not_read_removes_nothing() {
    // Goal: a file that cannot say how long to keep readings deletes none of them.
    // Method: damage the period where every window has outlived any period, pass, then put the
    // file right and pass again.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 3);
    let period = fixture.root.join("retention/period");
    fs::write(&period, b"soon").unwrap();

    let refused = retirement_pass(&fixture, &store, &[], FAR_NS);
    let kept = retirement_places(&fixture).len();
    fs::write(&period, b"60").unwrap();
    let removed = retirement_pass(&fixture, &store, &[], FAR_NS).unwrap();

    assert_eq!(refused.unwrap_err().kind(), io::ErrorKind::InvalidData);
    assert_eq!(kept, 3);
    assert_eq!(removed, 3);
}

#[test]
fn a_period_may_end_in_a_line_break() {
    // Goal: a period written by hand keeps its meaning, since that is the route with no window.
    // Method: write thirty days as a shell would, and pass two days after the last reading.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 3);
    fs::write(fixture.root.join("retention/period"), b"2592000\n").unwrap();
    let two_days_later = retirement_places(&fixture)[2] + 2 * 86_400 * SECOND_NS;

    let removed = retirement_pass(&fixture, &store, &[], two_days_later).unwrap();

    assert_eq!(removed, 0);
}

#[test]
fn a_damaged_record_of_the_running_boot_stops_nothing_and_comes_back() {
    // Goal: the running boot's record, which only the next reboot would otherwise clear, can stop
    // no removal, and the next publication puts it back.
    // Method: damage the record of the boot the clock runs in, pass where the older windows have
    // expired, and publish once more.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    let root = PrivateDirectory::open(&fixture.root).unwrap();
    retirement_windows(&mut fixture, &store, 3);
    let name = format!(
        "retention/boot-{}",
        collector::recording::identity_text(fixture.run.boot)
    );
    fs::write(fixture.root.join(name), b"damaged").unwrap();
    let memory = registration_budget();

    let removed = retirement_pass(&fixture, &store, &[fixture.run], FAR_NS).unwrap();
    let after_the_pass = boot_offset_ns(&root, fixture.run.boot).unwrap();
    publish_counted_window(&mut fixture, &store, 3, &memory);

    assert_eq!(removed, 2);
    assert_eq!(after_the_pass, None);
    assert!(boot_offset_ns(&root, fixture.run.boot).unwrap().is_some());
}

#[test]
fn a_frontier_that_does_not_read_keeps_its_run_for_the_pass() {
    // Goal: a registered run whose newest proof does not read loses nothing a later save may read,
    // and the pass ends without an error, so the rest of the recording still shrinks.
    // Method: damage the newest proof of the registered run and pass where the older windows, which
    // do read, have expired.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 3);
    let places = retirement_places(&fixture);
    fs::write(
        retirement_raw_of(&fixture, places[2]).join("metadata"),
        b"damaged",
    )
    .unwrap();

    let removed = retirement_pass(
        &fixture,
        &store,
        &[fixture.run],
        retirement_clock_keeping(places[2]),
    )
    .unwrap();

    assert_eq!(removed, 0);
    assert_eq!(retirement_places(&fixture), places);
}

#[test]
fn a_raw_directory_that_does_not_empty_stops_no_other_removal() {
    // Goal: what stops one entry stops that entry alone.
    // Method: leave a stray directory in the oldest window's raw directory and pass where every
    // window has expired.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 3);
    let places = retirement_places(&fixture);
    fs::create_dir(retirement_raw_of(&fixture, places[0]).join("stray")).unwrap();

    let removed = retirement_pass(&fixture, &store, &[], FAR_NS).unwrap();

    assert_eq!(removed, 2);
    assert_eq!(retirement_places(&fixture), places[..1]);
}

#[test]
fn a_clock_and_room_that_refuse_stop_no_removal() {
    // Goal: the state a pass keeps for itself cannot stand between it and what it removes.
    // Method: put directories where the clock and the room aside belong, so neither the save nor
    // giving up the room can work, and pass where every window has expired.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 3);
    let clock = fixture.root.join("retention/clock");
    fs::remove_file(&clock).unwrap();
    fs::create_dir(&clock).unwrap();
    fs::create_dir(fixture.root.join("retention/reserve")).unwrap();

    let removed = retirement_pass(&fixture, &store, &[], FAR_NS).unwrap();

    assert_eq!(removed, 3);
}

#[test]
fn an_hour_of_the_order_that_does_not_open_stops_no_removal() {
    // Goal: a name nothing here made in the order stops that name alone.
    // Method: leave a file named like an hour, past the hour the fixture's entries lie in, and pass
    // where every window has expired.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 3);
    fs::write(fixture.root.join("retention/order/1"), b"stray").unwrap();

    let removed = retirement_pass(&fixture, &store, &[], FAR_NS).unwrap();

    assert_eq!(removed, 3);
}

#[test]
fn a_proof_the_system_refuses_is_no_damage() {
    // Goal: a failure that says nothing about the proof deletes nothing of its window.
    // Method: take away permission on the oldest proof, and pass where every window has expired.
    use std::os::unix::fs::PermissionsExt;
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 3);
    let places = retirement_places(&fixture);
    let raw = retirement_raw_of(&fixture, places[0]);
    fs::set_permissions(raw.join("metadata"), fs::Permissions::from_mode(0o000)).unwrap();

    let removed = retirement_pass(&fixture, &store, &[], FAR_NS).unwrap();

    assert_eq!(removed, 2);
    assert!(raw.join("ticks.parquet").exists());
    assert_eq!(retirement_places(&fixture), places[..1]);
}

#[test]
fn a_budget_that_refuses_the_ticks_is_no_damage() {
    // Goal: running short of working memory deletes nothing.
    // Method: pass where every window has expired with a budget too small for any table.
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 3);
    let places = retirement_places(&fixture);
    let raw = retirement_raw_of(&fixture, places[0]);
    let refusing = Arc::new(crate::resources::ByteBudget::new(1));

    let removed = store
        .retire_expired(&retirement_at(&fixture, FAR_NS), &[], &refusing)
        .unwrap();

    assert_eq!(removed, 0);
    assert!(raw.join("ticks.parquet").exists());
    assert_eq!(retirement_places(&fixture), places);
}

/// What a catalog of the fixture lists once the oldest of three windows, whose ticks `damage`
/// leaves unreadable, has been removed: the oldest reading's publication and how many windows the
/// whole clock holds.
fn retirement_after_removing_damaged_ticks(damage: impl FnOnce(&Path)) -> (u64, u64, usize) {
    let mut fixture = Fixture::new(false);
    let store = retirement_store(&fixture);
    retirement_windows(&mut fixture, &store, 3);
    let places = retirement_places(&fixture);
    let catalog =
        crate::canonical::catalog::Catalog::open(&fixture.root, &registration_budget()).unwrap();
    let mut work = ReadWork::default();
    let second_oldest = catalog.windows_of(&fixture.run, &mut work).unwrap()[1].publication_first;
    damage(&retirement_raw_of(&fixture, places[0]).join("ticks.parquet"));

    let removed = retirement_pass(
        &fixture,
        &store,
        &[],
        retirement_clock_keeping(places[1] + 1),
    )
    .unwrap();

    assert_eq!(removed, 1);
    let oldest = catalog.oldest_known(&mut work).unwrap().unwrap();
    let windows = catalog.windows_in(0, u64::MAX, &mut work).unwrap();

    (oldest.publication, second_oldest, windows.len())
}

#[test]
fn a_window_whose_ticks_do_not_read_goes_whole() {
    // Goal: a reader meets nothing of a removed window, so damage in it leaves no name behind that
    // would read as damage in the recording around it.
    // Method: damage the oldest window's ticks, remove it, and list the clock's whole range.
    let (oldest, second_oldest, windows) = retirement_after_removing_damaged_ticks(|ticks| {
        fs::write(ticks, b"damaged").unwrap();
    });

    assert_eq!(oldest, second_oldest);
    assert_eq!(windows, 2);
}

#[test]
fn a_window_whose_ticks_are_gone_goes_whole() {
    // Goal: as above, for a table that is missing rather than damaged.
    // Method: delete the oldest window's ticks, remove it, and list the clock's whole range.
    let (oldest, second_oldest, windows) = retirement_after_removing_damaged_ticks(|ticks| {
        fs::remove_file(ticks).unwrap();
    });

    assert_eq!(oldest, second_oldest);
    assert_eq!(windows, 2);
}
