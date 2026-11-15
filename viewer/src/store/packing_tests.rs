use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use collector::processes::Process;

use crate::live::identity_fixture::Fixture;
use crate::store::block::AppsSpan;
use crate::store::payload::row_tests::{process, seconds_of};
use crate::store::query::{CheckedCoverage, Queries};
use crate::store::{
    ByteBudget, Held, Mailbox, Publication, QueryOutcome, QueryRequest, QuerySelection,
    SourceGeneration, View,
};

#[test]
fn the_store_thread_packs_nothing_while_the_packing_thread_holds_back() {
    // Goal: no take, step or query waits for a block to be packed, since the packing thread
    // packs it. Method: keep the packing thread's end without running it, take seconds that
    // close two blocks, serve a query and step, and check every block is still plain; then
    // run the packing loop's body, step again and check the store holds both blocks packed.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let (mut held, budget) = load(&fixture);
    for (publication, second) in (2..).zip(101..=161) {
        take(&mut held, &fixture, publication, second);
    }
    let queries = Queries::new();
    let handle = queries.handle(std::thread::current());
    handle.submit(request()).unwrap();
    queries.serve(&held, Some(&coverage(&fixture)), &budget);
    let mut ring = crate::store::make_ring(&fixture.root, &held.payload_budget);
    let mailbox = Mailbox::new();

    let view = step(&mut held, &mut ring, &mailbox);

    let outcome = handle.take().unwrap().outcome;
    assert!(matches!(outcome, QueryOutcome::Ready(_)), "{outcome:?}");
    assert_eq!(view.apps.block_count(), 3);
    assert_eq!(view.apps.packed_count(), 0);
    assert_eq!(held.apps.span().packed_count(), 0);
    assert_eq!(held.apps.answer_handed(), 2);
    drop(step(&mut held, &mut ring, &mailbox));
    assert_eq!(held.apps.span().packed_count(), 2);
}

#[test]
fn the_store_thread_frees_no_rows_of_a_packed_block_until_the_packing_thread_drops_them() {
    // Goal: neither releasing the Views nor the exchange frees the rows of a plain block that
    // a packed one replaced on the store's thread, which hands them to the packing thread.
    // Method: close a block of two seconds read through the real producer, hold it as a View
    // would, pack it, let go of it, release the Views and exchange, check no free buffer holds
    // a process row, an App row, an App detail or a name, and that the rows of the block's
    // first second stay alive until the packing loop's body has answered what was handed.
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let mut held = Held::keeping_packing_end(std::path::PathBuf::new(), Arc::clone(&budget));
    let ticks: Vec<Vec<Process>> = (0..3).map(|tick| vec![process(7, tick)]).collect();
    let seconds = seconds_of(&budget, &ticks, |_| b"/Owner.app");
    for (wall_second, mut second) in [1_000, 1_001, 1_030].into_iter().zip(seconds) {
        held.apps
            .push(wall_second * 1_000_000_000, false, |buffer| {
                std::mem::swap(buffer, &mut second);
            });
    }
    let mut holder = AppsSpan::default();
    held.apps.publish(&mut holder);
    let rows = Arc::downgrade(holder.stored(0).processes.as_ref().unwrap());
    assert_eq!(held.apps.answer_handed(), 1);
    held.apps.exchange();
    assert_eq!(held.apps.span().packed_count(), 1);
    drop(holder);

    held.release_views();
    assert!(rows.upgrade().is_some());
    held.apps.exchange();
    assert!(rows.upgrade().is_some());
    for buffer in held.apps.unheld_buffers() {
        assert!(buffer.processes.is_err());
        assert!(buffer.apps.as_slice().is_empty());
    }

    assert_eq!(held.apps.answer_handed(), 0);
    assert!(rows.upgrade().is_none());
}

#[test]
fn the_packing_thread_wakes_the_store_thread_with_each_answer() {
    // Goal: each answer wakes the store's thread, so a store parked for a hidden window acts
    // on it without waiting for a step.
    // Method: hand two batches over, run the packing loop's body, and count the wakes.
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let (store_end, mut packing_end) = super::pair(budget);
    let wakes = Arc::new(AtomicUsize::new(0));
    let counted = Arc::clone(&wakes);
    packing_end.wake_with(Box::new(move || {
        counted.fetch_add(1, Ordering::Relaxed);
    }));
    for _ in 0..2 {
        let batch = vec![crate::store::AppsSecond::default()];
        store_end.hand(super::Handed::Batch(super::Batch::Seconds(batch)));
    }

    assert_eq!(packing_end.answer_handed(), 0);

    assert_eq!(wakes.load(Ordering::Relaxed), 2);
}

#[test]
fn the_started_packing_thread_unparks_the_store_thread_with_its_answer() {
    // Goal: the packing thread, once started, unparks the store's thread with each answer, so
    // a store parked for a hidden window wakes as soon as the answer is there.
    // Method: start the packing thread for the test's thread, hand one batch over, park for at
    // most twenty seconds, and check the park ended well before that with the answer waiting.
    const PARK_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let (store_end, packing_end) = super::pair(budget);
    let packing = packing_end.start(std::thread::current()).unwrap();
    let parked = std::time::Instant::now();
    let batch = vec![crate::store::AppsSecond::default()];
    store_end.hand(super::Handed::Batch(super::Batch::Seconds(batch)));

    std::thread::park_timeout(PARK_TIMEOUT);

    let elapsed = parked.elapsed();
    assert!(elapsed < PARK_TIMEOUT / 2, "{elapsed:?}");
    assert!(matches!(store_end.answer(), Some(super::Answer::Freed)));
    drop(store_end);
    packing.join().unwrap();
}

#[test]
fn the_exchange_hands_over_at_most_two_batches_of_at_most_a_block_of_buffers() {
    // Goal: however many written buffers come free at once, the exchange hands over at most
    // two batches, each of at most a block's seconds of buffers, so the queue has room and the
    // packing thread frees a bounded batch at a time.
    // Method: take 300 seconds into a span of eight while holding every buffer the span holds,
    // let go of them all at once, exchange, and read what the queue holds; then answer it,
    // exchange again and check a batch more comes, so more than two batches' buffers were free.
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let (store_end, mut packing_end) = super::pair(budget);
    let buffers = crate::store::buffers::Buffers::new(400, crate::store::AppsSecond::default);
    let mut span = crate::store::block::BlockSpan::new(8, buffers, store_end);
    let mut held = Vec::new();
    for wall_second in 1_000..1_300 {
        span.push(wall_second * 1_000_000_000, false, |buffer| {
            *buffer = crate::store::AppsSecond::default();
        });
        held.extend(span.held().map(Arc::clone));
    }
    drop(held);

    span.exchange();

    let handed: Vec<super::Handed> = packing_end.handed.try_iter().collect();
    assert_eq!(
        batch_sizes(&handed),
        [super::BATCH_BUFFERS_MAX; super::BATCHES_OUT_MAX]
    );
    for handed in handed {
        assert!(packing_end.start_loop_turn(handed));
    }
    span.exchange();
    let handed: Vec<super::Handed> = packing_end.handed.try_iter().collect();
    assert!(!batch_sizes(&handed).is_empty());
}

#[cfg(feature = "read-benchmark")]
#[test]
fn the_tally_counts_each_batch_the_packing_thread_frees() {
    // Goal: the read benchmark's tally counts every batch the packing thread frees, with the time
    // freeing took, so the benchmark can report the freeing a second.
    // Method: hand two batches of seconds over, run the packing loop's body, and read the tally.
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let (store_end, mut packing_end) = super::pair(budget);
    for _ in 0..2 {
        let batch = vec![crate::store::AppsSecond::default()];
        store_end.hand(super::Handed::Batch(super::Batch::Seconds(batch)));
    }

    assert_eq!(packing_end.answer_handed(), 0);

    assert_eq!(store_end.tally().freeing_times().batch_count, 2);
}

#[test]
fn a_parked_store_exchanges_until_every_ready_block_is_answered() {
    // Goal: a store parked for a hidden window goes on exchanging as each answer wakes it,
    // until every ready block is packed or kept plain and nothing waits to be freed, without a
    // step.
    // Method: take seconds that close three blocks, hide the window, then run the store
    // thread's turn and the packing loop's body by turns, as each answer would wake it, and
    // check what is left.
    let operation = Arc::new(crate::cancellation::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut waited = Ok(());

    parked_store_settles(|root, held, ring, shared| {
        let flow = crate::store::run_turn(root, held, ring, shared, &operation, &mut waited);
        assert!(flow.is_continue());
    });
}

#[test]
fn a_store_parked_while_it_starts_exchanges_until_every_ready_block_is_answered() {
    // Goal: a store parked for a hidden window while it starts goes on exchanging as each
    // answer wakes it, as a started store does.
    // Method: as for a started store, with the staged launch's turn in place of the store
    // thread's.
    let operation = Arc::new(crate::cancellation::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut startup = crate::store::recording::staged::Startup::new();
    let mut blocked = None;

    parked_store_settles(|root, held, ring, shared| {
        let input = crate::store::recording::staged::Input {
            root,
            budget: &Arc::clone(&held.payload_budget),
            operation: &operation,
        };
        let turn = crate::store::startup_staged_turn(
            &input,
            held,
            ring,
            shared,
            &mut startup,
            &mut blocked,
        );
        assert!(!turn.unwrap());
    });
}

#[test]
fn the_packing_thread_packs_what_the_store_hands_over_until_the_store_is_gone() {
    // Goal: the packing thread, on a thread of its own, answers each block the store's thread
    // hands it, wakes that thread with each answer, and ends once the store's end is gone.
    // Method: start the packing thread for a span on the test's thread, take seconds that close
    // three blocks, exchange each time the test's thread wakes, within a bound, until the span
    // has settled, check every closed block is packed, then let go of the span and join.
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let (store_end, packing_end) = super::pair(Arc::clone(&budget));
    let packing = packing_end.start(std::thread::current()).unwrap();
    let buffers = crate::store::buffers::Buffers::new(256, crate::store::AppsSecond::default);
    let mut span = crate::store::block::BlockSpan::new(64, buffers, store_end);
    let wall_seconds = [1_000, 1_001, 1_030, 1_031, 1_060, 1_061, 1_090];
    let ticks: Vec<Vec<Process>> = (0..7).map(|tick| vec![process(7, tick)]).collect();
    let seconds = seconds_of(&budget, &ticks, |_| b"/Owner.app");
    for (wall_second, mut second) in wall_seconds.into_iter().zip(seconds) {
        span.push(wall_second * 1_000_000_000, false, |buffer| {
            std::mem::swap(buffer, &mut second);
        });
    }

    span.settle();

    assert!(span.settled());
    assert_eq!(span.span().packed_count(), 3);
    drop(span);
    packing.join().unwrap();
}

#[test]
fn settling_leaves_no_ready_block_plain_after_a_continued_load_and_a_launch() {
    // Goal: the benchmark's wait, after a continued load and after a launch, leaves no ready
    // block plain, none out and nothing to be freed, so the span's bytes are the same in two
    // runs and figure 9 stays an exact count.
    // Method: in each of two runs, take seconds ten wall seconds apart, which close a block every
    // three, into a span before its packing thread starts, so no answer is acted on between,
    // as in a continued load, check more blocks wait than can be out, start the thread, settle and read the
    // budget; then take seconds that wait for their Apps, install them with the launch's wait
    // before each half minute, end the launch, settle and read it again.
    let mut bytes_of_runs = Vec::with_capacity(2);
    for _ in 0..2 {
        let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
        let (store_end, packing_end) = super::pair(Arc::clone(&budget));
        let buffers = crate::store::buffers::Buffers::new(256, crate::store::AppsSecond::default);
        let mut span = crate::store::block::BlockSpan::new(64, buffers, store_end);
        let ticks: Vec<Vec<Process>> = (0..100).map(|tick| vec![process(7, tick)]).collect();
        let seconds = seconds_of(&budget, &ticks, |_| b"/Owner.app");
        for (wall_second, mut second) in (1_000..).step_by(10).zip(seconds) {
            span.push(wall_second * 1_000_000_000, false, |buffer| {
                std::mem::swap(buffer, &mut second);
            });
        }
        assert!(span.ready_plain_count() > super::BLOCKS_OUT_MAX);
        let packing = packing_end.start(std::thread::current()).unwrap();
        span.settle();
        let continued_bytes = budget.used();

        let mut rows = seconds_of(&budget, &ticks, |_| b"/Owner.app");
        for wall_second in 2_000..2_100 {
            span.push(wall_second * 1_000_000_000, true, |buffer| {
                *buffer = crate::store::AppsSecond::default();
            });
        }
        let left = rows.len() - span.span().len();
        for (index, second) in rows[left..].iter_mut().enumerate() {
            if index % 30 == 0 {
                span.launch_wait();
            }
            span.replace(index, |buffer| std::mem::swap(buffer, second));
        }
        drop(rows);
        span.end_awaiting();
        span.settle();
        assert!(span.span().packed_count() > 0);
        bytes_of_runs.push((continued_bytes, budget.used()));
        drop(span);
        packing.join().unwrap();
    }

    assert_eq!(bytes_of_runs[0], bytes_of_runs[1]);
}

/// Takes seconds that close three blocks, hides the window, and runs `turn`, the turn of the
/// store's thread that parks, and the packing loop's body by turns, as each answer would wake
/// it, then checks no ready block is plain and nothing is out or waits to be freed.
fn parked_store_settles(
    mut turn: impl FnMut(&std::path::Path, &mut Held, &mut crate::store::Ring, &crate::store::Shared),
) {
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal();
    let (mut held, _) = load(&fixture);
    for (publication, second) in (2..).zip(101..=191) {
        take(&mut held, &fixture, publication, second);
    }
    let mut ring = crate::store::make_ring(&fixture.root, &held.payload_budget);
    let shared = crate::store::Shared::starting();
    shared.active.store(false, Ordering::Release);
    assert!(held.apps.ready_plain_count() > 2);

    for _ in 0..held.apps.span().block_count() + 8 {
        // The turn's park consumes this token, as an answer's wake would.
        std::thread::current().unpark();
        turn(&fixture.root, &mut held, &mut ring, &shared);
        held.apps.answer_handed();
    }

    assert_eq!(held.apps.ready_plain_count(), 0);
    assert!(held.apps.settled());
}

/// The size of each batch of seconds in `handed`, in the order handed.
fn batch_sizes(handed: &[super::Handed]) -> Vec<usize> {
    handed
        .iter()
        .filter_map(|handed| match handed {
            super::Handed::Batch(super::Batch::Seconds(batch)) => Some(batch.len()),
            _ => None,
        })
        .collect()
}

/// Steps the store as its thread does after a take, and returns the View it published.
fn step(held: &mut Held, ring: &mut crate::store::Ring, mailbox: &Mailbox<View>) -> Arc<View> {
    held.unpublished = true;
    let operation = Arc::new(crate::cancellation::Cancellation::default())
        .operation(0)
        .unwrap();
    crate::store::step(
        held,
        ring,
        Ok(()),
        mailbox,
        &operation,
        Publication::Progress,
    )
    .unwrap();

    mailbox.take().unwrap()
}

/// Takes in the second at `second` of the fixture's run as its publication `publication`.
fn take(held: &mut Held, fixture: &Fixture, publication: u64, second: u64) {
    let machine = crate::store::MachineSecond {
        publication,
        run_tick: publication - 1,
        wall_ns: second * 1_000_000_000,
        ..crate::store::MachineSecond::default()
    };
    assert!(held.take(
        &fixture.writer.identity().unwrap(),
        &machine,
        crate::store::apps::TickApps::Missing(&crate::store::Missing::Unread)
    ));
}

/// The store's state once it has loaded the fixture's sealed seconds, and its budget.
fn load(fixture: &Fixture) -> (Held, Arc<ByteBudget>) {
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let operation = Arc::new(crate::cancellation::Cancellation::default())
        .operation(0)
        .unwrap();
    let mut held = Held::without_recording_with_payload(Arc::clone(&budget));
    crate::store::recording::load_payload(
        &fixture.root,
        &budget,
        &operation,
        |run, second, apps| assert!(held.take(run, second, apps)),
    )
    .unwrap();
    held.ring_complete = true;

    (held, budget)
}

fn coverage(fixture: &Fixture) -> CheckedCoverage {
    CheckedCoverage::new(
        SourceGeneration {
            recording: fixture.writer.identity().unwrap().recording,
            generation: 0,
        },
        crate::session::ContextRange {
            first_ns: 0,
            end_ns: 10_000_000_000_000,
        },
    )
}

fn request() -> QueryRequest {
    QueryRequest {
        epoch: 7,
        source: None,
        selection: QuerySelection::FollowLatest,
    }
}
