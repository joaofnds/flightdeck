//! The retention clock and period, with the Mac's clocks given to them.

use super::*;

const SECOND_NS: u64 = 1_000_000_000;
const THIS_BOOT: Identity = Identity([1, 2]);
const NEXT_BOOT: Identity = Identity([3, 4]);
const START_NS: u64 = 1_000_000 * SECOND_NS;

fn reading(boot: Identity, continuous_seconds: u64, wall_seconds: u64) -> ClockReading {
    ClockReading {
        boot,
        continuous_ns: continuous_seconds * SECOND_NS,
        wall_ns: START_NS + wall_seconds * SECOND_NS,
    }
}

fn elapsed_seconds(from: &ClockReading, to: &ClockReading) -> u64 {
    let start = Clock::started_at(from);
    (start.advanced_to(to).progress_ns() - start.progress_ns()) / SECOND_NS
}

/// A private root with the retention directory bootstrap makes, removed when the test ends.
struct Scratch {
    path: std::path::PathBuf,
    root: PrivateDirectory,
    retention: PrivateDirectory,
}

impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "flightdeck-retention-{}",
            collector::recording::identity_text(collector::recording::random_identity().unwrap())
        ));
        private::create_private_dir(&path).unwrap();
        let root = PrivateDirectory::open(&path).unwrap();
        let retention = root.create_directory(DIRECTORY_NAME).unwrap();

        Self {
            path,
            root,
            retention,
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.path).unwrap();
    }
}

fn bounds(newest_continuous_seconds: u64) -> TimeBounds {
    TimeBounds {
        min: 0,
        max: newest_continuous_seconds * SECOND_NS,
    }
}

#[test]
fn time_on_the_continuous_clock_is_time_within_a_boot() {
    // Goal: age within a boot counts elapsed time, sleep included.
    // Method: move the continuous clock, which counts sleep, and the wall clock together.
    let elapsed = elapsed_seconds(&reading(THIS_BOOT, 10, 0), &reading(THIS_BOOT, 70, 60));

    assert_eq!(elapsed, 60);
}

#[test]
fn a_wall_clock_stepping_forward_within_a_boot_ages_nothing() {
    // Goal: a clock set forward does not expire readings early.
    // Method: step the wall clock a year ahead while a minute passes.
    let elapsed = elapsed_seconds(
        &reading(THIS_BOOT, 10, 0),
        &reading(THIS_BOOT, 70, 365 * 86_400),
    );

    assert_eq!(elapsed, 60);
}

#[test]
fn a_wall_clock_stepping_back_within_a_boot_extends_nothing() {
    // Goal: a clock set back grants no reading more time.
    // Method: step the wall clock back a day while a minute passes, from a day after the start.
    let elapsed = elapsed_seconds(&reading(THIS_BOOT, 10, 86_400), &reading(THIS_BOOT, 70, 60));

    assert_eq!(elapsed, 60);
}

#[test]
fn a_frozen_wall_clock_stops_no_time() {
    // Goal: a wall clock that does not move cannot let a recording grow without end.
    // Method: hold the wall clock still while the continuous clock runs.
    let elapsed = elapsed_seconds(
        &reading(THIS_BOOT, 10, 500),
        &reading(THIS_BOOT, 3_610, 500),
    );

    assert_eq!(elapsed, 3_600);
}

#[test]
fn a_reboot_counts_the_time_the_wall_clock_says_passed() {
    // Goal: time away counts, though the continuous clock starts over.
    // Method: reboot an hour later on a continuous clock that reads less than before.
    let elapsed = elapsed_seconds(&reading(THIS_BOOT, 5_000, 0), &reading(NEXT_BOOT, 1, 3_600));

    assert_eq!(elapsed, 3_600);
}

#[test]
fn a_reboot_onto_an_earlier_clock_counts_no_time() {
    // Goal: a clock gone back across a reboot takes no time off and grants only the time it hides.
    // Method: reboot onto a wall clock a day earlier.
    let elapsed = elapsed_seconds(
        &reading(THIS_BOOT, 5_000, 86_400),
        &reading(NEXT_BOOT, 1, 0),
    );

    assert_eq!(elapsed, 0);
}

#[test]
fn a_continuous_clock_gone_back_within_a_boot_counts_no_time() {
    // Goal: an impossible reading from the kernel is no reason to stop or to wrap.
    // Method: read a continuous clock behind the saved one under the same boot.
    let elapsed = elapsed_seconds(&reading(THIS_BOOT, 70, 0), &reading(THIS_BOOT, 10, 60));

    assert_eq!(elapsed, 0);
}

#[test]
fn a_clock_keeps_its_value_across_a_save_and_a_restart() {
    // Goal: a restart within a boot measures from where the clock was.
    // Method: advance a clock, save it, and read it a minute later.
    let scratch = Scratch::new();
    let saved =
        Clock::started_at(&reading(THIS_BOOT, 10, 0)).advanced_to(&reading(THIS_BOOT, 70, 60));
    saved.write(&scratch.root).unwrap();

    let read = Clock::read(&scratch.root, &reading(THIS_BOOT, 130, 120));

    assert_eq!(read.progress_ns(), START_NS + 120 * SECOND_NS);
}

#[test]
fn a_saved_clock_measures_a_reboot_from_its_wall_clock() {
    // Goal: time away counts from when the record was saved.
    // Method: save a clock, and read it under another boot an hour after the record's wall clock.
    let scratch = Scratch::new();
    Clock::started_at(&reading(THIS_BOOT, 5_000, 60))
        .write(&scratch.root)
        .unwrap();

    let read = Clock::read(&scratch.root, &reading(NEXT_BOOT, 1, 3_660));

    assert_eq!(read.progress_ns(), START_NS + 3_660 * SECOND_NS);
}

#[test]
fn a_root_with_no_saved_clock_starts_at_the_wall_clock() {
    // Goal: a new recording needs nothing written before it can be read.
    // Method: read the clock of an empty root.
    let scratch = Scratch::new();

    let read = Clock::read(&scratch.root, &reading(THIS_BOOT, 10, 60));

    assert_eq!(read.progress_ns(), START_NS + 60 * SECOND_NS);
}

#[test]
fn a_damaged_clock_starts_over_and_stops_nothing() {
    // Goal: a save or a reader goes on past a record that does not read.
    // Method: write records of a wrong length and a wrong magic, and read the clock.
    let scratch = Scratch::new();

    private::atomic_write_in(&scratch.retention, CLOCK_NAME, b"short").unwrap();
    let short = Clock::read(&scratch.root, &reading(THIS_BOOT, 10, 60));
    private::atomic_write_in(&scratch.retention, CLOCK_NAME, &[7; CLOCK_SIZE]).unwrap();
    let wrong = Clock::read(&scratch.root, &reading(THIS_BOOT, 10, 60));

    assert_eq!(short.progress_ns(), START_NS + 60 * SECOND_NS);
    assert_eq!(wrong.progress_ns(), START_NS + 60 * SECOND_NS);
}

#[test]
fn a_reading_expires_once_older_than_the_period() {
    // Goal: the boundary is exact to the nanosecond, and the older side expires.
    // Method: read an hour's cutoff against readings on both sides of it.
    let period = Period::from_seconds(3_600).unwrap();
    let clock = Clock::started_at(&reading(THIS_BOOT, 0, 0));
    let cutoff = clock.cutoff(period);
    let exact = START_NS - 3_600 * SECOND_NS;

    assert!(cutoff.expired(exact - 1));
    assert!(!cutoff.expired(exact));
    assert!(!cutoff.expired(exact + 1));
}

#[test]
fn a_young_recording_has_nothing_expired() {
    // Goal: a clock smaller than the period, as after a reset, expires nothing.
    // Method: read a period longer than the clock has run.
    let clock = Clock {
        progress_ns: SECOND_NS,
        at: reading(THIS_BOOT, 0, 0),
    };

    let cutoff = clock.cutoff(Period::DEFAULT);

    assert!(!cutoff.expired(0));
}

#[test]
fn a_root_with_no_period_keeps_one_day() {
    // Goal: the default is one day.
    // Method: read the period of an empty root.
    let scratch = Scratch::new();

    let period = Period::read(&scratch.root);

    assert_eq!(period.seconds(), 86_400);
}

#[test]
fn a_saved_period_is_what_the_next_read_finds() {
    // Goal: one persistent policy.
    // Method: save a period and read it back.
    let scratch = Scratch::new();

    Period::from_seconds(600)
        .unwrap()
        .write(&scratch.root)
        .unwrap();

    assert_eq!(Period::read(&scratch.root).seconds(), 600);
}

#[test]
fn a_period_stays_within_its_bounds() {
    // Goal: no value can empty a recording or overflow a clock.
    // Method: ask for each bound and just past it.
    assert_eq!(Period::from_seconds(59), None);
    assert_eq!(Period::from_seconds(60).unwrap().seconds(), 60);
    assert_eq!(
        Period::from_seconds(Period::SECONDS_MAX).unwrap().seconds(),
        Period::SECONDS_MAX
    );
    assert_eq!(Period::from_seconds(Period::SECONDS_MAX + 1), None);
}

#[test]
fn a_period_that_does_not_read_keeps_the_default() {
    // Goal: a damaged file errs on keeping readings.
    // Method: write text, an out of bounds number and an overlong number, and read each.
    let scratch = Scratch::new();

    for text in ["soon", "1", "99999999999999999999999"] {
        private::atomic_write_in(&scratch.retention, PERIOD_NAME, text.as_bytes()).unwrap();

        assert_eq!(Period::read(&scratch.root), Period::DEFAULT, "{text}");
    }
}

#[test]
fn a_window_saved_late_keeps_the_time_of_its_newest_reading() {
    // Goal: seal lag ages nothing, and a window rebuilt after a crash is the same window.
    // Method: ask for the retained time of one window long after its reading, and again later.
    let scratch = Scratch::new();
    Clock::started_at(&reading(THIS_BOOT, 10, 0))
        .write(&scratch.root)
        .unwrap();

    let soon = retained_from_ns(
        &scratch.root,
        &reading(THIS_BOOT, 100, 90),
        THIS_BOOT,
        Some(bounds(40)),
    );
    let later = retained_from_ns(
        &scratch.root,
        &reading(THIS_BOOT, 500, 490),
        THIS_BOOT,
        Some(bounds(40)),
    );

    assert_eq!(soon.unwrap(), START_NS + 30 * SECOND_NS);
    assert_eq!(later.unwrap(), START_NS + 30 * SECOND_NS);
}

#[test]
fn a_window_rebuilt_on_a_root_with_no_clock_keeps_its_time() {
    // Goal: a wall clock read twice cannot give one window two times.
    // Method: with no saved clock, ask for one window's time under wall clocks a minute apart.
    let scratch = Scratch::new();

    let first = retained_from_ns(
        &scratch.root,
        &reading(THIS_BOOT, 100, 0),
        THIS_BOOT,
        Some(bounds(40)),
    );
    let second = retained_from_ns(
        &scratch.root,
        &reading(THIS_BOOT, 100, 60),
        THIS_BOOT,
        Some(bounds(40)),
    );

    assert_eq!(first.unwrap(), second.unwrap());
}

#[test]
fn a_window_of_an_earlier_boot_keeps_the_time_that_boot_gave_it() {
    // Goal: a reboot between a reading and its save changes nothing that boot measured.
    // Method: save a window in one boot, then ask for another window of that boot after a reboot.
    let scratch = Scratch::new();
    Clock::started_at(&reading(THIS_BOOT, 10, 0))
        .write(&scratch.root)
        .unwrap();
    retained_from_ns(
        &scratch.root,
        &reading(THIS_BOOT, 100, 90),
        THIS_BOOT,
        Some(bounds(40)),
    )
    .unwrap();

    let after_reboot = retained_from_ns(
        &scratch.root,
        &reading(NEXT_BOOT, 1, 3_600),
        THIS_BOOT,
        Some(bounds(50)),
    );

    assert_eq!(after_reboot.unwrap(), START_NS + 40 * SECOND_NS);
}

#[test]
fn a_window_of_a_boot_nothing_saved_in_counts_as_saved_now() {
    // Goal: a boot with no record has no measure of its readings, so they keep the whole period.
    // Method: ask for a window of a boot that never saved, under a later boot.
    let scratch = Scratch::new();
    Clock::started_at(&reading(THIS_BOOT, 10, 0))
        .write(&scratch.root)
        .unwrap();

    let retained = retained_from_ns(
        &scratch.root,
        &reading(THIS_BOOT, 110, 100),
        NEXT_BOOT,
        Some(bounds(9_000)),
    );

    assert_eq!(retained.unwrap(), START_NS + 100 * SECOND_NS);
}

#[test]
fn a_publication_with_no_clocks_takes_the_time_it_is_saved() {
    // Goal: readings without clocks, and kinds without readings, still expire.
    // Method: ask for the time of a publication with no clock bounds.
    let scratch = Scratch::new();
    Clock::started_at(&reading(THIS_BOOT, 10, 0))
        .write(&scratch.root)
        .unwrap();

    let retained = retained_from_ns(&scratch.root, &reading(THIS_BOOT, 70, 60), THIS_BOOT, None);

    assert_eq!(retained.unwrap(), START_NS + 60 * SECOND_NS);
}

#[test]
fn a_damaged_boot_record_stops_no_save() {
    // Goal: new minutes save past a record that does not read.
    // Method: damage a boot's record and ask for a window of that boot.
    let scratch = Scratch::new();
    Clock::started_at(&reading(THIS_BOOT, 10, 0))
        .write(&scratch.root)
        .unwrap();
    let name = format!("boot-{}", collector::recording::identity_text(THIS_BOOT));
    private::atomic_write_in(&scratch.retention, &name, b"short").unwrap();

    let retained = retained_from_ns(
        &scratch.root,
        &reading(THIS_BOOT, 70, 60),
        THIS_BOOT,
        Some(bounds(40)),
    );

    assert_eq!(retained.unwrap(), START_NS + 60 * SECOND_NS);
}

#[test]
fn an_order_entry_links_the_proof_under_the_hour_of_its_time() {
    // Goal: a pass finds the oldest publications by listing hours, and a repeat adds nothing.
    // Method: link one proof twice and read what the order holds.
    let scratch = Scratch::new();
    let attempt = scratch.root.create_directory("attempt").unwrap();
    let proof = attempt.create_file(crate::store::PROOF_NAME).unwrap();
    let entry = OrderEntry {
        retained_from_ns: 7_200 * SECOND_NS + 5,
        transaction: Identity([1, 2]),
        child: "ready",
    };

    entry.link(&scratch.root, &attempt, &mut 0).unwrap();
    entry.link(&scratch.root, &attempt, &mut 0).unwrap();

    let name = format!(
        "{}-{}-ready",
        7_200 * SECOND_NS + 5,
        collector::recording::identity_text(Identity([1, 2]))
    );
    assert_eq!(
        order_entry_names(&scratch.path),
        std::slice::from_ref(&name)
    );
    assert!(
        scratch
            .path
            .join(DIRECTORY_NAME)
            .join(ORDER_NAME)
            .join("2")
            .join(&name)
            .exists()
    );
    assert_eq!(
        std::os::unix::fs::MetadataExt::nlink(&proof.metadata().unwrap()),
        2
    );
}

fn linked_entry(scratch: &Scratch, retained_from_ns: u64, transaction: u64) {
    let attempt = scratch
        .root
        .create_directory(&format!("attempt-{transaction}"))
        .unwrap();
    attempt.create_file(crate::store::PROOF_NAME).unwrap();
    OrderEntry {
        retained_from_ns,
        transaction: Identity([1, transaction]),
        child: "ready",
    }
    .link(&scratch.root, &attempt, &mut 0)
    .unwrap();
}

fn cutoff_at(expired_below_ns: u64) -> Cutoff {
    Cutoff { expired_below_ns }
}

fn listed_times(scratch: &Scratch, cutoff: Cutoff, entries_max: usize) -> Vec<u64> {
    expired_entries(&scratch.root, cutoff, entries_max)
        .unwrap()
        .iter()
        .map(|entry| entry.retained_from_ns)
        .collect()
}

const HOUR_NS: u64 = 3_600 * SECOND_NS;

#[test]
fn the_listing_gives_what_expired_oldest_first() {
    // Goal: a pass removes the oldest first, and nothing at the cutoff or past it.
    // Method: link entries across hours in another order, and list at a cutoff inside an hour.
    let scratch = Scratch::new();
    for (transaction, retained) in [
        (1, 3 * HOUR_NS),
        (2, HOUR_NS + 5),
        (3, 2 * HOUR_NS),
        (4, HOUR_NS),
        (5, 3 * HOUR_NS + 2),
    ] {
        linked_entry(&scratch, retained, transaction);
    }

    let listed = listed_times(&scratch, cutoff_at(3 * HOUR_NS + 2), 16);

    assert_eq!(listed, [HOUR_NS, HOUR_NS + 5, 2 * HOUR_NS, 3 * HOUR_NS]);
}

#[test]
fn the_listing_stops_at_its_bound() {
    // Goal: a pass costs its bound however much has expired, in a dense hour as across hours.
    // Method: list fewer entries than one hour holds, then fewer than the next hours hold.
    let scratch = Scratch::new();
    for transaction in 1..=3 {
        linked_entry(&scratch, transaction, transaction);
    }
    linked_entry(&scratch, HOUR_NS, 4);
    linked_entry(&scratch, 2 * HOUR_NS, 5);

    assert_eq!(listed_times(&scratch, cutoff_at(u64::MAX), 2).len(), 2);
    assert_eq!(
        listed_times(&scratch, cutoff_at(u64::MAX), 4),
        [1, 2, 3, HOUR_NS]
    );
}

#[test]
fn the_listing_passes_over_names_no_link_made() {
    // Goal: a stray name reports and cannot stop expiry.
    // Method: put a stray hour and a stray entry beside a real one.
    let scratch = Scratch::new();
    linked_entry(&scratch, 7, 1);
    let order = order_directory(&scratch.root).unwrap();
    order.create_directory("stray").unwrap();
    order
        .open_directory("0")
        .unwrap()
        .create_file("stray")
        .unwrap();

    assert_eq!(listed_times(&scratch, cutoff_at(8), 16), [7]);
}

#[test]
fn removing_an_entry_takes_the_hour_it_empties() {
    // Goal: 30 days of hours leave no directory behind, and a repeat after a crash is done.
    // Method: remove each of two entries of an hour, and the second again.
    let scratch = Scratch::new();
    linked_entry(&scratch, 7, 1);
    linked_entry(&scratch, 8, 2);
    let listed = expired_entries(&scratch.root, cutoff_at(9), 16).unwrap();
    let hours = || order_directory(&scratch.root).unwrap();

    listed[0].remove(&scratch.root).unwrap();
    assert!(hours().open_directory("0").is_ok());

    listed[1].remove(&scratch.root).unwrap();
    let empty_hour = hours().open_directory("0").err().unwrap();
    assert_eq!(empty_hour.kind(), io::ErrorKind::NotFound);
}

#[test]
fn an_entry_names_the_raw_data_a_proof_that_does_not_read_cannot() {
    // Goal: an unreadable proof still gives up its raw data.
    // Method: read the names back from a linked entry.
    let scratch = Scratch::new();
    linked_entry(&scratch, 7, 1);
    let text = collector::recording::identity_text(Identity([1, 1])).to_string();

    let listed = expired_entries(&scratch.root, cutoff_at(8), 16).unwrap();

    assert_eq!(listed[0].raw_names(), Some((text.as_str(), "ready")));
}

fn retained_at_ten_seconds() -> Retained {
    Retained {
        cutoff: Cutoff {
            expired_below_ns: 10 * SECOND_NS,
        },
        offsets: vec![(THIS_BOOT, 3 * SECOND_NS)],
    }
}

fn clock_bounds(first_seconds: u64, last_seconds: u64) -> TimeBounds {
    TimeBounds {
        min: first_seconds * SECOND_NS,
        max: last_seconds * SECOND_NS,
    }
}

#[test]
fn a_reading_at_the_cutoff_is_retained_and_one_before_it_is_not() {
    // Goal: the boundary falls exactly where the period ends, on the reading's own clock.
    // Method: place readings either side of a cutoff that their boot's offset puts at 7 seconds.
    let retained = retained_at_ten_seconds();

    assert!(retained.holds(THIS_BOOT, Some(7 * SECOND_NS)));
    assert!(!retained.holds(THIS_BOOT, Some(7 * SECOND_NS - 1)));
    assert_eq!(retained.expired_by_ns(THIS_BOOT, 7 * SECOND_NS), 0);
    assert_eq!(retained.expired_by_ns(THIS_BOOT, 6 * SECOND_NS), SECOND_NS);
}

#[test]
fn a_reading_with_no_place_on_the_clock_stays() {
    // Goal: a reading with no clock, or of a boot with no record, waits for physical removal,
    // which is the visible limit on unknown time.
    // Method: ask about each, however old a clock would make it.
    let retained = retained_at_ten_seconds();

    assert!(retained.holds(THIS_BOOT, None));
    assert!(retained.holds(NEXT_BOOT, Some(0)));
    assert_eq!(retained.expired_by_ns(NEXT_BOOT, 0), 0);
}

#[test]
fn a_window_is_retained_whole_only_when_its_oldest_reading_is() {
    // Goal: a derived table that adds up a window is read only where nothing in it has expired.
    // Method: ask about windows either side of the cutoff, across it, and with no bounds.
    let retained = retained_at_ten_seconds();

    assert!(retained.holds_every(THIS_BOOT, Some(clock_bounds(7, 9))));
    assert!(!retained.holds_every(THIS_BOOT, Some(clock_bounds(6, 9))));
    assert!(!retained.holds_every(THIS_BOOT, None));
    assert!(retained.holds_every(NEXT_BOOT, None));
}

#[test]
fn a_window_holds_something_to_return_until_its_newest_reading_expires() {
    // Goal: a window wholly past the cutoff is never read, and physical removal may take it.
    // Method: ask about windows either side of the cutoff, across it, and with no bounds.
    let retained = retained_at_ten_seconds();

    assert!(retained.holds_any(THIS_BOOT, Some(clock_bounds(6, 7))));
    assert!(!retained.holds_any(THIS_BOOT, Some(clock_bounds(5, 6))));
    assert!(retained.holds_any(THIS_BOOT, None));
}

#[test]
fn a_boot_record_that_does_not_read_hides_nothing() {
    // Goal: damage in a record costs the cutoff and no reading, and the reader goes on.
    // Method: leave a record of the wrong size, and read what the root retains.
    let scratch = Scratch::new();
    Clock::started_at(&reading(NEXT_BOOT, 0, 0))
        .write(&scratch.root)
        .unwrap();
    private::atomic_write_in(&scratch.retention, &boot_record_name(THIS_BOOT), b"short").unwrap();

    let retained = Retained::read(&scratch.path, [THIS_BOOT]);

    assert!(retained.holds(THIS_BOOT, Some(0)));
}

#[test]
fn a_boot_is_asked_about_once_however_many_windows_name_it() {
    // Goal: a read of many windows reads each boot's record once.
    // Method: read what the root retains for one boot named three times.
    let scratch = Scratch::new();
    Clock::started_at(&reading(NEXT_BOOT, 0, 0))
        .write(&scratch.root)
        .unwrap();
    boot_offset_save(&scratch.root, THIS_BOOT, SECOND_NS).unwrap();

    let retained = Retained::read(&scratch.path, [THIS_BOOT, THIS_BOOT, THIS_BOOT]);

    assert_eq!(retained.offsets, [(THIS_BOOT, SECOND_NS)]);
}

#[test]
fn a_clock_no_persister_has_saved_hides_nothing() {
    // Goal: nothing expires before the retention clock has started, however long the Mac has been
    // up, since a boot's offset alone places an early reading far in the past.
    // Method: save a boot's offset and no clock, and ask about a reading the offset puts at zero.
    let scratch = Scratch::new();
    boot_offset_save(&scratch.root, THIS_BOOT, 0).unwrap();

    let retained = Retained::read(&scratch.path, [THIS_BOOT]);

    assert!(retained.holds(THIS_BOOT, Some(0)));
}
