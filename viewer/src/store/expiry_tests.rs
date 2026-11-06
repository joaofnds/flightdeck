//! What a store hands over once the retention period has passed some of its readings.
use std::sync::Arc;

use super::tests::view_where;
use super::{Held, MachineSecond, Mailbox, Missing, Store, View};
use crate::live::identity_fixture::Fixture;
use crate::session::retention_tests::{
    expire_and_remove, expire_before, expire_boot_before, saved_across_nine_minutes,
    saved_across_three_minutes, saved_seconds,
};
use collector::ring::{Identity, RunIdentity};
use persistence::resources::ByteBudget;

const NS: u64 = 1_000_000_000;

fn saved_across_three_minutes_for_store() -> Fixture {
    saved_across_three_minutes(&persistence::sealed_apps::SealedApps::default())
}

fn wall_seconds(view: &View) -> Vec<u64> {
    view.seconds
        .iter()
        .map(|second| second.wall_ns / NS)
        .collect()
}

/// The wall second of the tick each second's Apps belong to.
fn apps_seconds(view: &View) -> Vec<u64> {
    view.apps
        .iter()
        .map(|apps| apps.observation.unwrap().wall_ns.unwrap() / NS)
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
    assert_eq!(apps_seconds(&view), [1006, 1007, 1008]);
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
    let mut held = Held::new(fixture.root.clone());

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
    let mut held = Held::with_payload(fixture.root.clone(), budget);

    let view = past_the_live_span_view(&fixture, &mut held);

    assert_eq!(view.earlier[0].wall_ns, 50 * NS);
    assert_eq!(view.sampled.len(), view.earlier.len());
}

#[test]
fn a_second_with_no_clock_stops_nothing_expiring_behind_it() {
    // Goal: a second with no clock has no place on the retention clock, so the seconds behind it
    // expire as any others do.
    // Method: take in a hundred seconds, the tenth with no clock, place the cutoff at the
    // forty-fifth, and read the View.
    let fixture = Fixture::new();
    let mut held = Held::new(fixture.root.clone());
    expire_boot_before(&fixture.root, RUN.boot, 45 * NS);
    for tick in 1..=100 {
        let second = MachineSecond {
            publication: tick,
            run_tick: tick,
            wall_ns: tick * NS,
            continuous_ns: (tick != 10).then_some(tick * NS),
            ..MachineSecond::default()
        };
        let apps = super::apps::TickApps::Missing(&Missing::Unread);
        assert!(held.take(&RUN, &second, apps));
    }

    let mailbox = Mailbox::new();
    held.publish(&mailbox);
    let view = mailbox.take().unwrap();

    assert_eq!(view.seconds[0].wall_ns, 45 * NS);
    assert_eq!(view.apps.len(), view.seconds.len());
}

#[test]
fn a_store_reads_no_minute_wholly_past_the_period() {
    // Goal: a minute with no reading to show is not read, so removal can take it at any moment
    // from under a store that has just listed it, and no refusal follows.
    // Method: take the oldest of nine minutes offline, place the cutoff past the first three, and
    // start a store, which refuses a minute it reads and finds offline.
    let fixture = saved_across_nine_minutes();
    let manifest = persistence::read_committed(&fixture.root, &fixture.marker(0)).unwrap();
    let ticks = fixture
        .root
        .join(manifest.data_directory)
        .join("ticks.parquet");
    std::fs::remove_file(&ticks).unwrap();
    std::fs::write(&ticks, b"offline").unwrap();
    let boot = fixture.writer.identity().unwrap().boot;
    expire_boot_before(&fixture.root, boot, 180 * NS);

    let store = Store::start(&fixture.root).unwrap();
    let view = view_where(&store, |view| !view.seconds.is_empty());

    assert_eq!(view.left_out, None);
    assert_eq!(view.seconds[0].wall_ns, 2_003 * NS);
}

#[test]
fn a_store_keeps_the_minute_before_its_first_covered_reading_for_the_rates_it_lends() {
    // Goal: the first second a store shows has the rates the last of a hidden minute gives, as it
    // would with that minute still in the period, on the route that takes in a whole hour.
    // Method: load the hour of a recording before and after placing the cutoff on a minute's first
    // reading, and compare what each load takes in for that second.
    let fixture = saved_across_three_minutes_for_store();
    let whole = hour_of_seconds(&fixture.root);
    let boot = fixture.writer.identity().unwrap().boot;
    expire_boot_before(&fixture.root, boot, 60 * NS);

    let retained = hour_of_seconds(&fixture.root);

    assert!(whole[1].1.is_some(), "{whole:?}");
    assert_eq!(retained, whole);
}

#[test]
fn a_store_goes_on_through_a_removal_pass() {
    // Goal: a store running beside removal keeps every second the period covers, and takes in
    // minutes saved after the pass.
    // Method: start a store on three minutes, take a pass that removes the first, save one more
    // minute, and wait for its second to reach a View.
    let mut fixture = saved_across_three_minutes_for_store();
    let store = Store::start(&fixture.root).unwrap();
    view_where(&store, |view| view.seconds.len() == 5);

    let removed = expire_and_remove(&fixture.root, 121 * NS);
    fixture.skip_run_ticks(58);
    fixture.publish_with_system_rows(1006);
    fixture.seal_minute_derived(3, &persistence::sealed_apps::SealedApps::default());
    let view = view_where(&store, |view| {
        view.seconds
            .last()
            .is_some_and(|second| second.wall_ns == 1006 * NS)
    });

    assert_eq!(removed, 1);
    assert_eq!(view.left_out, None);
    assert_eq!(view.failure, None);
    assert_eq!(view.seconds.first().unwrap().wall_ns, 1005 * NS);
}

/// The wall seconds and CPU rates a load of the whole hour takes in, oldest first.
fn hour_of_seconds(root: &std::path::Path) -> Vec<(u64, Option<f64>)> {
    let mut buffers = super::recording::LoadBuffers::default();
    let mut seconds = Vec::new();
    let left_out = super::recording::load_hour(root, None, &mut buffers, |_, _, second, _| {
        seconds.push((second.wall_ns / NS, second.cpu_percent));
    });
    assert_eq!(left_out, None);

    seconds
}

const RUN: RunIdentity = RunIdentity {
    recording: Identity([1, 1]),
    run: Identity([2, 2]),
    boot: Identity([3, 3]),
};

/// The View of `held` once it took in 1,300 seconds, one a second on both clocks, of a recording
/// whose cutoff lies at 45 seconds.
fn past_the_live_span_view(fixture: &Fixture, held: &mut Held) -> Arc<View> {
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
