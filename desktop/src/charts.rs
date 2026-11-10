use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{
    Background, Bounds, IntoElement, Path, PathBuilder, Pixels, Point, Window, canvas, fill,
    linear_color_stop, linear_gradient, point, prelude::*, px, rgb, size,
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
            clock: panel_clock(panel),
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

/// The clock of a panel whose readings stand on one, or `None` for a chart of readings in order.
fn panel_clock(panel: &Panel) -> Option<ReadingClock> {
    panel
        .window
        .map(|window| ReadingClock::of(window, panel.detail_first, panel.reading_seconds))
}

pub(crate) struct Geometry<'a> {
    pub(crate) bounds: Bounds<Pixels>,
    pub(crate) minimum: f32,
    pub(crate) maximum: f32,
    pub(crate) count: usize,
    pub(crate) positions: Option<&'a [f32]>,
    pub(crate) segments: Option<&'a [u64]>,
    pub(crate) lower: usize,
    /// When each reading was taken, or `None` for a chart that draws its readings evenly apart.
    pub(crate) clock: Option<ReadingClock>,
}

impl Geometry<'_> {
    /// Where the chart's readings fall on the clock, or `None` for a chart with no clock.
    fn clock(&self) -> Option<ClockedReadings<'_>> {
        Some(ClockedReadings {
            clock: self.clock?,
            positions: self.positions?,
            lower: self.lower,
        })
    }

    /// Where a curve's point for the reading taken in `second` sits at height `value`.
    fn clock_point(&self, clock: ReadingClock, second: u64, value: f32) -> gpui::Point<Pixels> {
        point(
            self.bounds.origin.x + self.bounds.size.width * clock.fraction(clock.end(second)),
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
    // With a clock, the curves of a run break where a stretch of the clock holds no reading,
    // which `run_curves` finds, so only a change of segment ends a run. Without one, a missing
    // reading breaks the line.
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

/// One unbroken curve, as its points in time order and, on a chart with a clock, the reading
/// each point stands for.
#[derive(Debug)]
struct Curve {
    readings: Vec<CurveReading>,
    points: Vec<RunPoint>,
}

/// The unbroken curves through one run of readings, a point for each reading. A chart with a
/// clock draws each at the end of the stretch it covers, and breaks the curve where the next
/// reading starts past that stretch, after a missed tick. A chart with no clock draws its
/// readings evenly apart.
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
            readings: Vec::new(),
            points,
        }];
    };

    curve_readings(values, run, clock)
        .chunk_by(|left, right| clock.clock.joins(left.second, right.second))
        .map(|readings| Curve {
            points: readings
                .iter()
                .map(|reading| RunPoint {
                    top: geometry.clock_point(clock.clock, reading.second, reading.top),
                    bottom: geometry.clock_point(clock.clock, reading.second, reading.bottom),
                })
                .collect(),
            readings: readings.to_vec(),
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
    let Curve { readings, points } = curve;
    let Some(frame) = frame else {
        return curve_paths(look, &points, geometry.bounds);
    };
    // The points of a chart with no clock stand at no second, so no move finds them again.
    let Some(clock) = geometry.clock() else {
        return curve_paths(look, &points, geometry.bounds);
    };
    assert_eq!(readings.len(), points.len());

    let window = clock.clock.window;
    let shape = CurveShape {
        readings,
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
/// window of a following chart slides a second at a time while its points stay at their seconds,
/// so a paint that finds a curve with the shape it had, standing a second's width further left,
/// moves the paths held for it rather than tessellate them again.
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
/// the readings its points stand for, how it is drawn, its scale, the plot's size and the
/// seconds its window spans. Two curves of one shape differ only by a move.
#[derive(PartialEq)]
struct CurveShape {
    readings: Vec<CurveReading>,
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

/// The seconds between two ticks, a full-detail reading apart. Thinned older readings stand one
/// for each `viewer::EARLIER_STRIDE_SECONDS`.
pub(crate) const TICK_SECONDS: u64 = 1;
/// How far before the start of its second a reading's position may fall and still count in that
/// second. A position is an f32 fraction of the window, whose rounding can put a reading taken
/// on a whole second, as the self-monitoring reports are, a few microseconds before it, in some
/// windows and not others, so it would hop between seconds as the window slides.
const SECOND_TOLERANCE: f64 = 0.001;
const _: () = assert!(TICK_SECONDS < viewer::EARLIER_STRIDE_SECONDS);

/// When a chart's readings were taken. Each reading is a point at the end of the stretch of the
/// clock it covers, since only then is it whole. A point keeps its second as the window slides,
/// so it is drawn once and then only moves left.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ReadingClock {
    window: TimeWindow,
    spacing: Spacing,
    /// The first second of full-detail readings, before which readings are thinned.
    detail_first: Option<u64>,
}

/// How far apart a chart's full-detail readings stand, and which second of what it covers each
/// names.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Spacing {
    /// A reading of each tick, which names the second it was taken in and covers it.
    Ticks,
    /// Reports the collector writes every `seconds`, each naming the second this app read it,
    /// the end of the seconds it covers.
    Reports { seconds: u64 },
}

impl ReadingClock {
    fn of(window: TimeWindow, detail_first: Option<u64>, reading_seconds: u64) -> Self {
        assert!(window.span > 0);
        assert!(reading_seconds >= TICK_SECONDS);
        let spacing = if reading_seconds == TICK_SECONDS {
            Spacing::Ticks
        } else {
            assert!(detail_first.is_none(), "reports are never thinned");
            Spacing::Reports {
                seconds: reading_seconds,
            }
        };
        Self {
            window,
            spacing,
            detail_first,
        }
    }

    /// The seconds the reading taken in `second` covers.
    fn stride(&self, second: u64) -> u64 {
        match self.spacing {
            Spacing::Reports { seconds } => seconds,
            Spacing::Ticks if self.detail_first.is_some_and(|join| second < join) => {
                viewer::EARLIER_STRIDE_SECONDS
            }
            Spacing::Ticks => TICK_SECONDS,
        }
    }

    /// Where the point of the reading taken in `second` stands on the clock: the end of the
    /// stretch it covers, or the first full-detail second where a thinned reading's stretch
    /// reaches past it, so the curve never runs back in time.
    fn end(&self, second: u64) -> u64 {
        if let Spacing::Reports { .. } = self.spacing {
            return second;
        }

        let end = second + self.stride(second);
        match self.detail_first {
            Some(join) if second < join => end.min(join),
            _ => end,
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

    /// Whether a curve runs on from the reading taken in `left` to the next, taken in `right`:
    /// the next starts within the stretch `left` covers, so no tick between them was missed.
    fn joins(&self, left: u64, right: u64) -> bool {
        assert!(left <= right);
        right - left <= self.stride(left)
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

/// Where a chart's readings fall on the clock.
#[derive(Clone, Copy)]
struct ClockedReadings<'a> {
    clock: ReadingClock,
    /// Where each of the panel's readings sits across its window.
    positions: &'a [f32],
    /// The index in `positions` of the first reading in view.
    lower: usize,
}

/// One point of a curve: a line's reading taken in `second`, at its top and at its base.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CurveReading {
    second: u64,
    top: f32,
    bottom: f32,
}

/// The readings of one segment's run, `run`, in time order, each with the second it was taken
/// in. A reading taken before the clock's zero has no point.
fn curve_readings(
    values: &[Option<(f32, f32)>],
    run: std::ops::Range<usize>,
    clock: ClockedReadings<'_>,
) -> Vec<CurveReading> {
    run.filter_map(|index| {
        let (top, bottom) = values[index]?;
        let second = clock
            .clock
            .second_of(clock.positions[clock.lower + index])?;
        Some(CurveReading {
            second,
            top,
            bottom,
        })
    })
    .collect()
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

/// The range of the readings the lines on `axis` draw, each of which is a point.
fn axis_extent(panel: &Panel, inspection: Inspection, hidden: &[String], axis: &str) -> (f32, f32) {
    let floor = if axis == "left" {
        panel.minimum.unwrap_or(0.0).min(0.0)
    } else {
        0.0
    };
    let mut extent = (floor, 0.0_f32);
    stacked_lines(panel, inspection, hidden, |line, values| {
        if line.axis.as_deref().unwrap_or("left") != axis {
            return;
        }
        for (top, bottom) in values.iter().flatten() {
            extent = (
                extent.0.min(*top).min(*bottom),
                extent.1.max(*top).max(*bottom),
            );
        }
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

pub(crate) fn stacked(panel: &Panel, line: &crate::data::Line) -> bool {
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
#[path = "charts_live_edge_tests.rs"]
mod live_edge_tests;

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
        // read at 1020.
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
            clock: Some(ReadingClock::of(
                TimeWindow { first, span: 900 },
                None,
                TICK_SECONDS,
            )),
        }
    }

    /// The geometry of `live_geometry` over the `span` seconds from `first`, whose full-detail
    /// readings start at `detail_first`.
    fn spanned_geometry(
        first: u64,
        span: u16,
        detail_first: Option<u64>,
        positions: &[f32],
    ) -> Geometry<'_> {
        let mut geometry = live_geometry(first, 0, positions);
        let window = TimeWindow {
            first,
            span: u64::from(span),
        };
        geometry.clock = Some(ReadingClock::of(window, detail_first, TICK_SECONDS));
        geometry
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

    /// A reading that turns from second to second.
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

    /// Each point as its place across the chart, its height, and the height of its base.
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

    #[test]
    fn a_point_stands_at_the_end_of_its_readings_second_over_its_base() {
        // Goal: each reading is a point of its own at the end of the second it was taken in,
        // over its own base, with no mean of its neighbours.
        // Method: in a window from 1000, place readings of 20 over a base of 0 in seconds 1100
        // and 1101 and of 60 over a base of 10 in 1102 and 1103, and read the points on a chart
        // one pixel a second and one pixel a unit.
        let positions = live_positions();
        let mut values = vec![None; 900];
        for value in &mut values[100..102] {
            *value = Some((20.0, 0.0));
        }
        for value in &mut values[102..104] {
            *value = Some((60.0, 10.0));
        }

        let points = one_curve(&values, 0..900, &live_geometry(1000, 0, &positions));

        assert_places(
            &points,
            &[
                [101.0, 80.0, 100.0],
                [102.0, 80.0, 100.0],
                [103.0, 40.0, 90.0],
                [104.0, 40.0, 90.0],
            ],
        );
    }

    #[test]
    fn a_curve_breaks_where_a_tick_was_missed() {
        // Goal: a missed tick leaves a second with no reading, which breaks the line, and a
        // reading each second keeps it whole.
        // Method: in a window from 1000, place readings in 1100 to 1139 and in the forty seconds
        // after a gap of none or one second, and count the curves each draws.
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

        assert_eq!([curves(0), curves(1)], [1, 2]);
    }

    /// A one-line chart of a varying reading in each second from 1002 to `newest`, viewed in the
    /// 900 seconds from `first`, from 0 to 100.
    fn steady_panel(first: u64, newest: u64) -> (Panel, Inspection) {
        let seconds = 1002..=newest;
        let positions: Vec<f32> = seconds
            .clone()
            .map(|second| (f32::from(u16::try_from(second - first).unwrap()) + 0.3) / 900.0)
            .collect();
        let values: Vec<Option<f32>> = seconds
            .map(|second| Some(varying_reading(second)))
            .collect();
        let newest_index = values.len() - 1;
        let mut panel: Panel = serde_json::from_value(serde_json::json!({
            "id":"cpu", "title":"CPU", "unit":"%", "kind":"line",
            "positions": positions, "lines":[
                {"name":"All cores", "color":"#5794f2", "values": values}]}))
        .unwrap();
        panel.window = Some(TimeWindow { first, span: 900 });
        panel.maximum = Some(100.0);
        let inspection = Inspection {
            lower: 0,
            selected: newest_index,
            upper: newest_index,
        };
        (panel, inspection)
    }

    /// A plot 900 pixels wide and 100 tall whose top left corner stands at `x` and `y`.
    fn plot_at(x: f32, y: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(900.0), px(100.0)))
    }

    /// The paths of the one curve `steady_panel(first, newest)` paints on `plot` through
    /// `held`.
    fn held_paths(first: u64, newest: u64, plot: Bounds<Pixels>, held: &HeldCurves) -> CurvePaths {
        let (panel, inspection) = steady_panel(first, newest);

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

    /// The paths a plot that holds no curves yet builds for `held_paths`.
    fn built_paths(first: u64, newest: u64, plot: Bounds<Pixels>) -> CurvePaths {
        held_paths(first, newest, plot, &HeldCurves::new(Rc::new(Cell::new(0))))
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
    fn a_curve_whose_window_slid_with_no_new_reading_is_moved_rather_than_built_again() {
        // Goal: a plot painted again after its window slid a second with no reading arriving or
        // leaving draws its curve as a new build would, without tessellating it again.
        // Method: paint the readings of 1002 to 1900 in the window from 1001 and then from 1002
        // through one held set of curves, and compare the second paint with a new build.
        let builds = Rc::new(Cell::new(0));
        let held = HeldCurves::new(Rc::clone(&builds));
        held_paths(1001, 1900, plot_at(0.0, 0.0), &held);

        let moved = held_paths(1002, 1900, plot_at(0.0, 0.0), &held);

        let built = built_paths(1002, 1900, plot_at(0.0, 0.0));
        assert_eq!(builds.get(), 1);
        assert_same_shapes(&moved, &built);
        assert_eq!(area_fill(&moved), area_fill(&built));
    }

    #[test]
    fn a_curve_on_a_plot_laid_out_elsewhere_is_moved_with_its_plot() {
        // Goal: a plot the page lays out at another place, as a scroll does, draws its curve
        // there as a new build would, without tessellating the curve again. The fill fades by
        // the height within the plot, so a move leaves it as it was. Method: paint a chart on a
        // plot at the page's corner and then on one 20 pixels right and 30 down through one held
        // set of curves, and compare the second paint with a new build.
        let builds = Rc::new(Cell::new(0));
        let held = HeldCurves::new(Rc::clone(&builds));
        held_paths(1001, 1900, plot_at(0.0, 0.0), &held);

        let moved = held_paths(1001, 1900, plot_at(20.0, 30.0), &held);

        assert_eq!(builds.get(), 1);
        assert_same_shapes(&moved, &built_paths(1001, 1900, plot_at(20.0, 30.0)));
    }

    #[test]
    fn a_curve_that_gained_a_reading_is_built_again() {
        // Goal: a curve that gained a point is tessellated again, so no paint draws the curve it
        // had. Method: paint the readings of 1002 to 1900 in the window from 1001 and then those
        // to 1901 in the window from 1002 through one held set of curves, and compare the second
        // paint with a new build.
        let builds = Rc::new(Cell::new(0));
        let held = HeldCurves::new(Rc::clone(&builds));
        held_paths(1001, 1900, plot_at(0.0, 0.0), &held);

        let painted = held_paths(1002, 1901, plot_at(0.0, 0.0), &held);

        assert_eq!(builds.get(), 2);
        assert_same_shapes(&painted, &built_paths(1002, 1901, plot_at(0.0, 0.0)));
    }

    #[test]
    fn a_curve_on_a_taller_plot_is_built_again() {
        // Goal: a plot that changed its size draws its curve to the new size. Method: paint a
        // chart on a plot 100 pixels tall and then on one 150 tall through one held set of
        // curves, and count the curves built.
        let builds = Rc::new(Cell::new(0));
        let held = HeldCurves::new(Rc::clone(&builds));
        held_paths(1001, 1900, plot_at(0.0, 0.0), &held);
        let mut taller = plot_at(0.0, 0.0);
        taller.size.height = px(150.0);

        held_paths(1001, 1900, taller, &held);

        assert_eq!(builds.get(), 2);
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
    fn a_reading_on_a_whole_second_keeps_its_second_as_the_window_slides() {
        // Goal: a reading taken on or just after a whole second, whose f32 position rounds a
        // hair either side of it, stays in its own second in every window.
        // Method: place readings 0 and 10 microseconds into second 4980, as the projection does,
        // in each 900-second window that holds them, directly and through a zoom from an hour,
        // and collect the seconds they fall in.
        let mut seconds = std::collections::BTreeSet::new();
        for fraction in [0.0, 0.000_01] {
            for first in 4081..=4980 {
                let offset = f32::from(u16::try_from(4980 - first).unwrap());
                let window = TimeWindow { first, span: 900 };
                let clock = ReadingClock::of(window, None, TICK_SECONDS);
                seconds.insert(clock.second_of((offset + fraction) / 900.0));

                // The zoom from an hour, in the arithmetic the range zoom uses.
                let hour_position = (offset + 2700.0 + fraction) / 3600.0;
                let zoomed = (hour_position * 3600.0 - 2700.0) / 900.0;
                seconds.insert(clock.second_of(zoomed));
            }
        }

        assert_eq!(seconds, [Some(4980)].into_iter().collect());
    }

    /// The curves a 30-minute window from 1000 draws on the 900-pixel plot, two seconds a pixel,
    /// through a reading in each ten seconds before `join` and in each second after it, but none
    /// in the ten seconds from `missed`.
    fn half_hour_curves(join: u64, missed: Option<u64>) -> Vec<Curve> {
        let positions = tick_positions(1800);
        let geometry = spanned_geometry(1000, 1800, Some(join), &positions);
        let values: Vec<_> = (1000..2800_u64)
            .map(|second| {
                let held = second >= join || second % 10 == 0;
                let missing = missed.is_some_and(|from| (from..from + 10).contains(&second));
                (held && !missing).then(|| (varying_reading(second), 0.0))
            })
            .collect();

        run_curves(&values, 0..1800, &geometry)
    }

    /// The seconds each point of `curve` stands at on the 30-minute plot.
    fn half_hour_seconds(curve: &Curve) -> Vec<f32> {
        curve
            .points
            .iter()
            .map(|point| 1000.0 + f32::from(point.top.x) * 2.0)
            .collect()
    }

    #[test]
    fn a_curve_runs_on_from_thinned_readings_into_full_detail_and_never_back_in_time() {
        // Goal: a chart whose older part holds one reading in each ten seconds draws it as one
        // unbroken curve into the readings of each second, each older reading at the end of its
        // ten seconds, and the one whose ten seconds reach past the join at the join.
        // Method: in a 30-minute window from 1000 with the join at 1905, read where the points
        // of the readings of 1890, 1900 and 1905 stand, and check the points run forward.
        let curves = half_hour_curves(1905, None);

        assert_eq!(curves.len(), 1, "{curves:?}");
        let seconds = half_hour_seconds(&curves[0]);
        let at = |second: u64| {
            let index = curves[0]
                .readings
                .iter()
                .position(|reading| reading.second == second);
            index.map(|index| seconds[index])
        };
        assert_eq!(
            [at(1890), at(1900), at(1905)],
            [Some(1900.0), Some(1905.0), Some(1906.0)]
        );
        assert!(
            seconds.windows(2).all(|pair| pair[0] < pair[1]),
            "{seconds:?}"
        );
    }

    #[test]
    fn a_missed_older_reading_breaks_the_curve() {
        // Goal: an older reading the recording lacks breaks the line as a missed tick does.
        // Method: in the 30-minute window from 1000 with the join at 1905, leave out the reading
        // of the ten seconds from 1500, and count the curves.
        assert_eq!(half_hour_curves(1905, Some(1500)).len(), 2);
    }

    #[test]
    fn a_chart_scrolled_through_its_readings_stands_each_at_its_own_second() {
        // Goal: a chart whose first reading in view is not its first reading still stands each
        // reading at the second it was taken in.
        // Method: view the readings of a window from 1000 from the 107th, place three readings
        // in seconds 1107 to 1109, and read their points.
        let positions = live_positions();
        let values: Vec<_> = [10.0, 20.0, 30.0]
            .into_iter()
            .map(|reading| Some((reading, 0.0)))
            .collect();

        let points = one_curve(&values, 0..3, &live_geometry(1000, 107, &positions));

        assert_places(
            &points,
            &[
                [108.0, 90.0, 100.0],
                [109.0, 80.0, 100.0],
                [110.0, 70.0, 100.0],
            ],
        );
    }

    #[test]
    fn a_panel_with_no_clock_draws_each_reading_on_its_own() {
        // Goal: a chart that knows no seconds draws its readings as they are.
        // Method: draw three readings of a geometry with no clock.
        let positions = [0.0, 0.5, 1.0];
        let mut geometry = live_geometry(1000, 0, &positions);
        geometry.clock = None;
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

    #[test]
    fn ten_second_reports_draw_one_unbroken_curve_to_the_newest_report() {
        // Goal: a chart of reports the collector writes every ten ticks, as the Diagnostics
        // charts are, takes their spacing as the seconds each covers, so the curve does not
        // break into dots between them, and each report stands at the second this app read it,
        // the end of its ten seconds, so the newest report read this second is at the right
        // edge. Method: read a report every ten seconds into a 600-second window from 1000 that
        // ends at the newest, and read the curves and where the first and last points stand.
        let positions: Vec<f32> = (1..=60_u16)
            .map(|report| f32::from(report) * 10.0 / 600.0)
            .collect();
        let mut geometry = live_geometry(1000, 0, &positions);
        let window = TimeWindow {
            first: 1000,
            span: 600,
        };
        geometry.clock = Some(ReadingClock::of(window, None, 10));
        let values: Vec<_> = (0..60_u64)
            .map(|report| Some((varying_reading(report), 0.0)))
            .collect();

        let curves = run_curves(&values, 0..60, &geometry);

        assert_eq!(curves.len(), 1, "{curves:?}");
        let xs: Vec<f32> = curves[0]
            .points
            .iter()
            .map(|point| f32::from(point.top.x))
            .collect();
        assert_eq!(xs.len(), 60);
        assert!((xs[0] - 15.0).abs() < 0.01, "{xs:?}");
        assert!((xs[59] - 900.0).abs() < 0.01, "{xs:?}");
    }

    #[test]
    fn the_value_axis_spans_every_reading_in_view() {
        // Goal: each reading is a point, so the axis reaches a one-second spike.
        // Method: put one reading of 90 among readings of 10 and read the axis.
        let values: Vec<_> = (0..900)
            .map(|offset| Some(if offset == 500 { 90.0 } else { 10.0 }))
            .collect();

        let (_, maximum) = domain(&live_panel(1000, &values), LIVE_INSPECTION, &[]);

        assert!(maximum >= 90.0, "{maximum}");
    }
}
