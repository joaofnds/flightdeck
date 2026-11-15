use std::sync::Arc;

use collector::processes::Process;

use super::{
    AppsCursor, AppsSpan, BLOCK_DEPARTED_MAX, BLOCK_SECONDS_MAX, BLOCK_WALL_NS, Block, BlockSpan,
    DEPARTED_KEPT_MAX, Form, KeptPlain,
};
use crate::store::Buffers;
use crate::store::apps::{AppRows, TickApps};
use crate::store::packing::BLOCKS_OUT_MAX;
use crate::store::payload::packed::tests::{APPS_PAST_HALF, generated_block, past_the_app_bound};
use crate::store::payload::row_tests::{process, seconds_of};
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
        let held = payload(&seconds[index]).processes().nth(place).unwrap();
        assert!(found == held, "second {index}");
        assert_eq!(found.app_index(), app_index, "second {index}");
    }
    assert!(find(0, 9).is_none());
    assert!(find(1, 9).is_none());
    assert_eq!(find(2, 9).unwrap().pid(), 9);
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

    span.push(WALL_NS + 1_000_000_000, false, |buffer| {
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
    // has left and the packing thread has answered for it.
    // Method: fill a span of three seconds from one block of four, read a View of it, then
    // take three seconds of the next window, let the packing loop's body answer what was
    // handed over, and read again.
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
    span.pack_ready();
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
        installed.push(wall_ns(wall_second), true, |buffer| {
            *buffer = AppsSecond::default();
        });
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

#[test]
fn a_launch_installing_rows_the_newest_minute_first_packs_as_a_load_in_order_does() {
    // Goal: a launch that installs the rows of its minutes, the newest first and then the rest
    // from the oldest up, ends holding the blocks a load in order holds, packed and plain alike,
    // never reads the second before an install from a packed block, and never holds more plain
    // seconds with rows than a minute, three blocks and the ready blocks the launch's wait
    // leaves plain.
    // Method: take ten minutes of 50 seconds, a length a stop can seal, so the minutes begin at
    // every place a block's tens of seconds give and one at a block's first second, into a span
    // ten seconds too short for them, each second waiting for its Apps. Install each second's
    // rows, read through the real producer, minute by minute in the launch's order, reading the
    // second before each as the launch does and counting the plain seconds with rows after each.
    // The packing thread packs on a thread of its own, as in the app, and the launch waits for it
    // before each minute. End the launch, let each span settle, and compare each block's form and
    // place with those the same seconds taken in order make, and the seconds a reader reaches.
    // The ten seconds that have left are installed into neither, since no reader reaches them.
    const MINUTE_COUNT: u64 = 10;
    const SEALED_SECONDS: u64 = 50;
    let second_count = MINUTE_COUNT * SEALED_SECONDS;
    let capacity = usize::try_from(second_count - 10).unwrap();
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let mut rows = launch_rows(&budget, second_count);
    let mut in_order = launch_rows(&budget, second_count);
    for (second, of) in in_order.iter_mut().zip(&rows) {
        identity_of(second, of);
    }
    let (mut installed, mut loaded) = launch_spans(capacity, &mut in_order);
    let departed = installed.span().departed_count();

    let mut plain_max = 0;
    let order = std::iter::once(MINUTE_COUNT - 1).chain(0..MINUTE_COUNT - 1);
    for minute in order {
        installed.launch_wait();
        assert!(installed.ready_plain_count() <= BLOCKS_OUT_MAX);
        for wall_second in minute * SEALED_SECONDS..(minute + 1) * SEALED_SECONDS {
            let wall_index = usize::try_from(wall_second).unwrap();
            let Some(index) = wall_index.checked_sub(departed) else {
                continue;
            };
            if let Some(before) = index.checked_sub(1) {
                assert!(installed.stored(before).is_some());
            }
            installed.replace(index, |buffer| {
                std::mem::swap(buffer, &mut rows[wall_index]);
            });
            plain_max = plain_max.max(plain_with_rows_count(&installed));
        }
    }
    installed.end_awaiting();
    // The steps after the launch pack what waits, the oldest block's seconds that left the span
    // before any install included, which wait for their Apps until the launch ends.
    for span in [&mut installed, &mut loaded] {
        span.settle();
    }

    let block_seconds = usize::try_from(BLOCK_WALL_NS / 1_000_000_000).unwrap();
    let bound = crate::live_apps::committed::ENDPOINTS_MAX + (3 + BLOCKS_OUT_MAX) * block_seconds;
    assert!(
        plain_max <= bound,
        "plain seconds with rows at most: {plain_max} of {bound}"
    );
    assert!(loaded.span().packed_count() > 0);
    assert_blocks_alike(&installed, &loaded);
}

/// Two spans of `capacity` that take the seconds `in_order` at one wall second each: the first
/// as a launch takes them, without rows and waiting for their Apps, the second with their rows.
/// The packing thread of each packs on a thread of its own.
fn launch_spans(capacity: usize, in_order: &mut [AppsSecond]) -> (BlockSpan, BlockSpan) {
    let span = |capacity: usize| {
        BlockSpan::with_packing_thread(
            capacity,
            Buffers::new(2 * (capacity + BLOCK_DEPARTED_MAX), AppsSecond::default),
            Arc::new(ByteBudget::new(BUDGET_BYTES)),
        )
    };
    let mut installed = span(capacity);
    let mut loaded = span(capacity);
    for (wall_second, second) in (0..).zip(in_order) {
        installed.push(wall_ns(wall_second), true, |buffer| {
            *buffer = AppsSecond::default();
        });
        loaded.push(wall_ns(wall_second), false, |buffer| {
            std::mem::swap(buffer, second);
        });
    }

    (installed, loaded)
}

/// Checks that `installed` holds blocks of the forms, places and lengths `loaded` holds, and
/// that a reader reaches equal seconds in both.
fn assert_blocks_alike(installed: &BlockSpan, loaded: &BlockSpan) {
    assert_eq!(
        installed.span().departed_count(),
        loaded.span().departed_count()
    );
    assert_eq!(installed.blocks().len(), loaded.blocks().len());
    for (block_index, (installed, loaded)) in installed.blocks().zip(loaded.blocks()).enumerate() {
        let message = format!("block {block_index}");
        assert_eq!(installed.form, loaded.form, "{message}");
        assert_eq!(installed.awaiting, loaded.awaiting, "{message}");
        assert_eq!(
            installed.half_minute_index, loaded.half_minute_index,
            "{message}"
        );
        assert_eq!(installed.len(), loaded.len(), "{message}");
    }
    assert_eq!(installed.span(), loaded.span());
}

#[test]
fn a_block_past_a_bound_is_kept_plain_counted_by_its_cause_and_read_as_before() {
    // Goal: a closed block whose seconds name more Apps than a block holds stays plain for good,
    // the span counts it by its cause, and its seconds read as they did.
    // Method: take two seconds of 1,025 Apps each, none shared, into one block, close it with a
    // third second, pack twice, and compare the block's seconds with the ones taken.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let mut span = span(8);
    for second in past_the_app_bound(&budget) {
        span.push(WALL_NS, false, |buffer| *buffer = second);
    }
    let taken: Vec<Arc<AppsSecond>> = span.held().map(Arc::clone).collect();
    take(&mut span, 30, 0);

    span.pack_ready();

    assert_kept_plain(&span, KeptPlain::Bound, &taken);
    assert_eq!(span.span().second(1).app_count(), APPS_PAST_HALF);
}

#[test]
fn a_block_kept_plain_is_answered_once_and_leaves_room_for_the_next() {
    // Goal: the packing thread answers every block it is handed, packed or kept plain, and a
    // block kept plain is counted by its cause and never handed over again, so it leaves its
    // place among the blocks out to the next one that waits.
    // Method: close a block of two seconds of 1,025 Apps each, none shared, then two blocks of
    // one second each, hand over and answer what waits, exchange again, and count what the
    // second exchange handed over.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let mut span = span(8);
    for second in past_the_app_bound(&budget) {
        span.push(WALL_NS, false, |buffer| *buffer = second);
    }
    for (mark, wall_second) in (0..).zip([30, 60, 90]) {
        take(&mut span, wall_second, mark);
    }

    span.exchange();
    assert_eq!(span.answer_handed(), BLOCKS_OUT_MAX);
    span.exchange();

    assert_eq!(span.answer_handed(), 1);
    assert_eq!(span.span().kept_plain_count(KeptPlain::Bound), 1);
    assert_eq!(span.span().kept_plain_count(KeptPlain::Budget), 0);
    span.exchange();
    assert_eq!(span.answer_handed(), 0);
    assert_eq!(span.span().packed_count(), 2);
}

#[test]
fn a_block_the_budget_refuses_is_answered_once_and_leaves_room_for_the_next() {
    // Goal: a block kept plain because the budget had no room is counted by its cause and never
    // handed over again, as a block past a bound is not, so once the budget has room the block
    // that waits next is handed over and packed.
    // Method: close three blocks of one second each, take every byte the budget has left, hand
    // over and answer what waits, give the bytes back, exchange again, and count what each
    // exchange handed over.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let mut span = span_within(&budget);
    for (mark, wall_second) in (0..).zip([0, 30, 60, 90]) {
        take(&mut span, wall_second, mark);
    }
    let rest = budget.reserve(budget.maximum() - budget.used()).unwrap();

    span.exchange();
    assert_eq!(span.answer_handed(), BLOCKS_OUT_MAX);
    drop(rest);
    span.exchange();

    assert_eq!(span.answer_handed(), 1);
    assert_eq!(
        span.span().kept_plain_count(KeptPlain::Budget),
        BLOCKS_OUT_MAX
    );
    assert_eq!(span.span().kept_plain_count(KeptPlain::Bound), 0);
    span.exchange();
    assert_eq!(span.answer_handed(), 0);
    assert_eq!(span.span().packed_count(), 1);
}

#[test]
fn an_answer_for_a_block_no_longer_in_the_row_is_dropped() {
    // Goal: the packing thread's answer takes only the place of the very block it was handed,
    // so a block of the same half minute that came after it keeps its own seconds.
    // Method: hand over a closed block, clear the span and take the same wall seconds again
    // with other marks, answer the block handed over, close the new block, which exchanges,
    // and check the new block still holds its place, then that it is handed over and packed
    // with its own marks, and that once the span is cleared and every answer acted on, the
    // budget holds what it held before the first take.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let mut span = span_within(&budget);
    let used_before = budget.used();
    for (mark, wall_second) in [(1, 0), (2, 1), (3, 30)] {
        take(&mut span, wall_second, mark);
    }
    span.clear();
    for (mark, wall_second) in [(11, 0), (12, 1)] {
        take(&mut span, wall_second, mark);
    }
    let renewed = Arc::clone(span.blocks().next().unwrap());
    assert_eq!(span.answer_handed(), 1);

    take(&mut span, 30, 13);

    assert!(Arc::ptr_eq(span.blocks().next().unwrap(), &renewed));
    assert_eq!(span.answer_handed(), 1);
    span.exchange();
    let oldest = span.blocks().next().unwrap();
    assert_eq!(oldest.form, Form::Packed);
    assert_eq!(marks(oldest), [11, 12]);
    span.clear();
    span.pack_ready();
    assert_eq!(budget.used(), used_before);
}

#[test]
fn a_block_shares_first_rows_with_the_block_before_whether_or_not_its_answer_is_back() {
    // Goal: a block shares each first row that reads the same with the block before it, both
    // where the store has taken the block before's answer when it hands the block over and
    // where that answer is still out, so what a block shares does not depend on how fast the
    // packing thread runs.
    // Method: take two blocks whose seconds two producers read alike, with no row shared
    // between them, and a third second that closes the second block, once answering the first
    // block before the second is handed over and once after, and compare the first rows of
    // the second block's first second with those of the first block's.
    for answered_first in [false, true] {
        let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
        let mut span = span_within(&budget);
        let mut first = launch_rows(&budget, 2);
        let mut second = launch_rows(&budget, 2);
        predecessor_push(&mut span, 0, &mut first[0]);
        predecessor_push(&mut span, 1, &mut first[1]);
        predecessor_push(&mut span, 30, &mut second[0]);
        if answered_first {
            assert_eq!(span.answer_handed(), 1);
        }
        predecessor_push(&mut span, 31, &mut second[1]);
        take(&mut span, 60, 0);
        span.pack_ready();

        let [first, second, _] = predecessor_blocks(&span);
        let message = format!("answered first: {answered_first}");
        assert_eq!(first.form, Form::Packed, "{message}");
        assert_eq!(second.form, Form::Packed, "{message}");
        assert_eq!(
            predecessor_first_rows(second),
            predecessor_first_rows(first),
            "{message}"
        );
    }
}

#[test]
fn a_block_shares_no_first_row_with_a_block_before_kept_plain() {
    // Goal: a block after one kept plain shares none of its first rows, since that block has
    // no packed rows to share.
    // Method: take two blocks whose seconds two producers read alike, keep the first plain by
    // taking every byte of the budget while it is packed, give the bytes back, pack the
    // second, and check its first rows are the rows its plain seconds held.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let mut span = span_within(&budget);
    let mut first = launch_rows(&budget, 2);
    let mut second = launch_rows(&budget, 2);
    predecessor_push(&mut span, 0, &mut first[0]);
    predecessor_push(&mut span, 1, &mut first[1]);
    predecessor_push(&mut span, 30, &mut second[0]);
    let rest = budget.reserve(budget.maximum() - budget.used()).unwrap();
    assert_eq!(span.answer_handed(), 1);
    drop(rest);
    predecessor_push(&mut span, 31, &mut second[1]);
    let plain_rows = predecessor_first_rows(span.blocks().nth(1).unwrap());
    take(&mut span, 60, 0);

    span.pack_ready();

    let [first, second, _] = predecessor_blocks(&span);
    assert_eq!(first.form, Form::KeptPlain(KeptPlain::Budget));
    assert_eq!(second.form, Form::Packed);
    assert_eq!(predecessor_first_rows(second), plain_rows);
    assert_ne!(
        predecessor_first_rows(second),
        predecessor_first_rows(first)
    );
}

/// A span of eight seconds that packs within `budget`.
fn span_within(budget: &Arc<ByteBudget>) -> BlockSpan {
    let capacity = 8;
    let buffers = Buffers::new(2 * (capacity + BLOCK_DEPARTED_MAX), AppsSecond::default);

    BlockSpan::keeping_packing_end(capacity, buffers, Arc::clone(budget))
}

/// Takes `second` into `span` at `wall_second` past `WALL_NS`.
fn predecessor_push(span: &mut BlockSpan, wall_second: u64, second: &mut AppsSecond) {
    span.push(wall_ns(wall_second), false, |buffer| {
        std::mem::swap(buffer, second);
    });
}

/// The three blocks of `span`, oldest first.
fn predecessor_blocks(span: &BlockSpan) -> [&Arc<Block>; 3] {
    let blocks: Vec<&Arc<Block>> = span.blocks().collect();
    let Ok(blocks) = <[&Arc<Block>; 3]>::try_from(blocks) else {
        unreachable!("the span holds three blocks");
    };

    blocks
}

/// Where each process row of the first second of `block` lies in memory, in the second's order.
fn predecessor_first_rows(block: &Block) -> Vec<usize> {
    let payload = block.seconds[0].processes.as_ref().unwrap();
    let rows: Vec<usize> = payload
        .processes()
        .map(|process| process.row_address())
        .collect();
    assert!(!rows.is_empty());

    rows
}

#[test]
fn a_buffer_the_exchange_frees_keeps_what_clearing_it_in_place_leaves() {
    // Goal: a buffer whose readings the exchange hands to the packing thread is left as the
    // store's release left it when it cleared the buffer in place: the room of its App rows
    // where no launch reserved it, and an empty buffer where one did or there is no room.
    // Method: take a hundred seconds into a span of eight, by turns with App rows of room for 8,
    // of room for 9 that a launch reserved, and of no room, holding every buffer the span
    // holds, let go of them all at once, exchange, and read each free buffer. The two blocks
    // handed out hold their seconds, so only the later ones come free.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let buffers = Buffers::new(128 + BLOCK_DEPARTED_MAX, AppsSecond::default);
    let mut span = BlockSpan::keeping_packing_end(8, buffers, Arc::clone(&budget));
    let mut held = Vec::new();
    for (index, wall_second) in (0..100).enumerate() {
        let mut apps = match index % 3 {
            0 => AppRows::with_capacity(8),
            1 => AppRows::with_capacity(9),
            _ => AppRows::default(),
        };
        if index % 3 == 1 {
            apps.replace_admitted(9, &budget).unwrap();
        }
        span.push(wall_ns(wall_second), false, |buffer| {
            *buffer = AppsSecond {
                consecutive: true,
                elapsed_ns: Some(3),
                apps,
                ..AppsSecond::default()
            };
        });
        held.extend(span.held().map(Arc::clone));
    }
    drop(held);

    span.exchange();

    let free: Vec<&AppsSecond> = span.unheld_buffers().collect();
    let roomy = free
        .iter()
        .filter(|buffer| buffer.apps.capacity() > 0)
        .count();
    assert!(roomy > 0);
    for buffer in free {
        assert_eq!(buffer.apps.reserved_bytes(), 0);
        if buffer.apps.capacity() == 0 {
            assert_eq!(*buffer, AppsSecond::default());
            continue;
        }
        assert_eq!(buffer.apps.capacity(), 8);
        let cleared = AppsSecond {
            consecutive: true,
            apps: AppRows::with_capacity(8),
            ..AppsSecond::default()
        };
        assert_eq!(*buffer, cleared);
    }
}

#[test]
fn a_packed_block_that_left_the_row_is_kept_until_nothing_else_holds_it() {
    // Goal: the store's thread frees no packed block that left the row: it keeps the block
    // after the last View that held it lets go, hands it over at the next exchange, and the
    // packing thread frees it.
    // Method: pack a block, hold it as a View would, take seconds until its last one leaves
    // the span, let go of the View, and check the block is alive until the packing loop's body
    // has answered what the exchange handed over.
    let mut span = span(3);
    for (mark, wall_second) in [(1, 0), (2, 1), (3, 30)] {
        take(&mut span, wall_second, mark);
    }
    span.pack_ready();
    let mut holder = AppsSpan::default();
    span.publish(&mut holder);
    let packed = Arc::downgrade(span.blocks().next().unwrap());
    assert_eq!(packed.upgrade().unwrap().form, Form::Packed);
    for (mark, wall_second) in [(4, 31), (5, 32)] {
        take(&mut span, wall_second, mark);
    }
    assert_eq!(span.blocks().len(), 1);

    drop(holder);
    assert!(packed.upgrade().is_some());
    span.exchange();
    assert!(packed.upgrade().is_some());

    assert_eq!(span.answer_handed(), 0);
    assert!(packed.upgrade().is_none());
}

#[test]
fn a_store_keeps_at_most_its_bound_of_departed_blocks_and_counts_those_it_lets_go() {
    // Goal: the store keeps no more than its bound of packed blocks that left the row while
    // something else holds them, lets go of the oldest past it, counts that in the View, and
    // hands over every one it kept once nothing else holds it.
    // Method: pack one block of one second every half minute in a span of two seconds, hold
    // each packed block as it leaves, one more than the bound, then let go of them all and
    // pack until nothing is out.
    let mut span = span(2);
    take(&mut span, 0, 0);
    take(&mut span, 30, 1);
    span.pack_ready();
    let mut holder = Vec::with_capacity(DEPARTED_KEPT_MAX + 1);
    let departed_count = u64::try_from(DEPARTED_KEPT_MAX).unwrap() + 1;
    for index in 2..2 + departed_count {
        let oldest = span.blocks().next().unwrap();
        assert_eq!(oldest.form, Form::Packed);
        holder.push(Arc::clone(oldest));
        take(&mut span, 30 * index, index);
        span.pack_ready();
    }
    let mut view = AppsSpan::default();
    span.publish(&mut view);

    assert_eq!(view.departed_let_go_count(), 1);
    assert_eq!(span.departed_kept_count(), DEPARTED_KEPT_MAX);
    drop(holder);
    span.pack_ready();
    assert_eq!(span.departed_kept_count(), 0);
}

#[test]
fn a_block_the_budget_refuses_is_kept_plain_counted_by_its_cause_and_read_as_before() {
    // Goal: a closed block the budget has no room to pack stays plain for good, charges
    // nothing, the span counts it by its cause, and its seconds read as they did.
    // Method: take a generated block into a span that packs within the budget its rows are
    // charged to, close it, take every byte the budget has left, pack twice, and compare the
    // block's seconds with the ones taken.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let capacity = BLOCK_SECONDS_MAX + 1;
    let buffers = Buffers::new(2 * (capacity + BLOCK_DEPARTED_MAX), AppsSecond::default);
    let mut span = BlockSpan::keeping_packing_end(capacity, buffers, Arc::clone(&budget));
    for (offset, second) in (0..).zip(generated_block(3, &budget)) {
        let second = Arc::try_unwrap(second).unwrap();
        span.push(WALL_NS + offset * NS / 4, false, |buffer| *buffer = second);
    }
    let taken: Vec<Arc<AppsSecond>> = span.held().map(Arc::clone).collect();
    take(&mut span, 30, 0);
    let rest = budget.reserve(budget.maximum() - budget.used()).unwrap();
    let used = budget.used();

    span.pack_ready();

    assert_kept_plain(&span, KeptPlain::Budget, &taken);
    assert_eq!(budget.used(), used);
    drop(rest);
}

#[test]
fn packing_a_block_charges_exactly_what_the_packed_block_allocates() {
    // Goal: a packed block is charged, before it replaces anything, at the exact size of what
    // packing allocated: the process table, each second with its App rows and its payload, and
    // the block with its list of seconds.
    // Method: pack a generated block, take the budget's growth across the packing, and compare
    // it with the bytes summed from the packed block's parts.
    let budget = Arc::new(ByteBudget::new(BUDGET_BYTES));
    let plain = generated_block(3, &budget);
    let mut block = Block::starting(WALL_NS, Arc::clone(&plain[0]), false);
    for (offset, second) in (1..).zip(&plain[1..]) {
        let wall_ns = WALL_NS + offset * NS / 4;
        block = block.joining(wall_ns, Arc::clone(second), false).unwrap();
    }
    let used = budget.used();

    let packed = block.packing(None, &budget);

    assert_eq!(packed.form, Form::Packed);
    assert_eq!(budget.used() - used, packed_block_bytes(&packed));
}

/// The bytes of what packing allocated for `block`, summed from its parts: the block in its
/// `Arc` with its list of seconds, each second in its `Arc` with the reservation its header
/// holds, the strings it copied, its App rows and its payload, and the process table once.
fn packed_block_bytes(block: &Block) -> usize {
    let counts_bytes = 2 * size_of::<usize>();
    let mut bytes =
        counts_bytes + size_of::<Block>() + block.seconds.capacity() * size_of::<Arc<AppsSecond>>();
    for second in &block.seconds {
        bytes += counts_bytes
            + size_of::<AppsSecond>()
            + counts_bytes
            + size_of::<crate::byte_budget::ByteReservation>()
            + second.apps.reserved_bytes();
        for missing in [second.missing.as_ref(), second.processes.as_ref().err()]
            .into_iter()
            .flatten()
        {
            if let Missing::Refused(reason) = missing {
                bytes += reason.capacity();
            }
        }
        if let Ok(payload) = &second.processes {
            bytes += payload.bytes();
        }
    }

    bytes + block.table().unwrap().reserved_bytes()
}

/// Checks that the oldest block of `span` is kept plain for `cause` and holds the seconds
/// `taken`, and that the span counts it and no other.
fn assert_kept_plain(span: &BlockSpan, cause: KeptPlain, taken: &[Arc<AppsSecond>]) {
    let other = match cause {
        KeptPlain::Bound => KeptPlain::Budget,
        KeptPlain::Budget => KeptPlain::Bound,
    };
    assert_eq!(span.span().kept_plain_count(cause), 1);
    assert_eq!(span.span().kept_plain_count(other), 0);
    assert_eq!(span.span().packed_count(), 0);
    let oldest = span.blocks().next().unwrap();
    assert_eq!(oldest.form, Form::KeptPlain(cause));
    assert_eq!(oldest.len(), taken.len());
    for (index, taken) in taken.iter().enumerate() {
        assert!(Arc::ptr_eq(&oldest.seconds[index], taken), "second {index}");
        assert!(*oldest.second(index).stored() == **taken, "second {index}");
    }
}

/// `count` seconds of three processes, one of them coming and going, read through the real
/// producer.
fn launch_rows(budget: &Arc<ByteBudget>, count: u64) -> Vec<AppsSecond> {
    let ticks: Vec<Vec<Process>> = (0..count)
        .map(|tick| {
            let mut processes = vec![process(7, tick * 1_000_000), process(9, 4_000_000)];
            if tick % 7 < 3 {
                processes.push(process(11, tick * 2_000_000));
            }
            processes
        })
        .collect();

    seconds_of(budget, &ticks, |pid| match pid {
        9 => b"/Arrival.app",
        _ => b"/Owner.app",
    })
}

/// Gives `second` the identity of `of`, the same tick read through another fixture, which
/// names its recording and run anew.
fn identity_of(second: &mut AppsSecond, of: &AppsSecond) {
    second.observation = of.observation;
    if let (Ok(payload), Ok(of)) = (&mut second.processes, &of.processes) {
        let payload = Arc::get_mut(payload).unwrap();
        payload.selected = of.selected;
        payload.previous = of.previous;
    }
}

/// How many seconds of the plain blocks of `span` hold process rows.
fn plain_with_rows_count(span: &BlockSpan) -> usize {
    span.blocks()
        .filter(|block| block.form != Form::Packed)
        .flat_map(|block| block.seconds.iter())
        .filter(|second| {
            second
                .processes
                .as_ref()
                .is_ok_and(|payload| !payload.is_empty())
        })
        .count()
}

/// The wall time `wall_second` seconds past `WALL_NS`.
fn wall_ns(wall_second: u64) -> u64 {
    WALL_NS + wall_second * NS
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

/// One block holding `seconds`, each a wall second after the one before.
fn block_of(seconds: &[Arc<AppsSecond>]) -> Block {
    let mut block = Block::starting(WALL_NS, Arc::clone(&seconds[0]), false);
    for (offset, second) in (1..).zip(&seconds[1..]) {
        block = block
            .joining(WALL_NS + offset * 1_000_000_000, Arc::clone(second), false)
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
    BlockSpan::keeping_packing_end(
        capacity,
        Buffers::new(count, AppsSecond::default),
        Arc::new(ByteBudget::new(BUDGET_BYTES)),
    )
}

/// Takes into `span` a second at `wall_second` past `WALL_NS`, marked `mark`.
fn take(span: &mut BlockSpan, wall_second: u64, mark: u64) {
    span.push(WALL_NS + wall_second * 1_000_000_000, false, |buffer| {
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
