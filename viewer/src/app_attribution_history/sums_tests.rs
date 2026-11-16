use super::Total;
use crate::live_app_response::Measure;

const SECOND_NS: u64 = 1_000_000_000;
const AMOUNT: u64 = 7;

fn measured() -> Measure {
    Measure {
        known: AMOUNT,
        measured_members: 1,
        unavailable_members: 0,
        overflowed: false,
    }
}

#[test]
fn a_measured_reading_adds_its_amount_and_the_length_of_its_second() {
    // Goal: a measured reading of a second whose clock moved counts toward the App's mean.
    // Method: take what one such reading adds and compare it whole.
    let total = Total::of_reading(&measured(), Some(SECOND_NS));

    let expected = Total {
        amount: u128::from(AMOUNT),
        elapsed_ns: u128::from(SECOND_NS),
    };
    assert_eq!(total, expected);
}

#[test]
fn a_reading_of_a_second_with_no_elapsed_time_adds_nothing() {
    // Goal: a second whose length is unknown counts toward no mean.
    // Method: take what a measured reading adds over no elapsed time.
    let total = Total::of_reading(&measured(), None);

    assert_eq!(total, Total::default());
}

#[test]
fn a_reading_of_a_second_whose_clock_stood_still_adds_nothing() {
    // Goal: a second of no length counts toward no mean, so no mean divides by it.
    // Method: take what a measured reading adds over an elapsed time of zero.
    let total = Total::of_reading(&measured(), Some(0));

    assert_eq!(total, Total::default());
}

#[test]
fn a_reading_with_no_measured_member_adds_nothing() {
    // Goal: an amount no process of the App had measured leaves the mean as it was, and
    // does not lengthen the time the mean divides by.
    // Method: take what a reading whose only member is unavailable adds over one second.
    let unmeasured = Measure {
        measured_members: 0,
        unavailable_members: 1,
        ..measured()
    };

    let total = Total::of_reading(&unmeasured, Some(SECOND_NS));

    assert_eq!(total, Total::default());
}

#[test]
fn an_overflowed_reading_adds_nothing() {
    // Goal: an amount that overflowed is not known, so it leaves the mean as it was.
    // Method: take what a measured reading marked overflowed adds over one second.
    let overflowed = Measure {
        overflowed: true,
        ..measured()
    };

    let total = Total::of_reading(&overflowed, Some(SECOND_NS));

    assert_eq!(total, Total::default());
}
