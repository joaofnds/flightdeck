use std::sync::Arc;

use collector::processes::{Process, Task, Usage};
use collector::ring::{
    IdentityObservation, IdentityPublication, ObservationTimes, ProcessCreation, ProcessIdentity,
    ProgramBytes, Readings, UsageAvailability,
};

use super::name_tests::native_sample;
use super::{Payload, ProcessNames, ProcessRow};
use crate::apps::Amount;
use crate::cancellation::Cancellation;
use crate::live::identity_fixture::Fixture;
use crate::store::{AppsSecond, ByteBudget, Missing, ring::Ring};

const IDLE: u64 = 0;
const MOVED: u64 = 5;
const PROCESSES_MAX: u64 = 8;
const BUDGET_BYTES: usize = 256 * 1024 * 1024;

#[test]
fn a_row_equal_to_the_second_before_is_kept_once_for_both_seconds() {
    // Goal: a process whose readings repeat costs one row however many seconds hold it.
    // Method: publish one idle process for three ticks through the real producer and compare
    // where the second and third seconds keep its row.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));

    let payloads = payloads(&budget, &[&[(7, IDLE)], &[(7, IDLE)], &[(7, IDLE)]]);

    assert_eq!(row(&payloads[1], 7), row(&payloads[2], 7));
    assert!(std::ptr::eq(row(&payloads[1], 7), row(&payloads[2], 7)));
}

#[test]
fn a_row_that_changed_is_kept_apart_from_the_second_before() {
    // Goal: a process whose readings changed shows its new reading, and the idle processes
    // beside it still cost one row each.
    // Method: publish a process that spends more CPU time every tick between two idle ones and
    // compare where the second and third seconds keep each row.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));

    let payloads = payloads(
        &budget,
        &[
            &[(7, IDLE), (8, 1_000_000), (9, IDLE)],
            &[(7, IDLE), (8, 3_000_000), (9, IDLE)],
            &[(7, IDLE), (8, 6_000_000), (9, IDLE)],
        ],
    );

    assert_eq!(row(&payloads[1], 8).cpu_ns(), Amount::Known(2_000_000));
    assert_eq!(row(&payloads[2], 8).cpu_ns(), Amount::Known(3_000_000));
    assert!(std::ptr::eq(row(&payloads[1], 7), row(&payloads[2], 7)));
    assert!(std::ptr::eq(row(&payloads[1], 9), row(&payloads[2], 9)));
}

#[test]
fn a_row_is_kept_apart_whichever_reading_moved() {
    // Goal: a second never shows a reading of the second before, whichever reading moved.
    // Method: publish one process for eleven ticks, each from the third moving one reading the
    // ticks before it left alone, and count the rows the eleven seconds keep.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let ticks: Vec<Vec<Process>> = (1..=11).map(|tick| vec![moving(7, tick)]).collect();

    let payloads = payloads_of(&budget, &ticks);

    let rows: std::collections::HashSet<_> = payloads
        .iter()
        .map(|payload| std::ptr::from_ref(row(payload, 7)))
        .collect();
    assert_eq!(rows.len(), 11);
}

#[test]
fn a_row_stays_shared_when_a_process_listed_before_it_exits() {
    // Goal: one process leaving does not cost a new row for every process listed after it.
    // Method: publish two idle processes, drop the one with the lower pid from the third tick,
    // and compare where the second and third seconds keep the other's row.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));

    let payloads = payloads(
        &budget,
        &[
            &[(5, IDLE), (7, IDLE)],
            &[(5, IDLE), (7, IDLE)],
            &[(7, IDLE)],
        ],
    );

    assert!(std::ptr::eq(row(&payloads[1], 7), row(&payloads[2], 7)));
}

#[test]
fn a_row_stays_shared_when_a_process_listed_before_it_starts() {
    // Goal: one process arriving does not cost a new row for every process listed after it.
    // Method: publish one idle process, add one with a lower pid at the third tick, and compare
    // where the second and third seconds keep the first's row.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));

    let payloads = payloads(
        &budget,
        &[&[(7, IDLE)], &[(7, IDLE)], &[(5, IDLE), (7, IDLE)]],
    );

    assert!(std::ptr::eq(row(&payloads[1], 7), row(&payloads[2], 7)));
}

#[test]
fn seconds_that_repeat_a_row_are_charged_for_it_once() {
    // Goal: the budget counts a row once however many seconds hold it.
    // Method: publish one idle process for three ticks and compare the budget's use with the
    // payloads' own bytes, the process's name, the details of its App and two allocations of a
    // row, its two counts and its reservation: the first second's, which has no rate yet, and
    // the one the other two seconds share.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));

    let payloads = payloads(&budget, &[&[(7, IDLE)], &[(7, IDLE)], &[(7, IDLE)]]);

    let own: usize = payloads.iter().map(|payload| payload.bytes()).sum();
    let name = row(&payloads[0], 7)
        .executable_name()
        .unwrap()
        .reserved_bytes();
    let row_bytes = 2 * size_of::<usize>() + size_of::<super::Shared<ProcessRow>>();
    let details = crate::store::startup_tests::row_bytes(&[&payloads[0], &payloads[1]])
        - 2 * Payload::ROW_BYTES;
    assert_eq!(budget.used(), own + name + details + 2 * row_bytes);
}

#[test]
fn a_shared_row_stays_charged_while_a_later_second_holds_it() {
    // Goal: a row's charge lasts as long as the row, whichever second admitted it.
    // Method: publish one idle process for three ticks, let go of the second that admitted the
    // shared row, and compare the budget's use with what it was less that payload's own bytes.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let mut payloads = payloads(&budget, &[&[(7, IDLE)], &[(7, IDLE)], &[(7, IDLE)]]);
    let admitting = payloads.remove(1);
    let remaining = budget.used() - admitting.bytes();

    drop(admitting);

    assert_eq!(budget.used(), remaining);
}

#[test]
fn a_refused_row_leaves_the_budget_as_it_was() {
    // Goal: a second whose rows do not all fit takes nothing from the budget.
    // Method: capture a real Sample of two processes whose names the store already holds, with
    // room left for the payload's own bytes and one of its two rows.
    let sample = native_sample();
    let budget = Arc::new(ByteBudget::new(64 * 1024));
    let mut names = ProcessNames::new(sample.processes().len(), &budget).unwrap();
    let (payload, additions) = Payload::capture_with_names(&sample, &names, None).unwrap();
    names.commit(additions);
    drop(payload);
    let room = Payload::capture_bytes(&sample) + Payload::ROW_BYTES;
    let _pressure = budget
        .reserve(budget.maximum() - budget.used() - room)
        .unwrap();
    let before = budget.used();

    let refused = Payload::capture_with_names(&sample, &names, None);

    assert!(matches!(refused, Err(Missing::Budget)));
    assert_eq!(budget.used(), before);
}

#[test]
#[should_panic(expected = "the second before is charged to another budget")]
fn a_second_before_charged_to_another_budget_stops_the_capture() {
    // Goal: a row's charge never stays with a budget other than the one its second is admitted
    // under, so a capture offered such a second stops the program.
    // Method: capture one real Sample under one budget and offer its payload as the second
    // before to a capture under another.
    let sample = native_sample();
    let budget = Arc::new(ByteBudget::new(64 * 1024));
    let other_budget = Arc::new(ByteBudget::new(64 * 1024));
    let names = ProcessNames::new(sample.processes().len(), &budget).unwrap();
    let other_names = ProcessNames::new(sample.processes().len(), &other_budget).unwrap();
    let (payload, _additions) = Payload::capture_with_names(&sample, &other_names, None).unwrap();
    let previous = AppsSecond {
        processes: Ok(Arc::new(payload)),
        ..AppsSecond::default()
    };

    Payload::capture_with_names(&sample, &names, Some(&previous)).unwrap();
}

fn row(payload: &Payload, pid: i32) -> &ProcessRow {
    payload.iter().find(|row| row.pid == pid).unwrap()
}

/// What process `pid` has read by `tick`. Each reading stays as it was until its own tick, the
/// third for CPU time through the eleventh for threads, and moves by the same step from then on,
/// so each second from the third differs from the one before it in one reading.
fn moving(pid: i32, tick: u64) -> Process {
    let counted = |from: u64| MOVED * (tick + 1).saturating_sub(from);
    let raised = |from: u64| u64::from(tick >= from);
    let idle = crate::store::startup_tests::usage(0);

    Process {
        pid,
        usage: Usage {
            user_ns: counted(3),
            energy_nj: counted(4),
            disk_read_bytes: idle.disk_read_bytes + counted(5),
            disk_written_bytes: idle.disk_written_bytes + counted(6),
            resident_bytes: idle.resident_bytes + raised(7),
            footprint_bytes: idle.footprint_bytes + raised(8),
            idle_wakeups: counted(9),
            pageins: counted(10),
            ..idle
        },
        task: task(1 + i32::from(tick >= 11)),
    }
}

pub(super) fn task(threads: i32) -> Task {
    Task {
        threads,
        faults: 0,
        context_switches: 0,
        mach_syscalls: 0,
        unix_syscalls: 0,
    }
}

/// The payload of each tick of `ticks`, each process given as its pid and the CPU time it has
/// spent by that tick, read through the real producer with the second before it.
fn payloads(budget: &Arc<ByteBudget>, ticks: &[&[(i32, u64)]]) -> Vec<Arc<Payload>> {
    let ticks: Vec<Vec<Process>> = ticks
        .iter()
        .map(|processes| {
            processes
                .iter()
                .map(|&(pid, user_ns)| Process {
                    pid,
                    usage: Usage {
                        user_ns,
                        ..crate::store::startup_tests::usage(0)
                    },
                    task: task(1),
                })
                .collect()
        })
        .collect();

    payloads_of(budget, &ticks)
}

fn payloads_of(budget: &Arc<ByteBudget>, ticks: &[Vec<Process>]) -> Vec<Arc<Payload>> {
    seconds_of(budget, ticks, |_| b"/Owner.app")
        .into_iter()
        .map(|second| second.processes.unwrap())
        .collect()
}

/// The second of each tick of `ticks`, read through the real producer with the second before
/// it, each process running from the bundle `app_path` gives for its pid.
pub(super) fn seconds_of(
    budget: &Arc<ByteBudget>,
    ticks: &[Vec<Process>],
    app_path: fn(i32) -> &'static [u8],
) -> Vec<AppsSecond> {
    let mut fixture = Fixture::new_mode(true, PROCESSES_MAX, None);
    let operation = Arc::new(Cancellation::default()).operation(0).unwrap();
    let mut ring = Ring::with_budget(fixture.root.clone(), Arc::clone(budget));
    ring.follow().unwrap();
    let mut seconds: Vec<AppsSecond> = Vec::with_capacity(ticks.len());
    for (index, processes) in ticks.iter().enumerate() {
        publish(
            &mut fixture,
            u64::try_from(index + 1).unwrap(),
            processes,
            app_path,
        );
        let mut second = AppsSecond::default();
        ring.read_prepared(budget, &operation, seconds.last(), |_, prepared| {
            second = std::mem::take(&mut prepared.apps);
            true
        })
        .unwrap();
        seconds.push(second);
    }

    seconds
}

fn publish(
    fixture: &mut Fixture,
    step: u64,
    readings: &[Process],
    app_path: fn(i32) -> &'static [u8],
) {
    let identities: Vec<_> = readings
        .iter()
        .map(|process| identity(&mut fixture.writer, process.pid, app_path(process.pid)))
        .collect();

    fixture
        .writer
        .publish_with_identity(
            &Readings {
                run_tick: step - 1,
                cpus: &[],
                memory: &collector::machine::Memory::default(),
                interfaces: &[],
                ioreport: None,
                smc: None,
                processes: readings,
                processes_refused: 0,
                processes_gone: 0,
                coalitions: &[],
                events: &[],
                dropped: collector::ring::Dropped::default(),
            },
            Some(&ObservationTimes {
                wall_ns: (100 + step) * 1_000_000_000,
                continuous_ns: step * 1_000_000_000,
                awake_ns: step * 1_000_000_000,
            }),
            &IdentityPublication {
                processes: &identities,
                events: &[],
            },
        )
        .unwrap();
}

fn identity(
    writer: &mut collector::ring::Writer,
    pid: i32,
    app_path: &[u8],
) -> IdentityObservation {
    let mut program = None;
    writer
        .intern_program(
            &ProgramBytes {
                executable_name: b"worker",
                app_path: Some(app_path),
            },
            &mut program,
        )
        .unwrap();

    IdentityObservation {
        pid,
        usage: UsageAvailability::Measured,
        identity: ProcessIdentity::Measured {
            creation: ProcessCreation {
                unique_id: u64::try_from(pid).unwrap(),
                pid_version: 1,
            },
            program: program.unwrap(),
        },
    }
}
