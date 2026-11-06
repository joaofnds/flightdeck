use std::collections::BTreeSet;

use polars::prelude::Column;
use serde_json::{Value, json};

use super::super::sealed::fixture::{range, rewrite};
use super::*;
use crate::live::app_process_groups_history_tests::{named, usage};
use crate::live::identity_fixture::Fixture;
use crate::live_app_response::{PROCESSES_PAGE_ROWS_MAX, Page, Sample};
use crate::recording::apps::read_app_rows;
use crate::recording::{AppMinute, AppParts};
use persistence::app_tables::{program_code, state_code, unavailable_code};

#[test]
fn a_window_keeps_the_parts_the_replay_of_its_minute_gives_every_app() {
    // Goal: the part of each App a window keeps from its seal is the part the replay of its
    // minute gives, so a page shows the same detail from either.
    // Method: seal three observations with their Apps' parts, and compare the parts the window
    // keeps of each App with those a replay of its minute gives.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal_derived(&[], &persistence::sealed_apps::SealedApps::default());
    let session = Session::open(&fixture.root).unwrap();
    let replayed = replayed_samples(&session, 1002);
    let keys = app_keys(&replayed[2]);

    let kept: Vec<Vec<Shown>> = keys
        .iter()
        .map(|key| kept_parts(&session, 1002, key))
        .collect();

    assert_eq!(replayed.len(), 3);
    assert!(keys.len() > 1);
    assert_eq!(kept, replayed_parts(&replayed, &keys));
}

#[test]
fn a_window_keeps_every_app_of_each_observation_as_its_sample_gave_it() {
    // Goal: a window's App rows read back with no SQL as the facts and the Apps of each sample
    // the replay of its minute gives, so the store holds a sealed minute's Apps as it holds a
    // live second's.
    // Method: seal a window of two observations with its Apps' parts, read the table's App rows
    // back, and compare them with the rows the samples of a replay of its minute give.
    let mut fixture = Fixture::new();
    let session = sealed_seconds_with_parts(&mut fixture);
    let samples = replayed_samples(&session, 1001);
    let window = session.windows.last().unwrap();
    let table = window
        .tables
        .iter()
        .find(|table| table.name == APP_PARTS)
        .unwrap();
    let path = session
        .root
        .join(&window.data_directory)
        .join(format!("{APP_PARTS}.parquet"));
    let plan = persistence::engine::scan(&path).unwrap();
    let first = samples[0].selected.stamp.publication;
    let mut expected = AppMinute::default();
    for sample in &samples {
        expected.push_sample(sample);
    }
    let mut read = AppMinute::default();

    let parts = read_app_rows(plan, table.rows, first..first + 2, &mut read).unwrap();

    assert_eq!(parts, AppParts::Read);
    assert_eq!(expected.samples.len(), 2);
    assert!(expected.apps.len() > expected.samples.len());
    assert_eq!(read, expected);
}

#[test]
fn a_window_keeps_the_state_and_reasons_of_each_process_the_replay_gives() {
    // Goal: a window keeps a process that exited, one that replaced its program and one whose
    // usage macOS refused, as the replay of its minute shows them.
    // Method: seal three observations of an App whose processes exit, replace their program and
    // are refused, and compare the parts the window keeps of each App with those a replay
    // gives.
    let mut fixture = Fixture::new();
    publish_changing_processes(&mut fixture);
    fixture.seal_derived(&[], &persistence::sealed_apps::SealedApps::default());
    let session = Session::open(&fixture.root).unwrap();
    let replayed = replayed_samples(&session, 1003);
    let keys = app_keys(&replayed[2]);
    let expected = replayed_parts(&replayed, &keys);

    let kept: Vec<Vec<Shown>> = keys
        .iter()
        .map(|key| kept_parts(&session, 1003, key))
        .collect();

    assert!(shown(&expected, "state").contains(&json!("exited_verified")));
    assert!(shown(&expected, "app_attribution").contains(&json!("exec")));
    let refused = json!({ "known": null, "unavailable": "refused" });
    assert!(shown(&expected, "cpu_ns").contains(&refused));
    assert_eq!(kept, expected);
}

#[test]
fn a_window_keeps_the_rows_of_each_app_side_by_side_across_its_observations() {
    // Goal: a window keeps the rows of each series of all its observations together, the
    // observations' facts, each App's own and each process's, second after second, since a row
    // there changes little from the row before it and the table compresses to about half of what
    // it takes second by second.
    // Method: seal three observations of an App running two processes with their Apps' parts,
    // and read the runs of adjacent rows of the window's table that belong to one series, the
    // observations' facts, the App's own or one process's.
    let mut fixture = Fixture::new();
    for tick in 1..=3 {
        let rows = [
            named::member(7, b"worker", b"/A.app", usage()),
            named::member(8, b"helper", b"/A.app", usage()),
        ];
        named::publish(&mut fixture, tick, &rows);
    }
    fixture.seal_derived(&[], &persistence::sealed_apps::SealedApps::default());
    let session = Session::open(&fixture.root).unwrap();

    let runs = kept_runs(&session);

    let series: BTreeSet<&RowSeries> = runs.iter().map(|(series, _)| series).collect();
    assert_eq!(runs.len(), 4);
    assert_eq!(series.len(), runs.len());
    assert!(
        runs.iter()
            .all(|(_, publications)| publications == &[1, 2, 3])
    );
}

#[test]
fn a_window_keeps_the_facts_of_each_second_for_an_app_that_ran_at_none() {
    // Goal: an App that did not run at a second has no detail there, and the page still shows
    // that second's facts, as the replay gives them.
    // Method: seal two observations with their Apps' parts, and compare the parts the window
    // keeps of an App neither ran with those a replay of its minute gives.
    let mut fixture = Fixture::new();
    let session = sealed_seconds_with_parts(&mut fixture);
    let replayed = replayed_samples(&session, 1001);
    let absent = Key::new(KeyKind::Bundle, b"/Applications/Absent.app".to_vec()).unwrap();

    let kept = kept_parts(&session, 1001, &absent);

    assert_eq!(kept.len(), 2);
    assert_eq!(kept[0].detail, Value::Null);
    assert_eq!(vec![kept], replayed_parts(&replayed, &[absent]));
}

#[test]
fn a_window_derived_after_a_restart_keeps_what_a_derivation_that_stayed_up_keeps() {
    // Goal: restarting only the persister changes no saved App rate, since the minute before a
    // window is on disk whichever derivation saved it.
    // Method: seal run ticks 58 and 59 in one minute and tick 60 in the next, each minute through
    // a derivation of its own as a restarted persister seals them, and compare the parts of every
    // tick with a replay's. Two ticks in the first minute show which of them the second reads.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(58);
    fixture.publish_with_system_rows(1058);
    fixture.publish_with_system_rows(1059);
    fixture.publish_with_system_rows(1060);
    fixture.seal_minute_derived(0, &persistence::sealed_apps::SealedApps::default());
    fixture.seal_minute_derived(1, &persistence::sealed_apps::SealedApps::default());
    let session = Session::open(&fixture.root).unwrap();
    let samples = replayed_samples(&session, 1060);
    let keys = app_keys(&samples[2]);

    let kept = kept_parts(&session, 1060, &keys[0]);

    assert_eq!(kept.len(), 3);
    assert_ne!(kept[2].paging["previous"], Value::Null);
    assert_eq!(vec![kept], replayed_parts(&samples, &keys[..1]));
}

/// Seals what `publish` publishes as minute 0 and what `publish_next` publishes as minute 1, each
/// through a derivation of its own, as a persister restarted between them seals them.
fn sealed_across_a_restart(
    fixture: &mut Fixture,
    publish: impl FnOnce(&mut Fixture),
    publish_next: impl FnOnce(&mut Fixture),
) {
    fixture.start_at_run_tick(59);
    publish(fixture);
    fixture.seal_minute_derived(0, &persistence::sealed_apps::SealedApps::default());
    publish_next(fixture);
    fixture.seal_minute_derived(1, &persistence::sealed_apps::SealedApps::default());
}

/// The parts kept of each App of the replay's last second, and those the replay gives.
fn kept_and_replayed(fixture: &Fixture, second: u64) -> (Vec<Vec<Shown>>, Vec<Vec<Shown>>) {
    let session = Session::open(&fixture.root).unwrap();
    let samples = replayed_samples(&session, second);
    let keys = app_keys(samples.last().unwrap());
    let kept = keys
        .iter()
        .map(|key| kept_parts(&session, second, key))
        .collect();

    (kept, replayed_parts(&samples, &keys))
}

#[test]
fn a_restart_changes_nothing_for_a_process_that_replaced_a_pid() {
    // Goal: a pid another process took between two minutes bridges no rate across a restart, as it
    // bridges none in a replay.
    // Method: publish a pid in one minute and a new process on that pid in the next, restart
    // between them, and compare what is kept with the replay.
    let mut fixture = Fixture::new();
    let usage = usage();
    let worker = || named::member(7, b"worker", b"/A.app", usage);

    sealed_across_a_restart(
        &mut fixture,
        |fixture| named::publish(fixture, 0, &[worker()]),
        |fixture| {
            let mut replaced = worker();
            replaced.version = 2;
            named::publish(fixture, 1, &[replaced]);
        },
    );

    let (kept, replayed) = kept_and_replayed(&fixture, 1001);
    assert!(!kept.is_empty());
    assert_ne!(kept[0][1].paging["previous"], Value::Null);
    assert_eq!(kept, replayed);
}

#[test]
fn a_restart_changes_nothing_for_a_process_that_exited() {
    // Goal: a process seen only in its exit keeps the state a replay gives across a restart.
    // Method: publish a process and its exit in the last tick of a minute and the process alone in
    // the next tick, restart between them, and compare what is kept with the replay.
    let mut fixture = Fixture::new();
    let usage = usage();
    let helper = || named::member(8, b"helper", b"/A.app", usage);

    sealed_across_a_restart(
        &mut fixture,
        |fixture| {
            let mut exited = helper();
            exited.exited = true;
            named::publish(fixture, 0, &[helper(), exited]);
        },
        |fixture| named::publish(fixture, 1, &[helper()]),
    );

    let (kept, replayed) = kept_and_replayed(&fixture, 1001);
    assert!(!kept.is_empty());
    assert_ne!(kept[0][1].paging["previous"], Value::Null);
    assert_eq!(kept, replayed);
}

#[test]
fn a_restart_changes_nothing_for_a_process_that_used_the_gpu() {
    // Goal: GPU time is read back with the rest of a saved tick, so a restart keeps its rate.
    // Method: publish a process with GPU time on each side of a boundary, restart between them,
    // and compare what is kept with the replay.
    let mut fixture = Fixture::new();
    crate::recording::system_ring(&mut fixture);
    let usage = usage();
    let renderer = |gpu_ns| {
        let mut member = named::member(9, b"renderer", b"/A.app", usage);
        member.gpu_ns = Some(gpu_ns);
        member
    };

    sealed_across_a_restart(
        &mut fixture,
        |fixture| named::publish(fixture, 0, &[renderer(1_000)]),
        |fixture| named::publish(fixture, 1, &[renderer(5_000)]),
    );

    let (kept, replayed) = kept_and_replayed(&fixture, 1001);
    assert_ne!(kept[0][1].paging["previous"], Value::Null);
    assert_eq!(kept, replayed);
}

#[test]
fn a_restart_changes_nothing_for_a_process_whose_program_changed() {
    // Goal: an exec between two minutes reads across a restart as it reads in a replay.
    // Method: publish a tick with an exec on each side of the boundary, restart between them, and
    // compare what is kept with the replay.
    let mut fixture = Fixture::new();

    sealed_across_a_restart(
        &mut fixture,
        |fixture| fixture.publish(1059),
        |fixture| fixture.publish(1060),
    );

    let (kept, replayed) = kept_and_replayed(&fixture, 1060);
    assert!(!kept.is_empty());
    assert_ne!(kept[0][1].paging["previous"], Value::Null);
    assert_eq!(kept, replayed);
}

#[test]
fn a_restart_bridges_no_missed_tick() {
    // Goal: the minute before gives no rate across a tick nobody read, restarted or not. A replay
    // names the reading before and calls its rates unavailable, where a saved part names none, so
    // a derivation that stayed up differs from a replay in that one field too.
    // Method: publish run ticks 59 and 61 either side of the boundary, restart between them, and
    // compare tick 61's parts with the replay's with that field set aside.
    let mut fixture = Fixture::new();

    sealed_across_a_restart(
        &mut fixture,
        |fixture| fixture.publish_with_system_rows(1059),
        |fixture| {
            fixture.skip_run_ticks(1);
            fixture.publish_with_system_rows(1061);
        },
    );

    let (mut kept, mut replayed) = kept_and_replayed(&fixture, 1061);
    assert_eq!(kept[0][1].paging["previous"], Value::Null);
    for part in kept.iter_mut().chain(&mut replayed).flatten() {
        part.paging["previous"] = Value::Null;
        part.detail["previous"] = Value::Null;
    }
    assert_eq!(kept, replayed);
}

#[test]
fn a_saved_minute_that_does_not_read_costs_the_next_its_derived_tables_and_nothing_more() {
    // Goal: damage in the minute a restart reads back stops no save. The next window is saved with
    // no derived tables for a reader to rebuild or refuse, and the window after it derives again.
    // Method: save a minute, damage its processes, restart, and save two more minutes.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_system_rows(1059);
    fixture.seal_minute_derived(0, &persistence::sealed_apps::SealedApps::default());
    let saved = Session::open(&fixture.root).unwrap().windows[0].clone();
    let processes = fixture
        .root
        .join(&saved.data_directory)
        .join("processes.parquet");
    let bytes = std::fs::read(&processes).unwrap();
    std::fs::write(&processes, b"damaged").unwrap();
    let derivation = persistence::sealed_apps::SealedApps::default();
    fixture.publish_with_system_rows(1060);

    fixture.seal_minute_derived(1, &derivation);

    let mut session = crate::LiveSession::open(&fixture.root).unwrap();
    session.select(1060, 0).unwrap();
    let read_with_damage = session.app_history(KeyKind::Bundle, b"/Apps/Current.app");
    drop(session);
    std::fs::write(&processes, bytes).unwrap();
    fixture.skip_run_ticks(59);
    fixture.publish_with_system_rows(1120);
    fixture.publish_with_system_rows(1121);
    fixture.seal_minute_derived(2, &derivation);
    let windows = Session::open(&fixture.root).unwrap().windows;
    let derived =
        |window: &persistence::Manifest| window.tables.iter().any(|table| table.name == APP_PARTS);
    assert!(matches!(read_with_damage, Err(Error::Bound(_))));
    assert_eq!(windows.len(), 3);
    assert!(derived(&windows[0]));
    assert!(!derived(&windows[1]));
    assert!(derived(&windows[2]));
}

#[test]
fn a_saved_minute_of_an_identity_version_this_build_does_not_read_gives_the_next_no_rates() {
    // Goal: rows a restart reads back pass the checks a replay of them passes, so a minute a replay
    // refuses costs the next window its derived tables rather than lending it rates.
    // Method: save a minute, rewrite its processes to a later identity version, restart, and save
    // the next minute.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_system_rows(1059);
    fixture.seal_minute_derived(0, &persistence::sealed_apps::SealedApps::default());
    let saved = Session::open(&fixture.root).unwrap().windows[0].clone();
    let processes = fixture
        .root
        .join(&saved.data_directory)
        .join("processes.parquet");
    crate::recording::damage_table_file(&processes, |mut frame| {
        let versions = vec![persistence::schema::IDENTITY_VERSION + 1; frame.height()];
        frame
            .replace(
                "identity_version",
                Column::new("identity_version".into(), versions),
            )
            .unwrap();
        frame
    });
    fixture.publish_with_system_rows(1060);

    fixture.seal_minute_derived(1, &persistence::sealed_apps::SealedApps::default());

    let windows = Session::open(&fixture.root).unwrap().windows;
    assert_eq!(windows.len(), 2);
    assert!(
        !windows[1]
            .tables
            .iter()
            .any(|table| table.name == APP_PARTS)
    );
}

#[test]
fn a_window_takes_the_minute_before_from_the_derivation_that_sealed_it() {
    // Goal: a derivation gives the first observation of a window its rates from the last
    // observation it sampled of the minute before, without reading that minute again, as the
    // replay of both minutes gives them.
    // Method: seal run ticks 59 and 60 in two minutes with one derivation while the first
    // minute's tables are hidden from it, restore them, and compare the parts of tick 60 with
    // those a replay gives.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_system_rows(1059);
    fixture.publish_with_system_rows(1060);
    let derivation = persistence::sealed_apps::SealedApps::default();
    fixture.seal_minute_derived(0, &derivation);
    let hidden = hide_saved_tables(&fixture.root);
    fixture.seal_minute_derived(1, &derivation);
    restore_saved_tables(&hidden);
    let session = Session::open(&fixture.root).unwrap();
    let replayed = replayed_samples(&session, 1060);
    let keys = app_keys(&replayed[1]);

    let kept = kept_parts(&session, 1060, &keys[0]);

    assert_eq!(kept.len(), 2);
    assert_ne!(kept[1].paging["previous"], Value::Null);
    assert_eq!(vec![kept], replayed_parts(&replayed, &keys[..1]));
}

#[test]
fn a_derivation_that_last_read_another_run_takes_the_minute_before_from_the_windows_own_run() {
    // Goal: rates never cross a change of run or boot, so a persister that goes on to another
    // run's window takes the tick before from that window's run.
    // Method: seal a minute of a run through one derivation, then ticks 59 and 60 of the run after
    // it, the first through a derivation of its own and the second through the first derivation,
    // and compare the parts of tick 60 with those a replay gives.
    let derivation = persistence::sealed_apps::SealedApps::default();
    let mut fixture = Fixture::registered();
    fixture.publish_with_system_rows(1000);
    fixture.seal_minute_derived(0, &derivation);
    let mut next = Fixture::new_mode(true, 2, Some(&fixture));
    next.start_at_run_tick(59);
    next.publish_with_system_rows(1059);
    next.publish_with_system_rows(1060);
    next.seal_minute_derived(0, &persistence::sealed_apps::SealedApps::default());
    next.seal_minute_derived(1, &derivation);
    let session = Session::open(&next.root).unwrap();
    let keys = app_keys(replayed_samples(&session, 1060).last().unwrap());

    let kept = kept_parts(&session, 1060, &keys[0]);

    let newest = kept.last().unwrap();
    let replayed = replayed_samples_ending_at(&session, newest.selected);
    assert_ne!(newest.paging["previous"], Value::Null);
    let shown = replayed_parts(&replayed, &keys[..1]).remove(0);
    assert_eq!(
        shown.iter().find(|shown| shown.selected == newest.selected),
        Some(newest),
    );
}

#[test]
fn a_window_sealed_without_parts_keeps_none() {
    // Goal: a window sealed before the persister derived Apps' parts leaves its observations
    // to the replay.
    // Method: seal two observations without their Apps' parts, and read the parts their window
    // keeps of an executable.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    fixture.seal();
    let session = Session::open(&fixture.root).unwrap();
    let key = Key::new(KeyKind::Executable, b"launchd".to_vec()).unwrap();

    let kept = kept_parts(&session, 1001, &key);

    assert!(kept.is_empty());
}

#[test]
fn a_window_keeps_no_parts_from_a_table_of_another_version() {
    // Goal: a build reads no parts from a table a later build wrote in columns it may not know,
    // and leaves the window's observations to the replay.
    // Method: rewrite a sealed window's parts with the next version, keeping its rows, and read
    // the parts the window keeps of an App it holds.
    let mut fixture = Fixture::new();
    let session = sealed_seconds_with_parts(&mut fixture);
    let key = app_keys(&replayed_samples(&session, 1001)[1]).remove(0);
    rewrite(&session, APP_PARTS, |frame| {
        let version = vec![PARTS_VERSION + 1; frame.height()];
        frame
            .replace("version", Column::new("version".into(), version))
            .unwrap();
    });

    let kept = kept_parts(&session, 1001, &key);

    assert!(kept.is_empty());
}

#[test]
fn a_window_whose_parts_lack_a_row_its_seal_counted_is_refused() {
    // Goal: parts that lost a row since the window sealed are an error, not an App with fewer
    // processes.
    // Method: rewrite a sealed window's parts without their last row, and read them.
    let mut fixture = Fixture::new();
    let session = sealed_seconds_with_parts(&mut fixture);
    let key = app_keys(&replayed_samples(&session, 1001)[1]).remove(0);
    rewrite(&session, APP_PARTS, |frame| {
        *frame = frame.head(Some(frame.height() - 1));
    });

    let result = session.visit_sealed_parts(range(1001), key.kind, &key.bytes, |_part| Ok(()));

    assert!(matches!(result, Err(Error::Bound("sealed parts rows"))));
}

#[test]
fn a_window_whose_app_lost_a_process_to_another_key_is_refused() {
    // Goal: an App whose rows hold fewer processes than its members is an error, not an App
    // that shows part of its processes.
    // Method: rewrite a sealed window's parts with a process of one App under another App's
    // key, and read the parts of the App that lost it.
    let mut fixture = Fixture::new();
    let session = sealed_seconds_with_parts(&mut fixture);
    let keys = app_keys(&replayed_samples(&session, 1001)[1]);
    let own = &keys[0];
    let other = keys
        .iter()
        .find(|key| key.kind == own.kind && key.bytes != own.bytes)
        .unwrap();
    rewrite(&session, APP_PARTS, |frame| {
        move_process(frame, own, other);
    });

    let result = session.visit_sealed_parts(range(1001), own.kind, &own.bytes, |_part| Ok(()));

    assert!(matches!(result, Err(Error::Bound("sealed parts members"))));
}

#[test]
fn a_window_whose_app_counts_more_members_in_a_measure_than_it_has_is_refused() {
    // Goal: a measure that counts more members than its App has is an error, not a count the
    // App's history cannot hold.
    // Method: rewrite a sealed window's parts with one more unavailable member in each App's
    // CPU measure than the App has, and read the parts of one of its Apps.
    let mut fixture = Fixture::new();
    let session = sealed_seconds_with_parts(&mut fixture);
    let key = app_keys(&replayed_samples(&session, 1001)[1]).remove(0);
    rewrite(&session, APP_PARTS, overcount_members);

    let result = session.visit_sealed_parts(range(1001), key.kind, &key.bytes, |_part| Ok(()));

    assert!(matches!(result, Err(Error::Bound("sealed parts measure"))));
}

#[test]
fn every_code_a_part_is_written_with_reads_back_as_what_it_was_written_from() {
    // Goal: no reason, cause or state the persister writes a part with reads back as another.
    // Method: read back the code the persistence crate writes for each, and read every byte as
    // each kind of code.
    use model::apps::{ProcessState, ProgramUnavailable, Unavailable};
    let reasons = [
        Unavailable::Refused,
        Unavailable::Gone,
        Unavailable::Unknown,
        Unavailable::Interval,
        Unavailable::Creation,
        Unavailable::Baseline,
        Unavailable::CounterReset,
        Unavailable::Overflow,
        Unavailable::Exec,
        Unavailable::Program,
        Unavailable::ExitUnverified,
    ];
    let programs = [
        ProgramUnavailable::Refused,
        ProgramUnavailable::Gone,
        ProgramUnavailable::Unsupported,
        ProgramUnavailable::Changed,
        ProgramUnavailable::Invalid,
        ProgramUnavailable::Exhausted,
    ];
    let states = [
        ProcessState::Live,
        ProcessState::ExitedVerified,
        ProcessState::ExitedUnverified,
    ];

    codes_read_back(&reasons, unavailable_code, unavailable_of);
    codes_read_back(&programs, program_code, identity_of);
    codes_read_back(&states, state_code, state_of);
}

#[test]
fn a_read_decodes_the_parts_of_each_window_once_for_all_its_visits() {
    // Goal: one read of an App's histories visits the selected window's parts several times,
    // and decodes each window once, rather than once a visit.
    // Method: begin a read of an App, visit its parts, rewrite the window's parts to a version
    // this build does not read, and visit them again within the read.
    let mut fixture = Fixture::new();
    let session = sealed_seconds_with_parts(&mut fixture);
    let key = app_keys(&replayed_samples(&session, 1001)[1]).remove(0);
    session.parts_read_begin(key.kind, &key.bytes);
    let decoded = kept_parts(&session, 1001, &key);
    session.unread_kept_parts();

    let visited = kept_parts(&session, 1001, &key);

    assert_eq!(decoded.len(), 2);
    assert_eq!(visited, decoded);
}

#[test]
fn a_visit_after_its_read_ends_decodes_the_window_again() {
    // Goal: a read holds the parts it decoded only while it runs, so a window rewritten since
    // gives what its table now holds.
    // Method: visit an App's parts within a read and end it, rewrite the window's parts to a
    // version this build does not read, and visit them again.
    let mut fixture = Fixture::new();
    let session = sealed_seconds_with_parts(&mut fixture);
    let key = app_keys(&replayed_samples(&session, 1001)[1]).remove(0);
    session.parts_read_begin(key.kind, &key.bytes);
    let decoded = kept_parts(&session, 1001, &key);
    session.parts_read_end();
    session.unread_kept_parts();

    let visited = kept_parts(&session, 1001, &key);

    assert_eq!(decoded.len(), 2);
    assert!(visited.is_empty());
}

#[test]
fn a_read_decodes_again_the_parts_of_an_app_other_than_its_own() {
    // Goal: a read keeps the parts of the App it reads, and no other App's, so neither App is
    // later given what the read decoded of the other.
    // Method: begin a read of one App, visit another App's parts, rewrite the window's parts
    // to a version this build does not read, and visit both Apps' parts again.
    let mut fixture = Fixture::new();
    let session = sealed_seconds_with_parts(&mut fixture);
    let keys = app_keys(&replayed_samples(&session, 1001)[1]);
    session.parts_read_begin(keys[0].kind, &keys[0].bytes);
    let decoded = kept_parts(&session, 1001, &keys[1]);
    session.unread_kept_parts();

    let own = kept_parts(&session, 1001, &keys[0]);
    let other = kept_parts(&session, 1001, &keys[1]);

    assert_eq!(decoded.len(), 2);
    assert!(own.is_empty());
    assert!(other.is_empty());
}

#[test]
fn a_read_decodes_again_the_parts_past_its_bound() {
    // Goal: a read keeps no more of the parts it decodes than its bound, and decodes the rest
    // again at each visit.
    // Method: seal two minutes with their Apps' parts, read an App with a bound that holds
    // the first minute's parts alone, rewrite both minutes' parts to a version this build does
    // not read, and visit them again within the read.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    fixture.skip_run_ticks(58);
    fixture.publish_with_system_rows(1060);
    fixture.publish_with_system_rows(1061);
    fixture.seal_minute_derived(0, &persistence::sealed_apps::SealedApps::default());
    fixture.seal_minute_derived(1, &persistence::sealed_apps::SealedApps::default());
    let session = Session::open(&fixture.root).unwrap();
    let key = app_keys(&replayed_samples(&session, 1001)[1]).remove(0);
    let first_minute = kept_parts_bytes(&session, 1001, &key);
    session.parts_read_begin_bounded(key.kind, &key.bytes, first_minute);
    let decoded = kept_parts(&session, 1061, &key);
    session.unread_kept_parts();

    let visited = kept_parts(&session, 1061, &key);

    assert_eq!(decoded.len(), 4);
    assert_eq!(visited, decoded[..2]);
}

/// What a page shows of a part: its second, the App's detail, and the paging of the Apps page.
#[derive(Debug, PartialEq)]
struct Shown {
    selected: Observation,
    detail: Value,
    paging: Value,
}

impl Shown {
    fn of(part: &AppPart) -> Self {
        let page = Page {
            offset: 0,
            limit: PROCESSES_PAGE_ROWS_MAX,
        };

        Self {
            selected: *part.selected(),
            detail: part.detail(page).unwrap(),
            paging: part.apps_paging(page).unwrap(),
        }
    }
}

/// Seals seconds 1000 and 1001 with their Apps' parts, and opens their recording.
fn sealed_seconds_with_parts(fixture: &mut Fixture) -> Session {
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    fixture.seal_derived(&[], &persistence::sealed_apps::SealedApps::default());

    Session::open(&fixture.root).unwrap()
}

/// The bytes of the parts the windows of second `second`'s context keep of the App with `key`.
fn kept_parts_bytes(session: &Session, second: u64, key: &Key) -> usize {
    let mut bytes = 0;
    session
        .visit_sealed_parts(range(second), key.kind, &key.bytes, |part| {
            bytes += part.bytes();
            Ok(())
        })
        .unwrap();

    bytes
}

/// What a page shows of the parts of the App with `key` that the windows of second `second`'s
/// context keep, in publication order.
fn kept_parts(session: &Session, second: u64, key: &Key) -> Vec<Shown> {
    let mut kept = Vec::new();
    session
        .visit_sealed_parts(range(second), key.kind, &key.bytes, |part| {
            kept.push(Shown::of(part));
            Ok(())
        })
        .unwrap();
    kept.sort_by_key(|shown| shown.selected.stamp.publication);

    kept
}

/// What a page shows of the parts of each App with one of `keys` that `samples` give.
fn replayed_parts(samples: &[Sample], keys: &[Key]) -> Vec<Vec<Shown>> {
    keys.iter()
        .map(|key| {
            samples
                .iter()
                .map(|sample| Shown::of(&sample.app_part(key.kind, &key.bytes).unwrap()))
                .collect()
        })
        .collect()
}

/// The samples a replay of the saved minutes of second `second`'s context gives, in
/// publication order.
fn replayed_samples(session: &Session, second: u64) -> Vec<Sample> {
    let mut selected = None;
    session
        .visit_sealed_ranks(range(second), |ranks| {
            selected = Some(ranks.selected);
            Ok(())
        })
        .unwrap();

    replayed_samples_ending_at(session, selected.unwrap())
}

/// The samples a replay of the saved minutes up to the observation `selected` gives, in
/// publication order.
fn replayed_samples_ending_at(session: &Session, selected: Observation) -> Vec<Sample> {
    let mut replayed = Vec::new();
    session
        .visit_app_attribution(
            &crate::live_app_response::Selected::ending_at(selected).unwrap(),
            |_observation| Ok(false),
            |frame, previous, _windows| {
                let mut sample = None;
                Sample::capture(frame, previous, &mut sample)?;
                replayed.push(*sample.unwrap());
                Ok(())
            },
            |_pin| Ok(()),
        )
        .unwrap();

    replayed
}

/// Overwrites the tables the persister wrote for the sealed minutes, keeping their App tables,
/// so a read of them fails, and returns where outside the recording each one's bytes went.
fn hide_saved_tables(root: &Path) -> Vec<(PathBuf, PathBuf)> {
    let outside = root.with_extension("hidden");
    std::fs::create_dir(&outside).unwrap();
    let mut hidden = Vec::new();
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            let name = path.file_name().unwrap().to_str().unwrap();
            if path.is_dir() {
                directories.push(path);
            } else {
                if name.ends_with(".parquet") && !name.starts_with("app_") {
                    let aside = outside.join(hidden.len().to_string());
                    std::fs::copy(&path, &aside).unwrap();
                    std::fs::write(&path, b"hidden").unwrap();
                    hidden.push((path, aside));
                }
            }
        }
    }
    assert!(!hidden.is_empty());

    hidden
}

fn restore_saved_tables(hidden: &[(PathBuf, PathBuf)]) {
    for (path, aside) in hidden {
        std::fs::copy(aside, path).unwrap();
    }
    std::fs::remove_dir_all(hidden[0].1.parent().unwrap()).unwrap();
}

/// The series a row of the window's table belongs to: the App's kind and key, the row's kind,
/// and the process's name, pid and pid version.
type RowSeries = (
    Option<u8>,
    Option<Vec<u8>>,
    u8,
    Option<Vec<u8>>,
    Option<i32>,
    Option<i32>,
);

/// Each run of adjacent rows of the window's table that belong to one series, with the
/// publication of each row of the run, in the order the table holds them.
fn kept_runs(session: &Session) -> Vec<(RowSeries, Vec<u64>)> {
    let path = session
        .root
        .join(&session.windows[0].data_directory)
        .join(format!("{APP_PARTS}.parquet"));
    let frame = persistence::engine::scan(&path).unwrap().collect().unwrap();

    let mut runs: Vec<(RowSeries, Vec<u64>)> = Vec::new();
    for index in 0..frame.height() {
        let row = Row::new(&frame, index).unwrap();
        let series = (
            row.get(const { fixed("app_kind") }).unwrap(),
            row.get(const { fixed("app_key") }).unwrap(),
            row.get(const { fixed("row_kind") }).unwrap(),
            row.get(const { fixed("executable_name") }).unwrap(),
            row.get(const { fixed("pid") }).unwrap(),
            row.get(const { fixed("pid_version") }).unwrap(),
        );
        let publication = row.get(const { fixed("publication") }).unwrap();
        kept_runs_add(&mut runs, series, publication);
    }

    runs
}

fn kept_runs_add(runs: &mut Vec<(RowSeries, Vec<u64>)>, series: RowSeries, publication: u64) {
    if let Some((last, publications)) = runs.last_mut() {
        if *last == series {
            publications.push(publication);
            return;
        }
    }

    runs.push((series, vec![publication]));
}

/// Publishes run ticks 1 to 3 of an App whose "worker" replaces its program at tick 2 and has
/// its usage refused at tick 3, and whose "helper" exits at tick 2.
fn publish_changing_processes(fixture: &mut Fixture) {
    let usage = usage();
    let process = |pid, name: &'static [u8]| named::member(pid, name, b"/A.app", usage);
    let replaced = || {
        let mut worker = process(7, b"worker");
        worker.version = 2;
        worker
    };
    let mut exited = process(8, b"helper");
    exited.exited = true;
    let mut refused = replaced();
    refused.usage = None;

    named::publish(fixture, 1, &[process(7, b"worker"), process(8, b"helper")]);
    named::publish(fixture, 2, &[replaced(), exited]);
    named::publish(fixture, 3, &[refused]);
}

/// The value of `field` of each process that `parts` show.
fn shown(parts: &[Vec<Shown>], field: &str) -> Vec<Value> {
    parts
        .iter()
        .flatten()
        .filter_map(|part| part.detail["processes"].as_array())
        .flatten()
        .map(|process| process[field].clone())
        .collect()
}

fn app_keys(sample: &Sample) -> Vec<Key> {
    sample.apps().iter().map(|app| app.key.clone()).collect()
}

/// Puts the first process row of the App with key `own` under the key `other`.
fn move_process(frame: &mut DataFrame, own: &Key, other: &Key) {
    let mut keys: Vec<Option<Vec<u8>>> = (0..frame.height())
        .map(|index| {
            Row::new(frame, index)
                .unwrap()
                .get(const { fixed("app_key") })
                .unwrap()
        })
        .collect();
    let moved = (0..frame.height())
        .find(|index| {
            let kind: u8 = Row::new(frame, *index)
                .unwrap()
                .get(const { fixed("row_kind") })
                .unwrap();
            kind == ROW_PROCESS && keys[*index].as_deref() == Some(own.bytes.as_slice())
        })
        .unwrap();
    keys[moved] = Some(other.bytes.clone());

    let keys: Vec<Option<&[u8]>> = keys.iter().map(Option::as_deref).collect();
    frame
        .replace("app_key", Column::new("app_key".into(), keys))
        .unwrap();
}

/// Counts one more unavailable member in each App row's CPU measure than the App has.
fn overcount_members(frame: &mut DataFrame) {
    let unavailable: Vec<Option<u32>> = (0..frame.height())
        .map(|index| {
            let members: Option<u32> = Row::new(frame, index)
                .unwrap()
                .get(const { fixed("members") })
                .unwrap();
            members.map(|members| members + 1)
        })
        .collect();
    let name = "app_cpu_ns_unavailable_members";

    frame
        .replace(name, Column::new(name.into(), unavailable))
        .unwrap();
}

/// Reads every byte as a code by `of`, asserts each that reads writes back by `code` as that
/// byte, and gives how many read.
/// Asserts each of `written` reads back from its code as the value of the same name, and that
/// no other byte reads as a value.
fn codes_read_back<Written, Read>(
    written: &[Written],
    code: fn(Written) -> u8,
    of: fn(u8) -> Result<Read>,
) where
    Written: Copy + std::fmt::Debug,
    Read: std::fmt::Debug,
{
    for value in written {
        assert_eq!(
            format!("{:?}", of(code(*value)).unwrap()),
            format!("{value:?}")
        );
    }
    let read = (u8::MIN..=u8::MAX).filter(|byte| of(*byte).is_ok()).count();
    assert_eq!(read, written.len());
}
