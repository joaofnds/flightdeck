use std::collections::VecDeque;
use std::fs::File;
use std::io::{self, Read};
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::net::UnixStream;

pub(super) struct Output(Box<Storage>);

struct Storage {
    reader: UnixStream,
    writer: UnixStream,
    tail: VecDeque<u8>,
    buffer: Box<[u8]>,
    peak_bytes: u64,
    /// The line being written, kept until its newline arrives.
    line: Vec<u8>,
    /// Whether the line being written outgrew its bound or lost bytes, so it is not kept.
    line_damaged: bool,
    discontinuous: bool,
    lines: VecDeque<String>,
}

const TAIL_BYTES_MAX: usize = 16_384;
const READ_ATTEMPTS_MAX: u16 = 32;
const SOCKET_BYTES: libc::c_int = 65_536;
/// The collector's longest report line is about 600 bytes.
const LINE_BYTES_MAX: usize = 4_096;
/// Lines kept between two reads, a minute's reports many times over.
const LINES_MAX: usize = 256;
const _: () = {
    assert!(TAIL_BYTES_MAX <= 1_048_576);
    assert!(size_of::<Output>() <= 16);
};

impl Output {
    pub fn new() -> io::Result<Self> {
        let (reader, writer) = UnixStream::pair()?;
        reader.set_nonblocking(true)?;
        // A detached GUI closes its reader. Helpers must receive EPIPE rather than SIGPIPE.
        let enabled: libc::c_int = 1;
        let status = unsafe {
            libc::setsockopt(
                writer.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_NOSIGPIPE,
                std::ptr::from_ref(&enabled).cast(),
                libc::socklen_t::try_from(size_of_val(&enabled)).map_err(io::Error::other)?,
            )
        };
        if status != 0 {
            return Err(io::Error::last_os_error());
        }
        for (stream, option) in [(&writer, libc::SO_SNDBUF), (&reader, libc::SO_RCVBUF)] {
            let status = unsafe {
                libc::setsockopt(
                    stream.as_raw_fd(),
                    libc::SOL_SOCKET,
                    option,
                    std::ptr::from_ref(&SOCKET_BYTES).cast(),
                    libc::socklen_t::try_from(size_of_val(&SOCKET_BYTES))
                        .map_err(io::Error::other)?,
                )
            };
            if status != 0 {
                return Err(io::Error::last_os_error());
            }
        }

        Ok(Self(Box::new(Storage {
            reader,
            writer,
            tail: VecDeque::with_capacity(TAIL_BYTES_MAX),
            buffer: vec![0; TAIL_BYTES_MAX].into_boxed_slice(),
            peak_bytes: 0,
            line: Vec::with_capacity(LINE_BYTES_MAX),
            line_damaged: false,
            discontinuous: false,
            lines: VecDeque::with_capacity(LINES_MAX),
        })))
    }

    pub fn writer(&self) -> io::Result<File> {
        self.0.writer.try_clone().map(OwnedFd::from).map(File::from)
    }

    pub fn drain(&mut self) -> io::Result<()> {
        self.drain_tail()?;
        if self.0.discontinuous {
            self.0.lines.clear();
            if !self.0.line.is_empty() {
                self.0.line.clear();
                self.0.line_damaged = true;
            }
        }
        Ok(())
    }

    fn drain_tail(&mut self) -> io::Result<()> {
        let mut bytes = 0_u64;
        for _attempt in 0..READ_ATTEMPTS_MAX {
            match self.0.reader.read(&mut self.0.buffer) {
                Ok(0) => break,
                Ok(count) => {
                    bytes += u64::try_from(count).map_err(io::Error::other)?;
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
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
        }
        self.0.peak_bytes = self.0.peak_bytes.max(bytes);
        Ok(())
    }

    /// Whole lines in order, or none when a discontinuity invalidated the entire batch.
    pub fn take_lines(&mut self) -> Option<VecDeque<String>> {
        if std::mem::take(&mut self.0.discontinuous) {
            assert!(self.0.lines.is_empty());
            return None;
        }

        Some(std::mem::replace(
            &mut self.0.lines,
            VecDeque::with_capacity(LINES_MAX),
        ))
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
                self.discontinuous = true;
            }
            return;
        }

        if !self.line_damaged {
            if self.lines.len() == LINES_MAX {
                assert!(self.lines.pop_front().is_some());
                self.discontinuous = true;
            }
            match std::str::from_utf8(&self.line) {
                Ok(line) => self.lines.push_back(line.to_owned()),
                Err(_) => self.discontinuous = true,
            }
        }
        self.line.clear();
        self.line_damaged = false;
        assert!(self.lines.len() <= LINES_MAX);
    }
}

#[cfg(test)]
#[path = "collector_lifecycle_diagnostics_tests.rs"]
mod tests;
