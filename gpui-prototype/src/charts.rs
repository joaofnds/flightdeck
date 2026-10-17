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

#[derive(Clone, Copy)]
pub struct Inspection {
    pub lower: usize,
    pub selected: usize,
    pub upper: usize,
}

impl Inspection {
    pub fn select_fraction(&self, fraction: f32) -> usize {
        assert!(self.upper < crate::data::SAMPLE_COUNT);
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
    canvas(
        move |bounds, _, _| measured.set(bounds),
        move |bounds, (), window, _| {
            let chart = plot_bounds(bounds);
            paint_grid(chart, window);
            clip_plot(chart, window, |window| match panel.kind.as_str() {
                "timeline" => paint_timeline(&panel, chart, inspection, &hidden, window),
                "heatmap" => paint_heatmap(&panel, chart, inspection, &hidden, window),
                "core-bars" | "pressure" => paint_bars(&panel, chart, inspection, &hidden, window),
                _ => paint_lines(&panel, chart, inspection, &hidden, window),
            });
            let fraction = number(inspection.selected - inspection.lower)
                / number((inspection.upper - inspection.lower).max(1));
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

pub fn plot_bounds(bounds: Bounds<Pixels>) -> Bounds<Pixels> {
    Bounds::new(
        point(bounds.origin.x + px(40.0), bounds.origin.y + px(8.0)),
        size(
            (bounds.size.width - px(50.0)).max(px(1.0)),
            (bounds.size.height - px(28.0)).max(px(1.0)),
        ),
    )
}

pub fn color(value: &str) -> u32 {
    match u32::from_str_radix(value.trim_start_matches('#'), 16) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("invalid captured color {value}: {error}");
            MUTED
        }
    }
}

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
    let (minimum, maximum) = domain(panel, inspection, hidden);
    let mut previous = [0.0; crate::data::SAMPLE_COUNT];
    for line in &panel.lines {
        if hidden.contains(&line.name) {
            continue;
        }
        let stacked = stacked(panel, line);
        let values: Vec<_> = (inspection.lower..=inspection.upper)
            .map(|index| {
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
            })
            .collect();
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
        };
        paint_line_runs(line, panel, &values, &geometry, window);
    }
}

struct Geometry {
    bounds: Bounds<Pixels>,
    minimum: f32,
    maximum: f32,
    count: usize,
}

impl Geometry {
    fn point(&self, index: usize, value: f32) -> gpui::Point<Pixels> {
        point(
            self.bounds.origin.x
                + self.bounds.size.width
                    * (number(index) / number(self.count.saturating_sub(1).max(1))),
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
    geometry: &Geometry,
    window: &mut Window,
) {
    let mut start = 0;
    for end in 0..=values.len() {
        if end < values.len() {
            if values[end].is_some() {
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
    geometry: &Geometry,
    window: &mut Window,
) {
    let mut stroke = PathBuilder::stroke(px(1.5));
    let mut area = PathBuilder::fill();
    for index in range.clone() {
        if let Some((value, _)) = values[index] {
            let position = geometry.point(index, value);
            if index == range.start {
                stroke.move_to(position);
                area.move_to(position);
            } else {
                if matches!((line.dashed, index % 4), (Some(true), 0 | 1)) {
                    stroke.move_to(position);
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

pub fn domain(panel: &Panel, inspection: Inspection, hidden: &[String]) -> (f32, f32) {
    axis_domain(panel, inspection, hidden, "left")
}

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
            let tint = match value {
                Some(0.0) => GREEN,
                Some(1.0) => ORANGE,
                Some(2.0 | 3.0) => RED,
                _ => MUTED,
            };
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

pub fn alpha(value: u32, opacity: f32) -> gpui::Rgba {
    let mut color = rgb(value);
    color.a = opacity;
    color
}
