use super::{
    Amount, Apps, Creation, Deltas, Execs, Exits, Measurement, ProcessState, ProcessTotals,
    Processes, ProgramUnavailable, Stamp, Starts, Subtotal, Tick, Totals, Unavailable, Usages,
};
use crate::programs::{AppKey, Capacity, Program, Programs};

const CAPACITY: Capacity = Capacity {
    entries: 8,
    key_bytes: 1024,
};
const PROCESSES_MAX: u32 = 8;
const SAFARI: Program = Program {
    executable_name: b"Safari",
    app_path: Some(b"/Applications/Safari.app"),
};
const SAFARI_HELPER: Program = Program {
    executable_name: b"Safari Web Content",
    app_path: Some(b"/Applications/Safari.app"),
};
const BRAVE: Program = Program {
    executable_name: b"Brave Browser",
    app_path: Some(b"/Applications/Brave Browser.app"),
};
const GIT: Program = Program {
    executable_name: b"git",
    app_path: None,
};

#[test]
fn attributes_the_cpu_a_process_spent_between_two_ticks_to_its_app() {
    // Goal: an App takes the CPU time its one process spent between two ticks.
    // Method: index one bundled program as the ring adapter hands out its first entry, then
    // derive the Apps of two ticks of one process of it, whose CPU time grows by 5 ms.
    let mut programs = Programs::new(CAPACITY);
    let entry = programs.add(SAFARI);
    let previous = Columns::of(
        1,
        &[Process {
            program: Ok(entry),
            user_ns: 7_000_000,
            system_ns: 3_000_000,
            ..Process::default()
        }],
    );
    let selected = Columns::of(
        2,
        &[Process {
            program: Ok(entry),
            user_ns: 10_000_000,
            system_ns: 5_000_000,
            ..Process::default()
        }],
    );
    let mut apps = Apps::new(PROCESSES_MAX, &programs);

    apps.derive(&programs, &previous.tick(), &selected.tick());

    assert_eq!(entry, 0);
    assert_eq!(apps.totals().len(), 1);
    assert_eq!(apps.totals()[0].app, programs.app(entry));
    assert_eq!(apps.totals()[0].members, 1);
    assert_eq!(apps.totals()[0].cpu_ns, measured(5_000_000, 1));
}

#[test]
fn groups_the_processes_of_one_bundle_into_one_app() {
    // Goal: the processes of two programs inside one bundle are members of one App.
    // Method: index the bundle's main program and a helper inside it, then derive the Apps of
    // two ticks of one process of each, whose CPU times grow by 5 ms and 2 ms.
    let mut programs = Programs::new(CAPACITY);
    let main = programs.add(SAFARI);
    let helper = programs.add(SAFARI_HELPER);
    let previous = Columns::of(
        1,
        &[
            Process {
                program: Ok(main),
                ..Process::default()
            },
            Process {
                pid: 8,
                program: Ok(helper),
                ..Process::default()
            },
        ],
    );
    let selected = Columns::of(
        2,
        &[
            Process {
                program: Ok(main),
                user_ns: 5_000_000,
                ..Process::default()
            },
            Process {
                pid: 8,
                program: Ok(helper),
                user_ns: 2_000_000,
                ..Process::default()
            },
        ],
    );
    let mut apps = Apps::new(PROCESSES_MAX, &programs);

    apps.derive(&programs, &previous.tick(), &selected.tick());

    assert_eq!(apps.totals().len(), 1);
    assert_eq!(apps.totals()[0].app, programs.app(main));
    assert_eq!(apps.totals()[0].members, 2);
    assert_eq!(apps.totals()[0].cpu_ns, measured(7_000_000, 2));
}

#[test]
fn gives_each_process_the_cpu_it_spent() {
    // Goal: each process of a tick carries the CPU time it spent since the tick before.
    // Method: derive two ticks of one process whose user and system times grow by 4 and 1 ms.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(
        2,
        &[Process {
            user_ns: 4_000_000,
            system_ns: 1_000_000,
            ..Process::default()
        }],
    );

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes[0].deltas.cpu_ns, Amount::Known(5_000_000));
    assert_eq!(derived.processes[0].app_attribution, None);
}

/// Declares a test that a change to the selected tick's stamp leaves CPU time unknown, since
/// the two ticks then do not span one interval of one collector run.
macro_rules! interval_tests {
    ($($name:ident, $field:ident, $value:expr;)*) => {
        $(
            #[test]
            fn $name() {
                // Goal: CPU time is unknown when the ticks do not span one interval of a run.
                // Method: derive two ticks of one process whose selected stamp differs from
                // one interval after the previous in one field.
                let previous = Columns::of(1, &[Process::default()]);
                let mut selected = Columns::of(2, &[Process::default()]);
                selected.stamp.$field = $value;

                let derived = derive(&previous, &selected);

                assert_eq!(derived.processes[0].deltas, unavailable(Unavailable::Interval));
                assert_eq!(derived.totals[0].cpu_ns, absent(1));
            }
        )*
    };
}

interval_tests! {
    leaves_cpu_unknown_across_two_recordings, recording, [9, 9];
    leaves_cpu_unknown_across_two_boots, boot, [9, 9];
    leaves_cpu_unknown_across_two_runs, run, [9, 9];
    leaves_cpu_unknown_across_a_missed_tick, publication, 3;
    leaves_cpu_unknown_without_a_continuous_clock, continuous_ns, None;
    leaves_cpu_unknown_when_the_continuous_clock_stands_still, continuous_ns, Some(1_000_000_000);
}

#[test]
fn leaves_cpu_unknown_without_a_continuous_clock_at_the_previous_tick() {
    // Goal: CPU time is unknown when the previous tick has no continuous clock.
    // Method: derive two ticks of one process whose previous stamp has no continuous time.
    let mut previous = Columns::of(1, &[Process::default()]);
    previous.stamp.continuous_ns = None;
    let selected = Columns::of(2, &[Process::default()]);

    let derived = derive(&previous, &selected);

    assert_eq!(
        derived.processes[0].deltas,
        unavailable(Unavailable::Interval)
    );
}

#[test]
fn leaves_cpu_unknown_for_a_process_the_previous_tick_lacks() {
    // Goal: a process with no observation at the previous tick has no baseline.
    // Method: derive a tick whose process has a pid the previous tick does not hold.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(
        2,
        &[Process {
            pid: 8,
            ..Process::default()
        }],
    );

    let derived = derive(&previous, &selected);

    assert_eq!(
        derived.processes[0].deltas,
        unavailable(Unavailable::Baseline)
    );
}

#[test]
fn leaves_cpu_unknown_for_a_new_process_of_a_reused_pid() {
    // Goal: a new process that reuses a pid takes no baseline from the process before it.
    // Method: derive a tick whose process has the previous process's pid and another creation.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(
        2,
        &[Process {
            creation: Some(Creation {
                unique_id: 701,
                pid_version: 2,
            }),
            ..Process::default()
        }],
    );

    let derived = derive(&previous, &selected);

    assert_eq!(
        derived.processes[0].deltas,
        unavailable(Unavailable::Baseline)
    );
}

#[test]
fn leaves_cpu_unknown_for_a_process_without_its_creation() {
    // Goal: a process whose creation could not be read takes no baseline.
    // Method: derive a tick whose process has the previous process's pid and no creation.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(
        2,
        &[Process {
            creation: None,
            ..Process::default()
        }],
    );

    let derived = derive(&previous, &selected);

    assert_eq!(
        derived.processes[0].deltas,
        unavailable(Unavailable::Baseline)
    );
}

#[test]
fn leaves_cpu_unknown_when_a_counter_went_back() {
    // Goal: a counter that went back gives no CPU time, since its change is not what was spent.
    // Method: derive two ticks of one process whose system time falls.
    let previous = Columns::of(
        1,
        &[Process {
            system_ns: 2_000_000,
            ..Process::default()
        }],
    );
    let selected = Columns::of(
        2,
        &[Process {
            user_ns: 9_000_000,
            system_ns: 1_000_000,
            ..Process::default()
        }],
    );

    let derived = derive(&previous, &selected);

    assert_eq!(
        derived.processes[0].deltas.cpu_ns,
        Amount::Unavailable(Unavailable::CounterReset)
    );
}

#[test]
fn leaves_cpu_unknown_when_the_spent_time_overflows() {
    // Goal: CPU time a u64 cannot hold is unknown, not wrapped.
    // Method: derive two ticks of one process whose user and system times both grow by more
    // than half of a u64.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(
        2,
        &[Process {
            user_ns: u64::MAX / 2 + 1,
            system_ns: u64::MAX / 2 + 1,
            ..Process::default()
        }],
    );

    let derived = derive(&previous, &selected);

    assert_eq!(
        derived.processes[0].deltas.cpu_ns,
        Amount::Unavailable(Unavailable::Overflow)
    );
}

#[test]
fn marks_an_app_subtotal_that_overflows() {
    // Goal: an App whose members' CPU times sum past a u64 reports the overflow.
    // Method: derive two ticks of two processes of one App, each of which spent more than
    // half of a u64.
    let previous = Columns::of(
        1,
        &[
            Process::default(),
            Process {
                pid: 8,
                ..Process::default()
            },
        ],
    );
    let selected = Columns::of(
        2,
        &[
            Process {
                user_ns: u64::MAX / 2 + 1,
                ..Process::default()
            },
            Process {
                pid: 8,
                user_ns: u64::MAX / 2 + 1,
                ..Process::default()
            },
        ],
    );

    let derived = derive(&previous, &selected);

    assert_eq!(
        derived.totals[0].cpu_ns,
        Subtotal {
            known: u64::MAX / 2 + 1,
            measured_members: 1,
            unavailable_members: 1,
            overflowed: true,
        }
    );
}

#[test]
fn leaves_cpu_unknown_when_the_process_restarted_its_clock() {
    // Goal: a process whose awake time at start changed is not the process before it.
    // Method: derive two ticks of one pid and creation whose awake time at start differs.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(
        2,
        &[Process {
            awake_ns_at_start: 5,
            user_ns: 1_000_000,
            ..Process::default()
        }],
    );

    let derived = derive(&previous, &selected);

    assert_eq!(
        derived.processes[0].deltas,
        unavailable(Unavailable::Creation)
    );
}

/// Declares a test that a process whose usage could not be read at one tick has no CPU time,
/// for the reason the read gave.
macro_rules! measurement_tests {
    ($($name:ident, $previous:expr, $selected:expr, $reason:expr;)*) => {
        $(
            #[test]
            fn $name() {
                // Goal: CPU time is unknown, for the read's reason, when usage was not read.
                // Method: derive two ticks of one process whose usage one tick lacks.
                let previous = Columns::of(1, &[Process {
                    usage: $previous,
                    ..Process::default()
                }]);
                let selected = Columns::of(2, &[Process {
                    usage: $selected,
                    ..Process::default()
                }]);

                let derived = derive(&previous, &selected);

                assert_eq!(derived.processes[0].deltas, unavailable($reason));
            }
        )*
    };
}

measurement_tests! {
    leaves_cpu_unknown_when_the_selected_read_was_refused,
        Measurement::Measured, Measurement::Refused, Unavailable::Refused;
    leaves_cpu_unknown_when_the_process_was_gone_at_the_selected_read,
        Measurement::Measured, Measurement::Gone, Unavailable::Gone;
    leaves_cpu_unknown_when_the_previous_read_was_refused,
        Measurement::Refused, Measurement::Measured, Unavailable::Refused;
    leaves_cpu_unknown_when_the_process_was_gone_at_the_previous_read,
        Measurement::Gone, Measurement::Measured, Unavailable::Gone;
}

#[test]
fn gives_the_interval_reason_before_a_refused_read() {
    // Goal: a process across two runs reports the interval, not its refused read.
    // Method: derive two ticks of two runs of one process whose selected read was refused.
    let previous = Columns::of(1, &[Process::default()]);
    let mut selected = Columns::of(
        2,
        &[Process {
            usage: Measurement::Refused,
            ..Process::default()
        }],
    );
    selected.stamp.run = [9, 9];

    let derived = derive(&previous, &selected);

    assert_eq!(
        derived.processes[0].deltas,
        unavailable(Unavailable::Interval)
    );
}

#[test]
fn gives_the_apps_in_key_order() {
    // Goal: the Apps of a tick come out in the order of their keys, bundles before
    // executables, whatever order their processes and their programs came in.
    // Method: index git, Safari, and then Brave, which sorts before Safari, and derive a tick
    // of one process of each in that order.
    let mut programs = Programs::new(CAPACITY);
    let rows: Vec<Process> = [(5, GIT), (6, SAFARI), (7, BRAVE)]
        .into_iter()
        .map(|(pid, program)| Process {
            pid,
            program: Ok(programs.add(program)),
            ..Process::default()
        })
        .collect();
    let previous = Columns::of(1, &rows);
    let selected = Columns::of(2, &rows);
    let mut apps = Apps::new(PROCESSES_MAX, &programs);

    apps.derive(&programs, &previous.tick(), &selected.tick());

    let keys: Vec<AppKey> = apps
        .totals()
        .iter()
        .map(|totals| programs.key(totals.app))
        .collect();
    assert_eq!(
        keys,
        [
            AppKey::Bundle(b"/Applications/Brave Browser.app"),
            AppKey::Bundle(b"/Applications/Safari.app"),
            AppKey::Executable(b"git"),
        ]
    );
}

#[test]
fn keeps_the_cpu_of_a_process_that_ran_another_program_out_of_its_apps() {
    // Goal: a process that ran another program keeps its CPU time, which no App takes, since
    // the rule cannot split it between the two programs.
    // Method: derive two ticks of one process whose program changes from git to Safari.
    let mut programs = Programs::new(CAPACITY);
    let git = programs.add(GIT);
    let safari = programs.add(SAFARI);
    let previous = Columns::of(
        1,
        &[Process {
            program: Ok(git),
            ..Process::default()
        }],
    );
    let selected = Columns::of(
        2,
        &[Process {
            program: Ok(safari),
            user_ns: 3_000_000,
            ..Process::default()
        }],
    );
    let mut apps = Apps::new(PROCESSES_MAX, &programs);

    apps.derive(&programs, &previous.tick(), &selected.tick());

    assert_eq!(apps.processes()[0].deltas.cpu_ns, Amount::Known(3_000_000));
    assert_eq!(apps.processes()[0].app_attribution, Some(Unavailable::Exec));
    assert_eq!(apps.totals().len(), 1);
    assert_eq!(apps.totals()[0].app, programs.app(safari));
    assert_eq!(apps.totals()[0].cpu_ns, absent(1));
}

#[test]
fn keeps_the_cpu_of_a_process_whose_pid_version_changed_out_of_its_apps() {
    // Goal: a process whose pid version changed, as an exec changes it, gives no App its CPU.
    // Method: derive two ticks of one process of one program whose pid version grows.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(
        2,
        &[Process {
            creation: Some(Creation {
                unique_id: 700,
                pid_version: 2,
            }),
            user_ns: 3_000_000,
            ..Process::default()
        }],
    );

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes[0].deltas.cpu_ns, Amount::Known(3_000_000));
    assert_eq!(
        derived.processes[0].app_attribution,
        Some(Unavailable::Exec)
    );
}

#[test]
fn keeps_the_cpu_of_a_process_whose_previous_program_is_unknown_out_of_its_apps() {
    // Goal: a process whose program was unreadable at the previous tick gives no App its CPU,
    // since it may have run another program then.
    // Method: derive two ticks of one process whose previous program is unavailable.
    let previous = Columns::of(
        1,
        &[Process {
            program: Err(ProgramUnavailable::Refused),
            ..Process::default()
        }],
    );
    let selected = Columns::of(
        2,
        &[Process {
            user_ns: 3_000_000,
            ..Process::default()
        }],
    );

    let derived = derive(&previous, &selected);

    assert_eq!(
        derived.processes[0].app_attribution,
        Some(Unavailable::Exec)
    );
}

#[test]
fn leaves_a_process_without_its_program_out_of_every_app() {
    // Goal: a process whose program could not be read belongs to no App.
    // Method: derive two ticks of one process whose selected program is unavailable.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(
        2,
        &[Process {
            program: Err(ProgramUnavailable::Refused),
            user_ns: 3_000_000,
            ..Process::default()
        }],
    );

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes[0].deltas.cpu_ns, Amount::Known(3_000_000));
    assert_eq!(
        derived.processes[0].app_attribution,
        Some(Unavailable::Program)
    );
    assert_eq!(derived.totals, []);
    assert_eq!(derived.ungrouped_members, 1);
    assert_eq!(derived.live_ungrouped_members, 1);
}

#[test]
fn names_each_process_and_why_its_program_is_unknown() {
    // Goal: each process of a tick carries its pid, its creation and its program, or why the
    // program could not be read.
    // Method: derive two ticks of two processes, the second of which macOS refused the program
    // of.
    let rows = [
        Process::default(),
        Process {
            pid: 8,
            program: Err(ProgramUnavailable::Refused),
            ..Process::default()
        },
    ];
    let previous = Columns::of(1, &rows);
    let selected = Columns::of(2, &rows);

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes[0].pid, 7);
    assert_eq!(derived.processes[0].creation, Process::default().creation);
    assert_eq!(derived.processes[0].program, Ok(0));
    assert_eq!(derived.processes[1].pid, 8);
    assert_eq!(
        derived.processes[1].program,
        Err(ProgramUnavailable::Refused)
    );
}

#[test]
fn gives_each_process_the_change_of_each_counter() {
    // Goal: each counter of a process gives the change it made between the two ticks.
    // Method: derive two ticks of one process every counter of which grows by its own amount.
    let previous = Columns::of(
        1,
        &[Process {
            energy_nj: 100,
            gpu_ns: Some(1_000),
            disk_read_bytes: 4_096,
            disk_written_bytes: 512,
            idle_wakeups: 3,
            pageins: 9,
            ..Process::default()
        }],
    );
    let selected = Columns::of(
        2,
        &[Process {
            user_ns: 2_000_000,
            energy_nj: 350,
            gpu_ns: Some(1_600),
            disk_read_bytes: 8_192,
            disk_written_bytes: 1_536,
            idle_wakeups: 10,
            pageins: 11,
            ..Process::default()
        }],
    );

    let derived = derive(&previous, &selected);

    assert_eq!(
        derived.processes[0].deltas,
        Deltas {
            cpu_ns: Amount::Known(2_000_000),
            energy_nj: Amount::Known(250),
            gpu_ns: Amount::Known(600),
            disk_read_bytes: Amount::Known(4_096),
            disk_written_bytes: Amount::Known(1_024),
            disk_io_bytes: Amount::Known(5_120),
            idle_wakeups: Amount::Known(7),
            pageins: Amount::Known(2),
        }
    );
}

/// Declares a test that one counter that went back leaves only that counter's change
/// unknown.
macro_rules! counter_reset_tests {
    ($($name:ident, $field:ident, $delta:ident;)*) => {
        $(
            #[test]
            fn $name() {
                // Goal: a counter that went back leaves its own change unknown and no other.
                // Method: derive two ticks of one process one counter of which falls.
                let previous = Columns::of(1, &[Process {
                    $field: 5,
                    ..Process::default()
                }]);
                let selected = Columns::of(2, &[Process {
                    $field: 4,
                    ..Process::default()
                }]);

                let derived = derive(&previous, &selected);

                let deltas = derived.processes[0].deltas;
                assert_eq!(deltas.$delta, Amount::Unavailable(Unavailable::CounterReset));
                assert_eq!(deltas.cpu_ns, Amount::Known(0));
            }
        )*
    };
}

counter_reset_tests! {
    leaves_energy_unknown_when_its_counter_went_back, energy_nj, energy_nj;
    leaves_disk_reads_unknown_when_their_counter_went_back, disk_read_bytes, disk_read_bytes;
    leaves_disk_writes_unknown_when_their_counter_went_back,
        disk_written_bytes, disk_written_bytes;
    leaves_idle_wakeups_unknown_when_their_counter_went_back, idle_wakeups, idle_wakeups;
    leaves_pageins_unknown_when_their_counter_went_back, pageins, pageins;
}

#[test]
fn leaves_gpu_unknown_when_its_counter_went_back() {
    // Goal: a GPU time that went back gives no GPU time.
    // Method: derive two ticks of one process whose GPU time falls.
    let previous = Columns::of(
        1,
        &[Process {
            gpu_ns: Some(5),
            ..Process::default()
        }],
    );
    let selected = Columns::of(
        2,
        &[Process {
            gpu_ns: Some(4),
            ..Process::default()
        }],
    );

    let derived = derive(&previous, &selected);

    assert_eq!(
        derived.processes[0].deltas.gpu_ns,
        Amount::Unavailable(Unavailable::CounterReset)
    );
}

/// Declares a test that a GPU time missing at either tick leaves the GPU time unknown, since
/// a missing GPU time may be a failed read rather than no GPU use.
macro_rules! gpu_unknown_tests {
    ($($name:ident, $previous:expr, $selected:expr;)*) => {
        $(
            #[test]
            fn $name() {
                // Goal: GPU time is unknown where either tick lacks one.
                // Method: derive two ticks of one process one of which has no GPU time.
                let previous = Columns::of(1, &[Process {
                    gpu_ns: $previous,
                    ..Process::default()
                }]);
                let selected = Columns::of(2, &[Process {
                    gpu_ns: $selected,
                    ..Process::default()
                }]);

                let derived = derive(&previous, &selected);

                assert_eq!(
                    derived.processes[0].deltas.gpu_ns,
                    Amount::Unavailable(Unavailable::Unknown)
                );
            }
        )*
    };
}

gpu_unknown_tests! {
    leaves_gpu_unknown_without_a_previous_gpu_time, None, Some(5);
    leaves_gpu_unknown_without_a_selected_gpu_time, Some(5), None;
}

#[test]
fn gives_disk_traffic_the_reason_its_reads_are_unknown() {
    // Goal: disk traffic is unknown, for the reads' reason, when the reads are.
    // Method: derive two ticks of one process whose disk read counter falls.
    let previous = Columns::of(
        1,
        &[Process {
            disk_read_bytes: 5,
            ..Process::default()
        }],
    );
    let selected = Columns::of(
        2,
        &[Process {
            disk_written_bytes: 7,
            ..Process::default()
        }],
    );

    let derived = derive(&previous, &selected);

    assert_eq!(
        derived.processes[0].deltas.disk_io_bytes,
        Amount::Unavailable(Unavailable::CounterReset)
    );
}

#[test]
fn leaves_disk_traffic_unknown_when_its_sum_overflows() {
    // Goal: disk traffic a u64 cannot hold is unknown, not wrapped.
    // Method: derive two ticks of one process whose reads and writes each grow by more than
    // half of a u64.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(
        2,
        &[Process {
            disk_read_bytes: u64::MAX / 2 + 1,
            disk_written_bytes: u64::MAX / 2 + 1,
            ..Process::default()
        }],
    );

    let derived = derive(&previous, &selected);

    assert_eq!(
        derived.processes[0].deltas.disk_io_bytes,
        Amount::Unavailable(Unavailable::Overflow)
    );
}

#[test]
fn sums_each_counter_of_an_apps_members() {
    // Goal: an App's subtotal of each counter sums its members' changes.
    // Method: derive two ticks of two processes of one App, each counter of which grows.
    let previous = Columns::of(
        1,
        &[
            Process::default(),
            Process {
                pid: 8,
                ..Process::default()
            },
        ],
    );
    let grown = Process {
        energy_nj: 10,
        gpu_ns: Some(20),
        disk_read_bytes: 30,
        disk_written_bytes: 40,
        idle_wakeups: 50,
        pageins: 60,
        ..Process::default()
    };
    let selected = Columns::of(2, &[grown, Process { pid: 8, ..grown }]);

    let derived = derive(&previous, &selected);

    let totals = &derived.totals[0];
    assert_eq!(totals.energy_nj, measured(20, 2));
    assert_eq!(totals.gpu_ns, measured(40, 2));
    assert_eq!(totals.disk_read_bytes, measured(60, 2));
    assert_eq!(totals.disk_written_bytes, measured(80, 2));
    assert_eq!(totals.disk_io_bytes, measured(140, 2));
    assert_eq!(totals.idle_wakeups, measured(100, 2));
    assert_eq!(totals.pageins, measured(120, 2));
}

#[test]
fn gives_no_app_any_counter_of_a_process_that_ran_another_program() {
    // Goal: an App takes none of the counters of a member that ran another program.
    // Method: derive two ticks of one process whose pid version grows, each counter of which
    // grows.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(
        2,
        &[Process {
            creation: Some(Creation {
                unique_id: 700,
                pid_version: 2,
            }),
            energy_nj: 10,
            gpu_ns: Some(20),
            disk_read_bytes: 30,
            disk_written_bytes: 40,
            idle_wakeups: 50,
            pageins: 60,
            ..Process::default()
        }],
    );

    let derived = derive(&previous, &selected);

    let totals = &derived.totals[0];
    assert_eq!(totals.energy_nj, absent(1));
    assert_eq!(totals.gpu_ns, absent(1));
    assert_eq!(totals.disk_read_bytes, absent(1));
    assert_eq!(totals.disk_written_bytes, absent(1));
    assert_eq!(totals.disk_io_bytes, absent(1));
    assert_eq!(totals.idle_wakeups, absent(1));
    assert_eq!(totals.pageins, absent(1));
}

#[test]
fn sums_the_memory_and_threads_of_an_apps_live_members() {
    // Goal: an App's memory and threads are the sums of its live members' at the tick.
    // Method: derive a tick of two processes of one App with their own memory and threads.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(
        2,
        &[
            Process {
                resident_bytes: 100,
                footprint_bytes: 70,
                threads: Some(4),
                ..Process::default()
            },
            Process {
                pid: 8,
                resident_bytes: 50,
                footprint_bytes: 30,
                threads: Some(2),
                ..Process::default()
            },
        ],
    );

    let derived = derive(&previous, &selected);

    let totals = &derived.totals[0];
    assert_eq!(totals.live_members, 2);
    assert_eq!(totals.resident_bytes, measured(150, 2));
    assert_eq!(totals.footprint_bytes, measured(100, 2));
    assert_eq!(totals.threads, measured(6, 2));
}

#[test]
fn counts_a_member_whose_usage_was_not_read_out_of_its_apps_memory() {
    // Goal: a live member whose usage was not read adds nothing to its App's memory.
    // Method: derive a tick of one process whose read was refused.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(
        2,
        &[Process {
            usage: Measurement::Refused,
            resident_bytes: 100,
            ..Process::default()
        }],
    );

    let derived = derive(&previous, &selected);

    assert_eq!(derived.totals[0].resident_bytes, absent(1));
    assert_eq!(derived.totals[0].footprint_bytes, absent(1));
}

#[test]
fn counts_a_member_without_its_threads_out_of_its_apps_threads() {
    // Goal: a live member whose threads were not read adds nothing to its App's threads.
    // Method: derive a tick of one process with no thread count.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(
        2,
        &[Process {
            threads: None,
            ..Process::default()
        }],
    );

    let derived = derive(&previous, &selected);

    assert_eq!(derived.totals[0].threads, absent(1));
}

#[test]
fn gives_a_live_process_its_memory_and_threads_at_the_tick() {
    // Goal: a live process carries its memory and threads at the selected tick.
    // Method: derive a tick of one process with its own memory and threads.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(
        2,
        &[Process {
            resident_bytes: 100,
            footprint_bytes: 70,
            threads: Some(4),
            ..Process::default()
        }],
    );

    let derived = derive(&previous, &selected);

    let process = &derived.processes[0];
    assert_eq!(process.resident_bytes_last, Some(100));
    assert_eq!(process.footprint_bytes_last, Some(70));
    assert_eq!(process.threads, Some(4));
}

#[test]
fn gives_a_live_process_whose_usage_was_not_read_no_memory() {
    // Goal: a live process whose usage was not read carries no memory.
    // Method: derive a tick of one process whose read was refused.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(
        2,
        &[Process {
            usage: Measurement::Refused,
            resident_bytes: 100,
            ..Process::default()
        }],
    );

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes[0].resident_bytes_last, None);
    assert_eq!(derived.processes[0].footprint_bytes_last, None);
}

/// What a derivation of two ticks of processes of Safari gave.
#[test]
fn counts_a_process_that_exited_as_a_member_of_its_app() {
    // Goal: a process that exited during the tick gives its App what it spent until it exited,
    // and holds no memory in the App any more.
    // Method: derive two ticks of one process, which the selected tick reads only at its exit,
    // after 6 ms of CPU and with 300 bytes resident, where the previous tick read 2 ms.
    let previous = Columns::of(
        1,
        &[Process {
            user_ns: 2_000_000,
            resident_bytes: 100,
            ..Process::default()
        }],
    );
    let selected = Columns::of(2, &[]).with_exits(&[Exit {
        user_ns: 6_000_000,
        resident_bytes: 300,
        ..Exit::default()
    }]);

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes.len(), 1);
    assert_eq!(derived.processes[0].state, ProcessState::ExitedVerified);
    assert_eq!(derived.processes[0].deltas.cpu_ns, Amount::Known(4_000_000));
    assert_eq!(derived.processes[0].resident_bytes_last, Some(300));
    assert_eq!(derived.processes[0].threads, None);
    assert_eq!(derived.totals[0].members, 1);
    assert_eq!(derived.totals[0].live_members, 0);
    assert_eq!(derived.totals[0].cpu_ns, measured(4_000_000, 1));
    assert_eq!(derived.totals[0].resident_bytes, Subtotal::default());
}

#[test]
fn takes_the_exit_of_a_process_in_place_of_its_live_row() {
    // Goal: a process the tick read both live and at its exit counts once, by its exit.
    // Method: derive a selected tick holding one process live at 3 ms of CPU and its exit at
    // 5 ms.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(
        2,
        &[Process {
            user_ns: 3_000_000,
            ..Process::default()
        }],
    )
    .with_exits(&[Exit {
        user_ns: 5_000_000,
        ..Exit::default()
    }]);

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes.len(), 1);
    assert_eq!(derived.processes[0].state, ProcessState::ExitedVerified);
    assert_eq!(derived.processes[0].deltas.cpu_ns, Amount::Known(5_000_000));
    assert_eq!(derived.totals[0].live_members, 0);
}

/// Declares a test that an exit which read less than the live row of its process, or another
/// start, is not taken as the process's final usage.
macro_rules! reconcile_tests {
    ($($name:ident, $field:ident;)*) => {
        $(
            #[test]
            fn $name() {
                // Goal: an exit that disagrees with the live row of its process leaves the
                // process's counters unknown and keeps the live row's memory.
                // Method: derive a selected tick holding one process live and its exit, whose
                // one field is one less than the live row's.
                let previous = Columns::of(1, &[Process::default()]);
                let live = Process {
                    awake_ns_at_start: 9,
                    user_ns: 9,
                    system_ns: 9,
                    energy_nj: 9,
                    resident_bytes: 400,
                    ..Process::default()
                };
                let mut exit = Exit {
                    awake_ns_at_start: 9,
                    user_ns: 9,
                    system_ns: 9,
                    energy_nj: 9,
                    resident_bytes: 500,
                    ..Exit::default()
                };
                exit.$field = 8;
                let selected = Columns::of(2, &[live]).with_exits(&[exit]);

                let derived = derive(&previous, &selected);

                assert_eq!(derived.processes[0].state, ProcessState::ExitedUnverified);
                assert_eq!(
                    derived.processes[0].deltas,
                    unavailable(Unavailable::ExitUnverified)
                );
                assert_eq!(derived.processes[0].resident_bytes_last, Some(400));
            }
        )*
    };
}

reconcile_tests! {
    distrusts_an_exit_of_another_start, awake_ns_at_start;
    distrusts_an_exit_whose_user_time_went_back, user_ns;
    distrusts_an_exit_whose_system_time_went_back, system_ns;
    distrusts_an_exit_whose_energy_went_back, energy_nj;
}

#[test]
fn trusts_an_exit_whose_disk_counters_went_back() {
    // Goal: the rule checks an exit against its live row by awake time, CPU time and energy
    // alone, so a disk counter that went back leaves the exit trusted.
    // Method: derive a selected tick holding one process live and its exit, which read fewer
    // disk bytes than the live row.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(
        2,
        &[Process {
            disk_read_bytes: 9,
            ..Process::default()
        }],
    )
    .with_exits(&[Exit {
        disk_read_bytes: 8,
        ..Exit::default()
    }]);

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes[0].state, ProcessState::ExitedVerified);
    assert_eq!(
        derived.processes[0].deltas.disk_read_bytes,
        Amount::Known(8)
    );
}

#[test]
fn leaves_the_counters_of_an_exit_without_its_final_usage_unknown() {
    // Goal: an exit whose final usage was not read gives no counter, and the process keeps the
    // memory the previous tick read.
    // Method: derive two ticks of one process whose exit's usage macOS refused.
    let previous = Columns::of(
        1,
        &[Process {
            resident_bytes: 100,
            footprint_bytes: 50,
            ..Process::default()
        }],
    );
    let selected = Columns::of(2, &[]).with_exits(&[Exit {
        usage: Measurement::Refused,
        ..Exit::default()
    }]);

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes[0].state, ProcessState::ExitedUnverified);
    assert_eq!(
        derived.processes[0].deltas,
        unavailable(Unavailable::ExitUnverified)
    );
    assert_eq!(derived.processes[0].resident_bytes_last, Some(100));
    assert_eq!(derived.processes[0].footprint_bytes_last, Some(50));
}

#[test]
fn leaves_an_exit_whose_pid_version_changed_out_of_every_app() {
    // Goal: a process that ran another program before it exited belongs to no App, since its
    // program at its exit is unknown.
    // Method: derive two ticks of one process whose exit carries a greater pid version than
    // the previous tick read.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(2, &[]).with_exits(&[Exit {
        creation: Some(Creation {
            pid_version: 2,
            ..CREATION
        }),
        ..Exit::default()
    }]);

    let derived = derive(&previous, &selected);

    assert_eq!(
        derived.processes[0].program,
        Err(ProgramUnavailable::Changed)
    );
    assert_eq!(derived.totals, []);
    assert_eq!(derived.ungrouped_members, 1);
    assert_eq!(derived.live_ungrouped_members, 0);
}

#[test]
fn leaves_out_two_exits_of_one_process() {
    // Goal: two exits of one process leave which was its last unknown, so neither is taken.
    // Method: derive a selected tick holding one process live at 3 ms of CPU and two exits of
    // it.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(
        2,
        &[Process {
            user_ns: 3_000_000,
            ..Process::default()
        }],
    )
    .with_exits(&[
        Exit {
            user_ns: 4_000_000,
            ..Exit::default()
        },
        Exit {
            user_ns: 5_000_000,
            ..Exit::default()
        },
    ]);

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes.len(), 1);
    assert_eq!(derived.processes[0].state, ProcessState::Live);
    assert_eq!(derived.processes[0].deltas.cpu_ns, Amount::Known(3_000_000));
}

#[test]
fn leaves_out_an_exit_the_previous_tick_does_not_hold() {
    // Goal: an exit of a process that no reading before it names gives no App its usage, since
    // its program is unknown.
    // Method: derive two ticks, the previous without the process and the selected with only
    // its exit.
    let previous = Columns::of(1, &[]);
    let selected = Columns::of(2, &[]).with_exits(&[Exit::default()]);

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes, []);
    assert_eq!(derived.totals, []);
}

#[test]
fn leaves_out_an_exit_when_the_ticks_do_not_span_one_interval() {
    // Goal: the previous tick names no exited process unless the two ticks span one interval.
    // Method: derive two ticks a publication apart, the selected with only the exit of the
    // previous tick's process.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(3, &[]).with_exits(&[Exit::default()]);

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes, []);
}

#[test]
fn orders_the_exits_among_the_live_processes() {
    // Goal: the rule gives the tick's processes in order of pid, exits among the live ones.
    // Method: derive a selected tick of the live pids 5 and 9 and the exit of pid 7, all three
    // of which the previous tick read live.
    let rows = [5, 7, 9].map(|pid| Process {
        pid,
        creation: Some(Creation {
            unique_id: u64::try_from(pid).unwrap() * 100,
            pid_version: 1,
        }),
        ..Process::default()
    });
    let previous = Columns::of(1, &rows);
    let selected = Columns::of(2, &[rows[0], rows[2]]).with_exits(&[Exit {
        creation: rows[1].creation,
        ..Exit::default()
    }]);

    let derived = derive(&previous, &selected);

    let pids: Vec<i32> = derived
        .processes
        .iter()
        .map(|process| process.pid)
        .collect();
    let states: Vec<ProcessState> = derived
        .processes
        .iter()
        .map(|process| process.state)
        .collect();
    assert_eq!(pids, [5, 7, 9]);
    assert_eq!(
        states,
        [
            ProcessState::Live,
            ProcessState::ExitedVerified,
            ProcessState::Live
        ]
    );
}

#[test]
fn names_an_exit_by_the_exec_the_previous_tick_noticed() {
    // Goal: a process the previous tick noticed only as it ran another program, and which
    // exited before a tick read it live, belongs to that program, with no counter of its own,
    // since no reading before its exit measured it.
    // Method: derive a previous tick holding only the exec of pid 7 and a selected tick holding
    // only its exit, with 300 bytes resident.
    let (previous, selected) = noticed_before();

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes.len(), 1);
    assert_eq!(derived.processes[0].program, Ok(0));
    assert_eq!(
        derived.processes[0].deltas,
        unavailable(Unavailable::Baseline)
    );
    assert_eq!(derived.processes[0].resident_bytes_last, Some(300));
    assert_eq!(derived.totals[0].members, 1);
    assert_eq!(derived.totals[0].live_members, 0);
    assert_eq!(derived.totals[0].resident_bytes, Subtotal::default());
}

#[test]
fn names_an_exit_by_the_start_the_previous_tick_noticed() {
    // Goal: a process the previous tick noticed only as it started, and which exited before a
    // tick read it live, belongs to the program its start read.
    // Method: derive a previous tick holding only the start of pid 7 and a selected tick
    // holding only its exit.
    let (mut previous, selected) = noticed_before();
    previous = previous.with_execs(&[]).with_starts(&[Event::default()]);

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes[0].program, Ok(0));
    assert_eq!(
        derived.processes[0].deltas,
        unavailable(Unavailable::Baseline)
    );
    assert_eq!(derived.totals[0].members, 1);
}

#[test]
fn names_an_exit_by_the_start_of_its_own_tick() {
    // Goal: a process that started and exited between two ticks belongs to the program its
    // start read.
    // Method: derive a previous tick without the process and a selected tick holding its start
    // and then its exit.
    let previous = Columns::of(1, &[]);
    let selected = Columns::of(2, &[])
        .with_starts(&[Event::default()])
        .with_exits(&[Exit::default()]);

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes[0].program, Ok(0));
    assert_eq!(derived.processes[0].state, ProcessState::ExitedVerified);
    assert_eq!(derived.totals[0].members, 1);
}

#[test]
fn prefers_the_exec_of_its_own_tick_over_the_previous_event() {
    // Goal: a program the process ran later names its exit in place of the one an earlier
    // event read.
    // Method: derive a previous tick holding the exec of pid 7 to Safari and a selected tick
    // holding its exec to git and then its exit.
    let (previous, selected) = noticed_before();
    let selected = selected.with_execs(&[Event {
        number: 8,
        program: Ok(1),
        ..Event::default()
    }]);

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes[0].program, Ok(1));
    assert_eq!(derived.totals.len(), 1);
}

#[test]
fn prefers_the_start_of_its_own_tick_over_the_previous_event() {
    // Goal: a start the selected tick noticed names the exit in place of an earlier event.
    // Method: derive a previous tick holding the exec of pid 7 to Safari and a selected tick
    // holding its start reading git and then its exit.
    let (previous, selected) = noticed_before();
    let selected = selected.with_starts(&[Event {
        number: 8,
        program: Ok(1),
        ..Event::default()
    }]);

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes[0].program, Ok(1));
}

#[test]
fn prefers_the_exec_of_its_own_tick_over_the_previous_row() {
    // Goal: a program the process ran after the previous tick read it names its exit.
    // Method: derive a previous tick reading pid 7 live in Safari and a selected tick holding
    // its exec to git and then its exit.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(2, &[])
        .with_execs(&[Event {
            number: 8,
            program: Ok(1),
            ..Event::default()
        }])
        .with_exits(&[Exit::default()]);

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes[0].program, Ok(1));
}

#[test]
fn keeps_the_previous_row_ahead_of_the_previous_events() {
    // Goal: the previous tick's row of a process names its exit before that tick's events do.
    // Method: derive a previous tick reading pid 7 live with its program refused and holding
    // its exec to Safari, and a selected tick holding its exit.
    let (previous, selected) = noticed_before();
    let previous = Columns {
        execs: previous.execs,
        ..Columns::of(
            1,
            &[Process {
                program: Err(ProgramUnavailable::Refused),
                ..Process::default()
            }],
        )
    };

    let derived = derive(&previous, &selected);

    assert_eq!(
        derived.processes[0].program,
        Err(ProgramUnavailable::Refused)
    );
    assert_eq!(derived.ungrouped_members, 1);
}

#[test]
fn leaves_an_exit_whose_start_read_another_pid_version_out_of_every_app() {
    // Goal: a start of the process at another pid version than its exit leaves its program at
    // the exit unknown, though an earlier event named it.
    // Method: derive a previous tick holding the exec of pid 7 to Safari and a selected tick
    // holding its start at pid version 3 and then its exit at pid version 1.
    let (previous, selected) = noticed_before();
    let selected = selected.with_starts(&[Event {
        number: 8,
        creation: Some(Creation {
            pid_version: 3,
            ..CREATION
        }),
        program: Err(ProgramUnavailable::Refused),
        ..Event::default()
    }]);

    let derived = derive(&previous, &selected);

    assert_eq!(
        derived.processes[0].program,
        Err(ProgramUnavailable::Changed)
    );
    assert_eq!(derived.totals, []);
    assert_eq!(derived.ungrouped_members, 1);
}

#[test]
fn ignores_the_starts_after_an_exit() {
    // Goal: an event after the exit does not change the program the process ran at it.
    // Method: derive a previous tick holding the exec of pid 7 to Safari and a selected tick
    // holding its exit and then two starts of it.
    let (previous, selected) = noticed_before();
    let selected = selected.with_starts(&[12, 13].map(|number| Event {
        number,
        program: Err(ProgramUnavailable::Refused),
        ..Event::default()
    }));

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes[0].program, Ok(0));
}

#[test]
fn leaves_an_exit_between_two_starts_of_its_process_out_of_every_app() {
    // Goal: two starts of one process, one before its exit and one after it, leave its
    // program at the exit unknown.
    // Method: derive a previous tick holding the exec of pid 7 to Safari and a selected tick
    // holding a start of it, its exit, and a second start of it.
    let (previous, selected) = noticed_before();
    let selected = selected.with_starts(&[8, 12].map(|number| Event {
        number,
        program: Err(ProgramUnavailable::Refused),
        ..Event::default()
    }));

    let derived = derive(&previous, &selected);

    assert_eq!(
        derived.processes[0].program,
        Err(ProgramUnavailable::Changed)
    );
    assert_eq!(derived.ungrouped_members, 1);
}

#[test]
fn names_an_exit_by_the_last_exec_before_it() {
    // Goal: the program a process ran last before it exited names its exit.
    // Method: derive a previous tick without the process and a selected tick holding its execs
    // to Safari and to git, its exit, and then an exec it could not have run.
    let previous = Columns::of(1, &[]);
    let selected = Columns::of(2, &[])
        .with_execs(&[
            Event {
                number: 6,
                ..Event::default()
            },
            Event {
                number: 8,
                program: Ok(1),
                ..Event::default()
            },
            Event {
                number: 12,
                program: Err(ProgramUnavailable::Refused),
                ..Event::default()
            },
        ])
        .with_exits(&[Exit::default()]);

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes[0].program, Ok(1));
}

#[test]
fn leaves_the_program_changed_after_an_exec_of_another_creation_than_the_previous_row() {
    // Goal: an exec of the pid between the previous tick's row and the exit, of another
    // creation, leaves the program the row read unknown at the exit.
    // Method: derive a previous tick reading pid 7 live in Safari and a selected tick holding
    // an exec of pid 7 without its creation and then the exit.
    let previous = Columns::of(1, &[Process::default()]);
    let selected = Columns::of(2, &[])
        .with_execs(&[Event {
            number: 8,
            creation: None,
            ..Event::default()
        }])
        .with_exits(&[Exit::default()]);

    let derived = derive(&previous, &selected);

    assert_eq!(
        derived.processes[0].program,
        Err(ProgramUnavailable::Changed)
    );
}

#[test]
fn names_an_exit_by_its_start_after_an_exec_of_another_creation() {
    // Goal: where the last exec of the pid before the exit is of another creation, the exited
    // process's start names its program, ahead of an earlier event.
    // Method: derive a previous tick holding the exec of pid 7 to Safari and a selected tick
    // holding an exec of another unique id of pid 7, a start of the exited process reading git,
    // and its exit.
    let (previous, selected) = noticed_before();
    let selected = selected
        .with_execs(&[Event {
            number: 6,
            creation: Some(Creation {
                unique_id: 600,
                ..CREATION
            }),
            ..Event::default()
        }])
        .with_starts(&[Event {
            number: 8,
            program: Ok(1),
            ..Event::default()
        }]);

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes[0].program, Ok(1));
}

#[test]
fn ignores_the_starts_of_other_processes_of_the_pid() {
    // Goal: a start of an earlier or a later process of the pid does not name the exit of
    // another one.
    // Method: derive a previous tick holding the exec of pid 7 to Safari and a selected tick
    // holding starts of pid 7 of a lower and of a higher unique id and then the exit.
    let (previous, selected) = noticed_before();
    let selected = selected.with_starts(&[600, 800].map(|unique_id| Event {
        number: 8,
        creation: Some(Creation {
            unique_id,
            ..CREATION
        }),
        program: Ok(1),
        ..Event::default()
    }));

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes[0].program, Ok(0));
}

#[test]
fn leaves_out_an_exit_whose_start_an_exec_followed() {
    // Goal: an exec between a process's start and its exit leaves the program its start read
    // unknown at the exit.
    // Method: derive a previous tick without the process and a selected tick holding its
    // start, an exec of the pid without its creation, the exit, and a later exec.
    let previous = Columns::of(1, &[]);
    let unassociated = Event {
        creation: None,
        ..Event::default()
    };
    let selected = Columns::of(2, &[])
        .with_starts(&[Event::default()])
        .with_execs(&[7, 12].map(|number| Event {
            number,
            ..unassociated
        }))
        .with_exits(&[Exit::default()]);

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes, []);
}

/// Declares a test that an exec the selected tick read without its program names the exit's
/// program unknown, for the reason the exec gives.
macro_rules! unread_exec_tests {
    ($($name:ident, $creation:expr, $reason:expr;)*) => {
        $(
            #[test]
            fn $name() {
                // Goal: an exec after an earlier event names the process's program, even where
                // the collector could not read it, so the earlier program does not stand.
                // Method: derive a previous tick holding the exec of pid 7 to Safari and a
                // selected tick holding an exec of pid 7 without its program, then the exit.
                let (previous, selected) = noticed_before();
                let selected = selected.with_execs(&[Event {
                    number: 8,
                    creation: $creation,
                    program: Err(ProgramUnavailable::Refused),
                    ..Event::default()
                }]);

                let derived = derive(&previous, &selected);

                assert_eq!(derived.processes[0].program, Err($reason));
                assert_eq!(derived.processes[0].resident_bytes_last, Some(300));
                assert_eq!(derived.totals, []);
                assert_eq!(derived.ungrouped_members, 1);
            }
        )*
    };
}

unread_exec_tests! {
    names_the_reason_an_exec_before_the_exit_was_not_read,
        Some(CREATION), ProgramUnavailable::Refused;
    leaves_the_program_changed_after_an_exec_of_another_creation,
        None, ProgramUnavailable::Changed;
}

#[test]
fn leaves_out_an_exit_after_an_exec_of_another_creation() {
    // Goal: an exec of another creation of the pid names no program for an exit no reading
    // before it named.
    // Method: derive a previous tick without the process and a selected tick holding an exec
    // of pid 7 without its creation and then the exit.
    let previous = Columns::of(1, &[]);
    let selected = Columns::of(2, &[])
        .with_execs(&[Event {
            number: 8,
            creation: None,
            ..Event::default()
        }])
        .with_exits(&[Exit::default()]);

    let derived = derive(&previous, &selected);

    assert_eq!(derived.processes, []);
}

/// Declares a test that an event of the previous tick names no exited process.
macro_rules! unnamed_exit_tests {
    ($($name:ident, $change:expr;)*) => {
        $(
            #[test]
            fn $name() {
                // Goal: an event of the previous tick names no exited process where the ticks
                // do not span one interval, where it is of another process, or where it leaves
                // the process's program unknown.
                // Method: derive a previous tick holding only the exec of pid 7 and a selected
                // tick holding only its exit, with one change to either.
                let (mut previous, mut selected) = noticed_before();
                let change: fn(&mut Columns, &mut Columns) = $change;
                change(&mut previous, &mut selected);

                let derived = derive(&previous, &selected);

                assert_eq!(derived.processes, []);
                assert_eq!(derived.totals, []);
            }
        )*
    };
}

unnamed_exit_tests! {
    names_no_exit_by_an_event_of_another_recording,
        |previous, _| previous.stamp.recording = [9, 9];
    names_no_exit_by_an_event_of_another_boot, |previous, _| previous.stamp.boot = [9, 9];
    names_no_exit_by_an_event_of_another_run, |previous, _| previous.stamp.run = [9, 9];
    names_no_exit_by_an_event_across_a_missed_tick,
        |_, selected| selected.stamp.publication = 3;
    names_no_exit_by_an_event_without_a_continuous_clock,
        |previous, _| previous.stamp.continuous_ns = None;
    names_no_exit_by_an_event_after_the_continuous_clock_went_back,
        |previous, _| previous.stamp.continuous_ns = Some(3_000_000_000);
    names_no_exit_by_an_exec_of_a_reused_pid, |previous, _| {
        previous.execs.creation[0] = Some(Creation {
            unique_id: 701,
            ..CREATION
        });
    };
    names_no_exit_by_an_exec_of_another_pid_version, |previous, _| {
        previous.execs.creation[0] = Some(Creation {
            pid_version: 3,
            ..CREATION
        });
    };
    names_no_exit_by_one_of_two_starts, |previous, _| {
        previous.execs = EventColumns::default();
        previous.starts = EventColumns::of(&[4, 5].map(|number| Event {
            number,
            ..Event::default()
        }));
    };
    names_no_exit_by_a_start_before_an_exec, |previous, _| {
        previous.execs = EventColumns::of(&[Event {
            number: 6,
            creation: None,
            ..Event::default()
        }]);
        previous.starts = EventColumns::of(&[Event::default()]);
    };
}

/// A previous tick holding only the exec of pid 7 to Safari, and a selected tick holding only
/// its exit, with 300 bytes resident.
fn noticed_before() -> (Columns, Columns) {
    let previous = Columns::of(1, &[]).with_execs(&[Event::default()]);
    let selected = Columns::of(2, &[]).with_exits(&[Exit {
        resident_bytes: 300,
        ..Exit::default()
    }]);
    (previous, selected)
}

struct Derived {
    processes: Vec<ProcessTotals>,
    totals: Vec<Totals>,
    ungrouped_members: u32,
    live_ungrouped_members: u32,
}

/// Derives the Apps of `selected` after `previous`, whose processes run Safari as entry 0 and
/// git as entry 1.
fn derive(previous: &Columns, selected: &Columns) -> Derived {
    let mut programs = Programs::new(CAPACITY);
    assert_eq!(programs.add(SAFARI), 0);
    assert_eq!(programs.add(GIT), 1);
    let mut apps = Apps::new(PROCESSES_MAX, &programs);

    apps.derive(&programs, &previous.tick(), &selected.tick());

    Derived {
        processes: apps.processes().to_vec(),
        totals: apps.totals().to_vec(),
        ungrouped_members: apps.ungrouped_members(),
        live_ungrouped_members: apps.live_ungrouped_members(),
    }
}

fn measured(known: u64, members: u32) -> Subtotal {
    Subtotal {
        known,
        measured_members: members,
        unavailable_members: 0,
        overflowed: false,
    }
}

fn absent(members: u32) -> Subtotal {
    Subtotal {
        known: 0,
        measured_members: 0,
        unavailable_members: members,
        overflowed: false,
    }
}

/// The deltas of a process none of whose counters' changes are known, for `reason`.
fn unavailable(reason: Unavailable) -> Deltas {
    Deltas {
        cpu_ns: Amount::Unavailable(reason),
        energy_nj: Amount::Unavailable(reason),
        gpu_ns: Amount::Unavailable(reason),
        disk_read_bytes: Amount::Unavailable(reason),
        disk_written_bytes: Amount::Unavailable(reason),
        disk_io_bytes: Amount::Unavailable(reason),
        idle_wakeups: Amount::Unavailable(reason),
        pageins: Amount::Unavailable(reason),
    }
}

/// One process of a tick, as a test writes it.
#[derive(Clone, Copy)]
struct Process {
    pid: i32,
    creation: Option<Creation>,
    program: Result<u32, ProgramUnavailable>,
    usage: Measurement,
    awake_ns_at_start: u64,
    user_ns: u64,
    system_ns: u64,
    energy_nj: u64,
    gpu_ns: Option<u64>,
    disk_read_bytes: u64,
    disk_written_bytes: u64,
    idle_wakeups: u64,
    pageins: u64,
    resident_bytes: u64,
    footprint_bytes: u64,
    threads: Option<u64>,
}

impl Default for Process {
    fn default() -> Self {
        Self {
            pid: 7,
            creation: Some(CREATION),
            program: Ok(0),
            usage: Measurement::Measured,
            awake_ns_at_start: 0,
            user_ns: 0,
            system_ns: 0,
            energy_nj: 0,
            gpu_ns: Some(0),
            disk_read_bytes: 0,
            disk_written_bytes: 0,
            idle_wakeups: 0,
            pageins: 0,
            resident_bytes: 0,
            footprint_bytes: 0,
            threads: Some(1),
        }
    }
}

/// One exit of a tick, as a test writes it: the process's final usage.
#[derive(Clone, Copy)]
struct Exit {
    number: u64,
    pid: i32,
    creation: Option<Creation>,
    usage: Measurement,
    awake_ns_at_start: u64,
    user_ns: u64,
    system_ns: u64,
    energy_nj: u64,
    disk_read_bytes: u64,
    disk_written_bytes: u64,
    idle_wakeups: u64,
    pageins: u64,
    resident_bytes: u64,
    footprint_bytes: u64,
}

impl Default for Exit {
    fn default() -> Self {
        Self {
            number: 10,
            pid: 7,
            creation: Some(CREATION),
            usage: Measurement::Measured,
            awake_ns_at_start: 0,
            user_ns: 0,
            system_ns: 0,
            energy_nj: 0,
            disk_read_bytes: 0,
            disk_written_bytes: 0,
            idle_wakeups: 0,
            pageins: 0,
            resident_bytes: 0,
            footprint_bytes: 0,
        }
    }
}

/// One start or exec of a tick, as a test writes it: the program the collector read when it
/// noticed the event.
#[derive(Clone, Copy)]
struct Event {
    number: u64,
    pid: i32,
    creation: Option<Creation>,
    program: Result<u32, ProgramUnavailable>,
}

impl Default for Event {
    fn default() -> Self {
        Self {
            number: 5,
            pid: 7,
            creation: Some(CREATION),
            program: Ok(0),
        }
    }
}

const CREATION: Creation = Creation {
    unique_id: 700,
    pid_version: 1,
};

/// The usage columns of a tick's processes or of its exits.
#[derive(Default)]
struct UsageColumns {
    measurement: Vec<Measurement>,
    awake_ns_at_start: Vec<u64>,
    user_ns: Vec<u64>,
    system_ns: Vec<u64>,
    energy_nj: Vec<u64>,
    disk_read_bytes: Vec<u64>,
    disk_written_bytes: Vec<u64>,
    idle_wakeups: Vec<u64>,
    pageins: Vec<u64>,
    resident_bytes: Vec<u64>,
    footprint_bytes: Vec<u64>,
}

/// One row of usage columns.
struct UsageRow {
    measurement: Measurement,
    awake_ns_at_start: u64,
    user_ns: u64,
    system_ns: u64,
    energy_nj: u64,
    disk_read_bytes: u64,
    disk_written_bytes: u64,
    idle_wakeups: u64,
    pageins: u64,
    resident_bytes: u64,
    footprint_bytes: u64,
}

impl UsageColumns {
    fn of(rows: impl Iterator<Item = UsageRow>) -> Self {
        let mut columns = Self::default();
        for row in rows {
            columns.measurement.push(row.measurement);
            columns.awake_ns_at_start.push(row.awake_ns_at_start);
            columns.user_ns.push(row.user_ns);
            columns.system_ns.push(row.system_ns);
            columns.energy_nj.push(row.energy_nj);
            columns.disk_read_bytes.push(row.disk_read_bytes);
            columns.disk_written_bytes.push(row.disk_written_bytes);
            columns.idle_wakeups.push(row.idle_wakeups);
            columns.pageins.push(row.pageins);
            columns.resident_bytes.push(row.resident_bytes);
            columns.footprint_bytes.push(row.footprint_bytes);
        }
        columns
    }

    fn usages(&self) -> Usages<'_> {
        Usages {
            measurement: &self.measurement,
            awake_ns_at_start: &self.awake_ns_at_start,
            user_ns: &self.user_ns,
            system_ns: &self.system_ns,
            energy_nj: &self.energy_nj,
            disk_read_bytes: &self.disk_read_bytes,
            disk_written_bytes: &self.disk_written_bytes,
            idle_wakeups: &self.idle_wakeups,
            pageins: &self.pageins,
            resident_bytes: &self.resident_bytes,
            footprint_bytes: &self.footprint_bytes,
        }
    }
}

impl Process {
    fn usage(&self) -> UsageRow {
        UsageRow {
            measurement: self.usage,
            awake_ns_at_start: self.awake_ns_at_start,
            user_ns: self.user_ns,
            system_ns: self.system_ns,
            energy_nj: self.energy_nj,
            disk_read_bytes: self.disk_read_bytes,
            disk_written_bytes: self.disk_written_bytes,
            idle_wakeups: self.idle_wakeups,
            pageins: self.pageins,
            resident_bytes: self.resident_bytes,
            footprint_bytes: self.footprint_bytes,
        }
    }
}

impl Exit {
    fn usage(&self) -> UsageRow {
        UsageRow {
            measurement: self.usage,
            awake_ns_at_start: self.awake_ns_at_start,
            user_ns: self.user_ns,
            system_ns: self.system_ns,
            energy_nj: self.energy_nj,
            disk_read_bytes: self.disk_read_bytes,
            disk_written_bytes: self.disk_written_bytes,
            idle_wakeups: self.idle_wakeups,
            pageins: self.pageins,
            resident_bytes: self.resident_bytes,
            footprint_bytes: self.footprint_bytes,
        }
    }
}

/// A tick's stamp and the columns of its processes and exits, which the rule borrows.
struct Columns {
    stamp: Stamp,
    pid: Vec<i32>,
    creation: Vec<Option<Creation>>,
    program: Vec<Result<u32, ProgramUnavailable>>,
    usage: UsageColumns,
    gpu_ns: Vec<Option<u64>>,
    threads: Vec<Option<u64>>,
    exits: ExitColumns,
    starts: EventColumns,
    execs: EventColumns,
}

/// The columns of a tick's exits.
#[derive(Default)]
struct ExitColumns {
    number: Vec<u64>,
    pid: Vec<i32>,
    creation: Vec<Option<Creation>>,
    usage: UsageColumns,
}

impl Columns {
    /// The tick of `publication` of one run, a second after the one before it.
    fn of(publication: u64, rows: &[Process]) -> Self {
        Self {
            stamp: Stamp {
                recording: [1, 1],
                boot: [2, 2],
                run: [3, 3],
                publication,
                continuous_ns: Some(publication * 1_000_000_000),
            },
            pid: rows.iter().map(|row| row.pid).collect(),
            creation: rows.iter().map(|row| row.creation).collect(),
            program: rows.iter().map(|row| row.program).collect(),
            usage: UsageColumns::of(rows.iter().map(Process::usage)),
            gpu_ns: rows.iter().map(|row| row.gpu_ns).collect(),
            threads: rows.iter().map(|row| row.threads).collect(),
            exits: ExitColumns::default(),
            starts: EventColumns::default(),
            execs: EventColumns::default(),
        }
    }

    /// The tick with `exits`, which a test writes in the order the rule reads them.
    fn with_exits(mut self, exits: &[Exit]) -> Self {
        self.exits = ExitColumns {
            number: exits.iter().map(|exit| exit.number).collect(),
            pid: exits.iter().map(|exit| exit.pid).collect(),
            creation: exits.iter().map(|exit| exit.creation).collect(),
            usage: UsageColumns::of(exits.iter().map(Exit::usage)),
        };
        self
    }

    /// The tick with `starts`, which a test writes in the order the rule reads them.
    fn with_starts(mut self, starts: &[Event]) -> Self {
        self.starts = EventColumns::of(starts);
        self
    }

    /// The tick with `execs`, which a test writes in the order the rule reads them.
    fn with_execs(mut self, execs: &[Event]) -> Self {
        self.execs = EventColumns::of(execs);
        self
    }

    fn tick(&self) -> Tick<'_> {
        Tick {
            stamp: self.stamp,
            processes: Processes {
                pid: &self.pid,
                creation: &self.creation,
                program: &self.program,
                usage: self.usage.usages(),
                gpu_ns: &self.gpu_ns,
                threads: &self.threads,
            },
            exits: Exits {
                number: &self.exits.number,
                pid: &self.exits.pid,
                creation: &self.exits.creation,
                usage: self.exits.usage.usages(),
            },
            starts: Starts {
                number: &self.starts.number,
                pid: &self.starts.pid,
                creation: &self.starts.creation,
                program: &self.starts.program,
            },
            execs: Execs {
                number: &self.execs.number,
                pid: &self.execs.pid,
                creation: &self.execs.creation,
                program: &self.execs.program,
            },
        }
    }
}

/// The columns of a tick's starts or of its execs.
#[derive(Default)]
struct EventColumns {
    number: Vec<u64>,
    pid: Vec<i32>,
    creation: Vec<Option<Creation>>,
    program: Vec<Result<u32, ProgramUnavailable>>,
}

impl EventColumns {
    fn of(events: &[Event]) -> Self {
        Self {
            number: events.iter().map(|event| event.number).collect(),
            pid: events.iter().map(|event| event.pid).collect(),
            creation: events.iter().map(|event| event.creation).collect(),
            program: events.iter().map(|event| event.program).collect(),
        }
    }
}
