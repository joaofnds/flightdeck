use std::sync::Arc;

use collector::processes::{Process, Usage};

use super::name_tests::native_sample;
use super::row_tests::{seconds_of, task};
use super::{AppDetails, Payload, ProcessNames};
use crate::store::startup_tests::usage;
use crate::store::{AppsSecond, ByteBudget, Missing};

const EARLY: &[u8] = b"/Early.app";
const OWNER: &[u8] = b"/Owner.app";
const IDLE: u64 = 0;
const MOVED: u64 = 5;
const BUDGET_BYTES: usize = 256 * 1024 * 1024;

#[test]
fn app_details_equal_to_the_second_before_are_kept_once_for_both_seconds() {
    // Goal: an App whose additional measures repeat costs one details row however many seconds
    // hold it.
    // Method: publish one idle process of one App for three ticks through the real producer and
    // compare where the second and third seconds keep the App's details.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));

    let seconds = seconds(&budget, &[&[(7, IDLE)], &[(7, IDLE)], &[(7, IDLE)]]);

    assert_eq!(details(&seconds[1], OWNER), details(&seconds[2], OWNER));
    assert!(std::ptr::eq(
        details(&seconds[1], OWNER),
        details(&seconds[2], OWNER)
    ));
}

#[test]
fn app_details_that_changed_are_kept_apart_from_the_second_before() {
    // Goal: an App whose additional measures changed shows its new reading, and the idle App
    // beside it still costs one details row.
    // Method: publish a process whose memory footprint grows every tick beside an idle process
    // of another App and compare what and where the second and third seconds keep for each App.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));

    let seconds = seconds(
        &budget,
        &[
            &[(5, IDLE), (7, IDLE)],
            &[(5, IDLE), (7, 4_096)],
            &[(5, IDLE), (7, 8_192)],
        ],
    );

    let idle_bytes = usage(0).footprint_bytes;
    assert_eq!(
        details(&seconds[1], OWNER).footprint_bytes.known,
        idle_bytes + 4_096
    );
    assert_eq!(
        details(&seconds[2], OWNER).footprint_bytes.known,
        idle_bytes + 8_192
    );
    assert!(std::ptr::eq(
        details(&seconds[1], EARLY),
        details(&seconds[2], EARLY)
    ));
}

#[test]
fn app_details_are_kept_apart_whichever_measure_moved() {
    // Goal: a second never shows an additional measure of the second before, whichever moved.
    // Method: publish one App for six ticks, each from the third moving a measure the ticks
    // before it left alone, and count the details rows the seconds from the second keep.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let ticks: Vec<Vec<Process>> = (1..=6).map(moving).collect();

    let seconds = seconds_of(&budget, &ticks, app_path);

    let rows: std::collections::HashSet<_> = seconds[1..]
        .iter()
        .map(|second| std::ptr::from_ref(details(second, OWNER)))
        .collect();
    assert_eq!(rows.len(), 5);
}

#[test]
fn app_details_stay_shared_when_an_app_listed_before_it_leaves() {
    // Goal: one App leaving does not cost a new details row for every App listed after it.
    // Method: publish two idle Apps, drop the process of the one that sorts first from the third
    // tick, and compare where the second and third seconds keep the other's details.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));

    let seconds = seconds(
        &budget,
        &[
            &[(5, IDLE), (7, IDLE)],
            &[(5, IDLE), (7, IDLE)],
            &[(7, IDLE)],
        ],
    );

    assert_eq!(seconds[1].apps.len(), 2);
    assert_eq!(seconds[2].apps.len(), 1);
    assert!(std::ptr::eq(
        details(&seconds[1], OWNER),
        details(&seconds[2], OWNER)
    ));
}

#[test]
fn app_details_stay_shared_when_an_app_listed_before_it_arrives() {
    // Goal: one App arriving does not cost a new details row for every App listed after it.
    // Method: publish one idle App, add a process of one that sorts before it at the third
    // tick, and compare where the second and third seconds keep the first's details.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));

    let seconds = seconds(
        &budget,
        &[&[(7, IDLE)], &[(7, IDLE)], &[(5, IDLE), (7, IDLE)]],
    );

    assert_eq!(seconds[2].apps.len(), 2);
    assert!(std::ptr::eq(
        details(&seconds[1], OWNER),
        details(&seconds[2], OWNER)
    ));
}

#[test]
fn seconds_that_repeat_app_details_are_charged_for_them_once() {
    // Goal: the budget counts a details row once however many seconds hold it.
    // Method: publish one idle App for three ticks and compare the budget's use with the
    // payloads' own bytes, the process's name, its rows and two allocations of a details row,
    // its two counts and its reservation: the first second's, which has no interval yet, and the
    // one the other two seconds share.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));

    let payloads: Vec<Arc<Payload>> = seconds(&budget, &[&[(7, IDLE)], &[(7, IDLE)], &[(7, IDLE)]])
        .into_iter()
        .map(|second| second.processes.unwrap())
        .collect();

    let own: usize = payloads.iter().map(|payload| payload.bytes()).sum();
    let name = payloads[0][0].executable_name().unwrap().reserved_bytes();
    let rows = 2 * Payload::ROW_BYTES;
    let details_bytes = 2 * size_of::<usize>() + size_of::<super::Shared<AppDetails>>();
    assert_eq!(budget.used(), own + name + rows + 2 * details_bytes);
}

#[test]
fn refused_app_details_leave_the_budget_as_it_was() {
    // Goal: a second whose App details do not all fit takes nothing from the budget.
    // Method: capture a real Sample whose names the store already holds, with room left for the
    // payload's own bytes, its rows and all but one of its details rows.
    let sample = native_sample();
    let budget = Arc::new(ByteBudget::new(64 * 1024));
    let mut names = ProcessNames::new(sample.processes().len(), &budget).unwrap();
    let (payload, additions) = Payload::capture_with_names(&sample, &names, None).unwrap();
    names.commit(additions);
    drop(payload);
    let room = Payload::capture_bytes(&sample)
        + sample.processes().len() * Payload::ROW_BYTES
        + (sample.apps().len() - 1) * Payload::APP_DETAILS_BYTES;
    let _pressure = budget
        .reserve(budget.maximum() - budget.used() - room)
        .unwrap();
    let before = budget.used();

    let refused = Payload::capture_with_names(&sample, &names, None);

    assert!(matches!(refused, Err(Missing::Budget)));
    assert_eq!(budget.used(), before);
}

/// The additional measures `second` holds for the App of the bundle `app_path`.
fn details<'second>(second: &'second AppsSecond, app_path: &[u8]) -> &'second AppDetails {
    let index = second
        .apps
        .iter()
        .position(|row| row.name.bytes == app_path)
        .unwrap();

    second.processes.as_ref().unwrap().app(index)
}

/// The bundle a process of `pid` runs from: pids below 7 from one that sorts before the other.
fn app_path(pid: i32) -> &'static [u8] {
    if pid < 7 { EARLY } else { OWNER }
}

/// What the processes of one App have read by `tick`. Each measure stays as it was until its
/// own tick, the third for the bytes read, the fourth for the bytes written and the fifth for the
/// memory footprint, and a second process joins at the sixth, so each second from the third
/// differs from the one before it.
fn moving(tick: u64) -> Vec<Process> {
    let counted = |from: u64| MOVED * (tick + 1).saturating_sub(from);
    let idle = usage(0);
    let first = Process {
        pid: 7,
        usage: Usage {
            disk_read_bytes: idle.disk_read_bytes + counted(3),
            disk_written_bytes: idle.disk_written_bytes + counted(4),
            footprint_bytes: idle.footprint_bytes + u64::from(tick >= 5),
            ..idle
        },
        task: task(1),
    };
    let joined = Process {
        pid: 8,
        usage: idle,
        task: task(1),
    };

    if tick < 6 {
        vec![first]
    } else {
        vec![first, joined]
    }
}

/// The second of each tick of `ticks`, each process given as its pid and the bytes its memory
/// footprint has grown by at that tick, read through the real producer with the second before it.
fn seconds(budget: &Arc<ByteBudget>, ticks: &[&[(i32, u64)]]) -> Vec<AppsSecond> {
    let idle = usage(0);
    let ticks: Vec<Vec<Process>> = ticks
        .iter()
        .map(|processes| {
            processes
                .iter()
                .map(|&(pid, grown_bytes)| Process {
                    pid,
                    usage: Usage {
                        footprint_bytes: idle.footprint_bytes + grown_bytes,
                        ..idle
                    },
                    task: task(1),
                })
                .collect()
        })
        .collect();

    seconds_of(budget, &ticks, app_path)
}
