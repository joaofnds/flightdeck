//! Owned native fan surface. Call only on the window's main thread.
//!
//! The caller paints still rotors unless update/status returns true. Poll status on the
//! ordinary dashboard tick and notify only when its result changes. Suspend before overlays,
//! reduced motion, lost visibility, or a frame that did not paint this hardware stage.
use std::cell::Cell;

use foreign_types::ForeignTypeRef;
use gpui::{Bounds, Pixels, Window};
use objc2::MainThreadMarker;
use objc2::rc::autoreleasepool;
use objc2_app_kit::NSView;
use objc2_quartz_core::{CAMetalLayer, CATransaction};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};

use crate::hardware::Readings;
use crate::hardware_layout::Layout;

#[path = "hardware_animation_data.rs"]
mod data;
#[path = "hardware_animation_geometry.rs"]
mod geometry;
#[path = "hardware_animation_motion.rs"]
mod motion;
#[path = "hardware_animation_routes.rs"]
mod routes;
#[path = "hardware_animation_worker.rs"]
mod worker;

pub struct HardwareAnimation {
    layer: metal::MetalLayer,
    worker: worker::Owner,
    extent: [f32; 4],
    geometry: geometry::Geometry,
    geometry_pending: geometry::Geometry,
    geometry_current: bool,
    request: worker::Request,
    request_pending: worker::Request,
    request_current: bool,
    generation: u64,
    hidden: Cell<bool>,
    view: std::ptr::NonNull<NSView>,
    // This makes transferring native ownership away from the main thread a type error.
    main_thread: MainThreadMarker,
}

/// Approximate atomic telemetry for automation, independent of UI frame scheduling.
#[derive(Clone, Copy, Debug, serde::Serialize)]
pub struct Statistics {
    pub submitted: u64,
    pub completed: u64,
    pub completion_age_ms: Option<u64>,
    pub angles: [f32; 2],
    pub rpm: [f32; 2],
}

impl HardwareAnimation {
    pub fn new(window: &Window) -> Result<Self, String> {
        let main_thread = MainThreadMarker::new().ok_or("Fan owner requires the main thread")?;
        autoreleasepool(|_| Self::new_native(window, main_thread))
    }

    fn new_native(window: &Window, main_thread: MainThreadMarker) -> Result<Self, String> {
        let device = metal::Device::system_default().ok_or("No default Metal device")?;
        let resources = data::create(&device)?;
        let extent = resources.extent;
        let layer = metal::MetalLayer::new();
        new_layer(&layer, &device);
        let view = window_view(window)?;
        // GPUI owns the view. Its handle is used only while this main-thread borrow is live.
        let host = unsafe { view.as_ref() }
            .layer()
            .ok_or("GPUI view has no native layer")?;
        let worker = worker::Owner::new(layer.clone(), resources)?;

        CATransaction::begin();
        CATransaction::setDisableActions(true);
        host.addSublayer(native_layer(&layer));
        CATransaction::commit();

        Ok(Self {
            layer,
            worker,
            extent,
            geometry: geometry::Geometry::default(),
            geometry_pending: geometry::Geometry::default(),
            geometry_current: false,
            request: worker::Request::default(),
            request_pending: worker::Request::default(),
            request_current: false,
            generation: 1,
            hidden: Cell::new(true),
            view,
            main_thread,
        })
    }

    /// True means a completed frame matches this clip and the native layer is visible.
    pub fn update(
        &mut self,
        bounds: Bounds<Pixels>,
        layout: &Layout,
        readings: &Readings,
        window: &Window,
    ) -> Result<bool, String> {
        assert_main_thread(self.main_thread);
        let result = self.update_native(bounds, layout, readings, window);
        if result.is_err() {
            self.suspend();
        }

        result
    }

    fn update_native(
        &mut self,
        bounds: Bounds<Pixels>,
        layout: &Layout,
        readings: &Readings,
        window: &Window,
    ) -> Result<bool, String> {
        if window_view(window)? != self.view {
            return Err("Hardware animation cannot move between native windows".into());
        }

        if !window.is_visible() {
            self.suspend();
            return Ok(false);
        }

        if !window.is_window_active() {
            self.suspend();
            return Ok(false);
        }

        if !geometry::create(
            &mut self.geometry_pending,
            bounds,
            layout,
            window,
            self.extent,
        )? {
            self.suspend();
            return Ok(false);
        }

        let changed = if self.geometry_current {
            self.geometry != self.geometry_pending
        } else {
            true
        };
        if changed {
            self.set_hidden(true);
            self.next_generation()?;
            self.update_geometry(&self.geometry_pending);
            self.geometry.copy_from(&self.geometry_pending);
            self.geometry_current = true;
        }

        self.update_request(readings)?;

        let ready = self.status()?;
        self.set_hidden(!ready);

        Ok(ready)
    }

    /// Read-only readiness poll. Notify once on a change; `update()` reveals the layer.
    pub fn status(&self) -> Result<bool, String> {
        assert_main_thread(self.main_thread);
        if let Some(error) = self.worker.error()? {
            return Err(error);
        }

        let ready = self.request_current && self.worker.presented_generation() == self.generation;
        Ok(ready)
    }

    fn update_request(&mut self, readings: &Readings) -> Result<(), String> {
        self.request_pending.frame.copy_from(&self.geometry.frame);
        self.request_pending.input = motion::Input::new(readings);
        self.request_pending.generation = self.generation;
        let changed = if self.request_current {
            self.request != self.request_pending
        } else {
            true
        };
        if changed {
            self.worker.publish(Some(&self.request_pending))?;
            self.request.copy_from(&self.request_pending);
            self.request_current = true;
        }

        Ok(())
    }

    pub fn statistics(&self) -> Statistics {
        self.worker.statistics()
    }

    pub fn suspend(&mut self) {
        assert_main_thread(self.main_thread);
        self.set_hidden(true);
        if self.request_current {
            self.request_current = false;
            if let Err(error) = self.worker.publish(None) {
                log::error!("Suspending hardware animation: {error}");
            }
        }
        self.geometry_current = false;
    }

    fn next_generation(&mut self) -> Result<(), String> {
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or("Hardware animation generation exhausted")?;
        Ok(())
    }

    fn update_geometry(&self, geometry: &geometry::Geometry) {
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        let layer = native_layer(&self.layer);
        layer.setFrame(geometry.native);
        layer.setDrawableSize(geometry.drawable);
        layer.setContentsScale(f64::from(geometry.frame.backing_scale));
        CATransaction::commit();
    }

    fn set_hidden(&self, hidden: bool) {
        if self.hidden.replace(hidden) != hidden {
            CATransaction::begin();
            CATransaction::setDisableActions(true);
            native_layer(&self.layer).setHidden(hidden);
            CATransaction::commit();
        }
    }
}

impl Drop for HardwareAnimation {
    fn drop(&mut self) {
        assert_main_thread(self.main_thread);
        CATransaction::begin();
        CATransaction::setDisableActions(true);
        native_layer(&self.layer).setHidden(true);
        native_layer(&self.layer).removeFromSuperlayer();
        CATransaction::commit();
        self.worker.stop();
    }
}

fn new_layer(layer: &metal::MetalLayerRef, device: &metal::DeviceRef) {
    layer.set_device(device);
    layer.set_pixel_format(metal::MTLPixelFormat::BGRA8Unorm);
    layer.set_framebuffer_only(true);
    layer.set_presents_with_transaction(false);
    layer.set_display_sync_enabled(true);
    layer.set_maximum_drawable_count(2);
    let layer = native_layer(layer);
    layer.setAllowsNextDrawableTimeout(true);
    layer.setOpaque(false);
    layer.setMasksToBounds(true);
    layer.setHidden(true);
}

fn window_view(window: &Window) -> Result<std::ptr::NonNull<NSView>, String> {
    let handle = HasWindowHandle::window_handle(window).map_err(|error| error.to_string())?;
    let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
        return Err("Hardware animation requires an AppKit window".into());
    };

    Ok(handle.ns_view.cast())
}

fn native_layer(layer: &metal::MetalLayerRef) -> &CAMetalLayer {
    // Both wrappers bind CAMetalLayer; metal-rs retains the object for this entire borrow.
    unsafe { &*layer.as_ptr().cast::<CAMetalLayer>() }
}

fn assert_main_thread(_main_thread: MainThreadMarker) {
    assert!(MainThreadMarker::new().is_some());
}
