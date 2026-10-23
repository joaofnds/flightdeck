use super::{
    Amount, Apps, Creation, Measurement, ProcessTotals, Processes, Stamp, Subtotal, Tick, Totals,
    Unavailable,
};
use crate::programs::{Capacity, Program, Programs};

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
            program: Some(entry),
            user_ns: 7_000_000,
            system_ns: 3_000_000,
            ..Process::default()
        }],
    );
    let selected = Columns::of(
        2,
        &[Process {
            program: Some(entry),
            user_ns: 10_000_000,
            system_ns: 5_000_000,
            ..Process::default()
        }],
    );
    let mut apps = Apps::new(PROCESSES_MAX, &programs);

    apps.derive(&programs, &previous.tick(), &selected.tick());

    assert_eq!(entry, 0);
    assert_eq!(
        apps.totals(),
        [Totals {
            app: programs.app(entry),
            members: 1,
            cpu_ns: measured(5_000_000, 1),
        }]
    );
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
                program: Some(main),
                ..Process::default()
            },
            Process {
                pid: 8,
                program: Some(helper),
                ..Process::default()
            },
        ],
    );
    let selected = Columns::of(
        2,
        &[
            Process {
                program: Some(main),
                user_ns: 5_000_000,
                ..Process::default()
            },
            Process {
                pid: 8,
                program: Some(helper),
                user_ns: 2_000_000,
                ..Process::default()
            },
        ],
    );
    let mut apps = Apps::new(PROCESSES_MAX, &programs);

    apps.derive(&programs, &previous.tick(), &selected.tick());

    assert_eq!(
        apps.totals(),
        [Totals {
            app: programs.app(main),
            members: 2,
            cpu_ns: measured(7_000_000, 2),
        }]
    );
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

    assert_eq!(
        derived.processes,
        [ProcessTotals {
            cpu_ns: Amount::Known(5_000_000),
            app_attribution: None,
        }]
    );
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

                assert_eq!(derived.processes, [unavailable(Unavailable::Interval)]);
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

    assert_eq!(derived.processes, [unavailable(Unavailable::Interval)]);
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

    assert_eq!(derived.processes, [unavailable(Unavailable::Baseline)]);
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

    assert_eq!(derived.processes, [unavailable(Unavailable::Baseline)]);
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

    assert_eq!(derived.processes, [unavailable(Unavailable::Baseline)]);
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

    assert_eq!(derived.processes, [unavailable(Unavailable::CounterReset)]);
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

    assert_eq!(derived.processes, [unavailable(Unavailable::Overflow)]);
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

    assert_eq!(derived.processes, [unavailable(Unavailable::Creation)]);
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

                assert_eq!(derived.processes, [unavailable($reason)]);
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

    assert_eq!(derived.processes, [unavailable(Unavailable::Interval)]);
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
            program: Some(git),
            ..Process::default()
        }],
    );
    let selected = Columns::of(
        2,
        &[Process {
            program: Some(safari),
            user_ns: 3_000_000,
            ..Process::default()
        }],
    );
    let mut apps = Apps::new(PROCESSES_MAX, &programs);

    apps.derive(&programs, &previous.tick(), &selected.tick());

    assert_eq!(
        apps.processes(),
        [ProcessTotals {
            cpu_ns: Amount::Known(3_000_000),
            app_attribution: Some(Unavailable::Exec),
        }]
    );
    assert_eq!(
        apps.totals(),
        [Totals {
            app: programs.app(safari),
            members: 1,
            cpu_ns: absent(1),
        }]
    );
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

    assert_eq!(
        derived.processes,
        [ProcessTotals {
            cpu_ns: Amount::Known(3_000_000),
            app_attribution: Some(Unavailable::Exec),
        }]
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
            program: None,
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
            program: None,
            user_ns: 3_000_000,
            ..Process::default()
        }],
    );

    let derived = derive(&previous, &selected);

    assert_eq!(
        derived.processes,
        [ProcessTotals {
            cpu_ns: Amount::Known(3_000_000),
            app_attribution: Some(Unavailable::Program),
        }]
    );
    assert_eq!(derived.totals, []);
    assert_eq!(derived.ungrouped_members, 1);
}

/// What a derivation of two ticks of processes of Safari gave.
struct Derived {
    processes: Vec<ProcessTotals>,
    totals: Vec<Totals>,
    ungrouped_members: u32,
}

/// Derives the Apps of `selected` after `previous`, whose processes run Safari as entry 0.
fn derive(previous: &Columns, selected: &Columns) -> Derived {
    let mut programs = Programs::new(CAPACITY);
    assert_eq!(programs.add(SAFARI), 0);
    let mut apps = Apps::new(PROCESSES_MAX, &programs);

    apps.derive(&programs, &previous.tick(), &selected.tick());

    Derived {
        processes: apps.processes().to_vec(),
        totals: apps.totals().to_vec(),
        ungrouped_members: apps.ungrouped_members(),
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

fn unavailable(reason: Unavailable) -> ProcessTotals {
    ProcessTotals {
        cpu_ns: Amount::Unavailable(reason),
        app_attribution: None,
    }
}

/// One process of a tick, as a test writes it.
#[derive(Clone, Copy)]
struct Process {
    pid: i32,
    creation: Option<Creation>,
    program: Option<u32>,
    usage: Measurement,
    awake_ns_at_start: u64,
    user_ns: u64,
    system_ns: u64,
}

impl Default for Process {
    fn default() -> Self {
        Self {
            pid: 7,
            creation: Some(Creation {
                unique_id: 700,
                pid_version: 1,
            }),
            program: Some(0),
            usage: Measurement::Measured,
            awake_ns_at_start: 0,
            user_ns: 0,
            system_ns: 0,
        }
    }
}

/// A tick's stamp and the columns of its processes, which the rule borrows.
struct Columns {
    stamp: Stamp,
    pid: Vec<i32>,
    creation: Vec<Option<Creation>>,
    program: Vec<Option<u32>>,
    usage: Vec<Measurement>,
    awake_ns_at_start: Vec<u64>,
    user_ns: Vec<u64>,
    system_ns: Vec<u64>,
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
            usage: rows.iter().map(|row| row.usage).collect(),
            awake_ns_at_start: rows.iter().map(|row| row.awake_ns_at_start).collect(),
            user_ns: rows.iter().map(|row| row.user_ns).collect(),
            system_ns: rows.iter().map(|row| row.system_ns).collect(),
        }
    }

    fn tick(&self) -> Tick<'_> {
        Tick {
            stamp: self.stamp,
            processes: Processes {
                pid: &self.pid,
                creation: &self.creation,
                program: &self.program,
                usage: &self.usage,
                awake_ns_at_start: &self.awake_ns_at_start,
                user_ns: &self.user_ns,
                system_ns: &self.system_ns,
            },
        }
    }
}
