use std::cell::Cell;
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

pub struct HardwareState {
    started: Instant,
    visibility: Cell<Option<VisibilitySignature>>,
    visibility_logs: Cell<u16>,
    paints: Cell<u64>,
}

pub struct HardwareVisibility {
    pub clipped: [f32; 2],
    pub stage: [f32; 2],
    pub diagram_visible: bool,
    pub window_active: bool,
}

impl HardwareVisibility {
    pub fn visible(&self) -> bool {
        if self.clipped[0] > 0.0 {
            self.clipped[1] > 0.0
        } else {
            false
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct VisibilitySignature {
    dimensions: [u32; 4],
    window_active: bool,
}

impl HardwareState {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            visibility: Cell::new(None),
            visibility_logs: Cell::new(0),
            paints: Cell::new(0),
        }
    }

    pub fn acknowledge_visibility(&self, geometry: &HardwareVisibility) {
        if geometry.visible() {
            self.paints.set(self.paints.get().saturating_add(1));
        }
        let signature = VisibilitySignature {
            dimensions: [
                geometry.clipped[0].to_bits(),
                geometry.clipped[1].to_bits(),
                geometry.stage[0].to_bits(),
                geometry.stage[1].to_bits(),
            ],
            window_active: geometry.window_active,
        };
        if self.visibility.get() == Some(signature) {
            return;
        }
        if self.visibility_logs.get() >= 256 {
            return;
        }
        self.visibility.set(Some(signature));
        self.visibility_logs.set(self.visibility_logs.get() + 1);
        println!(
            "{}",
            serde_json::json!({"event":"hardware_visibility", "schema":2,
            "engine":"GPUI", "visible":geometry.visible(),
            "geometry_scope":"Assembly image inside the labeled schematic",
            "clipped_width":geometry.clipped[0], "clipped_height":geometry.clipped[1],
            "stage_width":geometry.stage[0], "stage_height":geometry.stage[1],
            "window_active":geometry.window_active,
            "elapsed_ms":self.started.elapsed().as_millis(), "wall_ms":observed_wall_ms(),
            "metric":"App-owned stage paint; not presentation"})
        );
    }

    pub fn paint_count(&self) -> u64 {
        self.paints.get()
    }
}

fn observed_wall_ms() -> Option<u64> {
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())
        .and_then(|duration| {
            u64::try_from(duration.as_millis()).map_err(|error| error.to_string())
        });
    match time {
        Ok(time) => Some(time),
        Err(error) => {
            eprintln!("Hardware observation clock: {error}");
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Vector {
    pub x: f32,
    pub y: f32,
}

impl Vector {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

pub fn perimeter() -> &'static [Vector] {
    static PERIMETER: OnceLock<Vec<Vector>> = OnceLock::new();
    PERIMETER.get_or_init(perimeter_make)
}

fn perimeter_make() -> Vec<Vector> {
    let mut points = Vec::with_capacity(102);
    points.extend([
        Vector::new(109.0, 83.0),
        Vector::new(292.0, 83.0),
        Vector::new(292.0, 221.0),
    ]);
    perimeter_curve(
        &mut points,
        [
            Vector::new(292.0, 277.0),
            Vector::new(251.0, 319.0),
            Vector::new(194.0, 319.0),
        ],
    );
    perimeter_curve(
        &mut points,
        [
            Vector::new(141.0, 319.0),
            Vector::new(102.0, 281.0),
            Vector::new(102.0, 227.0),
        ],
    );
    points.push(Vector::new(102.0, 174.0));
    perimeter_curve(
        &mut points,
        [
            Vector::new(102.0, 157.0),
            Vector::new(110.0, 150.0),
            Vector::new(123.0, 140.0),
        ],
    );
    points.push(Vector::new(109.0, 140.0));
    assert_eq!(points.len(), 101);
    points
}

fn perimeter_curve(points: &mut Vec<Vector>, curve: [Vector; 3]) {
    assert!(points.len() <= 69);
    let start = points[points.len() - 1];
    for step in 1..=32_u16 {
        points.push(bezier(
            [start, curve[0], curve[1], curve[2]],
            f32::from(step) / 32.0,
        ));
    }
}

fn bezier(points: [Vector; 4], progress: f32) -> Vector {
    let inverse = 1.0 - progress;
    let weights = [
        inverse.powi(3),
        3.0 * inverse * inverse * progress,
        3.0 * inverse * progress * progress,
        progress.powi(3),
    ];
    let mut result = Vector::default();
    for index in 0..4 {
        result.x += points[index].x * weights[index];
        result.y += points[index].y * weights[index];
    }
    result
}
