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
/// Display averaging keeps about one curve point per this many pixels, so a 900-second history
/// draws as a readable curve; inspection still reads each exact observation.
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
            let fraction = panel.positions.as_ref().map_or_else(
                || {
                    number(inspection.selected - inspection.lower)
                        / number((inspection.upper - inspection.lower).max(1))
                },
                |positions| positions[inspection.selected],
            );
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
    let mut start = 0;
    for end in 0..=values.len() {
        if end < values.len() {
            if values[end].is_some() {
                if end > start {
                    if let Some(segments) = geometry.segments {
                        if segments[geometry.lower + end] != segments[geometry.lower + end - 1] {
                            paint_run(line, panel, values, start..end, geometry, window);
                            start = end;
                        }
                    }
                }
                continue;
            }
        }
        if end > start {
            paint_run(line, panel, values, start..end, geometry, window);
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
    let curve = curve_points(values, range, geometry);
    if curve.len() == 1 {
        let position = point(curve[0].x, curve[0].top);
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
        .iter()
        .map(|sample| point(sample.x, sample.top))
        .collect();
    let bottom: Vec<_> = curve
        .iter()
        .rev()
        .map(|sample| point(sample.x, sample.bottom))
        .collect();
    smooth(&mut stroke, &top, true);
    smooth(&mut area, &top, true);
    smooth(&mut area, &bottom, false);
    area.close();

    let default_opacity = if stacked { 0.45 } else { 0.12 };
    let opacity = line.fill.or(panel.fill).unwrap_or(default_opacity);
    if line.dashed != Some(true) {
        if opacity > 0.0 {
            let span = curve
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
        if bucket.is_some_and(|bucket| bucket != current) {
            curve.push(curve_point(sum, count));
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
    if count > 0.0 {
        curve.push(curve_point(sum, count));
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

fn curve_point(sum: (f32, f32, f32), count: f32) -> CurvePoint {
    assert!(count > 0.0);
    CurvePoint {
        x: px(sum.0 / count),
        top: px(sum.1 / count),
        bottom: px(sum.2 / count),
    }
}

/// Draws a monotone cubic through the points, as the approved prototype does, so a curve never
/// overshoots the readings it joins.
fn smooth(builder: &mut PathBuilder, points: &[gpui::Point<Pixels>], start: bool) {
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

    let tangents = monotone_tangents(points);
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
    for index in 1..points.len() - 1 {
        let (before, after) = (slopes[index - 1], slopes[index]);
        if before * after > 0.0 {
            tangents[index] = f32::midpoint(before, after);
        }
    }
    for (index, slope) in slopes.iter().enumerate() {
        if *slope == 0.0 {
            tangents[index] = 0.0;
            tangents[index + 1] = 0.0;
            continue;
        }

        let a = tangents[index] / slope;
        let b = tangents[index + 1] / slope;
        let length = a.hypot(b);
        if length > 3.0 {
            tangents[index] = 3.0 * a / length * slope;
            tangents[index + 1] = 3.0 * b / length * slope;
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
/// one-second spike the averaged curve does not reach.
fn axis_extent(panel: &Panel, inspection: Inspection, hidden: &[String], axis: &str) -> (f32, f32) {
    let floor = if axis == "left" {
        panel.minimum.unwrap_or(0.0).min(0.0)
    } else {
        0.0
    };
    let mut extent = (floor, 0.0_f32);
    // One mean per line, then the stacked positive and negative totals.
    let mut slots = vec![(0.0_f32, 0.0_f32); panel.lines.len() + 2];
    let mut bucket = None;
    for index in inspection.lower..=inspection.upper {
        let fraction = panel.positions.as_deref().map_or_else(
            || {
                number(index - inspection.lower)
                    / number((inspection.upper - inspection.lower).max(1))
            },
            |positions| positions[index],
        );
        let current = bucket_of(fraction, panel.window);
        if bucket.is_some_and(|bucket| bucket != current) {
            extent = axis_extent_flush(&mut slots, extent);
        }
        bucket = Some(current);
        axis_extent_add(panel, hidden, axis, index, &mut slots);
    }
    axis_extent_flush(&mut slots, extent)
}

fn axis_extent_add(
    panel: &Panel,
    hidden: &[String],
    axis: &str,
    index: usize,
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

/// The focus indicator: a ring outside the element, so that focusing it moves nothing and
/// the text inside keeps the contrast it has unfocused.
#[must_use]
pub fn focus_ring(style: gpui::StyleRefinement) -> gpui::StyleRefinement {
    style.shadow(vec![gpui::BoxShadow {
        color: rgb(FOCUS_RING).into(),
        offset: point(px(0.0), px(0.0)),
        blur_radius: px(0.0),
        spread_radius: px(2.0),
        inset: false,
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
        let selection = serde_json::json!({"second": 1000, "selected": null, "trends": [{
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
