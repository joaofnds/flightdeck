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
            sliding: panel.sliding,
        };
        paint(line, &values, &geometry);
    }
}

/// The buckets of a panel with a clock, sized for its card, or for an overview card until its
/// card has been laid out.
fn panel_buckets(panel: &Panel) -> Option<Buckets> {
    let chart_width = panel.chart_width.get().unwrap_or(CHART_WIDTH_DEFAULT);
    panel
        .window
        .map(|window| Buckets::of(window, panel.detail_first, chart_width))
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
    /// Whether readings still leave at the window's start and arrive at its end.
    pub(crate) sliding: bool,
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
    let ends = RunEnds::of(&range, values.len(), geometry);
    let curve = drawn_curve(&bucket_points(values, range, geometry), ends);
    if curve.points.is_empty() {
        return;
    }

    if curve.points.len() == 1 {
        let position = curve.points[0].top;
        window.paint_quad(fill(
            Bounds::new(position - point(px(1.5), px(1.5)), size(px(3.0), px(3.0))),
            rgb(tint),
        ));
        return;
    }

    let dashed = line.dashed == Some(true);
    let stacked = stacked(panel, line);
    let width = if stacked { px(1.0) } else { px(2.0) };
    let mut stroke = PathBuilder::stroke(width);
    if dashed {
        stroke = stroke.dash_array(&[px(6.0), px(4.0)]);
    }
    let top: Vec<_> = curve.points.iter().map(|sample| sample.top).collect();
    smooth(&mut stroke, &top, &curve.tangents_top, true);

    let default_opacity = if stacked { 0.45 } else { 0.12 };
    let opacity = line.fill.or(panel.fill).unwrap_or(default_opacity);
    if !dashed {
        if opacity > 0.0 {
            let bottom: Vec<_> = curve
                .points
                .iter()
                .rev()
                .map(|sample| sample.bottom)
                .collect();
            let tangents_bottom: Vec<f32> = curve.tangents_bottom.iter().rev().copied().collect();
            let mut area = PathBuilder::fill();
            smooth(&mut area, &top, &curve.tangents_top, true);
            smooth(&mut area, &bottom, &tangents_bottom, false);
            area.close();
            let span = curve
                .points
                .iter()
                .fold(None, |span: Option<(Pixels, Pixels)>, sample| {
                    let (low, high) = span.unwrap_or((sample.top.y, sample.bottom.y));
                    Some((low.min(sample.top.y), high.max(sample.bottom.y)))
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

/// The fewest pixels of its card's width a bucket spans, so a curve looks as calm in a wide card
/// as in a narrow one. It gives a 15-minute chart in an overview card thirty buckets of thirty
/// seconds, the sampling interval of the approved prototype, and a card three times as wide
/// ten-second buckets, as Grafana draws a 15-minute panel of that width.
const BUCKET_WIDTH_MIN: f32 = 11.0;
/// The width a chart's buckets are sized for before its card is laid out, an overview card's in
/// the 1320-pixel window.
const CHART_WIDTH_DEFAULT: f32 = 360.0;
/// The bucket lengths in seconds, each dividing an hour, so buckets start on round seconds, and
/// each at most a third longer than the one before from 4 seconds to 10 minutes, so a bucket
/// is drawn between 11 and about 15 pixels wide.
const BUCKET_SECONDS: [u64; 30] = [
    1, 2, 3, 4, 5, 6, 8, 10, 12, 15, 20, 24, 30, 40, 45, 60, 75, 90, 120, 150, 180, 240, 300, 360,
    450, 600, 900, 1200, 1800, 3600,
];
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
    /// The buckets of `window` in a card `chart_width` pixels wide: the shortest length that
    /// draws each bucket at least `BUCKET_WIDTH_MIN` pixels wide, or the longest length when
    /// none does.
    fn of(window: TimeWindow, detail_first: Option<u64>, chart_width: f32) -> Self {
        assert!(window.span > 0);
        assert!(chart_width.is_finite());
        assert!(chart_width >= 0.0);
        let span = match u32::try_from(window.span) {
            Ok(span) => f64::from(span),
            Err(error) => panic!("a chart spans at most a few hours of seconds: {error}"),
        };
        let length_seconds = BUCKET_SECONDS
            .into_iter()
            .find(|length| {
                let length = match u32::try_from(*length) {
                    Ok(length) => f64::from(length),
                    Err(error) => panic!("a bucket is at most an hour long: {error}"),
                };
                length * f64::from(chart_width) >= f64::from(BUCKET_WIDTH_MIN) * span
            })
            .unwrap_or(BUCKET_SECONDS[BUCKET_SECONDS.len() - 1]);
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

/// The running totals of one bucket's readings on the plot.
#[derive(Clone, Copy)]
struct BucketSum {
    x: f32,
    top: f32,
    bottom: f32,
    count: u16,
}

impl BucketSum {
    fn of(reading: RunPoint) -> Self {
        Self {
            x: f32::from(reading.top.x),
            top: f32::from(reading.top.y),
            bottom: f32::from(reading.bottom.y),
            count: 1,
        }
    }

    fn add(&mut self, reading: RunPoint) {
        self.x += f32::from(reading.top.x);
        self.top += f32::from(reading.top.y);
        self.bottom += f32::from(reading.bottom.y);
        self.count += 1;
    }

    fn point(self) -> RunPoint {
        assert!(self.count > 0);
        let count = f32::from(self.count);
        let x = px(self.x / count);

        RunPoint {
            top: point(x, px(self.top / count)),
            bottom: point(x, px(self.bottom / count)),
        }
    }
}

/// The mean of each drawn bucket of one run, in time order. Each holds the readings of the run
/// in one bucket, so a gap or a change of segment inside a bucket splits it.
fn bucket_points(
    values: &[Option<(f32, f32)>],
    range: std::ops::Range<usize>,
    geometry: &Geometry<'_>,
) -> Vec<RunPoint> {
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

/// Whether each end of a run meets an edge of the view beyond which readings still leave or
/// arrive, which only a sliding view has.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RunEnds {
    head_open: bool,
    tail_open: bool,
}

impl RunEnds {
    /// The ends of `run`, as offsets into the `count` readings in view. A run that starts at the
    /// oldest reading in view, or in a bucket not drawn, meets the window's oldest edge or the
    /// join, which will cut its first drawn bucket. A run that reaches the newest reading, or
    /// the one before it while a late tick has not landed yet, still gains buckets.
    fn of(run: &std::ops::Range<usize>, count: usize, geometry: &Geometry<'_>) -> Self {
        assert!(!run.is_empty());
        assert!(run.end <= count);
        if !geometry.sliding {
            return Self {
                head_open: false,
                tail_open: false,
            };
        }

        let head_open = if run.start == 0 {
            true
        } else {
            geometry.bucket_index(run.start).is_none()
        };
        let tail_open = if run.end + 1 >= count {
            true
        } else {
            geometry.bucket_index(run.end - 1).is_none()
        };
        Self {
            head_open,
            tail_open,
        }
    }
}

/// The stretch of one run's curve that a chart draws, with the slope of its top and bottom
/// edges at each point. A lone point draws as a dot.
#[derive(Debug, Default, PartialEq)]
struct DrawnCurve {
    points: Vec<RunPoint>,
    tangents_top: Vec<f32>,
    tangents_bottom: Vec<f32>,
}

/// The stretch of a run's curve through the means of its whole buckets, `kept`, that no later
/// reading can change, so a curve once drawn keeps its shape as the window slides, and leaves
/// the chart only whole, at its oldest edge.
///
/// The slope at each mean depends on the means either side of it. At an open head, the oldest
/// kept mean will lose its bucket to the slide, and with it the slope of the mean after it, so
/// the oldest mean shapes the stretch after it without drawing its own. At an open tail, the
/// newest kept mean waits for the bucket still to come before its stretch is drawn. A run that
/// would draw one mean of several draws none, since that mean ends a stretch still to come.
fn drawn_curve(kept: &[RunPoint], ends: RunEnds) -> DrawnCurve {
    let mut drawn = 0..kept.len();
    if ends.head_open {
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
    if drawn.is_empty() {
        return DrawnCurve::default();
    }

    if kept.len() == 1 {
        return DrawnCurve {
            points: kept.to_vec(),
            tangents_top: vec![0.0],
            tangents_bottom: vec![0.0],
        };
    }

    let tangents_of = |side: fn(&RunPoint) -> gpui::Point<Pixels>| {
        let points: Vec<_> = kept.iter().map(side).collect();
        monotone_tangents(&points)
    };
    let tangents_top = tangents_of(|sample| sample.top);
    let tangents_bottom = tangents_of(|sample| sample.bottom);
    DrawnCurve {
        points: kept[drawn.clone()].to_vec(),
        tangents_top: tangents_top[drawn.clone()].to_vec(),
        tangents_bottom: tangents_bottom[drawn].to_vec(),
    }
}

/// Draws a cubic through the points with the slope given at each, which `monotone_tangents`
/// chooses so a curve never overshoots the means it joins.
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

/// The slope of a curve through `points` at each of them. Each slope depends on its two
/// neighbours alone, so a point leaving or joining the run reshapes no stretch beyond the one
/// beside it. Fritsch and Carlson keep a cubic monotone while each end's slope is at most three
/// times the slope it spans, so the limit holds the curve between the points it joins.
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

/// The range of the means of drawn buckets, so the axis spans the curves the lines draw, and a
/// reading in a bucket not drawn yet cannot rescale the buckets already drawn.
fn axis_extent(panel: &Panel, inspection: Inspection, hidden: &[String], axis: &str) -> (f32, f32) {
    let floor = if axis == "left" {
        panel.minimum.unwrap_or(0.0).min(0.0)
    } else {
        0.0
    };
    let mut extent = (floor, 0.0_f32);
    let buckets = panel_buckets(panel);
    // One sum and count per line, then the stacked positive and negative totals.
    let mut slots = vec![(0.0_f32, 0.0_f32); panel.lines.len() + 2];
    let mut current = None;
    for index in inspection.lower..=inspection.upper {
        let Some(bucket) = drawn_bucket_index(buckets, panel.positions.as_deref(), index) else {
            continue;
        };

        if current.is_some_and(|current| current != bucket) {
            extent = axis_extent_flush(&mut slots, extent);
        }
        current = Some(bucket);
        axis_extent_add(panel, hidden, axis, index, &mut slots);
    }
    axis_extent_flush(&mut slots, extent)
}

/// Where the reading at `index` sits across the plot, from 0 at its left to 1 at its right.
fn fraction_in_view(panel: &Panel, inspection: &Inspection, index: usize) -> f32 {
    panel.positions.as_deref().map_or_else(
        || number(index - inspection.lower) / number((inspection.upper - inspection.lower).max(1)),
        |positions| positions[index],
    )
}

/// Adds the readings at `index` on `axis` to the slots of the bucket they fall in: each
/// unstacked line's own reading, and the stacked lines' positive and negative totals, which
/// their areas reach.
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

    /// The width of an overview card in the 1320-pixel window.
    const CARD_WIDTH: f32 = 360.0;

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
        // read at 1020, so the thirty-second bucket of the reading at 1000 is whole and drawn.
        let selection = serde_json::json!({"second": 1020, "context_last_second": 1020,
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
    /// from 0 at its bottom to 100 at its top, whose readings in view start at `lower`, held
    /// still.
    fn live_geometry(first: u64, lower: usize, positions: &[f32]) -> Geometry<'_> {
        Geometry {
            bounds: Bounds::new(point(px(0.0), px(0.0)), size(px(900.0), px(100.0))),
            minimum: 0.0,
            maximum: 100.0,
            count: positions.len() - lower,
            positions: Some(positions),
            segments: None,
            lower,
            buckets: Some(Buckets::of(
                TimeWindow { first, span: 900 },
                None,
                CARD_WIDTH,
            )),
            sliding: false,
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

    /// A reading that turns from second to second, so the means of buckets differ.
    fn varying_reading(second: u64) -> f32 {
        f32::from(u16::try_from(second * 37 % 101).unwrap())
    }

    /// Varying readings in each second of the 900 from `first`.
    fn varying_readings(first: u64) -> Vec<Option<(f32, f32)>> {
        (first..first + 900)
            .map(|second| Some((varying_reading(second), 0.0)))
            .collect()
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

    /// Each bucket mean as its place across the chart, its height, and the height of its base.
    fn places(points: &[RunPoint]) -> Vec<[f32; 3]> {
        points
            .iter()
            .map(|sample| {
                [
                    f32::from(sample.top.x),
                    f32::from(sample.top.y),
                    f32::from(sample.bottom.y),
                ]
            })
            .collect()
    }

    fn same<const N: usize>(place: &[f32; N], expected: &[f32; N]) -> bool {
        place
            .iter()
            .zip(expected)
            .all(|(value, expected)| (value - expected).abs() < 0.01)
    }

    fn assert_places(points: &[RunPoint], expected: &[[f32; 3]]) {
        let places = places(points);
        assert_eq!(places.len(), expected.len(), "{places:?}");
        for (place, expected) in places.iter().zip(expected) {
            assert!(same(place, expected), "{places:?}");
        }
    }

    /// The bucket means the 900-second window from `first` draws of varying readings, each with
    /// its place on an x axis pinned to the clock rather than the window.
    fn clock_places(first: u64) -> Vec<[f32; 3]> {
        let positions = live_positions();
        let clock = f32::from(u16::try_from(first - 1000).unwrap());

        let points = bucket_points(
            &varying_readings(first),
            0..900,
            &live_geometry(first, 0, &positions),
        );

        places(&points)
            .into_iter()
            .map(|[x, top, bottom]| [clock + x, top, bottom])
            .collect()
    }

    /// Each stretch of curve the sliding 900-second window from `first` draws of varying
    /// readings, as its two ends on an x axis pinned to the clock and the slope at each.
    fn clock_stretches(first: u64) -> Vec<[f32; 6]> {
        let positions = live_positions();
        let clock = f32::from(u16::try_from(first - 1000).unwrap());
        let mut geometry = live_geometry(first, 0, &positions);
        geometry.sliding = true;
        let ends = RunEnds::of(&(0..900), 900, &geometry);

        let curve = drawn_curve(
            &bucket_points(&varying_readings(first), 0..900, &geometry),
            ends,
        );

        (1..curve.points.len())
            .map(|index| {
                let (left, right) = (curve.points[index - 1].top, curve.points[index].top);
                [
                    clock + f32::from(left.x),
                    f32::from(left.y),
                    clock + f32::from(right.x),
                    f32::from(right.y),
                    curve.tangents_top[index - 1],
                    curve.tangents_top[index],
                ]
            })
            .collect()
    }

    /// `count` bucket means in a row, ten pixels apart, at heights that turn up and down.
    fn kept_means(count: u8) -> Vec<RunPoint> {
        (0..count)
            .map(|index| {
                let x = px(10.0 * f32::from(index));
                let y = px(f32::from(index % 3) * 20.0);
                RunPoint {
                    top: point(x, y),
                    bottom: point(x, px(100.0)),
                }
            })
            .collect()
    }

    #[test]
    fn a_line_passes_through_the_mean_of_each_bucket_of_thirty_seconds() {
        // Goal: a 15-minute chart draws each thirty seconds of the clock as the mean of its
        // readings, over a base that is the mean of theirs.
        // Method: place sixty readings across seconds 1080 to 1139 of a window from 1000, the
        // first thirty rising from 0 to 29 and the last thirty at 70 over a base of 10, and read
        // the two buckets on a chart one pixel a second and one pixel a unit.
        let positions = live_positions();
        let mut values = vec![None; 900];
        for (offset, reading) in (80..110).zip(0_u8..) {
            values[offset] = Some((f32::from(reading), 0.0));
        }
        for value in &mut values[110..140] {
            *value = Some((70.0, 10.0));
        }

        let points = bucket_points(&values, 80..140, &live_geometry(1000, 0, &positions));

        assert_places(&points, &[[94.8, 85.5, 100.0], [124.8, 30.0, 90.0]]);
    }

    #[test]
    fn a_bucket_a_window_edge_cuts_is_not_drawn() {
        // Goal: a bucket missing readings that left the window, or still waiting for readings,
        // is not drawn, so no drawn bucket changes as readings come and go.
        // Method: draw a window from 1002 to 1901, whose whole buckets run from 1020 to 1889,
        // and read the first and last drawn means and the count of buckets.
        let places = clock_places(1002);

        assert_eq!(places.len(), 29);
        assert!((places[0][0] - 34.8).abs() < 0.01, "{:?}", places[0]);
        assert!((places[28][0] - 874.8).abs() < 0.01, "{:?}", places[28]);
    }

    #[test]
    fn a_bucket_waits_for_the_second_after_it() {
        // Goal: a tick that runs late can add a second reading to the newest second, so the
        // bucket that ends with it is not drawn until a later second has readings.
        // Method: draw a window from 1020 to 1919, whose newest bucket ends with its newest
        // second, and read the last drawn mean, the mean place of 1860 to 1889.
        let places = clock_places(1020);

        assert_eq!(places.len(), 29);
        assert!((places[28][0] - 874.8).abs() < 0.01, "{:?}", places[28]);
    }

    #[test]
    fn a_drawn_bucket_stays_put_as_the_window_slides() {
        // Goal: a bucket once drawn keeps its place and mean while readings arrive, and leaves
        // the chart only whole, at its oldest edge.
        // Method: slide a window one second at a time across three buckets and check, for each
        // two windows in a row, that the second draws every bucket of the first but its oldest,
        // and the first every bucket of the second but its newest.
        for first in 1000..1070 {
            let before = clock_places(first);
            let after = clock_places(first + 1);

            let held = |place: &[f32; 3], places: &[[f32; 3]]| {
                places.iter().any(|other| same(place, other))
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
    fn a_drawn_curve_keeps_its_shape_as_the_window_slides() {
        // Goal: each stretch of smooth curve once drawn keeps its ends and its slopes while
        // readings arrive, and leaves the chart only whole, at its oldest edge, where the slope
        // at each mean depends on the means either side of it.
        // Method: slide a following window one second at a time across three buckets and check,
        // for each two windows in a row, that a stretch of one missing from the other lies past
        // the other's oldest or newest stretch.
        for first in 1000..1070 {
            let before = clock_stretches(first);
            let after = clock_stretches(first + 1);

            let held = |stretch: &[f32; 6], stretches: &[[f32; 6]]| {
                stretches.iter().any(|other| same(stretch, other))
            };
            assert!(!before.is_empty());
            for stretch in &before {
                if !held(stretch, &after) {
                    assert!(
                        stretch[2] <= after[0][0] + 0.01,
                        "{stretch:?} of {first} changed"
                    );
                }
            }
            for stretch in &after {
                if !held(stretch, &before) {
                    let newest = before[before.len() - 1][2];
                    assert!(
                        stretch[0] >= newest - 0.01,
                        "{stretch:?} of {} is new",
                        first + 1
                    );
                }
            }
        }
    }

    #[test]
    fn a_reading_on_a_whole_second_keeps_its_bucket_as_the_window_slides() {
        // Goal: a reading taken on or just after a whole second, whose f32 position rounds a
        // hair either side of it, stays in the bucket of its own second in every window.
        // Method: place readings 0 and 10 microseconds into second 4980, the first of a bucket,
        // as the projection does, in each 900-second window that holds them, directly and
        // through a zoom from an hour, and collect the buckets they fall in.
        let mut buckets = std::collections::BTreeSet::new();
        for fraction in [0.0, 0.000_01] {
            for first in 4081..=4980 {
                let offset = f32::from(u16::try_from(4980 - first).unwrap());
                let window = TimeWindow { first, span: 900 };
                let buckets_of_window = Buckets::of(window, None, CARD_WIDTH);
                buckets.insert(buckets_of_window.bucket_index((offset + fraction) / 900.0));

                // The zoom from an hour, in the arithmetic the range zoom uses.
                let hour_position = (offset + 2700.0 + fraction) / 3600.0;
                let zoomed = (hour_position * 3600.0 - 2700.0) / 900.0;
                buckets.insert(buckets_of_window.bucket_index(zoomed));
            }
        }

        assert_eq!(buckets, [None, Some(166)].into_iter().collect());
    }

    #[test]
    fn a_bucket_the_join_of_thinned_and_full_detail_readings_cuts_is_not_drawn() {
        // Goal: a bucket holding both older readings, each standing for several seconds, and
        // full-detail readings is not drawn, since the join moves a second each second.
        // Method: in a 30-minute window from 1000, whose buckets are a minute long, put the
        // join at 1905 and read the buckets of readings either side of the bucket it cuts.
        let buckets = Buckets::of(
            TimeWindow {
                first: 1000,
                span: 1800,
            },
            Some(1905),
            CARD_WIDTH,
        );

        let found: Vec<_> = [1859_u16, 1860, 1919, 1920]
            .into_iter()
            .map(|second| buckets.bucket_index((f32::from(second - 1000) + 0.3) / 1800.0))
            .collect();

        assert_eq!(found, vec![Some(30), None, None, Some(32)]);
    }

    #[test]
    fn a_chart_scrolled_through_its_readings_buckets_each_by_its_own_second() {
        // Goal: a chart whose first reading in view is not its first reading still puts each
        // reading in the bucket of the second it was taken in.
        // Method: view the readings of a window from 1000 from the 107th, place five readings in
        // seconds 1107 to 1111, and read the two buckets they straddle.
        let positions = live_positions();
        let values: Vec<_> = [10.0, 20.0, 30.0, 40.0, 50.0]
            .into_iter()
            .map(|reading| Some((reading, 0.0)))
            .collect();

        let points = bucket_points(&values, 0..5, &live_geometry(1000, 107, &positions));

        assert_places(&points, &[[108.3, 80.0, 100.0], [110.8, 55.0, 100.0]]);
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
            CARD_WIDTH,
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
                [0.0, 90.0, 100.0],
                [450.0, 80.0, 100.0],
                [900.0, 70.0, 100.0],
            ],
        );
    }

    /// The spacing of the bucket means a 900-pixel plot of a reading in each second of the
    /// `span` seconds from `first` draws, in a card `card_width` pixels wide, and their count.
    fn mean_spacing(first: u64, span: u16, card_width: f32) -> (usize, Vec<f32>) {
        let positions = tick_positions(span);
        let mut geometry = live_geometry(first, 0, &positions);
        let window = TimeWindow {
            first,
            span: u64::from(span),
        };
        geometry.buckets = Some(Buckets::of(window, None, card_width));
        let values: Vec<_> = (first..first + u64::from(span))
            .map(|second| Some((varying_reading(second), 0.0)))
            .collect();

        let points = bucket_points(&values, 0..values.len(), &geometry);

        let spacing = points
            .windows(2)
            .map(|pair| f32::from(pair[1].top.x) - f32::from(pair[0].top.x))
            .collect();
        (points.len(), spacing)
    }

    #[test]
    fn a_range_of_five_minutes_draws_a_mean_every_ten_seconds() {
        // Goal: a short range keeps to about thirty buckets in an overview card, so its curve is
        // as calm as the 15-minute chart's. Method: draw a reading in each second of a
        // 300-second window on a 900-pixel plot and read the count and spacing of its means.
        let (count, spacing) = mean_spacing(1000, 300, CARD_WIDTH);

        assert_eq!(count, 29);
        for gap in spacing {
            assert!((gap - 30.0).abs() < 0.01, "{gap}");
        }
    }

    #[test]
    fn an_hour_draws_a_mean_every_two_minutes() {
        // Goal: a long range keeps to about thirty buckets too. Method: draw a reading in each
        // second of a 3600-second window on a 900-pixel plot and read its means.
        let (count, spacing) = mean_spacing(3600, 3600, CARD_WIDTH);

        assert_eq!(count, 29);
        for gap in spacing {
            assert!((gap - 30.0).abs() < 0.01, "{gap}");
        }
    }

    #[test]
    fn a_wider_card_draws_shorter_buckets() {
        // Goal: a chart keeps each bucket about as wide on screen whatever the size of its card,
        // so a wide card shows more detail and a narrow one stays calm. Method: draw a
        // 15-minute window in cards a half, one and three overview cards wide, and read the
        // count and spacing of the means.
        let widths = [CARD_WIDTH / 2.0, CARD_WIDTH, CARD_WIDTH * 3.0];

        let drawn = widths.map(|width| mean_spacing(1020, 900, width));

        for ((count, spacing), (count_expected, gap_expected)) in
            drawn.into_iter().zip([(14, 60.0), (29, 30.0), (89, 10.0)])
        {
            assert_eq!(count, count_expected);
            for gap in spacing {
                assert!((gap - gap_expected).abs() < 0.01, "{gap}");
            }
        }
    }

    #[test]
    fn bucket_lengths_stop_at_a_second_and_at_an_hour() {
        // Goal: a card too wide for whole seconds draws each second, and one too narrow for an
        // hour's bucket draws hours, never a length off the clock's list. Method: size the
        // buckets of a 15-minute window for cards of no width, one pixel and a million pixels.
        let window = TimeWindow {
            first: 1000,
            span: 900,
        };

        let lengths =
            [0.0, 1.0, 1_000_000.0].map(|width| Buckets::of(window, None, width).length_seconds);

        assert_eq!(lengths, [3600, 3600, 1]);
    }

    #[test]
    fn a_card_exactly_wide_enough_keeps_the_shorter_bucket() {
        // Goal: a bucket drawn exactly the narrowest width allowed is still drawn, so the length
        // steps up only once a bucket would fall below it. Method: size the buckets of a
        // 15-minute window for the card where 30 seconds span exactly that width, and for a card
        // a pixel narrower.
        let window = TimeWindow {
            first: 1000,
            span: 900,
        };
        let exact = BUCKET_WIDTH_MIN * 900.0 / 30.0;

        let lengths =
            [exact, exact - 1.0].map(|width| Buckets::of(window, None, width).length_seconds);

        assert_eq!(lengths, [30, 40]);
    }

    #[test]
    fn a_panel_sizes_its_buckets_for_its_card_once_laid_out() {
        // Goal: a chart draws buckets for the card it is laid out in, and for an overview card
        // before its first layout. Method: read the bucket length of a 15-minute chart before
        // layout and after it learns of a card three overview cards wide.
        let panel = live_panel(1000, &[]);
        let before = panel_buckets(&panel).map(|buckets| buckets.length_seconds);

        panel.chart_width.set(Some(CARD_WIDTH * 3.0));
        let after = panel_buckets(&panel).map(|buckets| buckets.length_seconds);

        assert_eq!([before, after], [Some(30), Some(10)]);
    }

    #[test]
    fn a_still_view_draws_a_run_to_both_ends() {
        // Goal: a view that no longer follows the newest second gains and loses no readings, so
        // it draws its whole curve. Method: read the ends of a run across all the readings of a
        // still view.
        let positions = live_positions();
        let geometry = live_geometry(1002, 0, &positions);

        let ends = RunEnds::of(&(0..900), 900, &geometry);

        assert_eq!(
            ends,
            RunEnds {
                head_open: false,
                tail_open: false
            }
        );
    }

    #[test]
    fn a_sliding_run_is_open_where_it_meets_the_moving_edges() {
        // Goal: in a following view, a run that starts in a bucket the window's oldest edge cuts,
        // or reaches the newest reading or the one before it, is open there, and a run between
        // gaps inside whole buckets is closed.
        // Method: in a following window from 1002, read the ends of a run over every reading,
        // one that stops a second short, and one from 1100 to 1199. In a window from 1021, whose
        // newest second makes the bucket from 1890 to 1919 whole, read a run that stops at 1919.
        let positions = live_positions();
        let mut geometry = live_geometry(1002, 0, &positions);
        geometry.sliding = true;
        let mut whole = live_geometry(1021, 0, &positions);
        whole.sliding = true;

        let every = RunEnds::of(&(0..900), 900, &geometry);
        let short = RunEnds::of(&(0..899), 900, &geometry);
        let inside = RunEnds::of(&(98..198), 900, &geometry);
        let late = RunEnds::of(&(0..899), 900, &whole);

        let open = RunEnds {
            head_open: true,
            tail_open: true,
        };
        assert_eq!(every, open);
        assert_eq!(short, open);
        assert_eq!(late, open);
        assert_eq!(
            inside,
            RunEnds {
                head_open: false,
                tail_open: false
            }
        );
    }

    #[test]
    fn an_open_tail_waits_for_the_bucket_after_its_newest_mean() {
        // Goal: the stretch to the newest mean of a growing run is not drawn, since its slope
        // there waits for the bucket still to come. Method: draw four means with an open tail.
        let kept = kept_means(4);

        let curve = drawn_curve(
            &kept,
            RunEnds {
                head_open: false,
                tail_open: true,
            },
        );

        assert_eq!(curve.points, kept[..3].to_vec());
    }

    #[test]
    fn an_open_head_draws_no_stretch_from_its_oldest_mean() {
        // Goal: the oldest mean at the sliding window's edge shapes the stretch after it and
        // draws none of its own, since its bucket will soon be cut. Method: draw four means
        // with an open head, and check the first drawn slope is shaped by the mean before it.
        let kept = kept_means(4);

        let curve = drawn_curve(
            &kept,
            RunEnds {
                head_open: true,
                tail_open: false,
            },
        );

        assert_eq!(curve.points, kept[1..].to_vec());
        let all = drawn_curve(
            &kept,
            RunEnds {
                head_open: false,
                tail_open: false,
            },
        );
        assert!((curve.tangents_top[0] - all.tangents_top[1]).abs() < f32::EPSILON);
    }

    #[test]
    fn a_run_that_would_draw_one_mean_of_several_draws_none() {
        // Goal: a growing run of two means draws nothing yet, since its one drawable mean ends a
        // stretch still to come. Method: draw two means with an open tail.
        let curve = drawn_curve(
            &kept_means(2),
            RunEnds {
                head_open: false,
                tail_open: true,
            },
        );

        assert_eq!(curve, DrawnCurve::default());
    }

    #[test]
    fn a_lone_mean_of_a_closed_run_draws_as_a_dot() {
        // Goal: a run of one bucket between gaps still shows. Method: draw one closed mean.
        let kept = kept_means(1);

        let curve = drawn_curve(
            &kept,
            RunEnds {
                head_open: false,
                tail_open: false,
            },
        );

        assert_eq!(curve.points, kept);
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

    #[test]
    fn the_value_axis_spans_the_means_the_curve_draws() {
        // Goal: the axis fits the curve through the bucket means, so a one-second spike that the
        // mean flattens does not shrink the curve to the bottom of the chart.
        // Method: put one reading of 90 among readings of 10, which raises its bucket's mean to
        // 10 + 80 / 30, and read the axis.
        let values: Vec<_> = (0..900)
            .map(|offset| Some(if offset == 500 { 90.0 } else { 10.0 }))
            .collect();

        let (_, maximum) = domain(&live_panel(1000, &values), LIVE_INSPECTION, &[]);

        assert!(maximum >= 10.0 + 80.0 / 30.0, "{maximum}");
        assert!(maximum < 90.0, "{maximum}");
    }

    #[test]
    fn the_value_axis_ignores_a_reading_whose_bucket_is_not_drawn() {
        // Goal: a reading in a bucket a window edge cuts cannot rescale the buckets drawn.
        // Method: put a run of readings of 90 in 1890 to 1901, the cut newest bucket of the
        // window from 1002, and read the axis of a steady chart.
        let values: Vec<_> = (0..900)
            .map(|offset| Some(if offset >= 888 { 90.0 } else { 10.0 }))
            .collect();

        let (_, maximum) = domain(&live_panel(1002, &values), LIVE_INSPECTION, &[]);

        assert!(maximum < 20.0, "{maximum}");
    }
}
