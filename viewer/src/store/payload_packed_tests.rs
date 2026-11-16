use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use collector::ring::{IdentityUnavailable, ProcessCreation};

use super::super::{
    AppDetails, AppIndex, CompactAmount, Payload, ProcessName, ProcessRow, ProcessRows, Shared,
};
use super::{
    APP_ROWS_PACKED_ALWAYS, AppRowsShare, AppStep, BlockTable, KeptPlain, pack_seconds,
    processes_read_as,
};
use crate::apps::{Amount, PROCESS_INTERVALS_MAX, ProcessState, Stamp, Unavailable};
use crate::live_app_response::{
    APPS_PAGE_ROWS_MAX, App, Bytes, Detail, Key, KeyKind, Measure, Observation,
    PROCESSES_PAGE_ROWS_MAX, Page, RankedApp, Sample,
};
use crate::store::{APPS_PER_SECOND_MAX, AppsCursor, AppsSecond, ByteBudget, Missing, Second};

const BUDGET_BYTES: usize = 256 * 1024 * 1024;
const NS: u64 = 1_000_000_000;
const SEEDS: u64 = 40;
/// One App more than half of what a block holds.
pub(in crate::store) const APPS_PAST_HALF: usize = APPS_PER_SECOND_MAX / 2 + 1;
/// The share under which no block packs its App rows, whatever they save.
pub(in crate::store) const APP_ROWS_PACKED_NEVER: AppRowsShare = AppRowsShare {
    numerator: 0,
    denominator: 1,
};

macro_rules! generated_block_case {
    ($name:ident, $share:expr, $rows_packed:expr) => {
        #[test]
        fn $name() {
            // Goal: a packed block reads at each of its seconds exactly what the plain second
            // held: its header, each process in its order with every reading and its App index,
            // and each App detail.
            // Method: generate a block of 31 to 64 plain seconds from each of forty seeds,
            // holding an exec, an exit, unavailable amounts, a process and Apps absent from some
            // seconds, an App that comes and goes twice, a process first listed between two the
            // block holds, one pid held twice with two creations, values that need all 64 bits,
            // a second with no Apps, a second whose process rows were refused and a restart,
            // where about one process in five changes its amounts each second and the rest keep
            // their rows, and Apps whose readings change at some seconds and not at others, pack
            // it under this case's share, and read every second back as `assert_reads_as` reads
            // it, and each of its processes as `find` reads it.
            let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));

            for seed in 1..=SEEDS {
                let plain = generated_block(seed, &budget);
                let (shared_count, row_count) = shared_rows(&plain);
                assert!(
                    4 * shared_count >= row_count,
                    "seed {seed}: {shared_count} of {row_count}"
                );
                let message = format!("seed {seed}");
                let (packed, table) = pack_seconds(&plain, None, &budget, $share).unwrap();
                assert_eq!(table.app_rows_packed(), $rows_packed, "{message}");
                assert_reads_as(&packed, &table, &plain, &message);
                assert_processes_find_as(&packed, &plain, &message);
            }
        }
    };
}

generated_block_case!(
    every_second_of_a_generated_block_with_app_rows_packed_rebuilds_equal_to_its_plain_second,
    APP_ROWS_PACKED_ALWAYS,
    true
);
generated_block_case!(
    every_second_of_a_generated_block_with_app_rows_plain_rebuilds_equal_to_its_plain_second,
    APP_ROWS_PACKED_NEVER,
    false
);

macro_rules! share_boundary_case {
    ($name:ident, $bytes_past:expr, $rows_packed:expr) => {
        #[test]
        fn $name() {
            // Goal: the packer keeps a block's App rows plain where packed they take exactly the
            // share it is handed of their plain bytes, packs them where the share is one byte
            // more, and the block reads as its plain seconds in either form.
            // Method: pack generated blocks once to learn the bytes of their App rows in each
            // form, pack each again under the share of those packed bytes and this case's bytes
            // past them, and read it back as `assert_reads_as` reads it.
            let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));

            for seed in 1..=SEEDS / 4 {
                let plain = generated_block(seed, &budget);
                let (_, learned) =
                    pack_seconds(&plain, None, &budget, APP_ROWS_PACKED_ALWAYS).unwrap();
                let bytes = learned.app_rows_bytes();
                assert!(bytes.packed < bytes.plain, "seed {seed}");
                let share = AppRowsShare {
                    numerator: u64::try_from(bytes.packed + $bytes_past).unwrap(),
                    denominator: u64::try_from(bytes.plain).unwrap(),
                };

                let message = format!("seed {seed}");
                let (packed, table) = pack_seconds(&plain, None, &budget, share).unwrap();
                assert_eq!(table.app_rows_packed(), $rows_packed, "{message}");
                assert_eq!(table.app_rows_bytes(), bytes, "{message}");
                assert_reads_as(&packed, &table, &plain, &message);
            }
        }
    };
}

share_boundary_case!(
    a_block_whose_packed_app_rows_take_exactly_the_share_keeps_them_plain,
    0,
    false
);
share_boundary_case!(
    a_block_whose_packed_app_rows_take_one_byte_less_than_the_share_packs_them,
    1,
    true
);

/// Checks that each process of each `plain` second is found in its `packed` second as `find`
/// finds it in the plain one.
fn assert_processes_find_as(packed: &[Arc<AppsSecond>], plain: &[Arc<AppsSecond>], message: &str) {
    for (index, (packed, plain)) in packed.iter().zip(plain).enumerate() {
        let message = format!("{message} second {index}");
        if let Ok(payload) = &packed.processes {
            assert!(payload.packed(), "{message}");
        }
        let (Ok(packed), Ok(plain)) = (&packed.processes, &plain.processes) else {
            continue;
        };
        for process in plain.processes() {
            let (pid, creation) = (process.pid(), process.creation());
            assert!(
                packed.find(pid, creation) == plain.find(pid, creation),
                "{message} pid {pid}"
            );
        }
    }
}

#[test]
fn a_block_the_app_producer_reads_packs_and_reads_as_its_plain_seconds() {
    // Goal: the order the packer asserts, by pid and then the creation's unique id, is the order
    // the producer the app uses gives a second's rows, so a change of either side's key fails.
    // Method: publish four ticks of nine processes, where at the second another process of pid
    // 7 starts and at the third it exits beside the running one, which the producer adds after
    // the processes the tick lists, out of their order and under a second unique id for pid 7;
    // read each second
    // through the producer the app uses, check the third holds pid 7 twice, pack the seconds,
    // and compare each packed second with its plain second.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let mut fixture = crate::live::identity_fixture::Fixture::new_mode(true, 9, None);
    let operation = Arc::new(crate::cancellation::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut ring = crate::store::ring::Ring::with_budget(fixture.root.clone(), Arc::clone(&budget));
    ring.follow().unwrap();
    let start = collector::tracker::Event::Start {
        pid: 7,
        coalition: None,
    };
    let exit = collector::tracker::Event::Exit {
        pid: 7,
        last: Ok(crate::store::startup_tests::usage(0)),
        noticed: collector::tracker::Noticed::AlreadyExited,
    };
    let mut seconds: Vec<Arc<AppsSecond>> = Vec::with_capacity(4);
    for tick in 0..4 {
        let events = match tick {
            1 => std::slice::from_ref(&start),
            2 => std::slice::from_ref(&exit),
            _ => &[],
        };
        fixture.publish_with_events(100 + tick, events);
        let mut second = AppsSecond::default();
        let previous = seconds.last().map(|previous| &**previous);
        ring.read_prepared(&budget, &operation, previous, |_, prepared| {
            second = std::mem::take(&mut prepared.apps);
            true
        })
        .unwrap();
        seconds.push(Arc::new(second));
    }
    let third = seconds[2].processes.as_ref().unwrap();
    let sevens: Vec<u64> = third
        .processes()
        .filter(|process| process.pid() == 7)
        .map(|process| unique(process.creation()))
        .collect();
    assert_eq!(sevens, [7, 700]);

    let (packed, table) = pack_seconds(&seconds, None, &budget, APP_ROWS_PACKED_ALWAYS).unwrap();

    assert_reads_as(&packed, &table, &seconds, "the producer's block");
}

/// How many process rows of the plain seconds of `block` are the row of the second before,
/// and how many rows the seconds after the first hold.
fn shared_rows(block: &[Arc<AppsSecond>]) -> (usize, usize) {
    let mut shared_count = 0;
    let mut row_count = 0;
    for pair in block.windows(2) {
        let (Ok(before), Ok(after)) = (&pair[0].processes, &pair[1].processes) else {
            continue;
        };
        let before = before.plain_rows();
        for shared in after.plain_rows() {
            row_count += 1;
            if before.iter().any(|held| Arc::ptr_eq(held, shared)) {
                shared_count += 1;
            }
        }
    }

    (shared_count, row_count)
}

#[test]
fn a_packed_process_reads_each_case_the_generator_holds() {
    // Goal: the cases the round trip is built to hold reach the packed block, so its equality
    // is not met by blocks that lack them.
    // Method: pack the block of one seed and read, through the packed seconds alone, the exec's
    // new name, the exit, both creations of the pid held twice, the 64-bit value, the
    // unavailable amount, the late process and the two seconds without process rows.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let plain = generated_block(7, &budget);

    let (packed, _) = pack_seconds(&plain, None, &budget, APP_ROWS_PACKED_ALWAYS).unwrap();

    let at = |index: usize, pid: i32, unique_id: u64| {
        let payload = packed[index].processes.as_ref().unwrap();
        payload
            .processes()
            .find(|process| process.pid() == pid && unique(process.creation()) == unique_id)
    };
    let layout = Layout::of(7, plain.len());
    let exec = at(layout.exec, 20, 2).unwrap();
    assert_eq!(exec.executable_name().unwrap().bytes, b"exec'd");
    assert_eq!(exec.creation().unwrap().pid_version, 2);
    let before_exec = at(layout.exec - 1, 20, 2).unwrap();
    assert_eq!(before_exec.executable_name().unwrap().bytes, b"worker");
    assert_eq!(
        at(layout.exit, 30, 3).unwrap().state(),
        ProcessState::ExitedVerified
    );
    assert!(at(layout.exit + 1, 30, 3).is_none());
    assert!(at(0, 50, 500).is_some());
    assert!(at(0, 50, 501).is_some());
    assert_eq!(
        at(1, 60, 6).unwrap().resident_bytes(),
        Amount::Known(u64::MAX)
    );
    assert_eq!(
        at(2, 70, 7).unwrap().cpu_ns(),
        Amount::Unavailable(Unavailable::Refused)
    );
    assert!(at(layout.late - 1, 15, 1).is_none());
    assert!(at(layout.late, 15, 1).is_some());
    assert_eq!(packed[layout.empty].processes, Err(Missing::TooMany));
    assert_eq!(packed[layout.refused].processes, Err(Missing::Budget));
    assert!(at(layout.restart, 10, 1).is_none());
    assert!(at(layout.restart, 10, 1_001).is_some());
}

#[test]
fn a_packed_app_reads_each_case_the_generator_holds() {
    // Goal: the App cases the round trip is built to hold reach the packed block, so its
    // equality is not met by blocks that lack them.
    // Method: pack the block of one seed and read, through the packed seconds alone, an App
    // absent from some seconds, an App that comes and goes twice, a second with no Apps, a
    // second whose process rows were refused, a value that needs all 64 bits, an unavailable
    // measure, an overflowed count, readings that change and readings that stay, a restart, and
    // more than thirty ticks.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let plain = generated_block(7, &budget);
    let layout = Layout::of(7, plain.len());

    let (packed, table) = pack_seconds(&plain, None, &budget, APP_ROWS_PACKED_ALWAYS).unwrap();

    let second = |index: usize| Second::packed_at(&packed[index], &table, index);
    let held = |key: &[u8], index: usize| second(index).app_of(KeyKind::Bundle, key);
    let arrivals = |key: &[u8]| {
        (1..packed.len())
            .filter(|&index| held(key, index).is_some() && held(key, index - 1).is_none())
            .count()
    };
    assert!(packed.len() > 30);
    assert!((0..packed.len()).any(|index| held(b"/B.app", index).is_none()));
    assert!((0..packed.len()).any(|index| held(b"/B.app", index).is_some()));
    assert!(arrivals(b"/C.app") >= 2);
    assert_eq!(second(layout.empty).app_count(), 0);
    assert!(second(layout.refused).app_count() > 0);
    assert!(second(layout.refused).processes().is_err());
    assert!(!packed[layout.restart].consecutive);
    let mut cursor = AppsCursor::default();
    let mut words: BTreeSet<u64> = BTreeSet::new();
    let mut unavailable = false;
    let mut overflowed = false;
    for index in 0..packed.len() {
        for row in cursor.apps(second(index)) {
            let view = row.presentation(&details_of(row));
            words.insert(view.cpu_ns.known);
            unavailable |= view.cpu_ns.unavailable_members > 0;
            overflowed |= view.cpu_ns.overflowed;
        }
    }
    assert!(words.contains(&u64::MAX));
    assert!(unavailable);
    assert!(overflowed);
    let readings = |key: &[u8]| -> BTreeSet<u32> {
        (0..packed.len())
            .filter_map(|index| held(key, index))
            .map(crate::store::block::AppAt::members)
            .collect()
    };
    assert!(readings(b"/B.app").len() > 1);
    assert_eq!(readings(b"/A.app").len(), 1);
}

/// Details for `row` that no reading of this test looks at, to give a row its presentation.
fn details_of(row: &crate::store::AppRow) -> AppDetails {
    let measure = Measure {
        known: 0,
        measured_members: 0,
        unavailable_members: 0,
        overflowed: false,
    };

    AppDetails {
        live_members: row.members,
        disk_read_bytes: measure,
        disk_written_bytes: measure,
        footprint_bytes: measure,
    }
}

#[test]
fn a_cursor_shows_each_second_as_its_plain_second_and_holds_one_reference_to_each_app_it_shows() {
    // Goal: a cursor over packed blocks shows at each second the rows its plain second holds,
    // whether it moves to the next second, jumps back or crosses into the next block, and holds
    // one reference to the name of each App it shows and none to any other, so no reference
    // leaks or goes missing when it replaces rows. A count touched and restored during a move
    // is not observable without a seam in the cursor, so this test does not show one.
    // Method: split a generated block into two packed blocks, count each App name's references
    // with no cursor, then move a cursor through every second of both blocks in order, back to
    // the first, to the second block's first second, which holds the same index, and across
    // the boundary again, and after each move compare the rows it shows with the plain
    // second's, and each name's references with the count without the cursor, one more for
    // each App the plain second holds, over all its name's allocations.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let plain = generated_block(5, &budget);
    let half = plain.len() / 2;
    let (first, first_table) =
        pack_seconds(&plain[..half], None, &budget, APP_ROWS_PACKED_ALWAYS).unwrap();
    let (second, second_table) = pack_seconds(
        &plain[half..],
        Some(&first_table),
        &budget,
        APP_ROWS_PACKED_ALWAYS,
    )
    .unwrap();
    let at = |index: usize| {
        if index < half {
            Second::packed_at(&first[index], &first_table, index)
        } else {
            Second::packed_at(&second[index - half], &second_table, index - half)
        }
    };
    // Each allocation of an App's name once, by address. The generator may give one App
    // several, and the cursor holds the one the block's table holds.
    let mut names: BTreeMap<usize, Arc<RankedApp>> = BTreeMap::new();
    for row in plain.iter().flat_map(|second| &second.apps) {
        names.insert(Arc::as_ptr(&row.name).addr(), Arc::clone(&row.name));
    }
    let alone: BTreeMap<usize, usize> = names
        .iter()
        .map(|(address, name)| (*address, Arc::strong_count(name)))
        .collect();

    let mut cursor = AppsCursor::default();
    let order = (0..plain.len()).chain([0, half, half - 1, half, half + 1, half - 1]);
    for index in order {
        assert_eq!(
            cursor.apps(at(index)),
            plain[index].apps.as_slice(),
            "second {index}"
        );
        // Every generated App is a bundle, so its key's bytes name it.
        let mut held: BTreeMap<&[u8], usize> = BTreeMap::new();
        for (address, name) in &names {
            *held.entry(&name.bytes).or_default() += Arc::strong_count(name) - alone[address];
        }
        for (key, references) in held {
            let shown = plain[index].apps.iter().any(|row| row.name.bytes == key);
            assert_eq!(
                references,
                usize::from(shown),
                "second {index} {}",
                String::from_utf8_lossy(key)
            );
        }
    }
}

#[test]
fn the_members_of_an_app_or_of_none_in_a_packed_block_walk_only_the_processes_ever_there() {
    // Goal: a reader that asks for one App's members at a second of a packed block, or for the
    // processes outside any App, reads the members the plain second holds, in its order, and
    // walks only the processes that were under that App, or outside any App, at some second of
    // the block, not every process of the block.
    // Method: pack the generated block of each seed, gather the processes under each App and
    // those outside any App at any of its plain seconds, and for each App of each second, and for
    // no App, compare the members read packed with those read plain, and check each walk is
    // bounded by the processes gathered for it.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));

    for seed in 1..=SEEDS {
        let plain = generated_block(seed, &budget);
        let (packed, _) = pack_seconds(&plain, None, &budget, APP_ROWS_PACKED_ALWAYS).unwrap();
        let mut under: BTreeMap<Vec<u8>, BTreeSet<(i32, u64)>> = BTreeMap::new();
        let mut outside: BTreeSet<(i32, u64)> = BTreeSet::new();
        for second in &plain {
            let Ok(payload) = &second.processes else {
                continue;
            };
            let members = payload.members(None);
            outside.extend(members.map(|process| (process.pid(), unique(process.creation()))));
            for (app_index, app) in (0..).zip(second.apps.iter()) {
                let members = payload.members(Some(app_index));
                let keys = members.map(|process| (process.pid(), unique(process.creation())));
                under
                    .entry(app.name.bytes.clone())
                    .or_default()
                    .extend(keys);
            }
        }

        let keys = |members: super::super::Members<'_>| -> Vec<(i32, u64)> {
            members
                .map(|process| (process.pid(), unique(process.creation())))
                .collect()
        };
        for (index, (packed, plain)) in packed.iter().zip(&plain).enumerate() {
            let (Ok(packed_payload), Ok(plain_payload)) = (&packed.processes, &plain.processes)
            else {
                continue;
            };
            let message = format!("seed {seed} second {index}");
            for (app_index, app) in (0..).zip(plain.apps.iter()) {
                let packed_members = packed_payload.members(Some(app_index));
                let bound = under[&app.name.bytes].len();
                assert_eq!(packed_members.size_hint().1, Some(bound), "{message}");
                let plain_members = plain_payload.members(Some(app_index));
                assert_eq!(keys(packed_members), keys(plain_members), "{message}");
            }
            let packed_outside = packed_payload.members(None);
            assert_eq!(
                packed_outside.size_hint().1,
                Some(outside.len()),
                "{message}"
            );
            assert_eq!(
                keys(packed_outside),
                keys(plain_payload.members(None)),
                "{message}"
            );
        }
    }
}

#[test]
fn a_block_of_more_apps_than_a_block_holds_stays_plain() {
    // Goal: a block whose seconds name more Apps between them than a block holds stays plain,
    // which is a state the data can bring about and no defect.
    // Method: pack two seconds of 1,025 Apps each, none shared, 2,050 Apps for a block that
    // holds 2,048.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let seconds: Vec<Arc<AppsSecond>> = past_the_app_bound(&budget).map(Arc::new).into();

    assert_eq!(
        pack_seconds(&seconds, None, &budget, APP_ROWS_PACKED_ALWAYS).err(),
        Some(KeptPlain::Bound)
    );
}

#[test]
fn a_block_of_more_processes_than_a_block_holds_stays_plain() {
    // Goal: a block whose seconds hold more processes between them than a block holds stays
    // plain, which is a state the data can bring about and no defect.
    // Method: pack two seconds of 12,001 processes each, none shared, 24,002 processes for a
    // block that holds 24,000.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let names = Names::new(&budget);
    let half = i32::try_from(PROCESS_INTERVALS_MAX / 2 + 1).unwrap();
    let seconds: Vec<Arc<AppsSecond>> = [0..half, half..2 * half]
        .into_iter()
        .enumerate()
        .map(|(index, pids)| Arc::new(processes_only(index, pids, &names, &budget)))
        .collect();

    assert_eq!(
        pack_seconds(&seconds, None, &budget, APP_ROWS_PACKED_ALWAYS).err(),
        Some(KeptPlain::Bound)
    );
}

#[test]
fn a_block_the_budget_refuses_stays_plain_and_leaves_the_budget_as_it_was() {
    // Goal: a block the budget has no room for stays plain, and its refusal charges nothing.
    // Method: generate a block, take every byte the budget has left, and pack it.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let plain = generated_block(3, &budget);
    let rest = budget.reserve(budget.maximum() - budget.used()).unwrap();
    let used = budget.used();

    let packed = pack_seconds(&plain, None, &budget, APP_ROWS_PACKED_ALWAYS);

    assert_eq!(packed.err(), Some(KeptPlain::Budget));
    assert_eq!(budget.used(), used);
    drop(rest);
}

#[test]
fn the_check_refuses_a_packed_second_that_holds_a_process_past_its_count() {
    // Goal: the check that a packed second reads as its plain one refuses a table that holds a
    // process more at the second than the plain second counts, even one past the last it lists.
    // Method: pack a generated block's table, find a second at which the block's last process is
    // absent and another is present, mark that last process present there too, and check the
    // second against its plain one.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    for seed in 1..=SEEDS {
        let plain = generated_block(seed, &budget);
        let mut table = BlockTable::pack(&plain, None, &budget, APP_ROWS_PACKED_ALWAYS).unwrap();
        let last = table.processes.len() - 1;
        let found = (0..plain.len()).find(|&index| {
            let bit = 1 << index;
            plain[index].processes.is_ok()
                && table.processes[last].seconds & bit == 0
                && table
                    .processes
                    .iter()
                    .any(|process| process.seconds & bit != 0)
        });
        let Some(index) = found else {
            continue;
        };
        Arc::get_mut(&mut table).unwrap().processes[last].seconds |= 1 << index;
        let plain_payload = plain[index].processes.as_ref().unwrap();
        let packed_payload = Payload {
            selected: plain_payload.selected,
            previous: plain_payload.previous,
            rows: ProcessRows::Packed {
                table: Arc::clone(&table),
                second_index: u32::try_from(index).unwrap(),
                count: plain_payload.len(),
            },
            apps: Vec::new(),
            reservation: budget.reserve(0).unwrap(),
        };

        assert!(
            !processes_read_as(&packed_payload, plain_payload, &mut table.checked()),
            "seed {seed} second {index}"
        );
        return;
    }
    panic!("no seed holds a second at which the block's last process is absent");
}

#[test]
fn the_check_refuses_a_packed_block_whose_apps_read_otherwise_than_its_plain_seconds() {
    // Goal: the check that a packed block's Apps read as its plain seconds refuses a table that
    // holds one App value, one App step, one App's presence or one App detail otherwise than
    // the plain seconds, through the read that reads what changed: the rows walked through each
    // second's steps, the rows rebuilt from the first rows and their changes, each App over the
    // block's seconds, or the second's details, and a step, which only the walk reads, leaves
    // the other reads passing.
    // Method: pack a generated block's table with its App rows packed four times, and in each
    // change one thing: the first changed value of an App's row, the value of the first step
    // that changes a word, the first changed value of an App's details, or the presence of two
    // Apps at a second, one held there and at the second before and one not held there,
    // swapped so the second's count stays. Pack the block's seconds over each table and read
    // them each way the check reads them.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let plain = generated_block(5, &budget);
    let table = |corrupt: &dyn Fn(&mut BlockTable)| {
        let mut table = BlockTable::pack(&plain, None, &budget, APP_ROWS_PACKED_ALWAYS).unwrap();
        corrupt(Arc::get_mut(&mut table).unwrap());
        table
    };
    let first_change = |kind: usize| {
        let table = table(&|_| {});
        (0..table.block_app_count())
            .find_map(|place| table.app_first_changes(place)[kind])
            .unwrap()
    };
    let (row_value, details_value) = (first_change(0), first_change(1));
    let swapped = swapped_presence(&table(&|_| {}), plain.len());

    let refused =
        |corrupt: &dyn Fn(&mut BlockTable)| refused_reads(&table(corrupt), &plain, &budget);

    let intact = refused(&|_| {});
    assert!(intact.is_empty(), "intact: {intact:?}");
    let row = refused(&|table| table.values[row_value] ^= 1);
    assert!(
        row.is_superset(&BTreeSet::from([CheckRead::Rebuilt, CheckRead::Over])),
        "row value: {row:?}"
    );
    let step = refused(&step_changed);
    assert_eq!(step, BTreeSet::from([CheckRead::Walked]), "step");
    let details = refused(&|table| table.values[details_value] ^= 1);
    assert!(
        details.contains(&CheckRead::Details),
        "details: {details:?}"
    );
    // Last, since a process of the App taken out of the second names an App the second does
    // not hold, which only the details check, refusing first, keeps the process reader from.
    let presence = refused(&|table| {
        let (held, absent, bit) = swapped;
        table.apps[held].seconds &= !bit;
        table.apps[absent].seconds |= bit;
    });
    assert!(
        presence.is_superset(&BTreeSet::from([CheckRead::Rebuilt, CheckRead::Over])),
        "presence: {presence:?}"
    );
}

/// Changes the value of the first step of `table` that changes a word of an App's row.
fn step_changed(table: &mut BlockTable) {
    let Some(AppStep::Change { value, .. }) = table
        .steps
        .iter_mut()
        .find(|step| matches!(step, AppStep::Change { .. }))
    else {
        unreachable!("a generated block changes a word of an App's row");
    };
    *value ^= 1;
}

/// One of the reads the packer's check compares with a block's plain seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum CheckRead {
    /// A second's header, processes or App details.
    Details,
    /// A second's Apps walked from the second before through the second's steps.
    Walked,
    /// A second's Apps rebuilt from the first rows and their changes.
    Rebuilt,
    /// Each App over the block's seconds.
    Over,
}

/// Which reads of the check refuse the `plain` seconds packed over `table`.
fn refused_reads(
    table: &Arc<BlockTable>,
    plain: &[Arc<AppsSecond>],
    budget: &Arc<ByteBudget>,
) -> BTreeSet<CheckRead> {
    let mut refused = BTreeSet::new();
    if !super::seconds::pack_seconds_apps_over(table, plain) {
        refused.insert(CheckRead::Over);
    }
    let (mut walked, mut walked_places) = (Vec::new(), Vec::new());
    let (mut rebuilt, mut rebuilt_places, mut spare) = (Vec::new(), Vec::new(), Vec::new());
    for (index, plain) in (0..).zip(plain) {
        let packed = super::seconds::pack_second(plain, table, index, budget).unwrap();
        if index == 0 {
            table.rebuild_app_rows(index, &mut walked, &mut walked_places, &mut spare);
        } else {
            table.step_app_rows(index, &mut walked, &mut walked_places);
        }
        table.rebuild_app_rows(index, &mut rebuilt, &mut rebuilt_places, &mut spare);
        let reads_as = |apps_read: &[&[crate::store::AppRow]]| {
            super::seconds::second_reads_as(&packed, plain, apps_read, &mut table.checked())
        };
        for (read, apps_read) in [
            (CheckRead::Details, &[][..]),
            (CheckRead::Walked, &[walked.as_slice()][..]),
            (CheckRead::Rebuilt, &[rebuilt.as_slice()][..]),
        ] {
            if !reads_as(apps_read) {
                refused.insert(read);
            }
        }
    }

    refused
}

/// Two App places of the block of `table`, of `count` seconds, and the bit of one second: the
/// first App is held at that second and the second before, and the second is not held at it.
fn swapped_presence(table: &BlockTable, count: usize) -> (usize, usize, u64) {
    for index in 1..count {
        let bit = 1 << index;
        let before = 1 << (index - 1);
        let held = table
            .apps
            .iter()
            .position(|app| app.seconds & bit != 0 && app.seconds & before != 0);
        let absent = table.apps.iter().position(|app| app.seconds & bit == 0);
        if let (Some(held), Some(absent)) = (held, absent) {
            return (held, absent, bit);
        }
    }
    unreachable!("a generated block holds an App at two seconds in a row and lacks another");
}

/// Checks that every second of the packed block `packed`, whose table is `table`, reads as its
/// plain second of `plain`: its header, processes and App details, its Apps through a cursor that
/// walks the seconds in order and through one that visits them back, by jumps and again, and
/// each one-second read of its Apps.
pub(in crate::store) fn assert_reads_as(
    packed: &[Arc<AppsSecond>],
    table: &Arc<BlockTable>,
    plain: &[Arc<AppsSecond>],
    message: &str,
) {
    assert_eq!(packed.len(), plain.len(), "{message}");
    let second = |index: usize| Second::packed_at(&packed[index], table, index);
    let mut walked = AppsCursor::default();
    for (index, plain) in plain.iter().enumerate() {
        let message = format!("{message} second {index}");
        assert_header_reads_as(&packed[index], plain, table.app_rows_packed(), &message);
        assert_eq!(
            walked.apps(second(index)),
            plain.apps.as_slice(),
            "{message}"
        );
        assert_second_reads_as(second(index), plain, &message);
    }

    let count = plain.len();
    let order = (0..count)
        .rev()
        .step_by(2)
        .chain((0..count).step_by(3))
        .chain((0..count).rev());
    let mut scattered = AppsCursor::default();
    for index in order {
        assert_eq!(
            scattered.apps(second(index)),
            plain[index].apps.as_slice(),
            "{message} second {index} out of order"
        );
    }
}

/// Checks that the packed `second` holds the header, processes and App details of `plain`, and
/// either no App rows of its own, where its block packs them, or the rows of `plain`.
fn assert_header_reads_as(
    second: &AppsSecond,
    plain: &AppsSecond,
    rows_packed: bool,
    message: &str,
) {
    let AppsSecond {
        metadata,
        processes,
        missing,
        observation,
        consecutive,
        elapsed_ns,
        membership_complete,
        ungrouped_members,
        live_ungrouped_members,
        usage_refused,
        usage_gone,
        apps,
        totals,
    } = second;

    if rows_packed {
        assert!(apps.is_empty(), "{message}");
    } else {
        assert_eq!(*apps, plain.apps, "{message}");
    }
    assert_eq!(*metadata, plain.metadata, "{message}");
    assert!(*processes == plain.processes, "{message}");
    assert_eq!(*missing, plain.missing, "{message}");
    assert_eq!(*observation, plain.observation, "{message}");
    assert_eq!(*consecutive, plain.consecutive, "{message}");
    assert_eq!(*elapsed_ns, plain.elapsed_ns, "{message}");
    assert_eq!(*membership_complete, plain.membership_complete, "{message}");
    assert_eq!(*ungrouped_members, plain.ungrouped_members, "{message}");
    assert_eq!(
        *live_ungrouped_members, plain.live_ungrouped_members,
        "{message}"
    );
    assert_eq!(*usage_refused, plain.usage_refused, "{message}");
    assert_eq!(*usage_gone, plain.usage_gone, "{message}");
    assert_eq!(*totals, plain.totals, "{message}");
}

/// Checks that each one-second read of the Apps of the packed `second` gives what it gives of
/// `plain`: the count, the ranks, the charts, the pages, and each App found by key with its
/// readings, index and members.
fn assert_second_reads_as(second: Second<'_>, plain: &AppsSecond, message: &str) {
    let plain_second = Second::unheld(plain);
    assert_eq!(second.app_count(), plain.apps.len(), "{message}");
    assert_eq!(second.ranks(), plain_second.ranks(), "{message}");
    let cohort: Vec<Arc<RankedApp>> = plain
        .apps
        .iter()
        .step_by(2)
        .map(|row| Arc::clone(&row.name))
        .collect();
    for metric in crate::store::attribution::METRICS {
        assert_eq!(
            second.attribution(metric, &cohort),
            plain_second.attribution(metric, &cohort),
            "{message}"
        );
    }
    assert_eq!(
        second.resident(&cohort),
        plain_second.resident(&cohort),
        "{message}"
    );
    assert_pages_read_as(second, plain, message);
    for index in 0..plain.apps.len() {
        assert_app_reads_as(second, plain, index, &format!("{message} App {index}"));
    }
}

/// Checks that each page of Apps of the packed `second`, over page sizes, and its page of the
/// processes in no App, are the pages of `plain`, and that a second that presents its pages
/// gives each page asked of it.
fn assert_pages_read_as(second: Second<'_>, plain: &AppsSecond, message: &str) {
    let plain_second = Second::unheld(plain);
    let presented = plain_second.presentation().is_ok();
    for page in pages_over(plain.apps.len(), APPS_PAGE_ROWS_MAX) {
        let read = refused_on(plain_second.apps_page(page));
        let message = format!("{message} page {} {}", page.offset, page.limit);
        if presented {
            assert!(read.is_ok(), "{message}");
        }
        assert_eq!(refused_on(second.apps_page(page)), read, "{message}");
    }
    let page = Page {
        offset: 0,
        limit: PROCESSES_PAGE_ROWS_MAX,
    };
    let read = refused_on(plain_second.ungrouped_page(page));
    if presented {
        assert!(read.is_ok(), "{message}");
    }
    assert_eq!(refused_on(second.ungrouped_page(page)), read, "{message}");
}

/// `read` as its JSON, or as the bound it was refused on, the one error a read of a held second
/// gives.
fn refused_on(
    read: persistence::Result<serde_json::Value>,
) -> Result<serde_json::Value, &'static str> {
    match read {
        Ok(value) => Ok(value),
        Err(persistence::Error::Bound(bound)) => Err(bound),
        Err(error) => panic!("a read of a held second is refused only on a bound: {error}"),
    }
}

/// Checks that the App at `index` of `plain`, found by key in the packed `second`, has the
/// name, members and index it has in `plain`, and, where the second holds its process rows, the
/// readings, the processes and the page of members.
fn assert_app_reads_as(second: Second<'_>, plain: &AppsSecond, index: usize, message: &str) {
    let plain_second = Second::unheld(plain);
    let row = &plain.apps[index];
    let Some(found) = second.app(&row.name) else {
        panic!("{message}: not found");
    };
    assert_eq!(**found.name(), *row.name, "{message}");
    assert_eq!(found.members(), row.members, "{message}");
    assert_eq!(found.index(), index, "{message}");
    let Ok(processes) = second.processes() else {
        return;
    };
    let plain_found = plain_second.app(&row.name).unwrap();
    let json = |found: crate::store::block::AppAt<'_>| {
        refused_on(
            found
                .presentation()
                .map(|view| crate::live_app_response::json::app(&view, index, None)),
        )
    };
    assert_eq!(json(found), json(plain_found), "{message}");
    let keys = |members: &mut dyn Iterator<Item = super::super::ProcessAt<'_>>| -> Vec<(i32, u64)> {
        members
            .map(|process| (process.pid(), unique(process.creation())))
            .collect()
    };
    let plain_processes = plain_second.processes().unwrap();
    assert_eq!(
        keys(&mut processes.members_of(found)),
        keys(&mut plain_processes.members_of(plain_found)),
        "{message}"
    );
    if let Some(observation) = &plain.observation {
        let detail = Detail {
            selected: observation,
            app_index: u32::try_from(index).unwrap(),
            page: Page {
                offset: 0,
                limit: PROCESSES_PAGE_ROWS_MAX,
            },
        };
        let read = refused_on(plain_second.app_detail(&detail));
        assert!(read.is_ok(), "{message}");
        assert_eq!(refused_on(second.app_detail(&detail)), read, "{message}");
    }
}

/// The pages a reader of `count` rows is asked for, at most `limit_max` rows each: one row,
/// a few, and the most a page holds, from the first row, the second, the middle, the last and
/// the end.
fn pages_over(count: usize, limit_max: u32) -> impl Iterator<Item = Page> {
    let count = u32::try_from(count).unwrap();
    let offsets: BTreeSet<u32> = [0, 1, count / 2, count.saturating_sub(1), count]
        .into_iter()
        .filter(|offset| *offset <= count)
        .collect();
    offsets.into_iter().flat_map(move |offset| {
        [1, 2, 3, 7, limit_max]
            .into_iter()
            .map(move |limit| Page { offset, limit })
    })
}

/// Where the cases of the block of `seed` lie, as second indexes of a block of `len` seconds.
struct Layout {
    late: usize,
    exec: usize,
    exit: usize,
    empty: usize,
    refused: usize,
    restart: usize,
}

impl Layout {
    fn of(seed: u64, len: usize) -> Self {
        let at = |offset: u64, span: usize| usize::try_from(seed * 7 + offset).unwrap() % span + 3;
        let half = len / 2;

        Self {
            late: at(1, half - 6),
            exec: at(2, half - 6),
            exit: at(3, half - 6),
            empty: half - 2,
            refused: half - 1,
            restart: half,
        }
    }
}

/// A block of 31 to 64 plain seconds, made from `seed`, holding the cases `Layout` places.
pub(in crate::store) fn generated_block(
    seed: u64,
    budget: &Arc<ByteBudget>,
) -> Vec<Arc<AppsSecond>> {
    let len = usize::try_from(31 + seed % 34).unwrap();
    let layout = Layout::of(seed, len);
    let names = Names::new(budget);
    let mut random = seed;
    let mut kept = BTreeMap::new();
    let mut seconds: Vec<Arc<AppsSecond>> = Vec::with_capacity(len);
    for index in 0..len {
        let processes = generated_processes(index, &layout, &mut random, &mut kept);
        let second = generated_second(index, &layout, &processes, &names, seconds.last(), budget);
        seconds.push(Arc::new(second));
    }

    seconds
}

/// One process at one second as the generator makes it.
struct Generated {
    pid: i32,
    unique_id: u64,
    pid_version: i32,
    name: Name,
    app: Option<&'static [u8]>,
    program_unavailable: Option<IdentityUnavailable>,
    app_attribution: Option<Unavailable>,
    state: ProcessState,
    amounts: [Amount; 11],
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Name {
    Worker,
    /// The same bytes as `Worker` in another allocation.
    WorkerAgain,
    Exec,
    None,
}

/// The processes of the second at `index`. About one process in five draws new amounts, and
/// the rest keep the amounts `kept` holds for them from the second before, which this updates.
fn generated_processes(
    index: usize,
    layout: &Layout,
    random: &mut u64,
    kept: &mut BTreeMap<(i32, u64), [Amount; 11]>,
) -> Vec<Generated> {
    let run = if index >= layout.restart { 1_000 } else { 0 };
    let mut processes = Vec::with_capacity(9);
    let mut add = |pid, unique_id: u64, app, random: &mut u64| {
        let key = (pid, run + unique_id);
        let amounts = match kept.get(&key) {
            Some(&amounts) if !next(random).is_multiple_of(5) => amounts,
            _ => generated_amounts(random),
        };
        kept.insert(key, amounts);
        processes.push(Generated {
            pid,
            unique_id: run + unique_id,
            pid_version: 1,
            name: Name::Worker,
            app,
            program_unavailable: None,
            app_attribution: None,
            state: ProcessState::Live,
            amounts,
        });
    };
    add(10, 1, Some(&b"/A.app"[..]), random);
    if index >= layout.late {
        add(15, 1, Some(&b"/B.app"[..]), random);
    }
    add(20, 2, Some(&b"/A.app"[..]), random);
    if index <= layout.exit {
        add(30, 3, Some(&b"/C.app"[..]), random);
    }
    if !next(random).is_multiple_of(3) {
        add(40, 4, Some(&b"/D.app"[..]), random);
    }
    add(50, 500, Some(&b"/A.app"[..]), random);
    add(50, 501, None, random);
    add(60, 6, None, random);
    add(70, 7, Some(&b"/E.app"[..]), random);
    generated_cases(index, layout, &mut processes);

    processes
}

/// Puts the exec, the exit, the 64-bit values, the unavailable amount and the App that comes
/// and goes twice where `layout` and the second's `index` place them.
fn generated_cases(index: usize, layout: &Layout, processes: &mut [Generated]) {
    for process in processes {
        match process.pid {
            20 if index >= layout.exec => {
                process.pid_version = 2;
                process.name = Name::Exec;
            }
            20 if index % 5 == 4 => process.name = Name::WorkerAgain,
            30 if index == layout.exit => process.state = ProcessState::ExitedVerified,
            40 if index % 3 == 1 => {
                process.program_unavailable = Some(IdentityUnavailable::Gone);
                process.app_attribution = Some(Unavailable::Unknown);
            }
            50 if process.app.is_none() => process.name = Name::None,
            60 if index % 2 == 1 => process.amounts[6] = Amount::Known(u64::MAX),
            60 => process.amounts[6] = Amount::Known(1 << 63),
            70 if index % 4 == 2 => process.amounts[0] = Amount::Unavailable(Unavailable::Refused),
            70 if (4..8).contains(&(index % 16)) => process.app = Some(&b"/C.app"[..]),
            _ => {}
        }
    }
}

fn generated_amounts(random: &mut u64) -> [Amount; 11] {
    let mut amounts = [Amount::Known(0); 11];
    for amount in &mut amounts {
        let roll = next(random);
        *amount = match roll % 8 {
            0 => Amount::Unavailable(Unavailable::Gone),
            1 | 2 => Amount::Known(roll >> 8),
            _ => Amount::Known(5),
        };
    }

    amounts
}

/// The next value of a xorshift sequence that `random` holds the state of.
fn next(random: &mut u64) -> u64 {
    *random ^= *random << 13;
    *random ^= *random >> 7;
    *random ^= *random << 17;
    *random
}

fn unique(creation: Option<ProcessCreation>) -> u64 {
    creation.unwrap().unique_id
}

/// The executable names a generated block holds.
struct Names {
    worker: Arc<ProcessName>,
    worker_again: Arc<ProcessName>,
    exec: Arc<ProcessName>,
}

impl Names {
    fn new(budget: &Arc<ByteBudget>) -> Self {
        let name = |bytes: &[u8]| {
            ProcessName::capture(&Bytes::new(bytes.to_vec()).unwrap(), budget).unwrap()
        };

        Self {
            worker: name(b"worker"),
            worker_again: name(b"worker"),
            exec: name(b"exec'd"),
        }
    }

    fn of(&self, name: Name) -> Option<Arc<ProcessName>> {
        match name {
            Name::Worker => Some(Arc::clone(&self.worker)),
            Name::WorkerAgain => Some(Arc::clone(&self.worker_again)),
            Name::Exec => Some(Arc::clone(&self.exec)),
            Name::None => None,
        }
    }
}

/// The plain second at `index` holding `processes`, sharing each row that reads as it read at
/// `previous`, as a capture shares them.
fn generated_second(
    index: usize,
    layout: &Layout,
    processes: &[Generated],
    names: &Names,
    previous: Option<&Arc<AppsSecond>>,
    budget: &Arc<ByteBudget>,
) -> AppsSecond {
    let observation = observation(index, layout);
    if index == layout.empty {
        return AppsSecond {
            observation: Some(observation),
            missing: Some(Missing::TooMany),
            processes: Err(Missing::TooMany),
            ..AppsSecond::default()
        };
    }

    let mut keys: Vec<&[u8]> = processes.iter().filter_map(|process| process.app).collect();
    keys.sort_unstable();
    keys.dedup();
    let keys: Vec<Vec<u8>> = keys.iter().map(|key| key.to_vec()).collect();
    let mut second = apps_of(observation, &keys, previous.map(AsRef::as_ref), budget);
    second.consecutive = index != layout.restart;
    if index == layout.refused {
        second.processes = Err(Missing::Budget);
        return second;
    }

    let previous = previous.and_then(|previous| previous.processes.as_ref().ok());
    second.processes = Ok(Arc::new(payload_of(
        observation,
        processes,
        &keys,
        names,
        previous.map(AsRef::as_ref),
        budget,
    )));
    second
}

fn observation(index: usize, layout: &Layout) -> Observation {
    let tick = u64::try_from(index).unwrap();
    let run = if index >= layout.restart {
        [7, 8]
    } else {
        [5, 6]
    };

    Observation {
        recording: [1, 2],
        stamp: Stamp {
            boot: [3, 4],
            run,
            publication: tick + 1,
            continuous_ns: Some(tick * NS),
            awake_ns: Some(tick * NS),
        },
        run_tick: tick,
        wall_ns: Some(1_020 * NS + tick * NS / 2),
    }
}

/// The plain second of `observation` holding the Apps of `keys`, which ascend, and no
/// processes.
fn apps_of(
    observation: Observation,
    keys: &[Vec<u8>],
    previous: Option<&AppsSecond>,
    budget: &Arc<ByteBudget>,
) -> AppsSecond {
    let apps = keys
        .iter()
        .map(|key| app(key, observation.run_tick))
        .collect();
    let sample = Sample::of_apps(observation, Some(NS), apps);
    let mut second = AppsSecond::default();
    second.apps.replace_admitted(keys.len(), budget).unwrap();
    second.fill(&sample, previous, budget);
    second.processes = Err(Missing::Unread);

    second
}

/// Two plain seconds, at index 0 and 1, of `APPS_PAST_HALF` Apps each and none shared, so a
/// block of both names more Apps than it holds.
pub(in crate::store) fn past_the_app_bound(budget: &Arc<ByteBudget>) -> [AppsSecond; 2] {
    let keys = |first: usize| -> Vec<Vec<u8>> {
        (first..first + APPS_PAST_HALF)
            .map(|index| format!("/App {index:05}.app").into_bytes())
            .collect()
    };

    [0, 1].map(|index| apps_only(index, &keys(index * APPS_PAST_HALF), budget))
}

/// The plain second at `index` holding the Apps `keys` and no process rows.
pub(in crate::store) fn apps_only(
    index: usize,
    keys: &[Vec<u8>],
    budget: &Arc<ByteBudget>,
) -> AppsSecond {
    let layout = Layout::of(1, 40);

    apps_of(observation(index, &layout), keys, None, budget)
}

/// The plain second at `index` holding a live process of each pid of `pids`, outside any App,
/// and no Apps.
fn processes_only(
    index: usize,
    pids: std::ops::Range<i32>,
    names: &Names,
    budget: &Arc<ByteBudget>,
) -> AppsSecond {
    let observation = observation(index, &Layout::of(1, 40));
    let processes: Vec<Generated> = pids
        .map(|pid| Generated {
            pid,
            unique_id: 1,
            pid_version: 1,
            name: Name::Worker,
            app: None,
            program_unavailable: None,
            app_attribution: None,
            state: ProcessState::Live,
            amounts: [Amount::Known(5); 11],
        })
        .collect();
    let mut second = apps_of(observation, &[], None, budget);
    let payload = payload_of(observation, &processes, &[], names, None, budget);
    second.processes = Ok(Arc::new(payload));

    second
}

/// The App `key` as the generator reads it at tick `tick`. About one App in three changes its
/// readings at every tick and the rest keep them, and at some ticks an App reads a value that
/// needs all 64 bits, an unavailable measure, an overflowed count or an incomplete membership.
fn app(key: &[u8], tick: u64) -> App {
    let seed: u64 = key.iter().map(|&byte| u64::from(byte)).sum();
    let changes = seed.is_multiple_of(3);
    let amount = if changes {
        seed * 1_000 + tick
    } else {
        seed * 1_000
    };
    let known = if seed.is_multiple_of(7) && tick % 4 == 2 {
        u64::MAX
    } else {
        amount
    };
    let unavailable = seed % 5 == 2 && tick % 3 == 1;
    let measure = Measure {
        known,
        measured_members: u32::from(!unavailable),
        unavailable_members: u32::from(unavailable),
        overflowed: seed % 11 == 6 && tick % 5 == 3,
    };
    let details = Measure {
        known: amount * 3,
        ..measure
    };
    let members = 1 + u32::try_from(seed % 4).unwrap() + u32::from(changes && tick % 2 == 1);
    let display = String::from_utf8(key.to_vec()).unwrap();

    App {
        key: Key {
            kind: KeyKind::Bundle,
            bytes: key.to_vec(),
            display: display.clone(),
        },
        display_name: display,
        members,
        live_members: members - u32::from(tick % 6 == 5),
        membership_complete: !(seed % 6 == 5 && tick.is_multiple_of(2)),
        cpu_ns: measure,
        energy_nj: Measure {
            known: amount + 1,
            ..measure
        },
        gpu_ns: measure,
        disk_read_bytes: details,
        disk_written_bytes: Measure {
            known: amount * 5,
            ..details
        },
        disk_io_bytes: measure,
        resident_bytes: Measure {
            known: seed,
            ..measure
        },
        footprint_bytes: details,
    }
}

/// The details of the App `key` as the generator reads them at tick `tick`, which vary as `app`
/// varies the App's readings by its key and the tick: the live members change, and at some ticks
/// a measure needs all 64 bits, is unavailable for its member or overflowed.
fn details(key: &[u8], tick: u64) -> AppDetails {
    let app = app(key, tick);

    AppDetails {
        live_members: app.live_members,
        disk_read_bytes: app.disk_read_bytes,
        disk_written_bytes: app.disk_written_bytes,
        footprint_bytes: app.cpu_ns,
    }
}

/// The plain payload of `processes`, whose Apps are `keys`, each row shared with the row
/// `previous` holds for the same process where the two read the same.
fn payload_of(
    observation: Observation,
    processes: &[Generated],
    keys: &[Vec<u8>],
    names: &Names,
    previous: Option<&Payload>,
    budget: &Arc<ByteBudget>,
) -> Payload {
    let previous_rows = previous.map_or(&[][..], Payload::plain_rows);
    let mut rows = Vec::with_capacity(processes.len());
    let mut app_indexes = Vec::with_capacity(processes.len());
    for process in processes {
        let row = generated_row(process, names);
        let held = previous_rows
            .iter()
            .find(|shared| shared.row.pid == row.pid && shared.row.creation == row.creation);
        rows.push(Shared::share(row, held, budget).unwrap());
        let app_index = process.app.map(|app| {
            let index = keys.iter().position(|key| key == app).unwrap();
            u32::try_from(index).unwrap()
        });
        app_indexes.push(AppIndex::of(app_index));
    }
    let details = keys
        .iter()
        .map(|key| Shared::admit(details(key, observation.run_tick), budget).unwrap())
        .collect();

    Payload {
        selected: observation,
        previous: None,
        rows: ProcessRows::Plain { rows, app_indexes },
        apps: details,
        reservation: budget.reserve(64).unwrap(),
    }
}

fn generated_row(process: &Generated, names: &Names) -> ProcessRow {
    let [
        cpu,
        energy,
        gpu,
        read,
        written,
        io,
        resident,
        footprint,
        wakeups,
        pageins,
        threads,
    ] = process.amounts.map(CompactAmount::capture);

    ProcessRow {
        pid: process.pid,
        creation: Some(ProcessCreation {
            unique_id: process.unique_id,
            pid_version: process.pid_version,
        }),
        executable_name: names.of(process.name),
        program_unavailable: process.program_unavailable,
        cpu_ns: cpu,
        energy_nj: energy,
        gpu_ns: gpu,
        disk_read_bytes: read,
        disk_written_bytes: written,
        disk_io_bytes: io,
        resident_bytes: resident,
        footprint_bytes: footprint,
        idle_wakeups: wakeups,
        pageins,
        threads,
        app_attribution: process.app_attribution,
        state: process.state,
    }
}
