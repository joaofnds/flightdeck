use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Background, Bounds, IntoElement, PathBuilder, Pixels, Window, canvas, fill, linear_color_stop,
    linear_gradient, point, prelude::*, px, rgb, size,
};

pub use crate::chart_axes::{AxisSide, TimeWindow, axis_column, axis_labels, time_labels};
use crate::data::Panel;

pub const BLUE: u32 = 0x0057_94f2;
pub const GREEN: u32 = 0x0073_bf69;
pub const GRID: u32 = 0x0028_2a2e;
/// Card and divider borders: the prototype's text colour at 12% over its panels.
pub const LINE: u32 = 0x002a_2c33;
pub const MUTED: u32 = 0x009a_9aa6;
pub const ORANGE: u32 = 0x00ff_9830;
pub const PANEL: u32 = 0x0018_1b1f;
/// The ring drawn around a focused control, which holds 3:1 over every fill beneath it.
pub const FOCUS_RING: u32 = 0x008a_b4ff;
pub const PURPLE: u32 = 0x00b8_77d9;
pub const RED: u32 = 0x00f2_495c;
pub const TEXT: u32 = 0x00cc_ccdc;
pub const YELLOW: u32 = 0x00f2_cc0c;
/// Green at 0%, yellow at 50% and red at 100%, as the prototype blends them.
const UTILIZATION_SCALE: [(f32, f32, f32); 3] = [
    (115.0, 191.0, 105.0),
    (250.0, 222.0, 42.0),
    (242.0, 73.0, 92.0),
];

/// The colour of a utilization percentage, which every CPU chart takes from here so that a
/// core's bar and the heatmap cell for the same reading are the same colour.
#[must_use]
pub fn utilization_color(percent: f64) -> gpui::Rgba {
    let Some(position) = num_traits::ToPrimitive::to_f32(&(percent.clamp(0.0, 100.0) / 50.0))
    else {
        unreachable!("A clamped percentage fits f32");
    };
    let stop = position.floor().min(1.0);
    let fraction = position - stop;
    let (from, to) = if stop < 1.0 {
        (UTILIZATION_SCALE[0], UTILIZATION_SCALE[1])
    } else {
        (UTILIZATION_SCALE[1], UTILIZATION_SCALE[2])
    };
    let channel = |from: f32, to: f32| {
        let Some(value) = num_traits::ToPrimitive::to_u32(&(from + (to - from) * fraction).round())
        else {
            unreachable!("A blend of two channels stays within 0 to 255");
        };
        value
    };

    rgb(channel(from.0, to.0) << 16 | channel(from.1, to.1) << 8 | channel(from.2, to.2))
}

pub const PLOT_INSETS: gpui::Edges<Pixels> = gpui::Edges {
    top: px(12.0),
    right: px(8.0),
    bottom: px(0.0),
    left: px(0.0),
};
/// Curves and axes average readings over a chart's span divided into this many buckets, thirty
/// seconds of a 15-minute chart, which is the sampling interval of the approved prototype.
const CURVE_BUCKETS: u8 = 30;

#[derive(Clone, Copy)]
pub struct Inspection {
    pub lower: usize,
    pub selected: usize,
    pub upper: usize,
}

impl Inspection {
    #[must_use]
    pub fn select_fraction(&self, fraction: f32) -> usize {
        assert!(self.upper < crate::data::LIVE_SAMPLE_LIMIT);
        assert!(self.lower <= self.upper);
        let count = self.upper - self.lower + 1;
        (self.lower..=self.upper)
            .find(|index| fraction < number(index - self.lower + 1) / number(count))
            .unwrap_or(self.upper)
    }
}

pub fn plot(
    panel: Rc<Panel>,
    inspection: Inspection,
    hidden: Rc<Vec<String>>,
    measured: Rc<Cell<Bounds<Pixels>>>,
) -> impl IntoElement {
    plot_cursor(panel, inspection, hidden, measured, (true, None))
}

pub fn plot_without_cursor(
    panel: Rc<Panel>,
    inspection: Inspection,
    hidden: Rc<Vec<String>>,
    measured: Rc<Cell<Bounds<Pixels>>>,
    time: Option<TimeWindow>,
) -> impl IntoElement {
    plot_cursor(panel, inspection, hidden, measured, (false, time))
}

fn plot_cursor(
    panel: Rc<Panel>,
    inspection: Inspection,
    hidden: Rc<Vec<String>>,
    measured: Rc<Cell<Bounds<Pixels>>>,
    (show_cursor, time): (bool, Option<TimeWindow>),
) -> impl IntoElement {
    canvas(
        move |bounds, _, _| measured.set(bounds),
        move |bounds, (), window, cx| {
            let chart = plot_bounds(bounds);
            crate::render_evidence::chart(&panel, inspection, chart, window);
            if matches!(panel.kind.as_str(), "line" | "self-chart") {
                paint_grid(
                    chart,
                    axis_scale(&panel, inspection, &hidden, "left"),
                    window,
                );
            }
            if let Some(time) = time {
                if panel.kind != "timeline" {
                    let ticks = crate::chart_axes::time_ticks(time, f32::from(chart.size.width));
                    paint_time_grid(chart, &ticks, window);
                }
            }
            clip_plot(chart, window, |window| match panel.kind.as_str() {
                "timeline" => paint_timeline(&panel, chart, inspection, &hidden, (window, cx)),
                "heatmap" => paint_heatmap(&panel, chart, inspection, &hidden, window),
                "core-bars" | "pressure" => paint_bars(&panel, chart, inspection, &hidden, window),
                _ => paint_lines(&panel, chart, inspection, &hidden, window),
            });
            if !show_cursor {
                return;
            }
            if panel.positions.is_some() {
                if panel.selected_index.is_none() {
                    return;
                }
            }
            let fraction = fraction_in_view(&panel, &inspection, inspection.selected);
            let x = chart.origin.x + chart.size.width * fraction;
            window.paint_quad(fill(
                Bounds::new(point(x, chart.origin.y), size(px(1.0), chart.size.height)),
                alpha(TEXT, 0.5),
            ));
        },
    )
    .w_full()
    .h_full()
}

#[must_use]
pub fn plot_bounds(bounds: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        point(
            bounds.origin.x + PLOT_INSETS.left,
            bounds.origin.y + PLOT_INSETS.top,
        ),
        size(
            (bounds.size.width - (PLOT_INSETS.left + PLOT_INSETS.right)).max(px(1.0)),
            (bounds.size.height - (PLOT_INSETS.top + PLOT_INSETS.bottom)).max(px(1.0)),
        ),
    )
}

#[must_use]
pub fn color(value: &str) -> u32 {
    match u32::from_str_radix(value.trim_start_matches('#'), 16) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("invalid captured color {value}: {error}");
            MUTED
        }
    }
}

#[must_use]
pub fn number(value: usize) -> f32 {
    match u16::try_from(value) {
        Ok(value) => f32::from(value),
        Err(error) => panic!("bounded UI dimension exceeds u16: {error}"),
    }
}

fn paint_grid(bounds: Bounds<Pixels>, scale: Scale, window: &mut Window) {
    for value in scale.ticks() {
        let y = bounds.bottom() - bounds.size.height * scale.fraction(value);
        let color = if value == 0.0 {
            alpha(TEXT, 0.12)
        } else {
            rgb(GRID)
        };
        window.paint_quad(fill(
            Bounds::new(point(bounds.origin.x, y), size(bounds.size.width, px(1.0))),
            color,
        ));
    }
}

fn paint_time_grid(bounds: Bounds<Pixels>, ticks: &[(f32, u64)], window: &mut Window) {
    for (fraction, _) in ticks.iter().take(64) {
        window.paint_quad(fill(
            Bounds::new(
                point(
                    bounds.origin.x + bounds.size.width * *fraction,
                    bounds.origin.y,
                ),
                size(px(1.0), bounds.size.height),
            ),
            alpha(GRID, 0.55),
        ));
    }
}

fn paint_lines(
    panel: &Panel,
    bounds: Bounds<Pixels>,
    inspection: Inspection,
    hidden: &[String],
    window: &mut Window,
) {
    prepare_lines(
        panel,
        bounds,
        inspection,
        hidden,
        |line, values, geometry| {
            paint_line_runs(line, panel, values, geometry, window);
        },
    );
}

pub(crate) fn prepare_lines(
    panel: &Panel,
    bounds: Bounds<Pixels>,
    inspection: Inspection,
    hidden: &[String],
    mut paint: impl FnMut(&crate::data::Line, &[Option<(f32, f32)>], &Geometry<'_>),
) {
    let left = axis_scale(panel, inspection, hidden, "left");
    assert!(inspection.upper < crate::data::LIVE_SAMPLE_LIMIT);
    let mut previous = [0.0; crate::data::LIVE_SAMPLE_LIMIT];
    let mut values = Vec::new();
    for line in &panel.lines {
        if hidden.contains(&line.name) {
            continue;
        }
        let stacked = stacked(panel, line);
        let readings = (inspection.lower..=inspection.upper).map(|index| {
            let value = line.values.get(index).copied().flatten()?;
            let value = if line.negative == Some(true) {
                -value
            } else {
                value
            };
            let bottom = if stacked { previous[index] } else { 0.0 };
            if stacked {
                previous[index] += value;
            }
            Some((value + bottom, bottom))
        });
        values.clear();
        values.reserve_exact(readings.size_hint().0);
        values.extend(readings);
        let scale = match &line.axis {
            Some(axis) => axis_scale(panel, inspection, hidden, axis),
            None => left,
        };
        let geometry = Geometry {
            bounds,
            minimum: scale.minimum,
            maximum: scale.maximum,
            count: values.len(),
            positions: panel.positions.as_deref(),
            segments: panel.segments.as_deref(),
            lower: inspection.lower,
            window: panel.window,
            follows: panel.follows,
        };
        paint(line, &values, &geometry);
    }
}

pub(crate) struct Geometry<'a> {
    pub(crate) bounds: Bounds<Pixels>,
    pub(crate) minimum: f32,
    pub(crate) maximum: f32,
    pub(crate) count: usize,
    pub(crate) positions: Option<&'a [f32]>,
    pub(crate) segments: Option<&'a [u64]>,
    pub(crate) lower: usize,
    /// The wall-clock seconds the positions span, which pins the averaging buckets to the clock.
    pub(crate) window: Option<TimeWindow>,
    /// Whether readings still leave at the window's start and arrive at its end.
    pub(crate) follows: bool,
}

impl Geometry<'_> {
    fn bucket(&self, index: usize) -> u8 {
        let fraction = self.positions.map_or_else(
            || number(index) / number(self.count.saturating_sub(1).max(1)),
            |positions| positions[self.lower + index],
        );
        bucket_of(fraction, self.window)
    }

    pub(crate) fn point(&self, index: usize, value: f32) -> gpui::Point<Pixels> {
        point(
            self.bounds.origin.x
                + self.bounds.size.width
                    * self.positions.map_or_else(
                        || number(index) / number(self.count.saturating_sub(1).max(1)),
                        |positions| positions[self.lower + index],
                    ),
            self.bounds.bottom()
                - self.bounds.size.height
                    * ((value - self.minimum) / (self.maximum - self.minimum)),
        )
    }
}

fn paint_line_runs(
    line: &crate::data::Line,
    panel: &Panel,
    values: &[Option<(f32, f32)>],
    geometry: &Geometry<'_>,
    window: &mut Window,
) {
    let present = |offset: usize| values[offset].is_some();
    for_each_run(
        values.len(),
        geometry.segments,
        geometry.lower,
        present,
        |run| {
            paint_run(line, panel, values, run, geometry, window);
        },
    );
}

/// Calls `visit` with each run of the `count` readings in view from `lower` that `present`
/// finds, as offsets from `lower`. A missing reading or a change of segment ends a run.
fn for_each_run(
    count: usize,
    segments: Option<&[u64]>,
    lower: usize,
    present: impl Fn(usize) -> bool,
    mut visit: impl FnMut(std::ops::Range<usize>),
) {
    let mut start = 0;
    for end in 0..=count {
        if end < count {
            if present(end) {
                if end > start {
                    if let Some(segments) = segments {
                        if segments[lower + end] != segments[lower + end - 1] {
                            visit(start..end);
                            start = end;
                        }
                    }
                }
                continue;
            }
        }
        if end > start {
            visit(start..end);
        }
        start = end + 1;
    }
}

fn paint_run(
    line: &crate::data::Line,
    panel: &Panel,
    values: &[Option<(f32, f32)>],
    range: std::ops::Range<usize>,
    geometry: &Geometry<'_>,
    window: &mut Window,
) {
    let tint = color(&line.color);
    let curve = drawn_curve(values, range, geometry);
    if curve.points.is_empty() {
        return;
    }

    if curve.points.len() == 1 {
        let position = point(curve.points[0].x, curve.points[0].top);
        window.paint_quad(fill(
            Bounds::new(position - point(px(1.5), px(1.5)), size(px(3.0), px(3.0))),
            rgb(tint),
        ));
        return;
    }

    let stacked = stacked(panel, line);
    let width = if stacked { px(1.0) } else { px(2.0) };
    let mut stroke = PathBuilder::stroke(width);
    if line.dashed == Some(true) {
        stroke = stroke.dash_array(&[px(6.0), px(4.0)]);
    }
    let mut area = PathBuilder::fill();
    let top: Vec<_> = curve
        .points
        .iter()
        .map(|sample| point(sample.x, sample.top))
        .collect();
    let bottom: Vec<_> = curve
        .points
        .iter()
        .rev()
        .map(|sample| point(sample.x, sample.bottom))
        .collect();
    let tangents_bottom: Vec<f32> = curve.tangents_bottom.iter().rev().copied().collect();
    smooth(&mut stroke, &top, &curve.tangents_top, true);
    smooth(&mut area, &top, &curve.tangents_top, true);
    smooth(&mut area, &bottom, &tangents_bottom, false);
    area.close();

    let default_opacity = if stacked { 0.45 } else { 0.12 };
    let opacity = line.fill.or(panel.fill).unwrap_or(default_opacity);
    if line.dashed != Some(true) {
        if opacity > 0.0 {
            let span = curve
                .points
                .iter()
                .fold(None, |span: Option<(Pixels, Pixels)>, sample| {
                    let (low, high) = span.unwrap_or((sample.top, sample.bottom));
                    Some((low.min(sample.top), high.max(sample.bottom)))
                });
            let fill = curve_fill(tint, opacity, line, geometry.bounds, span);
            paint_path(area, fill, window);
        }
    }
    paint_path(stroke, rgb(tint), window);
}

/// The prototype fades every area from its full opacity at the plot's top to nothing at the
/// plot's bottom, however tall the area is. A gradient spans the area's own bounds, so each end
/// takes the opacity the plot-wide fade has at that height.
fn curve_fill(
    tint: u32,
    opacity: f32,
    line: &crate::data::Line,
    plot: Bounds<Pixels>,
    span: Option<(Pixels, Pixels)>,
) -> Background {
    let Some((high, low)) = span else {
        return alpha(tint, 0.0).into();
    };
    let fade = |y: Pixels| {
        let depth = ((y - plot.origin.y) / plot.size.height).clamp(0.0, 1.0);
        let level = if line.negative == Some(true) {
            depth
        } else {
            1.0 - depth
        };
        alpha(tint, opacity * level)
    };

    linear_gradient(
        180.0,
        linear_color_stop(fade(high.min(low)), 0.0),
        linear_color_stop(fade(high.max(low)), 1.0),
    )
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct CurvePoint {
    x: Pixels,
    top: Pixels,
    bottom: Pixels,
    bucket: u8,
}

/// The stretch of one run's curve that a chart draws, with the tangent of its top and bottom
/// edges at each point. A lone point draws as a dot.
#[derive(Default)]
struct DrawnCurve {
    points: Vec<CurvePoint>,
    tangents_top: Vec<f32>,
    tangents_bottom: Vec<f32>,
}

/// The stretch of one run's curve that no later reading can change, so a curve once drawn keeps
/// its shape as the window slides, and leaves the chart only whole, at its oldest edge.
fn drawn_curve(
    values: &[Option<(f32, f32)>],
    range: std::ops::Range<usize>,
    geometry: &Geometry<'_>,
) -> DrawnCurve {
    let ends = RunEnds::of(&range, values.len(), geometry.follows);
    let curve = curve_points(values, range, geometry);
    let buckets: Vec<u8> = curve.iter().map(|sample| sample.bucket).collect();
    let trim = trim(&buckets, ends);
    if trim.drawn.is_empty() {
        return DrawnCurve::default();
    }

    let kept = &curve[trim.kept.clone()];
    if kept.len() == 1 {
        return DrawnCurve {
            points: kept.to_vec(),
            tangents_top: vec![0.0],
            tangents_bottom: vec![0.0],
        };
    }

    let tangents_of = |side: fn(&CurvePoint) -> Pixels| {
        let points: Vec<_> = kept
            .iter()
            .map(|sample| point(sample.x, side(sample)))
            .collect();
        monotone_tangents(&points)
    };
    let tangents_top = tangents_of(|sample| sample.top);
    let tangents_bottom = tangents_of(|sample| sample.bottom);
    let drawn = trim.drawn.start - trim.kept.start..trim.drawn.end - trim.kept.start;
    DrawnCurve {
        points: kept[drawn.clone()].to_vec(),
        tangents_top: tangents_top[drawn.clone()].to_vec(),
        tangents_bottom: tangents_bottom[drawn].to_vec(),
    }
}

/// Whether each end of a run meets an edge of the view beyond which readings still leave or
/// arrive, which only a view that follows the newest second has.
#[derive(Clone, Copy)]
struct RunEnds {
    head_open: bool,
    tail_open: bool,
}

impl RunEnds {
    /// The ends of `run`, as offsets into the `count` readings in view.
    fn of(run: &std::ops::Range<usize>, count: usize, follows: bool) -> Self {
        if !follows {
            return Self {
                head_open: false,
                tail_open: false,
            };
        }

        Self {
            head_open: run.start == 0,
            tail_open: run.end == count,
        }
    }
}

/// The points of a run, as ranges of its points in time order, that a chart keeps and the
/// ones it draws.
struct Trim {
    kept: std::ops::Range<usize>,
    drawn: std::ops::Range<usize>,
}

/// Which points of a run, from the bucket of each, keep the shape they are drawn with.
///
/// At an open head, a point in the window's first bucket has lost readings to the slide, so its
/// mean changes each second and it is not kept. The kept point after it has lost the neighbour
/// its tangent was shaped with, so it shapes the next stretch without drawing its own. At an
/// open tail, a point in the bucket after the window's last whole one is still filling and is
/// not kept, and the last kept point waits for the neighbour still to come before its stretch
/// is drawn. A run that would draw one point of several draws none, since that point ends a
/// stretch still to come.
fn trim(buckets: &[u8], ends: RunEnds) -> Trim {
    let mut kept = 0..buckets.len();
    let mut head_cut = false;
    if ends.head_open {
        if buckets.first() == Some(&0) {
            kept.start += 1;
            head_cut = true;
        }
    }
    if ends.tail_open {
        if buckets.last() == Some(&CURVE_BUCKETS) {
            kept.end -= 1;
        }
    }
    assert!(kept.start <= kept.end);

    let mut drawn = kept.clone();
    if head_cut {
        drawn.start = (drawn.start + 1).min(drawn.end);
    }
    if ends.tail_open {
        drawn.end = drawn.end.saturating_sub(1).max(drawn.start);
    }
    if drawn.len() == 1 {
        if kept.len() > 1 {
            drawn.end = drawn.start;
        }
    }
    assert!(kept.start <= drawn.start);
    assert!(drawn.end <= kept.end);
    Trim { kept, drawn }
}

/// Averages the measured readings in each bucket of one run, so the drawn curve carries no
/// reading that was not measured and keeps every bucket's mean.
fn curve_points(
    values: &[Option<(f32, f32)>],
    range: std::ops::Range<usize>,
    geometry: &Geometry<'_>,
) -> Vec<CurvePoint> {
    let mut curve = Vec::with_capacity(range.len());
    let mut bucket = None;
    let mut sum = (0.0_f32, 0.0_f32, 0.0_f32);
    let mut count = 0.0_f32;
    for index in range {
        let Some((value, bottom)) = values[index] else {
            continue;
        };

        let current = geometry.bucket(index);
        if let Some(bucket) = bucket.filter(|bucket| *bucket != current) {
            curve.push(curve_point(sum, count, bucket));
            sum = (0.0, 0.0, 0.0);
            count = 0.0;
        }
        bucket = Some(current);
        let top = geometry.point(index, value);
        sum.0 += f32::from(top.x);
        sum.1 += f32::from(top.y);
        sum.2 += f32::from(geometry.point(index, bottom).y);
        count += 1.0;
    }
    if let Some(bucket) = bucket {
        curve.push(curve_point(sum, count, bucket));
    }
    curve
}

/// The bucket a reading at `fraction` of the chart's span falls in. With a `window` the buckets
/// start at multiples of a thirtieth of its span on the clock, so a reading stays in its bucket
/// with the same neighbours as the window slides, and a past bucket's mean never changes.
fn bucket_of(fraction: f32, window: Option<TimeWindow>) -> u8 {
    assert!(fraction.is_finite());
    let fraction = fraction.clamp(0.0, 1.0);
    let bucket = match window {
        Some(window) => {
            assert!(window.span > 0);
            let buckets = u64::from(CURVE_BUCKETS);
            let offset = match num_traits::ToPrimitive::to_f64(&window.span) {
                Some(span) => (f64::from(fraction) * span).round(),
                None => panic!("a u64 span converts to f64"),
            };
            let second = match num_traits::ToPrimitive::to_u64(&offset) {
                Some(offset) => window.first + offset,
                None => panic!("a clamped fraction of the span is a whole number of seconds"),
            };
            // Both divisions floor, so the bucket is the clock bucket's distance from the first.
            num_traits::ToPrimitive::to_u8(
                &(second * buckets / window.span - window.first * buckets / window.span),
            )
        }
        None => num_traits::ToPrimitive::to_u8(&(fraction * f32::from(CURVE_BUCKETS)).floor()),
    };
    match bucket {
        Some(bucket) => {
            assert!(bucket <= CURVE_BUCKETS + 1);
            bucket
        }
        None => panic!("a clamped fraction maps to a bucket of at most {CURVE_BUCKETS} + 1"),
    }
}

fn curve_point(sum: (f32, f32, f32), count: f32, bucket: u8) -> CurvePoint {
    assert!(count > 0.0);
    CurvePoint {
        x: px(sum.0 / count),
        top: px(sum.1 / count),
        bottom: px(sum.2 / count),
        bucket,
    }
}

/// Draws a cubic through the points with the tangent given at each, which `monotone_tangents`
/// chooses so a curve never overshoots the readings it joins.
fn smooth(
    builder: &mut PathBuilder,
    points: &[gpui::Point<Pixels>],
    tangents: &[f32],
    start: bool,
) {
    assert_eq!(points.len(), tangents.len());
    let Some(first) = points.first() else {
        return;
    };

    if start {
        builder.move_to(*first);
    } else {
        builder.line_to(*first);
    }
    if points.len() < 2 {
        return;
    }

    for (index, pair) in points.windows(2).enumerate() {
        let (left, right) = (pair[0], pair[1]);
        let third = (f32::from(right.x) - f32::from(left.x)) / 3.0;
        builder.cubic_bezier_to(
            right,
            point(left.x + px(third), left.y + px(third * tangents[index])),
            point(
                right.x - px(third),
                right.y - px(third * tangents[index + 1]),
            ),
        );
    }
}

fn monotone_tangents(points: &[gpui::Point<Pixels>]) -> Vec<f32> {
    assert!(points.len() >= 2);
    let slopes: Vec<f32> = points
        .windows(2)
        .map(|pair| {
            let run = f32::from(pair[1].x) - f32::from(pair[0].x);
            if run == 0.0 {
                0.0
            } else {
                (f32::from(pair[1].y) - f32::from(pair[0].y)) / run
            }
        })
        .collect();
    let mut tangents = vec![0.0; points.len()];
    tangents[0] = slopes[0];
    tangents[points.len() - 1] = slopes[slopes.len() - 1];
    // Each tangent depends on its two neighbours alone, so a point leaving or joining the run
    // reshapes no stretch beyond the one beside it. Fritsch and Carlson keep a cubic monotone
    // while each end's tangent is at most three times the slope it spans, so the limit holds
    // the curve between the readings it joins without the prototype's pass along the run.
    for index in 1..points.len() - 1 {
        let (before, after) = (slopes[index - 1], slopes[index]);
        if before * after > 0.0 {
            let limit = 3.0 * before.abs().min(after.abs());
            tangents[index] = f32::midpoint(before, after).clamp(-limit, limit);
        }
    }
    tangents
}

/// One axis of a chart: its bounds and the spacing of its labelled ticks, chosen as the approved
/// prototype chooses them.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Scale {
    pub minimum: f32,
    pub maximum: f32,
    pub step: f32,
}

impl Scale {
    pub fn ticks(self) -> impl Iterator<Item = f32> {
        (0..=12_u8)
            .map(move |index| self.minimum + f32::from(index) * self.step)
            .take_while(move |value| *value <= self.maximum + self.step * 0.0001)
    }

    #[must_use]
    pub fn fraction(self, value: f32) -> f32 {
        (value - self.minimum) / (self.maximum - self.minimum)
    }
}

/// The left axis range, which the tests read without building a whole scale.
#[cfg(test)]
#[must_use]
pub fn domain(panel: &Panel, inspection: Inspection, hidden: &[String]) -> (f32, f32) {
    let scale = axis_scale(panel, inspection, hidden, "left");
    (scale.minimum, scale.maximum)
}

#[must_use]
pub fn axis_domain(
    panel: &Panel,
    inspection: Inspection,
    hidden: &[String],
    axis: &str,
) -> (f32, f32) {
    let scale = axis_scale(panel, inspection, hidden, axis);
    (scale.minimum, scale.maximum)
}

#[must_use]
pub fn axis_scale(panel: &Panel, inspection: Inspection, hidden: &[String], axis: &str) -> Scale {
    followed_scale(panel, inspection, hidden, axis)
        .unwrap_or_else(|| own_scale(panel, inspection, hidden, axis))
}

/// The scale of an axis whose line restates another line in another unit: the leading line's
/// range times the ratio between the two, so both axes mark the same heights. It is the
/// ratio at the newest second in view that both lines read.
fn followed_scale(
    panel: &Panel,
    inspection: Inspection,
    hidden: &[String],
    axis: &str,
) -> Option<Scale> {
    let restating = panel
        .lines
        .iter()
        .find(|line| line.axis.as_deref() == Some(axis) && !hidden.contains(&line.name))?;
    let name = restating.follows.as_deref()?;
    let leader = panel
        .lines
        .iter()
        .find(|line| line.name == name && !hidden.contains(&line.name))?;
    let ratio = (inspection.lower..=inspection.upper)
        .rev()
        .find_map(|index| {
            let led = leader.values.get(index).copied().flatten()?;
            let restated = restating.values.get(index).copied().flatten()?;
            (led > 0.0 && restated > 0.0).then(|| restated / led)
        })?;
    let leading = own_scale(
        panel,
        inspection,
        hidden,
        leader.axis.as_deref().unwrap_or("left"),
    );
    let minimum = leading.minimum * ratio;
    let maximum = leading.maximum * ratio;
    Some(Scale {
        minimum,
        maximum,
        step: decimal_step(maximum - minimum),
    })
}

fn own_scale(panel: &Panel, inspection: Inspection, hidden: &[String], axis: &str) -> Scale {
    let (low, high) = axis_extent(panel, inspection, hidden, axis);
    let fixed = if axis == "left" {
        panel.maximum.filter(|maximum| *maximum > 0.0)
    } else {
        None
    };
    let mut maximum = fixed.unwrap_or(high * 1.04);
    let mut minimum = low * 1.04;
    if maximum <= minimum {
        maximum = minimum + 1.0;
    }

    let binary = axis_unit(panel, axis) == "GiB";
    let step = if binary {
        binary_step((maximum - minimum) / 3.0)
    } else {
        decimal_step(maximum - minimum)
    };
    minimum = (minimum / step).floor() * step;
    if fixed.is_none() {
        if !binary {
            maximum = (maximum / step).ceil() * step;
        }
    }
    Scale {
        minimum,
        maximum,
        step,
    }
}

#[must_use]
pub fn axis_unit<'a>(panel: &'a Panel, axis: &str) -> &'a str {
    panel
        .lines
        .iter()
        .find(|line| line.axis.as_deref().unwrap_or("left") == axis)
        .and_then(|line| line.unit.as_deref())
        .unwrap_or(&panel.unit)
}

/// The range of the bucket means the curves draw, so the axis spans what is drawn rather than a
/// one-second spike the averaged curve does not reach, or a reading the curve does not draw yet
/// whose rescaling would move the curve already drawn.
fn axis_extent(panel: &Panel, inspection: Inspection, hidden: &[String], axis: &str) -> (f32, f32) {
    let floor = if axis == "left" {
        panel.minimum.unwrap_or(0.0).min(0.0)
    } else {
        0.0
    };
    let mut extent = (floor, 0.0_f32);
    let drawn = drawn_readings(panel, &inspection);
    let count = inspection.upper - inspection.lower + 1;
    // One mean per line, then the stacked positive and negative totals.
    let mut slots = vec![(0.0_f32, 0.0_f32); panel.lines.len() + 2];
    let mut bucket = None;
    for index in inspection.lower..=inspection.upper {
        let current = bucket_of(fraction_in_view(panel, &inspection, index), panel.window);
        if bucket.is_some_and(|bucket| bucket != current) {
            extent = axis_extent_flush(&mut slots, extent);
        }
        bucket = Some(current);
        let offset = index - inspection.lower;
        let shown = |slot: usize| drawn[slot * count + offset];
        axis_extent_add(panel, hidden, axis, index, shown, &mut slots);
    }
    axis_extent_flush(&mut slots, extent)
}

/// Marks each reading in view that its line's curve draws, at `slot * count + offset` for the
/// line in `slot` and the `count` readings from `inspection.lower`, so the axis spans the drawn
/// curves and no reading they hold back.
fn drawn_readings(panel: &Panel, inspection: &Inspection) -> Vec<bool> {
    let count = inspection.upper - inspection.lower + 1;
    let mut drawn = vec![false; panel.lines.len() * count];
    let mut buckets: Vec<u8> = Vec::with_capacity(usize::from(CURVE_BUCKETS) + 1);
    let bucket = |offset: usize| {
        let fraction = fraction_in_view(panel, inspection, inspection.lower + offset);
        bucket_of(fraction, panel.window)
    };
    for (slot, line) in panel.lines.iter().enumerate() {
        let present = |offset: usize| {
            let value = line.values.get(inspection.lower + offset).copied();
            value.flatten().is_some()
        };
        let segments = panel.segments.as_deref();
        for_each_run(count, segments, inspection.lower, present, |run| {
            buckets.clear();
            for offset in run.clone() {
                let current = bucket(offset);
                if buckets.last() != Some(&current) {
                    buckets.push(current);
                }
            }
            let trim = trim(&buckets, RunEnds::of(&run, count, panel.follows));
            if trim.drawn.is_empty() {
                return;
            }

            let shown = buckets[trim.drawn.start]..=buckets[trim.drawn.end - 1];
            for offset in run {
                drawn[slot * count + offset] = shown.contains(&bucket(offset));
            }
        });
    }
    drawn
}

/// Where the reading at `index` sits across the plot, from 0 at its left to 1 at its right.
fn fraction_in_view(panel: &Panel, inspection: &Inspection, index: usize) -> f32 {
    panel.positions.as_deref().map_or_else(
        || number(index - inspection.lower) / number((inspection.upper - inspection.lower).max(1)),
        |positions| positions[index],
    )
}

fn axis_extent_add(
    panel: &Panel,
    hidden: &[String],
    axis: &str,
    index: usize,
    shown: impl Fn(usize) -> bool,
    slots: &mut [(f32, f32)],
) {
    let stacks = panel.lines.len();
    let mut positive = None;
    let mut negative = None;
    for (slot, line) in panel.lines.iter().enumerate() {
        if hidden.contains(&line.name) {
            continue;
        }
        if line.axis.as_deref().unwrap_or("left") != axis {
            continue;
        }
        if !shown(slot) {
            continue;
        }
        let Some(value) = line.values.get(index).copied().flatten() else {
            continue;
        };

        let value = if line.negative == Some(true) {
            -value
        } else {
            value
        };
        if stacked(panel, line) {
            if value < 0.0 {
                negative = Some(negative.unwrap_or(0.0) + value);
            } else {
                positive = Some(positive.unwrap_or(0.0) + value);
            }
        } else {
            slots[slot].0 += value;
            slots[slot].1 += 1.0;
        }
    }
    for (slot, total) in [(stacks, positive), (stacks + 1, negative)] {
        if let Some(total) = total {
            slots[slot].0 += total;
            slots[slot].1 += 1.0;
        }
    }
}

/// Widens the extent by each slot's mean over the finished bucket and empties the slots.
fn axis_extent_flush(slots: &mut [(f32, f32)], (minimum, maximum): (f32, f32)) -> (f32, f32) {
    let mut extent = (minimum, maximum);
    for slot in slots.iter_mut() {
        if slot.1 > 0.0 {
            let mean = slot.0 / slot.1;
            extent = (extent.0.min(mean), extent.1.max(mean));
        }
        *slot = (0.0, 0.0);
    }
    extent
}

/// The prototype's tick spacing: about five ticks, each 1, 2, 2.5, 5 or 10 times a power of ten.
fn decimal_step(span: f32) -> f32 {
    assert!(span > 0.0);
    let rough = span / 5.0;
    let mut power = 1.0_f32;
    for _ in 0..40 {
        if power * 10.0 > rough {
            break;
        }
        power *= 10.0;
    }
    for _ in 0..40 {
        if power <= rough {
            break;
        }
        power /= 10.0;
    }
    let normalized = rough / power;
    let factor = [1.0, 2.0, 2.5, 5.0, 10.0]
        .into_iter()
        .find(|factor| *factor >= normalized)
        .unwrap_or(10.0);
    factor * power
}

/// Binary quantities tick at a power of two, as 16, 32 and 48 GiB do.
fn binary_step(rough: f32) -> f32 {
    assert!(rough > 0.0);
    let mut power = 1.0_f32;
    for _ in 0..64 {
        if power * 2.0 > rough {
            break;
        }
        power *= 2.0;
    }
    for _ in 0..64 {
        if power <= rough {
            break;
        }
        power /= 2.0;
    }
    power
}

fn stacked(panel: &Panel, line: &crate::data::Line) -> bool {
    if panel.stack == Some(true) {
        return line.stack != Some(false);
    }
    false
}

fn paint_timeline(
    panel: &Panel,
    bounds: Bounds<Pixels>,
    view: Inspection,
    hidden: &[String],
    (window, cx): (&mut Window, &mut gpui::App),
) {
    let Some(line) = panel.lines.first() else {
        return;
    };
    if hidden.contains(&line.name) {
        return;
    }
    let count = view.upper - view.lower + 1;
    let cell = bounds.size.width / number(count);
    let mut index = view.lower;
    while index <= view.upper {
        let Some(value) = line.values.get(index).copied().flatten() else {
            index += 1;
            continue;
        };
        let mut end = index + 1;
        while end <= view.upper && line.values.get(end).copied().flatten() == Some(value) {
            end += 1;
        }
        let run = Bounds::new(
            point(
                bounds.origin.x + bounds.size.width * (number(index - view.lower) / number(count)),
                bounds.origin.y,
            ),
            size(cell * number(end - index), bounds.size.height),
        );
        window.paint_quad(fill(run, alpha(timeline_color(panel, value), 0.85)));
        paint_run_label(panel, value, run, window, cx);
        index = end;
    }
}

/// The charging state's name inside a run wider than 90 pixels, as the prototype draws it.
fn paint_run_label(
    panel: &Panel,
    value: f32,
    run: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    if panel.id != "live-battery-charging" || run.size.width <= px(90.0) {
        return;
    }
    let Some(name) = panel
        .states
        .as_ref()
        .and_then(|states| states.get(usize::from(value != 0.0)))
    else {
        return;
    };
    let shaped = window.text_system().shape_line(
        name.clone().into(),
        px(12.0),
        &[gpui::TextRun {
            len: name.len(),
            font: gpui::font(".SystemUIFont"),
            color: rgb(0x0011_1217).into(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        None,
    );
    let origin = point(
        run.origin.x + (run.size.width - shaped.width) / 2.0,
        run.origin.y + (run.size.height - px(18.0)) / 2.0,
    );
    if let Err(error) = shaped.paint(origin, px(18.0), gpui::TextAlign::Left, None, window, cx) {
        eprintln!("timeline label: {error}");
    }
}

pub(crate) fn timeline_color(panel: &Panel, value: f32) -> u32 {
    if panel.id == "live-battery-charging" {
        return match value {
            0.0 => BLUE,
            1.0 => GREEN,
            _ => MUTED,
        };
    }

    match value {
        0.0 => GREEN,
        1.0 => ORANGE,
        2.0 | 3.0 => RED,
        _ => MUTED,
    }
}

fn paint_heatmap(
    panel: &Panel,
    bounds: Bounds<Pixels>,
    view: Inspection,
    hidden: &[String],
    window: &mut Window,
) {
    let columns = view.upper - view.lower + 1;
    for (row, line) in panel.lines.iter().enumerate() {
        if hidden.contains(&line.name) {
            continue;
        }
        for index in view.lower..=view.upper {
            let value = line.values.get(index).copied().flatten();
            let cell = Bounds::new(
                point(
                    bounds.origin.x
                        + bounds.size.width * (number(index - view.lower) / number(columns)),
                    bounds.origin.y
                        + bounds.size.height * (number(row) / number(panel.lines.len())),
                ),
                size(
                    bounds.size.width / number(columns) - px(1.0),
                    bounds.size.height / number(panel.lines.len()) - px(1.0),
                ),
            );
            let tint = value.map_or(MUTED, threshold);
            window.paint_quad(fill(
                cell,
                alpha(tint, value.map_or(0.1, |v| (v / 100.0).clamp(0.12, 1.0))),
            ));
        }
    }
}

fn paint_bars(
    panel: &Panel,
    bounds: Bounds<Pixels>,
    view: Inspection,
    hidden: &[String],
    window: &mut Window,
) {
    let count = panel.lines.len();
    for (index, line) in panel.lines.iter().enumerate() {
        if hidden.contains(&line.name) {
            continue;
        }
        if let Some(value) = line.values.get(view.selected).copied().flatten() {
            let height = bounds.size.height * value.clamp(0.0, 100.0) / 100.0;
            let width = bounds.size.width / number(count);
            window.paint_quad(fill(
                Bounds::new(
                    point(
                        bounds.origin.x + width * number(index),
                        bounds.bottom() - height,
                    ),
                    size(width - px(3.0), height),
                ),
                rgb(threshold(value)),
            ));
        }
    }
}

fn threshold(value: f32) -> u32 {
    if value < 60.0 {
        GREEN
    } else {
        if value < 80.0 {
            BLUE
        } else {
            if value < 90.0 { ORANGE } else { RED }
        }
    }
}

fn paint_path(builder: PathBuilder, color: impl Into<Background>, window: &mut Window) {
    match builder.build() {
        Ok(path) => window.paint_path(path, color),
        Err(error) => eprintln!("chart path: {error}"),
    }
}

pub fn clip_plot(bounds: Bounds<Pixels>, window: &mut Window, paint: impl FnOnce(&mut Window)) {
    window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| paint(window));
}

/// The focus indicator: a ring along the element's inner edge, so that focusing it moves
/// nothing, the text inside keeps the contrast it has unfocused, and an element with no
/// background of its own is outlined rather than filled.
#[must_use]
pub fn focus_ring(style: gpui::StyleRefinement) -> gpui::StyleRefinement {
    style.shadow(vec![gpui::BoxShadow {
        color: rgb(FOCUS_RING).into(),
        offset: point(px(0.0), px(0.0)),
        blur_radius: px(0.0),
        spread_radius: px(2.0),
        inset: true,
    }])
}

/// The inspection cursor, a brighter line than any gridline so the two never read alike.
#[must_use]
pub fn cursor_color() -> gpui::Rgba {
    alpha(TEXT, 0.55)
}

#[must_use]
pub fn alpha(value: u32, opacity: f32) -> gpui::Rgba {
    let mut color = rgb(value);
    color.a = opacity;
    color
}

#[cfg(test)]
mod live_tests {
    use super::*;

    #[test]
    fn the_focus_ring_is_drawn_inside_the_element_so_a_transparent_one_stays_unfilled() {
        // Goal: a focused control with no background of its own shows a ring and nothing else.
        // Method: read the shadows of the ring's style and check the one it adds is inset, since
        // a shadow outside the element fills the element's own bounds too.
        let style = focus_ring(gpui::StyleRefinement::default());

        let shadows = style.box_shadow.unwrap();

        assert_eq!(shadows.len(), 1);
        assert!(shadows[0].inset);
    }

    #[test]
    fn prepared_lines_keep_stack_bases_through_hidden_negative_and_missing_readings() {
        // Goal: storage reuse preserves independent gaps, stack bases and directional readings.
        // Method: prepare three visible lines and compare their exact values across two segments.
        let panel: Panel = serde_json::from_value(serde_json::json!({
            "id":"memory", "title":"Memory", "unit":"GiB", "kind":"line", "stack":true,
            "positions":[0.0,0.5,1.0], "segments":[0,0,1], "lines":[
                {"name":"active", "color":"#5794f2", "values":[1.0,null,3.0]},
                {"name":"hidden", "color":"#ffffff", "values":[100.0,100.0,100.0]},
                {"name":"wired", "color":"#ff9830", "values":[2.0,4.0,null]},
                {"name":"out", "color":"#b877d9", "values":[5.0,6.0,7.0],
                    "negative":true, "stack":false}]}))
        .unwrap();
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(200.0), px(100.0)));
        let inspection = Inspection {
            lower: 0,
            selected: 2,
            upper: 2,
        };
        let mut prepared = Vec::new();

        prepare_lines(
            &panel,
            bounds,
            inspection,
            &["hidden".into()],
            |line, values, geometry| {
                prepared.push((
                    line.name.clone(),
                    values.to_vec(),
                    geometry.segments.map(<[_]>::to_vec),
                ));
            },
        );

        assert_eq!(
            prepared,
            vec![
                (
                    "active".into(),
                    vec![Some((1.0, 0.0)), None, Some((3.0, 0.0))],
                    Some(vec![0, 0, 1])
                ),
                (
                    "wired".into(),
                    vec![Some((3.0, 1.0)), Some((4.0, 0.0)), None],
                    Some(vec![0, 0, 1])
                ),
                (
                    "out".into(),
                    vec![Some((-5.0, 0.0)), Some((-6.0, 0.0)), Some((-7.0, 0.0))],
                    Some(vec![0, 0, 1])
                ),
            ]
        );
    }

    #[test]
    fn shared_chart_domain_keeps_sent_traffic_negative_and_power_unstacked() {
        // Goal: chart rendering interprets projection metadata without summing independent power.
        // Method: calculate domains from one actual projection with directional and power data.
        let selection = serde_json::json!({"second": 1000, "context_last_second": 1000,
            "selected": null, "trends": [{
            "run": "00000000000000010000000000000002",
            "boot": "00000000000000030000000000000004", "publication": 1, "segment": 1,
            "wall_ns": 1_000_000_000_000_u64, "network_rx_bps": 2_000_000,
            "network_tx_bps": 1_000_000, "power_watts": 8.0, "gpu_watts": 2.0}]});
        let inspection = Inspection {
            lower: 0,
            selected: 0,
            upper: 0,
        };

        let network = crate::live_projection::panels("network", &selection, None).unwrap();
        let (minimum, maximum) = domain(&network[0], inspection, &[]);
        assert!((minimum + 2.0).abs() < f32::EPSILON);
        assert!((maximum - 3.0).abs() < f32::EPSILON);
        let energy = crate::live_projection::panels("energy", &selection, None).unwrap();
        let (minimum, maximum) = domain(&energy[0], inspection, &[]);
        assert!(minimum.abs() < f32::EPSILON);
        assert!((maximum - 10.0).abs() < f32::EPSILON);
    }

    #[test]
    fn scales_tick_like_the_prototype() {
        // Goal: axes use the prototype's readable ticks rather than quarters of the range.
        // Method: compute decimal and binary steps for spans taken from the captured reference.
        assert!((decimal_step(104.0) - 25.0).abs() < f32::EPSILON);
        assert!((decimal_step(8.32) - 2.0).abs() < f32::EPSILON);
        assert!((decimal_step(0.6) - 0.2).abs() < 0.000_001);
        assert!((binary_step(49.92 / 3.0) - 16.0).abs() < f32::EPSILON);
        let scale = Scale {
            minimum: 0.0,
            maximum: 49.92,
            step: 16.0,
        };
        assert_eq!(
            scale.ticks().collect::<Vec<_>>(),
            vec![0.0, 16.0, 32.0, 48.0]
        );
    }

    #[test]
    fn curve_points_average_each_bucket_and_keep_lone_readings() {
        // Goal: the drawn curve averages measured readings per bucket and invents none.
        // Method: place five readings across two buckets and compare the means.
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(8.0), px(100.0)));
        let geometry = Geometry {
            bounds,
            minimum: 0.0,
            maximum: 100.0,
            count: 5,
            positions: Some(&[0.0, 0.001, 0.002, 0.6, 0.9]),
            segments: None,
            lower: 0,
            window: None,
            follows: false,
        };
        let values = [
            Some((10.0, 0.0)),
            Some((20.0, 0.0)),
            Some((30.0, 0.0)),
            None,
            Some((50.0, 0.0)),
        ];

        let curve = curve_points(&values, 0..5, &geometry);

        assert_eq!(curve.len(), 2);
        assert!((f32::from(curve[0].top) - 80.0).abs() < 0.001);
        assert!((f32::from(curve[1].top) - 50.0).abs() < 0.001);
        assert!((f32::from(curve[0].x) - 0.008).abs() < 0.001);
        assert!((f32::from(curve[1].x) - 7.2).abs() < 0.001);
    }

    #[test]
    fn curve_points_keep_past_bucket_means_as_the_window_slides() {
        // Goal: a reading already drawn keeps its place when the next tick moves the window.
        // Method: draw the same varying readings in windows one second apart and compare the
        // mean of every bucket both windows hold whole, in seconds and in value.
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(900.0), px(100.0)));
        let curve = |first: u64| {
            let window = TimeWindow { first, span: 900 };
            let seconds: Vec<u64> = (first..first + 900).collect();
            let positions: Vec<f32> = (0..900_u16)
                .map(|second| f32::from(second) / 900.0)
                .collect();
            let values: Vec<_> = seconds
                .iter()
                .map(|second| Some((f32::from(u16::try_from(second % 97).unwrap()), 0.0)))
                .collect();
            let geometry = Geometry {
                bounds,
                minimum: 0.0,
                maximum: 100.0,
                count: 900,
                positions: Some(&positions),
                segments: None,
                lower: 0,
                window: Some(window),
                follows: false,
            };
            let curve = curve_points(&values, 0..900, &geometry);
            let first = f32::from(u16::try_from(first - 1000).unwrap());
            curve
                .iter()
                .map(|point| (first + f32::from(point.x), f32::from(point.top)))
                .collect::<Vec<_>>()
        };

        let before = curve(1000);
        let after = curve(1001);

        let shared = |curve: &[(f32, f32)]| curve[1..curve.len() - 1].to_vec();
        let before = shared(&before);
        let after = shared(&after);
        assert_eq!(before.len(), after.len());
        for (before, after) in before.iter().zip(&after) {
            assert!(
                (before.0 - after.0).abs() < 0.01,
                "{before:?} became {after:?}"
            );
            assert!(
                (before.1 - after.1).abs() < 0.01,
                "{before:?} became {after:?}"
            );
        }
    }

    /// A reading that turns often, so the curve has peaks and troughs to keep.
    fn varying_reading(second: u64) -> f32 {
        f32::from(u16::try_from(second * 7919 % 101).unwrap())
    }

    /// The geometry of a 900-pixel chart of the 900 seconds from `first`, one pixel a second,
    /// with each reading 0.3 seconds into its second as a tick lands.
    fn live_geometry(first: u64, positions: &[f32], follows: bool) -> Geometry<'_> {
        Geometry {
            bounds: Bounds::new(point(px(0.0), px(0.0)), size(px(900.0), px(100.0))),
            minimum: 0.0,
            maximum: 100.0,
            count: positions.len(),
            positions: Some(positions),
            segments: None,
            lower: 0,
            window: Some(TimeWindow { first, span: 900 }),
            follows,
        }
    }

    /// Where readings sit across a 900-second chart from `offset` seconds into it onwards.
    fn live_positions(offsets: std::ops::Range<u16>) -> Vec<f32> {
        offsets
            .map(|offset| (f32::from(offset) + 0.3) / 900.0)
            .collect()
    }

    /// The drawn curve of the readings of the 900 seconds from `first`, in a view that follows
    /// the newest second or one held still.
    fn live_curve(first: u64, follows: bool) -> DrawnCurve {
        let positions = live_positions(0..900);
        let values: Vec<_> = (first..first + 900)
            .map(|second| Some((varying_reading(second), 0.0)))
            .collect();

        drawn_curve(&values, 0..900, &live_geometry(first, &positions, follows))
    }

    /// Each drawn stretch of the curve of the window from `first`, as its two ends and their
    /// tangents, on an x axis pinned to the clock rather than to the window.
    fn drawn_stretches(first: u64) -> Vec<[f32; 6]> {
        let curve = live_curve(first, true);
        let clock = f32::from(u16::try_from(first - 1000).unwrap());

        (1..curve.points.len())
            .map(|index| {
                let (left, right) = (curve.points[index - 1], curve.points[index]);
                [
                    clock + f32::from(left.x),
                    f32::from(left.top),
                    curve.tangents_top[index - 1],
                    clock + f32::from(right.x),
                    f32::from(right.top),
                    curve.tangents_top[index],
                ]
            })
            .collect()
    }

    fn same_stretch(stretch: &[f32; 6], other: &[f32; 6]) -> bool {
        stretch
            .iter()
            .zip(other)
            .all(|(value, other)| (value - other).abs() < 0.01)
    }

    /// A one-line CPU chart of `values` across the 900 seconds from 1010, which follows the
    /// newest second.
    fn live_panel(values: &[Option<f32>]) -> Panel {
        let mut panel: Panel = serde_json::from_value(serde_json::json!({
            "id":"cpu", "title":"CPU", "unit":"%", "kind":"line",
            "positions": live_positions(0..900), "lines":[
                {"name":"All cores", "color":"#5794f2", "values": values}]}))
        .unwrap();
        panel.window = Some(TimeWindow {
            first: 1010,
            span: 900,
        });
        panel.follows = true;
        panel
    }

    const LIVE_INSPECTION: Inspection = Inspection {
        lower: 0,
        selected: 899,
        upper: 899,
    };

    #[test]
    fn a_drawn_curve_keeps_every_stretch_it_drew_as_the_window_slides() {
        // Goal: a stretch of curve once drawn stays exactly as drawn while readings arrive, and
        // leaves the chart only whole, at its oldest edge.
        // Method: slide a window one second at a time across five buckets of turning readings
        // and check, for each two windows in a row, that at most the oldest stretch of the
        // first and the newest of the second are drawn by only one of them.
        for first in 1000..1150 {
            let before = drawn_stretches(first);
            let after = drawn_stretches(first + 1);

            assert!(!before.is_empty());
            let held = |stretch: &[f32; 6], curve: &[[f32; 6]]| {
                curve.iter().any(|other| same_stretch(stretch, other))
            };
            let gone = before.iter().take_while(|s| !held(s, &after)).count();
            let added = after.iter().rev().take_while(|s| !held(s, &before)).count();
            assert!(gone <= 1, "{gone} stretches of {first} left");
            assert!(added <= 1, "{added} stretches of {} came", first + 1);
            for stretch in &before[gone..] {
                assert!(held(stretch, &after), "{stretch:?} of {first} changed");
            }
            for stretch in &after[..after.len() - added] {
                assert!(
                    held(stretch, &before),
                    "{stretch:?} of {} is new",
                    first + 1
                );
            }
        }
    }

    #[test]
    fn a_live_window_that_starts_inside_a_bucket_draws_its_third_to_third_last_bucket() {
        // Goal: a following curve holds back the cut first bucket, the filling last one, and
        // the bucket beside each, whose stretch those shape.
        // Method: draw a full window from 1010, whose 31 buckets run from 990 to 1919, and read
        // the places of its first and last drawn points, the mean places of 1050 to 1079 and of
        // 1830 to 1859.
        let curve = live_curve(1010, true);

        assert_eq!(curve.points.len(), 27);
        assert!((f32::from(curve.points[0].x) - 54.8).abs() < 0.01);
        assert!((f32::from(curve.points[26].x) - 834.8).abs() < 0.01);
    }

    #[test]
    fn a_live_window_that_starts_on_a_bucket_draws_its_third_to_second_last_bucket() {
        // Goal: a following curve holds back its oldest two buckets, which the next second cuts,
        // and its newest whole bucket, whose stretch waits for the next.
        // Method: draw a full window from 1020, whose 30 buckets run from 1020 to 1919, and read
        // the places of its first and last drawn points, the mean places of 1080 to 1109 and of
        // 1860 to 1889.
        let curve = live_curve(1020, true);

        assert_eq!(curve.points.len(), 27);
        assert!((f32::from(curve.points[0].x) - 74.8).abs() < 0.01);
        assert!((f32::from(curve.points[26].x) - 854.8).abs() < 0.01);
    }

    #[test]
    fn a_held_window_draws_every_bucket() {
        // Goal: a view that no longer follows the newest second loses and gains no reading, so
        // its curve shows every bucket.
        // Method: draw the window from 1010 held still and count its 31 bucket points.
        let curve = live_curve(1010, false);

        assert_eq!(curve.points.len(), 31);
    }

    #[test]
    fn a_short_held_selection_draws_every_reading() {
        // Goal: a five-second selection, where each second is a bucket of its own, shows all
        // five readings.
        // Method: draw five readings across a held window of five seconds.
        let positions: Vec<f32> = (0..5_u16)
            .map(|offset| (f32::from(offset) + 0.3) / 5.0)
            .collect();
        let values: Vec<_> = (0..5_u16)
            .map(|offset| Some((f32::from(offset) * 10.0, 0.0)))
            .collect();
        let mut geometry = live_geometry(2000, &positions, false);
        geometry.window = Some(TimeWindow {
            first: 2000,
            span: 5,
        });

        let curve = drawn_curve(&values, 0..5, &geometry);

        assert_eq!(curve.points.len(), 5);
    }

    #[test]
    fn a_new_recording_draws_from_its_first_bucket() {
        // Goal: the first readings of a recording slid out of nowhere and have no neighbour
        // still to lose, so the curve starts at once from their bucket.
        // Method: draw a following window from 1020 whose readings begin 600 seconds in, and
        // read its first drawn point, the mean place of 1620 to 1649, and its point count, the
        // buckets from 1620 to 1889.
        let positions = live_positions(600..900);
        let values: Vec<_> = (1620..1920_u64)
            .map(|second| Some((varying_reading(second), 0.0)))
            .collect();

        let curve = drawn_curve(&values, 0..300, &live_geometry(1020, &positions, true));

        assert_eq!(curve.points.len(), 9);
        assert!((f32::from(curve.points[0].x) - 614.8).abs() < 0.01);
    }

    #[test]
    fn a_curve_draws_up_to_the_last_reading_before_a_gap() {
        // Goal: a run that a gap ends cannot change, so its stretch reaches its last bucket.
        // Method: end the readings 600 seconds into a window and compare the last drawn point
        // with the mean place of its bucket's readings, 570.3 to 599.3 seconds in.
        let positions = live_positions(0..900);
        let values: Vec<_> = (1020..1920_u64)
            .map(|second| (second < 1620).then(|| (varying_reading(second), 0.0)))
            .collect();

        let curve = drawn_curve(&values, 0..600, &live_geometry(1020, &positions, true));

        assert!((f32::from(curve.points.last().unwrap().x) - 584.8).abs() < 0.01);
    }

    #[test]
    fn a_curve_draws_from_the_first_reading_after_a_gap() {
        // Goal: a run that a gap starts lost nothing to the slide, so it draws from its first
        // bucket even where that is the window's first.
        // Method: leave the first five seconds of the window from 1010 empty and compare the
        // first drawn point with the mean place of the rest of its bucket, 1015 to 1019.
        let positions = live_positions(0..900);
        let values: Vec<_> = (1010..1910_u64)
            .map(|second| (second >= 1015).then(|| (varying_reading(second), 0.0)))
            .collect();

        let curve = drawn_curve(&values, 5..900, &live_geometry(1010, &positions, true));

        assert!((f32::from(curve.points[0].x) - 7.3).abs() < 0.01);
    }

    #[test]
    fn a_lone_reading_between_gaps_draws_as_a_dot() {
        // Goal: a reading with gaps on both sides is final, so it shows at once.
        // Method: draw the one-reading run 450 seconds into a window of gaps.
        let positions = live_positions(0..900);
        let values: Vec<_> = (0..900)
            .map(|offset| (offset == 450).then_some((40.0, 0.0)))
            .collect();

        let curve = drawn_curve(&values, 450..451, &live_geometry(1020, &positions, true));

        assert_eq!(curve.points.len(), 1);
        assert!((f32::from(curve.points[0].x) - 450.3).abs() < 0.01);
    }

    #[test]
    fn a_stacked_curve_shapes_its_lower_edge_from_its_own_readings() {
        // Goal: the lower edge of a stacked area bends with the line beneath it, not with the
        // line's own top.
        // Method: draw a turning top over a flat base and check every base tangent is flat.
        let positions = live_positions(0..900);
        let values: Vec<_> = (1020..1920_u64)
            .map(|second| Some((varying_reading(second) + 5.0, 5.0)))
            .collect();

        let curve = drawn_curve(&values, 0..900, &live_geometry(1020, &positions, true));

        assert!(
            curve
                .tangents_top
                .iter()
                .any(|tangent| tangent.abs() > 0.01)
        );
        assert!(
            curve
                .tangents_bottom
                .iter()
                .all(|tangent| tangent.abs() < 0.001)
        );
    }

    #[test]
    fn the_value_axis_ignores_readings_the_curve_does_not_draw_yet() {
        // Goal: a reading the curve does not draw yet cannot rescale the curve already drawn.
        // Method: put a spike in 1860 to 1889, the newest whole bucket of the window from 1010,
        // whose stretch waits for the next, and read the axis of a steady chart.
        let values: Vec<_> = (0..900)
            .map(|offset| {
                Some(if (850..880).contains(&offset) {
                    90.0
                } else {
                    10.0
                })
            })
            .collect();

        let (_, maximum) = domain(&live_panel(&values), LIVE_INSPECTION, &[]);

        assert!((maximum - 12.5).abs() < f32::EPSILON, "{maximum}");
    }

    #[test]
    fn the_value_axis_spans_a_stretch_that_a_gap_ends_beside_the_newest_edge() {
        // Goal: a run a gap has closed draws its newest bucket at once, so the axis rises to
        // hold it rather than clip it.
        // Method: end a line with a spike over 1860 to 1889 and no readings after it, and read
        // the axis.
        let values: Vec<_> = (0..900)
            .map(|offset| match offset {
                0..850 => Some(10.0),
                850..880 => Some(90.0),
                _ => None,
            })
            .collect();

        let (_, maximum) = domain(&live_panel(&values), LIVE_INSPECTION, &[]);

        assert!(maximum >= 90.0, "{maximum}");
    }

    #[test]
    fn a_tangent_depends_on_its_two_neighbours_alone() {
        // Goal: dropping the oldest point of a run reshapes only the stretch beside it, so the
        // rest of a drawn curve stays put as its oldest bucket leaves.
        // Method: compute tangents through slopes that shrink tenfold each step, with and
        // without the first point, and compare every tangent the dropped point does not touch.
        let heights = [0.0, 1000.0, 1100.0, 1110.0, 1111.0, 1111.1];
        let points: Vec<_> = (0..6_u8)
            .map(|index| point(px(f32::from(index)), px(heights[usize::from(index)])))
            .collect();

        let whole = monotone_tangents(&points);
        let cut = monotone_tangents(&points[1..]);

        assert_eq!(whole[2..], cut[1..]);
    }

    #[test]
    fn monotone_tangents_flatten_at_turning_points() {
        // Goal: a smoothed curve never overshoots a peak, as the prototype's curve does not.
        // Method: compute tangents through a peak and check the peak tangent is flat.
        let points = [
            point(px(0.0), px(10.0)),
            point(px(1.0), px(0.0)),
            point(px(2.0), px(10.0)),
        ];
        assert!(monotone_tangents(&points)[1].abs() < f32::EPSILON);
    }
}
