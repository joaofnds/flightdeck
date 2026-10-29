use std::fs::File;
use std::io::{self, Write};
use std::os::fd::FromRawFd;
use std::sync::atomic::Ordering;

use super::{Group, Reporter, Stream, WRITER_STACK_BYTES, increment};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct LossSnapshot {
    queue_full: u64,
    staging_contention: u64,
    queue_contention: u64,
    oversize: u64,
    failed_delivery: u64,
    setup_status: i32,
    counts_inexact: bool,
}

struct Summary {
    data: [u8; 512],
    length: usize,
}

const F_DUPFD_CLOEXEC: i32 = 67;
// A setup failure without an OS code remains distinguishable from successful setup (zero).
const SETUP_STATUS_UNAVAILABLE: i32 = -1;

unsafe extern "C" {
    fn fcntl(descriptor: i32, command: i32, ...) -> i32;
}

pub(super) fn start(reporter: &'static Reporter) {
    let streams = [
        duplicate(reporter, Stream::Stdout, 1),
        duplicate(reporter, Stream::Stderr, 2),
    ];
    if streams.iter().all(Option::is_none) {
        reporter.stopped.store(true, Ordering::Release);
        return;
    }
    let scratch = Box::new(Group::empty());
    let thread = std::thread::Builder::new()
        .name("diagnostic-output".to_owned())
        .stack_size(WRITER_STACK_BYTES)
        .spawn(move || {
            std::thread::park_timeout(std::time::Duration::ZERO);
            reporter.ready.wait();
            run(reporter, streams, scratch);
        });

    start_complete(reporter, thread);
}

fn start_complete(reporter: &Reporter, thread: io::Result<std::thread::JoinHandle<()>>) {
    match thread {
        Ok(handle) => {
            assert!(reporter.thread.set(handle.thread().clone()).is_ok());
            reporter.ready.wait();
            drop(handle);
        }
        Err(error) => {
            let status = error.raw_os_error().unwrap_or(SETUP_STATUS_UNAVAILABLE);
            for stream in [Stream::Stdout, Stream::Stderr] {
                reporter.enabled[stream.index()].store(false, Ordering::Relaxed);
                reporter.loss[stream.index()]
                    .setup_status
                    .store(status, Ordering::Relaxed);
            }
            reporter.stopped.store(true, Ordering::Release);
        }
    }
}

fn duplicate(reporter: &Reporter, stream: Stream, descriptor: i32) -> Option<File> {
    let duplicated = unsafe { fcntl(descriptor, F_DUPFD_CLOEXEC, 3) };

    if duplicated >= 0 {
        Some(unsafe { File::from_raw_fd(duplicated) })
    } else {
        reporter.enabled[stream.index()].store(false, Ordering::Relaxed);
        let status = io::Error::last_os_error()
            .raw_os_error()
            .unwrap_or(SETUP_STATUS_UNAVAILABLE);
        reporter.loss[stream.index()]
            .setup_status
            .store(status, Ordering::Relaxed);

        None
    }
}

fn run(reporter: &Reporter, mut streams: [Option<File>; 2], mut scratch: Box<Group>) {
    let mut delivered = [LossSnapshot::default(); 2];

    for _step in 0..u64::MAX {
        // Read before the take, so a group admitted before exit set stopping is still taken.
        let stopping = reporter.stopping.load(Ordering::Acquire);
        if let Some(stream) = reporter.take(|group| scratch.copy_from(group)) {
            if reporter.enabled[stream.index()].load(Ordering::Relaxed) {
                summaries(reporter, &mut streams, stream, &mut delivered);
                deliver(reporter, &mut streams, stream, scratch.bytes());
            } else {
                let loss = &reporter.loss[stream.index()];
                increment(&loss.failed_delivery, &loss.counts_inexact);
            }
        } else {
            for stream in [Stream::Stdout, Stream::Stderr] {
                if reporter.enabled[stream.index()].load(Ordering::Relaxed) {
                    summaries(reporter, &mut streams, stream, &mut delivered);
                }
            }
            if stopping {
                reporter.stopped.store(true, Ordering::Release);
                return;
            }
            std::thread::park();
        }
    }
    unreachable!("diagnostic writer exhausted its process-lifetime operation bound");
}

fn summaries(
    reporter: &Reporter,
    streams: &mut [Option<File>; 2],
    destination: Stream,
    delivered: &mut [LossSnapshot; 2],
) {
    for affected in [Stream::Stdout, Stream::Stderr] {
        if affected != destination {
            if reporter.enabled[affected.index()].load(Ordering::Relaxed) {
                continue;
            }
        }
        let snapshot = LossSnapshot::take(reporter, affected);
        if snapshot != delivered[affected.index()] {
            let mut summary = Summary {
                data: [0; 512],
                length: 0,
            };
            let role = match reporter.role.load(Ordering::Relaxed) {
                0 => "collector",
                1 => "persister",
                _ => unreachable!("reporter role is installed before writer startup"),
            };
            summary.format(role, affected, &snapshot);
            if deliver(
                reporter,
                streams,
                destination,
                &summary.data[..summary.length],
            ) {
                delivered[affected.index()] = snapshot;
            }
        }
    }
}

fn deliver(
    reporter: &Reporter,
    streams: &mut [Option<File>; 2],
    stream: Stream,
    bytes: &[u8],
) -> bool {
    if reporter.enabled[stream.index()].load(Ordering::Relaxed) {
        let Some(output) = &mut streams[stream.index()] else {
            unreachable!("enabled stream owns its duplicated descriptor");
        };
        if write_bytes(output, bytes).is_ok() {
            return true;
        }
        reporter.enabled[stream.index()].store(false, Ordering::Relaxed);
    }
    let loss = &reporter.loss[stream.index()];
    increment(&loss.failed_delivery, &loss.counts_inexact);

    false
}

fn write_bytes(output: &mut impl Write, bytes: &[u8]) -> io::Result<()> {
    let mut position = 0;
    let mut interruptions = 0;

    for _attempt in 0..bytes.len() + 8 {
        if position == bytes.len() {
            return Ok(());
        }
        match output.write(&bytes[position..]) {
            Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
            Ok(count) => {
                assert!(count <= bytes.len() - position);
                position += count;
            }
            Err(error) => {
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
                interruptions += 1;
                if interruptions == 8 {
                    return Err(error);
                }
            }
        }
    }
    assert_eq!(position, bytes.len());

    Ok(())
}

impl LossSnapshot {
    fn take(reporter: &Reporter, stream: Stream) -> Self {
        let loss = &reporter.loss[stream.index()];

        Self {
            queue_full: loss.queue_full.load(Ordering::Relaxed),
            staging_contention: loss.staging_contention.load(Ordering::Relaxed),
            queue_contention: loss.queue_contention.load(Ordering::Relaxed),
            oversize: loss.oversize.load(Ordering::Relaxed),
            failed_delivery: loss.failed_delivery.load(Ordering::Relaxed),
            setup_status: loss.setup_status.load(Ordering::Relaxed),
            counts_inexact: loss.counts_inexact.load(Ordering::Relaxed),
        }
    }
}

impl Summary {
    fn format(&mut self, role: &str, affected: Stream, loss: &LossSnapshot) {
        let result = writeln!(
            self,
            "diagnostic loss role={role} stream={} queue_full={} staging_contention={} \
             queue_contention={} oversize={} failed_delivery={} setup_status={} counts_inexact={}",
            affected.name(),
            loss.queue_full,
            loss.staging_contention,
            loss.queue_contention,
            loss.oversize,
            loss.failed_delivery,
            loss.setup_status,
            u8::from(loss.counts_inexact),
        );

        assert!(result.is_ok());
    }
}

impl Write for Summary {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        assert!(self.length <= self.data.len());
        assert!(bytes.len() <= self.data.len() - self.length);
        let end = self.length + bytes.len();
        self.data[self.length..end].copy_from_slice(bytes);
        self.length = end;

        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
