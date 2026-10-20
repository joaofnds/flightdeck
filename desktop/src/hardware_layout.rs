use crate::hardware_motion::Vector;

/// The width the prototype reserves beside the stage for the callouts' labels.
const RAIL: f32 = 174.0;

pub struct Layout {
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    pub stage: Vector,
    pub label_width: f32,
    rails: bool,
}

#[derive(Clone, Copy)]
pub enum Callout {
    Fan0,
    Fan1,
    Cpu,
    Gpu,
    Memory,
    Battery,
}

pub const CALLOUTS: [Callout; 6] = [
    Callout::Fan0,
    Callout::Fan1,
    Callout::Cpu,
    Callout::Gpu,
    Callout::Memory,
    Callout::Battery,
];

impl Layout {
    pub fn new(width: f32) -> Self {
        assert!(width.is_finite());
        assert!(width > 0.0);
        let rails = width >= 850.0;
        if rails {
            // The prototype keeps 174 pixels either side of the stage for the callouts, 50 above
            // it and 36 below, and fits the 840 by 600 assembly into what is left.
            let scale = (width - 2.0 * RAIL) / 840.0;
            Self {
                width,
                height: 50.0 + 600.0 * scale + 36.0,
                scale,
                stage: Vector::new(RAIL, 50.0),
                label_width: 158.0,
                rails,
            }
        } else {
            Self {
                width,
                height: width * 600.0 / 840.0 + 320.0,
                scale: width / 840.0,
                stage: Vector::new(0.0, 216.0),
                label_width: (width / 2.0 - 24.0).max(0.0),
                rails,
            }
        }
    }

    pub fn point(&self, point: Vector) -> Vector {
        Vector::new(
            self.stage.x + point.x * self.scale,
            self.stage.y + point.y * self.scale,
        )
    }

    pub fn label(&self, callout: Callout) -> Vector {
        let left = if callout.left() {
            0.0
        } else {
            self.width - self.label_width
        };
        let top = if self.rails {
            (self.stage.y + (callout.anchor().y + callout.rise()) * self.scale - 76.0).max(0.0)
        } else {
            match callout {
                Callout::Fan0 | Callout::Fan1 => 98.0,
                Callout::Cpu | Callout::Gpu => 0.0,
                Callout::Memory | Callout::Battery => self.stage.y + 600.0 * self.scale + 36.0,
            }
        };
        Vector::new(left, top)
    }

    pub fn leader(&self, callout: Callout) -> [Vector; 3] {
        let anchor = self.point(callout.anchor());
        if self.rails {
            let rise = callout.rise() * self.scale;
            let direction = if callout.left() { -1.0 } else { 1.0 };
            let elbow = Vector::new(anchor.x + direction * rise.abs(), anchor.y + rise);
            let port = Vector::new(if callout.left() { 0.0 } else { self.width }, elbow.y);
            [anchor, elbow, port]
        } else {
            let below = matches!(callout, Callout::Memory | Callout::Battery);
            let port = Vector::new(
                self.width * callout.fraction(),
                self.label(callout).y + if below { -8.0 } else { 74.0 },
            );
            let elbow = Vector::new(
                port.x,
                anchor.y + if below { 1.0 } else { -1.0 } * (port.x - anchor.x).abs(),
            );
            [anchor, elbow, port]
        }
    }
}

impl Callout {
    pub fn left(self) -> bool {
        matches!(self, Self::Fan0 | Self::Cpu | Self::Memory)
    }
    pub fn title(self) -> &'static str {
        match self {
            Self::Fan0 => "FAN 0",
            Self::Fan1 => "FAN 1",
            Self::Cpu => "CPU",
            Self::Gpu => "GPU",
            Self::Memory => "MEMORY",
            Self::Battery => "BATTERY",
        }
    }
    pub fn anchor(self) -> Vector {
        let (x, y) = match self {
            Self::Fan0 => (183.0, 221.0),
            Self::Fan1 => (657.0, 221.0),
            Self::Cpu => (400.0, 238.0),
            Self::Gpu => (440.0, 238.0),
            Self::Memory => (380.0, 228.0),
            Self::Battery => (608.0, 458.0),
        };
        Vector::new(x, y)
    }
    fn rise(self) -> f32 {
        match self {
            Self::Fan0 | Self::Fan1 => -55.0,
            Self::Cpu | Self::Gpu => -195.0,
            Self::Memory => 132.0,
            Self::Battery => 60.0,
        }
    }
    fn fraction(self) -> f32 {
        match self {
            Self::Fan0 => 0.0,
            Self::Fan1 => 1.0,
            Self::Cpu => 0.46,
            Self::Gpu => 0.54,
            Self::Memory => 0.25,
            Self::Battery => 0.88,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_follow_assembly_coordinates_at_each_width() {
        // Check wide rails and compact cards without allowing anchors outside the stage.
        for width in [360.0, 849.0, 850.0, 1188.0, 1500.0] {
            let layout = Layout::new(width);
            for callout in CALLOUTS {
                let anchor = layout.leader(callout)[0];
                assert!(anchor.x >= 0.0);
                assert!(anchor.x <= width);
                assert!(anchor.y >= 0.0);
                assert!(anchor.y <= layout.height);
                assert!(layout.label(callout).x >= 0.0);
            }
        }
        let layout = Layout::new(1188.0);
        assert!((layout.point(Callout::Fan0.anchor()).x - 357.0).abs() < f32::EPSILON);
        assert!((layout.point(Callout::Fan0.anchor()).y - 271.0).abs() < f32::EPSILON);
    }
}
