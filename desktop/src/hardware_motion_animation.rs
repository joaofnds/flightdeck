use gpui::{Bounds, Pixels, Window};

use super::HardwareState;
use crate::hardware::Readings;
use crate::hardware_animation::HardwareAnimation;
use crate::hardware_layout::Layout;

impl HardwareState {
    pub(crate) fn begin_frame(&self, allowed: bool) {
        let reduced = if allowed {
            (self.motion_preference)()
        } else {
            self.motion_reduced.get()
        };
        self.motion_reduced.set(reduced);
        let allowed = allowed && !reduced;
        self.animation_painted.set(false);
        self.animation_allowed.set(allowed);
        if !allowed {
            self.suspend_animation();
        }
    }

    pub(crate) fn finish_frame(&self) {
        if !self.animation_painted.get() {
            self.suspend_animation();
        }
    }

    pub(crate) fn suspend_animation(&self) {
        if let Some(renderer) = &mut self.animation.borrow_mut().renderer {
            renderer.suspend();
        }
    }

    pub(crate) fn paint_animation(
        &self,
        bounds: Bounds<Pixels>,
        layout: &Layout,
        readings: &Readings,
        window: &Window,
    ) -> bool {
        if !self.animation_allowed.get() {
            return false;
        }

        if !window.is_visible() {
            return false;
        }

        if !window.is_window_active() {
            return false;
        }

        self.animation_painted.set(true);

        let mut animation = self.animation.borrow_mut();
        if animation.failure.is_some() {
            return false;
        }

        if animation.renderer.is_none() {
            match HardwareAnimation::new(window) {
                Ok(renderer) => animation.renderer = Some(renderer),
                Err(error) => {
                    animation.fail(error);
                    return false;
                }
            }
        }
        let Some(renderer) = &mut animation.renderer else {
            unreachable!("Successful animation setup installs its renderer");
        };

        match renderer.update(bounds, layout, readings, window) {
            Ok(visible) => visible,
            Err(error) => {
                animation.fail(error);
                false
            }
        }
    }

    pub(crate) fn poll_animation(&self, enabled: bool) -> bool {
        let reduced = if enabled {
            (self.motion_preference)()
        } else {
            false
        };
        let changed = self.motion_reduced.replace(reduced) != reduced;
        if reduced {
            self.suspend_animation();
        }

        let mut animation = self.animation.borrow_mut();
        if animation.failure.is_some() {
            return !self.animation_failure_reported.replace(true) || changed;
        }

        let ready = match &animation.renderer {
            Some(renderer) => match renderer.status() {
                Ok(ready) => ready,
                Err(error) => {
                    animation.fail(error);
                    self.animation_ready.set(false);
                    self.animation_failure_reported.set(true);
                    return true;
                }
            },
            None => false,
        };

        (self.animation_ready.replace(ready) != ready) || changed
    }

    pub(crate) fn animation_failure(&self) -> Option<String> {
        self.animation.borrow().failure.clone()
    }

    pub(crate) fn animation_statistics(&self) -> Option<crate::hardware_animation::Statistics> {
        self.animation
            .borrow()
            .renderer
            .as_ref()
            .map(HardwareAnimation::statistics)
    }
}

impl super::Animation {
    fn fail(&mut self, error: String) {
        if self.failure.is_none() {
            eprintln!("Fan animation unavailable: {error}");
            self.failure = Some(error);
        }
        if let Some(renderer) = &mut self.renderer {
            renderer.suspend();
        }
    }
}

pub(crate) fn observe_window(
    dashboard: &gpui::Entity<crate::live_dashboard::LiveDashboard>,
    window: &mut Window,
    cx: &mut gpui::App,
) {
    dashboard.update(cx, |_, cx| {
        cx.observe_window_activation(window, |dashboard, window, cx| {
            if !window.is_window_active() {
                dashboard.hardware_state.suspend_animation();
            }
            if dashboard.settings.animate_fans {
                cx.notify();
            }
        })
        .detach();
        cx.observe_window_visibility(window, |dashboard, visibility, _, cx| {
            if !visibility.is_visible() {
                dashboard.hardware_state.suspend_animation();
            }
            if dashboard.settings.animate_fans {
                cx.notify();
            }
        })
        .detach();
    });
}
