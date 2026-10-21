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
            buckets: panel_buckets(panel),
        };
        paint(line, &values, &geometry);
    }
}

/// The buckets of a panel with a clock.
fn panel_buckets(panel: &Panel) -> Option<Buckets> {
    panel
        .window
        .map(|window| Buckets::of(window, panel.detail_first))
}

pub(crate) struct Geometry<'a> {
    pub(crate) bounds: Bounds<Pixels>,
    pub(crate) minimum: f32,
    pub(crate) maximum: f32,
    pub(crate) count: usize,
    pub(crate) positions: Option<&'a [f32]>,
    pub(crate) segments: Option<&'a [u64]>,
    pub(crate) lower: usize,
    /// The stretches of the clock the readings are averaged over. A panel with no clock draws
    /// each reading as its own bucket.
    pub(crate) buckets: Option<Buckets>,
}

impl Geometry<'_> {
    /// The bucket index of the reading at `index` in view, or `None` when the chart does not
    /// draw it.
    fn bucket_index(&self, index: usize) -> Option<u64> {
        drawn_bucket_index(self.buckets, self.positions, self.lower + index)
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
    let buckets = bucket_points(values, range, geometry);
    if buckets.is_empty() {
        return;
    }

    if buckets.len() == 1 {
        let band = buckets[0].band;
        window.paint_quad(fill(
            Bounds::from_corners(
                band.top - point(px(1.5), px(0.0)),
                band.bottom + point(px(1.5), px(0.0)),
            ),
            alpha(tint, BAND_OPACITY),
        ));
        let position = buckets[0].line.top;
        window.paint_quad(fill(
            Bounds::new(position - point(px(1.5), px(1.5)), size(px(3.0), px(3.0))),
            rgb(tint),
        ));
        return;
    }

    let points: Vec<RunPoint> = buckets.iter().map(|bucket| bucket.line).collect();
    let dashed = line.dashed == Some(true);
    let stacked = stacked(panel, line);
    let width = if stacked { px(1.0) } else { px(2.0) };
    let mut stroke = PathBuilder::stroke(width);
    if dashed {
        stroke = stroke.dash_array(&[px(6.0), px(4.0)]);
    }
    stroke.move_to(points[0].top);
    for sample in &points[1..] {
        stroke.line_to(sample.top);
    }

    let default_opacity = if stacked { 0.45 } else { 0.12 };
    let opacity = line.fill.or(panel.fill).unwrap_or(default_opacity);
    if !dashed {
        if opacity > 0.0 {
            let span = points
                .iter()
                .fold(None, |span: Option<(Pixels, Pixels)>, sample| {
                    let (low, high) = span.unwrap_or((sample.top.y, sample.bottom.y));
                    Some((low.min(sample.top.y), high.max(sample.bottom.y)))
                });
            let fill = curve_fill(tint, opacity, line, geometry.bounds, span);
            window.paint_path(run_area(&points), fill);
        }
    }
    // A dashed line is a reading too, so its band shows its spikes as a solid line's does.
    let band: Vec<RunPoint> = buckets.iter().map(|bucket| bucket.band).collect();
    window.paint_path(run_area(&band), alpha(tint, BAND_OPACITY));
    paint_path(stroke, rgb(tint), window);
}

/// The opacity of the band from each bucket's lowest to its highest reading, faint enough that
/// the line through the means reads first.
const BAND_OPACITY: f32 = 0.25;

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

/// The fewest buckets a chart's span divides into, about one for every three pixels of an
/// overview chart, so the line through their means stays readable where one reading a pixel
/// would not. The bucket lengths below put between 180 and 450 buckets in any span of three
/// minutes or more.
const BUCKETS_PER_SPAN_MIN: u64 = 180;
/// The bucket lengths in seconds, each dividing a minute, so buckets start on round seconds.
const BUCKET_SECONDS: [u64; 8] = [1, 2, 5, 10, 15, 20, 30, 60];
/// How far before the start of its second a reading's position may fall and still count in that
/// second. A position is an f32 fraction of the window, whose rounding can put a reading taken
/// on a whole second, as the self-monitoring reports are, a few microseconds before it, in some
/// windows and not others, so it would hop between buckets as the window slides.
const SECOND_TOLERANCE: f64 = 0.001;

/// The stretches of the clock a chart averages its readings over, as Grafana and Netdata do,
/// sized to the chart's span and pinned to multiples of their length on the clock. A reading
/// keeps its bucket and neighbours as the window slides, so a bucket's mean never changes once
/// the bucket is whole. Only buckets whole inside the window, and whole on one side of the join
/// where full-detail readings follow thinned older ones, are drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Buckets {
    window: TimeWindow,
    length_seconds: u64,
    /// The first second of the first bucket whole inside the window.
    drawn_first: u64,
    /// The second after the last bucket whole inside the window and past its newest second.
    drawn_end: u64,
    /// The first second of full-detail readings, before which readings are thinned.
    detail_first: Option<u64>,
}

impl Buckets {
    fn of(window: TimeWindow, detail_first: Option<u64>) -> Self {
        assert!(window.span > 0);
        let length_seconds = BUCKET_SECONDS
            .into_iter()
            .rev()
            .find(|length| length * BUCKETS_PER_SPAN_MIN <= window.span)
            .unwrap_or(1);
        let drawn_first = window.first.div_ceil(length_seconds) * length_seconds;
        // The window's newest second may still gain a reading from a tick that ran late, so a
        // bucket waits for the second after it. The division floors, so a bucket the window's
        // end cuts is not drawn.
        let newest = window.first + window.span - 1;
        let drawn_end = newest / length_seconds * length_seconds;
        assert!(drawn_first <= drawn_end + length_seconds);

        Self {
            window,
            length_seconds,
            drawn_first,
            drawn_end,
            detail_first,
        }
    }

    /// The bucket index of a reading at `position` across the window, or `None` when a window
    /// edge or the join of thinned and full-detail readings cuts the bucket, which then still
    /// loses or gains readings as the window slides.
    fn bucket_index(&self, position: f32) -> Option<u64> {
        assert!(position.is_finite());
        let span = match u32::try_from(self.window.span) {
            Ok(span) => f64::from(span),
            Err(error) => panic!("a chart spans at most a few hours of seconds: {error}"),
        };
        // Floors, so a reading belongs to the second it was taken in.
        let offset = (f64::from(position) * span + SECOND_TOLERANCE).floor();
        if offset < 0.0 {
            return None;
        }

        let Some(offset) = num_traits::ToPrimitive::to_u64(&offset) else {
            panic!("a finite offset of a few hours of seconds fits u64");
        };
        let second = self.window.first + offset;
        if second < self.drawn_first {
            return None;
        }
        if second >= self.drawn_end {
            return None;
        }
        // Floors, so the index counts whole bucket lengths since the clock's zero.
        let index = second / self.length_seconds;
        if let Some(join) = self.detail_first {
            let start = index * self.length_seconds;
            if start < join {
                if join < start + self.length_seconds {
                    return None;
                }
            }
        }
        Some(index)
    }
}

/// The bucket index of the reading at `index` of `positions`, or `None` when the chart does not
/// draw it. The lines and the value axis both ask here, so the axis spans exactly the readings
/// drawn. A panel with no clock draws each reading as its own bucket.
fn drawn_bucket_index(
    buckets: Option<Buckets>,
    positions: Option<&[f32]>,
    index: usize,
) -> Option<u64> {
    if let Some(buckets) = buckets {
        if let Some(positions) = positions {
            return buckets.bucket_index(positions[index]);
        }
    }

    match u64::try_from(index) {
        Ok(index) => Some(index),
        Err(error) => panic!("an index in view fits u64: {error}"),
    }
}

/// Where a reading or a bucket's mean sits on the plot: the top of its line and the bottom of
/// its area.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RunPoint {
    top: gpui::Point<Pixels>,
    bottom: gpui::Point<Pixels>,
}

/// Where one bucket of a run sits on the plot: its mean as the line and area draw it, and its
/// band from its highest reading at the top to its lowest at the bottom.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BucketPoint {
    line: RunPoint,
    band: RunPoint,
}

/// The running totals of one bucket's readings on the plot.
#[derive(Clone, Copy)]
struct BucketSum {
    x: f32,
    top: f32,
    bottom: f32,
    highest: f32,
    lowest: f32,
    count: u16,
}

impl BucketSum {
    fn of(reading: RunPoint) -> Self {
        let top = f32::from(reading.top.y);
        Self {
            x: f32::from(reading.top.x),
            top,
            bottom: f32::from(reading.bottom.y),
            highest: top,
            lowest: top,
            count: 1,
        }
    }

    fn add(&mut self, reading: RunPoint) {
        let top = f32::from(reading.top.y);
        self.x += f32::from(reading.top.x);
        self.top += top;
        self.bottom += f32::from(reading.bottom.y);
        // The plot's y grows downwards, so the highest reading has the least y.
        self.highest = self.highest.min(top);
        self.lowest = self.lowest.max(top);
        self.count += 1;
    }

    fn point(self) -> BucketPoint {
        assert!(self.count > 0);
        let count = f32::from(self.count);
        let x = px(self.x / count);

        BucketPoint {
            line: RunPoint {
                top: point(x, px(self.top / count)),
                bottom: point(x, px(self.bottom / count)),
            },
            band: RunPoint {
                top: point(x, px(self.highest)),
                bottom: point(x, px(self.lowest)),
            },
        }
    }
}

/// The drawn buckets of one run, in time order. Each holds the readings of the run in one
/// bucket, so a gap or a change of segment inside a bucket splits it.
fn bucket_points(
    values: &[Option<(f32, f32)>],
    range: std::ops::Range<usize>,
    geometry: &Geometry<'_>,
) -> Vec<BucketPoint> {
    assert!(!range.is_empty());
    let mut points = Vec::with_capacity(range.len());
    let mut open: Option<(u64, BucketSum)> = None;
    for index in range {
        let Some((top, bottom)) = values[index] else {
            panic!("a run holds only readings");
        };
        let Some(bucket) = geometry.bucket_index(index) else {
            continue;
        };

        let reading = RunPoint {
            top: geometry.point(index, top),
            bottom: geometry.point(index, bottom),
        };
        open = match open {
            Some((current, mut sum)) if current == bucket => {
                sum.add(reading);
                Some((current, sum))
            }
            Some((_, sum)) => {
                points.push(sum.point());
                Some((bucket, BucketSum::of(reading)))
            }
            None => Some((bucket, BucketSum::of(reading))),
        };
    }
    if let Some((_, sum)) = open {
        points.push(sum.point());
    }
    points
}

/// The area between a run's line and its base, as two triangles between each two readings in a
/// row. Building the triangles here skips the general tessellation of a `PathBuilder` fill,
/// which costs about seven times the line itself over a 900-reading run.
fn run_area(points: &[RunPoint]) -> gpui::Path<Pixels> {
    assert!(points.len() >= 2);
    // Texture coordinates of (0, 1) mark a triangle as wholly inside, as gpui's own fills do.
    let inside = (point(0.0, 1.0), point(0.0, 1.0), point(0.0, 1.0));
    let mut area = gpui::Path::new(points[0].top);
    for pair in points.windows(2) {
        let (left, right) = (pair[0], pair[1]);
        area.push_triangle((left.top, right.top, left.bottom), inside);
        area.push_triangle((left.bottom, right.top, right.bottom), inside);
    }
    area
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

/// The range of the readings in drawn buckets, so the axis holds every band the lines draw, and
/// a reading in a bucket not drawn yet cannot rescale the buckets already drawn.
fn axis_extent(panel: &Panel, inspection: Inspection, hidden: &[String], axis: &str) -> (f32, f32) {
    let floor = if axis == "left" {
        panel.minimum.unwrap_or(0.0).min(0.0)
    } else {
        0.0
    };
    let mut extent = (floor, 0.0_f32);
    let buckets = panel_buckets(panel);
    for index in inspection.lower..=inspection.upper {
        if drawn_bucket_index(buckets, panel.positions.as_deref(), index).is_none() {
            continue;
        }
        extent = axis_extent_add(panel, hidden, axis, index, extent);
    }
    extent
}

/// Where the reading at `index` sits across the plot, from 0 at its left to 1 at its right.
fn fraction_in_view(panel: &Panel, inspection: &Inspection, index: usize) -> f32 {
    panel.positions.as_deref().map_or_else(
        || number(index - inspection.lower) / number((inspection.upper - inspection.lower).max(1)),
        |positions| positions[index],
    )
}

/// Widens `extent` by the readings at `index` on `axis`: each unstacked line's own reading, and
/// the stacked lines' positive and negative totals, which their areas reach.
fn axis_extent_add(
    panel: &Panel,
    hidden: &[String],
    axis: &str,
    index: usize,
    (mut minimum, mut maximum): (f32, f32),
) -> (f32, f32) {
    let mut positive = None;
    let mut negative = None;
    for line in &panel.lines {
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
            minimum = minimum.min(value);
            maximum = maximum.max(value);
        }
    }
    for total in [positive, negative].into_iter().flatten() {
        minimum = minimum.min(total);
        maximum = maximum.max(total);
    }
    (minimum, maximum)
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
        // Method: calculate domains from one actual projection with directional and power data,
        // read at 1005, so the five-second bucket of the reading at 1000 is whole and drawn.
        let selection = serde_json::json!({"second": 1005, "context_last_second": 1005,
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

    /// The geometry of a 900-pixel chart of the 900 seconds from `first`, one pixel a second,
    /// from 0 at its bottom to 100 at its top, whose readings in view start at `lower`.
    fn live_geometry(first: u64, lower: usize, positions: &[f32]) -> Geometry<'_> {
        Geometry {
            bounds: Bounds::new(point(px(0.0), px(0.0)), size(px(900.0), px(100.0))),
            minimum: 0.0,
            maximum: 100.0,
            count: positions.len() - lower,
            positions: Some(positions),
            segments: None,
            lower,
            buckets: Some(Buckets::of(TimeWindow { first, span: 900 }, None)),
        }
    }

    /// Where readings 0.3 seconds into each second of a window of `span` seconds sit across it,
    /// as a tick lands.
    fn tick_positions(span: u16) -> Vec<f32> {
        (0..span)
            .map(|offset| (f32::from(offset) + 0.3) / f32::from(span))
            .collect()
    }

    fn live_positions() -> Vec<f32> {
        tick_positions(900)
    }

    /// A reading that turns from second to second, so each bucket's mean, highest and lowest
    /// reading differ.
    fn varying_reading(second: u64) -> f32 {
        f32::from(u16::try_from(second * 37 % 101).unwrap())
    }

    /// A one-line CPU chart of `values` across the 900 seconds from `first`.
    fn live_panel(first: u64, values: &[Option<f32>]) -> Panel {
        let mut panel: Panel = serde_json::from_value(serde_json::json!({
            "id":"cpu", "title":"CPU", "unit":"%", "kind":"line",
            "positions": live_positions(), "lines":[
                {"name":"All cores", "color":"#5794f2", "values": values}]}))
        .unwrap();
        panel.window = Some(TimeWindow { first, span: 900 });
        panel
    }

    const LIVE_INSPECTION: Inspection = Inspection {
        lower: 0,
        selected: 899,
        upper: 899,
    };

    /// Each drawn bucket as the place of its mean, its base, and its highest and lowest reading.
    fn places(points: &[BucketPoint]) -> Vec<[f32; 5]> {
        points
            .iter()
            .map(|sample| {
                [
                    f32::from(sample.line.top.x),
                    f32::from(sample.line.top.y),
                    f32::from(sample.line.bottom.y),
                    f32::from(sample.band.top.y),
                    f32::from(sample.band.bottom.y),
                ]
            })
            .collect()
    }

    fn same_places(places: &[[f32; 5]], expected: &[[f32; 5]]) -> bool {
        places.len() == expected.len()
            && places.iter().zip(expected).all(|(place, expected)| {
                place
                    .iter()
                    .zip(expected)
                    .all(|(value, expected)| (value - expected).abs() < 0.01)
            })
    }

    fn assert_places(points: &[BucketPoint], expected: &[[f32; 5]]) {
        let places = places(points);
        assert!(same_places(&places, expected), "{places:?}");
    }

    /// The buckets the 900-second window from `first` draws of varying readings, each with its
    /// place on an x axis pinned to the clock rather than the window.
    fn clock_places(first: u64) -> Vec<[f32; 5]> {
        let positions = live_positions();
        let values: Vec<_> = (first..first + 900)
            .map(|second| Some((varying_reading(second), 0.0)))
            .collect();
        let clock = f32::from(u16::try_from(first - 1000).unwrap());

        let points = bucket_points(&values, 0..900, &live_geometry(first, 0, &positions));

        places(&points)
            .into_iter()
            .map(|[x, top, bottom, high, low]| [clock + x, top, bottom, high, low])
            .collect()
    }

    #[test]
    fn a_line_passes_through_the_mean_of_each_bucket_of_five_seconds() {
        // Goal: a 15-minute chart draws each five seconds of the clock as the mean of its
        // readings, over a base that is the mean of theirs, with a band from the lowest to the
        // highest reading.
        // Method: place ten readings, the last five over a base of 10, across seconds 1100 to
        // 1109 of a window from 1000, and read the two buckets on a chart one pixel a second and
        // one pixel a unit.
        let positions = live_positions();
        let readings = [10.0, 20.0, 30.0, 40.0, 50.0, 70.0, 70.0, 70.0, 70.0, 100.0];
        let mut values = vec![None; 900];
        for (offset, reading) in (100..110).zip(readings) {
            let base = if offset < 105 { 0.0 } else { 10.0 };
            values[offset] = Some((reading, base));
        }

        let points = bucket_points(&values, 100..110, &live_geometry(1000, 0, &positions));

        assert_places(
            &points,
            &[
                [102.3, 70.0, 100.0, 50.0, 90.0],
                [107.3, 24.0, 90.0, 0.0, 30.0],
            ],
        );
    }

    #[test]
    fn a_band_spans_each_bucket_from_its_lowest_to_its_highest_reading() {
        // Goal: a one-second spike that a bucket's mean flattens still shows at its full height.
        // Method: put one reading of 90 among readings of 10 in seconds 1100 to 1104 and read
        // the band of their bucket.
        let positions = live_positions();
        let mut values = vec![None; 900];
        for (offset, reading) in (100..105).zip([10.0, 10.0, 90.0, 10.0, 10.0]) {
            values[offset] = Some((reading, 0.0));
        }

        let points = bucket_points(&values, 100..105, &live_geometry(1000, 0, &positions));

        assert_places(&points, &[[102.3, 74.0, 100.0, 10.0, 90.0]]);
    }

    #[test]
    fn a_bucket_a_window_edge_cuts_is_not_drawn() {
        // Goal: a bucket missing readings that left the window, or still waiting for readings,
        // is not drawn, so no drawn bucket changes as readings come and go.
        // Method: draw a window from 1002 to 1901, whose whole buckets run from 1005 to 1899,
        // and read the first and last drawn means and the count of buckets.
        let places = clock_places(1002);

        assert_eq!(places.len(), 179);
        assert!((places[0][0] - 7.3).abs() < 0.01, "{:?}", places[0]);
        assert!((places[178][0] - 897.3).abs() < 0.01, "{:?}", places[178]);
    }

    #[test]
    fn a_bucket_waits_for_the_second_after_it() {
        // Goal: a tick that runs late can add a second reading to the newest second, so the
        // bucket that ends with it is not drawn until a later second has readings.
        // Method: draw a window from 1000 to 1899, whose newest bucket ends with its newest
        // second, and read the last drawn mean, the mean place of 1890 to 1894.
        let places = clock_places(1000);

        assert_eq!(places.len(), 179);
        assert!((places[178][0] - 892.3).abs() < 0.01, "{:?}", places[178]);
    }

    #[test]
    fn a_drawn_bucket_stays_put_as_the_window_slides() {
        // Goal: a bucket once drawn keeps its place, mean and band while readings arrive, and
        // leaves the chart only whole, at its oldest edge.
        // Method: slide a window one second at a time across three buckets and check, for each
        // two windows in a row, that the second draws every bucket of the first but its oldest,
        // and the first every bucket of the second but its newest.
        for first in 1000..1015 {
            let before = clock_places(first);
            let after = clock_places(first + 1);

            let held = |place: &[f32; 5], places: &[[f32; 5]]| {
                places.iter().any(|other| same_places(&[*place], &[*other]))
            };
            for place in &before[1..] {
                assert!(held(place, &after), "{place:?} of {first} changed");
            }
            for place in &after[..after.len() - 1] {
                assert!(held(place, &before), "{place:?} of {} is new", first + 1);
            }
        }
    }

    #[test]
    fn a_reading_on_a_whole_second_keeps_its_bucket_as_the_window_slides() {
        // Goal: a reading taken on or just after a whole second, whose f32 position rounds a
        // hair either side of it, stays in the bucket of its own second in every window.
        // Method: place readings 0 and 10 microseconds into second 5000 as the projection does,
        // in each 900-second window that holds them, directly and through a zoom from an hour,
        // and collect the buckets they fall in.
        let mut buckets = std::collections::BTreeSet::new();
        for fraction in [0.0, 0.000_01] {
            for first in 4101..=5000 {
                let offset = f32::from(u16::try_from(5000 - first).unwrap());
                let window = TimeWindow { first, span: 900 };
                buckets.insert(Buckets::of(window, None).bucket_index((offset + fraction) / 900.0));

                // The zoom from an hour, in the arithmetic the range zoom uses.
                let hour_position = (offset + 2700.0 + fraction) / 3600.0;
                let zoomed = (hour_position * 3600.0 - 2700.0) / 900.0;
                buckets.insert(Buckets::of(window, None).bucket_index(zoomed));
            }
        }

        assert_eq!(buckets, [None, Some(1000)].into_iter().collect());
    }

    #[test]
    fn a_bucket_the_join_of_thinned_and_full_detail_readings_cuts_is_not_drawn() {
        // Goal: a bucket holding both older readings, each standing for several seconds, and
        // full-detail readings is not drawn, since the join moves a second each second.
        // Method: in a 30-minute window from 1000, whose buckets are ten seconds long, put the
        // join at 1905 and read the buckets of readings either side of the bucket it cuts.
        let buckets = Buckets::of(
            TimeWindow {
                first: 1000,
                span: 1800,
            },
            Some(1905),
        );

        let found: Vec<_> = [1899_u16, 1900, 1909, 1910]
            .into_iter()
            .map(|second| buckets.bucket_index((f32::from(second - 1000) + 0.3) / 1800.0))
            .collect();

        assert_eq!(found, vec![Some(189), None, None, Some(191)]);
    }

    #[test]
    fn a_chart_scrolled_through_its_readings_buckets_each_by_its_own_second() {
        // Goal: a chart whose first reading in view is not its first reading still puts each
        // reading in the bucket of the second it was taken in.
        // Method: view the readings of a window from 1000 from the 102nd, place five readings in
        // seconds 1102 to 1106, and read the two buckets they straddle.
        let positions = live_positions();
        let values: Vec<_> = [10.0, 20.0, 30.0, 40.0, 50.0]
            .into_iter()
            .map(|reading| Some((reading, 0.0)))
            .collect();

        let points = bucket_points(&values, 0..5, &live_geometry(1000, 102, &positions));

        assert_places(
            &points,
            &[
                [103.3, 80.0, 100.0, 70.0, 90.0],
                [105.8, 55.0, 100.0, 50.0, 60.0],
            ],
        );
    }

    #[test]
    fn a_reading_a_hair_before_the_window_is_not_drawn() {
        // Goal: a reading the zoomed view keeps just past its oldest edge is left out, not a
        // crash. Method: read the bucket of a reading half a millisecond before the window.
        let buckets = Buckets::of(
            TimeWindow {
                first: 1000,
                span: 900,
            },
            None,
        );

        assert_eq!(buckets.bucket_index(-0.0005 / 900.0 * 1000.0), None);
    }

    #[test]
    fn a_panel_with_no_clock_draws_each_reading_on_its_own() {
        // Goal: a chart that knows no seconds draws its readings as they are.
        // Method: draw three readings of a geometry with no buckets.
        let positions = [0.0, 0.5, 1.0];
        let mut geometry = live_geometry(1000, 0, &positions);
        geometry.buckets = None;
        let values = [Some((10.0, 0.0)), Some((20.0, 0.0)), Some((30.0, 0.0))];

        let points = bucket_points(&values, 0..3, &geometry);

        assert_places(
            &points,
            &[
                [0.0, 90.0, 100.0, 90.0, 90.0],
                [450.0, 80.0, 100.0, 80.0, 80.0],
                [900.0, 70.0, 100.0, 70.0, 70.0],
            ],
        );
    }

    #[test]
    fn a_range_of_five_minutes_draws_each_reading_on_its_own() {
        // Goal: a range short enough for a few pixels a second shows each reading as the
        // collector recorded it, with no bucket mean.
        // Method: draw a reading in each second of a 300-second window on a 900-pixel chart and
        // count the points, all but the newest second, which waits for the next.
        let positions = tick_positions(300);
        let mut geometry = live_geometry(1000, 0, &positions);
        geometry.buckets = Some(Buckets::of(
            TimeWindow {
                first: 1000,
                span: 300,
            },
            None,
        ));
        let values: Vec<_> = (1000..1300)
            .map(|second| Some((varying_reading(second), 0.0)))
            .collect();

        let points = bucket_points(&values, 0..300, &geometry);

        assert_eq!(points.len(), 299);
        assert!((f32::from(points[298].line.top.x) - 894.9).abs() < 0.01);
    }

    #[test]
    fn an_hour_draws_a_point_every_twenty_seconds() {
        // Goal: a long range keeps to about one bucket for every three pixels, so its line
        // stays as readable as the 15-minute chart's.
        // Method: draw a reading in each second of a 3600-second window on a 900-pixel chart and
        // read the spacing of its points.
        let positions = tick_positions(3600);
        let mut geometry = live_geometry(3600, 0, &positions);
        geometry.buckets = Some(Buckets::of(
            TimeWindow {
                first: 3600,
                span: 3600,
            },
            None,
        ));
        let values: Vec<_> = (3600..7200)
            .map(|second| Some((varying_reading(second), 0.0)))
            .collect();

        let points = bucket_points(&values, 0..3600, &geometry);

        assert_eq!(points.len(), 179);
        for pair in points.windows(2) {
            let spacing = f32::from(pair[1].line.top.x) - f32::from(pair[0].line.top.x);
            assert!((spacing - 5.0).abs() < 0.01, "{spacing}");
        }
    }

    #[test]
    fn a_line_area_spans_from_its_highest_reading_to_its_base() {
        // Goal: the filled area under a line reaches each reading and the base beneath it.
        // Method: build the area of three readings and read its bounds.
        let points = [
            RunPoint {
                top: point(px(10.0), px(40.0)),
                bottom: point(px(10.0), px(100.0)),
            },
            RunPoint {
                top: point(px(11.0), px(5.0)),
                bottom: point(px(11.0), px(100.0)),
            },
            RunPoint {
                top: point(px(12.0), px(70.0)),
                bottom: point(px(12.0), px(100.0)),
            },
        ];

        let area = run_area(&points);

        assert_eq!(
            area.bounds,
            Bounds::from_corners(point(px(10.0), px(5.0)), point(px(12.0), px(100.0)))
        );
    }

    #[test]
    fn the_value_axis_reaches_a_one_second_spike() {
        // Goal: the axis holds every band the chart draws, so a one-second spike is not cut off
        // at the top of the chart.
        // Method: put one reading of 90 among readings of 10 and read the axis.
        let values: Vec<_> = (0..900)
            .map(|offset| Some(if offset == 500 { 90.0 } else { 10.0 }))
            .collect();

        let (_, maximum) = domain(&live_panel(1000, &values), LIVE_INSPECTION, &[]);

        assert!(maximum >= 90.0, "{maximum}");
    }

    #[test]
    fn the_value_axis_ignores_a_reading_whose_bucket_is_not_drawn() {
        // Goal: a reading in a bucket a window edge cuts cannot rescale the buckets drawn.
        // Method: put a spike in 1901, the cut newest bucket of the window from 1002, and read
        // the axis of a steady chart.
        let values: Vec<_> = (0..900)
            .map(|offset| Some(if offset == 899 { 90.0 } else { 10.0 }))
            .collect();

        let (_, maximum) = domain(&live_panel(1002, &values), LIVE_INSPECTION, &[]);

        assert!(maximum < 90.0, "{maximum}");
    }
}
