//! The GPU time of each process, from the GPU driver's clients in the I/O Registry. Each
//! process that submits GPU work opens clients, and each client names its process and counts
//! the GPU time its work took, in nanoseconds, since the client opened.

use std::io;

use crate::registry::{Entry, Key, PROPERTY_BYTES_MAX};

/// The most GPU clients read, past the hundred or so a busy Mac opens.
pub const CLIENTS_MAX: usize = 4096;

/// The GPU driver's entry and the property names a reading looks up, made once.
pub struct GpuClients {
    accelerator: Entry,
    usage: Key,
    time: Key,
}

/// The GPU time one process's open clients counted, in nanoseconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GpuClientTime {
    pub pid: i32,
    pub gpu_ns: u64,
}

impl GpuClients {
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            accelerator: Entry::of_class(c"AGXAccelerator", "GPU accelerator")?,
            usage: Key::new("AppUsage")?,
            time: Key::new("accumulatedGPUTime")?,
        })
    }

    /// Replaces `times` with each process's GPU time summed across its open clients, by pid.
    /// A client that names no process, or whose time is missing, is left out. A client that
    /// closed took its time with it, so a process's time can fall between readings. Each
    /// reading allocates every client's usage array, which the I/O Registry gives only as a
    /// CoreFoundation property, so a caller reads it at most once a second.
    pub fn read(&self, times: &mut Vec<GpuClientTime>) -> io::Result<()> {
        assert!(times.capacity() >= CLIENTS_MAX);

        times.clear();
        let mut creator = [0_u8; PROPERTY_BYTES_MAX];
        let limit = CLIENTS_MAX;
        self.accelerator
            .each_child_of_class(c"AGXDeviceUserClient", limit, |client| {
                let Some(count) = client.bytes(c"IOUserClientCreator", &mut creator) else {
                    return Ok(());
                };
                let Some(pid) = creator_pid(&creator[..count]) else {
                    return Ok(());
                };
                let Some(gpu_ns) = client.array_sum(&self.usage, &self.time) else {
                    return Ok(());
                };
                times.push(GpuClientTime { pid, gpu_ns });
                Ok(())
            })?;

        times.sort_unstable_by_key(|time| time.pid);
        times.dedup_by(|later, earlier| {
            if later.pid != earlier.pid {
                return false;
            }
            earlier.gpu_ns = earlier.gpu_ns.saturating_add(later.gpu_ns);
            true
        });
        assert!(times.len() <= CLIENTS_MAX);
        Ok(())
    }
}

/// The pid of a client creator text such as `pid 426, WindowServer`, NUL-terminated.
fn creator_pid(creator: &[u8]) -> Option<i32> {
    let text = creator.split(|byte| *byte == 0).next()?;
    let text = text.strip_prefix(b"pid ")?;
    let digits = text.split(|byte| *byte == b',').next()?;
    let pid: i32 = std::str::from_utf8(digits).ok()?.parse().ok()?;
    if pid <= 0 {
        return None;
    }
    Some(pid)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creator_pid_reads_the_pid_of_the_creator_text() {
        // Goal: the pid comes from the creator text, and a text of another shape names none.
        // Method: parse the shape macOS writes, then malformed and nonpositive ones.
        assert_eq!(creator_pid(b"pid 426, WindowServer\0\0"), Some(426));
        assert_eq!(creator_pid(b"pid 7, a\0"), Some(7));
        assert_eq!(creator_pid(b"426, WindowServer\0"), None);
        assert_eq!(creator_pid(b"pid x, WindowServer\0"), None);
        assert_eq!(creator_pid(b"pid 0, kernel\0"), None);
        assert_eq!(creator_pid(b"\0"), None);
    }

    #[test]
    fn reads_each_process_once_with_some_gpu_time() {
        // Goal: each pid appears once, and some process has GPU time, since the window server
        // draws every frame through the GPU.
        // Method: read this Mac's GPU clients once.
        let clients = GpuClients::new().unwrap();
        let mut times = Vec::with_capacity(CLIENTS_MAX);

        clients.read(&mut times).unwrap();

        assert!(times.windows(2).all(|pair| pair[0].pid < pair[1].pid));
        assert!(times.iter().any(|time| time.gpu_ns > 0));
    }
}
