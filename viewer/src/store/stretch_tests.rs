use std::ops::Range;
use std::sync::Arc;

use super::apps::TickApps;
use super::{
    APPS_PER_SECOND_MAX, AppRow, AppRows, AppsSecond, AppsStretch, Held, MachineSecond, Mailbox,
    Missing, STRETCHES_MAX, View, recording,
};
use crate::AppAttributionMetric;
use crate::live::identity_fixture::Fixture;
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

#[test]
fn a_load_adds_up_the_apps_of_older_minutes_as_their_seconds_add_up() {
    // Goal: an App chart draws the older part of the loaded hour as it would were the store to
    // hold each of its seconds' Apps. Method: seal a few ticks of run minutes 0 to 3 and 21, each
    // tick five wall seconds past its run tick, so minutes 0 to 2 are older than the seconds whose
    // Apps the store holds one by one, the stretch of wall seconds 60 to 69 crosses minutes 0 and
    // 1, and that of 180 to 189 crosses minutes 2 and 3. Compare the stretches of minutes 0 to 3
    // with those of a recording that seals those minutes alone.
    let older = [(0, 55..60), (1, 0..5), (2, 55..60), (3, 0..6)];
    let alone = loaded(&recording_of(&older));
    let cohort = cohort_of(&alone);

    let mut minutes = older.to_vec();
    minutes.push((21, 0..5));
    let view = loaded(&recording_of(&minutes));

    let expected = &alone.stretches[..];
    let stretches: Vec<_> = expected.iter().map(|stretch| stretch.stretch).collect();
    assert_eq!(stretches, [6, 18]);
    for expected in expected {
        let shown = expected.attribution(AppAttributionMetric::Cpu, &cohort);
        assert!(cpu_of(&shown) > 0);
    }
    let shown = &view.stretches[..expected.len()];
    for (shown, expected) in shown.iter().zip(expected) {
        assert_eq!(shown.stretch, expected.stretch);
        assert_eq!(readings_of(shown, &cohort), readings_of(expected, &cohort));
    }
}

#[test]
fn a_catch_up_adds_up_no_second_the_store_holds() {
    // Goal: a store that catches up on minutes it missed adds each second's Apps to its stretch
    // once, though the load hands over again the seconds it holds of the minute it held last.
    // Method: seal a few ticks of run minutes 0 and 1, and have the store take the seconds up to
    // wall second 66 alone, as it does when it takes minute 1's first seconds live before the
    // minute seals. Then seal ticks of minute 21 and catch up, so minutes 0 and 1 are older than
    // the seconds whose Apps the store holds one by one and the stretch of wall seconds 60 to 69
    // crosses them. Compare that stretch with the one a store that loads the whole recording at
    // once holds.
    let older = [(0, 55..60), (1, 0..5)];
    let newer = [(21, 0..5)];
    let mut fixture = recording_of(&older);
    let mut held = Held::new();
    let failure = recording::load(&fixture.root, |run, second, apps| {
        if second.wall_ns < 67 * 1_000_000_000 {
            assert!(held.take(run, second, apps));
        }
    });
    assert_eq!(failure, None);
    seal_minutes(&mut fixture, 65, &newer);
    let whole = loaded(&fixture);
    let cohort = cohort_of(&whole);

    let since = held.newest;
    let failure = recording::load_since(
        &fixture.root,
        since.as_ref(),
        &mut recording::LoadBuffers::default(),
        |continuity, run, second, apps| {
            assert_eq!(continuity, recording::Continuity::Extend);
            assert!(held.take(run, second, apps));
        },
    );

    assert_eq!(failure, None);
    let view = published(&mut held);
    let expected = &whole.stretches[0];
    assert_eq!(expected.stretch, 6);
    assert!(cpu_of(&expected.attribution(AppAttributionMetric::Cpu, &cohort)) > 0);
    assert_eq!(view.stretches[0].stretch, expected.stretch);
    assert_eq!(
        readings_of(&view.stretches[0], &cohort),
        readings_of(expected, &cohort)
    );
}

/// A recording of the ticks `minutes` names, as `seal_minutes` seals them.
fn recording_of(minutes: &[(u64, Range<u64>)]) -> Fixture {
    let mut fixture = Fixture::new();
    seal_minutes(&mut fixture, 0, minutes);

    fixture
}

/// Publishes the ticks `minutes` names in `fixture`, by run minute and the ticks of the minute,
/// after run tick `run_tick`, and seals each minute with its Apps, each tick five wall seconds
/// past its run tick. The fixture's ring holds eight ticks, so each minute names at most that
/// many. Returns the run tick after the last.
fn seal_minutes(fixture: &mut Fixture, mut run_tick: u64, minutes: &[(u64, Range<u64>)]) -> u64 {
    for (minute, ticks) in minutes {
        assert!(ticks.end - ticks.start <= 8);
        let first = minute * 60 + ticks.start;
        fixture.skip_run_ticks(first - run_tick);
        run_tick = first;
        for _ in ticks.clone() {
            fixture.publish_with_events(run_tick + 5, &[]);
            run_tick += 1;
        }
        fixture.seal_minute_derived(*minute, &persistence::sealed_apps::SealedApps::default());
    }

    run_tick
}

/// The View of a store that loaded `fixture`'s recording.
fn loaded(fixture: &Fixture) -> Arc<View> {
    let mut held = Held::new();
    load_into(&mut held, fixture);

    published(&mut held)
}

/// Loads `fixture`'s recording into `held`, as the store does as it starts.
fn load_into(held: &mut Held, fixture: &Fixture) {
    let failure = recording::load(&fixture.root, |run, second, apps| {
        assert!(held.take(run, second, apps));
    });
    assert_eq!(failure, None);
}

/// The Apps of the newest second `view` holds.
fn cohort_of(view: &View) -> Vec<Arc<RankedApp>> {
    let newest = view.apps.last().unwrap();
    let cohort: Vec<_> = newest
        .apps
        .as_slice()
        .iter()
        .map(|app| Arc::clone(&app.name))
        .collect();
    assert!(!cohort.is_empty());

    cohort
}

/// What `stretch` shows on each of Overview's App charts whose own lines draw `cohort`.
fn readings_of(
    stretch: &AppsStretch,
    cohort: &[Arc<RankedApp>],
) -> (Vec<super::Attribution>, super::Resident) {
    let rates = [
        AppAttributionMetric::Cpu,
        AppAttributionMetric::Power,
        AppAttributionMetric::Disk,
    ]
    .into_iter()
    .map(|metric| stretch.attribution(metric, cohort))
    .collect();

    (rates, stretch.resident(cohort))
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
