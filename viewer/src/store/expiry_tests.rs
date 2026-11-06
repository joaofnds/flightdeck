//! What a store hands over once the retention period has passed some of its readings.
use std::sync::Arc;

use super::tests::view_where;
use super::{Held, MachineSecond, Mailbox, Missing, Store, View};
use crate::live::identity_fixture::Fixture;
use crate::session::retention_tests::{expire_before, expire_boot_before, saved_seconds};
use collector::ring::{Identity, RunIdentity};
use persistence::resources::ByteBudget;

const NS: u64 = 1_000_000_000;

fn wall_seconds(view: &View) -> Vec<u64> {
    view.seconds
        .iter()
        .map(|second| second.wall_ns / NS)
        .collect()
}

#[test]
fn a_view_holds_no_second_the_period_no_longer_covers() {
    // Goal: a store started on a partly expired minute hands over only the seconds still covered,
    // with the Apps of those seconds and of no others.
    // Method: place the cutoff inside the saved minute, start a store, and read its first View.
    let fixture = saved_seconds();
    expire_before(&fixture.root, 5 * NS);

    let store = Store::start(&fixture.root).unwrap();
    let view = view_where(&store, |view| !view.seconds.is_empty());

    assert_eq!(wall_seconds(&view), [1006, 1007, 1008]);
    assert_eq!(view.apps.len(), view.seconds.len());
    assert_eq!(view.alternatives(1005), []);
    assert_eq!(view.alternatives(1006).len(), 1);
}

#[test]
fn a_running_store_lets_go_of_a_second_as_it_expires() {
    // Goal: a store with nothing new to take in still stops showing a second once it expires.
    // Method: start a store on a whole minute, move the cutoff into it, and wait for a View.
    let fixture = saved_seconds();
    let store = Store::start(&fixture.root).unwrap();
    let whole = view_where(&store, |view| view.seconds.len() == 8);

    expire_before(&fixture.root, 5 * NS);
    let view = view_where(&store, |view| view.seconds.len() < 8);

    assert_eq!(whole.seconds[0].wall_ns, 1001 * NS);
    assert_eq!(wall_seconds(&view), [1006, 1007, 1008]);
}

#[test]
fn a_store_lets_go_of_older_seconds_and_of_each_stretch_that_adds_up_an_expired_one() {
    // Goal: past the live span, an older second expires like any other, and a stretch goes as soon
    // as one of its seconds has expired, since its sums cannot give that second back.
    // Method: take in more seconds than the live span holds, one a second on both clocks, place
    // the cutoff inside a stretch, and read the View.
    let fixture = Fixture::new();
    let mut held = Held::new();

    let view = past_the_live_span_view(&fixture, &mut held);

    assert_eq!(view.earlier[0].wall_ns, 50 * NS);
    assert_eq!(view.stretches[0].stretch, 5);
    assert_eq!(view.seconds[0].wall_ns, 221 * NS);
}

#[test]
fn a_complete_store_lets_go_of_the_sample_beside_each_older_second() {
    // Goal: the samples a complete store keeps beside its older seconds expire with them, which
    // a View requires by holding one for each.
    // Method: as for a store that holds no samples, with a store that does.
    let fixture = Fixture::new();
    let budget = Arc::new(ByteBudget::new(256 * 1024 * 1024));
    let mut held = Held::with_payload(budget);

    let view = past_the_live_span_view(&fixture, &mut held);

    assert_eq!(view.earlier[0].wall_ns, 50 * NS);
    assert_eq!(view.sampled.len(), view.earlier.len());
}

/// The View of `held` once it took in 1,300 seconds, one a second on both clocks, of a recording
/// whose cutoff lies at 45 seconds.
fn past_the_live_span_view(fixture: &Fixture, held: &mut Held) -> Arc<View> {
    const RUN: RunIdentity = RunIdentity {
        recording: Identity([1, 1]),
        run: Identity([2, 2]),
        boot: Identity([3, 3]),
    };
    held.root = Some(fixture.root.clone());
    expire_boot_before(&fixture.root, RUN.boot, 45 * NS);
    for tick in 1..=1_300 {
        let second = MachineSecond {
            publication: tick,
            run_tick: tick,
            wall_ns: tick * NS,
            continuous_ns: Some(tick * NS),
            ..MachineSecond::default()
        };
        let apps = super::apps::TickApps::Missing(&Missing::Unread);
        assert!(held.take(&RUN, &second, apps));
    }

    let mailbox = Mailbox::new();
    held.publish(&mailbox);
    mailbox.take().unwrap()
}
