use num_traits::ToPrimitive;

pub(super) const LANES: usize = 24;
const RPM_MAX: f64 = 12_000.0;
const RESPONSE_SECONDS: f64 = 0.4;

#[derive(Clone, Copy, Default)]
pub(super) struct Motion {
    rpm: f64,
    turns: f64,
    distances: [f64; LANES],
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq)]
pub(super) struct Side {
    pub angle: f32,
    pub active: f32,
    pub wash_opacity: f32,
    padding: f32,
    pub color: [f32; 4],
    pub distances: [f32; LANES],
}

#[derive(Clone, Copy, Default, PartialEq)]
pub(super) struct Input {
    pub rpm: [f32; 2],
    pub color: [[f32; 4]; 2],
}

const _: () = {
    assert!(LANES == 24);
    assert!(size_of::<Side>() == 128);
    assert!(RPM_MAX > 1500.0);
    assert!(RESPONSE_SECONDS > 0.0);
};

impl Input {
    pub fn new(readings: &crate::hardware::Readings) -> Self {
        Self {
            rpm: readings.speeds.map(input_rpm),
            color: [
                color(readings.temperatures[0]),
                color(readings.temperatures[1]),
            ],
        }
    }

    pub fn moving(&self) -> bool {
        self.rpm.iter().any(|rpm| *rpm > 0.0)
    }
}

impl Motion {
    pub fn advance(&mut self, rpm: f32, seconds: f64, lengths: &[f32; LANES]) {
        assert!(rpm.is_finite());
        assert!(rpm >= 0.0);
        assert!(f64::from(rpm) <= RPM_MAX);
        assert!(seconds.is_finite());
        assert!((0.0..=1.0).contains(&seconds));
        if rpm == 0.0 {
            self.rpm = 0.0;
            return;
        }

        let target = f64::from(rpm);
        let decay = (-seconds / RESPONSE_SECONDS).exp();
        let integrated = target * seconds + (self.rpm - target) * RESPONSE_SECONDS * (1.0 - decay);
        // Integrating the bounded visual speed at the midpoint keeps positive-RPM changes smooth.
        let average = if seconds > 0.0 {
            integrated / seconds
        } else {
            self.rpm
        };
        self.rpm = target + (self.rpm - target) * decay;
        self.turns = (self.turns + seconds * average / (average + 1500.0) / 4.0).rem_euclid(1.0);
        let distance = integrated * 160.0 / 1500.0;
        for (completed, length) in self.distances.iter_mut().zip(lengths) {
            assert!(length.is_finite());
            assert!(*length > 0.0);
            *completed = (*completed + distance).rem_euclid(f64::from(*length));
        }
    }

    pub fn frame(&self, side: &mut Side, rpm: f32, color: [f32; 4]) {
        side.angle = number(-self.turns * std::f64::consts::TAU);
        if rpm > 0.0 {
            side.active = 1.0;
            side.wash_opacity = crate::hardware::AIRFLOW_WASH_OPACITY;
        } else {
            side.active = 0.0;
            side.wash_opacity = 0.0;
        }

        side.padding = 0.0;
        side.color = color;
        for (distance, completed) in side.distances.iter_mut().zip(&self.distances) {
            *distance = number(*completed);
        }
    }

    pub fn rpm(&self) -> f32 {
        number(self.rpm)
    }
}

impl Side {
    pub fn copy_from(&mut self, source: &Self) {
        self.angle = source.angle;
        self.active = source.active;
        self.wash_opacity = source.wash_opacity;
        self.padding = source.padding;
        self.color = source.color;
        self.distances.copy_from_slice(&source.distances);
    }
}

fn input_rpm(value: Option<f32>) -> f32 {
    match value {
        Some(value) if value.is_finite() => value.clamp(0.0, 12_000.0),
        _ => 0.0,
    }
}

fn number(value: f64) -> f32 {
    assert!(value.is_finite());
    assert!(value.abs() < f64::from(f32::MAX));
    match value.to_f32() {
        Some(value) => value,
        None => unreachable!("Finite bounded animation value must fit f32"),
    }
}

fn color(temperature: Option<f32>) -> [f32; 4] {
    let color = gpui::Rgba::from(crate::hardware::airflow_color(temperature));
    [color.r, color.g, color.b, color.a]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preserves_phase_when_speed_changes() {
        // Advance a running fan, then change the target with no elapsed time.
        let mut motion = Motion::default();
        let mut before = Side::default();
        let mut after = Side::default();
        motion.advance(1350.0, 1.0, &[400.0; LANES]);
        motion.frame(&mut before, 1350.0, [1.0; 4]);

        motion.advance(4000.0, 0.0, &[400.0; LANES]);
        motion.frame(&mut after, 4000.0, [1.0; 4]);

        assert_eq!(before, after);
    }

    #[test]
    fn freezes_zero_rpm_and_removes_particles() {
        // A stopped fan retains the last angle and route positions exactly.
        let mut motion = Motion::default();
        let mut before = Side::default();
        let mut after = Side::default();
        motion.advance(3000.0, 0.5, &[400.0; LANES]);
        motion.frame(&mut before, 3000.0, [1.0; 4]);

        motion.advance(0.0, 1.0, &[400.0; LANES]);
        motion.frame(&mut after, 0.0, [1.0; 4]);

        assert_eq!(after.angle.to_bits(), before.angle.to_bits());
        assert_eq!(
            after.distances.map(f32::to_bits),
            before.distances.map(f32::to_bits)
        );
        assert_eq!(after.active.to_bits(), 0.0_f32.to_bits());
    }

    #[test]
    fn keeps_long_running_motion_precise_and_bounded() {
        // Simulate one day at one-second steps and inspect every uploaded value.
        let mut motion = Motion::default();
        let mut frame = Side::default();
        for _ in 0..86_400_u32 {
            motion.advance(12_000.0, 1.0, &[317.0; LANES]);
            motion.frame(&mut frame, 12_000.0, [1.0; 4]);
            assert!(frame.angle >= -std::f32::consts::TAU);
            assert!(frame.angle <= 0.0);
            for distance in frame.distances {
                assert!((0.0..317.0).contains(&distance));
            }
        }
    }

    #[test]
    fn missing_speed_is_stopped() {
        // Missing channels must never acquire a fabricated positive speed.
        let readings = crate::hardware::Readings {
            speeds: [None, Some(1500.0)],
            ..crate::hardware::Readings::default()
        };

        let input = Input::new(&readings);

        assert_eq!(input.rpm[0].to_bits(), 0.0_f32.to_bits());
        assert_eq!(input.rpm[1].to_bits(), 1500.0_f32.to_bits());
    }

    #[test]
    fn invalid_speed_is_stopped() {
        // A corrupt reading freezes the side instead of reaching the GPU.
        let readings = crate::hardware::Readings {
            speeds: [Some(f32::NAN), Some(1500.0)],
            ..crate::hardware::Readings::default()
        };

        let input = Input::new(&readings);

        assert_eq!(input.rpm[0].to_bits(), 0.0_f32.to_bits());
        assert_eq!(input.rpm[1].to_bits(), 1500.0_f32.to_bits());
    }

    #[test]
    fn advances_phase_and_routes_during_positive_rpm_elapsed_time() {
        // A quarter-second at 1500 RPM advances without wrapping any route or the rotor.
        let mut motion = Motion::default();
        let mut frame = Side::default();
        let seconds = 0.25;
        let integrated_rpm = 1500.0 * (seconds - 0.4 * (1.0 - (-seconds / 0.4_f64).exp()));
        let average_rpm = integrated_rpm / seconds;
        let expected_angle =
            -std::f64::consts::TAU * seconds * average_rpm / (average_rpm + 1500.0) / 4.0;
        let expected_distance = integrated_rpm * 160.0 / 1500.0;

        motion.advance(1500.0, seconds, &[400.0; LANES]);
        motion.frame(&mut frame, 1500.0, [1.0; 4]);

        assert!((f64::from(frame.angle) - expected_angle).abs() < 0.000_001);
        for distance in frame.distances {
            assert!((f64::from(distance) - expected_distance).abs() < 0.000_01);
        }
    }

    #[test]
    fn accelerates_smoothly_toward_changed_positive_rpm() {
        // A target step changes velocity gradually while retaining forward motion.
        let mut motion = Motion::default();
        motion.advance(1500.0, 1.0, &[400.0; LANES]);
        let before = motion.rpm();

        motion.advance(6000.0, 0.01, &[400.0; LANES]);

        assert!(motion.rpm() > before);
        assert!(motion.rpm() < 2000.0);
    }
}
