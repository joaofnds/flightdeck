use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::unix::fs::{FileExt, OpenOptionsExt};
use std::sync::atomic::{AtomicU64, Ordering};

pub(super) struct Output(Box<Storage>);

struct Storage {
    file: File,
    tail: VecDeque<u8>,
    buffer: Box<[u8]>,
    position: u64,
    peak_bytes: u64,
}

const TAIL_BYTES_MAX: usize = 16_384;
const FILE_BYTES_MAX: u64 = 1_048_576;
const CREATE_ATTEMPTS_MAX: u16 = 64;
const READ_ATTEMPTS_MAX: u16 = 32;
static NEXT_FILE: AtomicU64 = AtomicU64::new(0);
const _: () = {
    assert!(TAIL_BYTES_MAX <= 1_048_576);
    assert!(size_of::<Output>() <= 16);
};

impl Output {
    pub fn new() -> io::Result<Self> {
        let file = create_file()?;

        Ok(Self(Box::new(Storage {
            file,
            tail: VecDeque::with_capacity(TAIL_BYTES_MAX),
            buffer: vec![0; TAIL_BYTES_MAX].into_boxed_slice(),
            position: 0,
            peak_bytes: 0,
        })))
    }

    pub fn writer(&self) -> io::Result<File> {
        self.0.file.try_clone()
    }

    pub fn drain(&mut self) -> io::Result<()> {
        let length = self.0.file.metadata()?.len();
        self.0.peak_bytes = self.0.peak_bytes.max(length);
        let tail_bytes = u64::try_from(TAIL_BYTES_MAX).map_err(io::Error::other)?;
        if self.0.position > length {
            self.0.position = 0;
        }
        self.0.position = self.0.position.max(length.saturating_sub(tail_bytes));

        self.drain_tail(length)?;
        if length > FILE_BYTES_MAX {
            // Writers retain O_APPEND, so truncation cannot create a sparse output file.
            // Diagnostic bytes appended concurrently with this truncation may be omitted.
            self.0.file.set_len(0)?;
            self.0.position = 0;
        }
        Ok(())
    }

    fn drain_tail(&mut self, length: u64) -> io::Result<()> {
        for _attempt in 0..READ_ATTEMPTS_MAX {
            if self.0.position == length {
                return Ok(());
            }

            let remaining = usize::try_from(length - self.0.position).map_err(io::Error::other)?;
            let size = remaining.min(self.0.buffer.len());
            match self
                .0
                .file
                .read_at(&mut self.0.buffer[..size], self.0.position)
            {
                Ok(0) => return Ok(()),
                Ok(count) => {
                    self.0.position += u64::try_from(count).map_err(io::Error::other)?;
                    for byte in &self.0.buffer[..count] {
                        if self.0.tail.len() == TAIL_BYTES_MAX {
                            assert!(self.0.tail.pop_front().is_some());
                        }
                        self.0.tail.push_back(*byte);
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(error),
            }
        }

        Err(io::Error::other(
            "Collector diagnostic read retry bound exhausted",
        ))
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.tail.iter().copied().collect::<Vec<_>>()).into_owned()
    }

    pub fn peak_bytes(&self) -> u64 {
        self.0.peak_bytes
    }
}

fn create_file() -> io::Result<File> {
    for _attempt in 0..CREATE_ATTEMPTS_MAX {
        let serial = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("flightdeck-output-{}-{serial}", std::process::id()));
        let opened = OpenOptions::new()
            .read(true)
            .append(true)
            .create_new(true)
            .mode(0o600)
            .open(&path);
        let file = match opened {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        };

        // The child owns an inode, not a pathname requiring cleanup after parent death.
        std::fs::remove_file(path)?;
        return Ok(file);
    }

    Err(io::Error::other(
        "Collector diagnostic file creation bound exhausted",
    ))
}

#[cfg(test)]
#[path = "collector_lifecycle_diagnostics_tests.rs"]
mod tests;
