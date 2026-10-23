use super::{Apps, Processes, Subtotal, Totals};
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

#[test]
fn attributes_the_cpu_a_process_spent_between_two_ticks_to_its_app() {
    // Goal: an App takes the CPU time its one process spent between two ticks.
    // Method: index one bundled program as the ring adapter hands out its first entry, then
    // derive the Apps of two ticks of one process of it, whose CPU time grows by 5 ms.
    let mut programs = Programs::new(CAPACITY);
    let entry = programs.add(SAFARI);
    let previous = Columns::of(&[Process {
        program: entry,
        user_ns: 7_000_000,
        system_ns: 3_000_000,
        ..Process::default()
    }]);
    let selected = Columns::of(&[Process {
        program: entry,
        user_ns: 10_000_000,
        system_ns: 5_000_000,
        ..Process::default()
    }]);
    let mut apps = Apps::new(PROCESSES_MAX, &programs);

    apps.derive(&programs, &previous.processes(), &selected.processes());

    assert_eq!(entry, 0);
    assert_eq!(
        apps.totals(),
        [Totals {
            app: programs.app(entry),
            members: 1,
            cpu_ns: Subtotal {
                known: 5_000_000,
                measured_members: 1,
                unavailable_members: 0,
                overflowed: false,
            },
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
    let previous = Columns::of(&[
        Process {
            program: main,
            ..Process::default()
        },
        Process {
            pid: 8,
            program: helper,
            ..Process::default()
        },
    ]);
    let selected = Columns::of(&[
        Process {
            program: main,
            user_ns: 5_000_000,
            ..Process::default()
        },
        Process {
            pid: 8,
            program: helper,
            user_ns: 2_000_000,
            ..Process::default()
        },
    ]);
    let mut apps = Apps::new(PROCESSES_MAX, &programs);

    apps.derive(&programs, &previous.processes(), &selected.processes());

    assert_eq!(
        apps.totals(),
        [Totals {
            app: programs.app(main),
            members: 2,
            cpu_ns: Subtotal {
                known: 7_000_000,
                measured_members: 2,
                unavailable_members: 0,
                overflowed: false,
            },
        }]
    );
}

/// One process of a tick, as a test writes it.
#[derive(Clone, Copy)]
struct Process {
    pid: i32,
    creation: u64,
    program: u32,
    user_ns: u64,
    system_ns: u64,
}

impl Default for Process {
    fn default() -> Self {
        Self {
            pid: 7,
            creation: 700,
            program: 0,
            user_ns: 0,
            system_ns: 0,
        }
    }
}

/// The columns of a tick's processes, which the rule borrows.
struct Columns {
    pid: Vec<i32>,
    creation: Vec<u64>,
    program: Vec<u32>,
    user_ns: Vec<u64>,
    system_ns: Vec<u64>,
}

impl Columns {
    fn of(rows: &[Process]) -> Self {
        Self {
            pid: rows.iter().map(|row| row.pid).collect(),
            creation: rows.iter().map(|row| row.creation).collect(),
            program: rows.iter().map(|row| row.program).collect(),
            user_ns: rows.iter().map(|row| row.user_ns).collect(),
            system_ns: rows.iter().map(|row| row.system_ns).collect(),
        }
    }

    fn processes(&self) -> Processes<'_> {
        Processes {
            pid: &self.pid,
            creation: &self.creation,
            program: &self.program,
            user_ns: &self.user_ns,
            system_ns: &self.system_ns,
        }
    }
}
