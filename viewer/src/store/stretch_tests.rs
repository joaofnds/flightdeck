use std::ops::Range;
use std::sync::Arc;

use super::apps::TickApps;
use super::cohorts::resume_count;
use super::sampled::LoadedRanks;
use super::stretch::{Folded, Folding, StretchRow, Stretches};
use super::{
    APPS_PER_SECOND_MAX, AppRow, AppRows, AppsSecond, AppsSpan, AppsStretch, Cohorts, Held,
    Leaders, MachinePlace, MachineSecond, Mailbox, MemorySecond, Missing, Readings,
    SECONDS_HELD_MAX, SECONDS_MAX, STRETCHES_MAX, View, WallSeconds, recording,
};
use crate::AppAttributionMetric;
use crate::live::identity_fixture::Fixture;
use crate::live_app_response::{App, Key, KeyKind, Measure, Metric, Rankable, RankedApp};
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
    // the seconds themselves add up. Method: take in seconds 1,000 to 1,010, A using 1,000 ns
    // of CPU times its wall second at each, so 1,010 closes the stretch of 1,000 to 1,009, over
    // which A used 10,045,000 ns, and read that stretch.
    let mut held = Held::without_recording();
    for wall_second in 1_000..=1_010 {
        take(
            &mut held,
            wall_second,
            &[cpu(b"/A.app", wall_second * 1_000)],
        );
    }

    let view = published(&mut held);

    let shown = view.stretches[0].attribution(AppAttributionMetric::Cpu, &[name(b"/A.app")]);
    assert_eq!(view.stretches.len(), 1);
    assert_eq!(first_app_amount(&shown), 10_045_000);
    assert_eq!(
        shown.apps[0].unwrap().rate,
        AppAttributionMetric::Cpu.rate(10_045_000, Some(10 * NANOSECONDS_PER_SECOND))
    );
}

#[test]
fn a_closed_stretch_adds_up_the_energy_and_the_disk_bytes_of_its_seconds() {
    // Goal: the Energy and Disk charts by App draw a point of ten seconds from what each App
    // used of their own resource over those seconds. Method: take in seconds 1,000 to 1,010, A
    // using 1,000 ns of CPU, 2,000 nJ and 3,000 bytes of disk at each, so 1,010 closes the
    // stretch of 1,000 to 1,009, and read what A used of energy and disk over the stretch.
    let mut held = Held::without_recording();
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
    let used_of = |metric| first_app_amount(&view.stretches[0].attribution(metric, &a));
    assert_eq!(used_of(AppAttributionMetric::Power), 20_000);
    assert_eq!(used_of(AppAttributionMetric::Disk), 30_000);
}

#[test]
fn a_closed_stretch_adds_up_the_gpu_time_of_its_seconds() {
    // Goal: the GPU page's chart by App draws a point of ten seconds from what each App used of
    // the GPU over those seconds. Method: take in seconds 1,000 to 1,010, A using 4,000 ns of
    // GPU time at each, so 1,010 closes the stretch of 1,000 to 1,009, and read what A used of
    // the GPU over the stretch.
    let mut held = Held::without_recording();
    for wall_second in 1_000..=1_010 {
        let used = Used {
            gpu_ns: Some(4_000),
            ..cpu(b"/A.app", 1_000)
        };
        take(&mut held, wall_second, &[used]);
    }

    let view = published(&mut held);

    let shown = view.stretches[0].attribution(AppAttributionMetric::Gpu, &[name(b"/A.app")]);
    assert_eq!(first_app_amount(&shown), 40_000);
}

#[test]
fn an_app_missing_from_a_second_holds_no_memory_there() {
    // Goal: the chart of resident memory stacks a stretch's Apps to what they held on average
    // over its seconds. Method: A holds four kibibytes at each second of the stretch of 1,000 to
    // 1,009, and B two at the first five only, so B holds one on average.
    let mut held = Held::without_recording();
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
    let mut held = Held::without_recording();
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
    assert_eq!(first_app_amount(&shown), 9_000);
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
    let mut held = Held::without_recording();
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
    let mut held = Held::without_recording();
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
    assert_eq!(first_app_amount(&shown), 9_000);
    assert_eq!(
        shown.apps[0].unwrap().rate,
        AppAttributionMetric::Cpu.rate(1_000, Some(NANOSECONDS_PER_SECOND))
    );
}

#[test]
fn the_store_lets_go_of_the_oldest_stretch_past_the_loaded_hour() {
    // Goal: the stretches stay within their bound. Method: close two stretches more than the
    // store holds, from wall second 1,000, and read the oldest stretch left.
    let mut held = Held::without_recording();
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
    let mut held = Held::without_recording();
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
fn a_view_of_the_whole_loaded_hour_holds_back_no_second() {
    // Goal: while the main thread holds a View of every stretch the store holds, the store takes
    // each second, past its spare buffers too, and the View keeps its stretches. Method: fill
    // every span from wall second 1,000 up to the last second of a stretch, hold the View, and
    // take one second more than there are spare ones.
    let mut held = Held::without_recording();
    let newest = 1_000 + (u64::try_from(STRETCHES_MAX).unwrap() + 2) * 10 - 1;
    for wall_second in 1_000..=newest {
        let unread = TickApps::Missing(&Missing::Unread);
        assert!(held.take(&RUN, &second_at(wall_second), unread));
    }
    let view = published(&mut held);
    assert_eq!(view.stretches.len(), STRETCHES_MAX);

    let spare = u64::try_from(SECONDS_HELD_MAX).unwrap();
    for wall_second in newest + 1..=newest + spare + 1 {
        let unread = TickApps::Missing(&Missing::Unread);
        assert!(held.take(&RUN, &second_at(wall_second), unread));
    }

    assert_eq!(view.stretches[0].stretch, 101);
    assert_eq!(view.stretches.len(), STRETCHES_MAX);
}

#[test]
fn a_stretch_of_more_apps_than_it_holds_holds_none() {
    // Goal: a stretch past its bound says it holds no Apps rather than showing some of them.
    // Method: seconds 1,000 and 1,001 each hold a different half of one App more than a
    // stretch holds, and the chart's Other of the stretch is read.
    let mut held = Held::without_recording();
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
        rankings: Box::new(LoadedRanks::of(&AppsSecond::default())),
    };
    let mut held = Held::without_recording();
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
        assert!(first_app_amount(&shown) > 0);
    }
    let shown = &view.stretches[..expected.len()];
    for (shown, expected) in shown.iter().zip(expected) {
        assert_eq!(shown.stretch, expected.stretch);
        assert_eq!(readings_of(shown, &cohort), readings_of(expected, &cohort));
    }
}

#[test]
fn a_launch_that_owes_older_minutes_apps_hands_over_no_stretch() {
    // Goal: a store that starts hands over its first View before it reads the Apps of the
    // minutes older than the live span, and that View draws no stretch without them, while it
    // holds the live span's Apps. Method: seal a few ticks of run minutes 0, 1 and 21, so minutes
    // 0 and 1 are older than the seconds whose Apps the store holds one by one, and launch a
    // store on the recording.
    let fixture = recording_of(&[(0, 55..60), (1, 0..5), (21, 0..5)]);
    let mut held = Held::without_recording();

    launch(&fixture, &mut held);

    let view = published(&mut held);
    assert!(view.stretches.is_empty());
    let newest = view.apps.last().unwrap();
    assert!(newest.missing.is_none());
    assert!(!newest.apps.as_slice().is_empty());
}

#[test]
fn a_launch_adds_up_the_apps_it_owes_once_the_main_thread_takes_its_first_view() {
    // Goal: a store that starts reads the Apps of the minutes older than the live span only once
    // the main thread has taken its first View, so the pass leaves the cores to the window's
    // first frame. Method: seal a few ticks of run minutes 0, 1 and 21, launch a store, hand over
    // its first View, and run the pass while that View waits and again once it is taken.
    let fixture = recording_of(&[(0, 55..60), (1, 0..5), (21, 0..5)]);
    let mut held = Held::without_recording();
    launch(&fixture, &mut held);
    let mailbox = Mailbox::new();
    held.publish(&mailbox);

    super::run_owed(&fixture.root, &mut held, &mailbox);
    assert!(held.owed.is_some());
    let first = mailbox.take().unwrap();
    super::run_owed(&fixture.root, &mut held, &mailbox);

    assert!(first.stretches.is_empty());
    assert!(held.owed.is_none());
    assert!(!mailbox.take().unwrap().stretches.is_empty());
}

#[test]
fn a_launch_adds_up_the_apps_it_owes_only_once_the_main_thread_takes_the_newest_view() {
    // Goal: a store that took in seconds after its first View, as a return does when the store
    // is shown again before the pass ran, hands them over before it reads the Apps its launch
    // owes, so the pass never holds back a return's View. Method: launch a store, let the main
    // thread take its first View, mark the store as holding seconds no View holds, as a
    // return's load does, and run the pass before and after that View is handed over and taken.
    let fixture = recording_of(&[(0, 55..60), (1, 0..5), (21, 0..5)]);
    let mut held = Held::without_recording();
    launch(&fixture, &mut held);
    let mailbox = Mailbox::new();
    held.publish(&mailbox);
    assert!(mailbox.take().is_some());
    held.unpublished = true;

    super::run_owed(&fixture.root, &mut held, &mailbox);
    assert!(held.owed.is_some());
    assert!(mailbox.take().is_none());
    held.publish(&mailbox);
    assert!(mailbox.take().is_some());
    super::run_owed(&fixture.root, &mut held, &mailbox);

    assert!(held.owed.is_none());
    assert!(!mailbox.take().unwrap().stretches.is_empty());
}

#[test]
fn a_launch_adds_up_the_apps_it_owes_a_stretch_across_two_minutes() {
    // Goal: once a store that started reads the Apps of the minutes older than the live span,
    // each stretch holds what a load of the whole hour holds. Method: seal a few ticks of run
    // minutes 0, 1 and 21, so the stretch of wall seconds 60 to 69 is older than the seconds
    // whose Apps the store holds one by one and crosses minutes 0 and 1. Launch a store, add up
    // the Apps it owes, and compare its stretches with a whole load's.
    let fixture = recording_of(&[(0, 55..60), (1, 0..5), (21, 0..5)]);

    assert_owed_apps_add_up(&fixture);
}

#[test]
fn a_launch_adds_up_the_apps_it_owes_stretches_one_loader_decodes_across_two_minutes() {
    // Goal: the Apps a store adds up after it starts reach a stretch whose two minutes one
    // loader decodes. Method: seal a few ticks of run minutes 0 to 3 and 21, which the loaders
    // decode as minutes 0 and 1, minutes 2 and 3, and minute 21, so the stretch of wall seconds
    // 60 to 69 crosses minutes 0 and 1, both older than the seconds whose Apps the store holds
    // one by one, and the stretch of wall seconds 180 to 189 crosses minute 2, which is older,
    // and minute 3, whose Apps the store holds one by one. Launch a store, add up the Apps it
    // owes, and compare its stretches with a whole load's.
    let batches = recording::load_batches_split(&[1; 5]);
    assert_eq!(batches, [0..2, 2..4, 4..5]);
    let fixture = recording_of(&[(0, 55..60), (1, 0..5), (2, 55..60), (3, 0..6), (21, 0..5)]);

    assert_owed_apps_add_up(&fixture);
}

#[test]
fn a_launch_adds_up_the_apps_it_owes_a_stretch_two_loaders_decode() {
    // Goal: the Apps a store adds up after it starts reach a stretch whose two minutes two
    // loaders decode, the later a minute whose Apps the store holds one by one. Method: seal a
    // few ticks of run minutes 1, 2, 3 and 21, which the loaders decode as minutes 1 and 2 and
    // minutes 3 and 21, so the stretch of wall seconds 180 to 189 crosses minute 2, which is
    // older, and minute 3. Launch a store, add up the Apps it owes, and compare its stretches
    // with a whole load's.
    let batches = recording::load_batches_split(&[1; 4]);
    assert_eq!(batches, [0..2, 2..4]);
    let fixture = recording_of(&[(1, 0..5), (2, 55..60), (3, 0..6), (21, 0..5)]);

    assert_owed_apps_add_up(&fixture);
}

#[test]
fn a_launch_adds_up_the_apps_it_owes_a_stretch_across_a_restart() {
    // Goal: the Apps a store adds up after it starts count each second of a stretch once where
    // the collector restarted inside the stretch. Method: seal ticks of run minutes 0 to 2 of one
    // run, the last at wall seconds 180 to 184, then ticks of the next run from wall second 185
    // on, so the stretch of wall seconds 180 to 189 is older than the seconds whose Apps the
    // store holds one by one and holds both runs. Launch a store, add up the Apps it owes, and
    // compare its stretches with a whole load's.
    let first = recording_of(&[(0, 0..5), (1, 55..60), (2, 55..60)]);
    let mut next = Fixture::new_mode(true, 2, Some(&first));
    next.start_at_run_tick(180);
    seal_minutes(
        &mut next,
        180,
        &[(3, 0..5), (4, 0..5), (5, 0..5), (23, 0..5)],
    );

    assert_owed_apps_add_up(&first);
}

#[test]
fn a_launch_adds_up_the_apps_it_owes_while_a_view_holds_every_stretch_the_store_has_room_for() {
    // Goal: a store adds up the Apps it owes its stretches while the main thread holds a View the
    // store handed over, though the store holds as many stretches as it has room for, which
    // leaves it fewer spare buffers than the stretches it owes. Method: seal a few ticks of run
    // minutes 0 to 9 and 59, so ten stretches are older than the seconds whose Apps the store
    // holds one by one, and launch a store. Have it take in newer stretches until it holds as many
    // as it has room for, hold the View it hands over, add up the Apps it owes, and compare its ten
    // oldest stretches with a whole load's.
    let minutes: Vec<_> = (0..10).map(|minute| (minute, 0..5)).collect();
    let mut fixture = recording_of(&minutes);
    seal_minutes(&mut fixture, 9 * 60 + 5, &[(59, 0..5)]);
    let whole = loaded(&fixture);
    let mut held = Held::with_packing_thread(Arc::new(super::ByteBudget::new(super::BYTES_MAX)));
    launch(&fixture, &mut held);
    // The launch closed ten stretches and opened an eleventh, which wall second 10,000 closes.
    let closed = u64::try_from(STRETCHES_MAX).unwrap() - 11;
    for wall_second in 10_000..=10_000 + closed * 10 {
        let unread = TickApps::Missing(&Missing::Unread);
        assert!(held.take(&RUN, &second_at(wall_second), unread));
    }
    let shown = published(&mut held);
    let owed = held.owed.take().unwrap();

    super::run_owed_add(&fixture.root, &mut held, &owed);

    assert_eq!(held.left_out, None);
    let view = published(&mut held);
    assert_eq!(view.stretches.len(), STRETCHES_MAX);
    for (owed, expected) in view.stretches.iter().zip(&whole.stretches[..10]) {
        assert_eq!(owed.stretch, expected.stretch);
        assert_eq!(owed.elapsed_ns(), expected.elapsed_ns());
    }
    assert!(shown.stretches.is_empty());
}

#[test]
fn a_launch_that_cannot_read_a_minute_it_owes_apps_reports_the_hour_incomplete() {
    // Goal: a store that cannot read the Apps of a minute it owes its stretches says that it
    // holds less of the hour than the recording sealed. Method: seal a few ticks of run minutes
    // 0, 1 and 21, launch a store, empty minute 0's ticks table, then add up the Apps the store
    // owes.
    let fixture = recording_of(&[(0, 55..60), (1, 0..5), (21, 0..5)]);
    let mut held = Held::without_recording();
    launch(&fixture, &mut held);
    let owed = held.owed.take().unwrap();
    super::tests::empty_ticks_file(&fixture, 0);

    super::run_owed_add(&fixture.root, &mut held, &owed);

    assert_eq!(held.left_out, Some(super::Failure::Refused));
}

#[test]
fn a_launch_that_cannot_read_a_minute_it_owes_apps_goes_on_from_its_newest_second_when_shown() {
    // Goal: a store that cannot read the Apps of a minute it owes its stretches reads no saved
    // minute when it shows again over a ring that still holds its newest second, since reading
    // the hour again would refuse the minute again. Method: seal a few ticks of run minutes 0, 1
    // and 21, launch a store, empty minute 0's ticks table, add up the Apps the store owes, take
    // the saved minutes offline, then show the store again.
    let fixture = recording_of(&[(0, 55..60), (1, 0..5), (21, 0..5)]);
    let mut held = Held::without_recording();
    let mut ring = launch(&fixture, &mut held);
    let owed = held.owed.take().unwrap();
    super::tests::empty_ticks_file(&fixture, 0);
    super::run_owed_add(&fixture.root, &mut held, &owed);
    let launched = published(&mut held);
    std::fs::rename(fixture.root.join("data"), fixture.root.join("offline_data")).unwrap();
    let shared = super::Shared::starting();
    let operation = Arc::clone(&shared.cancellation).operation(0).unwrap();

    super::refresh(&fixture.root, &mut held, &mut ring, &shared, &operation).unwrap();

    let shown = published(&mut held);
    assert_eq!(launched.left_out, Some(super::Failure::Refused));
    assert_eq!(shown.left_out, launched.left_out);
    assert_eq!(shown.failure, None);
    assert_eq!(shown.seconds.first(), launched.seconds.first());
}

#[test]
fn a_launch_that_cannot_read_a_minute_it_owes_apps_adds_up_those_of_the_other_minutes() {
    // Goal: a store that cannot read the Apps of one minute it owes its stretches still adds up
    // those of the other minutes it owes them. Method: seal a few ticks of run minutes 0, 1, 2
    // and 21, so the stretch of wall seconds 120 to 129, which minute 2 alone holds, is older
    // than the seconds whose Apps the store holds one by one. Launch a store, empty minute 0's
    // ticks table, add up the Apps the store owes, and compare that stretch with a whole load's.
    let fixture = recording_of(&[(0, 55..60), (1, 0..5), (2, 0..5), (21, 0..5)]);
    let whole = loaded(&fixture);
    let mut held = Held::without_recording();
    launch(&fixture, &mut held);
    let owed = held.owed.take().unwrap();
    super::tests::empty_ticks_file(&fixture, 0);

    super::run_owed_add(&fixture.root, &mut held, &owed);

    let view = published(&mut held);
    let shown = view
        .stretches
        .iter()
        .find(|shown| shown.stretch == 12)
        .unwrap();
    let expected = whole
        .stretches
        .iter()
        .find(|expected| expected.stretch == 12)
        .unwrap();
    assert!(expected.holds_apps());
    assert_eq!(shown.elapsed_ns(), expected.elapsed_ns());
}

#[test]
fn a_launch_adds_up_no_apps_it_owes_a_stretch_the_store_let_go_of() {
    // Goal: a store that let go of a stretch before it adds up the Apps it owed the stretch adds
    // them to no other stretch. Method: seal a few ticks of run minutes 0, 1 and 21 and launch a
    // store, then have it take in more stretches than it holds from wall second 10,000 on, so it
    // lets go of every stretch the launch handed over. Add up the Apps the store owes and read
    // its stretches.
    let fixture = recording_of(&[(0, 55..60), (1, 0..5), (21, 0..5)]);
    let mut held = Held::without_recording();
    launch(&fixture, &mut held);
    let closed = u64::try_from(STRETCHES_MAX).unwrap() + 2;
    for wall_second in 10_000..=10_000 + closed * 10 {
        let unread = TickApps::Missing(&Missing::Unread);
        assert!(held.take(&RUN, &second_at(wall_second), unread));
    }
    let owed = held.owed.take().unwrap();

    super::run_owed_add(&fixture.root, &mut held, &owed);

    assert_eq!(held.left_out, None);
    let view = published(&mut held);
    assert_eq!(view.stretches.len(), STRETCHES_MAX);
    assert_eq!(view.stretches[0].stretch, 1_002);
    for shown in &view.stretches {
        assert!(!shown.holds_apps());
    }
}

#[test]
fn a_started_store_hands_over_the_stretches_a_load_of_the_whole_hour_holds() {
    // Goal: a store that starts on a recording goes on to hand over the stretches its first View
    // leaves out. Method: seal a few ticks of run minutes 0, 1 and 21, start a store on the
    // recording, and compare the first View it hands over that holds a stretch with a whole
    // load's.
    let fixture = recording_of(&[(0, 55..60), (1, 0..5), (21, 0..5)]);
    let whole = loaded(&fixture);

    let store = super::Store::start(&fixture.root).unwrap();

    let view = super::tests::view_where(&store, |view| !view.stretches.is_empty());
    assert_stretches_match(&view, &whole, "once started");
}

#[test]
fn a_store_with_a_budget_hands_over_the_stretches_a_load_of_the_whole_hour_holds() {
    // Goal: the Store the app starts gives the older tail of its charts the means, the highest
    // readings and the Apps a load of the whole hour adds up. Method: seal a few ticks of run
    // minutes 0, 1 and 21, start a store with a budget, and compare the first View it hands over
    // that holds a stretch with a whole load's.
    let fixture = recording_of(&[(0, 55..60), (1, 0..5), (21, 0..5)]);
    let whole = loaded(&fixture);

    let store = store_with_budget(&fixture);

    let view = super::tests::view_where(&store, |view| !view.stretches.is_empty());
    assert_stretches_match(&view, &whole, "once started");
}

#[test]
fn a_store_with_a_budget_adds_the_seconds_it_follows_to_the_stretches_it_loaded() {
    // Goal: the seconds the app's Store takes from the ring add to the stretches its load began,
    // counting no App twice. Method: start a store with a budget on sealed ticks whose last, at
    // wall seconds 120 to 124, begin a stretch, publish and seal 125 to 127 of that stretch while
    // it follows, and compare with a whole load's.
    let mut fixture = Fixture::new();
    let run_tick = seal_minutes(&mut fixture, 0, &[(0, 40..43), (1, 55..60)]);
    let store = store_with_budget(&fixture);
    drop(super::tests::view_where(&store, |view| {
        !view.stretches.is_empty()
    }));

    let run_tick = seal_minutes(&mut fixture, run_tick, &[(2, 0..3)]);

    let newest = run_tick - 1 + 5;
    let view = super::tests::view_where(&store, |view| {
        view.seconds
            .last()
            .is_some_and(|second| second.wall_ns / NANOSECONDS_PER_SECOND == newest)
    });
    assert_stretches_match(&view, &loaded(&fixture), "while following");
}

#[test]
fn a_store_with_a_budget_counts_no_app_twice_when_it_reads_the_hour_again() {
    // Goal: a return past the ring reads the hour again and replaces what the app's Store held,
    // so its stretches read as a whole load's. Method: start a store with a budget, hide it, seal
    // more ticks than the fixture's ring of eight holds, show it, and compare.
    let mut fixture = Fixture::new();
    let run_tick = seal_minutes(&mut fixture, 0, &[(0, 40..43), (1, 0..5)]);
    let store = store_with_budget(&fixture);
    drop(super::tests::view_where(&store, |view| {
        !view.stretches.is_empty()
    }));
    store.set_active(false);
    super::activity_tests::wait_until_paused(&store);

    let run_tick = seal_minutes(&mut fixture, run_tick, &[(2, 0..5), (21, 0..5)]);
    store.set_active(true);

    let newest = run_tick - 1 + 5;
    let view = super::tests::view_where(&store, |view| {
        !view.stretches.is_empty()
            && view
                .seconds
                .last()
                .is_some_and(|second| second.wall_ns / NANOSECONDS_PER_SECOND == newest)
    });
    assert_stretches_match(&view, &loaded(&fixture), "once read again");
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
    let tail = view.apps.skipping(STRETCHED_SECONDS_COUNT);
    let from_stretches = window_from_stretches(&view, &tail);
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
    // Goal: the Energy, Disk and GPU charts of the loaded hour lead with the Apps that used the
    // most of their own resource over its stretches. Method: at every second from 100 to 139, A
    // uses the most CPU and GPU time, B the most energy and C the most disk bytes, each in a
    // different order after the first, and the window of 100 to 129 is chosen from the View's
    // stretches and its seconds from 125 on.
    let mut held = Held::without_recording();
    for wall_second in 100..=139 {
        let used = |key, [cpu_ns, energy_nj, disk_io_bytes, gpu_ns]: [u64; 4]| Used {
            energy_nj: Some(energy_nj),
            disk_io_bytes: Some(disk_io_bytes),
            gpu_ns: Some(gpu_ns),
            ..cpu(key, cpu_ns)
        };
        let apps = [
            used(b"/A.app", [300, 100, 200, 300]),
            used(b"/B.app", [200, 300, 100, 100]),
            used(b"/C.app", [100, 200, 300, 200]),
        ];
        take(&mut held, wall_second, &apps);
    }
    let view = published(&mut held);
    let window = WallSeconds {
        first: 100,
        last: 129,
    };

    let chosen = chosen_over(
        &mut Leaders::new(),
        &window_from_stretches(&view, &view.apps.skipping(STRETCHED_SECONDS_COUNT)),
        window,
    );

    assert_eq!(chosen.cpu, ["/A.app", "/B.app", "/C.app"]);
    assert_eq!(chosen.power, ["/B.app", "/C.app", "/A.app"]);
    assert_eq!(chosen.disk, ["/C.app", "/A.app", "/B.app"]);
    assert_eq!(chosen.gpu, ["/A.app", "/C.app", "/B.app"]);
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

    let chosen = chosen_over(
        &mut leaders,
        &window_from_stretches(&view, &view.apps.skipping(STRETCHED_SECONDS_COUNT)),
        window,
    );
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
    let mut held = Held::without_recording();
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
#[should_panic(expected = "cohort.capacity() >= COHORT_MAX")]
fn a_window_chooses_into_no_cohorts_without_room_for_their_apps() {
    // Goal: leaders chosen into cohorts with no room for them stop at the choice, so the panic
    // names its cause and not the first chosen App that needs room. Method: choose over a window
    // of no seconds, which chooses no App, into cohorts built by `Default`.
    let readings = Readings {
        seconds: &[],
        apps: &AppsSpan::default(),
        stretches: &[],
    };
    let mut cohorts = Cohorts::default();

    Leaders::new().choose(&readings, WallSeconds { first: 0, last: 0 }, &mut cohorts);
}

#[test]
fn a_window_chooses_its_apps_past_a_stretch_of_more_apps_than_it_holds() {
    // Goal: a stretch that holds no Apps, since its seconds came to more than it holds, adds
    // none to an App chart's leaders, as a second of more Apps than it holds adds none, and
    // leaves the chart the Apps of its other seconds. Method: seconds 1,000 and 1,001 each hold
    // a different half of one App more than a stretch holds, and A uses CPU at every second
    // from 1,010 to 1,020. The window of 1,000 to 1,020 is chosen from the View's stretches.
    let mut held = Held::without_recording();
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
fn leaders_keep_the_apps_of_their_last_choice_until_they_forget_them() {
    // Goal: between choices the room a chart's leaders are chosen in keeps the Apps the closed
    // stretches of its last choice add up, so once it forgets, no App's name of a View it let go
    // of is alive. Method: choose the leaders of `window_view` from its stretches, keep a weak
    // reference to the App leading CPU time, let go of the View and the cohorts, then forget.
    let view = window_view();
    let tail = view.apps.skipping(STRETCHED_SECONDS_COUNT);
    let mut leaders = Leaders::new();
    let mut cohorts = Cohorts::with_room();
    let window = WallSeconds {
        first: 100,
        last: 139,
    };
    leaders.choose(&window_from_stretches(&view, &tail), window, &mut cohorts);
    let leader = Arc::downgrade(&cohorts.cpu[0]);
    drop(cohorts);
    drop(tail);
    drop(view);
    assert!(leader.upgrade().is_some());

    leaders.forget();

    assert!(leader.upgrade().is_none());
}

#[test]
fn leaders_reused_as_a_window_slides_choose_as_fresh_leaders() {
    // Goal: one room reused for each second's choice, keeping what the stretches it held wholly
    // add up, chooses as a fresh room does as the window slides across stretches that close and
    // stretches that leave it. Method: take in seconds 100 to 175 one at a time, each a View, and
    // choose the window of the newest 30 seconds from each with one reused room and with a fresh
    // one, comparing the sums and the cohorts. The Views share their stretches, as the store
    // hands them over.
    let mut held = Held::without_recording();
    let mut reused = Leaders::new();
    let mut previous = published(&mut held);
    let mut shared = 0;
    for wall_second in 100..=175 {
        take(&mut held, wall_second, &sliding_used(wall_second));
        let view = published(&mut held);
        shared += shared_stretches(&previous, &view);
        let window = WallSeconds {
            first: wall_second.saturating_sub(29),
            last: wall_second,
        };

        assert_chooses_as_fresh_leaders(&mut reused, &view.readings(), window);
        previous = view;
    }

    assert!(shared > 0);
}

#[test]
fn leaders_add_up_again_a_stretch_replaced_by_another_of_its_number() {
    // Goal: a stretch the room kept the sums of, replaced by another object of the same stretch
    // number with other readings, is added up again. Method: choose over the stretches of a
    // View, then over the same stretches with the first replaced by the stretch of the same
    // number of a View of other readings, with the reused room and with a fresh one.
    let view = window_view();
    let other = sliding_view(100..=139);
    let mut reused = Leaders::new();
    let window = WallSeconds {
        first: 100,
        last: 139,
    };
    assert_chooses_as_fresh_leaders(&mut reused, &view.readings(), window);
    let mut stretches = view.stretches.clone();
    assert_eq!(stretches[0].stretch, other.stretches[0].stretch);
    stretches[0] = Arc::clone(&other.stretches[0]);

    let replaced = Readings {
        stretches: &stretches,
        ..view.readings()
    };
    assert_ne!(
        Leaders::new().sums_over(&replaced, window),
        Leaders::new().sums_over(&view.readings(), window)
    );

    assert_chooses_as_fresh_leaders(&mut reused, &replaced, window);
}

#[test]
fn leaders_keep_nothing_of_a_window_whose_stretches_hold_more_apps_than_they_add_up() {
    // Goal: where the stretches a window holds wholly come to more Apps than the room adds up,
    // it chooses none and keeps nothing, so its next choice is a fresh room's. Method: in a View
    // whose thirteen stretches each hold 2,048 Apps no other holds, choose the window of the
    // oldest six, then the window of all thirteen, which begins with the six it kept, then the
    // six again.
    let view = crowded_view();
    let readings = view.readings();
    let mut reused = Leaders::new();
    assert_chooses_as_fresh_leaders(&mut reused, &readings, crowded_window(6));
    let mut crowded = Cohorts::with_room();

    reused.choose(&readings, crowded_window(13), &mut crowded);

    assert!(crowded.refused);
    assert_chooses_as_fresh_leaders(&mut reused, &readings, crowded_window(6));
}

#[test]
fn leaders_keep_nothing_of_a_window_whose_seconds_one_by_one_hold_more_apps_than_they_add_up() {
    // Goal: where the stretches a window holds wholly fit in the room but its other seconds take
    // the Apps past what it adds up, it chooses none and keeps no App's name, so its next choice
    // is a fresh room's. Method: in a `crowded_view`, choose the window of the oldest eleven
    // stretches and the first second of the twelfth, whose 2,048 Apps take the 22,528 of the
    // eleven past 24,000, compare the references to an App of the oldest stretch with those
    // before the choice, then choose the window of the oldest six.
    let view = crowded_view();
    let readings = view.readings();
    let name = view.stretches[0].apps().unwrap()[0].ranked();
    let references = Arc::strong_count(&name);
    let mut reused = Leaders::new();
    let mut crowded = Cohorts::with_room();
    let window = WallSeconds {
        first: 1_000,
        last: 1_110,
    };

    reused.choose(&readings, window, &mut crowded);

    assert!(crowded.refused);
    assert_eq!(Arc::strong_count(&name), references);
    assert_chooses_as_fresh_leaders(&mut reused, &readings, crowded_window(6));
}

#[test]
fn leaders_choose_a_window_the_reader_picks_over_the_view_they_kept_the_sums_of() {
    // Goal: a window the reader picks over the same View, holding other stretches wholly than
    // the window followed, is chosen as a fresh room chooses it, and so is the followed window
    // after it. Method: choose the newest 30 seconds of a View of 100 to 175, then windows that
    // end earlier, start earlier, and lie inside it, then the newest 30 seconds again.
    let view = sliding_view(100..=175);
    let mut reused = Leaders::new();
    let readings = view.readings();
    for (first, last) in [(146, 175), (120, 159), (100, 175), (131, 148), (146, 175)] {
        let window = WallSeconds { first, last };

        assert_chooses_as_fresh_leaders(&mut reused, &readings, window);
    }
}

#[test]
fn leaders_take_no_reference_to_the_stretches_they_choose_over() {
    // Goal: the room a chart's leaders are chosen in holds no reference to a stretch, so the
    // store's pool writes and clears a stretch's buffer once its Views let go of it, as it does
    // where no room chose over it, and a closed stretch that takes its owed Apps finds a free
    // buffer. Method: a store of two closed stretches over three buffers takes stretches 100
    // and 101, a room chooses over them, and the counts of references to each stretch are
    // compared before and after. Then the View is let go of, stretch 102 closes in the third
    // buffer, so stretch 100's buffer is the only one the span does not hold, and stretch 102
    // takes owed Apps, which writes that buffer.
    let mut held = Held::without_recording();
    held.stretches = Stretches::new(2, 1);
    for wall_second in 1_000..=1_020 {
        take(&mut held, wall_second, &[cpu(b"/A.app", wall_second)]);
    }
    let view = published(&mut held);
    assert_eq!(view.stretches.len(), 2);
    let counts = reference_counts(&view.stretches);
    let window = WallSeconds {
        first: 1_000,
        last: 1_019,
    };

    let mut leaders = Leaders::new();

    let chosen = chosen_over(&mut leaders, &view.readings(), window);

    assert_eq!(chosen.cpu, ["/A.app"]);
    assert_eq!(reference_counts(&view.stretches), counts);
    drop(view);
    held.release_views();
    for wall_second in 1_021..=1_030 {
        take(&mut held, wall_second, &[cpu(b"/A.app", wall_second)]);
    }
    let mut folding = Folding::new();
    assert!(
        folding
            .add(102, &second_with(1_020, &[cpu(b"/B.app", 1_000)]))
            .is_none()
    );
    // `add_owed` asserts that it finds a free buffer, which a reference the living room held to
    // stretch 100 would take, so the room stays alive past it, and the drop shows where it ends.
    held.stretches
        .add_owed(&RUN, 1_020, &folding.finish().unwrap());
    drop(leaders);
}

/// The strong and the weak references to each of `stretches`, in their order.
fn reference_counts(stretches: &[Arc<AppsStretch>]) -> Vec<(usize, usize)> {
    stretches
        .iter()
        .map(|stretch| (Arc::strong_count(stretch), Arc::weak_count(stretch)))
        .collect()
}

#[test]
fn leaders_add_up_again_a_stretch_written_again_in_its_own_buffer() {
    // Goal: a stretch the store's pool writes again in the buffer of a stretch the room kept
    // the sums of, with the same address and stretch number but other readings, is added up
    // again. Method: a store of one closed stretch over two buffers closes stretch 100 of A in
    // its first buffer, a room chooses over it, the store resets and closes stretch 100 of C in
    // the second buffer, resets again and closes stretch 100 of B in the first buffer, and the
    // reused room chooses over that.
    let mut held = Held::without_recording();
    held.stretches = Stretches::new(1, 1);
    let window = WallSeconds {
        first: 1_000,
        last: 1_009,
    };
    let mut reused = Leaders::new();
    let first = published_stretch_of(&mut held, b"/A.app");
    assert_chooses_as_fresh_leaders(&mut reused, &first.readings(), window);
    let address = Arc::as_ptr(&first.stretches[0]);
    drop(first);
    held.reset();
    drop(published_stretch_of(&mut held, b"/C.app"));
    held.reset();

    let again = published_stretch_of(&mut held, b"/B.app");

    assert!(std::ptr::eq(Arc::as_ptr(&again.stretches[0]), address));
    assert_eq!(again.stretches[0].stretch, 100);
    assert_chooses_as_fresh_leaders(&mut reused, &again.readings(), window);
}

/// The View `held` publishes after it takes seconds 1,000 to 1,010, each naming the App `key`,
/// which closes stretch 100.
fn published_stretch_of(held: &mut Held, key: &'static [u8]) -> Arc<View> {
    for wall_second in 1_000..=1_010 {
        take(held, wall_second, &[cpu(key, wall_second)]);
    }
    let view = published(held);
    assert_eq!(view.stretches.len(), 1);
    view
}

#[test]
fn leaders_add_up_again_a_stretch_that_took_its_owed_apps() {
    // Goal: a closed stretch the room kept the sums of, which the store replaces with one that
    // adds the Apps a load owed it, is added up again. Method: close stretch 100 of seconds
    // that hold no Apps, choose over it, hand it the Apps a load added up, and choose over it
    // again with the reused room.
    let mut held = Held::without_recording();
    take_mac_readings(&mut held, 1_000..1_011, 95.0);
    let window = WallSeconds {
        first: 1_000,
        last: 1_009,
    };
    let mut reused = Leaders::new();
    let owing = published(&mut held);
    assert_chooses_as_fresh_leaders(&mut reused, &owing.readings(), window);
    drop(owing);
    let mut folding = Folding::new();
    assert!(
        folding
            .add(100, &second_with(1_000, &[cpu(b"/A.app", 1_000)]))
            .is_none()
    );
    held.stretches
        .add_owed(&RUN, 1_000, &folding.finish().unwrap());

    let owed = published(&mut held);

    assert_chooses_as_fresh_leaders(&mut reused, &owed.readings(), window);
    assert_eq!(
        chosen_over(&mut reused, &owed.readings(), window).cpu,
        ["/A.app"]
    );
}

#[test]
fn leaders_reused_past_a_stretch_of_more_apps_than_it_holds_choose_as_fresh_leaders() {
    // Goal: a room whose kept stretches include one that holds no Apps, since its seconds came
    // to more than it holds, chooses as a fresh room does once another stretch closes. Method:
    // stretch 100 holds too many Apps and stretch 101 holds A, a reused room chooses the window
    // of the two, then stretch 102 of A closes and the room chooses the window of all three.
    let mut held = Held::without_recording();
    take_more_apps_than_a_stretch_holds(&mut held, 1_000);
    for wall_second in 1_010..=1_020 {
        take(&mut held, wall_second, &[cpu(b"/A.app", wall_second)]);
    }
    let two = published(&mut held);
    assert!(two.stretches[0].apps().is_none());
    let mut reused = Leaders::new();
    let window = WallSeconds {
        first: 1_000,
        last: 1_019,
    };
    assert_chooses_as_fresh_leaders(&mut reused, &two.readings(), window);
    for wall_second in 1_021..=1_030 {
        take(&mut held, wall_second, &[cpu(b"/A.app", wall_second)]);
    }

    let three = published(&mut held);

    assert_eq!(three.stretches.len(), 3);
    let window = WallSeconds {
        first: 1_000,
        last: 1_029,
    };
    assert_chooses_as_fresh_leaders(&mut reused, &three.readings(), window);
}

#[test]
fn a_room_resumes_the_sums_of_the_stretches_a_window_begins_with() {
    // Goal: a room resumes the sums it kept where the window's stretches begin with the kept
    // closing numbers, counting those. Method: keep the numbers of the oldest two of a View's
    // stretches and compare them with all of its stretches.
    let view = sliding_view(100..=139);
    let kept = closing_numbers(&view.stretches[..2]);

    let count = resume_count(&kept, &view.stretches);

    assert_eq!(count, Some(2));
}

#[test]
fn a_room_resumes_no_sums_of_more_stretches_than_a_window_holds() {
    // Goal: a room resumes nothing where it kept more stretches than the window holds wholly.
    // Method: keep the numbers of all of a View's stretches and compare them with all but the
    // newest.
    let view = sliding_view(100..=139);
    let kept = closing_numbers(&view.stretches);

    let count = resume_count(&kept, &view.stretches[..2]);

    assert_eq!(count, None);
}

#[test]
fn a_room_resumes_no_sums_of_a_stretch_closed_again() {
    // Goal: a room resumes nothing where a stretch the window begins with took another closing
    // number than the one kept. Method: keep the number of the oldest stretch of one View and
    // compare it with the stretches of another View of the same seconds.
    let view = sliding_view(100..=139);
    let again = sliding_view(100..=139);
    let kept = closing_numbers(&view.stretches[..1]);

    let count = resume_count(&kept, &again.stretches);

    assert_eq!(count, None);
}

#[test]
fn a_room_that_kept_nothing_resumes_no_stretch() {
    // Goal: a room that kept no closing number resumes none of the window's stretches, so it
    // adds up all of them. Method: compare no number with a View's stretches.
    let view = sliding_view(100..=139);

    let count = resume_count(&[], &view.stretches);

    assert_eq!(count, Some(0));
}

/// The closing number of each of `stretches`, in their order.
fn closing_numbers(stretches: &[Arc<AppsStretch>]) -> Vec<u64> {
    stretches
        .iter()
        .map(|stretch| stretch.closing_number())
        .collect()
}

/// Asserts that `reused` chooses over `window` of `readings` the sums and the cohorts a fresh
/// room chooses.
fn assert_chooses_as_fresh_leaders(
    reused: &mut Leaders,
    readings: &Readings<'_>,
    window: WallSeconds,
) {
    let sums = reused.sums_over(readings, window);
    let chosen = chosen_over(reused, readings, window);

    let expected = Leaders::new().sums_over(readings, window);
    let fresh = chosen_over(&mut Leaders::new(), readings, window);
    assert!(expected.is_some());
    assert_eq!(sums, expected, "{window:?}");
    assert_eq!(chosen, fresh, "{window:?}");
}

/// How many of the stretches of `previous` are the very objects `view` holds.
fn shared_stretches(previous: &View, view: &View) -> usize {
    previous
        .stretches
        .iter()
        .filter(|stretch| view.stretches.iter().any(|held| Arc::ptr_eq(held, stretch)))
        .count()
}

/// The View of the seconds of `wall_seconds`, each using what `sliding_used` names.
fn sliding_view(wall_seconds: std::ops::RangeInclusive<u64>) -> Arc<View> {
    let mut held = Held::without_recording();
    for wall_second in wall_seconds {
        take(&mut held, wall_second, &sliding_used(wall_second));
    }

    published(&mut held)
}

/// What each App uses at `wall_second`, which differs from second to second so that each
/// window's sums differ: A at even seconds, B at every third, C at every second, D at every
/// seventh, and E at the seconds ending in 4, each in its own amounts of every resource.
fn sliding_used(wall_second: u64) -> Vec<Used<'static>> {
    let keys: [(&[u8], u64); 5] = [
        (b"/A.app", 2),
        (b"/B.app", 3),
        (b"/C.app", 1),
        (b"/D.app", 7),
        (b"/E.app", 10),
    ];
    (0_u64..)
        .zip(keys)
        .filter(|(index, (_, every))| wall_second % every == *index % *every)
        .map(|(index, (key, _))| {
            let amount = wall_second * 13 + index * 1_000;
            Used {
                key,
                cpu_ns: amount,
                energy_nj: Some(amount * 3),
                gpu_ns: Some(amount * 5),
                disk_io_bytes: Some(amount * 7),
                resident_bytes: amount * 11,
            }
        })
        .collect()
}

/// A View of seconds 1,000 to 1,130 whose first second of each ten, from 1,000 to 1,120, holds
/// 2,048 Apps no other second holds, as many as a stretch holds, and whose other seconds hold
/// none, so that its thirteen closed stretches hold more Apps than a window adds up.
fn crowded_view() -> Arc<View> {
    let mut held = Held::without_recording();
    for wall_second in 1_000..=1_130 {
        let crowded = wall_second % 10 == 0 && wall_second < 1_130;
        let keys: Vec<Vec<u8>> = if crowded {
            (0..APPS_PER_SECOND_MAX)
                .map(|index| format!("/{wall_second}-{index:04}.app").into_bytes())
                .collect()
        } else {
            Vec::new()
        };
        let used: Vec<_> = keys.iter().map(|key| cpu(key, 1_000)).collect();
        take(&mut held, wall_second, &used);
    }

    published(&mut held)
}

/// The window of a `crowded_view` that holds its oldest `stretches` stretches wholly.
fn crowded_window(stretches: u64) -> WallSeconds {
    WallSeconds {
        first: 1_000,
        last: 1_000 + stretches * 10 - 1,
    }
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

/// A store over `fixture`'s recording that starts as the app's does.
fn store_with_budget(fixture: &Fixture) -> super::Store {
    let budget = Arc::new(super::ByteBudget::new(super::BYTES_MAX));

    super::Store::start_with_budget(&fixture.root, budget).unwrap()
}

/// The View of a store that loaded `fixture`'s recording.
fn loaded(fixture: &Fixture) -> Arc<View> {
    let mut held = Held::without_recording();
    load_into(&mut held, fixture);

    published(&mut held)
}

/// Loads `fixture`'s recording into `held`, as the store does as it reads the whole hour again.
fn load_into(held: &mut Held, fixture: &Fixture) {
    let budget = Arc::clone(&held.payload_budget);
    let operation = Arc::new(crate::cancellation::Cancellation::default())
        .operation(0)
        .unwrap();
    let outcome = recording::load_payload_hour(
        &fixture.root,
        &budget,
        &operation,
        None,
        |run, second, apps| {
            assert!(held.take(run, second, apps));
        },
    )
    .unwrap();
    assert_eq!(outcome.left_out, None);
}

/// Starts a store on `fixture`'s recording in `held`, as a window's Store starts, which leaves
/// the Apps of the recording's sealed minutes owed to its stretches.
fn launch(fixture: &Fixture, held: &mut Held) -> super::Ring {
    let shared = super::Shared::starting();
    let mut ring = super::make_ring(&fixture.root, &held.payload_budget);
    let operation = Arc::clone(&shared.cancellation).operation(0).unwrap();

    super::startup_staged(&fixture.root, held, &mut ring, &shared, &operation).unwrap();

    ring
}

/// Asserts that a store that launches on `fixture`'s recording, then adds up the Apps it owes
/// its stretches, holds the stretches a store that loads the whole recording at once holds.
fn assert_owed_apps_add_up(fixture: &Fixture) {
    let whole = loaded(fixture);
    assert!(!whole.stretches.is_empty());
    for expected in &whole.stretches {
        assert!(expected.holds_apps());
    }
    let mut held = Held::with_packing_thread(Arc::new(super::ByteBudget::new(super::BYTES_MAX)));
    launch(fixture, &mut held);
    let owed = held.owed.take().unwrap();

    super::run_owed_add(&fixture.root, &mut held, &owed);

    assert_eq!(held.left_out, None);
    assert_stretches_match(&published(&mut held), &whole, "after the launch");
    assert_owed_ranks_add_up(fixture);
}

/// Asserts that a store whose live span holds two seconds, which launches on `fixture`'s
/// recording and then adds up the Apps it owes, holds the sampled seconds and their rankings
/// that such a store holds once it loads the whole recording at once.
fn assert_owed_ranks_add_up(fixture: &Fixture) {
    let mut whole = Held::without_recording();
    hold_two_live_seconds(&mut whole);
    load_into(&mut whole, fixture);
    let whole = published(&mut whole);
    let mut held = Held::with_packing_thread(Arc::new(super::ByteBudget::new(super::BYTES_MAX)));
    hold_two_live_seconds(&mut held);
    launch(fixture, &mut held);
    let owed = held.owed.take().unwrap();

    super::run_owed_add(&fixture.root, &mut held, &owed);

    let view = published(&mut held);
    assert!(!whole.sampled.is_empty());
    assert_eq!(view.sampled.len(), whole.sampled.len());
    for (shown, expected) in view.sampled.iter().zip(&whole.sampled) {
        assert_eq!(shown.observation, expected.observation);
        assert_eq!(shown.ranks(), expected.ranks());
    }
}

/// Shrinks `held`'s live span to two seconds, so the older seconds of a short recording are
/// sampled.
fn hold_two_live_seconds(held: &mut Held) {
    held.span = super::Span::new(2, super::Buffers::new(3, MachineSecond::default));
    let buffers = super::Buffers::new(
        2 + super::block::BLOCK_DEPARTED_MAX + 1,
        AppsSecond::default,
    );
    held.apps =
        super::block::BlockSpan::with_packing_thread(2, buffers, Arc::clone(&held.payload_budget));
}

/// Asserts that `view` holds the stretches `whole` holds, as each of Overview's App charts
/// draws them, naming `context` where one differs.
fn assert_stretches_match(view: &View, whole: &View, context: &str) {
    let cohort = cohort_of(whole);
    assert_eq!(view.stretches.len(), whole.stretches.len(), "{context}");
    for (shown, expected) in view.stretches.iter().zip(&whole.stretches) {
        assert_eq!(shown.stretch, expected.stretch, "{context}");
        assert_eq!(shown.elapsed_ns(), expected.elapsed_ns(), "{context}");
        assert_eq!(shown.machine, expected.machine, "{context}");
        assert_eq!(
            resident_measured_of(shown),
            resident_measured_of(expected),
            "{context}"
        );
        assert_eq!(
            readings_of(shown, &cohort),
            readings_of(expected, &cohort),
            "{context}"
        );
    }
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
    let mut held = Held::without_recording();
    for wall_second in 100..=139 {
        take(&mut held, wall_second, &window_used(wall_second));
    }

    published(&mut held)
}

/// How many of a `window_view`'s oldest seconds, 100 to 124, `window_from_stretches` reads
/// only through the stretches that hold them.
const STRETCHED_SECONDS_COUNT: usize = 25;

/// The readings of `view`, a `window_view`, that hold seconds 125 to 139 one by one and the
/// stretches of 100 to 129.
fn window_from_stretches<'view>(view: &'view View, apps: &'view AppsSpan) -> Readings<'view> {
    assert_eq!(apps.len(), view.apps.len() - STRETCHED_SECONDS_COUNT);
    Readings {
        seconds: &view.seconds[STRETCHED_SECONDS_COUNT..],
        apps,
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
            gpu_ns: None,
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

/// The Apps the App charts of CPU time, energy, disk bytes, GPU time and resident memory draw on
/// lines of their own over a window of readings, by name, and the seconds they draw one by one.
#[derive(Debug, PartialEq)]
struct Chosen {
    cpu: Vec<String>,
    power: Vec<String>,
    disk: Vec<String>,
    gpu: Vec<String>,
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
        gpu: names(cohorts.of(AppAttributionMetric::Gpu)),
        resident: names(&cohorts.resident),
        seconds: cohorts.seconds,
    }
}

/// What one App used over one second, with no energy, GPU time or disk bytes measured where it
/// names none.
pub(super) struct Used<'key> {
    key: &'key [u8],
    cpu_ns: u64,
    energy_nj: Option<u64>,
    gpu_ns: Option<u64>,
    disk_io_bytes: Option<u64>,
    resident_bytes: u64,
}

fn cpu(key: &[u8], cpu_ns: u64) -> Used<'_> {
    Used {
        key,
        cpu_ns,
        energy_nj: None,
        gpu_ns: None,
        disk_io_bytes: None,
        resident_bytes: 0,
    }
}

fn resident(key: &[u8], resident_bytes: u64) -> Used<'_> {
    Used {
        key,
        cpu_ns: 0,
        energy_nj: None,
        gpu_ns: None,
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

/// The amount the first App of `shown` used, in the unit of the metric `shown` charts.
fn first_app_amount(shown: &super::Attribution) -> u64 {
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
    let apps: Vec<_> = used.iter().map(app).collect();
    second_of_apps(wall_second, &apps)
}

/// The Apps `apps` at `wall_second`, in key order, measured over one second.
fn second_of_apps(wall_second: u64, apps: &[App]) -> AppsSecond {
    let budget = Arc::new(super::ByteBudget::new(1024 * 1024 * 1024));
    let mut rows = AppRows::default();
    rows.replace_admitted(apps.len(), &budget).unwrap();
    let mut second = AppsSecond {
        apps: rows,
        observation: Some(super::take_observation(&RUN, &second_at(wall_second))),
        missing: None,
        elapsed_ns: Some(NANOSECONDS_PER_SECOND),
        membership_complete: true,
        ..AppsSecond::default()
    };
    for app in apps {
        let key = app.key.bytes.clone();
        second.apps.push(AppRow::new(app, name(&key)));
    }
    second.totals = super::Totals::of(&second.apps);

    second
}

/// The App `used` names, one member that measured the CPU time, resident memory, energy, GPU time
/// and disk bytes it names and nothing else.
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
        gpu_ns: used.gpu_ns.map_or(unmeasured, measured),
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

/// Takes the seconds of `wall_seconds` into `held`, each reading `cpu_percent` and no Apps.
fn take_mac_readings(held: &mut Held, wall_seconds: Range<u64>, cpu_percent: f64) {
    for wall_second in wall_seconds {
        let mut second = second_at(wall_second);
        second.elapsed_ns = Some(NANOSECONDS_PER_SECOND);
        second.cpu_percent = Some(cpu_percent);
        assert!(held.take(&RUN, &second, TickApps::Missing(&Missing::Unread)));
    }
}

#[test]
fn a_stretch_reads_nothing_of_the_stretches_let_go_before_it() {
    // Goal: the buffer a stretch was let go from leaves none of its readings of the Mac in the
    // next stretch it holds, so a chart marks no peak the store no longer holds.
    // Method: stretches reading 95, a reset of the store, stretches reading 10, and how far the
    // first of those reached.
    let mut held = Held::without_recording();
    take_mac_readings(&mut held, 1_000..1_021, 95.0);

    held.reset();
    take_mac_readings(&mut held, 2_000..2_021, 10.0);

    let maximum = published(&mut held).stretches[0]
        .machine
        .cpu_percent
        .measured
        .unwrap()
        .maximum;
    assert!((maximum.value - 10.0).abs() < 0.01);
}

#[test]
fn a_stretch_that_takes_its_owed_apps_keeps_what_it_read_of_the_mac() {
    // Goal: the Apps a load owed a closed stretch cost it none of what it read of the Mac, which
    // a chart marks the stretch's peak from.
    // Method: close a stretch that read 95 and no Apps, hand it the Apps a load added up, and
    // read both.
    let mut held = Held::without_recording();
    take_mac_readings(&mut held, 1_000..1_011, 95.0);
    let mut folding = Folding::new();
    assert!(
        folding
            .add(100, &second_with(1_000, &[cpu(b"/A.app", 1_000)]))
            .is_none()
    );
    let folded = folding.finish().unwrap();

    held.stretches.add_owed(&RUN, 1_000, &folded);

    let view = published(&mut held);
    let shown = view.stretches[0].attribution(AppAttributionMetric::Cpu, &[name(b"/A.app")]);
    let maximum = view.stretches[0]
        .machine
        .cpu_percent
        .measured
        .unwrap()
        .maximum;
    assert_eq!(first_app_amount(&shown), 1_000);
    assert!((maximum.value - 95.0).abs() < 0.01);
}

/// Seconds 1,000 to 1,010, each following the one before, with `reading` writing into each what
/// the second at `wall_second` read.
fn stretch_of_mac_readings(reading: impl Fn(&mut MachineSecond, u64)) -> Arc<AppsStretch> {
    let mut held = Held::without_recording();
    for wall_second in 1_000..=1_010 {
        let mut second = second_at(wall_second);
        second.elapsed_ns = Some(NANOSECONDS_PER_SECOND);
        reading(&mut second, wall_second);
        assert!(held.take(&RUN, &second, TickApps::Missing(&Missing::Unread)));
    }

    Arc::clone(&published(&mut held).stretches[0])
}

#[test]
fn a_closed_stretch_finds_a_second_long_peak_of_the_mac_at_every_offset() {
    // Goal: a reading that lasted one second stays findable once a chart draws one reading of its
    // stretch, wherever in the stretch it fell, and names the reading it came from.
    // Method: for each second of a stretch, read a stretch whose CPU peaks at that second alone,
    // and compare its peak with the reading.
    for offset in 0..10 {
        let peak = 1_000 + offset;
        let stretch = stretch_of_mac_readings(|second, wall_second| {
            second.cpu_percent = Some(if wall_second == peak { 95.0 } else { 10.0 });
        });

        let maximum = stretch.machine.cpu_percent.measured.unwrap().maximum;

        assert!((maximum.value - 95.0).abs() < 0.01, "peak at {peak}");
        assert_eq!(
            maximum.place,
            MachinePlace {
                boot: RUN.boot.0,
                run: RUN.run.0,
                publication: peak,
                wall_ns: peak * NANOSECONDS_PER_SECOND,
            },
            "peak at {peak}",
        );
    }
}

#[test]
fn a_peak_keeps_its_place_on_either_side_of_the_seconds_a_store_holds_whole() {
    // Goal: a reading that lasted one second stays findable as it leaves the seconds a store holds
    // whole, wherever in its stretch it fell, and names its reading once no second shows it.
    // Method: take a span past those seconds with the CPU peaking at one second, for each second
    // around where the held seconds begin, and read that second's stretch.
    let newest = 1_020 + u64::try_from(SECONDS_MAX).unwrap();
    let first_held = newest - u64::try_from(SECONDS_MAX).unwrap() + 1;
    for peak in first_held - 10..first_held + 10 {
        let mut held = Held::without_recording();
        for wall_second in 1_000..=newest {
            let mut second = second_at(wall_second);
            second.elapsed_ns = Some(NANOSECONDS_PER_SECOND);
            second.cpu_percent = Some(if wall_second == peak { 95.0 } else { 10.0 });
            assert!(held.take(&RUN, &second, TickApps::Missing(&Missing::Unread)));
        }

        let view = published(&mut held);

        assert_eq!(view.seconds[0].wall_ns, first_held * NANOSECONDS_PER_SECOND);
        let stretch = view
            .stretches
            .iter()
            .find(|stretch| stretch.stretch == peak / 10)
            .unwrap();
        let maximum = stretch.machine.cpu_percent.measured.unwrap().maximum;
        assert!((maximum.value - 95.0).abs() < 0.01, "peak at {peak}");
        assert_eq!(maximum.place.publication, peak, "peak at {peak}");
    }
}

#[test]
fn a_closed_stretch_adds_up_what_the_memory_chart_stacks_at_each_reading() {
    // Goal: the highest stack a stretch shows is one the Mac reached, and not the most of each
    // part added up from different seconds.
    // Method: read a stretch where wired memory peaks at one second and App memory at another.
    let memory = |app_bytes, wired_bytes| MemorySecond {
        total_bytes: 1_000,
        app_bytes: Some(app_bytes),
        wired_bytes,
        compressed_bytes: 10,
        pressure: collector::machine::Pressure::Normal,
        swap_used_bytes: 0,
        swap_total_bytes: 0,
    };
    let stretch = stretch_of_mac_readings(|second, wall_second| {
        second.memory = Some(match wall_second {
            1_002 => memory(500, 100),
            1_004 => memory(100, 500),
            _ => memory(100, 100),
        });
    });

    let used = stretch.machine.memory_used_bytes.measured.unwrap();

    assert!((used.maximum.value - 610.0).abs() < 0.01);
    assert!((used.minimum.value - 210.0).abs() < 0.01);
}

#[test]
fn a_stretch_after_a_peak_reaches_only_its_own_readings() {
    // Goal: closing a stretch leaves nothing of it in the next one, so a chart marks a peak once.
    // Method: two stretches with a peak in the first, and read what the second reached.
    let mut held = Held::without_recording();
    for wall_second in 1_000..=1_020 {
        let mut second = second_at(wall_second);
        second.elapsed_ns = Some(NANOSECONDS_PER_SECOND);
        second.cpu_percent = Some(if wall_second == 1_003 { 95.0 } else { 10.0 });
        assert!(held.take(&RUN, &second, TickApps::Missing(&Missing::Unread)));
    }

    let view = published(&mut held);

    let later = view.stretches[1].machine.cpu_percent.measured.unwrap();
    assert!((later.maximum.value - 10.0).abs() < 0.01);
}

#[test]
fn a_closed_stretch_keeps_the_thermal_states_it_changed_through() {
    // Goal: a thermal state of one second stays visible, with no mean of states standing for it.
    // Method: read a stretch of one second at a higher state among seconds at a lower one.
    let stretch = stretch_of_mac_readings(|second, wall_second| {
        second.thermal_pressure = Some(if wall_second == 1_003 { 2 } else { 0 });
    });

    let thermal = &stretch.machine.thermal_pressure;

    let changes: Vec<_> = thermal
        .changes()
        .map(|change| (change.category, change.place.publication))
        .collect();
    assert_eq!(changes, [(2, 1_003), (0, 1_004)]);
    assert_eq!(thermal.highest.map(|mark| mark.category), Some(2));
}

#[test]
fn a_closed_stretch_weighs_each_rate_by_its_interval() {
    // Goal: a late tick's rate counts for the time it covered, as the counter behind it grew.
    // Method: read a stretch whose one high rate covers three seconds.
    let stretch = stretch_of_mac_readings(|second, wall_second| {
        let late = wall_second == 1_005;
        second.elapsed_ns = Some(if late { 3 } else { 1 } * NANOSECONDS_PER_SECOND);
        second.disk_read_bps = Some(if late { 120.0 } else { 0.0 });
    });

    let read = &stretch.machine.disk_read_bps;

    assert_eq!(
        read.covered_ns(),
        Some(12 * i128::from(NANOSECONDS_PER_SECOND))
    );
    assert!((read.mean().unwrap() - 30.0).abs() < 0.01);
}

#[test]
fn a_row_of_an_app_at_every_second_answers_what_they_add_up_to() {
    // Goal: an App's row of a stretch answers each reader what the App used over the stretch's
    // seconds. Method: A uses CPU, energy, disk and memory at each of ten seconds and no GPU,
    // and every reader of its row is read.
    let stretch = stretch_of_one_app(&regular(b"/A.app"));

    assert_eq!(
        answers_of(&stretch, b"/A.app"),
        RowAnswers {
            measures: [
                measured(10_000),
                measured(20_000),
                measured(30_000),
                GPU_UNMEASURED,
                measured(4_096),
            ],
            rates_measured: [
                Some((10_000, TEN_SECONDS_NS)),
                Some((20_000, TEN_SECONDS_NS)),
                Some((30_000, TEN_SECONDS_NS)),
                None,
            ],
            resident_measured: Some((40_960, 10)),
            membership_complete: true,
        }
    );
}

#[test]
fn a_row_of_an_app_that_left_within_the_stretch_answers_what_its_own_seconds_add_up_to() {
    // Goal: an App held at only some of a stretch's seconds answers what it used at those, and
    // its memory over all of them. Method: A is held at each of ten seconds and B at the first
    // five, and every reader of B's row is read.
    let seconds: Vec<_> = (1_000..1_010)
        .map(|wall_second| {
            let a = app(&regular(b"/A.app"));
            if wall_second < 1_005 {
                apps_at(wall_second, &[a, app(&regular(b"/B.app"))])
            } else {
                apps_at(wall_second, &[a])
            }
        })
        .collect();

    let stretch = AppsStretch::of_seconds(100, &seconds);

    assert_eq!(
        answers_of(&stretch, b"/B.app"),
        RowAnswers {
            measures: [
                measured(5_000),
                measured(10_000),
                measured(15_000),
                GPU_UNMEASURED,
                measured(2_048),
            ],
            rates_measured: [
                Some((5_000, 5 * NANOSECONDS_PER_SECOND)),
                Some((10_000, 5 * NANOSECONDS_PER_SECOND)),
                Some((15_000, 5 * NANOSECONDS_PER_SECOND)),
                None,
            ],
            resident_measured: Some((20_480, 5)),
            membership_complete: true,
        }
    );
}

#[test]
fn a_row_with_members_that_did_not_measure_a_rate_answers_how_many() {
    // Goal: a row keeps the members of its App that did not measure a resource. Method: A's
    // CPU time counts two members that did not measure it at each of ten seconds, and every
    // reader of its row is read.
    let cpu_ns = Measure {
        unavailable_members: 2,
        ..measured(1_000)
    };
    let seconds: Vec<_> = (1_000..1_010)
        .map(|wall_second| {
            let a = App {
                cpu_ns,
                ..app(&regular(b"/A.app"))
            };
            apps_at(wall_second, &[a])
        })
        .collect();

    let stretch = AppsStretch::of_seconds(100, &seconds);

    assert_eq!(
        answers_of(&stretch, b"/A.app"),
        RowAnswers {
            measures: [
                Measure {
                    unavailable_members: 2,
                    ..measured(10_000)
                },
                measured(20_000),
                measured(30_000),
                GPU_UNMEASURED,
                measured(4_096),
            ],
            rates_measured: [
                Some((10_000, TEN_SECONDS_NS)),
                Some((20_000, TEN_SECONDS_NS)),
                Some((30_000, TEN_SECONDS_NS)),
                None,
            ],
            resident_measured: Some((40_960, 10)),
            membership_complete: true,
        }
    );
}

#[test]
fn a_row_whose_measure_overflowed_answers_no_rate_of_it() {
    // Goal: a resource whose measure overflowed at one second gives the App no rate of it over
    // the stretch. Method: A's CPU time overflows at the fourth of ten seconds, and every reader
    // of its row is read.
    let seconds: Vec<_> = (1_000..1_010)
        .map(|wall_second| {
            let mut a = app(&regular(b"/A.app"));
            if wall_second == 1_003 {
                a.cpu_ns.overflowed = true;
            }
            apps_at(wall_second, &[a])
        })
        .collect();

    let stretch = AppsStretch::of_seconds(100, &seconds);

    assert_eq!(
        answers_of(&stretch, b"/A.app"),
        RowAnswers {
            measures: [
                Measure {
                    overflowed: true,
                    ..measured(9_000)
                },
                measured(20_000),
                measured(30_000),
                GPU_UNMEASURED,
                measured(4_096),
            ],
            rates_measured: [
                None,
                Some((20_000, TEN_SECONDS_NS)),
                Some((30_000, TEN_SECONDS_NS)),
                None,
            ],
            resident_measured: Some((40_960, 10)),
            membership_complete: true,
        }
    );
}

#[test]
fn a_row_whose_gpu_measured_answers_its_gpu_time() {
    // Goal: on a Mac whose GPU measures its Apps, a row answers the App's GPU time as it does
    // its other rates. Method: A uses the GPU too at each of ten seconds, and every reader of
    // its row is read.
    let stretch = stretch_of_one_app(&regular_on_the_gpu(b"/A.app"));

    assert_eq!(
        answers_of(&stretch, b"/A.app"),
        RowAnswers {
            measures: [
                measured(10_000),
                measured(20_000),
                measured(30_000),
                measured(40_000),
                measured(4_096),
            ],
            rates_measured: [
                Some((10_000, TEN_SECONDS_NS)),
                Some((20_000, TEN_SECONDS_NS)),
                Some((30_000, TEN_SECONDS_NS)),
                Some((40_000, TEN_SECONDS_NS)),
            ],
            resident_measured: Some((40_960, 10)),
            membership_complete: true,
        }
    );
}

#[test]
fn a_row_with_a_second_measured_over_no_interval_answers_the_rates_of_the_others() {
    // Goal: a second that measured its Apps over no interval, as a run's first does, adds the
    // App's memory but none of its rates. Method: the first of A's ten seconds measured over no
    // interval, and every reader of its row is read.
    let seconds: Vec<_> = (1_000..1_010)
        .map(|wall_second| {
            let mut second = apps_at(wall_second, &[app(&regular(b"/A.app"))]);
            if wall_second == 1_000 {
                Arc::get_mut(&mut second).unwrap().elapsed_ns = None;
            }
            second
        })
        .collect();

    let stretch = AppsStretch::of_seconds(100, &seconds);

    assert_eq!(
        answers_of(&stretch, b"/A.app"),
        RowAnswers {
            measures: [
                measured(9_000),
                measured(18_000),
                measured(27_000),
                GPU_UNMEASURED,
                measured(4_096),
            ],
            rates_measured: [
                Some((9_000, 9 * NANOSECONDS_PER_SECOND)),
                Some((18_000, 9 * NANOSECONDS_PER_SECOND)),
                Some((27_000, 9 * NANOSECONDS_PER_SECOND)),
                None,
            ],
            resident_measured: Some((40_960, 10)),
            membership_complete: true,
        }
    );
}

#[test]
fn a_row_added_up_from_two_stretches_answers_what_both_add_up_to() {
    // Goal: a row a load adds to the row of the same stretch the store took in answers what the
    // App used over both. Method: the store takes in the first five of ten seconds, A using a
    // microsecond of CPU at each, and adds a load's stretch of the other five, A using two at
    // each, and B only there, then closes it and reads every reader of A's and B's rows.
    let stretches = stretches_with_a_load_added();

    let stretch = stretches.held().next().unwrap();
    let answers = [
        answers_of(stretch, b"/A.app"),
        answers_of(stretch, b"/B.app"),
    ];
    let five_seconds_ns = 5 * NANOSECONDS_PER_SECOND;
    assert_eq!(
        answers,
        [
            RowAnswers {
                measures: [
                    measured(15_000),
                    measured(20_000),
                    measured(30_000),
                    GPU_UNMEASURED,
                    measured(4_096),
                ],
                rates_measured: [
                    Some((15_000, TEN_SECONDS_NS)),
                    Some((20_000, TEN_SECONDS_NS)),
                    Some((30_000, TEN_SECONDS_NS)),
                    None,
                ],
                resident_measured: Some((40_960, 10)),
                membership_complete: true,
            },
            RowAnswers {
                measures: [
                    measured(5_000),
                    measured(10_000),
                    measured(15_000),
                    GPU_UNMEASURED,
                    measured(2_048),
                ],
                rates_measured: [
                    Some((5_000, five_seconds_ns)),
                    Some((10_000, five_seconds_ns)),
                    Some((15_000, five_seconds_ns)),
                    None,
                ],
                resident_measured: Some((20_480, 5)),
                membership_complete: true,
            },
        ]
    );
}

#[test]
fn a_row_of_an_app_not_found_whole_at_a_second_answers_so() {
    // Goal: a row keeps that the App rule missed a member of the App at one of its seconds.
    // Method: A's membership is incomplete at the fourth of ten seconds, and every reader of its
    // row is read.
    let seconds: Vec<_> = (1_000..1_010)
        .map(|wall_second| {
            let mut a = app(&regular(b"/A.app"));
            a.membership_complete = wall_second != 1_003;
            apps_at(wall_second, &[a])
        })
        .collect();

    let stretch = AppsStretch::of_seconds(100, &seconds);

    assert_eq!(
        answers_of(&stretch, b"/A.app"),
        RowAnswers {
            measures: [
                measured(10_000),
                measured(20_000),
                measured(30_000),
                GPU_UNMEASURED,
                measured(4_096),
            ],
            rates_measured: [
                Some((10_000, TEN_SECONDS_NS)),
                Some((20_000, TEN_SECONDS_NS)),
                Some((30_000, TEN_SECONDS_NS)),
                None,
            ],
            resident_measured: Some((40_960, 10)),
            membership_complete: false,
        }
    );
}

const TEN_SECONDS_NS: u64 = 10 * NANOSECONDS_PER_SECOND;

/// What one member that did not measure the GPU adds up to over any seconds.
const GPU_UNMEASURED: Measure = Measure {
    known: 0,
    measured_members: 0,
    unavailable_members: 1,
    overflowed: false,
};

/// What every reader of an App's row of a stretch answers: its measure of CPU time, energy,
/// disk bytes, GPU time and resident memory, its rates as the chart's leaders take them, its
/// resident memory as they take it, and whether the App rule found it whole.
#[derive(Debug, PartialEq)]
struct RowAnswers {
    measures: [Measure; 5],
    rates_measured: [Option<(u64, u64)>; 4],
    resident_measured: Option<(u64, u32)>,
    membership_complete: bool,
}

/// What every reader answers of the row of the App `key` names in `stretch`.
fn answers_of(stretch: &AppsStretch, key: &[u8]) -> RowAnswers {
    let rows = stretch.apps().unwrap();
    let row = super::attribution::found_in(rows, &name(key)).unwrap();
    let rates = [Metric::Cpu, Metric::Power, Metric::Disk, Metric::Gpu];

    RowAnswers {
        measures: [
            Metric::Cpu,
            Metric::Power,
            Metric::Disk,
            Metric::Gpu,
            Metric::Resident,
        ]
        .map(|metric| row.measure(metric)),
        rates_measured: rates.map(|metric| row.rate_measured(metric)),
        resident_measured: row.resident_measured(),
        membership_complete: super::attribution::PointRow::membership_complete(row),
    }
}

/// What one member that measured `known` adds up to.
fn measured(known: u64) -> Measure {
    Measure {
        known,
        measured_members: 1,
        unavailable_members: 0,
        overflowed: false,
    }
}

/// One second's use of the App `key` names: a microsecond of CPU time, two of energy, three
/// kilobytes of disk and four kibibytes of memory, and no GPU time.
pub(super) fn regular(key: &[u8]) -> Used<'_> {
    Used {
        energy_nj: Some(2_000),
        disk_io_bytes: Some(3_000),
        resident_bytes: 4_096,
        ..cpu(key, 1_000)
    }
}

/// One second's use of the App `key` names as `regular`'s, and four microseconds of GPU time.
pub(super) fn regular_on_the_gpu(key: &[u8]) -> Used<'_> {
    Used {
        gpu_ns: Some(4_000),
        ..regular(key)
    }
}

/// The Apps `apps` at `wall_second`, in key order, measured over one second, as a stretch takes
/// them.
fn apps_at(wall_second: u64, apps: &[App]) -> Arc<AppsSecond> {
    Arc::new(second_of_apps(wall_second, apps))
}

/// The stretch of the ten seconds from 1,000, at each of which the one App `used` names uses
/// what it names.
pub(super) fn stretch_of_one_app(used: &Used<'_>) -> AppsStretch {
    let seconds: Vec<_> = (1_000..1_010)
        .map(|wall_second| apps_at(wall_second, &[app(used)]))
        .collect();

    AppsStretch::of_seconds(100, &seconds)
}

/// The stretch of the ten seconds from 1,000, of which the store takes in the first five, A
/// using a microsecond of CPU at each, and adds a load's stretch of the other five, A using two
/// at each, and B only there, before it closes the stretch.
pub(super) fn stretches_with_a_load_added() -> Stretches {
    let mut stretches = Stretches::new(1, 1);
    for wall_second in 1_000..1_005 {
        let apps = apps_at(wall_second, &[app(&regular(b"/A.app"))]);
        stretches.take(100, &apps, &RUN, &second_at(wall_second));
    }
    let folded: Vec<_> = (1_005..1_010)
        .map(|wall_second| {
            let a = Used {
                cpu_ns: 2_000,
                ..regular(b"/A.app")
            };
            apps_at(wall_second, &[app(&a), app(&regular(b"/B.app"))])
        })
        .collect();
    stretches.add_folded(&AppsStretch::of_seconds(100, &folded));

    let closing = apps_at(1_010, &[app(&regular(b"/A.app"))]);
    stretches.take(101, &closing, &RUN, &second_at(1_010));

    stretches
}
