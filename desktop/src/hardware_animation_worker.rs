use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use foreign_types::ForeignTypeRef;
use objc2::rc::{Retained, autoreleasepool};
use objc2::runtime::AnyObject;

use super::data::Resources;
use super::geometry::Frame;
use super::motion::{Input, Motion};

pub(super) struct Owner {
    shared: Arc<Shared>,
    wake: Option<SyncSender<()>>,
    thread: Option<JoinHandle<()>>,
}

struct Shared {
    latest: Mutex<Mailbox>,
    revision: AtomicU64,
    presented: AtomicU64,
    stopped: AtomicBool,
    error: Mutex<Option<String>>,
    telemetry: Telemetry,
}

#[derive(Default)]
struct Mailbox {
    request: Request,
    occupied: bool,
}

struct Telemetry {
    started: Instant,
    submitted: AtomicU64,
    completed: AtomicU64,
    completion_ms: AtomicU64,
    angles: [AtomicU32; 2],
    rpm: [AtomicU32; 2],
}

#[derive(Clone, Copy, Default, PartialEq)]
pub(super) struct Request {
    pub frame: Frame,
    pub input: Input,
    pub generation: u64,
}

impl Request {
    pub fn copy_from(&mut self, source: &Self) {
        self.frame.copy_from(&source.frame);
        self.input = source.input;
        self.generation = source.generation;
    }
}

struct Renderer {
    layer: metal::MetalLayer,
    resources: Resources,
    motion: [Motion; 2],
    frame: Frame,
    last_step: Option<Instant>,
    last_revision: u64,
    last_generation: u64,
    deadline: Instant,
}

const FRAME_INTERVAL: Duration = Duration::from_nanos(33_333_334);
const COMMAND_TIMEOUT: Duration = Duration::from_secs(2);
const ERROR_BYTES_MAX: usize = 2048;

impl Owner {
    pub fn new(layer: metal::MetalLayer, resources: Resources) -> Result<Self, String> {
        let shared = Arc::new(Shared {
            latest: Mutex::new(Mailbox::default()),
            revision: AtomicU64::new(0),
            presented: AtomicU64::new(0),
            stopped: AtomicBool::new(false),
            error: Mutex::new(None),
            telemetry: Telemetry {
                started: Instant::now(),
                submitted: AtomicU64::new(0),
                completed: AtomicU64::new(0),
                completion_ms: AtomicU64::new(0),
                angles: std::array::from_fn(|_| AtomicU32::new(0)),
                rpm: std::array::from_fn(|_| AtomicU32::new(0)),
            },
        });
        let (wake, receiver) = sync_channel(1);
        let worker_shared = Arc::clone(&shared);
        let thread = std::thread::Builder::new()
            .name("hardware-animation".into())
            .spawn(move || {
                let mut renderer = Renderer {
                    layer,
                    resources,
                    motion: [Motion::default(); 2],
                    frame: Frame::default(),
                    last_step: None,
                    last_revision: 0,
                    last_generation: 0,
                    deadline: Instant::now(),
                };
                let result = renderer.run(&worker_shared, &receiver);
                if let Err(error) = result {
                    log::error!("Hardware animation stopped: {error}");
                    match worker_shared.error.lock() {
                        Ok(mut slot) => *slot = Some(bound_error(error)),
                        Err(error) => log::error!("Hardware animation error lock: {error}"),
                    }
                }
                worker_shared.stopped.store(true, Ordering::Release);
            })
            .map_err(|error| format!("Starting hardware animation worker: {error}"))?;

        Ok(Self {
            shared,
            wake: Some(wake),
            thread: Some(thread),
        })
    }

    pub fn publish(&self, request: Option<&Request>) -> Result<(), String> {
        if self.shared.stopped.load(Ordering::Acquire) {
            return Err("Hardware animation worker has stopped".into());
        }

        let mut slot = self
            .shared
            .latest
            .lock()
            .map_err(|error| error.to_string())?;
        let revision = self
            .shared
            .revision
            .load(Ordering::Relaxed)
            .checked_add(1)
            .ok_or("Hardware animation mailbox revision exhausted")?;
        match request {
            Some(request) => {
                slot.request.copy_from(request);
                slot.occupied = true;
            }
            None => slot.occupied = false,
        }
        self.shared.revision.store(revision, Ordering::Release);
        drop(slot);
        let wake = self
            .wake
            .as_ref()
            .ok_or("Hardware animation worker is stopping")?;
        match wake.try_send(()) {
            Ok(()) | Err(TrySendError::Full(())) => Ok(()),
            Err(TrySendError::Disconnected(())) => Err("Hardware animation worker exited".into()),
        }
    }

    pub fn presented_generation(&self) -> u64 {
        self.shared.presented.load(Ordering::Acquire)
    }

    pub fn statistics(&self) -> super::Statistics {
        let telemetry = &self.shared.telemetry;
        let completed = telemetry.completed.load(Ordering::Acquire);
        super::Statistics {
            submitted: telemetry.submitted.load(Ordering::Acquire),
            completed,
            completion_age_ms: if completed > 0 {
                Some(
                    elapsed_ms(telemetry.started)
                        .saturating_sub(telemetry.completion_ms.load(Ordering::Relaxed)),
                )
            } else {
                None
            },
            angles: std::array::from_fn(|index| {
                f32::from_bits(telemetry.angles[index].load(Ordering::Relaxed))
            }),
            rpm: std::array::from_fn(|index| {
                f32::from_bits(telemetry.rpm[index].load(Ordering::Relaxed))
            }),
        }
    }

    pub fn error(&self) -> Result<Option<String>, String> {
        let error = self
            .shared
            .error
            .lock()
            .map_err(|error| error.to_string())?
            .clone();
        if error.is_some() {
            return Ok(error);
        }

        if self.shared.stopped.load(Ordering::Acquire) {
            return Ok(Some("Hardware animation worker has exited".into()));
        }

        Ok(None)
    }

    pub fn stop(&mut self) {
        self.shared.stopped.store(true, Ordering::Release);
        // Disconnect wakes recv without a lock or a join on the main thread.
        self.wake = None;
        if let Some(thread) = self.thread.take() {
            if thread.is_finished() {
                if let Err(error) = thread.join() {
                    log::error!("Hardware animation worker panicked: {error:?}");
                }
            }
        }
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Renderer {
    fn run(&mut self, shared: &Shared, receiver: &Receiver<()>) -> Result<(), String> {
        let mut request = Request::default();
        // Even at 30 Hz this bound is longer than the process's representable useful lifetime.
        for _ in 0..u64::MAX {
            if shared.stopped.load(Ordering::Acquire) {
                return Ok(());
            }

            let slot = shared.latest.lock().map_err(|error| error.to_string())?;
            request.copy_from(&slot.request);
            let occupied = slot.occupied;
            let revision = shared.revision.load(Ordering::Acquire);
            drop(slot);
            if !occupied {
                self.last_step = None;
                self.last_revision = revision;
                if receiver.recv().is_err() {
                    return Ok(());
                }

                continue;
            }

            let changed = revision != self.last_revision;
            if request.generation != self.last_generation {
                self.last_step = None;
                self.last_generation = request.generation;
            }

            let now = Instant::now();
            let scheduled = if changed {
                true
            } else {
                request.input.moving()
            };
            if scheduled {
                if now >= self.deadline {
                    let seconds = self
                        .last_step
                        .map_or(0.0, |last| now.duration_since(last).as_secs_f64().min(1.0));
                    self.last_step = Some(now);
                    let result =
                        autoreleasepool(|_| self.draw(&request, seconds, shared, revision));
                    result?;
                    self.last_revision = revision;
                    // Skip missed deadlines; drawable or GPU stalls never cause a catch-up burst.
                    self.deadline = now + FRAME_INTERVAL;
                    continue;
                }
            }

            let timeout = if scheduled {
                Some(self.deadline.saturating_duration_since(now))
            } else {
                self.last_step = None;
                None
            };
            if !wait(receiver, timeout) {
                return Ok(());
            }
        }

        Err("Hardware animation lifetime frame bound exhausted".into())
    }

    fn draw(
        &mut self,
        request: &Request,
        seconds: f64,
        shared: &Shared,
        revision: u64,
    ) -> Result<(), String> {
        // This may block until the system's drawable timeout. No AppKit or UI work runs here.
        let drawable = self.layer.next_drawable();
        if shared.stopped.load(Ordering::Acquire) {
            return Ok(());
        }

        if shared.revision.load(Ordering::Acquire) != revision {
            return Ok(());
        }

        let drawable = drawable.ok_or("Hardware animation drawable unavailable")?;

        self.frame.copy_from(&request.frame);
        for (index, motion) in self.motion.iter_mut().enumerate() {
            motion.advance(request.input.rpm[index], seconds, &self.resources.lengths);
            motion.frame(
                &mut self.frame.sides[index],
                request.input.rpm[index],
                request.input.color[index],
            );
        }
        let command = draw_encode(drawable, &self.resources, &self.frame)?;
        command.commit();
        shared.telemetry.submitted.fetch_add(1, Ordering::Release);
        if !draw_complete(command, shared)? {
            return Ok(());
        }

        draw_telemetry(shared, &self.frame, &self.motion);
        if !shared.stopped.load(Ordering::Acquire) {
            if shared.revision.load(Ordering::Acquire) == revision {
                shared
                    .presented
                    .store(request.generation, Ordering::Release);
            }
        }

        Ok(())
    }
}

fn draw_encode<'a>(
    drawable: &metal::MetalDrawableRef,
    resources: &'a Resources,
    frame: &Frame,
) -> Result<&'a metal::CommandBufferRef, String> {
    let descriptor = metal::RenderPassDescriptor::new();
    let attachment = descriptor
        .color_attachments()
        .object_at(0)
        .ok_or("Missing hardware animation render attachment")?;
    attachment.set_texture(Some(drawable.texture()));
    attachment.set_load_action(metal::MTLLoadAction::Clear);
    attachment.set_store_action(metal::MTLStoreAction::Store);
    attachment.set_clear_color(metal::MTLClearColor::new(0.0, 0.0, 0.0, 0.0));
    let command = resources.queue.new_command_buffer();
    command.set_label("Flightdeck hardware animation");
    let encoder = command.new_render_command_encoder(descriptor);
    encoder.set_cull_mode(metal::MTLCullMode::None);
    encoder.set_vertex_bytes(0, 576, std::ptr::from_ref(frame).cast());
    encoder.set_fragment_bytes(0, 576, std::ptr::from_ref(frame).cast());
    encoder.set_render_pipeline_state(&resources.rotor_pipeline);
    encoder.set_fragment_texture(0, Some(&resources.texture));
    encoder.draw_primitives_instanced(metal::MTLPrimitiveType::TriangleStrip, 0, 4, 2);
    encoder.set_render_pipeline_state(&resources.particle_pipeline);
    encoder.set_vertex_buffer(1, Some(&resources.routes), 0);
    encoder.set_vertex_buffer(2, Some(&resources.particles), 0);
    encoder.draw_primitives_instanced(metal::MTLPrimitiveType::TriangleStrip, 0, 4, 192);
    encoder.end_encoding();
    command.present_drawable(drawable);

    Ok(command)
}

fn draw_complete(command: &metal::CommandBufferRef, shared: &Shared) -> Result<bool, String> {
    let deadline = Instant::now() + COMMAND_TIMEOUT;
    // Timed waits run only while one command is in flight; idle and suspended workers park.
    for _ in 0..128_u16 {
        match command.status() {
            metal::MTLCommandBufferStatus::Completed => return Ok(true),
            metal::MTLCommandBufferStatus::Error => return Err(command_error(command)),
            _ => {}
        }

        if shared.stopped.load(Ordering::Acquire) {
            return Ok(false);
        }

        if Instant::now() >= deadline {
            return Err("Hardware animation GPU command exceeded two seconds".into());
        }

        std::thread::park_timeout(Duration::from_millis(16));
    }

    Err("Hardware animation GPU completion wait exhausted".into())
}

fn draw_telemetry(shared: &Shared, frame: &Frame, motion: &[Motion; 2]) {
    for (index, side) in frame.sides.iter().enumerate() {
        shared.telemetry.angles[index].store(side.angle.to_bits(), Ordering::Relaxed);
        shared.telemetry.rpm[index].store(motion[index].rpm().to_bits(), Ordering::Relaxed);
    }
    shared
        .telemetry
        .completion_ms
        .store(elapsed_ms(shared.telemetry.started), Ordering::Relaxed);
    shared.telemetry.completed.fetch_add(1, Ordering::Release);
}

fn elapsed_ms(started: Instant) -> u64 {
    let elapsed = started.elapsed();
    elapsed
        .as_secs()
        .saturating_mul(1000)
        .saturating_add(u64::from(elapsed.subsec_millis()))
}

fn wait(receiver: &Receiver<()>, timeout: Option<Duration>) -> bool {
    match timeout {
        Some(timeout) => match receiver.recv_timeout(timeout) {
            Ok(()) | Err(RecvTimeoutError::Timeout) => true,
            Err(RecvTimeoutError::Disconnected) => false,
        },
        None => receiver.recv().is_ok(),
    }
}

fn bound_error(mut error: String) -> String {
    if error.len() > ERROR_BYTES_MAX {
        let mut boundary = ERROR_BYTES_MAX;
        for _ in 0..4 {
            if error.is_char_boundary(boundary) {
                break;
            }
            boundary -= 1;
        }
        error.truncate(boundary);
    }

    error
}

fn command_error(command: &metal::CommandBufferRef) -> String {
    // metal-rs exposes status but not NSError, so borrow its retained Objective-C object.
    let command: &AnyObject = unsafe { &*command.as_ptr().cast::<AnyObject>() };
    let error: Option<Retained<AnyObject>> = unsafe { objc2::msg_send![command, error] };
    match error {
        Some(error) => {
            let description: Retained<objc2_foundation::NSString> =
                unsafe { objc2::msg_send![&*error, localizedDescription] };
            format!("Hardware animation Metal command failed: {description}")
        }
        None => "Hardware animation Metal command failed without NSError".into(),
    }
}
