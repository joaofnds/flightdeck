use super::super::{empty_panels, panels};
use super::overview_panels;
use crate::live_store_fixtures::{levels, reading_at, selection, view};

#[test]
fn a_view_draws_the_overview_the_trend_points_of_its_seconds_draw() {
    // Goal: the Overview draws the same charts from the seconds the store holds as from the
    // trend points of those seconds. Method: older seconds, live seconds older than the
    // full-detail ones, and full-detail seconds across a break, some readings missing, drawn
    // both ways.
    let earlier = [reading_at(6_345, 0), reading_at(7_001, 2)];
    let older = [reading_at(9_020, 3), reading_at(9_030, 3)];
    let mut detail = [
        reading_at(9_041, 3),
        reading_at(9_042, 3),
        reading_at(9_500, 4),
        reading_at(10_000, 4),
    ];
    detail[1].cpu_percent = None;
    detail[1].memory = None;
    detail[2].network = None;
    detail[2].thermal_pressure = None;
    let view = view(1, &earlier, &[older.as_slice(), &detail].concat());
    let expected = panels(
        "overview",
        &selection(&[earlier, older].concat(), &detail),
        None,
    );

    let drawn = overview_panels(&view, levels().as_ref());

    assert_eq!(drawn.unwrap(), expected.unwrap());
}

mod when_seconds_are_older_than_the_full_detail_ones {
    use super::*;

    #[test]
    fn a_second_of_a_stretch_already_drawn_is_left_out() {
        // Goal: an older stretch draws one reading, as the trend points keep one, even where the
        // store's older seconds and its live span both hold seconds of it. Method: a stretch
        // whose first second is an older second and whose next ones are live, and a live
        // stretch of two seconds.
        let earlier = [reading_at(9_020, 3)];
        let live = [
            reading_at(9_025, 3),
            reading_at(9_030, 3),
            reading_at(9_031, 3),
            reading_at(9_041, 3),
            reading_at(10_000, 3),
        ];
        let view = view(1, &earlier, &live);
        let kept = [reading_at(9_020, 3), reading_at(9_030, 3)];
        let expected = panels("overview", &selection(&kept, &live[3..]), None);

        let drawn = overview_panels(&view, levels().as_ref());

        assert_eq!(drawn.unwrap(), expected.unwrap());
    }

    #[test]
    fn a_second_of_the_stretch_of_the_first_full_detail_second_is_left_out() {
        // Goal: the older tail ends where the stretch that holds the first full-detail second
        // starts, so that stretch draws its full-detail seconds and nothing before them.
        // Method: a live second in that stretch one second before the first full-detail
        // second, which is 9,041.
        let live = [
            reading_at(9_030, 3),
            reading_at(9_040, 3),
            reading_at(9_041, 3),
            reading_at(10_000, 3),
        ];
        let view = view(1, &[], &live);
        let expected = panels("overview", &selection(&live[..1], &live[2..]), None);

        let drawn = overview_panels(&view, levels().as_ref());

        assert_eq!(drawn.unwrap(), expected.unwrap());
    }

    #[test]
    fn a_second_before_the_loaded_hour_is_left_out() {
        // Goal: the charts span the loaded hour and its lead-in, so an older second the store
        // still holds from before them draws nothing. Method: an older second one second
        // before the first second the charts hold, which is 3,659 seconds before the newest.
        let earlier = [reading_at(6_340, 0), reading_at(6_345, 0)];
        let live = [reading_at(9_041, 1), reading_at(10_000, 1)];
        let view = view(1, &earlier, &live);
        let expected = panels("overview", &selection(&earlier[1..], &live), None);

        let drawn = overview_panels(&view, levels().as_ref());

        assert_eq!(drawn.unwrap(), expected.unwrap());
    }
}

mod when_the_store_holds_no_second {
    use super::*;

    #[test]
    fn the_overview_draws_its_charts_without_readings() {
        // Goal: before the store holds a second, the page lays out the charts its first second
        // fills, so that second moves nothing. Method: a View with no second.
        let view = view(1, &[], &[]);

        let drawn = overview_panels(&view, None);

        assert_eq!(drawn.unwrap(), empty_panels("overview").unwrap());
    }
}
