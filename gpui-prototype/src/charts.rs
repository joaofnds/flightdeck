use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    Bounds, IntoElement, PathBuilder, Pixels, Window, canvas, fill, point, prelude::*, px, rgb,
    size,
};

use crate::data::Panel;

pub const BLUE: u32 = 0x0057_94f2;
pub const GREEN: u32 = 0x0073_bf69;
pub const GRID: u32 = 0x0028_2a2e;
pub const MUTED: u32 = 0x0089_8995;
pub const ORANGE: u32 = 0x00ff_9830;
pub const PANEL: u32 = 0x0018_1b1f;
pub const PURPLE: u32 = 0x00b8_77d9;
pub const RED: u32 = 0x00f2_495c;
pub const TEXT: u32 = 0x00cc_ccdc;
pub const YELLOW: u32 = 0x00f2_cc0c;
pub const PLOT_INSETS: gpui::Edges<Pixels> = gpui::Edges {
    top: px(8.0),
    right: px(10.0),
    bottom: px(20.0),
    left: px(40.0),
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
    plot_cursor(panel, inspection, hidden, measured, true)
}

pub fn plot_without_cursor(
    panel: Rc<Panel>,
    inspection: Inspection,
    hidden: Rc<Vec<String>>,
    measured: Rc<Cell<Bounds<Pixels>>>,
) -> impl IntoElement {
    plot_cursor(panel, inspection, hidden, measured, false)
}

fn plot_cursor(
    panel: Rc<Panel>,
    inspection: Inspection,
    hidden: Rc<Vec<String>>,
    measured: Rc<Cell<Bounds<Pixels>>>,
    show_cursor: bool,
) -> impl IntoElement {
    canvas(
        move |bounds, _, _| measured.set(bounds),
        move |bounds, (), window, _| {
            let chart = plot_bounds(bounds);
            crate::render_evidence::chart(&panel, inspection, chart, window);
            paint_grid(chart, window);
            clip_plot(chart, window, |window| match panel.kind.as_str() {
                "timeline" => paint_timeline(&panel, chart, inspection, &hidden, window),
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

fn paint_grid(bounds: Bounds<Pixels>, window: &mut Window) {
    for row in 0..=4 {
        window.paint_quad(fill(
            Bounds::new(
                point(
                    bounds.origin.x,
                    bounds.origin.y + bounds.size.height * (number(row) / 4.0),
                ),
                size(bounds.size.width, px(1.0)),
            ),
            rgb(GRID),
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
    let (minimum, maximum) = domain(panel, inspection, hidden);
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
        let (low, high) = if let Some(axis) = &line.axis {
            axis_domain(panel, inspection, hidden, axis)
        } else {
            (minimum, maximum)
        };
        let geometry = Geometry {
            bounds,
            minimum: low,
            maximum: high,
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
    if range.len() == 1 {
        if let Some((value, _)) = values[range.start] {
            let position = geometry.point(range.start, value);
            window.paint_quad(fill(
                Bounds::new(position - point(px(1.5), px(1.5)), size(px(3.0), px(3.0))),
                rgb(color(&line.color)),
            ));
        }
        return;
    }
    let mut stroke = PathBuilder::stroke(px(1.5));
    let mut area = PathBuilder::fill();
    for index in range.clone() {
        if let Some((value, _)) = values[index] {
            let position = geometry.point(index, value);
            if index == range.start {
                stroke.move_to(position);
                area.move_to(position);
            } else {
                if line.dashed == Some(true) {
                    if let Some((previous, _)) = values[index - 1] {
                        dashed_segments(
                            geometry.point(index - 1, previous),
                            position,
                            |start, end| {
                                stroke.move_to(start);
                                stroke.line_to(end);
                            },
                        );
                    }
                } else {
                    stroke.line_to(position);
                }
                area.line_to(position);
            }
        }
    }
    for index in range.rev() {
        if let Some((_, bottom)) = values[index] {
            area.line_to(geometry.point(index, bottom));
        }
    }
    area.close();
    let opacity = line.fill.or(panel.fill).unwrap_or(0.10);
    paint_path(area, alpha(color(&line.color), opacity), window);
    paint_path(stroke, rgb(color(&line.color)), window);
}

fn dashed_segments(
    start: gpui::Point<Pixels>,
    end: gpui::Point<Pixels>,
    mut paint: impl FnMut(gpui::Point<Pixels>, gpui::Point<Pixels>),
) {
    let delta = end - start;
    let distance = f32::from(delta.x).hypot(f32::from(delta.y));
    if distance <= 0.0 {
        return;
    }
    if !distance.is_finite() {
        eprintln!("Nonfinite chart segment");
        return;
    }
    if distance > 32_768.0 {
        paint(start, end);
        return;
    }
    for index in 0..4096 {
        let offset = number(index) * 8.0;
        if offset >= distance {
            return;
        }
        paint(
            start + delta * (offset / distance),
            start + delta * ((offset + 5.0).min(distance) / distance),
        );
    }
}

#[must_use]
pub fn domain(panel: &Panel, inspection: Inspection, hidden: &[String]) -> (f32, f32) {
    axis_domain(panel, inspection, hidden, "left")
}

#[must_use]
pub fn axis_domain(
    panel: &Panel,
    inspection: Inspection,
    hidden: &[String],
    axis: &str,
) -> (f32, f32) {
    let mut minimum = if axis == "left" {
        panel.minimum.unwrap_or(0.0)
    } else {
        0.0
    };
    let mut maximum = 1.0_f32;
    for index in inspection.lower..=inspection.upper {
        let mut sum = 0.0;
        for line in &panel.lines {
            if hidden.contains(&line.name) {
                continue;
            }
            if line.axis.as_deref().unwrap_or("left") != axis {
                continue;
            }
            if let Some(value) = line.values.get(index).copied().flatten() {
                let value = if line.negative == Some(true) {
                    -value
                } else {
                    value
                };
                if stacked(panel, line) {
                    sum += value;
                    maximum = maximum.max(sum);
                } else {
                    maximum = maximum.max(value);
                }
                minimum = minimum.min(value);
            }
        }
    }
    maximum *= 1.08;
    if axis == "left" {
        maximum = panel.maximum.unwrap_or(maximum);
    }
    (minimum, maximum.max(minimum + 0.001))
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
    window: &mut Window,
) {
    if let Some(line) = panel.lines.first() {
        if hidden.contains(&line.name) {
            return;
        }
        let count = view.upper - view.lower + 1;
        for index in view.lower..=view.upper {
            let value = line.values.get(index).copied().flatten();
            let Some(value) = value else {
                continue;
            };

            let tint = timeline_color(panel, value);
            window.paint_quad(fill(
                Bounds::new(
                    point(
                        bounds.origin.x
                            + bounds.size.width * (number(index - view.lower) / number(count)),
                        bounds.origin.y + px(35.0),
                    ),
                    size(bounds.size.width / number(count), px(28.0)),
                ),
                alpha(tint, 0.7),
            ));
        }
    }
}

pub(crate) fn timeline_color(panel: &Panel, value: f32) -> u32 {
    if panel.id == "live-battery-charging" {
        return match value {
            0.0 => MUTED,
            1.0 => YELLOW,
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

fn paint_path(builder: PathBuilder, color: gpui::Rgba, window: &mut Window) {
    match builder.build() {
        Ok(path) => window.paint_path(path, color),
        Err(error) => eprintln!("chart path: {error}"),
    }
}

pub fn clip_plot(bounds: Bounds<Pixels>, window: &mut Window, paint: impl FnOnce(&mut Window)) {
    window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| paint(window));
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
            "wall_ns": 1_000_000_000_000_u64, "network_rx_bps": 2048,
            "network_tx_bps": 1024, "power_watts": 8.0, "gpu_watts": 2.0}]});
        let inspection = Inspection {
            lower: 0,
            selected: 0,
            upper: 0,
        };

        let network = crate::live_projection::panels("network", &selection, None).unwrap();
        let (minimum, maximum) = domain(&network[0], inspection, &[]);
        assert!((minimum + 1.0).abs() < f32::EPSILON);
        assert!((maximum - 2.16).abs() < 0.000_001);
        let energy = crate::live_projection::panels("energy", &selection, None).unwrap();
        let (minimum, maximum) = domain(&energy[0], inspection, &[]);
        assert!(minimum.abs() < f32::EPSILON);
        assert!((maximum - 8.64).abs() < 0.000_001);
    }

    #[test]
    fn a_two_point_dashed_run_keeps_measured_geometry() {
        // A short measured interval must paint, even before a third sample arrives.
        let mut strokes = Vec::new();
        dashed_segments(
            point(px(0.0), px(0.0)),
            point(px(2.0), px(0.0)),
            |start, end| strokes.push((start, end)),
        );
        assert_eq!(strokes.len(), 1);
        assert_eq!(strokes[0].0, point(px(0.0), px(0.0)));
        assert_eq!(strokes[0].1, point(px(2.0), px(0.0)));
    }
}
