use gpui::{Bounds, Pixels, Window};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};

use crate::hardware_layout::{CALLOUTS, Callout, Layout};

use super::motion::Side;

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
pub(super) struct Frame {
    pub scale: f32,
    pub width: f32,
    pub height: f32,
    pub backing_scale: f32,
    pub stage: [f32; 2],
    padding: [f32; 2],
    pub sides: [Side; 2],
    pub leaders: [Leader; 6],
}

#[repr(C)]
#[derive(Clone, Copy, Default, PartialEq)]
pub(super) struct Leader {
    pub points: [[f32; 2]; 3],
    padding: [f32; 2],
    pub color: [f32; 4],
}

#[derive(Clone, Copy, Default, PartialEq)]
pub(super) struct Geometry {
    pub frame: Frame,
    pub native: CGRect,
    pub drawable: CGSize,
}

#[derive(Clone, Copy)]
struct Rectangle {
    left: f32,
    top: f32,
    right: f32,
    bottom: f32,
}

struct ClipBounds {
    mask: Rectangle,
    extent: Rectangle,
}

const _: () = {
    assert!(size_of::<Leader>() == 48);
    assert!(size_of::<Frame>() == 576);
    assert!(size_of::<Frame>() < 4096);
};

impl Frame {
    pub fn copy_from(&mut self, source: &Self) {
        self.scale = source.scale;
        self.width = source.width;
        self.height = source.height;
        self.backing_scale = source.backing_scale;
        self.stage = source.stage;
        self.padding = source.padding;
        for (side, source) in self.sides.iter_mut().zip(&source.sides) {
            side.copy_from(source);
        }
        self.leaders.copy_from_slice(&source.leaders);
    }
}

impl Geometry {
    pub fn copy_from(&mut self, source: &Self) {
        self.frame.copy_from(&source.frame);
        self.native = source.native;
        self.drawable = source.drawable;
    }
}

pub(super) fn create(
    geometry: &mut Geometry,
    bounds: Bounds<Pixels>,
    layout: &Layout,
    window: &Window,
    extent: [f32; 4],
) -> Result<bool, String> {
    let mask = window.content_mask().bounds.intersect(&bounds);
    let stage = [
        f32::from(bounds.origin.x) + layout.stage.x,
        f32::from(bounds.origin.y) + layout.stage.y,
    ];
    let clipped = clip(&ClipBounds {
        mask: Rectangle {
            left: f32::from(mask.origin.x),
            top: f32::from(mask.origin.y),
            right: f32::from(mask.right()),
            bottom: f32::from(mask.bottom()),
        },
        extent: Rectangle {
            left: stage[0] + extent[0] * layout.scale,
            top: stage[1] + extent[1] * layout.scale,
            right: stage[0] + (extent[0] + extent[2]) * layout.scale,
            bottom: stage[1] + (extent[1] + extent[3]) * layout.scale,
        },
    });
    let Some(clipped) = clipped else {
        return Ok(false);
    };

    geometry.frame.scale = layout.scale;
    geometry.frame.width = clipped.right - clipped.left;
    geometry.frame.height = clipped.bottom - clipped.top;
    geometry.frame.backing_scale = window.scale_factor();
    geometry.frame.stage = [stage[0] - clipped.left, stage[1] - clipped.top];
    create_leaders(
        &mut geometry.frame.leaders,
        layout,
        [
            f32::from(bounds.origin.x) - clipped.left,
            f32::from(bounds.origin.y) - clipped.top,
        ],
    );
    geometry.drawable = create_drawable(&geometry.frame)?;
    let viewport_height = f32::from(window.viewport_size().height);
    if !viewport_height.is_finite() {
        return Err("Nonfinite hardware animation viewport height".into());
    }

    geometry.native = CGRect::new(
        CGPoint::new(
            f64::from(clipped.left),
            f64::from(viewport_height - clipped.bottom),
        ),
        CGSize::new(
            f64::from(geometry.frame.width),
            f64::from(geometry.frame.height),
        ),
    );

    Ok(true)
}

fn create_leaders(leaders: &mut [Leader; 6], layout: &Layout, origin: [f32; 2]) {
    for (leader, callout) in leaders.iter_mut().zip(CALLOUTS) {
        leader.points = layout
            .leader(callout)
            .map(|point| [origin[0] + point.x, origin[1] + point.y]);
        leader.padding = [0.0; 2];
        leader.color = create_leader_color(callout);
    }
}

fn create_leader_color(callout: Callout) -> [f32; 4] {
    let tint = gpui::Rgba::from(crate::hardware::callout_color(callout));
    [tint.r, tint.g, tint.b, tint.a]
}

fn clip(bounds: &ClipBounds) -> Option<Rectangle> {
    let clipped = Rectangle {
        left: bounds.mask.left.max(bounds.extent.left),
        top: bounds.mask.top.max(bounds.extent.top),
        right: bounds.mask.right.min(bounds.extent.right),
        bottom: bounds.mask.bottom.min(bounds.extent.bottom),
    };
    if clipped.right <= clipped.left {
        return None;
    }

    if clipped.bottom <= clipped.top {
        return None;
    }

    Some(clipped)
}

fn create_drawable(frame: &Frame) -> Result<CGSize, String> {
    for value in [frame.scale, frame.width, frame.height, frame.backing_scale] {
        if !value.is_finite() {
            return Err("Nonfinite hardware animation geometry".into());
        }

        if value <= 0.0 {
            return Err("Nonpositive hardware animation geometry".into());
        }
    }
    for value in frame.stage {
        if !value.is_finite() {
            return Err("Nonfinite hardware animation stage origin".into());
        }
    }

    let drawable = CGSize::new(
        f64::from((frame.width * frame.backing_scale).ceil()),
        f64::from((frame.height * frame.backing_scale).ceil()),
    );
    for value in [drawable.width, drawable.height] {
        if !value.is_finite() {
            return Err("Nonfinite hardware animation drawable".into());
        }

        if value > 8192.0 {
            return Err("Hardware animation drawable exceeds 8192 pixels".into());
        }
    }

    Ok(drawable)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{
        AppContext, Context, IntoElement, Render, Styled, TestAppContext, canvas, point, px, size,
    };

    use super::*;

    struct GeometryView {
        exercise: fn(&mut Window),
        exercised: Rc<Cell<bool>>,
    }

    impl Render for GeometryView {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let exercise = self.exercise;
            let exercised = Rc::clone(&self.exercised);
            canvas(
                |_, _, _| (),
                move |_, (), window, _| {
                    exercise(window);
                    exercised.set(true);
                },
            )
            .size_full()
        }
    }

    fn draw_geometry(cx: &mut TestAppContext, exercise: fn(&mut Window)) {
        let exercised = Rc::new(Cell::new(false));
        let window = cx.open_window(size(px(900.0), px(600.0)), |_, _| GeometryView {
            exercise,
            exercised: Rc::clone(&exercised),
        });
        cx.update_window(window.into(), |_, window, cx| {
            window.draw(cx).clear(cx);
            window.remove_window();
        })
        .unwrap();

        assert!(exercised.get());
    }

    #[gpui::test]
    fn keeps_stage_placement_when_scrolled_and_clipped(cx: &mut TestAppContext) {
        // Paint through the creation boundary: clipping changes the drawable, not stage placement.
        draw_geometry(cx, |window| {
            let layout = Layout::new(840.0);
            let bounds = Bounds::new(
                point(px(17.0), px(-200.0)),
                size(px(840.0), px(layout.height)),
            );
            let mut geometry = Geometry::default();
            let mask = gpui::ContentMask {
                bounds: Bounds::new(point(px(0.0), px(100.0)), size(px(900.0), px(500.0))),
            };

            let visible = window
                .with_content_mask(Some(mask), |window| {
                    create(
                        &mut geometry,
                        bounds,
                        &layout,
                        window,
                        [100.0, 30.0, 600.0, 300.0],
                    )
                })
                .unwrap();

            assert!(visible);
            assert_eq!(geometry.native.origin.x.to_bits(), 117.0_f64.to_bits());
            assert_eq!(
                geometry.frame.stage.map(f32::to_bits),
                [-100.0_f32, -84.0].map(f32::to_bits)
            );
            assert_eq!(geometry.frame.width.to_bits(), 600.0_f32.to_bits());
            assert_eq!(geometry.frame.height.to_bits(), 246.0_f32.to_bits());
            assert_eq!(
                geometry.frame.leaders[0].points[0].map(f32::to_bits),
                [83.0_f32, 137.0].map(f32::to_bits)
            );
        });
    }

    #[gpui::test]
    fn rounds_fractional_clip_up_to_retina_pixels(cx: &mut TestAppContext) {
        // The creation boundary retains the final physical pixel of a fractional Retina clip.
        draw_geometry(cx, |window| {
            window.set_scale_factor(2.0);
            let layout = Layout::new(840.0);
            let bounds = Bounds::new(
                point(px(0.0), px(-216.0)),
                size(px(840.0), px(layout.height)),
            );
            let mut geometry = Geometry::default();

            let visible = create(
                &mut geometry,
                bounds,
                &layout,
                window,
                [10.125, 20.25, 100.25, 80.125],
            )
            .unwrap();

            assert!(visible);
            assert_eq!(geometry.drawable, CGSize::new(201.0, 161.0));
            assert_eq!(
                geometry.frame.stage.map(f32::to_bits),
                [-10.125_f32, -20.25].map(f32::to_bits)
            );
        });
    }

    #[gpui::test]
    fn reports_no_surface_when_the_diagram_is_outside_the_viewport(cx: &mut TestAppContext) {
        // A fully scrolled-away stage must produce no drawable through the same creation path.
        draw_geometry(cx, |window| {
            let layout = Layout::new(840.0);
            let bounds = Bounds::new(
                point(px(0.0), window.viewport_size().height + px(100.0)),
                size(px(840.0), px(layout.height)),
            );
            let mut geometry = Geometry::default();

            let visible = create(
                &mut geometry,
                bounds,
                &layout,
                window,
                [100.0, 30.0, 600.0, 300.0],
            )
            .unwrap();

            assert!(!visible);
        });
    }

    #[gpui::test]
    fn rejects_unbounded_drawable_at_creation_boundary(cx: &mut TestAppContext) {
        // An external scale that exceeds the surface bound is rejected before Metal allocation.
        draw_geometry(cx, |window| {
            window.set_scale_factor(32.0);
            let layout = Layout::new(840.0);
            let bounds = Bounds::new(
                point(px(0.0), px(-216.0)),
                size(px(840.0), px(layout.height)),
            );
            let mut geometry = Geometry::default();

            let result = create(
                &mut geometry,
                bounds,
                &layout,
                window,
                [0.0, 0.0, 300.0, 300.0],
            );

            assert!(result.is_err());
        });
    }
}
