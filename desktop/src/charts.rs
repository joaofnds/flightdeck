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
}

impl Geometry<'_> {
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
    let points = run_points(values, range, geometry);
    if points.len() == 1 {
        let position = points[0].top;
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
    stroke.move_to(points[0].top);
    for sample in &points[1..] {
        stroke.line_to(sample.top);
    }

    let default_opacity = if stacked { 0.45 } else { 0.12 };
    let opacity = line.fill.or(panel.fill).unwrap_or(default_opacity);
    if line.dashed != Some(true) {
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

/// Where one reading sits on the plot: the top of its line and the bottom of its area.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RunPoint {
    top: gpui::Point<Pixels>,
    bottom: gpui::Point<Pixels>,
}

/// Where each reading of one run sits on the plot, so the line passes through every reading as
/// the collector recorded it. A reading, once drawn, depends on no other reading.
fn run_points(
    values: &[Option<(f32, f32)>],
    range: std::ops::Range<usize>,
    geometry: &Geometry<'_>,
) -> Vec<RunPoint> {
    assert!(!range.is_empty());
    let count = range.len();
    let points: Vec<_> = range
        .filter_map(|index| {
            let (top, bottom) = values[index]?;
            Some(RunPoint {
                top: geometry.point(index, top),
                bottom: geometry.point(index, bottom),
            })
        })
        .collect();
    assert_eq!(points.len(), count, "a run holds only readings");
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

/// The range of the readings in view, so the axis holds every reading the lines draw.
fn axis_extent(panel: &Panel, inspection: Inspection, hidden: &[String], axis: &str) -> (f32, f32) {
    let floor = if axis == "left" {
        panel.minimum.unwrap_or(0.0).min(0.0)
    } else {
        0.0
    };
    let mut extent = (floor, 0.0_f32);
    for index in inspection.lower..=inspection.upper {
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

    /// The geometry of a 900-pixel chart of 900 seconds, one pixel a second, from 0 at its
    /// bottom to 100 at its top, with each reading 0.3 seconds into its second as a tick lands.
    fn live_geometry(positions: &[f32]) -> Geometry<'_> {
        Geometry {
            bounds: Bounds::new(point(px(0.0), px(0.0)), size(px(900.0), px(100.0))),
            minimum: 0.0,
            maximum: 100.0,
            count: positions.len(),
            positions: Some(positions),
            segments: None,
            lower: 0,
        }
    }

    fn live_positions() -> Vec<f32> {
        (0..900_u16)
            .map(|offset| (f32::from(offset) + 0.3) / 900.0)
            .collect()
    }

    /// A one-line CPU chart of `values` across 900 seconds.
    fn live_panel(values: &[Option<f32>]) -> Panel {
        serde_json::from_value(serde_json::json!({
            "id":"cpu", "title":"CPU", "unit":"%", "kind":"line",
            "positions": live_positions(), "lines":[
                {"name":"All cores", "color":"#5794f2", "values": values}]}))
        .unwrap()
    }

    const LIVE_INSPECTION: Inspection = Inspection {
        lower: 0,
        selected: 899,
        upper: 899,
    };

    fn places(points: &[RunPoint]) -> Vec<(f32, f32, f32)> {
        points
            .iter()
            .map(|sample| {
                (
                    f32::from(sample.top.x),
                    f32::from(sample.top.y),
                    f32::from(sample.bottom.y),
                )
            })
            .collect()
    }

    #[test]
    fn a_line_passes_through_every_reading_of_its_run() {
        // Goal: the line shows each reading as the collector recorded it, a one-second spike
        // included, and no average of its neighbours.
        // Method: place four readings with a spike, the third over a base of 5, and read where
        // each lands on a chart one pixel a second and one pixel a unit.
        let positions = live_positions();
        let mut values = vec![None; 900];
        values[100] = Some((10.0, 0.0));
        values[101] = Some((90.0, 0.0));
        values[102] = Some((15.0, 5.0));
        values[103] = Some((50.0, 0.0));

        let points = run_points(&values, 100..104, &live_geometry(&positions));

        let expected = [
            (100.3, 90.0, 100.0),
            (101.3, 10.0, 100.0),
            (102.3, 85.0, 95.0),
            (103.3, 50.0, 100.0),
        ];
        for (place, expected) in places(&points).iter().zip(expected) {
            assert!((place.0 - expected.0).abs() < 0.01, "{place:?}");
            assert!((place.1 - expected.1).abs() < 0.01, "{place:?}");
            assert!((place.2 - expected.2).abs() < 0.01, "{place:?}");
        }
        assert_eq!(points.len(), 4);
    }

    #[test]
    fn a_lone_reading_between_gaps_draws_as_one_point() {
        // Goal: a reading with gaps on both sides shows on its own at once.
        // Method: draw the one-reading run 450 seconds into a chart of gaps.
        let positions = live_positions();
        let values: Vec<_> = (0..900)
            .map(|offset| (offset == 450).then_some((40.0, 0.0)))
            .collect();

        let points = run_points(&values, 450..451, &live_geometry(&positions));

        assert_eq!(places(&points), vec![(450.3, 60.0, 100.0)]);
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
        // Goal: the axis holds every reading the line draws, so a one-second spike is not cut
        // off at the top of the chart.
        // Method: put one reading of 90 among readings of 10 and read the axis.
        let values: Vec<_> = (0..900)
            .map(|offset| Some(if offset == 500 { 90.0 } else { 10.0 }))
            .collect();

        let (_, maximum) = domain(&live_panel(&values), LIVE_INSPECTION, &[]);

        assert!(maximum >= 90.0, "{maximum}");
    }
}
