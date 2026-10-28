use super::*;

use crate::collector::Collector;
use crate::test_process as process;

pub(super) const COMPLETED: i32 = 71;

pub(super) fn supervised(test: &str, work: impl FnOnce()) {
    const SELECTED: &str = "FLIGHTDECK_DIAGNOSTIC_SELECTED_TEST";
    if std::env::var(SELECTED).is_ok_and(|selected| selected == test) {
        work();
        std::process::exit(COMPLETED);
    }
    let mut command = std::process::Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", test, "--nocapture"])
        .env(SELECTED, test);
    let mut child = process::OwnedChild::spawn(&mut command);

    let status = child.wait(Duration::from_secs(2));

    assert_eq!(
        Some(COMPLETED),
        status.code(),
        "selected child failed: {test}"
    );
}

#[test]
fn reports_optional_smc_failure_from_collection() {
    // Goal: continuing after an SMC failure still identifies the source and native status.
    // Method: fail the real collection path and inspect its admitted event in an isolated child.
    supervised(
        "diagnostics::tests::reports_optional_smc_failure_from_collection",
        || {
            let mut collector =
                Collector::new(crate::clock::Timebase::read().unwrap(), Duration::ZERO).unwrap();
            collector.ioreport = None;
            collector.smc = Some(crate::smc::tests::m5_pro());
            crate::smc::tests::fail_read(collector.smc.as_mut().unwrap());

            reports_optional_failure(&mut collector, b"collector: SMC read failed: Native(-71)\n");
        },
    );
}

#[test]
fn reports_optional_ioreport_failure_from_collection() {
    // Goal: continuing after an IOReport failure still identifies the source and native result.
    // Method: fail the real collection path and inspect its admitted event in an isolated child.
    supervised(
        "diagnostics::tests::reports_optional_ioreport_failure_from_collection",
        || {
            let mut collector =
                Collector::new(crate::clock::Timebase::read().unwrap(), Duration::ZERO).unwrap();
            collector.smc = None;
            collector.ioreport = Some(crate::ioreport::tests::one_core());
            crate::ioreport::tests::fail_read(collector.ioreport.as_mut().unwrap());

            reports_optional_failure(
                &mut collector,
                b"collector: IOReport read failed: NullSample\n",
            );
        },
    );
}

fn reports_optional_failure(collector: &mut Collector, expected: &[u8]) {
    let mut scratch = Box::new(Group::empty());
    let mut meter = crate::cost::Meter::default();
    // Leave the writer unstarted so queue admission can be observed without a delivery race.
    REPORTER.prepare();
    assert!(REPORTER.thread.get().is_none());
    assert!(!INSTALLED.swap(true, Ordering::AcqRel));

    collector.read(&mut meter).unwrap();

    assert_eq!(Some(Stream::Stderr), REPORTER.take(&mut scratch));
    assert_eq!(expected, scratch.bytes());
    assert_eq!(None, REPORTER.take(&mut scratch));
}

#[test]
fn notifies_a_parked_writer_after_rejecting_the_final_group() {
    // Goal: rejected output can report its loss even when no complete group is queued.
    // Method: park an independently controlled receiver and reject an oversized group.
    let reporter = Reporter::new();
    let mut group = Box::new(Group::empty());
    group.write_all(&[b'x'; GROUP_BYTES + 1]).unwrap();
    let (ready_sender, ready_receiver) = std::sync::mpsc::channel();
    let (wake_sender, wake_receiver) = std::sync::mpsc::channel();
    let handle = std::thread::spawn(move || {
        ready_sender.send(()).unwrap();
        std::thread::park();
        wake_sender.send(()).unwrap();
    });
    assert!(reporter.thread.set(handle.thread().clone()).is_ok());
    let ready = ready_receiver.recv_timeout(Duration::from_secs(1));

    if ready.is_ok() {
        reporter.admit(Stream::Stdout, &group);
    } else {
        handle.thread().unpark();
    }
    let notified = wake_receiver.recv_timeout(Duration::from_secs(1));
    handle.thread().unpark();
    handle.join().unwrap();

    assert!(ready.is_ok());
    assert!(notified.is_ok());
}

#[test]
fn rejects_contention_and_disabled_output_before_formatting() {
    // Goal: unavailable staging and disabled sinks never execute an error formatter.
    // Method: use a formatter that fails the test if touched in either condition.
    supervised(
        "diagnostics::tests::rejects_contention_and_disabled_output_before_formatting",
        || {
            struct Untouched;
            impl fmt::Display for Untouched {
                fn fmt(&self, _output: &mut fmt::Formatter<'_>) -> fmt::Result {
                    panic!("rejected output was formatted");
                }
            }
            let reporter = Reporter::new();
            let occupied = reporter.staging.lock().unwrap();

            reporter.diagnostic(format_args!("{Untouched}"));
            drop(occupied);
            reporter.enabled[1].store(false, Ordering::Relaxed);
            reporter.diagnostic(format_args!("{Untouched}"));

            assert_eq!(
                1,
                reporter.loss[1].staging_contention.load(Ordering::Relaxed)
            );
            assert_eq!(1, reporter.loss[1].failed_delivery.load(Ordering::Relaxed));
        },
    );
}

#[test]
fn drops_complete_groups_when_the_queue_is_occupied_or_full() {
    // Goal: sink backlog and queue ownership cannot wait on a producer.
    // Method: hold queue state, then fill all slots with distinguishable whole groups.
    supervised(
        "diagnostics::tests::drops_complete_groups_when_the_queue_is_occupied_or_full",
        || {
            let reporter = Reporter::new();
            let mut group = Box::new(Group::empty());
            group.write_all(b"complete\n").unwrap();
            let occupied = reporter.queue.lock().unwrap();

            reporter.admit(Stream::Stdout, &group);
            drop(occupied);
            for _index in 0..=QUEUE_SLOTS {
                reporter.admit(Stream::Stdout, &group);
            }
            let mut scratch = Box::new(Group::empty());
            for _index in 0..QUEUE_SLOTS {
                assert_eq!(Some(Stream::Stdout), reporter.take(&mut scratch));
                assert_eq!(b"complete\n", scratch.bytes());
            }

            assert_eq!(None, reporter.take(&mut scratch));
            assert_eq!(1, reporter.loss[0].queue_contention.load(Ordering::Relaxed));
            assert_eq!(1, reporter.loss[0].queue_full.load(Ordering::Relaxed));
        },
    );
}

#[test]
fn rejects_a_nested_diagnostic_without_damaging_the_outer_event() {
    // Goal: nested errors remain nonwaiting while the outer formatter owns staging.
    // Method: report again inside Display and read the surviving complete outer event.
    supervised(
        "diagnostics::tests::rejects_a_nested_diagnostic_without_damaging_the_outer_event",
        || {
            struct Nested<'reporter>(&'reporter Reporter);
            impl fmt::Display for Nested<'_> {
                fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
                    self.0.diagnostic(format_args!("inner"));
                    output.write_str("outer")
                }
            }
            let reporter = Reporter::new();
            let mut scratch = Box::new(Group::empty());

            reporter.diagnostic(format_args!("{}", Nested(&reporter)));

            assert_eq!(Some(Stream::Stderr), reporter.take(&mut scratch));
            assert_eq!(b"outer\n", scratch.bytes());
            assert_eq!(None, reporter.take(&mut scratch));
            assert_eq!(
                1,
                reporter.loss[1].staging_contention.load(Ordering::Relaxed)
            );
        },
    );
}

#[test]
fn rejects_a_failed_formatter_without_delivering_its_prefix() {
    // Goal: an arbitrary Display failure cannot crash collection or publish a partial event.
    // Method: fail after writing a prefix, then offer an ordinary diagnostic.
    struct Failed;
    impl fmt::Display for Failed {
        fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
            output.write_str("partial")?;
            Err(fmt::Error)
        }
    }
    let reporter = Reporter::new();
    let mut scratch = Box::new(Group::empty());

    reporter.diagnostic(format_args!("{Failed}"));
    reporter.diagnostic(format_args!("complete"));

    assert_eq!(Some(Stream::Stderr), reporter.take(&mut scratch));
    assert_eq!(b"complete\n", scratch.bytes());
    assert_eq!(None, reporter.take(&mut scratch));
    assert_eq!(1, reporter.loss[1].failed_delivery.load(Ordering::Relaxed));
}

#[test]
fn allocates_nothing_during_repeated_formatting_admission_and_rejection() {
    // Goal: transport work remains inside the collector's startup allocation boundary.
    // Method: count this thread's allocator calls while exercising accepted and lost events.
    supervised(
        "diagnostics::tests::allocates_nothing_during_repeated_formatting_admission_and_rejection",
        || {
            let reporter = Reporter::new();
            reporter.prepare();
            let mut scratch = Box::new(Group::empty());
            let calls = crate::recording::discovery::tests::allocator_calls_in(|| {
                for index in 0..1_000 {
                    reporter.diagnostic(format_args!("counter={index}"));
                    assert_eq!(Some(Stream::Stderr), reporter.take(&mut scratch));
                    let occupied = reporter.staging.lock().unwrap();
                    reporter.diagnostic(format_args!("discarded={index}"));
                    drop(occupied);
                }
            });

            assert_eq!(0, calls);
            assert_eq!(
                1_000,
                reporter.loss[1].staging_contention.load(Ordering::Relaxed)
            );
            eprintln!(
                "reporter_payload_bytes={} group_storage_bytes={} shared_bytes={}",
                GROUP_BYTES,
                size_of::<Group>(),
                size_of::<Reporter>()
            );
        },
    );
}

#[test]
fn discards_an_oversized_group_without_admitting_its_prefix() {
    // Goal: a rejected report cannot leave a plausible partial group in the transport.
    // Method: exceed the fixed capacity by one byte, then admit an independent report.
    let reporter = Reporter::new();
    let mut group = Box::new(Group::empty());
    group.write_all(&[b'x'; GROUP_BYTES]).unwrap();
    group.write_all(b"x").unwrap();

    reporter.admit(Stream::Stdout, &group);
    group.begin();
    group.write_all(b"complete\n").unwrap();
    reporter.admit(Stream::Stdout, &group);
    let mut scratch = Box::new(Group::empty());
    let stream = reporter.take(&mut scratch);

    assert_eq!(Some(Stream::Stdout), stream);
    assert_eq!(b"complete\n", scratch.bytes());
    assert_eq!(1, reporter.loss[0].oversize.load(Ordering::Relaxed));
    assert_eq!(None, reporter.take(&mut scratch));
}
