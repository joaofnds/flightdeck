//! The drawings of a sliding chart's live edge that TASK-154 compares on a burst: base's, whose
//! newest point is the mean of its window and waits for the second after its bucket, the newest
//! reading itself at the live edge, and the reading one second behind it, which no late tick can
//! still change. Only base's drawing paints the app's charts, so the others are drawn here, over
//! the curve base's drawing builds.
use std::collections::BTreeMap;

use gpui::{Bounds, point, px, size};

use super::*;

/// The width of an overview card in the 1320-pixel window.
const CARD_WIDTH: f32 = 360.0;
/// The publications a burst is followed through, past the slowest drawing's pass.
const PUBLICATIONS: u64 = 10;
/// The steady reading before the burst, and the burst's.
const STEADY: f32 = 10.0;
const BURST: f32 = 100.0;
/// The share of the chart a burst's line must pass.
const PASS: f32 = 70.0;

/// A way to draw the newest end of a sliding chart's line.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Drawing {
    /// Base's: every point the mean of its window.
    Mean,
    /// The newest reading at the end of its own second, after the means.
    Newest,
    /// The reading one second before the newest, at the end of its second, in place of the
    /// mean that ends there.
    OneBehind,
}

#[test]
fn a_burst_passes_seventy_percent_after_as_many_publications_as_each_drawing_waits() {
    // Goal: state, for each drawing compared, how many publications after a burst's first 100%
    // reading the newest point of the line passes 70%, at the 5 and 15 minute ranges, with
    // nothing on screen. Method: slide a chart one publication a second over a steady 10% and
    // then 100% from a known second, a reading 0.3 s into each second as a tick lands, in an
    // overview card, starting the burst at each phase of a 15 minute chart's 3 s bucket.
    let mut publications = BTreeMap::new();
    for drawing in [Drawing::Mean, Drawing::Newest, Drawing::OneBehind] {
        for span in [300_u16, 900] {
            for burst in 2000_u64..2003 {
                let passed = publications_to_pass(drawing, span, burst);
                publications.insert((drawing, span, burst % 3), passed);
            }
        }
    }

    assert_eq!(
        publications,
        BTreeMap::from([
            ((Drawing::Mean, 300, 0), Some(3)),
            ((Drawing::Mean, 300, 1), Some(3)),
            ((Drawing::Mean, 300, 2), Some(3)),
            ((Drawing::Mean, 900, 0), Some(6)),
            ((Drawing::Mean, 900, 1), Some(5)),
            ((Drawing::Mean, 900, 2), Some(7)),
            ((Drawing::Newest, 300, 0), Some(0)),
            ((Drawing::Newest, 300, 1), Some(0)),
            ((Drawing::Newest, 300, 2), Some(0)),
            ((Drawing::Newest, 900, 0), Some(0)),
            ((Drawing::Newest, 900, 1), Some(0)),
            ((Drawing::Newest, 900, 2), Some(0)),
            ((Drawing::OneBehind, 300, 0), Some(1)),
            ((Drawing::OneBehind, 300, 1), Some(1)),
            ((Drawing::OneBehind, 300, 2), Some(1)),
            ((Drawing::OneBehind, 900, 0), Some(1)),
            ((Drawing::OneBehind, 900, 1), Some(1)),
            ((Drawing::OneBehind, 900, 2), Some(1)),
        ])
    );
}

/// How many publications after the first 100% reading, at second `burst`, the newest point of
/// `drawing`'s line over `span` seconds passes 70%, or `None` within `PUBLICATIONS`.
fn publications_to_pass(drawing: Drawing, span: u16, burst: u64) -> Option<u64> {
    let positions = tick_positions(span);
    (0..PUBLICATIONS).find(|publication| {
        let newest = burst + publication;
        let first = newest + 1 - u64::from(span);
        let values = burst_readings(first, newest, burst);
        let geometry = sliding_geometry(first, span, &positions);

        let curves = drawn_curves(drawing, &values, &geometry);

        let Some(newest_point) = curves.last().and_then(|curve| curve.means.last()) else {
            panic!("a chart of whole readings draws a point");
        };
        newest_point.top > PASS
    })
}

/// The steady readings and then the burst's, from `first` to `newest`.
fn burst_readings(first: u64, newest: u64, burst: u64) -> Vec<Option<(f32, f32)>> {
    (first..=newest)
        .map(|second| {
            let reading = if second < burst { STEADY } else { BURST };
            Some((reading, 0.0))
        })
        .collect()
}

/// Where readings 0.3 seconds into each second of a window of `span` seconds sit across it.
fn tick_positions(span: u16) -> Vec<f32> {
    (0..span)
        .map(|offset| (f32::from(offset) + 0.3) / f32::from(span))
        .collect()
}

/// A chart following the Mac over the `span` seconds from `first`, in an overview card.
fn sliding_geometry(first: u64, span: u16, positions: &[f32]) -> Geometry<'_> {
    let window = TimeWindow {
        first,
        span: u64::from(span),
    };
    Geometry {
        bounds: Bounds::new(point(px(0.0), px(0.0)), size(px(CARD_WIDTH), px(100.0))),
        minimum: 0.0,
        maximum: 100.0,
        count: positions.len(),
        positions: Some(positions),
        segments: None,
        lower: 0,
        buckets: Some(Buckets::of(window, None, TICK_SECONDS, CARD_WIDTH)),
        sliding: true,
    }
}

/// The curves `drawing` draws through `values`, one unbroken run of readings in view.
fn drawn_curves(
    drawing: Drawing,
    values: &[Option<(f32, f32)>],
    geometry: &Geometry<'_>,
) -> Vec<Curve> {
    let mut curves = run_curves(values, 0..values.len(), geometry);
    let behind = match drawing {
        Drawing::Mean => return curves,
        Drawing::Newest => 1,
        Drawing::OneBehind => 2,
    };

    let Some(clock) = geometry.clock() else {
        panic!("a sliding chart has a clock");
    };
    let index = values.len() - behind;
    let Some((top, bottom)) = values[index] else {
        panic!("a burst's readings are whole");
    };
    let Some(second) = clock
        .buckets
        .second_of(clock.positions[clock.lower + index])
    else {
        panic!("a reading in view lies after the clock's zero");
    };
    let edge = WindowMean {
        end: second + 1,
        top,
        bottom,
    };
    let Some(curve) = curves.last_mut() else {
        panic!("a chart of whole readings draws a curve");
    };
    if curve.means.last().is_some_and(|mean| mean.end == edge.end) {
        curve.means.pop();
        curve.points.pop();
    }
    assert!(curve.means.last().is_none_or(|mean| mean.end < edge.end));
    curve.means.push(edge);
    curve.points.push(RunPoint {
        top: geometry.clock_point(clock.buckets, edge.end, edge.top),
        bottom: geometry.clock_point(clock.buckets, edge.end, edge.bottom),
    });
    curves
}

/// The publications after the burst's first 100% reading that the images show, from the first.
const SHOWN: u64 = 8;
/// How many times the images magnify the newest end of each card.
const ZOOM: f32 = 8.0;
/// A card's height, and the room beside and below each card.
const CARD_HEIGHT: f32 = 100.0;
const GAP: f32 = 12.0;
/// The width of the column naming each drawing.
const LABEL_WIDTH: f32 = 220.0;

#[test]
#[ignore = "draws the compared drawings as images for the owner; needs FLIGHTDECK_DRAWINGS_OUTPUT"]
fn the_compared_drawings_draw_a_burst_as_images() {
    // Goal: let the owner see each compared drawing draw the same burst, with nothing on screen.
    // Method: paint each drawing's line with the app's own curve painter in an overview card at
    // each of the first publications after the burst, whole and with its newest end magnified,
    // in a window gpui renders off screen through Metal, and save one image a range.
    let Some(output) = std::env::var_os("FLIGHTDECK_DRAWINGS_OUTPUT") else {
        panic!("FLIGHTDECK_DRAWINGS_OUTPUT names the directory the images go in");
    };
    let output = std::path::PathBuf::from(output);
    let text = std::sync::Arc::new(gpui_apple::AppleTextSystem::new());
    let mut app = gpui::HeadlessAppContext::with_platform(text, std::sync::Arc::new(()), || {
        let renderer: Box<dyn gpui::PlatformHeadlessRenderer> =
            Box::new(gpui_apple::metal_renderer::MetalHeadlessRenderer::new());
        Ok(Some(renderer))
    });

    for span in [300_u16, 900] {
        let columns = f32::from(u16::try_from(SHOWN).unwrap());
        let width = LABEL_WIDTH + columns * (CARD_WIDTH + GAP) + GAP;
        let height = 6.0 * (CARD_HEIGHT + GAP) + 2.0 * GAP + 24.0;
        let window = app
            .open_window(size(px(width), px(height)), |_, cx| {
                cx.new(|_| BurstSheet { span })
            })
            .unwrap();
        app.run_until_parked();

        let image = app.capture_screenshot(window.into()).unwrap();

        let path = output.join(format!("burst-{}-minutes.png", span / 60));
        image.save(&path).unwrap();
        println!("drawings: {}", path.display());
    }
}

/// The compared drawings of one range, a row each whole and a row each magnified, over the
/// publications after the burst.
struct BurstSheet {
    span: u16,
}

impl gpui::Render for BurstSheet {
    fn render(&mut self, _: &mut Window, _: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use gpui::{IntoElement, ParentElement, Styled, div};

        let span = self.span;
        let mut sheet = div()
            .size_full()
            .bg(rgb(0x0011_1217))
            .text_color(rgb(0x00cc_ccdc))
            .text_size(px(13.0))
            .p(px(GAP))
            .flex()
            .flex_col()
            .gap(px(GAP))
            .child(format!(
                "{} minutes: a steady 10% and then 100%, publications 0 to {} after the first \
                 100% reading. The dashed line is 70%.",
                span / 60,
                SHOWN - 1
            ));
        for drawing in [Drawing::Mean, Drawing::Newest, Drawing::OneBehind] {
            for zoom in [1.0, ZOOM] {
                let mut row = div().flex().flex_row().gap(px(GAP)).child(
                    div()
                        .w(px(LABEL_WIDTH - GAP))
                        .child(drawing_label(drawing, zoom)),
                );
                for publication in 0..SHOWN {
                    row = row.child(
                        gpui::canvas(
                            |_, _, _| {},
                            move |bounds, (), window, _| {
                                paint_card(drawing, span, publication, zoom, bounds, window);
                            },
                        )
                        .w(px(CARD_WIDTH))
                        .h(px(CARD_HEIGHT))
                        .bg(rgb(0x0018_1b1f)),
                    );
                }
                sheet = sheet.child(row);
            }
        }
        sheet.into_element()
    }
}

fn drawing_label(drawing: Drawing, zoom: f32) -> String {
    let name = match drawing {
        Drawing::Mean => "Base: every point a mean",
        Drawing::Newest => "The newest reading",
        Drawing::OneBehind => "The reading a second behind",
    };
    if zoom > 1.0 {
        format!("{name}, newest end at {zoom}x")
    } else {
        name.to_owned()
    }
}

/// Paints `drawing`'s line at `publication` after the burst into the card at `bounds`, the
/// newest `1 / zoom` of it across the whole card where `zoom` is past 1.
fn paint_card(
    drawing: Drawing,
    span: u16,
    publication: u64,
    zoom: f32,
    bounds: Bounds<gpui::Pixels>,
    window: &mut Window,
) {
    let burst = 2000;
    let newest = burst + publication;
    let first = newest + 1 - u64::from(span);
    let positions = tick_positions(span);
    let values = burst_readings(first, newest, burst);
    let mut geometry = sliding_geometry(first, span, &positions);
    let plot_width = bounds.size.width * zoom;
    geometry.bounds = Bounds::new(
        point(bounds.right() - plot_width, bounds.origin.y),
        size(plot_width, bounds.size.height),
    );
    let panel = burst_panel(&values, &positions, first, span);

    let guide = geometry.y(PASS);
    let mask = gpui::ContentMask { bounds };
    window.with_content_mask(Some(mask), |window| {
        let mut x = bounds.origin.x;
        while x < bounds.right() {
            let dash = Bounds::new(point(x, guide), size(px(4.0), px(1.0)));
            window.paint_quad(fill(dash, gpui::rgba(0xffff_ff55)));
            x += px(8.0);
        }
        for curve in drawn_curves(drawing, &values, &geometry) {
            paint_curve(&panel.lines[0], &panel, curve, &geometry, None, window);
        }
    });
}

/// A one-line CPU chart of the burst's readings, coloured as the app colours a CPU line.
fn burst_panel(values: &[Option<(f32, f32)>], positions: &[f32], first: u64, span: u16) -> Panel {
    let readings: Vec<Option<f32>> = values
        .iter()
        .map(|value| value.map(|(top, _)| top))
        .collect();
    let mut panel: Panel = serde_json::from_value(serde_json::json!({
        "id":"cpu", "title":"CPU", "unit":"%", "kind":"line", "positions": positions,
        "lines":[{"name":"All cores", "color":"#5794f2", "values": readings}]}))
    .unwrap();
    panel.window = Some(TimeWindow {
        first,
        span: u64::from(span),
    });
    panel.sliding = true;
    panel
}
