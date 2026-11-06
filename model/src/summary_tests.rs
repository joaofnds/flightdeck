use super::{CategorySummary, Extreme, GaugeSummary, Mark, RateSummary, fixed_from_value};

fn gauge_of(readings: &[(Option<f64>, bool)], first_place: u32) -> GaugeSummary<u32> {
    let mut summary = GaugeSummary::default();
    for (place, (value, follows)) in (first_place..).zip(readings) {
        summary.push(*value, *follows, place);
    }

    summary
}

fn rate_of(readings: &[(Option<(f64, u64)>, bool)], first_place: u32) -> RateSummary<u32> {
    let mut summary = RateSummary::default();
    for (place, (rate, follows)) in (first_place..).zip(readings) {
        summary.push(*rate, *follows, place);
    }

    summary
}

type Thermal = CategorySummary<u8, u32, 3>;

fn thermal_of(readings: &[(Option<u8>, bool)], first_place: u32) -> Thermal {
    let mut summary = Thermal::default();
    for (place, (category, follows)) in (first_place..).zip(readings) {
        summary.push(*category, *follows, place);
    }

    summary
}

/// A small fixed-seed generator, so a failing case repeats.
struct Generator(u64);

impl Generator {
    fn next_below(&mut self, bound: u64) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);

        (self.0 >> 33) % bound
    }

    /// A reading of few distinct values, so ties are common, one in eight missing.
    fn value(&mut self) -> Option<f64> {
        match self.next_below(8) {
            0 => None,
            _ => Some(f64::from(u32::try_from(self.next_below(5)).unwrap()) * 0.1 - 0.2),
        }
    }

    fn follows(&mut self) -> bool {
        self.next_below(6) != 0
    }

    /// Three places that cut `length` readings into four parts, in order.
    fn cuts(&mut self, length: usize) -> [usize; 3] {
        let bound = u64::try_from(length + 1).unwrap();
        let mut cuts = [0_u64; 3].map(|_| usize::try_from(self.next_below(bound)).unwrap());
        cuts.sort_unstable();

        cuts
    }
}

#[test]
fn a_gauge_finds_a_reading_that_lasted_a_second_among_lower_ones() {
    // Goal: the one reading that spiked names its value and its place, whatever the mean says.
    // Method: reduce ten readings of which the fourth is far above the rest.
    let readings = [10.0, 10.0, 10.0, 95.0, 10.0, 10.0, 10.0, 10.0, 10.0, 10.0]
        .map(|value| (Some(value), true));

    let summary = gauge_of(&readings, 0);

    let measured = summary.measured.unwrap();
    assert_eq!(
        measured.maximum,
        Extreme {
            value: 95.0,
            place: 3
        }
    );
    assert_eq!(
        measured.minimum,
        Extreme {
            value: 10.0,
            place: 0
        }
    );
    assert_eq!(
        measured.last,
        Extreme {
            value: 10.0,
            place: 9
        }
    );
    assert_eq!(measured.count, 10);
    assert!((summary.mean().unwrap() - 18.5).abs() < 0.001);
}

#[test]
fn a_reading_of_zero_is_a_value_and_a_missing_reading_is_not() {
    // Goal: a gap is never read as zero, and zero is never read as a gap.
    // Method: reduce a zero beside a missing reading and a reading that is not finite.
    let readings = [(Some(0.0), true), (None, true), (Some(f64::NAN), true)];

    let summary = gauge_of(&readings, 0);

    let measured = summary.measured.unwrap();
    assert_eq!(measured.count, 1);
    assert_eq!(
        measured.maximum,
        Extreme {
            value: 0.0,
            place: 0
        }
    );
    assert_eq!(summary.missing, 2);
}

#[test]
fn a_reduction_of_no_reading_has_no_mean() {
    // Goal: a summary with nothing in it states no value.
    // Method: reduce only missing readings.
    let summary = gauge_of(&[(None, true)], 0);

    assert_eq!(summary.measured, None);
    assert_eq!(summary.mean(), None);
}

#[test]
fn a_reading_that_does_not_follow_counts_as_a_break_with_or_without_a_value() {
    // Goal: a reader can tell that a line must not cross these readings.
    // Method: reduce a first reading, one after a gap with a value, and one after a gap without.
    let readings = [(Some(1.0), true), (Some(2.0), false), (None, false)];

    assert_eq!(gauge_of(&readings, 0).breaks, 2);
}

#[test]
fn a_rate_counts_in_its_mean_for_as_long_as_its_interval_lasted() {
    // Goal: a long interval weighs more than a short one, as the counter's growth does, where a
    // gauge's readings each count the same.
    // Method: reduce a rate over one second and a rate over three seconds.
    let second = 1_000_000_000;
    let readings = [
        (Some((100.0, second)), true),
        (Some((0.0, 3 * second)), true),
    ];

    let summary = rate_of(&readings, 0);

    assert!((summary.mean().unwrap() - 25.0).abs() < 0.001);
    assert_eq!(summary.covered_ns(), Some(4 * i128::from(second)));
}

#[test]
fn a_rate_over_no_interval_is_missing() {
    // Goal: no division by a time that did not pass.
    // Method: reduce a rate with no elapsed time and a reading with no rate.
    let readings = [(Some((5.0, 0)), true), (None, true)];

    let summary = rate_of(&readings, 0);

    assert_eq!(summary.measured, None);
    assert_eq!(summary.missing, 2);
}

#[test]
fn a_reading_past_any_quantity_the_mac_reports_is_missing() {
    // Goal: a corrupt reading cannot outgrow the total and take every other reading's mean with it.
    // Method: reduce the largest float beside an ordinary reading.
    let summary = gauge_of(&[(Some(f64::MAX), true), (Some(4.0), true)], 0);

    assert_eq!(summary.missing, 1);
    assert_eq!(summary.mean(), Some(4.0));
}

#[test]
fn a_mean_keeps_a_negative_reading_negative() {
    // Goal: a battery that discharges keeps its sign through the total.
    // Method: reduce two negative readings.
    let summary = gauge_of(&[(Some(-6.0), true), (Some(-2.0), true)], 0);

    assert_eq!(summary.mean(), Some(-4.0));
}

#[test]
fn a_reading_too_small_for_the_total_still_counts_as_a_value() {
    // Goal: a tiny reading is measured, though it adds nothing the total can count.
    // Method: reduce a reading below the total's finest unit.
    assert_eq!(fixed_from_value(1e-300), Some(0));
    assert_eq!(gauge_of(&[(Some(1e-300), true)], 0).mean(), Some(0.0));
}

#[test]
fn a_category_keeps_each_change_and_the_highest_with_their_places() {
    // Goal: a brief change of thermal state stays visible where no mean could show it.
    // Method: reduce readings that rise for one reading and fall back.
    let readings = [0, 0, 2, 0, 0].map(|category| (Some(category), true));

    let summary = thermal_of(&readings, 0);

    let changes: Vec<_> = summary.changes().copied().collect();
    let mark = |category, place| Mark { category, place };
    assert_eq!(changes, [mark(2, 2), mark(0, 3)]);
    assert_eq!(summary.highest, Some(mark(2, 2)));
    assert_eq!(summary.first, Some(mark(0, 0)));
    assert_eq!(summary.last, Some(mark(0, 4)));
}

#[test]
fn a_category_counts_the_changes_it_could_not_keep() {
    // Goal: the size of a summary stays fixed however often a category changes, and a reader knows
    // what was left out.
    // Method: reduce more changes than the summary keeps.
    let readings = [0, 1, 0, 1, 0, 1].map(|category| (Some(category), true));

    let summary = thermal_of(&readings, 0);

    assert_eq!(summary.changes().count(), 3);
    assert_eq!(summary.change_count, 5);
}

#[test]
fn merging_gauges_cut_anywhere_equals_reducing_their_readings() {
    // Goal: a stream reduced in any partition gives what a batch reduction gives, for every
    // statistic and with ties, gaps and breaks among the readings.
    // Method: generate readings, reduce them whole, and merge the reductions of four parts.
    let mut generator = Generator(1);
    for _case in 0..2_000 {
        let length = usize::try_from(generator.next_below(40)).unwrap();
        let readings: Vec<_> = (0..length)
            .map(|_| (generator.value(), generator.follows()))
            .collect();
        let [one, two, three] = generator.cuts(length);

        let mut merged = gauge_of(&readings[..one], 0);
        for (start, end) in [(one, two), (two, three), (three, length)] {
            let first_place = u32::try_from(start).unwrap();
            merged.merge(&gauge_of(&readings[start..end], first_place));
        }

        assert_eq!(
            merged,
            gauge_of(&readings, 0),
            "{readings:?} cut at {one} {two} {three}"
        );
    }
}

#[test]
fn merging_rates_cut_anywhere_equals_reducing_their_readings() {
    // Goal: rates, whose mean weighs each by its interval, keep the same property as gauges.
    // Method: generate rates over varied intervals, reduce them whole, and merge four parts.
    let mut generator = Generator(2);
    for _case in 0..2_000 {
        let length = usize::try_from(generator.next_below(40)).unwrap();
        let readings: Vec<_> = (0..length)
            .map(|_| {
                let elapsed_ns = generator.next_below(4) * 500_000_000;
                (
                    generator.value().map(|rate| (rate, elapsed_ns)),
                    generator.follows(),
                )
            })
            .collect();
        let [one, two, three] = generator.cuts(length);

        let mut merged = rate_of(&readings[..one], 0);
        for (start, end) in [(one, two), (two, three), (three, length)] {
            let first_place = u32::try_from(start).unwrap();
            merged.merge(&rate_of(&readings[start..end], first_place));
        }

        assert_eq!(
            merged,
            rate_of(&readings, 0),
            "{readings:?} cut at {one} {two} {three}"
        );
    }
}

#[test]
fn merging_categories_cut_anywhere_equals_reducing_their_readings() {
    // Goal: a change that falls on a cut is found as it is anywhere else, kept or counted.
    // Method: generate categories, reduce them whole, and merge the reductions of four parts.
    let mut generator = Generator(3);
    for _case in 0..2_000 {
        let length = usize::try_from(generator.next_below(40)).unwrap();
        let readings: Vec<_> = (0..length)
            .map(|_| {
                let category = match generator.next_below(8) {
                    0 => None,
                    _ => Some(u8::try_from(generator.next_below(4)).unwrap()),
                };
                (category, generator.follows())
            })
            .collect();
        let [one, two, three] = generator.cuts(length);

        let mut merged = thermal_of(&readings[..one], 0);
        for (start, end) in [(one, two), (two, three), (three, length)] {
            let first_place = u32::try_from(start).unwrap();
            merged.merge(&thermal_of(&readings[start..end], first_place));
        }

        assert_eq!(
            merged,
            thermal_of(&readings, 0),
            "{readings:?} cut at {one} {two} {three}"
        );
    }
}

#[test]
fn a_summary_keeps_the_earliest_of_readings_that_tie() {
    // Goal: a peak two readings share names the first of them, so a reduction and a merge of the
    // same readings name the same reading.
    // Method: two equal readings of a gauge and of a category, at different places.
    let mut gauge = GaugeSummary::<u32>::default();
    let mut thermal = Thermal::default();

    gauge.push(Some(5.0), true, 1);
    gauge.push(Some(5.0), true, 2);
    thermal.push(Some(3), true, 1);
    thermal.push(Some(3), true, 2);

    let measured = gauge.measured.unwrap();
    assert_eq!(measured.maximum.place, 1);
    assert_eq!(measured.minimum.place, 1);
    assert_eq!(thermal.highest.unwrap().place, 1);
}
