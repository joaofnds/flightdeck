use std::path::PathBuf;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::time::Instant;

use gpui::{
    App, AvailableSpace, Bounds, Element, ElementId, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, PathBuilder, Pixels, Point, RenderImage, Style, TextRun, Window, div,
    fill, font, point, prelude::*, px, rgb, size,
};
use image::{Frame, RgbaImage};

use crate::charts::{BLUE, GREEN, GRID, MUTED, ORANGE, PANEL, PURPLE, RED, TEXT};
use crate::dashboard::Dashboard;
use crate::data::state;
use crate::hardware_layout::{CALLOUTS, Callout, Layout};
use crate::hardware_motion::{HardwareState, HardwareVisibility, Vector, perimeter};

pub(crate) const AIRFLOW_WASH_OPACITY: f32 = 0.11;

#[derive(Default, Clone, Debug, PartialEq)]
pub struct Readings {
    pub temperatures: [Option<f32>; 3],
    pub speeds: [Option<f32>; 2],
    pub utilization: [Option<f32>; 2],
    pub memory: [Option<f32>; 2],
    pub battery: [Option<f32>; 2],
    pub thermal: String,
}

impl Dashboard {
    pub fn hardware(&self) -> impl IntoElement {
        let selected = self.inspection.selected;
        let value = |key| self.capture.value(key, selected);
        let readings = Readings {
            temperatures: [
                value("temperatures0"),
                value("temperatures1"),
                value("temperatures3"),
            ],
            speeds: [
                self.capture.fan("0", selected),
                self.capture.fan("1", selected),
            ],
            utilization: [value("cpu0"), value("gpu0")],
            memory: [
                value("memoryUsed0").map(|value| value / 1_073_741_824.0),
                value("installed").map(|value| value / 1_073_741_824.0),
            ],
            battery: [value("battery0"), value("batteryPower0")],
            thermal: state(value("thermal0"), true).into(),
        };
        div()
            .p(px(16.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(rgb(GRID))
            .bg(rgb(PANEL))
            .child(format!("Thermal state · {}", readings.thermal))
            .child(hardware_stage(readings, Rc::clone(&self.hardware_state)))
            .child(hardware_caption())
    }
}

pub fn hardware_caption() -> impl IntoElement {
    div()
        .text_size(px(11.0))
        .text_color(rgb(MUTED))
        .child(concat!(
            "MacBook Pro 16-inch assembly illustration; bottom cover removed, hinge at top. ",
            "Airflow is illustrative; fan channel sides are unverified."
        ))
}

pub fn hardware_stage(readings: Readings, state: Rc<HardwareState>) -> HardwareStage {
    HardwareStage { readings, state }
}

pub struct HardwareStage {
    readings: Readings,
    state: Rc<HardwareState>,
}

impl IntoElement for HardwareStage {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for HardwareStage {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _global_element_id: Option<&GlobalElementId>,
        _inspector_element_id: Option<&InspectorElementId>,
        window: &mut Window,
        _context: &mut App,
    ) -> (LayoutId, ()) {
        let layout = window.request_measured_layout(Style::default(), |known, available, _, _| {
            let width = known
                .width
                .map_or_else(
                    || match available.width {
                        AvailableSpace::Definite(width) => f32::from(width),
                        AvailableSpace::MinContent | AvailableSpace::MaxContent => 850.0,
                    },
                    f32::from,
                )
                .max(1.0);
            size(px(width), px(Layout::new(width).height))
        });
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _global_element_id: Option<&GlobalElementId>,
        _inspector_element_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        (): &mut (),
        _window: &mut Window,
        _context: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _global_element_id: Option<&GlobalElementId>,
        _inspector_element_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        (): &mut (),
        (): &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let visibility = hardware_visibility(bounds, window);
        self.state.acknowledge_visibility(&visibility);
        if !visibility.diagram_visible {
            return;
        }

        let layout = Layout::new(f32::from(bounds.size.width).max(1.0));
        let animated = self
            .state
            .paint_animation(bounds, &layout, &self.readings, window);
        match artwork() {
            Ok(artwork) => {
                paint_assembly(bounds, &layout, &self.readings, artwork, animated, window);
            }
            Err(error) => paint_text(error, bounds.origin, 12.0, rgb(ORANGE).into(), window, cx),
        }
        paint_callouts(bounds, &layout, &self.readings, window, cx);
    }
}

fn hardware_visibility(bounds: Bounds<Pixels>, window: &Window) -> HardwareVisibility {
    let diagram = bounds.intersect(&window.content_mask().bounds);
    let layout = Layout::new(f32::from(bounds.size.width).max(1.0));
    let stage = bounds_for(
        bounds,
        layout.point(Vector::new(0.0, 0.0)),
        Vector::new(840.0 * layout.scale, 600.0 * layout.scale),
    );
    let clipped = stage.intersect(&window.content_mask().bounds);
    HardwareVisibility {
        clipped: [
            f32::from(clipped.size.width).max(0.0),
            f32::from(clipped.size.height).max(0.0),
        ],
        stage: [f32::from(stage.size.width), f32::from(stage.size.height)],
        diagram_visible: if diagram.size.width > px(0.0) {
            diagram.size.height > px(0.0)
        } else {
            false
        },
        window_active: window.is_window_active(),
    }
}

/// The chassis, and the rotor as the left fan shows it and mirrored as the right one does.
struct Artwork {
    chassis: Arc<RenderImage>,
    rotors: [Arc<RenderImage>; 2],
}

fn artwork() -> &'static Result<Artwork, String> {
    static ARTWORK: OnceLock<Result<Artwork, String>> = OnceLock::new();
    ARTWORK.get_or_init(artwork_load)
}

fn artwork_load() -> Result<Artwork, String> {
    let started = Instant::now();
    let chassis = image::open(artwork_path("schematic_chassis.png"))
        .map_err(|error| format!("Schematic chassis unavailable: {error}"))?
        .into_rgba8();
    let rotor = image::open(artwork_path("schematic_rotor.png"))
        .map_err(|error| format!("Schematic rotor unavailable: {error}"))?
        .into_rgba8();
    if chassis.dimensions() != (2520, 1800) {
        return Err("Schematic chassis dimensions changed".into());
    }
    if rotor.dimensions() != (312, 312) {
        return Err("Schematic rotor dimensions changed".into());
    }
    let rotor = image::imageops::resize(&rotor, 208, 208, image::imageops::FilterType::Lanczos3);
    let mirrored = image::imageops::flip_horizontal(&rotor);
    let rotor_bytes = rotor.as_raw().len() + mirrored.as_raw().len();
    let rotors = [render_image(rotor), render_image(mirrored)];
    eprintln!(
        "schematic artwork cache: {} ms; rotor CPU bytes={rotor_bytes}; orientations=2",
        started.elapsed().as_millis()
    );
    Ok(Artwork {
        chassis: render_image(chassis),
        rotors,
    })
}

fn artwork_path(name: &str) -> PathBuf {
    match std::env::current_exe() {
        Ok(executable) => {
            if let Some(parent) = executable.parent() {
                if parent.file_name().is_some_and(|name| name == "MacOS") {
                    let bundled = parent.join("../Resources").join(name);
                    if bundled.is_file() {
                        return bundled;
                    }
                    eprintln!(
                        "Schematic bundle resource unavailable: {}",
                        bundled.display()
                    );
                }
            }
        }
        Err(error) => eprintln!("Locating schematic executable: {error}"),
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("resources")
        .join(name)
}

fn render_image(mut buffer: RgbaImage) -> Arc<RenderImage> {
    for pixel in buffer.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    Arc::new(RenderImage::new(vec![Frame::new(buffer)]))
}

fn paint_assembly(
    bounds: Bounds<Pixels>,
    layout: &Layout,
    readings: &Readings,
    artwork: &Artwork,
    animated: bool,
    window: &mut Window,
) {
    let stage = bounds_for(
        bounds,
        layout.point(Vector::new(0.0, 0.0)),
        Vector::new(840.0 * layout.scale, 600.0 * layout.scale),
    );
    if let Err(error) = window.paint_image(
        stage,
        stage,
        px(0.0).into(),
        Arc::clone(&artwork.chassis),
        0,
        false,
    ) {
        eprintln!("Painting schematic chassis: {error}");
    }
    for (index, callout) in [Callout::Fan0, Callout::Fan1].into_iter().enumerate() {
        if !animated {
            paint_assembly_rotor(
                bounds,
                layout,
                callout,
                Arc::clone(&artwork.rotors[index]),
                window,
            );
        }
        if readings.speeds[index].is_some_and(|speed| speed > 0.0) {
            paint_airflow(
                bounds,
                layout,
                index == 1,
                readings.temperatures[index],
                window,
            );
        }
    }
}

fn paint_assembly_rotor(
    bounds: Bounds<Pixels>,
    layout: &Layout,
    callout: Callout,
    image: Arc<RenderImage>,
    window: &mut Window,
) {
    let center = callout.anchor();
    let rotor = bounds_for(
        bounds,
        layout.point(Vector::new(center.x - 52.0, center.y - 52.0)),
        Vector::new(104.0 * layout.scale, 104.0 * layout.scale),
    );

    if let Err(error) = window.paint_image(rotor, rotor, px(0.0).into(), image, 0, false) {
        eprintln!("Painting schematic rotor: {error}");
    }
}

fn bounds_for(bounds: Bounds<Pixels>, origin: Vector, extent: Vector) -> Bounds<Pixels> {
    Bounds::new(
        point(
            bounds.origin.x + px(origin.x),
            bounds.origin.y + px(origin.y),
        ),
        size(px(extent.x), px(extent.y)),
    )
}

fn paint_airflow(
    bounds: Bounds<Pixels>,
    layout: &Layout,
    mirrored: bool,
    temperature: Option<f32>,
    window: &mut Window,
) {
    let tint = airflow_color(temperature);
    let mut boundary = PathBuilder::fill();
    boundary.style =
        gpui::PathStyle::Fill(gpui::FillOptions::default().with_fill_rule(gpui::FillRule::NonZero));
    let mut housing = perimeter().to_vec();
    housing.push(Vector::new(109.0, 83.0));
    airflow_contour(&mut boundary, &housing, bounds, layout, mirrored);
    airflow_contour(
        &mut boundary,
        &[
            Vector::new(112.0, 33.0),
            Vector::new(289.0, 33.0),
            Vector::new(289.0, 84.0),
            Vector::new(112.0, 84.0),
        ],
        bounds,
        layout,
        mirrored,
    );
    let hole: Vec<_> = (0..32_u16)
        .map(|step| {
            let angle = -f32::from(step) * std::f32::consts::TAU / 32.0;
            Vector::new(183.0 + angle.cos() * 29.0, 221.0 + angle.sin() * 29.0)
        })
        .collect();
    airflow_contour(&mut boundary, &hole, bounds, layout, mirrored);
    match boundary.build() {
        Ok(path) => paint_airflow_wash(&path, bounds, layout, tint, window),
        Err(error) => eprintln!("Painting airflow housing: {error}"),
    }
}

fn paint_airflow_wash(
    path: &gpui::Path<Pixels>,
    bounds: Bounds<Pixels>,
    layout: &Layout,
    tint: Hsla,
    window: &mut Window,
) {
    // Two clipped gradients reproduce four color stops using GPUI's two-stop primitive.
    for (first, end, stops) in [
        (33.0, 310.42, [(0.0, 0.0), (0.08, AIRFLOW_WASH_OPACITY)]),
        (310.42, 319.0, [(0.97, AIRFLOW_WASH_OPACITY), (1.0, 0.0)]),
    ] {
        let origin = layout.point(Vector::new(0.0, first));
        let extent = Vector::new(840.0 * layout.scale, (end - first) * layout.scale);
        let mask = gpui::ContentMask {
            bounds: bounds_for(bounds, origin, extent),
        };
        let background = gpui::linear_gradient(
            180.0,
            gpui::linear_color_stop(tint.opacity(stops[0].1), stops[0].0),
            gpui::linear_color_stop(tint.opacity(stops[1].1), stops[1].0),
        );
        window.with_content_mask(Some(mask), |window| {
            window.paint_path(path.clone(), background);
        });
    }
}

fn airflow_contour(
    path: &mut PathBuilder,
    points: &[Vector],
    bounds: Bounds<Pixels>,
    layout: &Layout,
    mirrored: bool,
) {
    assert!(points.len() <= 102);
    for (index, position) in points.iter().enumerate() {
        let position = airflow_point(*position, bounds, layout, mirrored);
        if index == 0 {
            path.move_to(position);
        } else {
            path.line_to(position);
        }
    }
    path.close();
}

fn airflow_point(
    mut position: Vector,
    bounds: Bounds<Pixels>,
    layout: &Layout,
    mirrored: bool,
) -> Point<Pixels> {
    if mirrored {
        position.x = 840.0 - position.x;
    }
    let position = layout.point(position);
    point(
        bounds.origin.x + px(position.x),
        bounds.origin.y + px(position.y),
    )
}

pub(crate) fn airflow_color(temperature: Option<f32>) -> Hsla {
    let Some(temperature) = temperature.filter(|value| value.is_finite()) else {
        return rgb(MUTED).into();
    };
    let cool = rgb(BLUE);
    let warm = rgb(0x00f2_cc0d);
    let hot = rgb(RED);
    let (first, second, fraction) = if temperature < 70.0 {
        (cool, warm, ((temperature - 50.0) / 20.0).clamp(0.0, 1.0))
    } else {
        (warm, hot, ((temperature - 70.0) / 20.0).clamp(0.0, 1.0))
    };
    gpui::Rgba {
        r: first.r + (second.r - first.r) * fraction,
        g: first.g + (second.g - first.g) * fraction,
        b: first.b + (second.b - first.b) * fraction,
        a: 1.0,
    }
    .into()
}

fn paint_callouts(
    bounds: Bounds<Pixels>,
    layout: &Layout,
    readings: &Readings,
    window: &mut Window,
    cx: &mut App,
) {
    for callout in CALLOUTS {
        let tint = callout_color(callout);
        let leader = layout.leader(callout);
        let mut path = PathBuilder::stroke(px(1.0));
        for (index, position) in leader.iter().enumerate() {
            let position = point(
                bounds.origin.x + px(position.x),
                bounds.origin.y + px(position.y),
            );
            if index == 0 {
                path.move_to(position);
            } else {
                path.line_to(position);
            }
        }
        match path.build() {
            Ok(path) => window.paint_path(path, tint.opacity(0.65)),
            Err(error) => eprintln!("Painting schematic leader: {error}"),
        }
        let anchor = bounds_for(
            bounds,
            Vector::new(leader[0].x - 2.0, leader[0].y - 2.0),
            Vector::new(4.0, 4.0),
        );
        window.paint_quad(fill(anchor, tint).corner_radii(px(2.0)));
        paint_label(bounds, layout, callout, readings, window, cx);
    }
}

pub(crate) fn callout_color(callout: Callout) -> Hsla {
    rgb(match callout {
        Callout::Gpu => PURPLE,
        Callout::Memory => GREEN,
        Callout::Battery => ORANGE,
        _ => BLUE,
    })
    .into()
}

/// How far apart the prototype sets the letters of a callout's name, in em.
const TITLE_TRACKING: f32 = 0.08;

fn paint_label(
    bounds: Bounds<Pixels>,
    layout: &Layout,
    callout: Callout,
    readings: &Readings,
    window: &mut Window,
    cx: &mut App,
) {
    let (primary, secondary) = label_values(callout, readings);
    let title = callout.title().to_owned();
    let origin = layout.label(callout);
    let at = |offset: f32, x: f32| {
        point(
            bounds.origin.x + px(origin.x + x),
            bounds.origin.y + px(origin.y + offset),
        )
    };
    let right = |width: Pixels| {
        if callout.left() {
            0.0
        } else {
            layout.label_width - f32::from(width)
        }
    };

    let tracked = tracked_width(&title, 11.0, window);
    paint_tracked(&title, at(0.0, right(tracked)), 11.0, window, cx);
    for (text, offset, size, line, color) in [
        (primary, 18.0, 22.0, 33.0, TEXT),
        (secondary, 51.0, 11.0, 18.0, MUTED),
    ] {
        let mut shaped = shape_text(&text, size, rgb(color).into(), window);
        if shaped.width > px(layout.label_width) {
            let factor = (layout.label_width / f32::from(shaped.width)).max(0.65);
            shaped = shape_text(&text, size * factor, rgb(color).into(), window);
        }
        if let Err(error) = shaped.paint(
            at(offset, right(shaped.width)),
            px(line),
            gpui::TextAlign::Left,
            None,
            window,
            cx,
        ) {
            eprintln!("Painting schematic label: {error}");
        }
    }
}

/// The width of an upper-cased name with the prototype's letter spacing, which follows every
/// letter, the last one too.
fn tracked_width(text: &str, size: f32, window: &Window) -> Pixels {
    text.to_uppercase()
        .chars()
        .map(|letter| {
            shape_text(&letter.to_string(), size, rgb(MUTED).into(), window).width
                + px(size * TITLE_TRACKING)
        })
        .fold(px(0.0), |total, width| total + width)
}

fn paint_tracked(text: &str, origin: Point<Pixels>, size: f32, window: &mut Window, cx: &mut App) {
    let mut x = origin.x;
    for letter in text.to_uppercase().chars() {
        let shaped = shape_text(&letter.to_string(), size, rgb(MUTED).into(), window);
        let width = shaped.width;
        if let Err(error) = shaped.paint(
            point(x, origin.y),
            px(18.0),
            gpui::TextAlign::Left,
            None,
            window,
            cx,
        ) {
            eprintln!("Painting schematic label: {error}");
        }
        x += width + px(size * TITLE_TRACKING);
    }
}

/// A reading to a fixed number of decimals with a unit, or Unavailable where there is none.
fn measure(value: Option<f32>, digits: usize, unit: &str) -> String {
    match value {
        Some(value) if value.is_finite() => {
            format!(
                "{}{unit}",
                crate::live_projection::fixed_text(value, digits)
            )
        }
        _ => "Unavailable".into(),
    }
}

fn label_values(callout: Callout, readings: &Readings) -> (String, String) {
    match callout {
        Callout::Fan0 | Callout::Fan1 => {
            let index = usize::from(matches!(callout, Callout::Fan1));
            let speed = readings.speeds[index];
            (
                measure(speed, 0, " rpm"),
                match speed {
                    Some(0.0) => "Stopped",
                    Some(_) => "Running",
                    None => "Unavailable",
                }
                .into(),
            )
        }
        Callout::Cpu | Callout::Gpu => {
            let index = usize::from(matches!(callout, Callout::Gpu));
            (
                measure(readings.temperatures[index], 1, " °C"),
                format!(
                    "Utilization {}",
                    measure(readings.utilization[index], 1, "%")
                ),
            )
        }
        Callout::Memory => {
            let total = readings.memory[1].map(|total| {
                let text = crate::live_projection::fixed_text(total, 1);
                format!("{} GiB", text.strip_suffix(".0").unwrap_or(&text))
            });
            (
                match (readings.memory[0], total) {
                    (Some(used), Some(total)) => {
                        format!("{} / {total}", crate::live_projection::fixed_text(used, 1))
                    }
                    (None, Some(total)) => format!("{total} installed"),
                    _ => "Unavailable".into(),
                },
                format!("Peak {}", measure(readings.temperatures[2], 1, " °C")),
            )
        }
        Callout::Battery => (
            measure(readings.battery[0], 1, "%"),
            format!("Power {}", measure(readings.battery[1], 1, " W")),
        ),
    }
}

fn shape_text(text: &str, height: f32, color: Hsla, window: &Window) -> gpui::ShapedLine {
    window.text_system().shape_line(
        text.to_owned().into(),
        px(height),
        &[TextRun {
            len: text.len(),
            font: font(".SystemUIFont"),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        None,
    )
}

fn paint_text(
    text: &str,
    origin: Point<Pixels>,
    height: f32,
    color: Hsla,
    window: &mut Window,
    cx: &mut App,
) {
    if let Err(error) = shape_text(text, height, color, window).paint(
        origin,
        px(height + 5.0),
        gpui::TextAlign::Left,
        None,
        window,
        cx,
    ) {
        eprintln!("Painting schematic error: {error}");
    }
}
