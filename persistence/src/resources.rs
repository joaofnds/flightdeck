//! Whole-process native allocation observation, independent of `DuckDB`'s buffer manager.
mod native;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

pub const WORKING_BYTES_MAX: u64 = 2 * 1024 * 1024 * 1024;

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
    thread: Option<JoinHandle<()>>,
}

impl Footprint {
    pub fn read() -> io::Result<Self> {
        native::read()
    }
}

impl Watchdog {
    pub fn start(limit_bytes: u64) -> io::Result<Self> {
        if limit_bytes == 0 {
            return Err(io::Error::from(io::ErrorKind::InvalidInput));
        }
        let stop = Arc::new(AtomicBool::new(false));
        let watched = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("persistence-memory".into())
            .spawn(move || {
                for _poll in 0..u64::MAX {
                    if watched.load(Ordering::Acquire) {
                        return;
                    }
                    match Footprint::read() {
                        Ok(memory) => {
                            let charged_bytes = memory.current_bytes.max(memory.lifetime_bytes_max);
                            if charged_bytes > limit_bytes {
                                eprintln!(
                                    "persister: memory limit current={} peak={} limit={}",
                                    memory.current_bytes, memory.lifetime_bytes_max, limit_bytes
                                );
                                std::process::exit(74);
                            }
                        }
                        Err(error) => {
                            eprintln!("persister: memory observation failed: {error}");
                            std::process::exit(74);
                        }
                    }
                    thread::sleep(Duration::from_millis(100));
                }
                unreachable!("watchdog polling bound exhausted");
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
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

/// Evaluates the observed completed-window footprints after the warmup period.
#[must_use]
pub fn stable_growth(footprints: &[u64]) -> bool {
    if footprints.len() != 64 {
        return false;
    }
    let first = footprints[0];
    let Some(last) = footprints.last() else {
        return false;
    };
    if last.saturating_sub(first) > 63 * 128 * 1024 {
        return false;
    }
    let minimum = footprints.iter().min();
    let maximum = footprints.iter().max();
    match (minimum, maximum) {
        (Some(minimum), Some(maximum)) => maximum.saturating_sub(*minimum) <= 8 * 1024 * 1024,
        _ => false,
    }
}
