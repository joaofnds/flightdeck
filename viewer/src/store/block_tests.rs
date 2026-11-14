use std::sync::Arc;

use collector::processes::{Process, Usage};

use super::{AppsCursor, AppsSpan, BLOCK_DEPARTED_MAX, BLOCK_SECONDS_MAX, Block, BlockSpan};
use crate::store::Buffers;
use crate::store::apps::TickApps;
use crate::store::payload::row_tests::{seconds_of, task};
use crate::store::{
    AppRow, AppsSecond, ByteBudget, Held, MachineSecond, Mailbox, Missing, Payload, SECONDS_MAX,
};

const BUDGET_BYTES: usize = 256 * 1024 * 1024;
const WALL_NS: u64 = 1_000 * 1_000_000_000;
const MINUTE_SECONDS: u64 = 60;

#[test]
fn a_second_reads_its_header_by_index() {
    // Goal: a reader that takes a second's header by index reads what the second holds.
    // Method: hold three seconds read through the real producer in one block, and a fourth that
    // holds no Apps because its tick had too many, and compare each header field read by index
    // with the field of the second the block was given.
    let mut seconds = owner_then_arrival();
    seconds.push(Arc::new(AppsSecond {
        missing: Some(Missing::TooMany),
        ..AppsSecond::default()
    }));

    let block = block_of(&seconds);

    assert_eq!(block.len(), seconds.len());
    for (index, second) in seconds.iter().enumerate() {
        let header = block.second(index);
        assert_eq!(header.observation(), second.observation, "second {index}");
        assert_eq!(header.consecutive(), second.consecutive, "second {index}");
        assert_eq!(header.elapsed_ns(), second.elapsed_ns, "second {index}");
        assert_eq!(header.missing(), second.missing.as_ref(), "second {index}");
        assert_eq!(
            header.completeness(),
            second.completeness(),
            "second {index}"
        );
        assert_eq!(header.totals(), &second.totals, "second {index}");
        assert_eq!(header.origin(), second.metadata.origin, "second {index}");
        assert_eq!(header.app_count(), second.apps.len(), "second {index}");
    }
    let missing: Vec<Option<&Missing>> = (0..block.len())
        .map(|index| block.second(index).missing())
        .collect();
    assert_eq!(missing, [None, None, None, Some(&Missing::TooMany)]);
}

#[test]
fn a_cursor_reads_exactly_the_apps_of_each_second_in_key_order() {
    // Goal: a reader that ranks or merges a whole second's Apps reads every App of that second
    // and no other, in key order.
    // Method: hold three seconds in one block, the third with an App the first two lack, and
    // read each through one cursor, oldest first and then back to the first.
    let seconds = owner_then_arrival();
    let block = block_of(&seconds);
    let mut cursor = AppsCursor::default();

    for (index, second) in seconds.iter().enumerate() {
        assert_eq!(
            cursor.apps(block.second(index)),
            second.apps.as_slice(),
            "second {index}"
        );
    }
    assert_eq!(
        names(cursor.apps(block.second(1))),
        [b"/Owner.app".as_slice()]
    );
    assert_eq!(
        names(cursor.apps(block.second(2))),
        [b"/Arrival.app".as_slice(), b"/Owner.app".as_slice()]
    );
    assert_eq!(cursor.apps(block.second(0)), seconds[0].apps.as_slice());
}

#[test]
fn an_app_followed_through_a_block_reads_its_row_at_each_second() {
    // Goal: a reader that follows one App through a block reads the row each second holds for
    // it, at the App's place in that second, and nothing where the second does not hold it.
    // Method: find both Apps of a three second block by key at every second, where one App
    // arrives at the third second ahead of the other in key order.
    let seconds = owner_then_arrival();
    let block = block_of(&seconds);
    let owner = Arc::clone(&seconds[0].apps[0].name);
    let arrival = Arc::clone(&seconds[2].apps[0].name);

    let owner = |index| block.second(index).app(&owner);
    let arrival = |index| block.second(index).app(&arrival);

    for (index, place) in [(0, 0), (1, 0), (2, 1)] {
        let found = owner(index).unwrap();
        assert_eq!(found.row(), &seconds[index].apps[place], "second {index}");
        assert_eq!(found.index(), place, "second {index}");
    }
    assert!(arrival(0).is_none());
    assert!(arrival(1).is_none());
    assert_eq!(arrival(2).unwrap().row(), &seconds[2].apps[0]);
    assert_eq!(arrival(2).unwrap().index(), 0);
}

#[test]
fn a_process_followed_through_a_block_reads_its_row_and_app_at_each_second() {
    // Goal: a reader that follows one process through a block reads the row each second holds
    // for it with the App index of that second, and nothing where the second lacks it.
    // Method: find both processes of a three second block by pid and creation at every second,
    // where the second process arrives at the third second in an App that moves the first
    // one's App from index 0 to 1.
    let seconds = owner_then_arrival();
    let block = block_of(&seconds);
    let creation = |pid| {
        payload(&seconds[2])
            .iter()
            .find(|row| row.pid == pid)
            .unwrap()
            .creation
    };

    let find = |index, pid| {
        let processes = block.second(index).processes().unwrap();
        processes.find(pid, creation(pid))
    };

    for (index, app_index) in [(0, Some(0)), (1, Some(0)), (2, Some(1))] {
        let found = find(index, 7).unwrap();
        let place = payload(&seconds[index])
            .iter()
            .position(|row| row.pid == 7)
            .unwrap();
        assert!(
            std::ptr::eq(found.row(), &raw const payload(&seconds[index])[place]),
            "second {index}"
        );
        assert_eq!(found.app_index(), app_index, "second {index}");
    }
    assert!(find(0, 9).is_none());
    assert!(find(1, 9).is_none());
    assert_eq!(find(2, 9).unwrap().row().pid, 9);
    assert_eq!(find(2, 9).unwrap().app_index(), Some(0));
}

#[test]
fn a_process_read_by_place_carries_its_own_app_index_into_its_presentation() {
    // Goal: a reader that takes a second's processes by place, as the pages do, shows each in
    // the App the second gives it, and cannot pair a row with another row's index.
    // Method: read every process of the third second by place, where the arriving App moved
    // the owner's index, and compare each presentation's App index with the payload's index
    // at that place.
    let seconds = owner_then_arrival();
    let block = block_of(&seconds);

    let processes = block.second(2).processes().unwrap();

    assert_eq!(processes.len(), 2);
    let mut pids = Vec::with_capacity(processes.len());
    for (place, process) in processes.iter().enumerate() {
        let pid = process.presentation().pid;
        // The Apps lie in key order, so the arriving App is App 0 and the owner App 1.
        let expected = match pid {
            7 => Some(1),
            9 => Some(0),
            _ => panic!("place {place} holds pid {pid}, which the second never took"),
        };
        assert_eq!(process.app_index(), expected, "place {place}");
        assert_eq!(process.presentation().app_index, expected, "place {place}");
        assert_eq!(pid, payload(&seconds[2])[place].pid, "place {place}");
        pids.push(pid);
    }
    pids.sort_unstable();
    assert_eq!(pids, [7, 9]);
}

#[test]
fn taking_a_second_replaces_the_newest_block_and_leaves_its_holder_unchanged() {
    // Goal: the store takes a second by putting a new plain block holding one more second in
    // place of the newest, and a View that holds the block before reads its seconds unchanged.
    // Method: take two seconds of one wall window, hold the newest block as a View would, take
    // a third, and compare both blocks.
    let mut span = span(8);
    take(&mut span, 0, 1);
    take(&mut span, 1, 2);
    let held = Arc::clone(span.blocks().next_back().unwrap());

    take(&mut span, 2, 3);

    assert_eq!(span.blocks().len(), 1);
    let newest = span.blocks().next_back().unwrap();
    assert!(!Arc::ptr_eq(newest, &held));
    assert_eq!(marks(&held), [1, 2]);
    assert_eq!(marks(newest), [1, 2, 3]);
    for index in 0..2 {
        assert!(
            std::ptr::eq(held.second(index).totals(), newest.second(index).totals()),
            "second {index}"
        );
    }
}

#[test]
fn a_block_closes_when_a_second_of_other_thirty_wall_seconds_arrives() {
    // Goal: a block holds the seconds of one window of thirty wall seconds and no other.
    // Method: take seconds at the first and the last wall second of one window, wall seconds
    // 1020 and 1049, and at the first of the next, 1050, and read which block holds each. A
    // window of sixty wall seconds would hold all three.
    let mut span = span(8);
    take(&mut span, 20, 1);
    take(&mut span, 49, 2);

    take(&mut span, 50, 3);

    let blocks: Vec<_> = span.blocks().map(|block| marks(block)).collect();
    assert_eq!(blocks, [vec![1, 2], vec![3]]);
}

#[test]
fn a_block_closes_when_it_holds_sixty_four_seconds() {
    // Goal: no block holds more than `BLOCK_SECONDS_MAX` seconds, even inside one window.
    // Method: take one more than that many seconds at one wall second, as ticks the collector
    // wrote faster than once a second would be, and count the seconds of each block.
    let mut span = span(BLOCK_SECONDS_MAX + 1);

    for mark in 0..=BLOCK_SECONDS_MAX {
        take(&mut span, 0, u64::try_from(mark).unwrap());
    }

    let counts: Vec<_> = span.blocks().map(|block| block.len()).collect();
    assert_eq!(counts, [BLOCK_SECONDS_MAX, 1]);
}

#[test]
fn two_ticks_in_one_wall_second_and_a_restarted_run_join_the_open_block() {
    // Goal: a block closes only on its window and its bound, so neither a second tick in one
    // wall second nor a run started again opens a block of its own.
    // Method: take two ticks at one wall second, then a second of a run started again, which
    // does not follow the tick before, and read the blocks.
    let mut span = span(8);
    take(&mut span, 0, 1);
    take(&mut span, 0, 2);

    span.push(WALL_NS + 1_000_000_000, |buffer| {
        *buffer = AppsSecond {
            elapsed_ns: Some(3),
            consecutive: false,
            ..AppsSecond::default()
        };
    });

    let blocks: Vec<_> = span.blocks().map(|block| marks(block)).collect();
    assert_eq!(blocks, [vec![1, 2, 3]]);
}

#[test]
fn seconds_leave_one_at_a_time_and_a_block_is_freed_with_its_last() {
    // Goal: the span lets go of its oldest second at each second past its bound, no reader
    // reaches a second that has left, and the block that held it is freed once its last second
    // has left.
    // Method: fill a span of three seconds from one block of four, read a View of it, then
    // take three seconds of the next window and read again.
    let mut span = span(3);
    for mark in 1..=4 {
        take(&mut span, 0, mark);
    }
    let mut view = AppsSpan::default();
    span.publish(&mut view);
    let oldest = Arc::downgrade(span.blocks().next().unwrap());

    assert_eq!(view.departed_count(), 1);
    assert_eq!(view.len(), 3);
    assert_eq!(view_marks(&view), [2, 3, 4]);
    assert_eq!(view.second(0).elapsed_ns(), Some(2));

    for mark in 5..=7 {
        take(&mut span, 30, mark);
    }
    span.publish(&mut view);

    assert_eq!(view.departed_count(), 0);
    assert_eq!(view_marks(&view), [5, 6, 7]);
    assert_eq!(view.blocks().len(), 1);
    assert!(oldest.upgrade().is_none());
}

#[test]
fn replacing_a_second_puts_a_new_block_in_its_blocks_place() {
    // Goal: a launch that installs the Apps of one second changes no block a View holds, and
    // no other block of the span.
    // Method: hold two blocks of two seconds each as a View would, replace the first second of
    // the second block, and compare both blocks with those held.
    let mut span = span(8);
    take(&mut span, 0, 1);
    take(&mut span, 1, 2);
    take(&mut span, 30, 3);
    take(&mut span, 31, 4);
    let held: Vec<_> = span.blocks().map(Arc::clone).collect();

    span.replace(2, |buffer| {
        *buffer = AppsSecond {
            elapsed_ns: Some(30),
            ..AppsSecond::default()
        };
    });

    let blocks: Vec<_> = span.blocks().collect();
    assert!(Arc::ptr_eq(blocks[0], &held[0]));
    assert!(!Arc::ptr_eq(blocks[1], &held[1]));
    assert_eq!(marks(blocks[1]), [30, 4]);
    assert_eq!(marks(&held[1]), [3, 4]);
    assert!(std::ptr::eq(
        blocks[1].second(1).totals(),
        held[1].second(1).totals()
    ));
}

#[test]
fn an_install_with_every_buffer_held_takes_a_slot_and_leaves_the_holder_unchanged() {
    // Goal: a launch installs while Views or fixed contexts hold every buffer it could write,
    // without growing the buffers, and the holder goes on reading what it held.
    // Method: over the fewest buffers a span takes, hold the published blocks and every other
    // buffer, check that none is free, install into both seconds, then compare the buffer
    // count, the span's seconds and the holder's.
    let capacity = 2;
    let mut span = BlockSpan::with_fewest_buffers(capacity);
    take(&mut span, 0, 1);
    take(&mut span, 1, 2);
    let mut view = AppsSpan::with_capacity(capacity);
    span.publish(&mut view);
    let others = span.hold_every_free_buffer();
    assert_eq!(span.free_count(), 0);
    let count = span.buffer_count();
    let install = |buffer: &mut AppsSecond| {
        *buffer = AppsSecond {
            elapsed_ns: Some(30),
            ..AppsSecond::default()
        };
    };

    span.replace(0, install);
    span.replace(1, install);

    assert_eq!(span.buffer_count(), count);
    let mut installed = AppsSpan::with_capacity(capacity);
    span.publish(&mut installed);
    assert_eq!(view_marks(&installed), [30, 30]);
    assert_eq!(view_marks(&view), [1, 2]);
    drop(others);
}

#[test]
fn a_span_with_departed_seconds_reads_across_a_block_boundary_in_both_directions() {
    // Goal: every reader of a span whose oldest block holds seconds that have left reaches the
    // seconds of the span and no other, across the boundary of two blocks, oldest first and
    // newest first.
    // Method: take six seconds into a span of four, the last four wall seconds of one window
    // and the first two of the next, so two seconds of the oldest block have left, and compare
    // each reader's seconds with lists written out here.
    let mut span = span(4);
    for (wall_second, mark) in [(46, 1), (47, 2), (48, 3), (49, 4), (50, 5), (51, 6)] {
        take(&mut span, wall_second, mark);
    }
    let mut view = AppsSpan::with_capacity(4);
    span.publish(&mut view);
    let mark = |second: super::Second<'_>| second.elapsed_ns().unwrap();
    let blocks = view.blocks();

    assert_eq!(
        blocks.iter().map(|block| marks(block)).collect::<Vec<_>>(),
        [vec![1, 2, 3, 4], vec![5, 6]]
    );
    assert_eq!(view.departed_count(), 2);
    assert_eq!(view.seconds().map(mark).collect::<Vec<_>>(), [3, 4, 5, 6]);
    assert_eq!(view.seconds().len(), 4);
    assert_eq!(
        view.seconds_from(1).map(mark).collect::<Vec<_>>(),
        [4, 5, 6]
    );
    assert_eq!(view.seconds_from(2).map(mark).collect::<Vec<_>>(), [5, 6]);
    assert_eq!(view.seconds_from(4).count(), 0);
    assert_eq!(
        view.seconds().rev().map(mark).collect::<Vec<_>>(),
        [6, 5, 4, 3]
    );
    assert_eq!(
        view.seconds_from(1).rev().map(mark).collect::<Vec<_>>(),
        [6, 5, 4]
    );
    let places: Vec<_> = view
        .places()
        .map(|(block, index)| {
            let block_index = blocks
                .iter()
                .position(|held| Arc::ptr_eq(held, block))
                .unwrap();
            (block_index, index)
        })
        .collect();
    assert_eq!(places, [(0, 2), (0, 3), (1, 0), (1, 1)]);
}

#[test]
fn a_launch_installing_the_newest_minute_first_ends_with_the_blocks_of_a_load_in_order() {
    // Goal: the order a launch installs its minutes in leaves no trace in the blocks, so a span
    // filled the newest minute first and then from the oldest up equals one loaded in order.
    // Method: take three minutes of empty seconds into a span ten seconds too short for them,
    // install the newest minute and then the rest from the oldest up, and compare its blocks
    // and the seconds a reader reaches with those of the same seconds taken in order. The ten
    // seconds that have left are installed into neither, since no reader reaches them.
    let capacity = usize::try_from(3 * MINUTE_SECONDS - 10).unwrap();
    let mut installed = span(capacity);
    let mut loaded = span(capacity);
    for wall_second in 0..3 * MINUTE_SECONDS {
        take(&mut installed, wall_second, 0);
        take(&mut loaded, wall_second, wall_second + 1);
    }
    let departed = installed.span().departed_count();

    for minute in [2, 0, 1] {
        for wall_second in minute * MINUTE_SECONDS..(minute + 1) * MINUTE_SECONDS {
            let Some(index) = usize::try_from(wall_second).unwrap().checked_sub(departed) else {
                continue;
            };
            installed.replace(index, |buffer| {
                *buffer = AppsSecond {
                    elapsed_ns: Some(wall_second + 1),
                    ..AppsSecond::default()
                };
            });
        }
    }

    assert_eq!(departed, 10);
    assert_eq!(
        installed.span().departed_count(),
        loaded.span().departed_count()
    );
    assert_eq!(installed.blocks().len(), loaded.blocks().len());
    for (block_index, (installed, loaded)) in installed.blocks().zip(loaded.blocks()).enumerate() {
        let message = format!("block {block_index}");
        assert_eq!(
            installed.half_minute_index, loaded.half_minute_index,
            "{message}"
        );
        assert_eq!(installed.len(), loaded.len(), "{message}");
    }
    assert_eq!(installed.span().len(), capacity);
    let pairs = installed.span().seconds().zip(loaded.span().seconds());
    for (index, (installed, loaded)) in pairs.enumerate() {
        assert_eq!(installed.stored(), loaded.stored(), "second {index}");
    }
}

/// Three seconds of process 7 of `/Owner.app`, the third with process 9 of `/Arrival.app`,
/// which sorts before it, read through the real producer.
fn owner_then_arrival() -> Vec<Arc<AppsSecond>> {
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let ticks = [
        vec![process(7, 1_000_000)],
        vec![process(7, 2_000_000)],
        vec![process(7, 2_000_000), process(9, 4_000_000)],
    ];

    seconds_of(&budget, &ticks, |pid| match pid {
        9 => b"/Arrival.app",
        _ => b"/Owner.app",
    })
    .into_iter()
    .map(Arc::new)
    .collect()
}

fn process(pid: i32, user_ns: u64) -> Process {
    Process {
        pid,
        usage: Usage {
            user_ns,
            ..crate::store::startup_tests::usage(0)
        },
        task: task(1),
    }
}

/// One block holding `seconds`, each a wall second after the one before.
fn block_of(seconds: &[Arc<AppsSecond>]) -> Block {
    let mut block = Block::starting(WALL_NS, Arc::clone(&seconds[0]));
    for (offset, second) in (1..).zip(&seconds[1..]) {
        block = block
            .joining(WALL_NS + offset * 1_000_000_000, Arc::clone(second))
            .unwrap();
    }

    block
}

fn names(apps: &[AppRow]) -> Vec<&[u8]> {
    apps.iter().map(|app| app.name.bytes.as_ref()).collect()
}

fn payload(second: &AppsSecond) -> &Payload {
    second.processes.as_ref().unwrap()
}

/// A row of blocks holding at most `capacity` seconds, over twice as many buffers as the span
/// and the seconds that have left it hold.
fn span(capacity: usize) -> BlockSpan {
    let count = 2 * (capacity + BLOCK_DEPARTED_MAX);
    BlockSpan::new(capacity, Buffers::new(count, AppsSecond::default))
}

/// Takes into `span` a second at `wall_second` past `WALL_NS`, marked `mark`.
fn take(span: &mut BlockSpan, wall_second: u64, mark: u64) {
    span.push(WALL_NS + wall_second * 1_000_000_000, |buffer| {
        *buffer = AppsSecond {
            elapsed_ns: Some(mark),
            ..AppsSecond::default()
        };
    });
}

/// The mark of each second `block` holds, oldest first.
fn marks(block: &Block) -> Vec<u64> {
    (0..block.len())
        .map(|index| block.second(index).elapsed_ns().unwrap())
        .collect()
}

/// The mark of each second `view` holds, oldest first.
fn view_marks(view: &AppsSpan) -> Vec<u64> {
    view.seconds()
        .map(|second| second.elapsed_ns().unwrap())
        .collect()
}

#[test]
fn a_published_view_holds_the_stores_blocks_and_the_count_of_departed_seconds() {
    // Goal: a View shares the blocks of the store's App seconds rather than holding a copy of
    // each second, and no reader of it reaches a second that has left the span.
    // Method: take in twenty seconds more than the live span holds, one a wall second, publish
    // a View, and compare its blocks with the store's by pointer and its seconds by wall time.
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let mut held = Held::without_recording_with_payload(budget);
    let count = u64::try_from(SECONDS_MAX).unwrap() + 20;
    for tick in 1..=count {
        let second = MachineSecond {
            publication: tick,
            run_tick: tick,
            wall_ns: tick * NS,
            continuous_ns: Some(tick * NS),
            ..MachineSecond::default()
        };
        assert!(
            held.take(&RUN, &second, TickApps::Missing(&Missing::Unread)),
            "tick {tick}"
        );
    }

    let mailbox = Mailbox::new();
    held.publish(&mailbox);
    let view = mailbox.take().unwrap();

    let stored: Vec<_> = held.apps.blocks().collect();
    assert_eq!(view.apps.blocks().len(), stored.len());
    for (block_index, (viewed, stored)) in view.apps.blocks().iter().zip(stored).enumerate() {
        assert!(Arc::ptr_eq(viewed, stored), "block {block_index}");
    }
    // The oldest block holds wall seconds 1 to 29, and the span starts at 21.
    assert_eq!(view.apps.departed_count(), 20);
    assert_eq!(view.apps.len(), SECONDS_MAX);
    let wall_second =
        |second: super::Second<'_>| second.observation().unwrap().wall_ns.unwrap() / NS;
    assert_eq!(wall_second(view.apps.second(0)), 21);
    assert_eq!(view.apps.seconds().next().map(wall_second), Some(21));
    assert_eq!(view.apps.newest().map(wall_second), Some(count));
}

const NS: u64 = 1_000_000_000;

const RUN: collector::ring::RunIdentity = collector::ring::RunIdentity {
    recording: collector::ring::Identity([1, 1]),
    run: collector::ring::Identity([2, 2]),
    boot: collector::ring::Identity([3, 3]),
};
