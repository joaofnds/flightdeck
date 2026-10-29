use super::super::{INSTALLED, REPORTER, error, exit, stdout_group};
use super::*;

struct ShortWriter {
    data: Vec<u8>,
    interruptions: u8,
    calls: u32,
}

#[test]
fn completes_partial_writes_after_a_bounded_number_of_interruptions() {
    // Goal: complete groups survive short writes and recoverable interrupted syscalls.
    // Method: accept at most two bytes per call after two controlled interruptions.
    let mut output = ShortWriter {
        data: Vec::with_capacity(6),
        interruptions: 2,
        calls: 0,
    };

    write_bytes(&mut output, b"abcdef").unwrap();

    assert_eq!(output.data, b"abcdef");
    assert_eq!(output.calls, 5);
}

#[test]
fn gives_up_after_eight_immediately_interrupted_writes() {
    // Goal: an immediately failing destination cannot spin the writer forever.
    // Method: refuse every write and count the actual adapter calls.
    let mut output = ShortWriter {
        data: Vec::with_capacity(6),
        interruptions: u8::MAX,
        calls: 0,
    };

    let result = write_bytes(&mut output, b"abcdef");

    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
    assert_eq!(output.calls, 8);
    assert!(output.data.is_empty());
}

#[test]
fn keeps_usable_stream_summaries_on_the_affected_stream() {
    // Goal: delivery on stderr cannot hide a required stdout discontinuity record.
    // Method: offer stderr first, then stdout, then disable stdout and observe fallback.
    let reporter = Reporter::new();
    let root = std::env::temp_dir().join(format!("fd-report-summary-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let mut streams = [
        Some(File::create(root.join("stdout")).unwrap()),
        Some(File::create(root.join("stderr")).unwrap()),
    ];
    let mut delivered = [LossSnapshot::default(); 2];
    reporter.loss[0].queue_full.store(1, Ordering::Relaxed);

    summaries(&reporter, &mut streams, Stream::Stderr, &mut delivered);
    assert_eq!(0, std::fs::metadata(root.join("stderr")).unwrap().len());
    summaries(&reporter, &mut streams, Stream::Stdout, &mut delivered);
    reporter.enabled[0].store(false, Ordering::Relaxed);
    reporter.loss[0]
        .counts_inexact
        .store(true, Ordering::Relaxed);
    summaries(&reporter, &mut streams, Stream::Stderr, &mut delivered);

    let stdout = std::fs::read_to_string(root.join("stdout")).unwrap();
    let stderr = std::fs::read_to_string(root.join("stderr")).unwrap();
    assert!(stdout.contains("stream=stdout queue_full=1"));
    assert!(stdout.contains("counts_inexact=0"));
    assert!(stderr.contains("stream=stdout queue_full=1"));
    assert!(stderr.contains("counts_inexact=1"));
    drop(streams);
    std::fs::remove_file(root.join("stdout")).unwrap();
    std::fs::remove_file(root.join("stderr")).unwrap();
    std::fs::remove_dir(root).unwrap();
}

#[test]
fn disables_reporting_when_the_writer_thread_cannot_start() {
    // Goal: a refused writer thread leaves ordinary work independent of diagnostic delivery.
    // Method: refuse the installed spawn result, reject untouched formatting, then exit normally.
    super::super::tests::supervised(
        "diagnostics::writer::tests::disables_reporting_when_the_writer_thread_cannot_start",
        || {
            struct Untouched;
            impl std::fmt::Display for Untouched {
                fn fmt(&self, _output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    panic!("disabled reporting formatted an event");
                }
            }
            REPORTER.prepare();
            assert!(!INSTALLED.swap(true, Ordering::AcqRel));

            start_complete(&REPORTER, Err(io::Error::from_raw_os_error(11)));
            error(format_args!("{Untouched}"));
            let mut group = stdout_group();
            group.write_all(b"ordinary caller continues\n").unwrap();
            group.submit();

            assert!(REPORTER.stopped.load(Ordering::Acquire));
            assert!(REPORTER.thread.get().is_none());
            for stream in [Stream::Stdout, Stream::Stderr] {
                assert!(!REPORTER.enabled[stream.index()].load(Ordering::Relaxed));
                let loss = LossSnapshot::take(&REPORTER, stream);
                assert_eq!(loss.setup_status, 11);
                assert_eq!(loss.failed_delivery, 1);
            }
            let mut scratch = Group::empty();
            assert_eq!(REPORTER.take(|group| scratch.copy_from(group)), None);
            drop(group);

            exit(super::super::tests::COMPLETED);
        },
    );
}

impl Write for ShortWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.calls += 1;
        if self.interruptions > 0 {
            self.interruptions -= 1;
            return Err(io::ErrorKind::Interrupted.into());
        }
        let count = bytes.len().min(2);
        assert!(self.data.len() + count <= self.data.capacity());
        self.data.extend_from_slice(&bytes[..count]);

        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
