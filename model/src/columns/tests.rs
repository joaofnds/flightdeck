use super::{EventRow, ExitRow, Full, ProcessRow, Refusal, TickColumns};
use crate::apps::{Creation, Measurement, ProgramUnavailable, Stamp, Usage};

const STAMP: Stamp = Stamp {
    recording: [1, 2],
    boot: [3, 4],
    run: [5, 6],
    publication: 7,
    continuous_ns: Some(8),
};

const USAGE: Usage = Usage {
    awake_ns_at_start: 1,
    user_ns: 2,
    system_ns: 3,
    energy_nj: 4,
    disk_read_bytes: 5,
    disk_written_bytes: 6,
    idle_wakeups: 7,
    pageins: 8,
    resident_bytes: 9,
    footprint_bytes: 10,
};

fn process(pid: i32) -> ProcessRow {
    ProcessRow {
        pid,
        creation: Some(creation(u64::from(pid.unsigned_abs()))),
        program: Ok(0),
        usage: Ok(USAGE),
        gpu_ns: None,
        threads: Some(1),
    }
}

fn creation(unique_id: u64) -> Creation {
    Creation {
        unique_id,
        pid_version: 0,
    }
}

fn event(number: u64, pid: i32, creation: Option<Creation>) -> EventRow {
    EventRow {
        number,
        pid,
        creation,
        program: Ok(0),
    }
}

#[test]
fn holds_its_processes_in_pid_order_with_the_tick_stamp() {
    // Goal: the tick reads back the stamp it started with and each process as it was added.
    // Method: start a tick, add pids 4 and 9, and read the tick's columns.
    let mut columns = TickColumns::new(4, 4);
    columns.start(STAMP);

    for pid in [4, 9] {
        assert_eq!(columns.push_process(&process(pid)), Ok(()));
    }
    columns.finish();
    let tick = columns.tick();

    assert_eq!(tick.stamp, STAMP);
    assert_eq!(tick.processes.pid, [4, 9]);
    assert_eq!(tick.processes.usage.user_ns, [2, 2]);
    assert_eq!(tick.processes.threads, [Some(1), Some(1)]);
}

#[test]
fn puts_events_in_the_orders_the_app_rule_reads() {
    // Goal: exits and starts sort by pid, then unique id with a missing creation first, then
    // number, and execs by pid and then number, whatever order they were added in.
    // Method: add each kind out of order and read the numbers back.
    let mut columns = TickColumns::new(4, 4);
    columns.start(STAMP);
    let exits = [
        (30, 9, Some(creation(2))),
        (31, 9, None),
        (32, 4, Some(creation(5))),
        (33, 9, Some(creation(1))),
    ];
    let starts = [(40, 9, Some(creation(1))), (41, 4, None), (42, 9, None)];
    let execs = [(52, 4, None), (50, 9, None), (51, 4, None)];

    for (number, pid, creation) in exits {
        let exit = ExitRow {
            number,
            pid,
            creation,
            usage: Ok(USAGE),
        };
        assert_eq!(columns.push_exit(&exit), Ok(()));
    }
    for (number, pid, creation) in starts {
        assert_eq!(columns.push_start(&event(number, pid, creation)), Ok(()));
    }
    for (number, pid, creation) in execs {
        assert_eq!(columns.push_exec(&event(number, pid, creation)), Ok(()));
    }
    columns.finish();
    let tick = columns.tick();

    assert_eq!(tick.exits.number, [32, 31, 33, 30]);
    assert_eq!(tick.starts.number, [41, 42, 40]);
    assert_eq!(tick.execs.number, [51, 52, 50]);
}

#[test]
fn keeps_the_reason_a_usage_was_not_read() {
    // Goal: a process whose usage was not read reads back as not measured, for why it was not.
    // Method: add a refused process and a gone one beside a measured one.
    let mut columns = TickColumns::new(4, 4);
    columns.start(STAMP);
    let refused = ProcessRow {
        usage: Err(Measurement::Refused),
        ..process(2)
    };
    let gone = ProcessRow {
        usage: Err(Measurement::Gone),
        program: Err(ProgramUnavailable::Gone),
        ..process(3)
    };

    for row in [process(1), refused, gone] {
        assert_eq!(columns.push_process(&row), Ok(()));
    }
    columns.finish();
    let tick = columns.tick();

    assert_eq!(
        tick.processes.usage.measurement,
        [
            Measurement::Measured,
            Measurement::Refused,
            Measurement::Gone
        ]
    );
    assert_eq!(tick.processes.program[2], Err(ProgramUnavailable::Gone));
}

#[test]
fn starting_a_tick_empties_the_one_before() {
    // Goal: no row of a tick survives into the next one.
    // Method: fill a tick with a process and an exit, start another, and read it.
    let mut columns = TickColumns::new(4, 4);
    columns.start(STAMP);
    assert_eq!(columns.push_process(&process(4)), Ok(()));
    let exit = ExitRow {
        number: 1,
        pid: 5,
        creation: None,
        usage: Ok(USAGE),
    };
    assert_eq!(columns.push_exit(&exit), Ok(()));
    columns.finish();

    columns.start(Stamp {
        publication: 8,
        ..STAMP
    });
    columns.finish();
    let tick = columns.tick();

    assert_eq!(tick.stamp.publication, 8);
    assert!(tick.processes.pid.is_empty());
    assert!(tick.exits.pid.is_empty());
}

mod when_a_row_does_not_fit {
    use super::{ExitRow, Full, Refusal, STAMP, TickColumns, USAGE, event, process};

    #[test]
    fn refuses_a_pid_not_above_the_one_before() {
        // Goal: the processes stay in strictly ascending pid order.
        // Method: add pid 9, then 9 again and 4, and read the tick.
        let mut columns = TickColumns::new(4, 4);
        columns.start(STAMP);
        assert_eq!(columns.push_process(&process(9)), Ok(()));

        let refusals = [
            columns.push_process(&process(9)),
            columns.push_process(&process(4)),
        ];
        columns.finish();

        assert_eq!(refusals, [Err(Refusal::Order), Err(Refusal::Order)]);
        assert_eq!(columns.tick().processes.pid, [9]);
    }

    #[test]
    fn refuses_a_process_past_its_room() {
        // Goal: the columns never grow past the processes they were sized for.
        // Method: size them for two processes and add three.
        let mut columns = TickColumns::new(2, 4);
        columns.start(STAMP);
        assert_eq!(columns.push_process(&process(1)), Ok(()));
        assert_eq!(columns.push_process(&process(2)), Ok(()));

        let refusal = columns.push_process(&process(3));

        assert_eq!(refusal, Err(Refusal::Full));
    }

    #[test]
    fn refuses_an_event_past_its_room() {
        // Goal: each kind of event has the room it was sized for, and no more.
        // Method: size the columns for one event of each kind and add two of each.
        let mut columns = TickColumns::new(4, 1);
        columns.start(STAMP);
        let exit = ExitRow {
            number: 1,
            pid: 5,
            creation: None,
            usage: Ok(USAGE),
        };
        assert_eq!(columns.push_exit(&exit), Ok(()));
        assert_eq!(columns.push_start(&event(2, 5, None)), Ok(()));
        assert_eq!(columns.push_exec(&event(3, 5, None)), Ok(()));

        let refusals = [
            columns.push_exit(&exit),
            columns.push_start(&event(4, 5, None)),
            columns.push_exec(&event(5, 5, None)),
        ];

        assert_eq!(refusals, [Err(Full); 3]);
    }
}
