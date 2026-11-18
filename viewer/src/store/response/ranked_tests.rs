//! Tests that a reading ranks only the seconds the readings before did not rank, shows what a
//! reading from nothing shows, and holds no second its window left.
use std::sync::Arc;

use super::RankedSeconds;
use super::tests::{assert_window_ranks, context_of, key};
use crate::ObservationKey;
use crate::live::identity_fixture::Fixture;
use crate::store::apps::TickApps;
use crate::store::block::BLOCK_WALL_NS;
use crate::store::ring::Ring;
use crate::store::{ByteBudget, Held, Missing, QueryContext};

const NS: u64 = 1_000_000_000;
/// A wall second of the half minute before the one `NEWER_BLOCK_SECOND` opens, so the store
/// holds the two in two blocks.
const OLDER_BLOCK_SECOND: u64 = 100;
/// The first wall second of a half minute, so the second after it joins its block.
const NEWER_BLOCK_SECOND: u64 = 120;
const _: () = {
    assert!(OLDER_BLOCK_SECOND * NS / BLOCK_WALL_NS < NEWER_BLOCK_SECOND * NS / BLOCK_WALL_NS);
    assert!((NEWER_BLOCK_SECOND * NS).is_multiple_of(BLOCK_WALL_NS));
    assert!(BLOCK_WALL_NS >= 3 * NS);
};

/// A recording a collector writes to and the store that follows it, read once for each
/// reading a test takes.
struct Followed {
    fixture: Fixture,
    ring: Ring,
    held: Held,
    budget: Arc<ByteBudget>,
    /// The publication whose Apps the store takes as refused.
    refused_publication: Option<u64>,
}

impl Followed {
    fn new() -> Self {
        let fixture = Fixture::new();
        let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
        let mut held = Held::without_recording_with_payload(Arc::clone(&budget));
        held.ring_complete = true;
        let ring = Ring::new(fixture.root.clone());

        Self {
            fixture,
            ring,
            held,
            budget,
            refused_publication: None,
        }
    }

    /// Publishes the wall second `second` and has the store take it.
    fn publish(&mut self, second: u64) {
        self.fixture.publish_with_system_rows(second);
        self.ring.follow().unwrap();
        let refused = Missing::Refused(String::from("fixture App refusal"));
        self.ring
            .read(|run, machine, apps| {
                if Some(machine.publication) == self.refused_publication {
                    self.held.take(run, machine, TickApps::Missing(&refused))
                } else {
                    self.held.take(run, machine, apps)
                }
            })
            .unwrap();
    }

    fn context(&self) -> Arc<QueryContext> {
        context_of(&self.fixture, &self.held, &self.budget)
    }
}

/// The key of the observation `context` holds at the wall second `second`.
fn key_at(context: &QueryContext, second: u64) -> ObservationKey {
    let held = context
        .seconds()
        .iter()
        .find(|held| held.observation.wall_ns.map(|wall_ns| wall_ns / NS) == Some(second))
        .unwrap();

    key(&held.observation)
}

#[test]
fn a_second_the_reading_before_ranked_is_not_ranked_again() {
    // Goal: a reading does not rank again a second the reading before ranked, where the store
    // still holds the second in the same block.
    // Method: read a store twice, a second apart, and compare where the two windows hold the
    // ranks of a second of the older block.
    let mut store = Followed::new();
    store.publish(OLDER_BLOCK_SECOND);
    store.publish(NEWER_BLOCK_SECOND);
    let mut ranked = RankedSeconds::default();
    let first = store.context().app_ranks(&mut ranked).unwrap();
    store.publish(NEWER_BLOCK_SECOND + 1);
    let context = store.context();
    let older = key_at(&context, OLDER_BLOCK_SECOND);

    let second = context.app_ranks(&mut ranked).unwrap();

    assert!(std::ptr::eq(
        first.get(&older).unwrap(),
        second.get(&older).unwrap()
    ));
}

#[test]
fn a_second_ranked_once_is_shared_by_every_reading_after() {
    // Goal: a reading hands on the ranks it took from the reading before as well as the ones it
    // ranked, so a second is not ranked again at every other reading.
    // Method: read a store three times, a second apart, and compare where the first and the
    // third windows hold the ranks of a second of the older block.
    let mut store = Followed::new();
    store.publish(OLDER_BLOCK_SECOND);
    store.publish(NEWER_BLOCK_SECOND);
    let mut ranked = RankedSeconds::default();
    let first = store.context().app_ranks(&mut ranked).unwrap();
    store.publish(NEWER_BLOCK_SECOND + 1);
    drop(store.context().app_ranks(&mut ranked).unwrap());
    store.publish(NEWER_BLOCK_SECOND + 2);
    let context = store.context();
    let older = key_at(&context, OLDER_BLOCK_SECOND);

    let third = context.app_ranks(&mut ranked).unwrap();

    assert!(std::ptr::eq(
        first.get(&older).unwrap(),
        third.get(&older).unwrap()
    ));
}

#[test]
fn a_reading_that_fails_leaves_the_ranks_of_the_reading_before() {
    // Goal: a reading the budget refuses costs the next reading nothing it had ranked.
    // Method: reserve every byte left between two readings, so the one between them fails, and
    // compare where the first and the last windows hold a second's ranks.
    let mut store = Followed::new();
    store.publish(OLDER_BLOCK_SECOND);
    let mut ranked = RankedSeconds::default();
    let context = store.context();
    let older = key_at(&context, OLDER_BLOCK_SECOND);
    let first = context.app_ranks(&mut ranked).unwrap();
    let budget = context.response_budget();
    let blocked = budget.reserve(budget.maximum() - budget.used()).unwrap();
    assert!(context.app_ranks(&mut ranked).is_err());
    drop(blocked);

    let last = context.app_ranks(&mut ranked).unwrap();

    assert!(std::ptr::eq(
        first.get(&older).unwrap(),
        last.get(&older).unwrap()
    ));
}

#[test]
fn a_second_whose_block_the_store_replaced_is_ranked_again() {
    // Goal: a reading ranks again a second whose block the store put another in place of, since
    // only a second still in its block is known to hold the Apps it held.
    // Method: read a store twice, a second apart, and compare where the two windows hold the
    // ranks of the second the newer second joined.
    let mut store = Followed::new();
    store.publish(OLDER_BLOCK_SECOND);
    store.publish(NEWER_BLOCK_SECOND);
    let mut ranked = RankedSeconds::default();
    let first = store.context().app_ranks(&mut ranked).unwrap();
    store.publish(NEWER_BLOCK_SECOND + 1);
    let context = store.context();
    let joined = key_at(&context, NEWER_BLOCK_SECOND);

    let second = context.app_ranks(&mut ranked).unwrap();

    assert!(!std::ptr::eq(
        first.get(&joined).unwrap(),
        second.get(&joined).unwrap()
    ));
    assert_eq!(first.get(&joined), second.get(&joined));
}

#[test]
fn a_reading_shows_what_a_reading_from_nothing_shows() {
    // Goal: the ranks a reading takes from the reading before change nothing a window shows or
    // charges, over seconds of two blocks, a second without Apps and a second the reading
    // before never saw.
    // Method: read a store twice, the second time with what the first ranked, and compare that
    // window with one a reading from nothing takes of the same context.
    let mut store = Followed::new();
    store.refused_publication = Some(2);
    store.publish(OLDER_BLOCK_SECOND);
    store.publish(OLDER_BLOCK_SECOND + 1);
    store.publish(NEWER_BLOCK_SECOND);
    let mut ranked = RankedSeconds::default();
    drop(store.context().app_ranks(&mut ranked).unwrap());
    store.publish(NEWER_BLOCK_SECOND + 1);
    let context = store.context();
    let expected = context.app_ranks(&mut RankedSeconds::default()).unwrap();

    let shown = context.app_ranks(&mut ranked).unwrap();

    assert_window_ranks(&context, &shown, &expected);
    assert_eq!(shown.held_values(), 3);
    assert_eq!(shown.bytes(), expected.bytes());
}

#[test]
fn a_reader_holds_no_ranks_of_a_second_its_window_left() {
    // Goal: the ranks a reader holds are those of its last window alone, so what it holds is
    // bounded by the window and not by how long it has read.
    // Method: read a store, then read it again once its window has passed the first second, and
    // see that nothing holds that second's ranks once the first window is dropped.
    let mut store = Followed::new();
    store.publish(OLDER_BLOCK_SECOND);
    let mut ranked = RankedSeconds::default();
    let context = store.context();
    let first = context.app_ranks(&mut ranked).unwrap();
    let left = Arc::downgrade(first.shared(&key_at(&context, OLDER_BLOCK_SECOND)).unwrap());
    drop(context);
    store.publish(OLDER_BLOCK_SECOND + 1_000);

    let second = store.context().app_ranks(&mut ranked).unwrap();

    drop(first);
    assert!(left.upgrade().is_none());
    assert_eq!(second.held_values(), 1);
}
