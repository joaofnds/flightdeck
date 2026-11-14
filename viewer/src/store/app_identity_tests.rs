//! The charge of each App name a second holds, which lives in the name, so whatever holds the
//! name, a row, a later second or a returned rank, keeps it charged until the last one drops.

use std::sync::Arc;

use super::AppsSecond;
use crate::apps::Stamp;
use crate::live_app_response::{App, Key, KeyKind, Measure, Observation, RankedApp, Sample};
use crate::store::{AppRow, ByteBudget, Missing};

const NS: u64 = 1_000_000_000;

#[test]
fn an_app_name_keeps_its_charge_after_the_second_that_named_it_drops() {
    // Goal: a name held past its second cannot return its charge while its bytes still live.
    // Method: fill a second under a budget, keep one name, drop the second, then the name.
    let budget = Arc::new(ByteBudget::new(1024 * 1024));
    let second = filled(&sample(&[b"/A.app"]), None, &budget);
    let name = Arc::clone(&second.apps[0].name);
    let backing = name.bytes.capacity() + name.display_name.capacity();
    assert!(backing > 0);
    assert_eq!(budget.used(), name.reserved_bytes());

    drop(second);

    assert_eq!(name.bytes, b"/A.app");
    assert_eq!(budget.used(), name.reserved_bytes());
    // The charge counts the Arc's two counters, so a name's whole allocation is charged.
    assert!(name.reserved_bytes() >= 2 * size_of::<usize>() + size_of::<RankedApp>() + backing);
    drop(name);
    assert_eq!(budget.used(), 0);
}

#[test]
fn ranks_keep_their_app_names_charged_after_the_second_drops() {
    // Goal: a returned rank cannot outlive the charge of the name it shows.
    // Method: rank a filled second, check each rank shares the row's name, drop the second,
    // then the ranks.
    let budget = Arc::new(ByteBudget::new(1024 * 1024));
    let second = filled(&sample(&[b"/A.app"]), None, &budget);
    let ranks = second.ranks().unwrap();
    let charge = second.apps[0].name.reserved_bytes();
    assert!(charge > 0);
    for (metric_index, metric) in ranks.metrics[..2].iter().enumerate() {
        assert!(
            Arc::ptr_eq(&metric.entries[0].app, &second.apps[0].name),
            "metric {metric_index}"
        );
    }

    drop(second);

    assert_eq!(budget.used(), charge);
    assert_eq!(ranks.metrics[0].entries[0].app.bytes, b"/A.app");
    drop(ranks);
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_later_second_shares_each_name_and_its_one_charge() {
    // Goal: seconds that name one App hold one copy of its name, charged once, while each
    // second keeps its own rows.
    // Method: fill a second after another that measured the App differently, compare names and
    // charge, compare each row with the row its own sample makes, then drop the first.
    let budget = Arc::new(ByteBudget::new(1024 * 1024));
    let first = filled(&sample(&[b"/A.app"]), None, &budget);
    let charge = first.apps[0].name.reserved_bytes();
    let later = filled(&sample_measuring(&[b"/A.app"], 5), Some(&first), &budget);
    assert!(Arc::ptr_eq(&first.apps[0].name, &later.apps[0].name));
    assert_eq!(budget.used(), charge);
    let name = Arc::clone(&first.apps[0].name);
    assert_eq!(
        first.apps.as_slice(),
        [AppRow::new(&app(b"/A.app", 1), Arc::clone(&name))]
    );
    assert_eq!(
        later.apps.as_slice(),
        [AppRow::new(&app(b"/A.app", 5), Arc::clone(&name))]
    );
    drop(name);

    drop(first);

    assert_eq!(budget.used(), charge);
    assert_eq!(later.apps[0].name.bytes, b"/A.app");
    drop(later);
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_refused_name_releases_the_names_admitted_before_it_and_a_retry_admits_them() {
    // Goal: a second the budget refuses cannot keep the charge of names it admitted first.
    // Method: leave room for one of two names, fill, free the room and fill again.
    let budget = Arc::new(ByteBudget::new(1024 * 1024));
    let both = sample(&[b"/A.app", b"/B.app"]);
    let admitted = filled(&both, None, &budget);
    let required = budget.used();
    let first = admitted.apps[0].name.reserved_bytes();
    assert!(first < required);
    drop(admitted);
    assert_eq!(budget.used(), 0);
    let pressure = budget.reserve(budget.maximum() - required + 1).unwrap();
    assert!(budget.maximum() - budget.used() >= first);

    let refused = filled(&both, None, &budget);

    assert_eq!(refused.missing, Some(Missing::Budget));
    assert!(refused.apps.is_empty());
    assert_eq!(budget.used(), pressure.bytes());
    drop((refused, pressure));
    let retried = filled(&both, None, &budget);
    assert!(retried.missing.is_none());
    assert_eq!(budget.used(), required);
    drop(retried);
    assert_eq!(budget.used(), 0);
}

/// The second `AppsSecond::fill` makes of `sample` under `budget`, after `previous`.
fn filled(sample: &Sample, previous: Option<&AppsSecond>, budget: &Arc<ByteBudget>) -> AppsSecond {
    let mut second = AppsSecond::with_room();
    second.fill(sample, previous, budget);
    second
}

/// A sample that measured one second and names the bundles `keys`, which ascend, each measured
/// by its one member.
fn sample(keys: &[&[u8]]) -> Sample {
    sample_measuring(keys, 1)
}

/// A sample as `sample` makes it, whose first App measured `amount_first` of every resource,
/// each App after it one more than the App before.
fn sample_measuring(keys: &[&[u8]], amount_first: u64) -> Sample {
    let apps = keys
        .iter()
        .zip(amount_first..)
        .map(|(key, amount)| app(key, amount))
        .collect();
    let selected = Observation {
        recording: [1, 2],
        stamp: Stamp {
            boot: [3, 4],
            run: [5, 6],
            publication: 2,
            continuous_ns: Some(2 * NS),
            awake_ns: Some(2 * NS),
        },
        run_tick: 1,
        wall_ns: Some(1001 * NS),
    };
    Sample::of_apps(selected, Some(NS), apps)
}

/// The bundle `key`, whose one member measured `amount` of every resource.
fn app(key: &[u8], amount: u64) -> App {
    let measure = Measure {
        known: amount,
        measured_members: 1,
        unavailable_members: 0,
        overflowed: false,
    };
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
