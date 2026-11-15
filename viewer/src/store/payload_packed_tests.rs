use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use collector::ring::{IdentityUnavailable, ProcessCreation};

use super::super::{
    AppDetails, AppIndex, CompactAmount, Payload, ProcessName, ProcessRow, ProcessRows, Shared,
};
use super::{KeptPlain, ProcessTable, pack_seconds, processes_read_as};
use crate::apps::{Amount, PROCESS_INTERVALS_MAX, ProcessState, Stamp, Unavailable};
use crate::live_app_response::{App, Bytes, Key, KeyKind, Measure, Observation, Sample};
use crate::store::{APPS_PER_SECOND_MAX, AppsSecond, ByteBudget, Missing};

const BUDGET_BYTES: usize = 256 * 1024 * 1024;
const NS: u64 = 1_000_000_000;
const SEEDS: u64 = 40;
/// One App more than half of what a block holds.
pub(in crate::store) const APPS_PAST_HALF: usize = APPS_PER_SECOND_MAX / 2 + 1;

#[test]
fn every_second_of_a_generated_block_rebuilds_equal_to_its_plain_second() {
    // Goal: a packed block reads at each of its seconds exactly what the plain second held:
    // its header, each process in its order with every reading and its App index, and each
    // App detail.
    // Method: generate a block of 31 to 64 plain seconds from each of forty seeds, holding an
    // exec, an exit, unavailable amounts, a process and Apps absent from some seconds, an App
    // that comes and goes twice, a process first listed between two the block holds, one pid
    // held twice with two creations, values that need all 64 bits, a second with no Apps, a
    // second whose process rows were refused and a restart, where about one process in five
    // changes its amounts each second and the rest keep their rows, pack it, and compare every
    // second with its plain second by `==`, and each of its processes as `find` reads it.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));

    for seed in 1..=SEEDS {
        let plain = generated_block(seed, &budget);
        let (shared_count, row_count) = shared_rows(&plain);
        assert!(
            4 * shared_count >= row_count,
            "seed {seed}: {shared_count} of {row_count}"
        );
        let packed = pack_seconds(&plain, None, &budget).unwrap();

        assert_eq!(packed.len(), plain.len(), "seed {seed}");
        for (index, (packed, plain)) in packed.iter().zip(&plain).enumerate() {
            let message = format!("seed {seed} second {index}");
            if let Ok(payload) = &packed.processes {
                assert!(payload.packed(), "{message}");
            }
            assert!(**packed == **plain, "{message}");
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

    let packed = pack_seconds(&seconds, None, &budget).unwrap();

    for (index, (packed, plain)) in packed.iter().zip(&seconds).enumerate() {
        assert!(**packed == **plain, "second {index}");
    }
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

    let packed = pack_seconds(&plain, None, &budget).unwrap();

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
        let packed = pack_seconds(&plain, None, &budget).unwrap();
        let mut under: BTreeMap<Vec<u8>, BTreeSet<(i32, u64)>> = BTreeMap::new();
        let mut outside: BTreeSet<(i32, u64)> = BTreeSet::new();
        for second in &plain {
            let Ok(payload) = &second.processes else {
                continue;
            };
            let members = payload.members(None, &second.apps);
            outside.extend(members.map(|process| (process.pid(), unique(process.creation()))));
            for (app_index, app) in (0..).zip(second.apps.iter()) {
                let members = payload.members(Some(app_index), &second.apps);
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
                let packed_members = packed_payload.members(Some(app_index), &packed.apps);
                let bound = under[&app.name.bytes].len();
                assert_eq!(packed_members.size_hint().1, Some(bound), "{message}");
                let plain_members = plain_payload.members(Some(app_index), &plain.apps);
                assert_eq!(keys(packed_members), keys(plain_members), "{message}");
            }
            let packed_outside = packed_payload.members(None, &packed.apps);
            assert_eq!(
                packed_outside.size_hint().1,
                Some(outside.len()),
                "{message}"
            );
            assert_eq!(
                keys(packed_outside),
                keys(plain_payload.members(None, &plain.apps)),
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

    assert_eq!(pack_seconds(&seconds, None, &budget), Err(KeptPlain::Bound));
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
        pack_seconds(&seconds, None, &budget).err(),
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

    let packed = pack_seconds(&plain, None, &budget);

    assert_eq!(packed, Err(KeptPlain::Budget));
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
        let mut table = ProcessTable::pack(&plain, None, &budget).unwrap();
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
            apps: plain_payload.apps.clone(),
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
        .zip(1..)
        .map(|(key, amount)| app(key, amount))
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

fn app(key: &[u8], amount: u64) -> App {
    let measure = measure(amount);
    let display = String::from_utf8(key.to_vec()).unwrap();

    App {
        key: Key {
            kind: KeyKind::Bundle,
            bytes: key.to_vec(),
            display: display.clone(),
        },
        display_name: display,
        members: 1,
        live_members: 1,
        membership_complete: true,
        cpu_ns: measure,
        energy_nj: measure,
        gpu_ns: measure,
        disk_read_bytes: measure,
        disk_written_bytes: measure,
        disk_io_bytes: measure,
        resident_bytes: measure,
        footprint_bytes: measure,
    }
}

fn measure(known: u64) -> Measure {
    Measure {
        known,
        measured_members: 1,
        unavailable_members: 0,
        overflowed: false,
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
        .zip(1..)
        .map(|(_, amount)| {
            let details = AppDetails {
                live_members: 1,
                disk_read_bytes: measure(amount),
                disk_written_bytes: measure(amount),
                footprint_bytes: measure(amount),
            };
            Shared::admit(details, budget).unwrap()
        })
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
