//! Whole-process native allocation observation, independent of `DuckDB`'s buffer manager.
mod native;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const WORKING_BYTES_MAX: u64 = 2 * 1024 * 1024 * 1024;

pub(crate) fn producer_owner_changed(pid: i32) -> io::Result<bool> {
    native::producer_owner_changed(pid)
}

#[derive(Debug, Clone, Copy)]
pub struct Footprint {
    pub current_bytes: u64,
    pub lifetime_bytes_max: u64,
    pub resident_bytes: u64,
}

/// The thread owns no polling allocation. Exiting terminates the whole worker without
/// returning a successful seal; its parent detects and reaps the failed child.
pub struct Watchdog {
    stop: Arc<AtomicBool>,
    deadline_ms: Arc<AtomicU64>,
    epoch: Instant,
    operation_limit_ms: u64,
    thread: Option<JoinHandle<()>>,
}

pub struct Operation<'watchdog>(&'watchdog AtomicU64);

impl Footprint {
    pub fn read() -> io::Result<Self> {
        native::read()
    }
}

impl Watchdog {
    pub fn start(limit_bytes: u64) -> io::Result<Self> {
        Self::with_operation_limit(limit_bytes, Duration::from_mins(2))
    }

    /// An operation receives the same upper bound as finite collector draining.
    pub fn with_operation_limit(limit_bytes: u64, operation_limit: Duration) -> io::Result<Self> {
        if limit_bytes == 0 {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        if operation_limit.is_zero() {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        if operation_limit > Duration::from_mins(2) {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let stop = Arc::new(AtomicBool::new(false));
        let watched = Arc::clone(&stop);
        let deadline_ms = Arc::new(AtomicU64::new(0));
        let watched_deadline = Arc::clone(&deadline_ms);
        let epoch = Instant::now();
        let operation_limit_ms =
            u64::try_from(operation_limit.as_millis().max(1)).map_err(io::Error::other)?;
        let thread = thread::Builder::new()
            .name("persistence-memory".into())
            .spawn(move || watchdog_poll(&watched, &watched_deadline, epoch, limit_bytes))?;
        Ok(Self {
            stop,
            deadline_ms,
            epoch,
            operation_limit_ms,
            thread: Some(thread),
        })
    }

    pub fn operation(&self) -> io::Result<Operation<'_>> {
        let elapsed = u64::try_from(self.epoch.elapsed().as_millis()).map_err(io::Error::other)?;
        let deadline = elapsed
            .checked_add(self.operation_limit_ms)
            .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
        assert_eq!(self.deadline_ms.swap(deadline, Ordering::AcqRel), 0);
        Ok(Operation(&self.deadline_ms))
    }
}

impl Drop for Operation<'_> {
    fn drop(&mut self) {
        assert!(self.0.swap(0, Ordering::AcqRel) > 0);
    }
}

impl Drop for Watchdog {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            if thread.join().is_err() {
                eprintln!("persister: memory watchdog panicked");
                std::process::abort();
            }
        }
    }
}

fn watchdog_poll(stop: &AtomicBool, deadline_ms: &AtomicU64, epoch: Instant, limit_bytes: u64) {
    for _poll in 0..u64::MAX {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let deadline = deadline_ms.load(Ordering::Acquire);
        if deadline > 0 {
            if epoch.elapsed().as_millis() >= u128::from(deadline) {
                eprintln!("persister: operation exceeded drain deadline; unsealed history at risk");
                native::immediate_exit(75);
            }
        }
        watchdog_memory(limit_bytes);
        thread::sleep(Duration::from_millis(100));
    }
    unreachable!("watchdog polling bound exhausted");
}

fn watchdog_memory(limit_bytes: u64) {
    match Footprint::read() {
        Ok(memory) => {
            let charged_bytes = memory.current_bytes.max(memory.lifetime_bytes_max);
            if charged_bytes > limit_bytes {
                eprintln!(
                    "persister: memory limit current={} peak={} limit={}",
                    memory.current_bytes, memory.lifetime_bytes_max, limit_bytes
                );
                native::immediate_exit(74);
            }
        }
        Err(error) => {
            eprintln!("persister: memory observation failed: {error}");
            native::immediate_exit(74);
        }
    }
}
