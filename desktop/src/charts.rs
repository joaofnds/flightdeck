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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

/// The inspection a plot of `panel` draws. Bars draw the selected reading, and every other
/// kind draws the readings in view whatever is selected, so a hover leaves its plot as it was.
#[must_use]
pub fn plot_inspection(panel: &Panel, inspection: Inspection) -> Inspection {
    if matches!(panel.kind.as_str(), "core-bars" | "pressure") {
        return inspection;
    }

    Inspection {
        selected: inspection.upper,
        ..inspection
    }
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
    let inspection = Inspection {
        lower: curve_lower(panel, inspection),
        ..inspection
    };
    stacked_lines(panel, inspection, hidden, |line, values| {
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
        paint(line, values, &geometry);
    });
}

/// Calls `visit` with each shown line of `panel` and its readings in view, each as the top of
/// its area and the base it stands on, which is the line stacked below it or zero.
fn stacked_lines(
    panel: &Panel,
    inspection: Inspection,
    hidden: &[String],
    mut visit: impl FnMut(&crate::data::Line, &[Option<(f32, f32)>]),
) {
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
        visit(line, &values);
    }
}

/// The index of the first reading the curves of `panel` draw from. A point of a chart with a
/// clock averages the readings before it, so the first point in view draws from readings before
/// the window's oldest edge where the panel holds them.
fn curve_lower(panel: &Panel, inspection: Inspection) -> usize {
    let Some(buckets) = panel_buckets(panel) else {
        return inspection.lower;
    };
    let Some(positions) = panel.positions.as_deref() else {
        return inspection.lower;
    };

    // Positions run in time order, so the readings before the first a point averages lead.
    positions[..inspection.lower].partition_point(|position| {
        buckets
            .second_of(*position)
            .is_none_or(|second| second < buckets.lead_first)
    })
}

/// The buckets of a panel with a clock, sized for its card, or for an overview card until its
/// card has been laid out.
fn panel_buckets(panel: &Panel) -> Option<Buckets> {
    let chart_width = panel.chart_width.get().unwrap_or(CHART_WIDTH_DEFAULT);
    panel.window.map(|window| {
        Buckets::of(
            window,
            panel.detail_first,
            panel.reading_seconds,
            chart_width,
        )
    })
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
    /// Where the chart's readings fall on the clock, or `None` for a chart with no clock.
    fn clock(&self) -> Option<Clock<'_>> {
        Some(Clock {
            buckets: self.buckets?,
            positions: self.positions?,
            lower: self.lower,
            sliding: self.sliding,
        })
    }

    /// Where a curve's point for the window that ends at `second` sits at height `value`.
    fn clock_point(&self, buckets: Buckets, second: u64, value: f32) -> gpui::Point<Pixels> {
        point(
            self.bounds.origin.x + self.bounds.size.width * buckets.fraction(second),
            self.y(value),
        )
    }

    fn y(&self, value: f32) -> Pixels {
        self.bounds.bottom()
            - self.bounds.size.height * ((value - self.minimum) / (self.maximum - self.minimum))
    }

    pub(crate) fn point(&self, index: usize, value: f32) -> gpui::Point<Pixels> {
        point(
            self.bounds.origin.x
                + self.bounds.size.width
                    * self.positions.map_or_else(
                        || number(index) / number(self.count.saturating_sub(1).max(1)),
                        |positions| positions[self.lower + index],
                    ),
            self.y(value),
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
    // With a clock, the curves of a run break at each bucket with no readings of its own, which
    // `run_curves` finds, so only a change of segment ends a run. Without one, a missing reading
    // breaks the line.
    let clocked = geometry.clock().is_some();
    let present = |offset: usize| clocked || values[offset].is_some();
    for_each_run(
        values.len(),
        geometry.segments,
        geometry.lower,
        present,
        |run| {
            for curve in run_curves(values, run, geometry) {
                paint_curve(line, panel, &curve, geometry, window);
            }
        },
    );
}

/// The unbroken curves through one run of readings, each as its points in time order. A chart
/// with a clock draws a point at the end of each bucket that holds readings of its own, and
/// breaks the curve at each bucket that holds none. The bucket the join of thinned and
/// full-detail readings cuts holds readings the chart leaves out, so the curve runs past it. A
/// chart with no clock draws each reading.
fn run_curves(
    values: &[Option<(f32, f32)>],
    run: std::ops::Range<usize>,
    geometry: &Geometry<'_>,
) -> Vec<Vec<RunPoint>> {
    let Some(clock) = geometry.clock() else {
        let points = run
            .map(|index| {
                let Some((top, bottom)) = values[index] else {
                    panic!("a run of a chart with no clock holds only readings");
                };
                RunPoint {
                    top: geometry.point(index, top),
                    bottom: geometry.point(index, bottom),
                }
            })
            .collect();
        return vec![points];
    };

    window_means(values, run, clock)
        .chunk_by(|left, right| clock.buckets.joins(left.end, right.end))
        .map(|curve| {
            curve
                .iter()
                .map(|mean| RunPoint {
                    top: geometry.clock_point(clock.buckets, mean.end, mean.top),
                    bottom: geometry.clock_point(clock.buckets, mean.end, mean.bottom),
                })
                .collect()
        })
        .collect()
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

/// Draws one unbroken curve through `points`, in time order, with the area beneath it.
fn paint_curve(
    line: &crate::data::Line,
    panel: &Panel,
    points: &[RunPoint],
    geometry: &Geometry<'_>,
    window: &mut Window,
) {
    let tint = color(&line.color);
    if points.is_empty() {
        return;
    }

    if points.len() == 1 {
        let position = points[0].top;
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
    let top: Vec<_> = points.iter().map(|sample| sample.top).collect();
    smooth(&mut stroke, &top, true);

    let default_opacity = if stacked { 0.45 } else { 0.12 };
    let opacity = line.fill.or(panel.fill).unwrap_or(default_opacity);
    if !dashed {
        if opacity > 0.0 {
            let bottom: Vec<_> = points.iter().rev().map(|sample| sample.bottom).collect();
            let mut area = PathBuilder::fill();
            smooth(&mut area, &top, true);
            smooth(&mut area, &bottom, false);
            area.close();
            let span = points
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

/// The seconds between two ticks, a full-detail reading apart, which Grafana calls the scrape
/// interval and takes as the fewest seconds between two points of a curve. Thinned older readings
/// stand one for each `viewer::EARLIER_STRIDE_SECONDS`.
pub(crate) const TICK_SECONDS: u64 = 1;
/// The fewest scrape intervals a point averages, the floor of Grafana's `$__rate_interval`.
const WINDOW_SCRAPES_MIN: u64 = 4;
/// The width a chart's buckets are sized for before its card is laid out, an overview card's in
/// the 1320-pixel window.
const CHART_WIDTH_DEFAULT: f32 = 360.0;
/// The bucket lengths in seconds, each dividing an hour, so buckets start on round seconds.
const BUCKET_SECONDS: [u64; 28] = [
    1, 2, 3, 4, 5, 6, 10, 12, 15, 20, 24, 30, 40, 45, 60, 75, 90, 120, 150, 180, 240, 300, 360,
    450, 600, 900, 1200, 1800,
];
/// How far before the start of its second a reading's position may fall and still count in that
/// second. A position is an f32 fraction of the window, whose rounding can put a reading taken
/// on a whole second, as the self-monitoring reports are, a few microseconds before it, in some
/// windows and not others, so it would hop between buckets as the window slides.
const SECOND_TOLERANCE: f64 = 0.001;
const _: () = {
    assert!(TICK_SECONDS < viewer::EARLIER_STRIDE_SECONDS);
    assert!(viewer::EARLIER_STRIDE_SECONDS <= BUCKET_SECONDS[BUCKET_SECONDS.len() - 1]);
    assert!(3600_u64.is_multiple_of(BUCKET_SECONDS[0]));
    let mut index = 1;
    while index < BUCKET_SECONDS.len() {
        assert!(BUCKET_SECONDS[index - 1] < BUCKET_SECONDS[index]);
        assert!(3600_u64.is_multiple_of(BUCKET_SECONDS[index]));
        index += 1;
    }
};

/// The scrape interval of a chart of `window` whose readings come `reading_seconds` apart, or
/// the stride of thinned readings when the window starts before `detail_first`, since a point's
/// window shorter than four strides would hold too few of them and break the curve into dots.
fn scrape_seconds(window: TimeWindow, detail_first: Option<u64>, reading_seconds: u64) -> u64 {
    assert!(reading_seconds >= TICK_SECONDS);
    if detail_first.is_some_and(|join| window.first < join) {
        reading_seconds.max(viewer::EARLIER_STRIDE_SECONDS)
    } else {
        reading_seconds
    }
}

/// The stretches of the clock a chart draws its curve on, as Grafana draws a panel: one point at
/// the end of each bucket, the mean of the readings in the window of buckets before it, as
/// Grafana draws a rate over `$__rate_interval` at each step. Buckets are pinned to multiples of
/// their length on the clock, so a reading keeps its bucket as the window slides and a point
/// never changes once drawn. A point stands in view from the window's oldest edge on, and its
/// window may reach before that edge into the readings the panel holds there, as Grafana's query
/// reaches a rate interval before its range. A bucket the join where full-detail readings follow
/// thinned older ones cuts does not count.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Buckets {
    window: TimeWindow,
    length_seconds: u64,
    /// How many buckets each point averages.
    per_point: u64,
    /// The end of the first point in view, the first bucket end at or after the window's oldest
    /// second.
    first_end: u64,
    /// The first second whose readings a point in view averages.
    lead_first: u64,
    /// The second after the last bucket whole inside the window and past its newest second.
    drawn_end: u64,
    /// The first second of full-detail readings, before which readings are thinned.
    detail_first: Option<u64>,
}

impl Buckets {
    /// The buckets of `window` in a card `chart_width` pixels wide, sized as Grafana sizes its
    /// step: the shortest length of at least a scrape interval that leaves a pixel of card per
    /// point, or the longest length when none does. Each point averages Grafana's
    /// `$__rate_interval`, the longer of `WINDOW_SCRAPES_MIN` scrape intervals and a bucket and
    /// a scrape interval, rounded up to whole buckets.
    fn of(
        window: TimeWindow,
        detail_first: Option<u64>,
        reading_seconds: u64,
        chart_width: f32,
    ) -> Self {
        assert!(window.span > 0);
        assert!(chart_width.is_finite());
        assert!(chart_width >= 0.0);
        let span = match u32::try_from(window.span) {
            Ok(span) => f64::from(span),
            Err(error) => panic!("a chart spans at most a few hours of seconds: {error}"),
        };
        let scrape_seconds = scrape_seconds(window, detail_first, reading_seconds);
        let length_seconds = BUCKET_SECONDS
            .into_iter()
            .filter(|length| *length >= scrape_seconds)
            .find(|length| {
                let length = match u32::try_from(*length) {
                    Ok(length) => f64::from(length),
                    Err(error) => panic!("a bucket is at most an hour long: {error}"),
                };
                length * f64::from(chart_width) >= span
            })
            .unwrap_or(BUCKET_SECONDS[BUCKET_SECONDS.len() - 1]);
        let window_seconds =
            (WINDOW_SCRAPES_MIN * scrape_seconds).max(length_seconds + scrape_seconds);
        let per_point = window_seconds.div_ceil(length_seconds);
        assert!(per_point >= 2);
        let first_end = window.first.div_ceil(length_seconds) * length_seconds;
        let lead_first = first_end.saturating_sub(per_point * length_seconds);
        // The window's newest second may still gain a reading from a tick that ran late, so a
        // bucket waits for the second after it. The division floors, so a bucket the window's
        // end cuts is not drawn.
        let newest = window.first + window.span - 1;
        let drawn_end = newest / length_seconds * length_seconds;
        assert!(first_end <= drawn_end + length_seconds);

        Self {
            window,
            length_seconds,
            per_point,
            first_end,
            lead_first,
            drawn_end,
            detail_first,
        }
    }

    /// Where `second` sits across the window, from 0 at its first second to 1 past its newest.
    fn fraction(&self, second: u64) -> f32 {
        let span = match u32::try_from(self.window.span) {
            Ok(span) => f64::from(span),
            Err(error) => panic!("a chart spans at most a few hours of seconds: {error}"),
        };
        let offset = match second.checked_sub(self.window.first).map(u32::try_from) {
            Some(Ok(offset)) => f64::from(offset),
            _ => panic!("a point's second lies within a few hours after its window's first"),
        };
        match num_traits::ToPrimitive::to_f32(&(offset / span)) {
            Some(fraction) => fraction,
            None => panic!("a fraction of a window fits f32"),
        }
    }

    /// The second a reading at `position` across the window was taken in, or `None` for a reading
    /// before the clock's zero.
    fn second_of(&self, position: f32) -> Option<u64> {
        assert!(position.is_finite());
        let span = match u32::try_from(self.window.span) {
            Ok(span) => f64::from(span),
            Err(error) => panic!("a chart spans at most a few hours of seconds: {error}"),
        };
        // Floors, so a reading belongs to the second it was taken in.
        let offset = (f64::from(position) * span + SECOND_TOLERANCE).floor();
        let Some(seconds) = num_traits::ToPrimitive::to_u64(&offset.abs()) else {
            panic!("a finite offset of a few hours of seconds fits u64");
        };
        if offset < 0.0 {
            return self.window.first.checked_sub(seconds);
        }

        Some(self.window.first + seconds)
    }

    /// The bucket index of a reading at `position` across the window, or `None` when no point in
    /// view averages it, when the window's newest edge cuts its bucket, or when the join of thinned
    /// and full-detail readings does, since such a bucket still loses or gains readings as the
    /// window slides.
    fn bucket_index(&self, position: f32) -> Option<u64> {
        let second = self.second_of(position)?;
        if second < self.lead_first {
            return None;
        }
        if second >= self.drawn_end {
            return None;
        }
        // Floors, so the index counts whole bucket lengths since the clock's zero.
        let index = second / self.length_seconds;
        if self.join_cuts(index * self.length_seconds) {
            return None;
        }
        Some(index)
    }

    /// Whether the join of thinned and full-detail readings cuts the bucket from `start`, which
    /// then holds readings of both kinds.
    fn join_cuts(&self, start: u64) -> bool {
        let Some(join) = self.detail_first else {
            return false;
        };
        if start >= join {
            return false;
        }
        join < start + self.length_seconds
    }

    /// Whether a curve runs on from its point at the bucket end `left` to its next point, at
    /// `right`: the two buckets are neighbours, or only the bucket the join of thinned and
    /// full-detail readings cuts lies between them, which holds readings left out, not none.
    fn joins(&self, left: u64, right: u64) -> bool {
        assert!(left < right);
        let gap = right - left;
        if gap == self.length_seconds {
            return true;
        }
        if gap != 2 * self.length_seconds {
            return false;
        }
        self.join_cuts(left)
    }
}

/// Where a chart's readings fall on the clock, which sets the windows its curve averages.
#[derive(Clone, Copy)]
struct Clock<'a> {
    buckets: Buckets,
    /// Where each of the panel's readings sits across its window.
    positions: &'a [f32],
    /// The index in `positions` of the first reading in view.
    lower: usize,
    /// Whether readings still leave at the window's start and arrive at its end.
    sliding: bool,
}

/// One point of a curve: the mean of a line's readings in the window that ends at `end`, a
/// second at the end of a bucket, at their top and at their base.
#[derive(Clone, Copy, Debug, PartialEq)]
struct WindowMean {
    end: u64,
    top: f32,
    bottom: f32,
}

/// The sums of one bucket's readings at their top and at their base.
#[derive(Clone, Copy)]
struct BucketTotal {
    index: u64,
    top: f32,
    bottom: f32,
    count: u16,
}

impl BucketTotal {
    fn add(&mut self, top: f32, bottom: f32) {
        self.top += top;
        self.bottom += bottom;
        self.count = match self.count.checked_add(1) {
            Some(count) => count,
            None => panic!("a bucket of at most half an hour holds a few readings a second"),
        };
    }
}

/// The point at the end of the newest bucket of `recent`, the mean of the readings in its window,
/// or `None` when `recent` holds no bucket, the point stands before the window's oldest edge, or
/// its window reaches before `whole_from`, the first bucket no slide of the window can still
/// change.
fn window_mean(
    recent: &std::collections::VecDeque<BucketTotal>,
    whole_from: Option<u64>,
    buckets: Buckets,
) -> Option<WindowMean> {
    let newest = recent.back()?.index;
    let end = (newest + 1) * buckets.length_seconds;
    if end < buckets.first_end {
        return None;
    }
    let start = (newest + 1).saturating_sub(buckets.per_point);
    if let Some(whole_from) = whole_from {
        if start < whole_from {
            return None;
        }
    }

    let (mut top, mut bottom, mut count) = (0.0_f32, 0.0_f32, 0_u16);
    for bucket in recent {
        assert!(bucket.index <= newest);
        if bucket.index < start {
            continue;
        }
        top += bucket.top;
        bottom += bucket.bottom;
        count = match count.checked_add(bucket.count) {
            Some(count) => count,
            None => panic!("a window of at most an hour holds a few readings a second"),
        };
    }
    assert!(count > 0);

    let count = f32::from(count);
    Some(WindowMean {
        end,
        top: top / count,
        bottom: bottom / count,
    })
}

/// The points of one segment's readings, `run`, in time order: one at the end of each bucket
/// that holds readings of its own, the mean of the readings in its window, so a bucket with none
/// breaks the curve. A point whose window holds readings that left the chart, or that the join of
/// thinned and full-detail readings will still change, is not drawn.
///
/// Readings before the first in view left a sliding window, so a run that starts there draws
/// only windows whole inside the chart. A run that starts at a change of segment has no
/// readings before it, so its first windows hold only its own.
fn window_means(
    values: &[Option<(f32, f32)>],
    run: std::ops::Range<usize>,
    clock: Clock<'_>,
) -> Vec<WindowMean> {
    let mut whole_from = first_whole_bucket(clock);
    let Clock {
        buckets,
        positions,
        lower,
        sliding,
    } = clock;
    let capacity = match usize::try_from(buckets.per_point) {
        Ok(capacity) => capacity,
        Err(error) => panic!("a window holds a few buckets: {error}"),
    };
    let mut recent: std::collections::VecDeque<BucketTotal> =
        std::collections::VecDeque::with_capacity(capacity);
    let mut means = Vec::with_capacity(run.len() / 8 + 1);
    let mut cut = false;
    for index in run {
        let Some((top, bottom)) = values[index] else {
            continue;
        };
        let Some(bucket) = buckets.bucket_index(positions[lower + index]) else {
            // A still view gains and loses no readings, so a cut bucket only leaves its own
            // readings out. In a sliding one, the windows around it would still change.
            if sliding {
                means.extend(window_mean(&recent, whole_from, buckets));
                recent.clear();
                cut = true;
            }
            continue;
        };

        if cut {
            whole_from = Some(bucket);
            cut = false;
        }
        if let Some(open) = recent.back_mut() {
            if open.index == bucket {
                open.add(top, bottom);
                continue;
            }
        }
        means.extend(window_mean(&recent, whole_from, buckets));
        while recent
            .front()
            .is_some_and(|oldest| oldest.index + buckets.per_point <= bucket)
        {
            recent.pop_front();
        }
        recent.push_back(BucketTotal {
            index: bucket,
            top,
            bottom,
            count: 1,
        });
    }
    means.extend(window_mean(&recent, whole_from, buckets));
    means
}

/// The first bucket a sliding chart's windows may reach, the one after its oldest reading's,
/// since that reading may share its bucket with readings already gone. A still chart has none.
fn first_whole_bucket(clock: Clock<'_>) -> Option<u64> {
    if !clock.sliding {
        return None;
    }
    let oldest = clock.buckets.second_of(clock.positions[0]);
    Some(oldest.map_or(0, |second| second / clock.buckets.length_seconds + 1))
}

/// Where a reading or a curve's point sits on the plot: the top of its line and the bottom of
/// its area.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RunPoint {
    top: gpui::Point<Pixels>,
    bottom: gpui::Point<Pixels>,
}

/// Draws a curve through `points` in the order given, one stretch to each point from the one
/// before.
fn smooth(builder: &mut PathBuilder, points: &[gpui::Point<Pixels>], start: bool) {
    let Some(first) = points.first() else {
        return;
    };

    if start {
        builder.move_to(*first);
    } else {
        builder.line_to(*first);
    }
    for [to, leaving, reaching] in half_step_stretches(points) {
        builder.cubic_bezier_to(to, leaving, reaching);
    }
}

/// The stretches of a curve through `points` in the order given, each as the point it reaches
/// and its two control points, as GNOME System Monitor and Smoothie Charts draw a live curve: a
/// stretch leaves one point level and reaches the next level, bending halfway between them. It
/// depends on its two ends alone, so a point arriving or leaving reshapes no stretch already
/// drawn, and it never passes above or below its ends. Drawn the other way it traces the same
/// curve, which the bottom edge of an area relies on, since that edge runs back in time.
fn half_step_stretches(
    points: &[gpui::Point<Pixels>],
) -> impl Iterator<Item = [gpui::Point<Pixels>; 3]> + '_ {
    points.windows(2).map(|pair| {
        let (source, target) = (pair[0], pair[1]);
        let halfway = source.x + (target.x - source.x) / 2.0;
        [target, point(halfway, source.y), point(halfway, target.y)]
    })
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

/// The range of the points the lines on `axis` draw, so the axis spans the curves, and a
/// reading whose window is not drawn yet cannot rescale the points already drawn.
fn axis_extent(panel: &Panel, inspection: Inspection, hidden: &[String], axis: &str) -> (f32, f32) {
    let floor = if axis == "left" {
        panel.minimum.unwrap_or(0.0).min(0.0)
    } else {
        0.0
    };
    let inspection = Inspection {
        lower: curve_lower(panel, inspection),
        ..inspection
    };
    let mut extent = (floor, 0.0_f32);
    let mut widen = |top: f32, bottom: f32| {
        extent = (extent.0.min(top).min(bottom), extent.1.max(top).max(bottom));
    };
    let clock = panel_buckets(panel)
        .zip(panel.positions.as_deref())
        .map(|(buckets, positions)| Clock {
            buckets,
            positions,
            lower: inspection.lower,
            sliding: panel.sliding,
        });
    stacked_lines(panel, inspection, hidden, |line, values| {
        if line.axis.as_deref().unwrap_or("left") != axis {
            return;
        }
        let Some(clock) = clock else {
            for (top, bottom) in values.iter().flatten() {
                widen(*top, *bottom);
            }
            return;
        };

        let segments = panel.segments.as_deref();
        for_each_run(
            values.len(),
            segments,
            inspection.lower,
            |_| true,
            |run| {
                for mean in window_means(values, run, clock) {
                    widen(mean.top, mean.bottom);
                }
            },
        );
    });
    extent
}

/// Where the reading at `index` sits across the plot, from 0 at its left to 1 at its right.
fn fraction_in_view(panel: &Panel, inspection: &Inspection, index: usize) -> f32 {
    panel.positions.as_deref().map_or_else(
        || number(index - inspection.lower) / number((inspection.upper - inspection.lower).max(1)),
        |positions| positions[index],
    )
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

    /// The width of a card that draws 15 minutes in ten-second buckets, whose round numbers the
    /// tests of how a curve slides read most easily.
    const TEN_SECOND_CARD: f32 = 90.0;

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
    fn a_line_plot_draws_the_readings_in_view_whatever_is_selected() {
        // Goal: a hover over a line chart leaves what its plot draws as it was. Method: take the
        // plot's inspection of a line panel while an earlier reading is selected.
        let panel = kind_panel("line");
        let hovered = Inspection {
            lower: 3,
            selected: 5,
            upper: 9,
        };

        let drawn = plot_inspection(&panel, hovered);

        assert_eq!(
            drawn,
            Inspection {
                selected: 9,
                ..hovered
            }
        );
    }

    #[test]
    fn a_bar_plot_draws_the_selected_reading() {
        // Goal: the bars of a hovered second are the bars the plot draws. Method: take the
        // plot's inspection of a core bar panel while an earlier reading is selected.
        let panel = kind_panel("core-bars");
        let hovered = Inspection {
            lower: 3,
            selected: 5,
            upper: 9,
        };

        let drawn = plot_inspection(&panel, hovered);

        assert_eq!(drawn, hovered);
    }

    fn kind_panel(kind: &str) -> Panel {
        serde_json::from_value(
            serde_json::json!({"id":"chart", "title":"Chart", "unit":"%",
            "kind":kind, "lines":[]}),
        )
        .unwrap()
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
        // read at 1020, so the three-second bucket of the reading at 1000 is whole and drawn.
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
                TICK_SECONDS,
                TEN_SECOND_CARD,
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

    /// The points of the one curve a run of `values` draws, which must not break.
    fn one_curve(
        values: &[Option<(f32, f32)>],
        run: std::ops::Range<usize>,
        geometry: &Geometry<'_>,
    ) -> Vec<RunPoint> {
        let mut curves = run_curves(values, run, geometry);
        assert_eq!(curves.len(), 1, "{curves:?}");
        curves.remove(0)
    }

    /// The 900-second window from `first` of varying readings, following the newest second, in a
    /// card `card_width` pixels wide.
    fn sliding_curve(first: u64, card_width: f32) -> Vec<RunPoint> {
        let positions = live_positions();
        let mut geometry = live_geometry(first, 0, &positions);
        geometry.sliding = true;
        let window = TimeWindow { first, span: 900 };
        geometry.buckets = Some(Buckets::of(window, None, TICK_SECONDS, card_width));

        one_curve(&varying_readings(first), 0..900, &geometry)
    }

    /// The points that window draws, each with its place on an x axis pinned to the clock rather
    /// than the window.
    fn clock_places(first: u64, card_width: f32) -> Vec<[f32; 3]> {
        let clock = f32::from(u16::try_from(first - 1000).unwrap());

        places(&sliding_curve(first, card_width))
            .into_iter()
            .map(|[x, top, bottom]| [clock + x, top, bottom])
            .collect()
    }

    #[test]
    fn a_point_in_ten_second_buckets_is_the_mean_of_the_two_before_it() {
        // Goal: a chart in ten-second buckets draws, as Grafana draws a rate over
        // `$__rate_interval` for one-second readings, a point at the end of every bucket, the
        // mean of the readings of the eleven seconds before it rounded up to two whole buckets,
        // over a base that is the mean of theirs.
        // Method: in a still window from 1000, place twenty readings of 20 over a base of 0 in
        // seconds 1100 to 1119 and twenty of 60 over a base of 10 in 1120 to 1139, and read the
        // points on a chart one pixel a second and one pixel a unit. The curve ends with the
        // last bucket that holds readings.
        let positions = live_positions();
        let mut values = vec![None; 900];
        for value in &mut values[100..120] {
            *value = Some((20.0, 0.0));
        }
        for value in &mut values[120..140] {
            *value = Some((60.0, 10.0));
        }

        let points = one_curve(&values, 0..900, &live_geometry(1000, 0, &positions));

        assert_places(
            &points,
            &[
                [110.0, 80.0, 100.0],
                [120.0, 80.0, 100.0],
                [130.0, 60.0, 95.0],
                [140.0, 40.0, 90.0],
            ],
        );
    }

    #[test]
    fn a_curve_breaks_at_a_bucket_with_no_readings_of_its_own() {
        // Goal: a chart breaks its line at any bucket with no readings of its own, so every
        // missed reading the buckets can show stays visible, and missing readings inside a
        // bucket that holds others leave the line whole.
        // Method: in a still window from 1000 in ten-second buckets, leave five seconds without
        // readings from 1140, inside the bucket from 1140, then ten, the whole bucket, between
        // two stretches of readings, and count the curves each draws.
        let positions = live_positions();
        let curves = |gap: usize| {
            let mut values = vec![None; 900];
            for value in &mut values[100..140] {
                *value = Some((50.0, 0.0));
            }
            for value in &mut values[140 + gap..180 + gap] {
                *value = Some((50.0, 0.0));
            }
            run_curves(&values, 0..900, &live_geometry(1000, 0, &positions)).len()
        };

        assert_eq!([curves(5), curves(10)], [1, 2]);
    }

    #[test]
    fn a_point_beside_a_missed_tick_stays_the_mean_of_its_window() {
        // Goal: the break a missed tick leaves changes no point around it, so the first point
        // after the gap is still the mean of the readings its whole window holds, those before
        // the gap included.
        // Method: in a still window from 1000 on a card wide enough for one-second buckets, whose
        // points each average four seconds, place readings of 20 in seconds 1090 to 1099, miss
        // 1100, and place readings of 60 in 1101 to 1109. Read the end of the curve before the
        // gap and the start of the one after it, on a chart one pixel a second and a unit.
        let positions = live_positions();
        let mut geometry = live_geometry(1000, 0, &positions);
        let window = TimeWindow {
            first: 1000,
            span: 900,
        };
        geometry.buckets = Some(Buckets::of(window, None, TICK_SECONDS, 900.0));
        let mut values = vec![None; 900];
        for value in &mut values[90..100] {
            *value = Some((20.0, 0.0));
        }
        for value in &mut values[101..110] {
            *value = Some((60.0, 0.0));
        }

        let curves = run_curves(&values, 0..900, &geometry);

        assert_eq!(curves.len(), 2, "{curves:?}");
        assert_places(&curves[0][9..], &[[100.0, 80.0, 100.0]]);
        assert_places(
            &curves[1][..2],
            &[[102.0, 66.67, 100.0], [103.0, 53.33, 100.0]],
        );
    }

    #[test]
    fn a_sliding_window_draws_points_whose_windows_hold_every_reading() {
        // Goal: a point that would still lose readings to the window's oldest edge, or gain one
        // from a late tick at its newest second, is not drawn, so no drawn point changes as
        // readings come and go. Method: draw a following window from 1002 to 1901, whose whole
        // buckets run from 1010 to 1899, and read its first and last points and their count:
        // the first ends the window of two buckets from 1010.
        let places = clock_places(1002, TEN_SECOND_CARD);

        assert_eq!(places.len(), 88);
        assert!((places[0][0] - 30.0).abs() < 0.01, "{:?}", places[0]);
        assert!((places[87][0] - 900.0).abs() < 0.01, "{:?}", places[87]);
    }

    /// A following 15-minute chart from `first` whose panel also holds the `lead_in` seconds
    /// before its oldest edge, with `reading` in each second, and its inspection of the newest.
    fn led_panel(first: u64, lead_in: u16, reading: fn(u64) -> f32) -> (Panel, Inspection) {
        let held = 900 + lead_in;
        let positions: Vec<f32> = (0..held)
            .map(|offset| (f32::from(offset) - f32::from(lead_in) + 0.3) / 900.0)
            .collect();
        let values: Vec<Option<f32>> = (first - u64::from(lead_in)..first + 900)
            .map(|second| Some(reading(second)))
            .collect();
        let mut panel: Panel = serde_json::from_value(serde_json::json!({
            "id":"cpu", "title":"CPU", "unit":"%", "kind":"line",
            "positions": positions, "lines":[
                {"name":"All cores", "color":"#5794f2", "values": values}]}))
        .unwrap();
        panel.window = Some(TimeWindow { first, span: 900 });
        panel.sliding = true;
        panel.chart_width.set(Some(TEN_SECOND_CARD));
        let newest = usize::from(held) - 1;
        let inspection = Inspection {
            lower: usize::from(lead_in),
            selected: newest,
            upper: newest,
        };
        (panel, inspection)
    }

    /// The curve a following 15-minute chart from `first` draws, whose panel also holds a
    /// varying reading in each of the `lead_in` seconds before its oldest edge.
    fn led_curve(first: u64, lead_in: u16) -> Vec<RunPoint> {
        let (mut panel, inspection) = led_panel(first, lead_in, varying_reading);
        panel.maximum = Some(100.0);
        let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(900.0), px(100.0)));

        let mut curves = Vec::new();
        prepare_lines(&panel, bounds, inspection, &[], |_, values, geometry| {
            curves.extend(run_curves(values, 0..values.len(), geometry));
        });

        assert_eq!(curves.len(), 1);
        curves.remove(0)
    }

    #[test]
    fn a_lead_in_before_the_oldest_edge_starts_the_curve_at_that_edge() {
        // Goal: as in Grafana, whose query reaches back a rate interval before the range, the
        // readings of the minute before a following chart complete the windows of its first
        // points, so its curve starts at its oldest edge and still never changes once drawn.
        // Method: draw a following window from 1000 to 1899 whose panel holds the 60 seconds
        // before it, and read its first and last points and their count.
        let points = led_curve(1000, 60);

        assert_eq!(points.len(), 90);
        assert!(f32::from(points[0].top.x).abs() < 0.01);
        assert!((f32::from(points[89].top.x) - 890.0).abs() < 0.01);
    }

    #[test]
    fn the_value_axis_reaches_a_point_the_lead_in_raises() {
        // Goal: the value axis spans the points drawn, so a first point whose window averages
        // readings before the oldest edge stays inside the plot. Method: hold 90 in the minute
        // before a following window from 1000 and 10 inside it, and read the axis's extent.
        let (panel, inspection) =
            led_panel(1000, 60, |second| if second < 1000 { 90.0 } else { 10.0 });

        assert_eq!(axis_extent(&panel, inspection, &[], "left"), (0.0, 90.0));
    }

    #[test]
    fn a_lead_in_starts_the_curve_at_the_first_bucket_end_in_view() {
        // Goal: a point stands at the end of its bucket on the clock, so a window whose oldest
        // edge falls inside a bucket starts its curve at that bucket's end, less than a step in.
        // Method: draw a following window from 1003 with a 60-second lead-in, whose first bucket
        // end in view is 1010, and read where its first point sits.
        let points = led_curve(1003, 60);

        assert!((f32::from(points[0].top.x) - 7.0).abs() < 0.01);
    }

    #[test]
    fn without_a_lead_in_the_first_point_waits_for_a_whole_window() {
        // Goal: a following chart holding nothing before its oldest edge, whose oldest bucket
        // still loses readings each second, draws no point whose window reaches that bucket.
        // Method: draw a following window from 1003 with no lead-in, whose oldest bucket runs
        // from 1000 to 1009, and read where its first point sits: the end of the window of two
        // buckets from 1010.
        let points = led_curve(1003, 0);

        assert!((f32::from(points[0].top.x) - 27.0).abs() < 0.01);
    }

    #[test]
    fn the_newest_point_waits_for_the_second_after_its_window() {
        // Goal: a tick that runs late can add a second reading to the newest second, so the
        // point whose window ends with it is drawn only once a later second has readings, and
        // then at once. Method: draw following windows whose newest seconds are 1899 and 1900,
        // and read the last point of each.
        let waiting = clock_places(1000, TEN_SECOND_CARD);
        let drawn = clock_places(1001, TEN_SECOND_CARD);

        assert!((waiting[waiting.len() - 1][0] - 890.0).abs() < 0.01);
        assert!((drawn[drawn.len() - 1][0] - 900.0).abs() < 0.01);
    }

    #[test]
    fn a_drawn_point_stays_put_as_the_window_slides() {
        // Goal: a point once drawn keeps its place and mean while readings arrive, and leaves
        // the chart only at its oldest edge.
        // Method: slide a following window one second at a time across seventy seconds, on
        // cards that draw one-second, three-second and ten-second buckets, and check, for each
        // two windows in a row, that the second draws every point of the first from the second's
        // oldest on, and the first every point of the second to the first's newest.
        for card_width in [900.0, CARD_WIDTH, TEN_SECOND_CARD] {
            for first in 1000..1070 {
                let before = clock_places(first, card_width);
                let after = clock_places(first + 1, card_width);

                let held = |place: &[f32; 3], places: &[[f32; 3]]| {
                    places.iter().any(|other| same(place, other))
                };
                for place in before.iter().filter(|place| place[0] >= after[0][0]) {
                    assert!(held(place, &after), "{place:?} of {first} changed");
                }
                let newest = before[before.len() - 1][0];
                for place in after.iter().filter(|place| place[0] <= newest) {
                    assert!(held(place, &before), "{place:?} of {} is new", first + 1);
                }
            }
        }
    }

    #[test]
    fn a_stretch_of_curve_bends_halfway_between_its_ends_at_their_heights() {
        // Goal: as GNOME System Monitor and Smoothie Charts draw a live curve, each stretch
        // leaves one point level and reaches the next level, bending halfway between them, and
        // drawn backwards, as an area's bottom edge is, it traces the same curve.
        // Method: read the stretches through (10, 80), (40, 20) and (70, 50), each as the point
        // it reaches and its two control points, and those of the same points backwards.
        let at = |x: f32, y: f32| point(px(x), px(y));
        let forward = [at(10.0, 80.0), at(40.0, 20.0), at(70.0, 50.0)];
        let mut backward = forward;
        backward.reverse();

        let drawn: Vec<_> = half_step_stretches(&forward).collect();
        let reversed: Vec<_> = half_step_stretches(&backward).collect();

        assert_eq!(
            drawn,
            vec![
                [at(40.0, 20.0), at(25.0, 80.0), at(25.0, 20.0)],
                [at(70.0, 50.0), at(55.0, 20.0), at(55.0, 50.0)],
            ]
        );
        assert_eq!(
            reversed,
            vec![
                [at(40.0, 20.0), at(55.0, 50.0), at(55.0, 20.0)],
                [at(10.0, 80.0), at(25.0, 20.0), at(25.0, 80.0)],
            ]
        );
    }

    #[test]
    fn a_still_view_draws_windows_to_its_oldest_edge() {
        // Goal: a view that no longer follows the newest second gains and loses no readings, so
        // a point whose window holds only part of a bucket is still drawn, from the first bucket
        // end in view. Method: read the first and last points of a still window from 1002 to
        // 1901 of readings in each second, and their count.
        let positions = live_positions();

        let points = one_curve(
            &varying_readings(1002),
            0..900,
            &live_geometry(1002, 0, &positions),
        );

        assert_eq!(points.len(), 90);
        assert!((f32::from(points[0].top.x) - 8.0).abs() < 0.01);
        assert!((f32::from(points[89].top.x) - 898.0).abs() < 0.01);
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
                let buckets_of_window = Buckets::of(window, None, TICK_SECONDS, TEN_SECOND_CARD);
                buckets.insert(buckets_of_window.bucket_index((offset + fraction) / 900.0));

                // The zoom from an hour, in the arithmetic the range zoom uses.
                let hour_position = (offset + 2700.0 + fraction) / 3600.0;
                let zoomed = (hour_position * 3600.0 - 2700.0) / 900.0;
                buckets.insert(buckets_of_window.bucket_index(zoomed));
            }
        }

        assert_eq!(buckets, [None, Some(498)].into_iter().collect());
    }

    /// The seconds of the first and last points of each curve of a 30-minute window from 1000 on
    /// the 900-pixel plot, two seconds a pixel.
    fn half_hour_ends(curves: &[Vec<RunPoint>]) -> Vec<(f32, f32)> {
        let seconds = |point: &RunPoint| 1000.0 + f32::from(point.top.x) * 2.0;
        curves
            .iter()
            .map(|curve| (seconds(&curve[0]), seconds(&curve[curve.len() - 1])))
            .collect()
    }

    #[test]
    fn a_bucket_the_join_of_thinned_and_full_detail_readings_cuts_is_not_drawn() {
        // Goal: a bucket holding both older readings, each standing for several seconds, and
        // full-detail readings is not drawn, since the join moves a second each second.
        // Method: in a 30-minute window from 1000 in ten-second buckets, put the join at 1905 and
        // read the buckets of readings either side of the bucket it cuts.
        let buckets = Buckets::of(
            TimeWindow {
                first: 1000,
                span: 1800,
            },
            Some(1905),
            TICK_SECONDS,
            TEN_SECOND_CARD * 2.0,
        );

        let found: Vec<_> = [1899_u16, 1900, 1909, 1910]
            .into_iter()
            .map(|second| buckets.bucket_index((f32::from(second - 1000) + 0.3) / 1800.0))
            .collect();

        assert_eq!(found, vec![Some(189), None, None, Some(191)]);
    }

    #[test]
    fn no_window_across_the_join_is_drawn_while_the_window_slides() {
        // Goal: a point whose window holds readings the moving join will still thin or cut is
        // not drawn, so the curve beside the join does not change from second to second.
        // Method: in a following 30-minute window from 1000 in ten-second buckets with a reading
        // each second and the join at 1905, read the points either side of the bucket the join
        // cuts.
        let positions = tick_positions(1800);
        let mut geometry = live_geometry(1000, 0, &positions);
        geometry.sliding = true;
        let window = TimeWindow {
            first: 1000,
            span: 1800,
        };
        geometry.buckets = Some(Buckets::of(
            window,
            Some(1905),
            TICK_SECONDS,
            TEN_SECOND_CARD * 2.0,
        ));
        let values: Vec<_> = (1000..2800)
            .map(|second| Some((varying_reading(second), 0.0)))
            .collect();

        let curves = run_curves(&values, 0..1800, &geometry);

        let ends = half_hour_ends(&curves);
        assert_eq!(ends, vec![(1050.0, 1900.0), (1950.0, 2790.0)]);
    }

    #[test]
    fn a_chart_of_thinned_readings_takes_their_stride_as_its_scrape_interval() {
        // Goal: a chart whose older part holds one reading in each ten seconds draws it as one
        // unbroken curve, on a card wide enough for buckets shorter than that.
        // Method: in a still 30-minute window from 1000 with the join at 1905, put a reading in
        // the seventh second of each ten before the join and one each second after it, size
        // the buckets for a card that would give three-second buckets, and read the curves.
        let positions = tick_positions(1800);
        let mut geometry = live_geometry(1000, 0, &positions);
        let window = TimeWindow {
            first: 1000,
            span: 1800,
        };
        geometry.buckets = Some(Buckets::of(window, Some(1905), TICK_SECONDS, 600.0));
        let values: Vec<_> = (1000..2800_u64)
            .map(|second| {
                let held = second >= 1905 || second % 10 == 7;
                held.then(|| (varying_reading(second), 0.0))
            })
            .collect();

        let curves = run_curves(&values, 0..1800, &geometry);

        let ends = half_hour_ends(&curves);
        assert_eq!(ends, vec![(1010.0, 2790.0)]);
    }

    #[test]
    fn a_chart_scrolled_through_its_readings_buckets_each_by_its_own_second() {
        // Goal: a chart whose first reading in view is not its first reading still puts each
        // reading in the bucket of the second it was taken in.
        // Method: view the readings of a still window from 1000 from the 107th, place five
        // readings in seconds 1107 to 1111, and read the points their windows draw.
        let positions = live_positions();
        let values: Vec<_> = [10.0, 20.0, 30.0, 40.0, 50.0]
            .into_iter()
            .map(|reading| Some((reading, 0.0)))
            .collect();

        let points = one_curve(&values, 0..5, &live_geometry(1000, 107, &positions));

        assert_places(&points, &[[110.0, 80.0, 100.0], [120.0, 70.0, 100.0]]);
    }

    #[test]
    fn a_reading_before_the_window_counts_only_for_a_point_in_view() {
        // Goal: a reading before the window's oldest edge falls in its own bucket when the window
        // of the first point in view reaches it, and in none when no point in view averages it.
        // Method: in a window from 1000 in ten-second buckets, whose first point ends at 1000
        // and averages the twenty seconds from 980, read the buckets of readings half a second
        // before the window, in second 980 and in second 979.
        let buckets = Buckets::of(
            TimeWindow {
                first: 1000,
                span: 900,
            },
            None,
            TICK_SECONDS,
            TEN_SECOND_CARD,
        );

        let found: Vec<_> = [-0.5, -19.7, -20.7]
            .into_iter()
            .map(|offset: f32| buckets.bucket_index(offset / 900.0))
            .collect();

        assert_eq!(found, vec![Some(99), Some(98), None]);
    }

    #[test]
    fn a_panel_with_no_clock_draws_each_reading_on_its_own() {
        // Goal: a chart that knows no seconds draws its readings as they are.
        // Method: draw three readings of a geometry with no buckets.
        let positions = [0.0, 0.5, 1.0];
        let mut geometry = live_geometry(1000, 0, &positions);
        geometry.buckets = None;
        let values = [Some((10.0, 0.0)), Some((20.0, 0.0)), Some((30.0, 0.0))];

        let points = one_curve(&values, 0..3, &geometry);

        assert_places(
            &points,
            &[
                [0.0, 90.0, 100.0],
                [450.0, 80.0, 100.0],
                [900.0, 70.0, 100.0],
            ],
        );
    }

    /// The spacing of the points a 900-pixel plot of a reading in each second of the `span`
    /// seconds from `first` draws, in a card `card_width` pixels wide whose full-detail readings
    /// start at `detail_first`, and their count.
    fn point_spacing(
        first: u64,
        span: u16,
        detail_first: Option<u64>,
        card_width: f32,
    ) -> (usize, Vec<f32>) {
        let positions = tick_positions(span);
        let mut geometry = live_geometry(first, 0, &positions);
        let window = TimeWindow {
            first,
            span: u64::from(span),
        };
        geometry.buckets = Some(Buckets::of(window, detail_first, TICK_SECONDS, card_width));
        let values: Vec<_> = (first..first + u64::from(span))
            .map(|second| Some((varying_reading(second), 0.0)))
            .collect();

        let points = one_curve(&values, 0..values.len(), &geometry);

        let spacing = points
            .windows(2)
            .map(|pair| f32::from(pair[1].top.x) - f32::from(pair[0].top.x))
            .collect();
        (points.len(), spacing)
    }

    #[test]
    fn a_range_of_a_minute_draws_a_point_every_second() {
        // Goal: a short range draws each second, as Grafana's step for one-second readings
        // does, so its newest reading shows a second after it is taken. Method: draw a reading
        // in each second of a 60-second window on a 900-pixel plot in an overview card and read
        // the count and spacing of its points.
        let (count, spacing) = point_spacing(1000, 60, None, CARD_WIDTH);

        assert_eq!(count, 59);
        for gap in spacing {
            assert!((gap - 15.0).abs() < 0.01, "{gap}");
        }
    }

    #[test]
    fn a_range_after_the_thinned_readings_keeps_a_point_every_second() {
        // Goal: only a chart that reaches back into the thinned older readings takes their
        // stride as its scrape interval, so a short range of a live chart, whose older readings
        // lie before it, still draws each second.
        // Method: draw a 60-second window from 1000 with full detail from 940, and again with
        // full detail from exactly 1000, and read the count and spacing of their points.
        for detail_first in [940, 1000] {
            let (count, spacing) = point_spacing(1000, 60, Some(detail_first), CARD_WIDTH);

            assert_eq!(count, 59, "{detail_first}");
            for gap in spacing {
                assert!((gap - 15.0).abs() < 0.01, "{detail_first}: {gap}");
            }
        }
    }

    #[test]
    fn a_point_in_one_second_buckets_is_the_mean_of_the_four_seconds_before_it() {
        // Goal: a chart in one-second buckets draws, as Grafana draws a rate over
        // `$__rate_interval` for one-second readings, a point at the end of every second, the
        // mean of the readings of the four seconds before it.
        // Method: in a still window from 1000 on a card wide enough for one-second buckets,
        // place ten readings of 20 in seconds 1090 to 1099 and ten of 60 in 1100 to 1109, and
        // read the points either side of the step on a chart one pixel a second and one pixel a
        // unit.
        let positions = live_positions();
        let mut geometry = live_geometry(1000, 0, &positions);
        let window = TimeWindow {
            first: 1000,
            span: 900,
        };
        geometry.buckets = Some(Buckets::of(window, None, TICK_SECONDS, 900.0));
        let mut values = vec![None; 900];
        for value in &mut values[90..100] {
            *value = Some((20.0, 0.0));
        }
        for value in &mut values[100..110] {
            *value = Some((60.0, 0.0));
        }

        let points = one_curve(&values, 0..900, &geometry);

        assert_places(
            &points[9..14],
            &[
                [100.0, 80.0, 100.0],
                [101.0, 70.0, 100.0],
                [102.0, 60.0, 100.0],
                [103.0, 50.0, 100.0],
                [104.0, 40.0, 100.0],
            ],
        );
    }

    #[test]
    fn ten_second_reports_draw_one_unbroken_curve() {
        // Goal: a chart of reports the collector writes every ten ticks, as the Diagnostics
        // charts are, takes their spacing as its scrape interval, so each point averages several
        // reports and the curve does not break into dots between them.
        // Method: place a report every ten seconds in a still 600-second window from 1000, size
        // the buckets for a card before it is laid out, and count the curves.
        let positions: Vec<f32> = (0..60_u16)
            .map(|report| (f32::from(report) * 10.0 + 0.3) / 600.0)
            .collect();
        let mut geometry = live_geometry(1000, 0, &positions);
        let window = TimeWindow {
            first: 1000,
            span: 600,
        };
        geometry.buckets = Some(Buckets::of(window, None, 10, CHART_WIDTH_DEFAULT));
        let values: Vec<_> = (0..60_u64)
            .map(|report| Some((varying_reading(report), 0.0)))
            .collect();

        let curves = run_curves(&values, 0..60, &geometry);

        assert_eq!(curves.len(), 1, "{curves:?}");
    }

    #[test]
    fn a_range_of_fifteen_minutes_draws_a_point_every_three_seconds() {
        // Goal: a 15-minute chart in an overview card draws a point for each pixel of the card at
        // most, as Grafana sizes its step, at the shortest bucket length on the clock's list.
        // Method: draw a reading in each second of a 900-second window from 1200 on a 900-pixel
        // plot in an overview card, 2.5 seconds a pixel, and read the count and spacing of its
        // points.
        let (count, spacing) = point_spacing(1200, 900, None, CARD_WIDTH);

        assert_eq!(count, 299);
        for gap in spacing {
            assert!((gap - 3.0).abs() < 0.01, "{gap}");
        }
    }

    #[test]
    fn a_narrower_card_spaces_the_points_of_an_hour_further_apart() {
        // Goal: as Grafana sizes its step, a chart draws a point for each pixel of its card at
        // most, so a wide card shows as much detail as the readings allow and a narrow one stays
        // calm. Method: draw an hour in cards a quarter, a half, one and three overview cards
        // wide, and read the count and spacing of points.
        let widths = [
            CARD_WIDTH / 4.0,
            CARD_WIDTH / 2.0,
            CARD_WIDTH,
            CARD_WIDTH * 3.0,
        ];

        let drawn = widths.map(|width| point_spacing(3600, 3600, None, width));

        let expected = [(89, 10.0), (179, 5.0), (359, 2.5), (899, 1.0)];
        for ((count, spacing), (count_expected, gap_expected)) in drawn.into_iter().zip(expected) {
            assert_eq!(count, count_expected);
            for gap in spacing {
                assert!((gap - gap_expected).abs() < 0.01, "{gap}");
            }
        }
    }

    #[test]
    fn bucket_lengths_stop_at_a_second_and_at_half_an_hour() {
        // Goal: a card wider than its readings draws each second, the collector's tick, and one
        // too narrow for any length draws half an hour, never a length off the clock's list.
        // Method: size the buckets of a 15-minute window for cards of no width, a tenth of a
        // pixel and a million pixels.
        let window = TimeWindow {
            first: 1000,
            span: 900,
        };

        let lengths = [0.0, 0.1, 1_000_000.0]
            .map(|width| Buckets::of(window, None, TICK_SECONDS, width).length_seconds);

        assert_eq!(lengths, [1800, 1800, 1]);
    }

    #[test]
    fn a_card_exactly_a_pixel_a_point_keeps_the_shorter_bucket() {
        // Goal: a card with exactly one pixel for each point keeps that length, so the length
        // steps up only once points would outnumber pixels. Method: size the buckets of an hour
        // for a card of 180 pixels, where 20 seconds is exactly a pixel a point, and for a card
        // a pixel narrower.
        let window = TimeWindow {
            first: 1000,
            span: 3600,
        };

        let lengths = [180.0, 179.0]
            .map(|width| Buckets::of(window, None, TICK_SECONDS, width).length_seconds);

        assert_eq!(lengths, [20, 24]);
    }

    #[test]
    fn a_point_averages_grafanas_rate_interval_for_one_second_readings_in_whole_buckets() {
        // Goal: each point averages the longer of four seconds and a bucket and a second, as
        // Grafana's `$__rate_interval` reaches for a one-second scrape interval, in whole
        // buckets. Method: size the buckets of 15 minutes for cards that give lengths of 1, 2,
        // 3, 10 and 60 seconds, and read how many buckets each window holds.
        let window = TimeWindow {
            first: 1000,
            span: 900,
        };

        let sized = [900.0, 450.0, 300.0, 90.0, 15.0].map(|width| {
            let buckets = Buckets::of(window, None, TICK_SECONDS, width);
            (buckets.length_seconds, buckets.per_point)
        });

        assert_eq!(sized, [(1, 4), (2, 2), (3, 2), (10, 2), (60, 2)]);
    }

    #[test]
    fn a_panel_sizes_its_buckets_for_its_card_once_laid_out() {
        // Goal: a chart draws buckets for the card it is laid out in, and for an overview card
        // before its first layout. Method: read the bucket length of a 15-minute chart before
        // layout and after it learns of a card 45 pixels wide.
        let panel = live_panel(1000, &[]);
        let before = panel_buckets(&panel).map(|buckets| buckets.length_seconds);

        panel.chart_width.set(Some(45.0));
        let after = panel_buckets(&panel).map(|buckets| buckets.length_seconds);

        assert_eq!([before, after], [Some(3), Some(20)]);
    }

    #[test]
    fn the_value_axis_spans_the_points_the_curve_draws() {
        // Goal: the axis fits the curve through the window means, so a one-second spike that the
        // mean flattens does not shrink the curve to the bottom of the chart.
        // Method: put one reading of 90 among readings of 10 on an overview card, which raises
        // the mean of each six-second window holding it to 10 + 80 / 6, and read the axis.
        let values: Vec<_> = (0..900)
            .map(|offset| Some(if offset == 500 { 90.0 } else { 10.0 }))
            .collect();

        let (_, maximum) = domain(&live_panel(1000, &values), LIVE_INSPECTION, &[]);

        assert!(maximum >= 10.0 + 80.0 / 6.0, "{maximum}");
        assert!(maximum < 90.0, "{maximum}");
    }

    #[test]
    fn the_value_axis_ignores_a_reading_whose_bucket_is_not_drawn() {
        // Goal: a reading in a bucket a window edge cuts cannot rescale the points drawn.
        // Method: put readings of 90 in 1900 and 1901, the cut newest bucket of the window from
        // 1002, and read the axis of a steady chart.
        let values: Vec<_> = (0..900)
            .map(|offset| Some(if offset >= 898 { 90.0 } else { 10.0 }))
            .collect();

        let (_, maximum) = domain(&live_panel(1002, &values), LIVE_INSPECTION, &[]);

        assert!(maximum < 20.0, "{maximum}");
    }
}
