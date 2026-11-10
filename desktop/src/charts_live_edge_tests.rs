//! The newest end of a sliding chart's line, which draws each reading once, at the end of its
//! own second, and then only scrolls it left.
use gpui::{Bounds, point, px, size};

use super::*;

/// The width of an overview card in the 1320-pixel window, and of a card as wide as a page.
const CARD_WIDTHS: [f32; 2] = [360.0, 900.0];
/// The steady reading before the burst, and the burst's.
const STEADY: f32 = 10.0;
const BURST: f32 = 100.0;
/// The share of the chart a burst's line must pass.
const PASS: f32 = 70.0;

#[test]
fn a_burst_passes_seventy_percent_at_the_publication_of_its_first_reading() {
    // Goal: a burst reaches the line at the publication of its first 100% reading, at the 5 and
    // 15 minute ranges, in a card of each width. Method: slide a chart to a steady 10% and then
    // 100% from a known second, a reading 0.3 s into each second as a tick lands, starting the
    // burst at each phase of a 3 s stretch, and read the line's newest point.
    for card_width in CARD_WIDTHS {
        for span in [300_u16, 900] {
            for burst in 2000_u64..2003 {
                let first = burst + 1 - u64::from(span);
                let positions = tick_positions(span);
                let values = readings(
                    first,
                    burst,
                    |second| {
                        if second < burst { STEADY } else { BURST }
                    },
                );
                let geometry = sliding_geometry(first, span, card_width, &positions);

                let curves = run_curves(&values, 0..values.len(), &geometry);

                let newest = curves.last().and_then(|curve| curve.readings.last());
                assert!(
                    newest.is_some_and(|point| point.top > PASS),
                    "{span} s in {card_width} px from {burst}: {newest:?}"
                );
            }
        }
    }
}

#[test]
fn each_point_is_one_reading_at_the_end_of_its_second() {
    // Goal: a sliding chart draws every reading in view as its own point, at the end of the
    // second it was taken in, so a point is a measurement and no later reading moves it, however
    // many readings the card has pixels for. Method: chart 15 minutes of readings that differ
    // from second to second in an overview card, and compare its points with the readings.
    let span = 900_u16;
    let first = 2000;
    let newest = first + u64::from(span) - 1;
    let reading = |second: u64| f32::from(u16::try_from(second * 37 % 101).unwrap());
    let positions = tick_positions(span);
    let values = readings(first, newest, reading);
    let geometry = sliding_geometry(first, span, CARD_WIDTHS[0], &positions);

    let curves = run_curves(&values, 0..values.len(), &geometry);

    let drawn: Vec<(u64, Pixels, f32)> = curves
        .iter()
        .flat_map(|curve| {
            let points = curve.points.iter().map(|point| point.top.x);
            let readings = curve.readings.iter();
            readings
                .zip(points)
                .map(|(reading, x)| (reading.second, x, reading.top))
        })
        .collect();
    let expected: Vec<(u64, Pixels, f32)> = (first..=newest)
        .map(|second| {
            let end = window_fraction(second + 1 - first, u64::from(span));
            (second, px(CARD_WIDTHS[0]) * end, reading(second))
        })
        .collect();
    assert_eq!(drawn, expected);
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
/// pixels wide.
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
