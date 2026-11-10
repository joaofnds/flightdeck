//! The newest end of a sliding chart's line, which draws each reading once, at the end of its
//! own second, and then only scrolls it left.
use gpui::{Bounds, point, px, size};

use super::*;

/// The width of an overview card in the 1320-pixel window.
const CARD_WIDTH: f32 = 360.0;
/// The steady reading before the burst, and the burst's.
const STEADY: f32 = 10.0;
const BURST: f32 = 100.0;
/// The second the burst's first reading is taken in, the newest a chart shows.
const BURST_SECOND: u64 = 2000;

#[test]
fn a_burst_draws_its_first_reading_at_the_right_edge_of_a_five_minute_chart() {
    // Goal: the frame after a burst's first 100% reading draws it at the top right corner, so
    // the line passes 70% at its publication. Method: chart five minutes of a steady 10% ending in
    // one 100% reading, and read where the newest point is drawn.
    assert_eq!(newest_point_after_a_burst(300), (CARD_WIDTH, 0.0));
}

#[test]
fn a_burst_draws_its_first_reading_at_the_right_edge_of_a_fifteen_minute_chart() {
    // Goal: as at five minutes, at fifteen, where the card has fewer pixels than readings.
    // Method: as at five minutes, over fifteen.
    assert_eq!(newest_point_after_a_burst(900), (CARD_WIDTH, 0.0));
}

#[test]
fn each_point_is_one_reading_at_the_end_of_its_second() {
    // Goal: a sliding chart draws every reading in view as its own point, at the end of the
    // second it was taken in, so a point is a measurement and no later reading moves it. Method:
    // chart 15 minutes of readings that differ from second to second in a card 900 pixels wide,
    // one pixel a second and one pixel a percent, and compare its points with the readings.
    let span = 900_u16;
    let first = 2000;
    let newest = first + u64::from(span) - 1;
    let reading = |second: u64| f32::from(u16::try_from(second * 37 % 101).unwrap());
    let positions = tick_positions(span);
    let values = readings(first, newest, reading);
    let geometry = sliding_geometry(first, span, 900.0, &positions);

    let curves = run_curves(&values, 0..values.len(), &geometry);

    let drawn: Vec<(f32, f32)> = curves
        .iter()
        .flat_map(|curve| curve.points.iter().map(|point| drawn_at(point.top)))
        .collect();
    let expected: Vec<(f32, f32)> = (first..=newest)
        .map(|second| {
            let pixel = u16::try_from(second + 1 - first).unwrap();
            (f32::from(pixel), 100.0 - reading(second))
        })
        .collect();
    assert_eq!(drawn, expected);
}

/// Where the newest point of a chart over `span` seconds of a steady reading ending in one
/// burst reading is drawn, in pixels from the card's top left corner.
fn newest_point_after_a_burst(span: u16) -> (f32, f32) {
    let first = BURST_SECOND + 1 - u64::from(span);
    let positions = tick_positions(span);
    let values = readings(first, BURST_SECOND, |second| {
        if second < BURST_SECOND { STEADY } else { BURST }
    });
    let geometry = sliding_geometry(first, span, CARD_WIDTH, &positions);

    let curves = run_curves(&values, 0..values.len(), &geometry);

    let newest = curves.last().and_then(|curve| curve.points.last());
    drawn_at(newest.expect("the chart drew a point").top)
}

/// A drawn point's place in pixels, to the thousandth of a pixel, which leaves out the rounding
/// of the fractions of the window the chart computes it from.
fn drawn_at(place: gpui::Point<Pixels>) -> (f32, f32) {
    let thousandths = |pixels: Pixels| (f32::from(pixels) * 1000.0).round() / 1000.0;
    (thousandths(place.x), thousandths(place.y))
}

/// The reading `reading` gives each second from `first` to `newest`.
fn readings(first: u64, newest: u64, reading: impl Fn(u64) -> f32) -> Vec<Option<(f32, f32)>> {
    (first..=newest)
        .map(|second| Some((reading(second), 0.0)))
        .collect()
}

/// Where readings 0.3 seconds into each second of a window of `span` seconds sit across it.
fn tick_positions(span: u16) -> Vec<f32> {
    (0..span)
        .map(|offset| (f32::from(offset) + 0.3) / f32::from(span))
        .collect()
}

/// A chart following the Mac over the `span` seconds from `first`, in a card `card_width`
/// pixels wide and 100 pixels high, on a scale from 0 to 100.
fn sliding_geometry(first: u64, span: u16, card_width: f32, positions: &[f32]) -> Geometry<'_> {
    let window = TimeWindow {
        first,
        span: u64::from(span),
    };
    Geometry {
        bounds: Bounds::new(point(px(0.0), px(0.0)), size(px(card_width), px(100.0))),
        minimum: 0.0,
        maximum: 100.0,
        count: positions.len(),
        positions: Some(positions),
        segments: None,
        lower: 0,
        clock: Some(ReadingClock::of(window, None, TICK_SECONDS)),
    }
}
