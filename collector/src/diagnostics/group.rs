use std::fmt;
use std::io::{self, Write};

use super::{GROUP_BYTES, REPORTER, Stream};

/// One complete stdout group. Output rejection never becomes a collection error.
pub struct Group {
    data: [u8; GROUP_BYTES],
    pub(super) length: usize,
    pub(super) oversized: bool,
}

impl Group {
    pub(super) const fn empty() -> Self {
        Self {
            data: [0; GROUP_BYTES],
            length: 0,
            oversized: false,
        }
    }

    /// Starts a new complete group, discarding any previously staged bytes.
    pub fn begin(&mut self) {
        self.length = 0;
        self.oversized = false;
    }

    /// Attempts whole-group admission without waiting for the writer or other producers.
    pub fn submit(&mut self) {
        REPORTER.admit(Stream::Stdout, self);
        self.begin();
    }

    pub(super) fn bytes(&self) -> &[u8] {
        assert!(self.length <= GROUP_BYTES);
        &self.data[..self.length]
    }

    pub(super) fn copy_from(&mut self, source: &Self) {
        assert!(!source.oversized);
        self.data[..source.length].copy_from_slice(source.bytes());
        self.length = source.length;
        self.oversized = false;
    }
}

impl Write for Group {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        assert!(self.length <= GROUP_BYTES);
        if bytes.len() > GROUP_BYTES - self.length {
            self.oversized = true;
        } else {
            if !self.oversized {
                let end = self.length + bytes.len();
                self.data[self.length..end].copy_from_slice(bytes);
                self.length = end;
            }
        }

        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl fmt::Write for Group {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        if self.write_all(text.as_bytes()).is_err() {
            unreachable!("bounded staging absorbs byte-write errors");
        }

        Ok(())
    }
}
