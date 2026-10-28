use std::ops::Range;
use std::sync::Arc;

use super::apps::TickApps;
use super::{
    APPS_PER_SECOND_MAX, AppRow, AppRows, AppsSecond, AppsStretch, Cohorts, Held, Leaders,
    MachineSecond, Mailbox, Missing, Readings, STRETCHES_MAX, View, WallSeconds, recording,
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
fn a_stretch_whose_intervals_add_up_past_a_counter_draws_no_rate() {
    // Goal: intervals a sealed minute's file reports too long to add up leave a stretch with no
    // rate rather than stopping the app. Method: A uses a microsecond of CPU at each second of
    // the stretch of 1,000 to 1,009, each measured over just over half the nanoseconds a u64
    // counts, so two of them add up past it.
    let mut held = Held::new();
    for wall_second in 1_000..=1_010 {
        let mut apps = second_with(wall_second, &[cpu(b"/A.app", 1_000)]);
        apps.elapsed_ns = Some(u64::MAX / 2 + 1);
        let second = second_at(wall_second);
        assert!(held.take(&RUN, &second, TickApps::Loaded(&mut apps)));
    }

    let view = published(&mut held);

    let shown = view.stretches[0].attribution(AppAttributionMetric::Cpu, &[name(b"/A.app")]);
    assert_eq!(view.stretches[0].elapsed_ns(), None);
    assert_eq!(shown.apps[0].unwrap().rate, None);
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
    take_more_apps_than_a_stretch_holds(&mut held, 1_000);
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

#[test]
fn a_window_chooses_the_apps_its_seconds_choose_one_by_one() {
    // Goal: an App chart of the loaded hour draws on lines of their own the Apps that its
    // seconds, taken one by one, choose, each App's mean taken over the seconds that measured
    // it. Method: the View holds seconds 125 to 139 one by one and the stretches of 100 to 129,
    // as `window_view` fills it. C uses 10,000 at 107, which the window of 105 to 139 holds but
    // whose stretch it cuts, so the window counts from 110. The window is compared with a View
    // of every second one by one, and with A, F, E and D, which a mean over the stretch's
    // interval, the cut stretch, or a second of 125 counted twice would put in another order.
    let view = window_view();
    let mut leaders = Leaders::new();
    let from_stretches = window_from_stretches(&view);
    let one_by_one = window_one_by_one(&view);

    let chosen = chosen_over(
        &mut leaders,
        &from_stretches,
        WallSeconds {
            first: 105,
            last: 139,
        },
    );
    let expected = chosen_over(
        &mut leaders,
        &one_by_one,
        WallSeconds {
            first: 110,
            last: 139,
        },
    );

    assert_eq!(chosen.cpu, expected.cpu);
    assert_eq!(chosen.resident, expected.resident);
    assert_eq!(chosen.cpu, ["/A.app", "/F.app", "/E.app", "/D.app"]);
    assert_eq!(chosen.resident, ["/A.app", "/F.app", "/E.app", "/D.app"]);
    assert_eq!(chosen.seconds, 15);
}

#[test]
fn a_window_that_ends_with_a_stretch_counts_all_of_its_seconds() {
    // Goal: a window whose last second ends a closed stretch counts the whole stretch, the
    // seconds of it the View no longer holds one by one too. Method: the View holds seconds
    // 125 to 139 one by one and the stretches of 100 to 129, as `window_view` fills it, and
    // the window runs from 110 to 129. F uses 40 at 122, which only the stretch of 120 to 129
    // holds. The window is compared with a View of every second one by one, which counts the
    // 20 seconds of the window.
    let view = window_view();
    let mut leaders = Leaders::new();
    let window = WallSeconds {
        first: 110,
        last: 129,
    };

    let chosen = chosen_over(&mut leaders, &window_from_stretches(&view), window);
    let expected = chosen_over(&mut leaders, &window_one_by_one(&view), window);

    assert_eq!(chosen.cpu, expected.cpu);
    assert_eq!(chosen.resident, expected.resident);
    assert_eq!(chosen.cpu, ["/D.app", "/A.app", "/F.app", "/E.app"]);
    assert_eq!(chosen.seconds, 5);
    assert_eq!(expected.seconds, 20);
}

#[test]
fn a_window_counts_no_stretch_from_before_the_clock_was_set_back() {
    // Goal: an App chart counts only the seconds after the clock was last set back, in the
    // stretches closed since as well as one by one. Method: X uses 90 at every second from 100
    // to 129, then the clock goes back to 105 and Y uses 20 at every second to 119, so the View
    // holds the stretches of 100 to 129 and then that of 105 to 109. The window of 100 to 119
    // is compared with the same View's seconds one by one.
    let mut held = Held::new();
    for wall_second in 100..=129 {
        take(&mut held, wall_second, &[cpu(b"/X.app", 90)]);
    }
    for (publication, wall_second) in (130..).zip(105..=119) {
        take_published(&mut held, publication, wall_second, &[cpu(b"/Y.app", 20)]);
    }
    let view = published(&mut held);
    let mut leaders = Leaders::new();
    let window = WallSeconds {
        first: 100,
        last: 119,
    };

    let chosen = chosen_over(&mut leaders, &window_one_by_one(&view), window);
    let from_stretches = Readings {
        stretches: &view.stretches,
        ..window_one_by_one(&view)
    };
    let chosen_from_stretches = chosen_over(&mut leaders, &from_stretches, window);

    assert_eq!(chosen_from_stretches.cpu, chosen.cpu);
    assert_eq!(chosen.cpu, ["/Y.app"]);
    assert_eq!(chosen_from_stretches.seconds, 15);
}

#[test]
fn a_window_chooses_its_apps_past_a_stretch_of_more_apps_than_it_holds() {
    // Goal: a stretch that holds no Apps, since its seconds came to more than it holds, adds
    // none to an App chart's leaders, as a second of more Apps than it holds adds none, and
    // leaves the chart the Apps of its other seconds. Method: seconds 1,000 and 1,001 each hold
    // a different half of one App more than a stretch holds, and A uses CPU at every second
    // from 1,010 to 1,020. The window of 1,000 to 1,020 is chosen from the View's stretches.
    let mut held = Held::new();
    take_more_apps_than_a_stretch_holds(&mut held, 1_000);
    for wall_second in 1_010..=1_020 {
        take(&mut held, wall_second, &[cpu(b"/A.app", 1_000)]);
    }
    let view = published(&mut held);
    let readings = Readings {
        stretches: &view.stretches,
        ..window_one_by_one(&view)
    };
    let window = WallSeconds {
        first: 1_000,
        last: 1_020,
    };

    let chosen = chosen_over(&mut Leaders::new(), &readings, window);

    assert_eq!(chosen.cpu, ["/A.app"]);
}

#[test]
fn leaders_keep_no_app_alive_between_choices() {
    // Goal: the room a chart's leaders are chosen in keeps no App's name alive once the View
    // and the chosen Apps are let go of, as the store lets go of a stretch's names with it.
    // Method: choose the leaders of `window_view` one by one, keep only a weak reference to
    // the App leading CPU time, and let go of the View and the cohorts.
    let view = window_view();
    let mut leaders = Leaders::new();
    let mut cohorts = Cohorts::with_room();
    let window = WallSeconds {
        first: 100,
        last: 139,
    };
    leaders.choose(&window_one_by_one(&view), window, &mut cohorts);
    let leader = Arc::downgrade(&cohorts.cpu[0]);

    drop(cohorts);
    drop(view);

    assert!(leader.upgrade().is_none());
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

/// What A, C, D and E use at `wall_second`, in key order: the CPU time each uses is also the
/// resident memory it holds.
/// A View of seconds 100 to 139, each with the Apps `window_used` names.
fn window_view() -> Arc<View> {
    let mut held = Held::new();
    for wall_second in 100..=139 {
        take(&mut held, wall_second, &window_used(wall_second));
    }

    published(&mut held)
}

/// The readings of `view`, a `window_view`, that hold seconds 125 to 139 one by one and the
/// stretches of 100 to 129.
fn window_from_stretches(view: &View) -> Readings<'_> {
    Readings {
        seconds: &view.seconds[25..],
        apps: &view.apps[25..],
        stretches: &view.stretches,
    }
}

/// The readings of `view` that hold each of its seconds one by one and no stretch.
fn window_one_by_one(view: &View) -> Readings<'_> {
    Readings {
        seconds: &view.seconds,
        apps: &view.apps,
        stretches: &[],
    }
}

/// What each App uses at `wall_second` of `window_view`: A 50 at 112 only, F 40 at 122 only,
/// C 10,000 at 107 only, D 60 at 125 and none at 130, and E 35 at every second, each in
/// nanoseconds of CPU time and in bytes of resident memory.
fn window_used(wall_second: u64) -> Vec<Used<'static>> {
    let mut used = Vec::new();
    let mut add = |key, amount| {
        used.push(Used {
            key,
            cpu_ns: amount,
            resident_bytes: amount,
        });
    };
    if wall_second == 112 {
        add(b"/A.app".as_slice(), 50);
    }
    if wall_second == 107 {
        add(b"/C.app".as_slice(), 10_000);
    }
    if wall_second == 125 {
        add(b"/D.app".as_slice(), 60);
    }
    if wall_second == 130 {
        add(b"/D.app".as_slice(), 0);
    }
    add(b"/E.app".as_slice(), 35);
    if wall_second == 122 {
        add(b"/F.app".as_slice(), 40);
    }

    used
}

/// The Apps the App charts of CPU time and resident memory draw on lines of their own over a
/// window of readings, by name, and the seconds they draw one by one.
struct Chosen {
    cpu: Vec<String>,
    resident: Vec<String>,
    seconds: usize,
}

fn chosen_over(leaders: &mut Leaders, readings: &Readings<'_>, window: WallSeconds) -> Chosen {
    let mut cohorts = Cohorts::with_room();
    leaders.choose(readings, window, &mut cohorts);
    assert!(!cohorts.refused);
    let names = |cohort: &[Arc<RankedApp>]| {
        cohort
            .iter()
            .map(|app| String::from_utf8(app.bytes.clone()).unwrap())
            .collect()
    };

    Chosen {
        cpu: names(&cohorts.cpu),
        resident: names(&cohorts.resident),
        seconds: cohorts.seconds,
    }
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
/// Takes in `wall_second` and the second after it, each holding a different half of one App
/// more than a stretch holds.
fn take_more_apps_than_a_stretch_holds(held: &mut Held, wall_second: u64) {
    let keys: Vec<Vec<u8>> = (0..=APPS_PER_SECOND_MAX)
        .map(|index| format!("/{index:05}.app").into_bytes())
        .collect();
    let (first, second) = keys.split_at(keys.len() / 2);

    for (wall_second, keys) in [(wall_second, first), (wall_second + 1, second)] {
        let used: Vec<_> = keys.iter().map(|key| cpu(key, 1_000)).collect();
        take(held, wall_second, &used);
    }
}

fn take(held: &mut Held, wall_second: u64, used: &[Used<'_>]) {
    take_published(held, wall_second, wall_second, used);
}

/// Takes into `held` the second published at `publication` at whole wall second
/// `wall_second`, with the Apps `used` names, in key order.
fn take_published(held: &mut Held, publication: u64, wall_second: u64, used: &[Used<'_>]) {
    let second = MachineSecond {
        publication,
        run_tick: publication,
        ..second_at(wall_second)
    };
    let mut apps = second_with(wall_second, used);
    apps.observation = Some(super::take_observation(&RUN, &second));
    assert!(held.take(&RUN, &second, TickApps::Loaded(&mut apps)));
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
