// Particle routes share the housing used by the still airflow tint.
use crate::hardware_motion::{Vector, bezier, perimeter};

pub struct Route {
    pub points: Vec<Vector>,
    pub length: f32,
}

pub fn create() -> Vec<Route> {
    let perimeter = perimeter();
    (0..24_u16)
        .map(|lane| Route::new(lane, perimeter))
        .collect()
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
        Self { points, length }
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
            &[end, first, second, Vector::new(exit_x, 108.0)],
            f32::from(step) / 32.0,
        ));
    }
    points.push(Vector::new(exit_x, 33.0));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_exit_upward_with_continuous_positive_segments() {
        // Inspect all measured lanes: each ends at the vent and every segment advances.
        let routes = create();
        assert_eq!(routes.len(), 24);
        for route in routes {
            assert_eq!(route.points.len(), 162);
            assert_eq!(route.points[161].y.to_bits(), 33.0_f32.to_bits());
            assert!((112.0..=289.0).contains(&route.points[161].x));
            let mut length = 0.0;
            for pair in route.points.windows(2) {
                let segment = pair[0].distance(pair[1]);
                assert!(segment > 0.0);
                length += segment;
            }
            assert!((length - route.length).abs() < 0.01);
        }
    }
}
