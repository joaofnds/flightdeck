use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{
    Background, Bounds, IntoElement, Path, PathBuilder, Pixels, Point, Window, canvas, fill,
    linear_color_stop, linear_gradient, point, prelude::*, px, rgb, size,
};

pub use crate::chart_axes::{AxisSide, TimeWindow, axis_column, axis_labels, time_labels};
use crate::data::{MachinePlace, Panel};

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
    plot_cursor(panel, inspection, hidden, measured, (true, None), None)
}

/// A plot with no cursor, which paints its curves through `held_curves` where it holds them.
pub fn plot_without_cursor(
    panel: Rc<Panel>,
    inspection: Inspection,
    hidden: Rc<Vec<String>>,
    measured: Rc<Cell<Bounds<Pixels>>>,
    time: Option<TimeWindow>,
    held_curves: Option<Rc<HeldCurves>>,
) -> impl IntoElement {
    plot_cursor(
        panel,
        inspection,
        hidden,
        measured,
        (false, time),
        held_curves,
    )
}

fn plot_cursor(
    panel: Rc<Panel>,
    inspection: Inspection,
    hidden: Rc<Vec<String>>,
    measured: Rc<Cell<Bounds<Pixels>>>,
    (show_cursor, time): (bool, Option<TimeWindow>),
    held_curves: Option<Rc<HeldCurves>>,
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
                _ => {
                    let held_curves = held_curves.as_deref();
                    paint_lines(&panel, chart, inspection, &hidden, held_curves, window);
                }
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
    held_curves: Option<&HeldCurves>,
    window: &mut Window,
) {
    HeldCurves::hold(held_curves, |mut frame| {
        prepare_lines(
            panel,
            bounds,
            inspection,
            hidden,
            |line, values, geometry| {
                if line_marks_peaks(panel, line, hidden) {
                    paint_line_peaks(line, values, geometry, window);
                }
                let frame = frame.as_deref_mut();
                paint_line_runs(line, panel, values, geometry, frame, window);
            },
        );
    });
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
            places: panel.places.as_deref(),
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
    /// The reading at each of `positions`, where the panel names them.
    pub(crate) places: Option<&'a [MachinePlace]>,
}

impl Geometry<'_> {
    /// Where the chart's readings fall on the clock, or `None` for a chart with no clock.
    fn clock(&self) -> Option<Clock<'_>> {
        Some(Clock {
            buckets: self.buckets?,
            positions: self.positions?,
            lower: self.lower,
            sliding: self.sliding,
            places: self.places,
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
    mut frame: Option<&mut CurveFrame<'_>>,
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
                paint_curve(line, panel, curve, geometry, frame.as_deref_mut(), window);
            }
        },
    );
}

/// One unbroken curve, as its points in time order and, on a chart with a clock, the window
/// mean each point stands for.
#[derive(Debug)]
struct Curve {
    means: Vec<WindowMean>,
    points: Vec<RunPoint>,
}

/// The unbroken curves through one run of readings. A chart with a clock draws a point at the
/// end of each bucket that holds readings of its own, and breaks the curve at each bucket that
/// holds none. The bucket the join of thinned and full-detail readings cuts holds readings the
/// chart leaves out, so a still chart's curve runs past it. A sliding chart draws no window the
/// join parts, so its curve breaks there. A chart with no clock draws each reading.
fn run_curves(
    values: &[Option<(f32, f32)>],
    run: std::ops::Range<usize>,
    geometry: &Geometry<'_>,
) -> Vec<Curve> {
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
        return vec![Curve {
            means: Vec::new(),
            points,
        }];
    };

    window_means(values, run, clock)
        .chunk_by(|left, right| clock.buckets.joins(left.end, right.end))
        .map(|means| Curve {
            points: means
                .iter()
                .map(|mean| RunPoint {
                    top: geometry.clock_point(clock.buckets, mean.end, mean.top),
                    bottom: geometry.clock_point(clock.buckets, mean.end, mean.bottom),
                })
                .collect(),
            means: means.to_vec(),
        })
        .collect()
}

/// Whether `line` marks the peaks its curve smooths. A restated line has no peak of its own. A
/// stacked line reaches only as its whole stack, so only the line that names that reach marks it,
/// and not with part of the stack hidden.
fn line_marks_peaks(panel: &Panel, line: &crate::data::Line, hidden: &[String]) -> bool {
    if line.follows.is_some() {
        return false;
    }
    if !stacked(panel, line) {
        return true;
    }

    line.extremes.is_some()
        && !panel
            .lines
            .iter()
            .any(|shown| stacked(panel, shown) && hidden.contains(&shown.name))
}

/// Draws a stem from the curve to each peak that stands out of `line`, and a cap at its tip, so a
/// reading the curve's mean smooths or a thinned point leaves out stays on the chart.
fn paint_line_peaks(
    line: &crate::data::Line,
    values: &[Option<(f32, f32)>],
    geometry: &Geometry<'_>,
    window: &mut Window,
) {
    let tint = color(&line.color);
    for mark in peak_marks(line, values, geometry) {
        let stem = Bounds::from_corners(
            point(mark.x, mark.tip.min(mark.base)),
            point(mark.x + PEAK_STEM_WIDTH, mark.tip.max(mark.base)),
        );
        window.paint_quad(fill(stem, alpha(tint, PEAK_STEM_OPACITY)));
        let cap_top = if mark.tip <= mark.base {
            mark.tip
        } else {
            mark.tip - PEAK_CAP_HEIGHT
        };
        let cap = Bounds::from_corners(
            point(mark.x - PEAK_CAP_WIDTH / 2.0, cap_top),
            point(mark.x + PEAK_CAP_WIDTH / 2.0, cap_top + PEAK_CAP_HEIGHT),
        );
        window.paint_quad(fill(cap, rgb(tint)));
    }
}

/// Where a standing peak draws on the plot.
struct PeakMark {
    x: Pixels,
    base: Pixels,
    tip: Pixels,
    pick: PeakPick,
}

/// The marks of the peaks that stand out of `line`.
fn peak_marks(
    line: &crate::data::Line,
    values: &[Option<(f32, f32)>],
    geometry: &Geometry<'_>,
) -> Vec<PeakMark> {
    let Some(clock) = geometry.clock() else {
        return Vec::new();
    };
    let overshoot_min = (geometry.maximum - geometry.minimum)
        * (PEAK_OVERSHOOT_HEIGHT_MIN / geometry.bounds.size.height);

    standing_peaks(line, values, geometry.segments, clock, overshoot_min)
        .into_iter()
        .map(|peak| {
            let base = geometry.clock_point(clock.buckets, peak.end, peak.base);

            PeakMark {
                x: base.x,
                base: base.y,
                tip: peak_tip(geometry, peak.peak),
                pick: peak.pick,
            }
        })
        .collect()
}

/// The reading a peak's mark stands for, which picking the mark holds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct PeakPick {
    /// The second it was read in.
    pub second: u64,
    /// Which reading of that second, where the panel names its readings. Two runs can read the
    /// same wall second.
    pub place: Option<MachinePlace>,
}

/// The peak whose mark the pointer at `pointer` is on, the nearest across where several are, in
/// the chart `bounds` holds. Ordinary places on the plot pick no peak, so a hover beside a mark but
/// off its height still shows the second under the pointer.
pub(crate) fn peak_under(
    panel: &Panel,
    bounds: Bounds<Pixels>,
    inspection: Inspection,
    hidden: &[String],
    pointer: gpui::Point<Pixels>,
) -> Option<PeakPick> {
    if matches!(
        panel.kind.as_str(),
        "timeline" | "heatmap" | "core-bars" | "pressure"
    ) {
        return None;
    }

    let mut nearest: Option<(Pixels, PeakPick)> = None;
    prepare_lines(
        panel,
        plot_bounds(bounds),
        inspection,
        hidden,
        |line, values, geometry| {
            if !line_marks_peaks(panel, line, hidden) {
                return;
            }
            for mark in peak_marks(line, values, geometry) {
                let across = (mark.x - pointer.x).abs();
                let along = pointer.y >= mark.tip.min(mark.base) - PEAK_TARGET_REACH
                    && pointer.y <= mark.tip.max(mark.base) + PEAK_TARGET_REACH;
                if across <= PEAK_TARGET_REACH
                    && along
                    && nearest.is_none_or(|(least, _)| across <= least)
                {
                    nearest = Some((across, mark.pick));
                }
            }
        },
    );

    nearest.map(|(_, pick)| pick)
}

/// Where the mark of a peak at `value` tips. The axis spans what the curve draws, so a peak beyond
/// it marks the edge of the plot rather than stretching the chart.
fn peak_tip(geometry: &Geometry<'_>, value: f32) -> Pixels {
    geometry
        .y(value)
        .clamp(geometry.bounds.top(), geometry.bounds.bottom())
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

/// Draws one unbroken curve, in time order, with the area beneath it, moving the paths `frame`
/// holds from the last paint where it holds the curve.
fn paint_curve(
    line: &crate::data::Line,
    panel: &Panel,
    curve: Curve,
    geometry: &Geometry<'_>,
    frame: Option<&mut CurveFrame<'_>>,
    window: &mut Window,
) {
    let tint = color(&line.color);
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

    let paths = held_curve_paths(line, panel, curve, geometry, frame);
    if let Some((area, fill)) = paths.area {
        window.paint_path(area, fill);
    }
    if let Some(stroke) = paths.stroke {
        window.paint_path(stroke, rgb(tint));
    }
}

/// The paths of `curve`, which has at least two points: those `frame` holds for the curve
/// painted in its order last, moved to where `curve` stands, where that curve had the shape
/// `curve` has, and a new tessellation otherwise.
fn held_curve_paths(
    line: &crate::data::Line,
    panel: &Panel,
    curve: Curve,
    geometry: &Geometry<'_>,
    frame: Option<&mut CurveFrame<'_>>,
) -> CurvePaths {
    assert!(curve.points.len() > 1);
    let look = CurveLook::of(line, panel);
    let Curve { means, points } = curve;
    let Some(frame) = frame else {
        return curve_paths(look, &points, geometry.bounds);
    };
    // The points of a chart with no clock stand for readings, which no mean records.
    let Some(clock) = geometry.clock() else {
        return curve_paths(look, &points, geometry.bounds);
    };
    assert_eq!(means.len(), points.len());

    let window = clock.buckets.window;
    let shape = CurveShape {
        means,
        look,
        scale: [geometry.minimum, geometry.maximum],
        size: geometry.bounds.size,
        span: window.span,
    };
    let place = CurvePlace {
        first: window.first,
        origin: geometry.bounds.origin,
    };
    frame.paths(shape, place, || curve_paths(look, &points, geometry.bounds))
}

/// How a line's curve is drawn, whatever its points.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CurveLook {
    tint: u32,
    dashed: bool,
    stacked: bool,
    /// The opacity of the area beneath the curve at the plot's top.
    opacity: f32,
    negative: bool,
}

impl CurveLook {
    fn of(line: &crate::data::Line, panel: &Panel) -> Self {
        let stacked = stacked(panel, line);
        let default_opacity = if stacked { 0.45 } else { 0.12 };
        Self {
            tint: color(&line.color),
            dashed: line.dashed == Some(true),
            stacked,
            opacity: line.fill.or(panel.fill).unwrap_or(default_opacity),
            negative: line.negative == Some(true),
        }
    }
}

/// The tessellated stroke of the curve through `points` on `plot` and, for a curve that is
/// neither dashed nor clear, the area beneath it with its fill.
fn curve_paths(look: CurveLook, points: &[RunPoint], plot: Bounds<Pixels>) -> CurvePaths {
    let width = if look.stacked { px(1.0) } else { px(2.0) };
    let mut stroke = PathBuilder::stroke(width);
    if look.dashed {
        stroke = stroke.dash_array(&[px(6.0), px(4.0)]);
    }
    let top: Vec<_> = points.iter().map(|sample| sample.top).collect();
    smooth(&mut stroke, &top, true);

    let mut area = None;
    if !look.dashed {
        if look.opacity > 0.0 {
            let bottom: Vec<_> = points.iter().rev().map(|sample| sample.bottom).collect();
            let mut builder = PathBuilder::fill();
            smooth(&mut builder, &top, true);
            smooth(&mut builder, &bottom, false);
            builder.close();
            let span = points
                .iter()
                .fold(None, |span: Option<(Pixels, Pixels)>, sample| {
                    let (low, high) = span.unwrap_or((sample.top.y, sample.bottom.y));
                    Some((low.min(sample.top.y), high.max(sample.bottom.y)))
                });
            let fill = curve_fill(look, plot, span);
            area = built_path(builder).map(|path| (path, fill));
        }
    }

    CurvePaths {
        stroke: built_path(stroke),
        area,
    }
}

/// The prototype fades every area from its full opacity at the plot's top to nothing at the
/// plot's bottom, however tall the area is. A gradient spans the area's own bounds, so each end
/// takes the opacity the plot-wide fade has at that height.
fn curve_fill(look: CurveLook, plot: Bounds<Pixels>, span: Option<(Pixels, Pixels)>) -> Background {
    let Some((high, low)) = span else {
        return alpha(look.tint, 0.0).into();
    };
    let fade = |y: Pixels| {
        let depth = ((y - plot.origin.y) / plot.size.height).clamp(0.0, 1.0);
        let level = if look.negative { depth } else { 1.0 - depth };
        alpha(look.tint, look.opacity * level)
    };

    linear_gradient(
        180.0,
        linear_color_stop(fade(high.min(low)), 0.0),
        linear_color_stop(fade(high.max(low)), 1.0),
    )
}

/// The tessellation of `builder`, or `None`, reported, where lyon refuses the path.
fn built_path(builder: PathBuilder) -> Option<Path<Pixels>> {
    match builder.build() {
        Ok(path) => Some(path),
        Err(error) => {
            eprintln!("chart path: {error}");
            None
        }
    }
}

/// The tessellated paths of one curve: its stroke, and the area beneath it with its fill.
#[derive(Clone)]
struct CurvePaths {
    stroke: Option<Path<Pixels>>,
    area: Option<(Path<Pixels>, Background)>,
}

impl CurvePaths {
    /// These paths, each moved by `offset`.
    fn moved(&self, offset: Point<Pixels>) -> Self {
        let moved = |path: &Path<Pixels>| {
            let mut path = path.clone();
            path.bounds.origin += offset;
            for vertex in &mut path.vertices {
                vertex.xy_position += offset;
            }
            path
        };

        Self {
            stroke: self.stroke.as_ref().map(moved),
            area: self.area.as_ref().map(|(path, fill)| (moved(path), *fill)),
        }
    }
}

/// The most curves a plot holds, far more than the lines and breaks of any chart draw. A plot
/// tessellates each curve past them at every paint.
const HELD_CURVES_MAX: usize = 256;

/// The paths of the curves a plot painted last, each with the place it was tessellated for. The
/// window of a following chart slides a second at a time while its buckets stay pinned to the
/// clock, so most paints find each curve with the shape it had, standing a second's width
/// further left, and move the paths held for it rather than tessellate them again.
pub struct HeldCurves {
    curves: RefCell<Vec<HeldCurve>>,
    /// How many curves the plots sharing it have tessellated, which the page's reports give.
    builds: Rc<Cell<u64>>,
}

struct HeldCurve {
    shape: CurveShape,
    place: CurvePlace,
    paths: CurvePaths,
}

impl HeldCurves {
    pub fn new(builds: Rc<Cell<u64>>) -> Self {
        Self {
            curves: RefCell::new(Vec::new()),
            builds,
        }
    }

    /// Runs `paint` with the curves `held` holds from the last paint, and holds the curves it
    /// paints for the next. Where nothing holds a plot's curves, `paint` tessellates each.
    fn hold(held: Option<&Self>, paint: impl FnOnce(Option<&mut CurveFrame<'_>>)) {
        let Some(held) = held else {
            paint(None);
            return;
        };

        let last = held.curves.take();
        let mut frame = CurveFrame {
            next: Vec::with_capacity(last.len()),
            last: last.into_iter(),
            builds: &held.builds,
        };
        paint(Some(&mut frame));
        assert!(frame.next.len() <= HELD_CURVES_MAX);
        *held.curves.borrow_mut() = frame.next;
    }
}

/// One paint of a plot's curves, which meets them in the order the last paint did.
struct CurveFrame<'a> {
    last: std::vec::IntoIter<HeldCurve>,
    next: Vec<HeldCurve>,
    builds: &'a Cell<u64>,
}

impl CurveFrame<'_> {
    /// The paths of the next curve, of `shape` at `place`: the last paint's curve in the same
    /// order moved to `place` where it had `shape`, and the tessellation `build` makes otherwise.
    fn paths(
        &mut self,
        shape: CurveShape,
        place: CurvePlace,
        build: impl FnOnce() -> CurvePaths,
    ) -> CurvePaths {
        if let Some(last) = self.last.next() {
            if last.shape == shape {
                let paths = last.paths.moved(last.place.offset(place, &last.shape));
                self.next.push(last);
                return paths;
            }
        }

        let paths = build();
        self.builds.set(self.builds.get() + 1);
        if self.next.len() < HELD_CURVES_MAX {
            self.next.push(HeldCurve {
                shape,
                place,
                paths: paths.clone(),
            });
        }
        paths
    }
}

/// What a curve's paths depend on besides where its window starts and where its plot stands:
/// the window means its points stand for, how it is drawn, its scale, the plot's size and the
/// seconds its window spans. Two curves of one shape differ only by a move.
#[derive(PartialEq)]
struct CurveShape {
    means: Vec<WindowMean>,
    look: CurveLook,
    scale: [f32; 2],
    size: gpui::Size<Pixels>,
    span: u64,
}

/// Where a curve stands: the first second of its window and the top left corner of its plot.
#[derive(Clone, Copy)]
struct CurvePlace {
    first: u64,
    origin: Point<Pixels>,
}

impl CurvePlace {
    /// How far a curve of `shape` tessellated at `self` moves to stand at `to`: as far as its plot
    /// moved, and back across the plot by the share of its window the window slid on by.
    fn offset(self, to: Self, shape: &CurveShape) -> Point<Pixels> {
        let moved = to.origin - self.origin;
        let slide = if to.first >= self.first {
            -(shape.size.width * window_fraction(to.first - self.first, shape.span))
        } else {
            shape.size.width * window_fraction(self.first - to.first, shape.span)
        };
        point(moved.x + slide, moved.y)
    }
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
/// How far beyond its curve a peak must lie to draw. A shorter mark is too small to read.
const PEAK_OVERSHOOT_HEIGHT_MIN: Pixels = px(6.0);
/// How few buckets a chart can mark a peak in. Two buckets show a rise and not a peak, which needs
/// readings on both sides or a line to stand out of, and the curve reaches the newer of them as
/// soon as more readings join it.
const PEAK_BUCKETS_MIN: usize = 3;

/// How many times the overshoot three quarters of its line's buckets stay within a peak must reach
/// to draw. Drawing how far every bucket reaches read as shaky on a 15-minute chart, so a chart
/// marks what stands out of its noise. Half a line's buckets lie under its curve, so the median
/// would read as no noise.
const PEAK_OVERSHOOT_TYPICAL_RATIO: f32 = 2.0;
const PEAK_STEM_OPACITY: f32 = 0.55;
const PEAK_STEM_WIDTH: Pixels = px(1.0);
const PEAK_CAP_WIDTH: Pixels = px(5.0);
const PEAK_CAP_HEIGHT: Pixels = px(2.0);
/// How far from a peak's mark, across and along it, the pointer still picks the peak, which makes
/// the mark a target at least 24 points wide.
const PEAK_TARGET_REACH: Pixels = px(12.0);
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
        let Some(offset) = second.checked_sub(self.window.first) else {
            panic!("a point's second lies after its window's first");
        };
        window_fraction(offset, self.window.span)
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

    /// The bucket whose range holds a reading at `position` across the window, or `None` for a
    /// reading before the window, or one a sliding window's newest second may still join.
    fn range_index(&self, position: f32, sliding: bool) -> Option<u64> {
        let second = self.second_of(position)?;
        if second < self.window.first {
            return None;
        }
        if sliding && second >= self.drawn_end {
            return None;
        }

        // Floors, so the index counts whole bucket lengths since the clock's zero.
        Some(second / self.length_seconds)
    }

    /// The second the bucket `index` ends at, as far as the window shows it.
    fn range_end(&self, index: u64) -> u64 {
        let end = (index + 1) * self.length_seconds;

        end.min(self.window.first + self.window.span)
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

    /// The first bucket of the window of buckets a point averages, the point at the end of the
    /// bucket `newest_bucket`.
    fn window_first_bucket(&self, newest_bucket: u64) -> u64 {
        (newest_bucket + 1).saturating_sub(self.per_point)
    }

    /// Whether the window of the point at the end of the bucket `newest_bucket` reaches across the
    /// join of thinned and full-detail readings, holding buckets both before and from the first
    /// bucket wholly of full-detail readings.
    fn join_parts(&self, newest_bucket: u64) -> bool {
        let Some(join) = self.detail_first else {
            return false;
        };
        let detail_bucket_first = join.div_ceil(self.length_seconds);
        if self.window_first_bucket(newest_bucket) >= detail_bucket_first {
            return false;
        }
        detail_bucket_first <= newest_bucket
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

/// The share of a window of `span` seconds that `seconds` cover.
fn window_fraction(seconds: u64, span: u64) -> f32 {
    let span = match u32::try_from(span) {
        Ok(span) => f64::from(span),
        Err(error) => panic!("a chart spans at most a few hours of seconds: {error}"),
    };
    let seconds = match u32::try_from(seconds) {
        Ok(seconds) => f64::from(seconds),
        Err(error) => panic!("a point lies within a few hours of its window's first: {error}"),
    };
    match num_traits::ToPrimitive::to_f32(&(seconds / span)) {
        Some(fraction) => fraction,
        None => panic!("a fraction of a window fits f32"),
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
    /// The reading at each of `positions`, where the panel names them.
    places: Option<&'a [MachinePlace]>,
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
/// or `None` when `recent` holds no bucket or the point stands before the window's oldest edge. A
/// sliding chart names `sliding_whole_from`, the first bucket no slide of the window can still
/// change, and draws no point whose window reaches before it or that the join of thinned and
/// full-detail readings parts. A still chart names none.
fn window_mean(
    recent: &std::collections::VecDeque<BucketTotal>,
    sliding_whole_from: Option<u64>,
    buckets: Buckets,
) -> Option<WindowMean> {
    let newest = recent.back()?.index;
    let end = (newest + 1) * buckets.length_seconds;
    if end < buckets.first_end {
        return None;
    }
    let start = buckets.window_first_bucket(newest);
    if let Some(whole_from) = sliding_whole_from {
        if start < whole_from {
            return None;
        }
        if buckets.join_parts(newest) {
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
/// breaks the curve. A point whose window holds readings that left the chart, or reaches across
/// the join of thinned and full-detail readings, is not drawn.
///
/// Readings before the first in view left a sliding window, so a run that starts there draws
/// only windows whole inside the chart. A run that starts at a change of segment has no
/// readings before it, so its first windows hold only its own.
fn window_means(
    values: &[Option<(f32, f32)>],
    run: std::ops::Range<usize>,
    clock: Clock<'_>,
) -> Vec<WindowMean> {
    let sliding_whole_from = first_whole_bucket(clock);
    let Clock {
        buckets,
        positions,
        lower,
        ..
    } = clock;
    let capacity = match usize::try_from(buckets.per_point) {
        Ok(capacity) => capacity,
        Err(error) => panic!("a window holds a few buckets: {error}"),
    };
    let mut recent: std::collections::VecDeque<BucketTotal> =
        std::collections::VecDeque::with_capacity(capacity);
    let mut means = Vec::with_capacity(run.len() / 8 + 1);
    for index in run {
        let Some((top, bottom)) = values[index] else {
            continue;
        };
        let Some(bucket) = buckets.bucket_index(positions[lower + index]) else {
            continue;
        };

        if let Some(open) = recent.back_mut() {
            if open.index == bucket {
                open.add(top, bottom);
                continue;
            }
        }
        means.extend(window_mean(&recent, sliding_whole_from, buckets));
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
    means.extend(window_mean(&recent, sliding_whole_from, buckets));
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

/// How far the readings of one bucket reach, which the mean a curve draws leaves out.
#[derive(Clone, Copy, Debug, PartialEq)]
struct BucketRange {
    index: u64,
    lowest: f32,
    highest: f32,
    /// The reading at the far end of the range from the line's zero.
    pick: PeakPick,
}

impl BucketRange {
    /// Takes in `next`, another range of the bucket.
    fn widen(&mut self, mirrored: bool, next: Self) {
        let further = if mirrored {
            next.lowest < self.lowest
        } else {
            next.highest > self.highest
        };
        if further {
            self.pick = next.pick;
        }
        self.lowest = self.lowest.min(next.lowest);
        self.highest = self.highest.max(next.highest);
    }
}

/// The range of each bucket in view that holds readings of `run`, in time order. Unlike a curve's
/// point, a range stands for its own bucket alone and draws the buckets a curve leaves out, those
/// the window's edge or the join of thinned and full-detail readings cuts, since a peak may lie in
/// any of them. A sliding chart waits only for the bucket the newest second may still change.
fn bucket_ranges(
    line: &crate::data::Line,
    values: &[Option<(f32, f32)>],
    run: std::ops::Range<usize>,
    clock: Clock<'_>,
) -> Vec<BucketRange> {
    let mut ranges: Vec<BucketRange> = Vec::with_capacity(run.len() / 8 + 1);
    for index in run {
        let Some(next) = reading_range(line, values[index], clock.lower + index, clock) else {
            continue;
        };
        match ranges.last_mut() {
            Some(open) if open.index == next.index => {
                open.widen(line.negative == Some(true), next);
            }
            _ => ranges.push(next),
        }
    }
    ranges
}

/// A reading that stands beyond the curve of its line.
#[derive(Clone, Copy, Debug, PartialEq)]
struct StandingPeak {
    /// The second the mark stands at, the end of the bucket as far as the window shows it.
    end: u64,
    /// The height of the curve there.
    base: f32,
    peak: f32,
    pick: PeakPick,
}

/// The peaks of `line` that stand out, in time order: those at least `overshoot_min` beyond the
/// curve, and `PEAK_OVERSHOOT_TYPICAL_RATIO` times as far as three quarters of its other buckets
/// reach, and the tallest of them wherever it goes past those buckets at all, since a line that
/// peaks often still has one reading above the rest.
fn standing_peaks(
    line: &crate::data::Line,
    values: &[Option<(f32, f32)>],
    segments: Option<&[u64]>,
    clock: Clock<'_>,
    overshoot_min: f32,
) -> Vec<StandingPeak> {
    let mut peaks = Vec::new();
    for_each_run(
        values.len(),
        segments,
        clock.lower,
        |_| true,
        |run| {
            let means = window_means(values, run.clone(), clock);
            for range in bucket_ranges(line, values, run, clock) {
                peaks.push(standing_peaks_of_bucket(line, &means, range, clock.buckets));
            }
        },
    );

    let mut overshoots: Vec<f32> = peaks
        .iter()
        .map(|peak| (peak.peak - peak.base).abs())
        .collect();
    overshoots.sort_unstable_by(f32::total_cmp);
    if overshoots.len() < PEAK_BUCKETS_MIN {
        return Vec::new();
    }
    let Some((&tallest, others)) = overshoots.split_last() else {
        return peaks;
    };
    // Floors, which only matters for a chart of a few buckets.
    let typical = others.get(others.len() * 3 / 4).copied().unwrap_or(0.0);
    let stands_out = PEAK_OVERSHOOT_TYPICAL_RATIO * typical;
    let least = if tallest > typical {
        stands_out.min(tallest)
    } else {
        stands_out
    };
    let least = overshoot_min.max(least);
    peaks.retain(|peak| (peak.peak - peak.base).abs() >= least);
    peaks
}

/// The peak of the bucket `range` holds, beside the curve's point for that bucket. A bucket with no
/// point of its own, which an edge of the window or the join cuts, takes the next point or else the
/// last, and a run too short for any point stands on the bucket's own far reading.
fn standing_peaks_of_bucket(
    line: &crate::data::Line,
    means: &[WindowMean],
    range: BucketRange,
    buckets: Buckets,
) -> StandingPeak {
    let bucket_end = (range.index + 1) * buckets.length_seconds;
    let next = means.partition_point(|mean| mean.end < bucket_end);
    let mean = means.get(next).or(means.last());
    let (peak, far) = if line.negative == Some(true) {
        (range.lowest, range.highest)
    } else {
        (range.highest, range.lowest)
    };
    let base = mean.map_or(far, |mean| mean.top);
    let base = if line.negative == Some(true) {
        base.max(peak)
    } else {
        base.min(peak)
    };

    StandingPeak {
        end: buckets.range_end(range.index),
        base,
        peak,
        pick: range.pick,
    }
}

/// The range of the reading at `position` of the panel, if it lies in a bucket a range stands for:
/// the reach the line names for it, widened by `value`'s own top, which it has with or without a
/// stack beneath it. The first detailed point names the reach of the stretch it joins, which can
/// end before a gap, so its own reading can lie past that reach.
fn reading_range(
    line: &crate::data::Line,
    value: Option<(f32, f32)>,
    position: usize,
    clock: Clock<'_>,
) -> Option<BucketRange> {
    let mirrored = line.negative == Some(true);
    let at = clock.positions[position];
    let index = clock.buckets.range_index(at, clock.sliding)?;
    let named = line
        .extremes
        .as_deref()
        .and_then(|extremes| extremes.get(position).copied().flatten())
        .map(|reach| {
            let (lowest, highest) = if mirrored {
                (-reach.high, -reach.low)
            } else {
                (reach.low, reach.high)
            };
            let pick = PeakPick {
                second: reach.peak.second(),
                place: Some(reach.peak),
            };
            BucketRange {
                index,
                lowest,
                highest,
                pick,
            }
        });
    let own = value.and_then(|(top, _)| {
        let place = clock.places.map(|places| places[position]);
        let second = match place {
            Some(place) => place.second(),
            None => clock.buckets.second_of(at)?,
        };
        Some(BucketRange {
            index,
            lowest: top,
            highest: top,
            pick: PeakPick { second, place },
        })
    });

    match (named, own) {
        (Some(mut named), Some(own)) => {
            named.widen(mirrored, own);
            Some(named)
        }
        (named, own) => named.or(own),
    }
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
    for stretch in smooth_stretches(points) {
        match stretch {
            Stretch::Straight(to) => builder.line_to(to),
            Stretch::Bend([to, leaving, reaching]) => {
                builder.cubic_bezier_to(to, leaving, reaching);
            }
        }
    }
}

/// One stretch of a curve, as the point it reaches and, for a bend, its two control points.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Stretch {
    Straight(gpui::Point<Pixels>),
    Bend([gpui::Point<Pixels>; 3]),
}

/// A straight stretch this narrow lies under a third of a pixel from the bend it stands for,
/// and costs the tessellator a fifth of the bend's triangles.
const STRAIGHT_STRETCH_WIDTH_MAX: Pixels = px(2.0);

/// The stretches of a curve through `points` in the order given, as GNOME System Monitor and
/// Smoothie Charts draw a live curve: a stretch leaves one point level and reaches the next
/// level, bending halfway between them, or runs straight where it is too narrow for the bend to
/// show. It depends on its two ends alone, so a point arriving or leaving reshapes no stretch
/// already drawn, and it never passes above or below its ends. Drawn the other way it traces the
/// same curve, which the bottom edge of an area relies on, since that edge runs back in time.
fn smooth_stretches(points: &[gpui::Point<Pixels>]) -> impl Iterator<Item = Stretch> + '_ {
    points.windows(2).map(|pair| {
        let (source, target) = (pair[0], pair[1]);
        if (target.x - source.x).abs() <= STRAIGHT_STRETCH_WIDTH_MAX {
            Stretch::Straight(target)
        } else {
            let halfway = source.x + (target.x - source.x) / 2.0;
            Stretch::Bend([target, point(halfway, source.y), point(halfway, target.y)])
        }
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
            places: panel.places.as_deref(),
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
            places: None,
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

    /// How far a reading reaches, read at a place the test does not look at.
    fn line_reach(low: f32, high: f32) -> crate::data::LineReach {
        crate::data::LineReach {
            low,
            high,
            peak: MachinePlace::default(),
        }
    }

    /// A line that names `extremes` as how far each of its readings reaches.
    fn reaching_line(extremes: Option<crate::data::LineExtremes>) -> crate::data::Line {
        crate::data::Line {
            name: "cpu".into(),
            color: "#73bf69".into(),
            values: Vec::new(),
            stack: None,
            negative: None,
            dashed: None,
            axis: None,
            follows: None,
            unit: None,
            fill: None,
            extremes,
        }
    }

    /// The ranges of the readings `values` of `line` across 900 seconds from 1,000, in a still
    /// chart with a card `card_width` pixels wide.
    fn ranges_at_width(
        line: &crate::data::Line,
        values: &[Option<(f32, f32)>],
        card_width: f32,
    ) -> (Vec<BucketRange>, u64) {
        let positions = live_positions();
        let window = TimeWindow {
            first: 1000,
            span: 900,
        };
        let buckets = Buckets::of(window, None, TICK_SECONDS, card_width);
        let clock = Clock {
            buckets,
            positions: &positions,
            lower: 0,
            sliding: false,
            places: None,
        };

        (
            bucket_ranges(line, values, 0..values.len(), clock),
            buckets.length_seconds,
        )
    }

    /// The peaks of the readings `values` of `line` that stand out across 900 seconds from 1,000,
    /// in a still chart with a card `card_width` pixels wide.
    fn peaks_at_width(
        line: &crate::data::Line,
        values: &[Option<(f32, f32)>],
        card_width: f32,
        overshoot_min: f32,
    ) -> Vec<StandingPeak> {
        let positions = live_positions();
        let window = TimeWindow {
            first: 1000,
            span: 900,
        };
        let clock = Clock {
            buckets: Buckets::of(window, None, TICK_SECONDS, card_width),
            positions: &positions,
            lower: 0,
            sliding: false,
            places: None,
        };

        standing_peaks(line, values, None, clock, overshoot_min)
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
        curves.remove(0).points
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
    fn a_peak_of_one_second_keeps_its_height_in_its_bucket_at_every_offset_and_width() {
        // Goal: a reading that lasted one second stays on the chart, whatever the chart's width
        // and wherever in its bucket it fell, though the mean a curve draws smooths it away.
        // Method: at widths that give each length of bucket, place a peak in each second of a
        // long enough stretch of the window and in its first and last seconds, and read the
        // bucket that holds it.
        let line = reaching_line(None);
        let widths = [900.0, 450.0, 300.0, 180.0, 90.0, 60.0, 30.0, 15.0];
        let seconds = (0..120).chain([899]);
        for width in widths {
            for second in seconds.clone() {
                let mut values = vec![Some((10.0, 0.0)); 900];
                values[second] = Some((95.0, 0.0));

                let (ranges, length) = ranges_at_width(&line, &values, width);

                let peak = (1000 + u64::try_from(second).unwrap()) / length;
                let held = ranges.iter().find(|range| range.index == peak);
                assert_eq!(
                    held.map(|range| range.highest),
                    Some(95.0),
                    "width {width} second {second} length {length}"
                );
                let others = ranges.iter().filter(|range| range.index != peak);
                assert!(
                    others
                        .into_iter()
                        .all(|range| (range.highest - 10.0).abs() < f32::EPSILON)
                );
            }
        }
    }

    #[test]
    fn a_point_that_stands_for_several_seconds_reaches_what_its_line_names() {
        // Goal: an older point draws one reading of its stretch, and its bucket reaches what the
        // whole stretch reached, or a peak past the stretch's first second would vanish as it ages.
        // Method: a reading of 10 that names a reach of 5 to 95, beside one that names none.
        let mut values = vec![None; 900];
        values[100] = Some((10.0, 0.0));
        values[110] = Some((10.0, 0.0));
        let mut extremes = vec![None; 900];
        extremes[100] = Some(line_reach(5.0, 95.0));
        let line = reaching_line(Some(extremes));

        let (ranges, _) = ranges_at_width(&line, &values, TEN_SECOND_CARD);

        let reached: Vec<_> = ranges
            .iter()
            .map(|range| (range.index, range.lowest, range.highest))
            .collect();
        assert_eq!(reached, [(110, 5.0, 95.0), (111, 10.0, 10.0)]);
    }

    #[test]
    fn a_stretch_whose_own_first_reading_is_missing_still_reaches_its_peak() {
        // Goal: a missing first second does not hide what the rest of its stretch read.
        // Method: a point with no value of its own that names a reach.
        let values = vec![None; 900];
        let mut extremes = vec![None; 900];
        extremes[100] = Some(line_reach(5.0, 95.0));
        let line = reaching_line(Some(extremes));

        let (ranges, _) = ranges_at_width(&line, &values, TEN_SECOND_CARD);

        assert_eq!(ranges.len(), 1);
        assert!((ranges[0].highest - 95.0).abs() < f32::EPSILON);
    }

    #[test]
    fn a_range_stays_within_its_own_buckets_readings_beside_a_gap() {
        // Goal: a bucket with no readings draws no range, so a band never bridges a gap, and zero
        // stays a reading. Method: readings of zero either side of a missing bucket.
        let mut values = vec![None; 900];
        values[100..110].fill(Some((0.0, 0.0)));
        values[120..130].fill(Some((0.0, 0.0)));
        let line = reaching_line(None);

        let (ranges, _) = ranges_at_width(&line, &values, TEN_SECOND_CARD);

        let indices: Vec<u64> = ranges.iter().map(|range| range.index).collect();
        assert_eq!(indices, [110, 112]);
        assert!(ranges.iter().all(|range| range.highest == 0.0));
    }

    #[test]
    fn a_line_mirrored_below_zero_reaches_down_from_zero() {
        // Goal: a line drawn below zero, such as traffic sent, reaches down, and the reach it names
        // turns over with it. Method: a negative line naming a reach of 2 to 8.
        let mut values = vec![None; 900];
        values[100] = Some((-5.0, 0.0));
        let mut extremes = vec![None; 900];
        extremes[100] = Some(line_reach(2.0, 8.0));
        let mut line = reaching_line(Some(extremes));
        line.negative = Some(true);

        let (ranges, _) = ranges_at_width(&line, &values, TEN_SECOND_CARD);

        assert!((ranges[0].lowest - (-8.0)).abs() < f32::EPSILON);
        assert!((ranges[0].highest - (-2.0)).abs() < f32::EPSILON);
    }

    #[test]
    fn a_sliding_chart_waits_only_for_the_bucket_its_newest_second_may_change() {
        // Goal: a reading of the newest bucket cannot rescale a sliding chart until no late tick
        // can change it, and a peak in the window's oldest bucket, which a curve leaves out, stays
        // shown. Method: peaks in the oldest second and the newest of a sliding window.
        let positions = live_positions();
        let window = TimeWindow {
            first: 1003,
            span: 900,
        };
        let clock = Clock {
            buckets: Buckets::of(window, None, TICK_SECONDS, TEN_SECOND_CARD),
            positions: &positions,
            lower: 0,
            sliding: true,
            places: None,
        };
        let mut values = vec![Some((10.0, 0.0)); 900];
        values[0] = Some((95.0, 0.0));
        values[899] = Some((80.0, 0.0));
        let line = reaching_line(None);

        let ranges = bucket_ranges(&line, &values, 0..900, clock);

        assert!((ranges[0].highest - 95.0).abs() < f32::EPSILON);
        assert!(
            ranges
                .iter()
                .all(|range| (range.highest - 80.0).abs() >= f32::EPSILON)
        );
    }

    #[test]
    fn a_chart_keeps_one_range_a_bucket_however_many_readings_it_covers() {
        // Goal: what a chart keeps of a line is bounded by the chart's width and not by how many
        // readings its span holds, so a wider span costs no more marks.
        // Method: every second turns, at widths that give each length of bucket, and the ranges
        // of those readings are counted against the buckets of the span.
        let line = reaching_line(None);
        let values: Vec<_> = (0..900)
            .map(|second| Some((varying_reading(second), 0.0)))
            .collect();
        for width in [900.0, 450.0, 300.0, 180.0, 90.0, 60.0, 30.0, 15.0] {
            let (ranges, length) = ranges_at_width(&line, &values, width);

            let buckets = 900_u64.div_ceil(length) + 1;
            assert!(
                collector::u64_from_usize(ranges.len()) <= buckets,
                "width {width}: {} ranges of {buckets} buckets",
                ranges.len(),
            );
            assert!(peaks_at_width(&line, &values, width, 1.0).len() <= ranges.len());
        }
    }

    #[test]
    fn a_peak_of_one_second_stands_out_of_a_steady_line_at_every_offset_and_width() {
        // Goal: a reading that lasted one second marks the chart at every width and wherever in
        // its bucket it fell, and nothing else does.
        // Method: at widths that give each length of bucket, place a peak in each second of a long
        // enough stretch of the window and in its last second, and read what stands.
        let line = reaching_line(None);
        let widths = [900.0, 450.0, 300.0, 180.0, 90.0, 60.0, 30.0, 15.0];
        let seconds = (1..120).chain([899]);
        for width in widths {
            for second in seconds.clone() {
                let mut values = vec![Some((10.0, 0.0)); 900];
                values[second] = Some((95.0, 0.0));

                let peaks = peaks_at_width(&line, &values, width, 1.0);

                let tips: Vec<f32> = peaks.iter().map(|peak| peak.peak).collect();
                assert_eq!(tips.len(), 1, "width {width} second {second}: {peaks:?}");
                assert!(
                    (tips[0] - 95.0).abs() < 0.01,
                    "width {width} second {second}"
                );
            }
        }
    }

    #[test]
    fn a_reading_the_first_point_draws_itself_marks_nothing() {
        // Goal: a curve whose first point holds a reading alone already reaches it.
        // Method: a chart of one-second buckets whose first reading is its peak.
        let line = reaching_line(None);
        let mut values = vec![Some((10.0, 0.0)); 900];
        values[0] = Some((95.0, 0.0));

        let peaks = peaks_at_width(&line, &values, 900.0, 1.0);

        assert_eq!(peaks, []);
    }

    #[test]
    fn the_ordinary_noise_of_a_line_marks_nothing() {
        // Goal: a chart that drew how far each bucket reached read as shaky, so what every bucket
        // does marks nothing. Method: a line that turns between two levels each second.
        let line = reaching_line(None);
        let values: Vec<_> = (0..900)
            .map(|second| Some((if second % 2 == 0 { 5.0 } else { 25.0 }, 0.0)))
            .collect();

        let peaks = peaks_at_width(&line, &values, 900.0, 1.0);

        assert_eq!(peaks, []);
    }

    #[test]
    fn a_peak_stands_out_of_a_noisy_line() {
        // Goal: noise raises what a peak must reach and does not hide one past it.
        // Method: the line that turns between two levels, with one reading far above them.
        let line = reaching_line(None);
        let mut values: Vec<_> = (0..900)
            .map(|second| Some((if second % 2 == 0 { 5.0 } else { 25.0 }, 0.0)))
            .collect();
        values[400] = Some((95.0, 0.0));

        let peaks = peaks_at_width(&line, &values, 900.0, 1.0);

        assert_eq!(peaks.len(), 1, "{peaks:?}");
        assert!((peaks[0].peak - 95.0).abs() < 0.01);
    }

    #[test]
    fn a_peak_too_short_to_read_marks_nothing() {
        // Goal: a mark shorter than the least a card can show is left out.
        // Method: one reading 2 above a steady line, with a least overshoot of 5 and of 1.
        let line = reaching_line(None);
        let mut values = vec![Some((10.0, 0.0)); 900];
        values[400] = Some((12.0, 0.0));

        let short = peaks_at_width(&line, &values, 900.0, 5.0);
        let tall = peaks_at_width(&line, &values, 900.0, 1.0);

        assert_eq!(short, []);
        assert_eq!(tall.len(), 1);
    }

    #[test]
    fn a_stretch_marks_what_its_line_names_it_reached() {
        // Goal: an older point draws one reading of its stretch, so the peak it names marks the
        // chart, or a peak past the stretch's first second would vanish as it ages.
        // Method: older points of 10 every ten seconds, one naming a reach of 95.
        let mut extremes = vec![None; 900];
        extremes[100] = Some(line_reach(5.0, 95.0));
        let line = reaching_line(Some(extremes));
        let values: Vec<_> = (0..900)
            .map(|second| (second % 10 == 0).then_some((10.0, 0.0)))
            .collect();

        let peaks = peaks_at_width(&line, &values, TEN_SECOND_CARD, 1.0);

        assert_eq!(peaks.len(), 1, "{peaks:?}");
        assert!((peaks[0].peak - 95.0).abs() < 0.01);
    }

    #[test]
    fn a_peak_of_a_line_mirrored_below_zero_stands_downward() {
        // Goal: a line drawn below zero peaks away from zero, so its mark falls from the curve.
        // Method: a mirrored line with one reading far below its level.
        let mut line = reaching_line(None);
        line.negative = Some(true);
        let mut values = vec![Some((-10.0, 0.0)); 900];
        values[400] = Some((-95.0, 0.0));

        let peaks = peaks_at_width(&line, &values, 900.0, 1.0);

        assert_eq!(peaks.len(), 1, "{peaks:?}");
        assert!((peaks[0].peak + 95.0).abs() < 0.01);
        assert!(peaks[0].base > peaks[0].peak);
    }

    #[test]
    fn a_peak_beyond_the_axis_marks_the_edge_of_the_plot() {
        // Goal: the axis spans what the curve draws, so a peak beyond it stays inside the plot.
        // Method: ask where the mark tips for readings above and below an axis of 0 to 20.
        let positions = live_positions();
        let geometry = Geometry {
            maximum: 20.0,
            ..live_geometry(1000, 0, &positions)
        };

        assert_eq!(peak_tip(&geometry, 95.0), geometry.bounds.top());
        assert_eq!(peak_tip(&geometry, -95.0), geometry.bounds.bottom());
    }

    /// The panel of 900 seconds from 1,000 with `values`, each reading named by its own place, on a
    /// card wide enough for buckets of a second.
    fn picked_panel(values: &[Option<f32>]) -> Panel {
        let mut panel = live_panel(1000, values);
        panel.places = Some((0..900).map(picked_place).collect());
        panel.chart_width.set(Some(900.0));
        panel
    }

    /// The place of the reading at `index` of a `picked_panel`.
    fn picked_place(index: u64) -> MachinePlace {
        MachinePlace {
            boot: [3, 4],
            run: [1, 2],
            publication: 5000 + index,
            wall_ns: (1000 + index) * 1_000_000_000,
        }
    }

    /// The readings of a `picked_panel`: 10 throughout, but for each of `peaks`, an index and what
    /// reads there.
    fn picked_values(peaks: &[(usize, f32)]) -> Vec<Option<f32>> {
        let mut values = vec![Some(10.0); 900];
        for (index, value) in peaks {
            values[*index] = Some(*value);
        }
        values
    }

    fn picked_bounds() -> Bounds<Pixels> {
        Bounds::new(point(px(0.0), px(0.0)), size(px(1000.0), px(200.0)))
    }

    fn picked_inspection() -> Inspection {
        Inspection {
            lower: 0,
            selected: 899,
            upper: 899,
        }
    }

    /// The marks `panel` draws in `picked_bounds`, line by line.
    fn picked_marks(panel: &Panel) -> Vec<PeakMark> {
        let mut marks = Vec::new();
        let plot = plot_bounds(picked_bounds());
        prepare_lines(
            panel,
            plot,
            picked_inspection(),
            &[],
            |line, values, geometry| {
                marks.extend(peak_marks(line, values, geometry));
            },
        );
        marks
    }

    fn picked_under(panel: &Panel, x: Pixels, y: Pixels) -> Option<PeakPick> {
        peak_under(
            panel,
            picked_bounds(),
            picked_inspection(),
            &[],
            point(x, y),
        )
    }

    #[test]
    fn a_pointer_on_a_peaks_mark_picks_the_reading_that_read_it() {
        // Goal: picking a peak names the exact reading, its run, boot and publication.
        // Method: one peak on a steady line, and point at the middle of its mark.
        let panel = picked_panel(&picked_values(&[(450, 95.0)]));
        let marks = picked_marks(&panel);
        assert_eq!(marks.len(), 1);
        let mark = &marks[0];

        let picked = picked_under(&panel, mark.x, (mark.tip + mark.base) / 2.0);

        let expected = PeakPick {
            second: 1450,
            place: Some(picked_place(450)),
        };
        assert_eq!(picked, Some(expected));
    }

    #[test]
    fn a_mark_is_a_target_twenty_four_points_wide_and_no_more() {
        // Goal: a person can land on a one-pixel mark, and the rest of the plot still hovers the
        // second under the pointer.
        // Method: point at the edges of the reach across the mark and along it, and just past them.
        let panel = picked_panel(&picked_values(&[(450, 95.0)]));
        let marks = picked_marks(&panel);
        let mark = &marks[0];
        let middle = (mark.tip + mark.base) / 2.0;
        let (top, bottom) = (mark.tip.min(mark.base), mark.tip.max(mark.base));

        assert!(picked_under(&panel, mark.x - px(12.0), middle).is_some());
        assert!(picked_under(&panel, mark.x + px(12.0), middle).is_some());
        assert!(picked_under(&panel, mark.x - px(13.0), middle).is_none());
        assert!(picked_under(&panel, mark.x + px(13.0), middle).is_none());
        assert!(picked_under(&panel, mark.x, top - px(12.0)).is_some());
        assert!(picked_under(&panel, mark.x, bottom + px(12.0)).is_some());
        assert!(picked_under(&panel, mark.x, top - px(13.0)).is_none());
        assert!(picked_under(&panel, mark.x, bottom + px(13.0)).is_none());
    }

    #[test]
    fn the_nearer_of_two_marks_is_picked() {
        // Goal: where the targets of two peaks overlap the pointer picks the mark it is nearest.
        // Method: two peaks 20 seconds apart, and point just beside each of them.
        let panel = picked_panel(&picked_values(&[(450, 95.0), (470, 95.0)]));
        let marks = picked_marks(&panel);
        let (first, second) = (&marks[0], &marks[1]);
        let middle = (first.tip + first.base) / 2.0;

        let near_first = picked_under(&panel, first.x + px(5.0), middle).unwrap();
        let near_second = picked_under(&panel, second.x - px(5.0), middle).unwrap();

        assert_eq!(near_first.second, 1450);
        assert_eq!(near_second.second, 1470);
    }

    #[test]
    fn the_mark_of_a_bucket_picks_its_furthest_reading() {
        // Goal: a bucket of several readings picks the one the mark reaches, whichever came first.
        // Method: two readings in one ten-second bucket, higher first and then higher last, on a
        // chart that peaks upward and on one mirrored below zero.
        let higher_first = picked_values(&[(450, 95.0), (451, 60.0)]);
        let higher_last = picked_values(&[(450, 60.0), (451, 95.0)]);
        let picks = |values: &[Option<f32>], mirrored: bool| {
            let panel = picked_panel(values);
            panel.chart_width.set(Some(TEN_SECOND_CARD));
            let mut line = panel.lines[0].clone();
            line.negative = Some(mirrored);
            let mirrored_panel = Panel {
                lines: vec![line],
                ..panel
            };
            picked_marks(&mirrored_panel)[0].pick.second
        };

        assert_eq!(picks(&higher_first, false), 1450);
        assert_eq!(picks(&higher_last, false), 1451);
        assert_eq!(picks(&higher_first, true), 1450);
        assert_eq!(picks(&higher_last, true), 1451);
    }

    #[test]
    fn the_mark_of_a_stretch_picks_the_reading_its_line_names_and_not_the_point() {
        // Goal: the point of a stretch is one reading and its peak usually another, so the pick
        // holds the one the stretch names, which may belong to another run.
        // Method: a point that names a reach read at a place of its own.
        let mut panel = picked_panel(&picked_values(&[]));
        let peak = MachinePlace {
            boot: [7, 7],
            run: [8, 8],
            publication: 77,
            wall_ns: 1_453 * 1_000_000_000,
        };
        let mut extremes = vec![None; 900];
        extremes[450] = Some(crate::data::LineReach {
            low: 5.0,
            high: 95.0,
            peak,
        });
        panel.lines[0].extremes = Some(extremes);

        let marks = picked_marks(&panel);

        assert_eq!(marks.len(), 1);
        let expected = PeakPick {
            second: 1453,
            place: Some(peak),
        };
        assert_eq!(marks[0].pick, expected);
    }

    #[test]
    fn a_panel_that_names_no_readings_picks_a_peak_by_its_second() {
        // Goal: a chart with no places, such as one of a process, still holds its peak's second.
        // Method: a panel with no places, and the mark of its peak.
        let mut panel = picked_panel(&picked_values(&[(450, 95.0)]));
        panel.places = None;

        let marks = picked_marks(&panel);

        let expected = PeakPick {
            second: 1450,
            place: None,
        };
        assert_eq!(marks[0].pick, expected);
    }

    #[test]
    fn a_hidden_line_and_a_chart_of_states_offer_no_peak() {
        // Goal: only a mark on the plot can be picked. A hidden line draws none, and a chart that
        // draws no lines draws none.
        // Method: point at the mark's place with its line hidden, and on a chart of another kind.
        let panel = picked_panel(&picked_values(&[(450, 95.0)]));
        let marks = picked_marks(&panel);
        let at = point(marks[0].x, (marks[0].tip + marks[0].base) / 2.0);
        let hidden = [panel.lines[0].name.clone()];
        let bounds = picked_bounds();
        let inspection = picked_inspection();
        let states = Panel {
            kind: "timeline".into(),
            ..panel.clone()
        };

        assert!(peak_under(&panel, bounds, inspection, &hidden, at).is_none());
        assert!(peak_under(&states, bounds, inspection, &[], at).is_none());
    }

    #[test]
    fn two_runs_in_one_bucket_each_keep_their_own_peak() {
        // Goal: a run boundary is never crossed, so a bucket that two runs share marks each run's
        // peak rather than one peak standing for both.
        // Method: a ten-second bucket that a run boundary cuts, with a peak on each side, against
        // the same readings with no boundary.
        let mut values = vec![Some(10.0); 900];
        values[450] = Some(95.0);
        values[452] = Some(60.0);
        let line = reaching_line(None);
        let tops: Vec<_> = values
            .iter()
            .map(|value| value.map(|top| (top, 0.0)))
            .collect();
        let positions = live_positions();
        let segments: Vec<u64> = (0..900).map(|index| u64::from(index > 451)).collect();
        let clock = Clock {
            buckets: Buckets::of(
                TimeWindow {
                    first: 1000,
                    span: 900,
                },
                None,
                TICK_SECONDS,
                TEN_SECOND_CARD,
            ),
            positions: &positions,
            lower: 0,
            sliding: false,
            places: None,
        };
        let seconds = |segments: Option<&[u64]>| -> Vec<u64> {
            standing_peaks(&line, &tops, segments, clock, 6.0)
                .iter()
                .map(|peak| peak.pick.second)
                .collect()
        };

        assert_eq!(seconds(Some(&segments)), [1450, 1452]);
        assert_eq!(seconds(None), [1450]);
    }

    #[test]
    fn only_the_line_that_names_a_stacks_total_draws_how_far_the_stack_reaches() {
        // Goal: one layer's own peak is not a peak of the stack, and with a layer hidden the total
        // the line names is no longer what the chart stacks. Method: ask for a stacked line that
        // names no reach, for one that does, and for the same with a line hidden.
        let mut panel = live_panel(1000, &[Some(10.0); 900]);
        panel.stack = Some(true);
        let layer = panel.lines[0].clone();
        let mut top = layer.clone();
        top.name = "top".into();
        top.extremes = Some(vec![None; 900]);
        panel.lines.push(top.clone());
        let hidden = ["top".to_owned()];

        assert!(!line_marks_peaks(&panel, &layer, &[]));
        assert!(line_marks_peaks(&panel, &top, &[]));
        assert!(!line_marks_peaks(&panel, &top, &hidden));
    }

    #[test]
    fn a_line_that_restates_another_marks_no_peak_of_its_own() {
        // Goal: the line it restates already marks the peak.
        // Method: ask for a line and for one that follows it.
        let panel = live_panel(1000, &[Some(10.0); 900]);
        let mut restated = panel.lines[0].clone();
        restated.follows = Some("cpu".into());

        assert!(line_marks_peaks(&panel, &panel.lines[0], &[]));
        assert!(!line_marks_peaks(&panel, &restated, &[]));
    }

    #[test]
    fn a_dashed_line_marks_its_peaks() {
        // Goal: a dash is how a line looks, and the Overview's fans draw theirs dashed, so a fan's
        // spike marks that chart. A dashed level never rises past its line, so it marks nothing.
        // Method: ask for a dashed line.
        let panel = live_panel(1000, &[Some(10.0); 900]);
        let mut dashed = panel.lines[0].clone();
        dashed.dashed = Some(true);

        assert!(line_marks_peaks(&panel, &dashed, &[]));
    }

    #[test]
    fn hiding_a_line_outside_a_stack_keeps_the_marks_of_the_stack() {
        // Goal: a guide drawn beside a stack is no part of its total, so hiding it leaves what the
        // top line names true. Method: a stack, a line outside it, and that line hidden.
        let mut panel = live_panel(1000, &[Some(10.0); 900]);
        panel.stack = Some(true);
        let mut top = panel.lines[0].clone();
        top.extremes = Some(vec![None; 900]);
        let mut guide = panel.lines[0].clone();
        guide.name = "guide".into();
        guide.stack = Some(false);
        panel.lines = vec![top.clone(), guide];

        assert!(line_marks_peaks(&panel, &top, &["guide".to_owned()]));
    }

    #[test]
    fn a_lone_peak_marks_a_chart_of_few_buckets() {
        // Goal: a short recording's one peak has no others to be ordinary among, so it does not
        // have to reach twice itself. Method: three readings, the middle one far above the others.
        let line = reaching_line(None);
        let mut values = vec![None; 900];
        values[100] = Some((10.0, 0.0));
        values[101] = Some((95.0, 0.0));
        values[102] = Some((10.0, 0.0));

        let peaks = peaks_at_width(&line, &values, 900.0, 1.0);

        assert_eq!(peaks.len(), 1, "{peaks:?}");
        assert!((peaks[0].peak - 95.0).abs() < 0.01);
    }

    #[test]
    fn the_tallest_peak_marks_a_line_that_peaks_often() {
        // Goal: a line peaking in over a quarter of its buckets still shows the one reading that
        // went past all the others. Method: 60 in every third ten-second bucket and one 95.
        let line = reaching_line(None);
        let mut values: Vec<_> = (0..900)
            .map(|second| Some((if second % 30 == 5 { 60.0 } else { 10.0 }, 0.0)))
            .collect();
        values[405] = Some((95.0, 0.0));

        let peaks = peaks_at_width(&line, &values, TEN_SECOND_CARD, 1.0);

        assert_eq!(peaks.len(), 1, "{peaks:?}");
        assert!((peaks[0].peak - 95.0).abs() < 0.01);
    }

    #[test]
    fn a_reading_past_what_its_line_names_widens_the_range() {
        // Goal: a point takes the reach of the stretch it joins, which may end before a gap, and
        // the point's own reading stays on the chart past that reach, picked by its own second.
        // Method: a reading of 95 whose line names a reach of 5 to 20.
        let mut values = vec![None; 900];
        values[100] = Some((95.0, 0.0));
        let mut extremes = vec![None; 900];
        extremes[100] = Some(line_reach(5.0, 20.0));
        let line = reaching_line(Some(extremes));

        let (ranges, _) = ranges_at_width(&line, &values, TEN_SECOND_CARD);

        assert_eq!(ranges.len(), 1);
        assert!((ranges[0].highest - 95.0).abs() < f32::EPSILON);
        assert!((ranges[0].lowest - 5.0).abs() < f32::EPSILON);
        assert_eq!(ranges[0].pick.second, 1100);
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
        assert_places(&curves[0].points[9..], &[[100.0, 80.0, 100.0]]);
        assert_places(
            &curves[1].points[..2],
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
        curves.remove(0).points
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

    /// A plot 900 pixels wide and 100 tall whose top left corner stands at `x` and `y`.
    fn plot_at(x: f32, y: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(900.0), px(100.0)))
    }

    /// The paths of the one curve a following 15-minute chart from `first` paints on `plot`
    /// through `held`, whose panel holds the minute before its oldest edge.
    fn led_paths(first: u64, plot: Bounds<Pixels>, held: &HeldCurves) -> CurvePaths {
        let (mut panel, inspection) = led_panel(first, 60, varying_reading);
        panel.maximum = Some(100.0);

        let mut paths = Vec::new();
        HeldCurves::hold(Some(held), |mut frame| {
            prepare_lines(&panel, plot, inspection, &[], |line, values, geometry| {
                for curve in run_curves(values, 0..values.len(), geometry) {
                    let frame = frame.as_deref_mut();
                    paths.push(held_curve_paths(line, &panel, curve, geometry, frame));
                }
            });
        });

        assert_eq!(paths.len(), 1);
        paths.remove(0)
    }

    /// The paths a plot that holds no curves yet builds for `led_paths`.
    fn built_led_paths(first: u64, plot: Bounds<Pixels>) -> CurvePaths {
        led_paths(first, plot, &HeldCurves::new(Rc::new(Cell::new(0))))
    }

    fn assert_near(value: gpui::Point<Pixels>, expected: gpui::Point<Pixels>) {
        assert!(
            (value.x - expected.x).abs() < px(0.01),
            "{value:?} {expected:?}"
        );
        assert!(
            (value.y - expected.y).abs() < px(0.01),
            "{value:?} {expected:?}"
        );
    }

    fn assert_same_bounds(path: &gpui::Path<Pixels>, expected: &gpui::Path<Pixels>) {
        assert_near(path.bounds.origin, expected.bounds.origin);
        assert_near(
            point(path.bounds.size.width, path.bounds.size.height),
            point(expected.bounds.size.width, expected.bounds.size.height),
        );
    }

    /// The square pixels the triangles of `path` cover.
    fn covered_area(path: &gpui::Path<Pixels>) -> f64 {
        assert!(path.vertices.len().is_multiple_of(3));
        path.vertices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|triangle| {
                let [a, b, c] = triangle.each_ref().map(|corner| corner.xy_position);
                let cross = f32::from(b.x - a.x) * f32::from(c.y - a.y)
                    - f32::from(c.x - a.x) * f32::from(b.y - a.y);
                f64::from(cross.abs()) / 2.0
            })
            .sum()
    }

    /// Asserts that `paths` draw the shapes `expected` draws, whatever their fills. The
    /// tessellator can split an area into other triangles once its points move by a rounding
    /// error, so areas compare by where they stand and what they cover, and strokes, which it
    /// builds point by point, by each vertex.
    fn assert_same_shapes(paths: &CurvePaths, expected: &CurvePaths) {
        let (Some(stroke), Some(expected_stroke)) = (&paths.stroke, &expected.stroke) else {
            panic!("both curves have a stroke");
        };
        assert_same_bounds(stroke, expected_stroke);
        assert_eq!(stroke.vertices.len(), expected_stroke.vertices.len());
        for (vertex, expected) in stroke.vertices.iter().zip(&expected_stroke.vertices) {
            assert_near(vertex.xy_position, expected.xy_position);
        }

        let (Some((area, _)), Some((expected_area, _))) = (&paths.area, &expected.area) else {
            panic!("both curves have an area");
        };
        assert_same_bounds(area, expected_area);
        let (covered, expected_covered) = (covered_area(area), covered_area(expected_area));
        assert!(
            (covered - expected_covered).abs() < 0.5,
            "{covered} {expected_covered}"
        );
    }

    fn area_fill(paths: &CurvePaths) -> Background {
        let Some((_, fill)) = paths.area else {
            panic!("the curve has an area");
        };
        fill
    }

    #[test]
    fn a_curve_whose_window_slid_past_still_buckets_is_moved_rather_than_built_again() {
        // Goal: a plot painted again after its window slid a second, with no bucket closed or
        // gone, draws its curve as a new build would without tessellating the curve again.
        // Method: paint a following chart from 1001 and then from 1002 through one held set of
        // curves, both with buckets from 1010 to 1900, and compare the second paint with a new
        // build from 1002.
        let builds = Rc::new(Cell::new(0));
        let held = HeldCurves::new(Rc::clone(&builds));
        led_paths(1001, plot_at(0.0, 0.0), &held);

        let moved = led_paths(1002, plot_at(0.0, 0.0), &held);

        let built = built_led_paths(1002, plot_at(0.0, 0.0));
        assert_eq!(builds.get(), 1);
        assert_same_shapes(&moved, &built);
        assert_eq!(area_fill(&moved), area_fill(&built));
    }

    #[test]
    fn a_curve_on_a_plot_laid_out_elsewhere_is_moved_with_its_plot() {
        // Goal: a plot the page lays out at another place, as a scroll does, draws its curve
        // there as a new build would, without tessellating the curve again. The fill fades by
        // the height within the plot, so a move leaves it as it was. Method: paint a following
        // chart from 1001 on a plot at the page's corner and then on one 20 pixels right and 30
        // down through one held set of curves, and compare the second paint with a new build.
        let builds = Rc::new(Cell::new(0));
        let held = HeldCurves::new(Rc::clone(&builds));
        led_paths(1001, plot_at(0.0, 0.0), &held);

        let moved = led_paths(1001, plot_at(20.0, 30.0), &held);

        assert_eq!(builds.get(), 1);
        assert_same_shapes(&moved, &built_led_paths(1001, plot_at(20.0, 30.0)));
    }

    #[test]
    fn a_curve_whose_newest_bucket_closed_is_built_again() {
        // Goal: a curve that gained a point is tessellated again, so no paint draws the curve it
        // had. Method: paint a following chart from 1000, whose newest whole bucket ends at
        // 1890, and then from 1001, whose newest ends at 1900, through one held set of curves,
        // and compare the second paint with a new build from 1001.
        let builds = Rc::new(Cell::new(0));
        let held = HeldCurves::new(Rc::clone(&builds));
        led_paths(1000, plot_at(0.0, 0.0), &held);

        let painted = led_paths(1001, plot_at(0.0, 0.0), &held);

        assert_eq!(builds.get(), 2);
        assert_same_shapes(&painted, &built_led_paths(1001, plot_at(0.0, 0.0)));
    }

    #[test]
    fn a_curve_on_a_taller_plot_is_built_again() {
        // Goal: a plot that changed its size draws its curve to the new size. Method: paint a
        // following chart from 1001 on a plot 100 pixels tall and then on one 150 tall through
        // one held set of curves, and count the curves built.
        let builds = Rc::new(Cell::new(0));
        let held = HeldCurves::new(Rc::clone(&builds));
        led_paths(1001, plot_at(0.0, 0.0), &held);
        let mut taller = plot_at(0.0, 0.0);
        taller.size.height = px(150.0);

        led_paths(1001, taller, &held);

        assert_eq!(builds.get(), 2);
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

        let drawn: Vec<_> = smooth_stretches(&forward).collect();
        let reversed: Vec<_> = smooth_stretches(&backward).collect();

        assert_eq!(
            drawn,
            vec![
                Stretch::Bend([at(40.0, 20.0), at(25.0, 80.0), at(25.0, 20.0)]),
                Stretch::Bend([at(70.0, 50.0), at(55.0, 20.0), at(55.0, 50.0)]),
            ]
        );
        assert_eq!(
            reversed,
            vec![
                Stretch::Bend([at(40.0, 20.0), at(55.0, 50.0), at(55.0, 20.0)]),
                Stretch::Bend([at(10.0, 80.0), at(25.0, 20.0), at(25.0, 80.0)]),
            ]
        );
    }

    #[test]
    fn a_stretch_of_curve_two_pixels_wide_or_narrower_runs_straight() {
        // Goal: where a chart's points sit two pixels apart or closer, the halfway bend lies
        // under a third of a pixel from a straight line, so the stretch is drawn straight, which
        // costs the tessellator a fifth of the triangles, and a wider stretch still bends.
        // Method: read the stretches through points one, two and two and a half pixels apart,
        // forwards and backwards.
        let at = |x: f32, y: f32| point(px(x), px(y));
        let forward = [
            at(10.0, 80.0),
            at(11.0, 20.0),
            at(13.0, 50.0),
            at(15.5, 30.0),
        ];
        let mut backward = forward;
        backward.reverse();

        let drawn: Vec<_> = smooth_stretches(&forward).collect();
        let reversed: Vec<_> = smooth_stretches(&backward).collect();

        assert_eq!(
            drawn,
            vec![
                Stretch::Straight(at(11.0, 20.0)),
                Stretch::Straight(at(13.0, 50.0)),
                Stretch::Bend([at(15.5, 30.0), at(14.25, 50.0), at(14.25, 30.0)]),
            ]
        );
        assert_eq!(
            reversed,
            vec![
                Stretch::Bend([at(13.0, 50.0), at(14.25, 30.0), at(14.25, 50.0)]),
                Stretch::Straight(at(11.0, 20.0)),
                Stretch::Straight(at(10.0, 80.0)),
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
    fn half_hour_ends(curves: &[Curve]) -> Vec<(f32, f32)> {
        let seconds = |point: &RunPoint| 1000.0 + f32::from(point.top.x) * 2.0;
        curves
            .iter()
            .map(|curve| curve.points.as_slice())
            .map(|points| (seconds(&points[0]), seconds(&points[points.len() - 1])))
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

    /// The curves a following 30-minute window from 1000 in ten-second buckets draws through a
    /// reading each second, with full detail from `join`.
    fn sliding_half_hour_curves(join: u64) -> Vec<Curve> {
        let positions = tick_positions(1800);
        let mut geometry = live_geometry(1000, 0, &positions);
        geometry.sliding = true;
        let window = TimeWindow {
            first: 1000,
            span: 1800,
        };
        geometry.buckets = Some(Buckets::of(
            window,
            Some(join),
            TICK_SECONDS,
            TEN_SECOND_CARD * 2.0,
        ));
        let values: Vec<_> = (1000..2800)
            .map(|second| Some((varying_reading(second), 0.0)))
            .collect();

        run_curves(&values, 0..1800, &geometry)
    }

    #[test]
    fn no_window_across_the_join_is_drawn_while_the_window_slides() {
        // Goal: a point whose window holds readings the moving join will still thin or cut is
        // not drawn, so the curve beside the join does not change from second to second.
        // Method: in a following 30-minute window with the join at 1905, read the points either
        // side of the bucket the join cuts. The first window after it starts at the bucket after
        // the cut one, so its point ends the window's four buckets later, at 1950.
        let curves = sliding_half_hour_curves(1905);

        let ends = half_hour_ends(&curves);
        assert_eq!(ends, vec![(1050.0, 1900.0), (1950.0, 2790.0)]);
    }

    #[test]
    fn no_window_across_a_join_on_a_bucket_start_is_drawn_while_the_window_slides() {
        // Goal: a join on the first second of a bucket cuts no bucket, yet the curve breaks
        // there as it does at a join that cuts one.
        // Method: in a following 30-minute window with the join at 1900, a bucket's start, read
        // the points either side of the join. The first window after it starts at the join's
        // bucket, so its point ends the window's four buckets later, at 1940.
        let curves = sliding_half_hour_curves(1900);

        let ends = half_hour_ends(&curves);
        assert_eq!(ends, vec![(1050.0, 1900.0), (1940.0, 2790.0)]);
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
