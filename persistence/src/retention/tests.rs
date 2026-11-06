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

fn scratch_root() -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!(
        "flightdeck-retention-{}",
        collector::recording::identity_text(collector::recording::random_identity().unwrap())
    ));
    collector::recording::private::create_private_dir(&root).unwrap();
    root
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
    // Goal: a clock set forward does not expire readings early (P03.4).
    // Method: step the wall clock a year ahead while a minute passes.
    let elapsed = elapsed_seconds(
        &reading(THIS_BOOT, 10, 0),
        &reading(THIS_BOOT, 70, 365 * 86_400),
    );

    assert_eq!(elapsed, 60);
}

#[test]
fn a_wall_clock_stepping_back_within_a_boot_extends_nothing() {
    // Goal: a clock set back grants no reading more time (P03.4).
    // Method: step the wall clock back a day while a minute passes, from a day after the start.
    let elapsed = elapsed_seconds(&reading(THIS_BOOT, 10, 86_400), &reading(THIS_BOOT, 70, 60));

    assert_eq!(elapsed, 60);
}

#[test]
fn a_frozen_wall_clock_stops_no_time() {
    // Goal: a wall clock that does not move cannot let a recording grow without end (P03.3).
    // Method: hold the wall clock still while the continuous clock runs.
    let elapsed = elapsed_seconds(
        &reading(THIS_BOOT, 10, 500),
        &reading(THIS_BOOT, 3_610, 500),
    );

    assert_eq!(elapsed, 3_600);
}

#[test]
fn a_reboot_counts_the_time_the_wall_clock_says_passed() {
    // Goal: time away counts, though the continuous clock starts over (P03.4).
    // Method: reboot an hour later on a continuous clock that reads less than before.
    let elapsed = elapsed_seconds(&reading(THIS_BOOT, 5_000, 0), &reading(NEXT_BOOT, 1, 3_600));

    assert_eq!(elapsed, 3_600);
}

#[test]
fn a_reboot_onto_an_earlier_clock_counts_no_time() {
    // Goal: a clock gone back across a reboot takes no time off and grants only the time it hides
    // (P03.4).
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
    // Goal: a restart within a boot measures from where the clock was (P03.4).
    // Method: advance a clock, save it, and read it a minute later.
    let root = scratch_root();
    let saved =
        Clock::started_at(&reading(THIS_BOOT, 10, 0)).advanced_to(&reading(THIS_BOOT, 70, 60));
    saved.write(&root).unwrap();

    let read = Clock::read(&root, &reading(THIS_BOOT, 130, 120));

    assert_eq!(read.progress_ns(), START_NS + 120 * SECOND_NS);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_saved_clock_measures_a_reboot_from_its_wall_clock() {
    // Goal: time away counts from when the record was saved (P03.4).
    // Method: save a clock, and read it under another boot an hour after the record's wall clock.
    let root = scratch_root();
    Clock::started_at(&reading(THIS_BOOT, 5_000, 60))
        .write(&root)
        .unwrap();

    let read = Clock::read(&root, &reading(NEXT_BOOT, 1, 3_660));

    assert_eq!(read.progress_ns(), START_NS + 3_660 * SECOND_NS);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_root_with_no_saved_clock_starts_at_the_wall_clock() {
    // Goal: a new recording needs nothing written before it can be read.
    // Method: read the clock of an empty root.
    let root = scratch_root();

    let read = Clock::read(&root, &reading(THIS_BOOT, 10, 60));

    assert_eq!(read.progress_ns(), START_NS + 60 * SECOND_NS);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_damaged_clock_starts_over_and_stops_nothing() {
    // Goal: a save or a reader goes on past a record that does not read.
    // Method: write records of a wrong length and a wrong magic, and read the clock.
    let root = scratch_root();

    collector::recording::private::atomic_write(&root, CLOCK_NAME, b"short").unwrap();
    let short = Clock::read(&root, &reading(THIS_BOOT, 10, 60));
    collector::recording::private::atomic_write(&root, CLOCK_NAME, &[7; CLOCK_SIZE]).unwrap();
    let wrong = Clock::read(&root, &reading(THIS_BOOT, 10, 60));

    assert_eq!(short.progress_ns(), START_NS + 60 * SECOND_NS);
    assert_eq!(wrong.progress_ns(), START_NS + 60 * SECOND_NS);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_reading_expires_once_older_than_the_period() {
    // Goal: the boundary is exact to the nanosecond, and the older side expires (P03.1).
    // Method: read an hour's cutoff against readings on both sides of it.
    let period = Period::from_seconds(3_600).unwrap();
    let clock = Clock::started_at(&reading(THIS_BOOT, 0, 0));
    let cutoff = clock.cutoff(period);
    let exact = START_NS - 3_600 * SECOND_NS;

    assert!(cutoff.expired(exact - 1, 0));
    assert!(!cutoff.expired(exact, 0));
    assert!(!cutoff.expired(exact + 1, 0));
}

#[test]
fn a_reading_takes_its_age_from_how_far_behind_its_window_newest_it_was() {
    // Goal: a partly expired minute expires reading by reading (P03.1).
    // Method: compare readings a second apart in one window whose newest sits on the cutoff.
    let period = Period::from_seconds(3_600).unwrap();
    let cutoff = Clock::started_at(&reading(THIS_BOOT, 0, 0)).cutoff(period);
    let newest = START_NS - 3_600 * SECOND_NS;

    assert!(!cutoff.expired(newest, 0));
    assert!(cutoff.expired(newest, SECOND_NS));
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

    assert!(!cutoff.expired(0, 0));
}

#[test]
fn a_root_with_no_period_keeps_one_day() {
    // Goal: the default is one day (P03.1).
    // Method: read the period of an empty root.
    let root = scratch_root();

    let period = Period::read(&root);

    assert_eq!(period.seconds(), 86_400);
    std::fs::remove_dir_all(root).unwrap();
}

#[test]
fn a_saved_period_is_what_the_next_read_finds() {
    // Goal: one persistent policy (P03.1).
    // Method: save a period and read it back.
    let root = scratch_root();

    Period::from_seconds(600).unwrap().write(&root).unwrap();

    assert_eq!(Period::read(&root).seconds(), 600);
    std::fs::remove_dir_all(root).unwrap();
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
    // Goal: a damaged file errs on keeping readings (P03.1).
    // Method: write text, an out of bounds number and an overlong number, and read each.
    let root = scratch_root();

    for text in ["soon", "1", "99999999999999999999999"] {
        collector::recording::private::atomic_write(&root, PERIOD_NAME, text.as_bytes()).unwrap();

        assert_eq!(Period::read(&root), Period::DEFAULT, "{text}");
    }
    std::fs::remove_dir_all(root).unwrap();
}
