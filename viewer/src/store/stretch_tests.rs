use std::sync::Arc;

use super::apps::TickApps;
use super::{
    APPS_PER_SECOND_MAX, AppRow, AppRows, AppsSecond, Held, MachineSecond, Mailbox, Missing,
    STRETCHES_MAX, View,
};
use crate::AppAttributionMetric;
use crate::live_app_response::{App, Key, KeyKind, Measure, RankedApp};
use collector::ring::{Identity, RunIdentity};

const RUN: RunIdentity = RunIdentity {
    recording: Identity([1, 1]),
    run: Identity([2, 2]),
    boot: Identity([3, 3]),
};

const NANOSECONDS_PER_SECOND: u64 = 1_000_000_000;

#[test]
fn a_closed_stretch_adds_up_the_apps_of_its_seconds() {
    // Goal: an App chart's point of ten seconds shows what its App used over those seconds, as
    // the seconds themselves add up. Method: take in seconds 1,000 to 1,010, A using more CPU
    // each second, so 1,010 closes the stretch of 1,000 to 1,009, and compare that stretch's
    // reading with the readings of its ten seconds added up.
    let mut held = Held::new();
    for wall_second in 1_000..=1_010 {
        take(
            &mut held,
            wall_second,
            &[cpu(b"/A.app", wall_second * 1_000)],
        );
    }

    let view = published(&mut held);

    let a = name(b"/A.app");
    let known: u64 = view.apps[..10]
        .iter()
        .map(|second| cpu_of(&second.attribution(AppAttributionMetric::Cpu, &[Arc::clone(&a)])))
        .sum();
    let shown = view.stretches[0].attribution(AppAttributionMetric::Cpu, &[a]);
    assert_eq!(view.stretches.len(), 1);
    assert_eq!(cpu_of(&shown), known);
    assert_eq!(
        shown.apps[0].unwrap().rate,
        AppAttributionMetric::Cpu.rate(known, Some(10 * NANOSECONDS_PER_SECOND))
    );
}

#[test]
fn an_app_missing_from_a_second_holds_no_memory_there() {
    // Goal: the chart of resident memory stacks a stretch's Apps to what they held on average
    // over its seconds. Method: A holds four kibibytes at each second of the stretch of 1,000 to
    // 1,009, and B two at the first five only, so B holds one on average.
    let mut held = Held::new();
    for wall_second in 1_000..=1_010 {
        let mut used = vec![resident(b"/A.app", 4_096)];
        if wall_second < 1_005 {
            used.push(resident(b"/B.app", 2_048));
        }
        take(&mut held, wall_second, &used);
    }

    let view = published(&mut held);

    let shown = view.stretches[0].resident(&[name(b"/A.app"), name(b"/B.app")]);
    let known: Vec<_> = shown.apps[..2]
        .iter()
        .map(|app| app.unwrap().known)
        .collect();
    assert_eq!(known, [4_096, 1_024]);
}

#[test]
fn a_second_without_its_apps_adds_no_interval() {
    // Goal: a second whose Apps the store did not read leaves a stretch's rates those of its
    // other seconds. Method: A uses a microsecond of CPU at each second of the stretch of 1,000
    // to 1,009 but 1,003, whose Apps are unread, and its rate is compared with one second's.
    let mut held = Held::new();
    for wall_second in 1_000..=1_010 {
        if wall_second == 1_003 {
            let unread = TickApps::Missing(&Missing::Unread);
            assert!(held.take(&RUN, &second_at(wall_second), unread));
        } else {
            take(&mut held, wall_second, &[cpu(b"/A.app", 1_000)]);
        }
    }

    let view = published(&mut held);

    let shown = view.stretches[0].attribution(AppAttributionMetric::Cpu, &[name(b"/A.app")]);
    assert_eq!(cpu_of(&shown), 9_000);
    assert_eq!(
        shown.apps[0].unwrap().rate,
        AppAttributionMetric::Cpu.rate(1_000, Some(NANOSECONDS_PER_SECOND))
    );
}

#[test]
fn a_second_measured_over_no_interval_adds_no_use() {
    // Goal: a second that measured its Apps over no interval, as a run's first does, leaves a
    // stretch's rates those of its other seconds. Method: A uses a microsecond of CPU at each
    // second of the stretch of 1,000 to 1,009, 1,000 measured over no interval, and its rate is
    // compared with one second's.
    let mut held = Held::new();
    for wall_second in 1_000..=1_010 {
        let mut apps = second_with(wall_second, &[cpu(b"/A.app", 1_000)]);
        if wall_second == 1_000 {
            apps.elapsed_ns = None;
        }
        let second = second_at(wall_second);
        assert!(held.take(&RUN, &second, TickApps::Loaded(&mut apps)));
    }

    let view = published(&mut held);

    let shown = view.stretches[0].attribution(AppAttributionMetric::Cpu, &[name(b"/A.app")]);
    assert_eq!(cpu_of(&shown), 9_000);
    assert_eq!(
        shown.apps[0].unwrap().rate,
        AppAttributionMetric::Cpu.rate(1_000, Some(NANOSECONDS_PER_SECOND))
    );
}

#[test]
fn the_store_lets_go_of_the_oldest_stretch_past_the_loaded_hour() {
    // Goal: the stretches stay within their bound. Method: close two stretches more than the
    // store holds, from wall second 1,000, and read the oldest stretch left.
    let mut held = Held::new();
    let closed = u64::try_from(STRETCHES_MAX).unwrap() + 2;

    for wall_second in 1_000..=1_000 + closed * 10 {
        let unread = TickApps::Missing(&Missing::Unread);
        assert!(held.take(&RUN, &second_at(wall_second), unread));
    }

    let view = published(&mut held);
    assert_eq!(view.stretches.len(), STRETCHES_MAX);
    assert_eq!(view.stretches[0].stretch, 102);
}

#[test]
fn a_stretch_of_more_apps_than_it_holds_holds_none() {
    // Goal: a stretch past its bound says it holds no Apps rather than showing some of them.
    // Method: seconds 1,000 and 1,001 each hold a different half of one App more than a
    // stretch holds, and the chart's Other of the stretch is read.
    let mut held = Held::new();
    let keys: Vec<Vec<u8>> = (0..=APPS_PER_SECOND_MAX)
        .map(|index| format!("/{index:05}.app").into_bytes())
        .collect();
    let (first, second) = keys.split_at(keys.len() / 2);

    for (wall_second, keys) in [(1_000, first), (1_001, second)] {
        let used: Vec<_> = keys.iter().map(|key| cpu(key, 1_000)).collect();
        take(&mut held, wall_second, &used);
    }
    take(&mut held, 1_010, &[]);

    let view = published(&mut held);
    let shown = view.stretches[0].attribution(AppAttributionMetric::Cpu, &[]);
    assert_eq!(shown.other, None);
}

/// What one App used over one second.
struct Used<'key> {
    key: &'key [u8],
    cpu_ns: u64,
    resident_bytes: u64,
}

fn cpu(key: &[u8], cpu_ns: u64) -> Used<'_> {
    Used {
        key,
        cpu_ns,
        resident_bytes: 0,
    }
}

fn resident(key: &[u8], resident_bytes: u64) -> Used<'_> {
    Used {
        key,
        cpu_ns: 0,
        resident_bytes,
    }
}

/// Takes the second at `wall_second` into `held`, with the Apps `used` names, in key order.
fn take(held: &mut Held, wall_second: u64, used: &[Used<'_>]) {
    let mut apps = second_with(wall_second, used);
    assert!(held.take(&RUN, &second_at(wall_second), TickApps::Loaded(&mut apps)));
}

/// The CPU time the first App of `shown` used.
fn cpu_of(shown: &super::Attribution) -> u64 {
    shown.apps[0].unwrap().measure.unwrap().known
}

/// The second at whole wall second `wall_second`, which is also its publication.
fn second_at(wall_second: u64) -> MachineSecond {
    MachineSecond {
        publication: wall_second,
        run_tick: wall_second,
        wall_ns: wall_second * NANOSECONDS_PER_SECOND,
        ..MachineSecond::default()
    }
}

/// The Apps at `wall_second`, as `used` names them, in key order.
fn second_with(wall_second: u64, used: &[Used<'_>]) -> AppsSecond {
    let mut apps = AppsSecond {
        apps: AppRows::with_capacity(used.len()),
        observation: Some(super::take_observation(&RUN, &second_at(wall_second))),
        missing: None,
        elapsed_ns: Some(NANOSECONDS_PER_SECOND),
        membership_complete: true,
        ..AppsSecond::default()
    };
    for used in used {
        apps.apps.push(AppRow::new(&app(used), name(used.key)));
    }
    apps.totals = super::Totals::of(&apps.apps);

    apps
}

/// The App `used` names, one member that measured the CPU time and resident memory it names
/// and nothing else.
fn app(used: &Used<'_>) -> App {
    let unmeasured = Measure {
        known: 0,
        measured_members: 0,
        unavailable_members: 1,
        overflowed: false,
    };
    let measured = |known| Measure {
        known,
        measured_members: 1,
        unavailable_members: 0,
        overflowed: false,
    };
    let key = used.key;

    App {
        key: Key::new(KeyKind::Bundle, key.to_vec()).unwrap(),
        display_name: String::from_utf8(key.to_vec()).unwrap(),
        members: 1,
        live_members: 1,
        membership_complete: true,
        cpu_ns: measured(used.cpu_ns),
        energy_nj: unmeasured,
        gpu_ns: unmeasured,
        disk_read_bytes: unmeasured,
        disk_written_bytes: unmeasured,
        disk_io_bytes: unmeasured,
        resident_bytes: measured(used.resident_bytes),
        footprint_bytes: unmeasured,
    }
}

fn name(key: &[u8]) -> Arc<RankedApp> {
    Arc::new(RankedApp::new(KeyKind::Bundle, key.to_vec()).unwrap())
}

/// The View `held` hands over next.
fn published(held: &mut Held) -> Arc<View> {
    let mailbox = Mailbox::new();
    held.publish(&mailbox);
    mailbox.take().unwrap()
}
