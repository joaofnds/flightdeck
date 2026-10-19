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
    /// The line being written, kept until its newline arrives.
    line: Vec<u8>,
    /// Whether the line being written outgrew its bound or lost bytes, so it is not kept.
    line_damaged: bool,
    lines: VecDeque<String>,
}

const TAIL_BYTES_MAX: usize = 16_384;
const FILE_BYTES_MAX: u64 = 1_048_576;
const CREATE_ATTEMPTS_MAX: u16 = 64;
const READ_ATTEMPTS_MAX: u16 = 32;
/// The collector's longest report line is about 600 bytes.
const LINE_BYTES_MAX: usize = 4_096;
/// Lines kept between two reads, a minute's reports many times over.
const LINES_MAX: usize = 256;
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
            line: Vec::with_capacity(LINE_BYTES_MAX),
            line_damaged: false,
            lines: VecDeque::with_capacity(LINES_MAX),
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
            self.0.line_damaged = true;
        }
        let skipped_to = length.saturating_sub(tail_bytes);
        if self.0.position < skipped_to {
            self.0.position = skipped_to;
            self.0.line_damaged = true;
        }

        self.drain_tail(length)?;
        if length > FILE_BYTES_MAX {
            // Writers retain O_APPEND, so truncation cannot create a sparse output file.
            // Diagnostic bytes appended concurrently with this truncation may be omitted.
            self.0.file.set_len(0)?;
            self.0.position = 0;
            if !self.0.line.is_empty() {
                self.0.line_damaged = true;
            }
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
                    for index in 0..count {
                        let byte = self.0.buffer[index];
                        if self.0.tail.len() == TAIL_BYTES_MAX {
                            assert!(self.0.tail.pop_front().is_some());
                        }
                        self.0.tail.push_back(byte);
                        self.0.assemble(byte);
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

    /// The whole lines read since the last call, oldest first. A line that lost bytes to a
    /// skip or a truncation, or outgrew its bound, is left out.
    pub fn take_lines(&mut self) -> VecDeque<String> {
        std::mem::replace(&mut self.0.lines, VecDeque::with_capacity(LINES_MAX))
    }

    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.0.tail.iter().copied().collect::<Vec<_>>()).into_owned()
    }

    pub fn peak_bytes(&self) -> u64 {
        self.0.peak_bytes
    }
}

impl Storage {
    fn assemble(&mut self, byte: u8) {
        if byte != b'\n' {
            if self.line.len() < LINE_BYTES_MAX {
                self.line.push(byte);
            } else {
                self.line_damaged = true;
            }
            return;
        }

        if !self.line_damaged {
            if self.lines.len() == LINES_MAX {
                assert!(self.lines.pop_front().is_some());
            }
            self.lines
                .push_back(String::from_utf8_lossy(&self.line).into_owned());
        }
        self.line.clear();
        self.line_damaged = false;
        assert!(self.lines.len() <= LINES_MAX);
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
