use super::tests::{aggregate, interval, observation, program, usage};
use super::*;

struct Counters {
    read_before: u64,
    read_selected: u64,
    written_before: u64,
    written_selected: u64,
}

struct DiskResult {
    process: [Amount; 3],
    app: AppTotals,
}

macro_rules! disk_delta {
    ($name:ident, $read_before:expr, $read_selected:expr, $written_before:expr,
        $written_selected:expr, $read:expr, $written:expr, $combined:expr) => {
        #[test]
        fn $name() {
            // Goal: disk directions and the jointly measured sum retain independent availability.
            // Method: vary counter endpoints and compare exact public aggregate amounts.
            let counters = Counters {
                read_before: $read_before,
                read_selected: $read_selected,
                written_before: $written_before,
                written_selected: $written_selected,
            };

            let actual = summarize(&[counters]);

            assert_eq!(actual.process, [$read, $written, $combined]);
        }
    };
}

disk_delta!(
    keeps_writes_when_the_read_counter_resets,
    100,
    1,
    200,
    208,
    Amount::Unavailable(Unavailable::CounterReset),
    Amount::Known(8),
    Amount::Unavailable(Unavailable::CounterReset)
);
disk_delta!(
    keeps_reads_when_the_write_counter_resets,
    100,
    112,
    200,
    1,
    Amount::Known(12),
    Amount::Unavailable(Unavailable::CounterReset),
    Amount::Unavailable(Unavailable::CounterReset)
);
disk_delta!(
    keeps_directional_deltas_when_the_combined_amount_overflows,
    0,
    u64::MAX,
    0,
    1,
    Amount::Known(u64::MAX),
    Amount::Known(1),
    Amount::Unavailable(Unavailable::Overflow)
);
disk_delta!(
    keeps_measured_zero_disk_activity,
    100,
    100,
    200,
    200,
    Amount::Known(0),
    Amount::Known(0),
    Amount::Known(0)
);
disk_delta!(
    subtracts_exact_disk_counters_above_float_precision,
    9_007_199_254_740_993,
    9_007_199_254_745_089,
    9_007_199_254_741_993,
    9_007_199_254_743_017,
    Amount::Known(4096),
    Amount::Known(1024),
    Amount::Known(5120)
);

#[test]
fn excludes_disjoint_directional_members_from_combined_app_io() {
    // Goal: independent partial read/write populations cannot fabricate measured combined I/O.
    // Method: one member resets reads and the other resets writes in the same App.
    let counters = [
        Counters {
            read_before: 100,
            read_selected: 1,
            written_before: 200,
            written_selected: 208,
        },
        Counters {
            read_before: 100,
            read_selected: 112,
            written_before: 200,
            written_selected: 1,
        },
    ];

    let actual = summarize(&counters);

    assert_eq!(
        actual.app.disk_read_bytes,
        Subtotal {
            known: 12,
            measured_members: 1,
            unavailable_members: 1,
            overflowed: false
        }
    );
    assert_eq!(
        actual.app.disk_written_bytes,
        Subtotal {
            known: 8,
            measured_members: 1,
            unavailable_members: 1,
            overflowed: false
        }
    );
    assert_eq!(
        actual.app.disk_io_bytes,
        Subtotal {
            known: 0,
            measured_members: 0,
            unavailable_members: 2,
            overflowed: false
        }
    );
}

#[test]
fn marks_read_and_combined_app_overflow_without_erasing_measured_writes() {
    // Goal: an App sum overflow retains exact known subtotals and independent directional data.
    // Method: sum read deltas u64::MAX and one while both members measured zero writes.
    let counters = [
        Counters {
            read_before: 0,
            read_selected: u64::MAX,
            written_before: 0,
            written_selected: 0,
        },
        Counters {
            read_before: 0,
            read_selected: 1,
            written_before: 0,
            written_selected: 0,
        },
    ];

    let actual = summarize(&counters);

    assert_eq!(
        actual.app.disk_read_bytes,
        Subtotal {
            known: u64::MAX,
            measured_members: 1,
            unavailable_members: 1,
            overflowed: true
        }
    );
    assert_eq!(actual.app.disk_io_bytes, actual.app.disk_read_bytes);
    assert_eq!(
        actual.app.disk_written_bytes,
        Subtotal {
            known: 0,
            measured_members: 2,
            unavailable_members: 0,
            overflowed: false
        }
    );
}

fn summarize(counters: &[Counters]) -> Box<DiskResult> {
    assert!(counters.len() <= 2);
    let interval = interval();
    let program = program(b"/A.app");
    let before_usage: Vec<_> = counters
        .iter()
        .map(|counter| {
            let mut value = usage(100, 200, 300);
            value.disk_read_bytes = counter.read_before;
            value.disk_written_bytes = counter.written_before;
            value
        })
        .collect();
    let selected_usage: Vec<_> = counters
        .iter()
        .map(|counter| {
            let mut value = usage(400, 600, 800);
            value.disk_read_bytes = counter.read_selected;
            value.disk_written_bytes = counter.written_selected;
            value
        })
        .collect();
    let previous = summarize_observations(&interval.previous, &program, &before_usage);
    let selected = summarize_observations(&interval.selected, &program, &selected_usage);
    let processes: Vec<_> = previous
        .iter()
        .zip(&selected)
        .map(|(previous, selected)| ProcessInterval {
            previous: Some(previous),
            selected,
            start: None,
        })
        .collect();
    let mut aggregate = aggregate();

    aggregate.compute(&interval, &processes).unwrap();

    let process = &aggregate.processes[0];
    Box::new(DiskResult {
        process: [
            process.disk_read_bytes,
            process.disk_written_bytes,
            process.disk_io_bytes,
        ],
        app: aggregate.apps.remove(&AppKey::Bundle(b"/A.app")).unwrap(),
    })
}

fn summarize_observations<'a>(
    stamp: &Stamp,
    program: &'a ProgramBytes<'a>,
    usages: &'a [Box<Usage>],
) -> Vec<Box<ProcessObservation<'a>>> {
    assert!(usages.len() <= 2);
    usages
        .iter()
        .enumerate()
        .map(|(index, usage)| {
            let mut value = observation(stamp, program, usage);
            value.pid += i32::try_from(index).unwrap();
            value
        })
        .collect()
}

macro_rules! disk_process_boundary {
    ($name:ident, $change:expr, $expected:expr, $known:expr, $measured:expr) => {
        #[test]
        fn $name() {
            // Goal: disk follows process lifecycle evidence without attributing an Exec interval.
            // Method: change one boundary on a process with measured 12/8-byte deltas.
            let interval = interval();
            let program = program(b"/A.app");
            let counters = Counters {
                read_before: 100,
                read_selected: 112,
                written_before: 200,
                written_selected: 208,
            };
            let mut before_usage = usage(100, 200, 300);
            before_usage.disk_read_bytes = counters.read_before;
            before_usage.disk_written_bytes = counters.written_before;
            let mut selected_usage = usage(400, 600, 800);
            selected_usage.disk_read_bytes = counters.read_selected;
            selected_usage.disk_written_bytes = counters.written_selected;
            let previous = observation(&interval.previous, &program, &before_usage);
            let mut selected = observation(&interval.selected, &program, &selected_usage);
            $change(&mut selected);
            let mut aggregate = aggregate();

            aggregate
                .compute(
                    &interval,
                    &[ProcessInterval {
                        previous: Some(&previous),
                        selected: &selected,
                        start: None,
                    }],
                )
                .unwrap();

            let process = &aggregate.processes[0];
            assert_eq!(
                [
                    process.disk_read_bytes,
                    process.disk_written_bytes,
                    process.disk_io_bytes
                ],
                $expected
            );
            assert_eq!(
                aggregate.apps[&AppKey::Bundle(b"/A.app")].disk_io_bytes,
                Subtotal {
                    known: $known,
                    measured_members: $measured,
                    unavailable_members: 1 - $measured,
                    overflowed: false
                }
            );
        }
    };
}

disk_process_boundary!(
    keeps_disk_deltas_at_a_verified_exit,
    |value: &mut ProcessObservation| value.state = ProcessState::ExitedVerified,
    [Amount::Known(12), Amount::Known(8), Amount::Known(20)],
    20,
    1
);
disk_process_boundary!(
    refuses_disk_deltas_at_an_unverified_exit,
    |value: &mut ProcessObservation| value.state = ProcessState::ExitedUnverified,
    [Amount::Unavailable(Unavailable::ExitUnverified); 3],
    0,
    0
);
disk_process_boundary!(
    refuses_disk_deltas_for_reused_pids,
    |value: &mut ProcessObservation| value.creation.as_mut().unwrap().unique_id = 701,
    [Amount::Unavailable(Unavailable::Creation); 3],
    0,
    0
);
disk_process_boundary!(
    preserves_refused_disk_usage,
    |value: &mut ProcessObservation| value.usage = Err(Unavailable::Refused),
    [Amount::Unavailable(Unavailable::Refused); 3],
    0,
    0
);
disk_process_boundary!(
    preserves_gone_disk_usage,
    |value: &mut ProcessObservation| value.usage = Err(Unavailable::Gone),
    [Amount::Unavailable(Unavailable::Gone); 3],
    0,
    0
);
disk_process_boundary!(
    keeps_process_disk_but_excludes_exec_from_app_disk,
    |value: &mut ProcessObservation| value.creation.as_mut().unwrap().pid_version = 2,
    [Amount::Known(12), Amount::Known(8), Amount::Known(20)],
    0,
    0
);

#[test]
fn uses_a_zero_disk_baseline_only_for_a_verified_in_interval_birth() {
    // Goal: independently verified birth permits exact disk deltas without a previous usage row.
    // Method: provide a matching Start whose usage birth is inside the source awake interval.
    let interval = interval();
    let program = program(b"/A.app");
    let mut usage = usage(100, 200, 300);
    usage.awake_ns_at_start = 1_500_000_000;
    usage.disk_read_bytes = 4096;
    usage.disk_written_bytes = 1024;
    let selected = observation(&interval.selected, &program, &usage);
    let start = VerifiedStart {
        stamp: interval.selected,
        pid: selected.pid,
        creation: selected.creation.unwrap(),
        program: Ok(&program),
    };
    let mut aggregate = aggregate();

    aggregate
        .compute(
            &interval,
            &[ProcessInterval {
                previous: None,
                selected: &selected,
                start: Some(&start),
            }],
        )
        .unwrap();

    assert_eq!(aggregate.processes[0].disk_read_bytes, Amount::Known(4096));
    assert_eq!(
        aggregate.processes[0].disk_written_bytes,
        Amount::Known(1024)
    );
    assert_eq!(aggregate.processes[0].disk_io_bytes, Amount::Known(5120));
    assert_eq!(
        aggregate.apps[&AppKey::Bundle(b"/A.app")].disk_io_bytes,
        Subtotal {
            known: 5120,
            measured_members: 1,
            unavailable_members: 0,
            overflowed: false
        }
    );
}

#[test]
fn refuses_a_zero_elapsed_disk_rate() {
    // Goal: callers cannot turn an undefined duration into infinite throughput.
    // Method: pass measured bytes with a zero continuous-time denominator.
    assert_eq!(
        bytes_per_second(BytesRate {
            bytes: 4096,
            elapsed_ns: 0
        }),
        None
    );
}
