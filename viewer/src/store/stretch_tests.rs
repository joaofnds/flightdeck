use std::ops::Range;
use std::sync::Arc;

use super::apps::TickApps;
use super::stretch::{Folded, Folding, StretchRow};
use super::{
    APPS_PER_SECOND_MAX, AppRow, AppRows, AppsSecond, AppsStretch, Cohorts, Held, Leaders,
    MachineSecond, Mailbox, Missing, Readings, SECONDS_HELD_MAX, STRETCHES_MAX, View, WallSeconds,
    recording,
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
fn a_closed_stretch_adds_up_the_energy_and_the_disk_bytes_of_its_seconds() {
    // Goal: the Energy and Disk charts by App draw a point of ten seconds from what each App
    // used of their own resource over those seconds. Method: take in seconds 1,000 to 1,010, A
    // using 1,000 ns of CPU, 2,000 nJ and 3,000 bytes of disk at each, so 1,010 closes the
    // stretch of 1,000 to 1,009, and read what A used of energy and disk over the stretch.
    let mut held = Held::new();
    for wall_second in 1_000..=1_010 {
        let used = Used {
            energy_nj: Some(2_000),
            disk_io_bytes: Some(3_000),
            ..cpu(b"/A.app", 1_000)
        };
        take(&mut held, wall_second, &[used]);
    }

    let view = published(&mut held);

    let a = [name(b"/A.app")];
    let used_of = |metric| cpu_of(&view.stretches[0].attribution(metric, &a));
    assert_eq!(used_of(AppAttributionMetric::Power), 20_000);
    assert_eq!(used_of(AppAttributionMetric::Disk), 30_000);
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
fn a_reset_store_holds_no_stretch_of_the_seconds_before_it() {
    // Goal: a store reset for a load that replaces what it held keeps no stretch of the seconds
    // it held before, closed or open. Method: close stretch 100 and open 101, each holding A,
    // reset the store, take wall seconds 10,000 and 10,010, and read the View's stretches.
    let mut held = Held::new();
    take(&mut held, 1_000, &[cpu(b"/A.app", 1_000)]);
    take(&mut held, 1_010, &[cpu(b"/A.app", 1_000)]);

    held.reset();
    take(&mut held, 10_000, &[]);
    take(&mut held, 10_010, &[]);

    let view = published(&mut held);
    let stretches: Vec<_> = view
        .stretches
        .iter()
        .map(|stretch| stretch.stretch)
        .collect();
    assert_eq!(stretches, [1_000]);
}

#[test]
fn a_view_of_the_whole_loaded_hour_holds_back_no_second_the_live_span_has_room_for() {
    // Goal: while the main thread holds a View of every stretch the store holds, the store takes
    // each second its live span has a spare buffer for, since those seconds close fewer stretches
    // than there are spare ones. Method: fill every span from wall second 1,000 up to the last
    // second of a stretch, hold the View, take a second at a time until the store refuses one,
    // and count the seconds it took beside the spare seconds.
    let mut held = Held::new();
    let newest = 1_000 + (u64::try_from(STRETCHES_MAX).unwrap() + 2) * 10 - 1;
    for wall_second in 1_000..=newest {
        let unread = TickApps::Missing(&Missing::Unread);
        assert!(held.take(&RUN, &second_at(wall_second), unread));
    }
    let view = published(&mut held);
    assert_eq!(view.stretches.len(), STRETCHES_MAX);

    let spare = u64::try_from(SECONDS_HELD_MAX).unwrap();
    let mut taken = 0;
    for wall_second in newest + 1..=newest + spare + 1 {
        let unread = TickApps::Missing(&Missing::Unread);
        if !held.take(&RUN, &second_at(wall_second), unread) {
            break;
        }
        taken += 1;
    }

    assert_eq!(taken, spare);
    assert_eq!(view.stretches[0].stretch, 101);
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
fn a_stretch_a_load_added_up_part_of_holds_none_where_that_part_holds_more_apps_than_it_holds() {
    // Goal: a stretch holds no Apps where the part of it a load added up held more than a
    // stretch holds, rather than showing the Apps of its other seconds as all of them. Method:
    // a load adds up seconds 1,000 and 1,001, each holding a different half of one App more than
    // a stretch holds, and hands them to 1,000. The store takes both in, then 1,002 with A, closes
    // the stretch at 1,010, and the chart's Other of the stretch is read.
    let mut folding = Folding::new();
    for (_, apps) in more_apps_than_a_stretch_holds(1_000) {
        assert!(folding.add(100, &apps).is_none());
    }
    let folded = Folded {
        apps: folding.finish().unwrap(),
        publication: 1_001,
    };
    let mut held = Held::new();
    assert!(held.take(&RUN, &second_at(1_000), TickApps::Folded(&folded)));
    let unread = TickApps::Missing(&Missing::Unread);
    assert!(held.take(&RUN, &second_at(1_001), unread));
    take(&mut held, 1_002, &[cpu(b"/A.app", 1_000)]);
    take(&mut held, 1_010, &[]);

    let view = published(&mut held);
    let shown = view.stretches[0].attribution(AppAttributionMetric::Cpu, &[]);
    assert_eq!(view.stretches[0].stretch, 100);
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
fn a_load_stopped_at_any_second_then_caught_up_adds_up_each_stretch_whole() {
    // Goal: a load the store stops partway, as when the window hides, leaves no stretch adding
    // up fewer seconds once the store catches up. Method: seal a few ticks of run minutes 0, 1
    // and 21, so the stretch of wall seconds 60 to 69 is older than the seconds whose Apps the
    // store holds one by one and crosses minutes 0 and 1, which two loaders decode. Stop a load
    // at each second of that stretch and the one after it, and catch up.
    let fixture = recording_of(&[(0, 55..60), (1, 0..5), (21, 0..5)]);

    assert_stopped_loads_catch_up(&fixture, 60..71);
}

#[test]
fn a_load_stopped_inside_a_stretch_across_a_restart_then_caught_up_adds_up_the_stretch_whole() {
    // Goal: a stopped load leaves no stretch counting a second twice or not at all where the
    // collector restarted inside the stretch. Method: seal ticks of run minutes 0 to 2 of one run,
    // the last at wall seconds 180 to 184, then ticks of the next run from wall second 185 on, so
    // the stretch of wall seconds 180 to 189 is older than the seconds whose Apps the store holds
    // one by one and holds both runs. Seven minutes put two on each loader, so one loader decodes
    // both runs of that stretch. Stop a load at each second of that stretch and the one after it,
    // and catch up.
    let first = recording_of(&[(0, 0..5), (1, 55..60), (2, 55..60)]);
    let mut next = Fixture::new_mode(true, 2, Some(&first));
    next.start_at_run_tick(180);
    seal_minutes(
        &mut next,
        180,
        &[(3, 0..5), (4, 0..5), (5, 0..5), (23, 0..5)],
    );

    assert_stopped_loads_catch_up(&first, 180..191);
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
fn a_window_chooses_the_leaders_of_each_resource_by_what_they_used_of_it() {
    // Goal: the Energy and Disk charts of the loaded hour lead with the Apps that used the most
    // of their own resource over its stretches. Method: at every second from 100 to 139, A uses
    // the most CPU, B the most energy and C the most disk bytes, and the window of 100 to 129
    // is chosen from the View's stretches and its seconds from 125 on.
    let mut held = Held::new();
    for wall_second in 100..=139 {
        let used = |key, [cpu_ns, energy_nj, disk_io_bytes]: [u64; 3]| Used {
            energy_nj: Some(energy_nj),
            disk_io_bytes: Some(disk_io_bytes),
            ..cpu(key, cpu_ns)
        };
        let apps = [
            used(b"/A.app", [300, 100, 200]),
            used(b"/B.app", [200, 300, 100]),
            used(b"/C.app", [100, 200, 300]),
        ];
        take(&mut held, wall_second, &apps);
    }
    let view = published(&mut held);
    let window = WallSeconds {
        first: 100,
        last: 129,
    };

    let chosen = chosen_over(&mut Leaders::new(), &window_from_stretches(&view), window);

    assert_eq!(chosen.cpu, ["/A.app", "/B.app", "/C.app"]);
    assert_eq!(chosen.power, ["/B.app", "/C.app", "/A.app"]);
    assert_eq!(chosen.disk, ["/C.app", "/A.app", "/B.app"]);
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

/// Asserts that a store that loads `fixture`'s recording, stops before the second at each of
/// `stop_seconds`, then catches up, as the store does once the window shows again, holds the
/// stretches a store that loads the whole recording at once holds.
fn assert_stopped_loads_catch_up(fixture: &Fixture, stop_seconds: Range<u64>) {
    let whole = loaded(fixture);
    let cohort = cohort_of(&whole);
    assert!(!whole.stretches.is_empty());
    for expected in &whole.stretches {
        assert!(expected.holds_apps());
    }

    for stop_second in stop_seconds {
        let mut held = Held::new();
        let failure = recording::load(&fixture.root, |run, second, apps| {
            if second.wall_ns < stop_second * 1_000_000_000 {
                assert!(held.take(run, second, apps));
            }
        });
        assert_eq!(failure, None);
        catch_up(&mut held, fixture);
        let view = published(&mut held);

        assert_eq!(view.stretches.len(), whole.stretches.len());
        for (shown, expected) in view.stretches.iter().zip(&whole.stretches) {
            assert_eq!(shown.stretch, expected.stretch, "stopped at {stop_second}");
            assert_eq!(
                shown.elapsed_ns(),
                expected.elapsed_ns(),
                "stopped at {stop_second}"
            );
            assert_eq!(
                resident_measured_of(shown),
                resident_measured_of(expected),
                "stopped at {stop_second}"
            );
            assert_eq!(
                readings_of(shown, &cohort),
                readings_of(expected, &cohort),
                "stopped at {stop_second}"
            );
        }
    }
}

/// Catches `held` up on `fixture`'s recording after its newest second, as the store does, which
/// lets go of every second it holds where the load replaces them.
fn catch_up(held: &mut Held, fixture: &Fixture) {
    let since = held.newest;
    let mut first = true;
    let failure = recording::load_since(
        &fixture.root,
        since.as_ref(),
        &mut recording::LoadBuffers::default(),
        |continuity, run, second, apps| {
            if first && continuity == recording::Continuity::Replace {
                held.reset();
            }
            first = false;
            assert!(held.take(run, second, apps));
        },
    );
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

/// Each App's resident memory added up over the seconds of `stretch` that measured it, and how
/// many they are, in key order.
fn resident_measured_of(stretch: &AppsStretch) -> Vec<Option<(u64, u32)>> {
    let rows = stretch.apps().unwrap();

    rows.iter().map(StretchRow::resident_measured).collect()
}

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
            energy_nj: None,
            disk_io_bytes: None,
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

/// The Apps the App charts of CPU time, energy, disk bytes and resident memory draw on lines of
/// their own over a window of readings, by name, and the seconds they draw one by one.
struct Chosen {
    cpu: Vec<String>,
    power: Vec<String>,
    disk: Vec<String>,
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
        power: names(&cohorts.power),
        disk: names(&cohorts.disk),
        resident: names(&cohorts.resident),
        seconds: cohorts.seconds,
    }
}

/// What one App used over one second, with no energy or disk bytes measured where it names
/// none.
struct Used<'key> {
    key: &'key [u8],
    cpu_ns: u64,
    energy_nj: Option<u64>,
    disk_io_bytes: Option<u64>,
    resident_bytes: u64,
}

fn cpu(key: &[u8], cpu_ns: u64) -> Used<'_> {
    Used {
        key,
        cpu_ns,
        energy_nj: None,
        disk_io_bytes: None,
        resident_bytes: 0,
    }
}

fn resident(key: &[u8], resident_bytes: u64) -> Used<'_> {
    Used {
        key,
        cpu_ns: 0,
        energy_nj: None,
        disk_io_bytes: None,
        resident_bytes,
    }
}

/// Takes in `wall_second` and the second after it, each holding a different half of one App
/// more than a stretch holds.
fn take_more_apps_than_a_stretch_holds(held: &mut Held, wall_second: u64) {
    for (wall_second, mut apps) in more_apps_than_a_stretch_holds(wall_second) {
        let second = second_at(wall_second);
        assert!(held.take(&RUN, &second, TickApps::Loaded(&mut apps)));
    }
}

/// The Apps at `wall_second` and the second after it, each beside its wall second and a
/// different half of one App more than a stretch holds.
fn more_apps_than_a_stretch_holds(wall_second: u64) -> [(u64, AppsSecond); 2] {
    let keys: Vec<Vec<u8>> = (0..=APPS_PER_SECOND_MAX)
        .map(|index| format!("/{index:05}.app").into_bytes())
        .collect();
    let (first, second) = keys.split_at(keys.len() / 2);

    [(wall_second, first), (wall_second + 1, second)].map(|(wall_second, keys)| {
        let used: Vec<_> = keys.iter().map(|key| cpu(key, 1_000)).collect();
        (wall_second, second_with(wall_second, &used))
    })
}

/// Takes the second at `wall_second` into `held`, with the Apps `used` names, in key order.
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

/// The App `used` names, one member that measured the CPU time, resident memory, energy and disk
/// bytes it names and nothing else.
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
        energy_nj: used.energy_nj.map_or(unmeasured, measured),
        gpu_ns: unmeasured,
        disk_read_bytes: unmeasured,
        disk_written_bytes: unmeasured,
        disk_io_bytes: used.disk_io_bytes.map_or(unmeasured, measured),
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
