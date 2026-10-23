use super::{Apps, Processes, Subtotal, Totals};
use crate::programs::{Capacity, Program, Programs};

const CAPACITY: Capacity = Capacity { entries: 8 };
const PROCESSES_MAX: u32 = 8;
const SAFARI: Program = Program {
    executable_name: b"Safari",
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
