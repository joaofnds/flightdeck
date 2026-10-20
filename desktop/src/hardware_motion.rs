use std::cell::{Cell, RefCell};
use std::sync::OnceLock;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use num_traits::ToPrimitive;

pub struct HardwareState {
    pub clock: RefCell<Option<crate::live_timer::Timer>>,
    started: Instant,
    painted: Cell<bool>,
    enabled: Cell<bool>,
    motions: RefCell<[FanMotion; 2]>,
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
    animation_enabled: bool,
}

impl HardwareState {
    pub fn new() -> Self {
        Self {
            clock: RefCell::new(None),
            started: Instant::now(),
            painted: Cell::new(false),
            enabled: Cell::new(false),
            motions: RefCell::new([FanMotion::default(); 2]),
            visibility: Cell::new(None),
            visibility_logs: Cell::new(0),
            paints: Cell::new(0),
        }
    }

    pub fn begin_frame(&self) {
        self.painted.set(false);
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
            animation_enabled: self.needs_frame(),
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
            serde_json::json!({"event":"hardware_visibility", "schema":1,
            "engine":"GPUI", "visible":geometry.visible(),
            "geometry_scope":"Assembly image inside the labeled schematic",
            "clipped_width":geometry.clipped[0], "clipped_height":geometry.clipped[1],
            "stage_width":geometry.stage[0], "stage_height":geometry.stage[1],
            "window_active":geometry.window_active,
            "animation_enabled":signature.animation_enabled,
            "elapsed_ms":self.started.elapsed().as_millis(), "wall_ms":observed_wall_ms(),
            "metric":"App-owned stage paint and clock eligibility; not presentation"})
        );
    }

    pub fn paint_count(&self) -> u64 {
        self.paints.get()
    }

    pub fn needs_frame(&self) -> bool {
        if !self.enabled.get() {
            return false;
        }
        if !self.painted.get() {
            self.enabled.set(false);
            for motion in self.motions.borrow_mut().iter_mut() {
                motion.enabled = false;
            }
            return false;
        }
        self.motions.borrow().iter().any(|motion| motion.rpm > 0.0)
    }

    pub fn frame(&self, speeds: [Option<f32>; 2], enabled: bool) -> [FanFrame; 2] {
        let now = self.started.elapsed().as_secs_f64();
        self.painted.set(true);
        self.enabled.set(enabled);
        let mut motions = self.motions.borrow_mut();
        let mut result = [FanFrame::default(); 2];
        for index in 0..2 {
            motions[index].set(speeds[index], enabled, now);
            result[index] = motions[index].frame(now);
        }
        drop(motions);
        let running = if enabled {
            speeds.iter().flatten().any(|speed| *speed > 0.0)
        } else {
            false
        };
        if let Some(clock) = self.clock.borrow().as_ref() {
            clock.set_running(running);
        }
        result
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

#[derive(Clone, Copy, Default)]
pub struct FanMotion {
    rpm: f64,
    enabled: bool,
    date: f64,
    completed_turns: f64,
    completed_distance: f64,
}

#[derive(Clone, Copy, Default)]
pub struct FanFrame {
    pub turns: f32,
    pub distances: [f32; 24],
}

impl FanMotion {
    pub fn set(&mut self, rpm: Option<f32>, enabled: bool, date: f64) {
        assert!(date.is_finite());
        assert!(date >= 0.0);
        self.completed_turns = self.turns(date);
        self.completed_distance = self.distance(date);
        self.date = date;
        self.rpm = match rpm {
            Some(value) if value.is_finite() => f64::from(value.max(0.0)),
            _ => 0.0,
        };
        self.enabled = enabled;
    }

    pub fn turns(&self, date: f64) -> f64 {
        self.completed_turns + self.elapsed(date) * self.rpm / (self.rpm + 1500.0) / 4.0
    }

    pub fn distance(&self, date: f64) -> f64 {
        self.completed_distance + self.elapsed(date) * 160.0 * (self.rpm / 1500.0).min(8.0)
    }

    fn frame(&self, date: f64) -> FanFrame {
        let distance = self.distance(date);
        let mut frame = FanFrame {
            turns: frame_number(self.turns(date).rem_euclid(1.0)),
            distances: [0.0; 24],
        };
        // Reduce each lane before narrowing, so multi-day totals never enter f32 arithmetic.
        for (lane, route) in routes().iter().enumerate() {
            frame.distances[lane] = frame_number(distance.rem_euclid(f64::from(route.length)));
        }
        frame
    }

    fn elapsed(&self, date: f64) -> f64 {
        if self.enabled {
            (date - self.date).max(0.0)
        } else {
            0.0
        }
    }
}

fn frame_number(value: f64) -> f32 {
    assert!(value.is_finite());
    assert!(value >= 0.0);
    assert!(value <= f64::from(f32::MAX));
    match value.to_f32() {
        Some(value) => value,
        None => unreachable!("A bounded finite render phase must fit f32"),
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
    fn distance(self, other: Self) -> f32 {
        (self.x - other.x).hypot(self.y - other.y)
    }
}

pub struct Route {
    points: Vec<Vector>,
    distances: Vec<f32>,
    pub length: f32,
}

pub struct Particle {
    pub position: Vector,
    pub opacity: f32,
    pub radius: f32,
}

pub fn routes() -> &'static [Route] {
    static ROUTES: OnceLock<Vec<Route>> = OnceLock::new();
    ROUTES.get_or_init(|| {
        (0..24_u16)
            .map(|lane| Route::new(lane, perimeter()))
            .collect()
    })
}

pub fn particle(lane: u16, phase: u16, distance: f32) -> Particle {
    assert!(lane < 24);
    assert!(phase < 4);
    let route = &routes()[usize::from(lane)];
    let variation = (f32::from(lane) * 0.618_034 + f32::from(phase) * 0.381_966).fract();
    let offset = (f32::from(phase) + (f32::from(lane) * 0.618_034).fract()) / 4.0;
    let progress = (distance / route.length + offset).fract();
    Particle {
        position: route.position(progress),
        opacity: (progress / 0.06).min((1.0 - progress) / 0.08).min(1.0)
            * (0.36 + variation * 0.24),
        radius: 0.65 + variation * 0.45,
    }
}

impl Route {
    fn new(lane: u16, perimeter: &[Vector]) -> Self {
        assert!(lane < 24);
        let fraction = (f32::from(lane) + 0.5) / 24.0;
        let variation = (f32::from(lane) * 0.618_034).fract();
        let final_angle = -0.4 - fraction * 0.1;
        let start_angle = std::f32::consts::PI + 0.9 - fraction * 0.3;
        let mut points = Vec::with_capacity(162);
        for step in 0..=128_u16 {
            let angle = start_angle + (final_angle - start_angle) * f32::from(step) / 128.0;
            let wall = radius(angle, perimeter) - 3.0 - variation * 8.0;
            let radius = 32.0 + (wall - 32.0) * (1.0 - fraction);
            points.push(Vector::new(
                183.0 + angle.cos() * radius,
                221.0 + angle.sin() * radius,
            ));
        }
        route_exit(&mut points, fraction);
        assert_eq!(points.len(), 162);
        let mut distances = Vec::with_capacity(points.len());
        distances.push(0.0);
        for index in 1..points.len() {
            distances.push(distances[index - 1] + points[index].distance(points[index - 1]));
        }
        let length = distances[distances.len() - 1];
        assert!(length.is_finite());
        assert!(length > 0.0);
        Self {
            points,
            distances,
            length,
        }
    }

    fn position(&self, progress: f32) -> Vector {
        let distance = progress.clamp(0.0, 1.0) * self.length;
        let mut lower = 0;
        let mut upper = self.distances.len() - 1;
        for _ in 0..9 {
            if lower + 1 < upper {
                let middle = usize::midpoint(lower, upper);
                if self.distances[middle] < distance {
                    lower = middle;
                } else {
                    upper = middle;
                }
            }
        }
        let fraction =
            (distance - self.distances[lower]) / (self.distances[upper] - self.distances[lower]);
        Vector::new(
            self.points[lower].x + (self.points[upper].x - self.points[lower].x) * fraction,
            self.points[lower].y + (self.points[upper].y - self.points[lower].y) * fraction,
        )
    }
}

pub fn perimeter() -> &'static [Vector] {
    static PERIMETER: OnceLock<Vec<Vector>> = OnceLock::new();
    PERIMETER.get_or_init(perimeter_make)
}

pub fn contains(position: Vector) -> bool {
    if (position.x - 183.0).hypot(position.y - 221.0) < 29.0 {
        return false;
    }
    if (112.0..=289.0).contains(&position.x) {
        if (33.0..=84.0).contains(&position.y) {
            return true;
        }
    }
    let perimeter = perimeter();
    let mut inside = false;
    for index in 0..perimeter.len() {
        let first = perimeter[index];
        let second = perimeter[(index + 1) % perimeter.len()];
        if (first.y > position.y) != (second.y > position.y) {
            let intersection =
                (second.x - first.x) * (position.y - first.y) / (second.y - first.y) + first.x;
            if position.x < intersection {
                inside = !inside;
            }
        }
    }
    inside
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

fn radius(angle: f32, perimeter: &[Vector]) -> f32 {
    assert!(perimeter.len() <= 102);
    let direction = Vector::new(angle.cos(), angle.sin());
    let mut radius = if direction.y < 0.0 {
        (145.0 - 221.0) / direction.y
    } else {
        f32::INFINITY
    };
    for index in 0..perimeter.len() {
        let start = perimeter[index];
        let end = perimeter[(index + 1) % perimeter.len()];
        let edge = Vector::new(end.x - start.x, end.y - start.y);
        let relative = Vector::new(start.x - 183.0, start.y - 221.0);
        let cross = direction.x * edge.y - direction.y * edge.x;
        if cross.abs() > 1.0e-8 {
            let distance = (relative.x * edge.y - relative.y * edge.x) / cross;
            let fraction = (relative.x * direction.y - relative.y * direction.x) / cross;
            if distance > 0.0 {
                if (0.0..=1.0).contains(&fraction) {
                    radius = radius.min(distance);
                }
            }
        }
    }
    assert!(radius.is_finite());
    radius
}

fn route_exit(points: &mut Vec<Vector>, fraction: f32) {
    assert_eq!(points.len(), 129);
    let end = points[points.len() - 1];
    let previous = points[points.len() - 2];
    let exit_x = 112.0 + (1.0 - fraction) * 177.0;
    assert!(end.y > 108.0);
    let tangent = 28.0_f32.min((end.y - 108.0) * 0.45);
    let heading = end.distance(previous);
    assert!(heading > 0.0);
    let first = Vector::new(
        end.x + (end.x - previous.x) / heading * tangent,
        end.y + (end.y - previous.y) / heading * tangent,
    );
    let second = Vector::new(exit_x, first.y + (108.0 - first.y) * 0.7);
    for step in 1..=32_u16 {
        points.push(bezier(
            [end, first, second, Vector::new(exit_x, 108.0)],
            f32::from(step) / 32.0,
        ));
    }
    points.push(Vector::new(exit_x, 33.0));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_fan_motion_and_preserves_phase_when_paused() {
        // The 1500-rpm visualization completes one eighth turn and 160 points per second.
        let mut motion = FanMotion::default();
        motion.set(Some(1500.0), true, 0.0);
        assert!((motion.turns(4.0) - 0.5).abs() < f64::EPSILON);
        assert!((motion.distance(4.0) - 640.0).abs() < f64::EPSILON);
        motion.set(Some(3000.0), false, 4.0);
        assert!((motion.turns(10.0) - 0.5).abs() < f64::EPSILON);
        assert!((motion.distance(10.0) - 640.0).abs() < f64::EPSILON);
        motion.set(Some(3000.0), true, 10.0);
        assert!((motion.turns(13.0) - 1.0).abs() < 0.000_01);
        assert!((motion.distance(13.0) - 1600.0).abs() < f64::EPSILON);
    }

    #[test]
    fn multi_day_uptime_preserves_each_thirtieth_second_phase_increment() {
        // Rebase a real accumulated total at six and seven days, then check one 30-Hz step.
        for uptime in [524_288.0, 604_799.0] {
            let mut motion = FanMotion::default();
            motion.set(Some(1500.0), true, 0.0);
            motion.set(Some(1500.0), true, uptime);
            let before = motion.frame(uptime);
            motion.set(Some(1500.0), true, uptime + 1.0 / 30.0);
            let after = motion.frame(uptime + 1.0 / 30.0);
            let turns = (f64::from(after.turns) - f64::from(before.turns)).rem_euclid(1.0);
            assert!((turns - 1.0 / 240.0).abs() < 0.000_000_1);
            for (lane, route) in routes().iter().enumerate() {
                let distance = (f64::from(after.distances[lane])
                    - f64::from(before.distances[lane]))
                .rem_euclid(f64::from(route.length));
                assert!((distance - 160.0 / 30.0).abs() < 0.000_1);
            }
            motion.set(None, false, uptime + 1.0 / 30.0);
            let paused = motion.frame(uptime + 100.0);
            assert_eq!(paused.turns.to_bits(), after.turns.to_bits());
            assert_eq!(
                paused.distances.map(f32::to_bits),
                after.distances.map(f32::to_bits)
            );
        }
    }

    #[test]
    fn airflow_has_the_same_bounded_route_and_particle_counts() {
        // Every airflow lane has 129 arc points, 32 exit samples and one duct endpoint.
        assert_eq!(routes().len(), 24);
        for (lane, route) in routes().iter().enumerate() {
            assert_eq!(route.points.len(), 162);
            assert!(route.length > 0.0);
            for phase in 0..4_u16 {
                let particle = particle(u16::try_from(lane).unwrap(), phase, 1234.0);
                assert!(particle.position.x.is_finite());
                assert!(particle.position.y.is_finite());
                assert!((0.0..=0.6).contains(&particle.opacity));
                assert!((0.65..=1.1).contains(&particle.radius));
            }
        }
    }

    #[test]
    fn only_visible_running_fans_keep_the_clock_active() {
        // Exercise visible, stopped, hidden and resumed states without waiting on a timer.
        let state = HardwareState::new();
        state.frame([Some(1500.0), None], true);
        assert!(state.needs_frame());
        state.frame([Some(0.0), None], true);
        assert!(!state.needs_frame());
        state.frame([Some(1500.0), None], false);
        assert!(!state.needs_frame());
        state.frame([Some(1500.0), None], true);
        state.begin_frame();
        assert!(!state.needs_frame());
        state.frame([Some(1500.0), None], true);
        assert!(state.needs_frame());
    }

    #[test]
    fn airflow_clip_excludes_motor_and_exterior() {
        // Match the housing union and inverse motor clip used by the schematic.
        assert!(!contains(Vector::new(183.0, 221.0)));
        assert!(!contains(Vector::new(0.0, 0.0)));
        assert!(contains(Vector::new(200.0, 50.0)));
        assert!(contains(Vector::new(240.0, 220.0)));
    }
}
