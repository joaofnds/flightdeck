use std::collections::BTreeSet;

use polars::prelude::Column;
use serde_json::{Value, json};

use super::super::sealed::fixture::{range, rewrite};
use super::*;
use crate::live::app_process_groups_history_tests::{named, usage};
use crate::live::identity_fixture::Fixture;
use crate::live_app_response::{PAGE_ROWS_MAX, Page};

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
    fixture.seal_derived(&[], &crate::SealedApps);
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
fn a_window_keeps_the_state_and_reasons_of_each_process_the_replay_gives() {
    // Goal: a window keeps a process that exited, one that replaced its program and one whose
    // usage macOS refused, as the replay of its minute shows them.
    // Method: seal three observations of an App whose processes exit, replace their program and
    // are refused, and compare the parts the window keeps of each App with those a replay
    // gives.
    let mut fixture = Fixture::new();
    publish_changing_processes(&mut fixture);
    fixture.seal_derived(&[], &crate::SealedApps);
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
    // Goal: a window keeps each App's rows of all its observations together, the App's own and
    // then each process's second after second, since a row there changes little from the row
    // before it and the table compresses to about half of what it takes second by second.
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
    fixture.seal_derived(&[], &crate::SealedApps);
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
fn a_window_keeps_the_rates_its_first_observation_takes_from_the_minute_before() {
    // Goal: the first observation of a window shows the rates it takes from the last
    // observation of the sealed minute before it, as the replay shows them.
    // Method: seal run ticks 59 and 60 in two minutes with their Apps' parts, and compare the
    // parts of tick 60, which need the tick before, with those a replay of both minutes gives.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_system_rows(1059);
    fixture.publish_with_system_rows(1060);
    fixture.seal_minute_derived(0, &crate::SealedApps);
    fixture.seal_minute_derived(1, &crate::SealedApps);
    let session = Session::open(&fixture.root).unwrap();
    let replayed = replayed_samples(&session, 1060);
    let keys = app_keys(&replayed[1]);

    let kept = kept_parts(&session, 1060, &keys[0]);

    assert_eq!(kept.len(), 2);
    assert_ne!(kept[1].paging["previous"], Value::Null);
    assert_eq!(vec![kept], replayed_parts(&replayed, &keys[..1]));
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
        let version = vec![VERSION + 1; frame.height()];
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
    // Goal: no reason, cause or state a part keeps reads back as another.
    // Method: read every byte as each kind of code, and write back each that reads.
    assert_eq!(codes_read_back(unavailable_of, unavailable_code), 11);
    assert_eq!(codes_read_back(identity_of, identity_code), 6);
    assert_eq!(codes_read_back(state_of, state_code), 3);
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
    fixture.seal_minute_derived(0, &crate::SealedApps);
    fixture.seal_minute_derived(1, &crate::SealedApps);
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
            limit: PAGE_ROWS_MAX,
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
    fixture.seal_derived(&[], &crate::SealedApps);

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
    let mut replayed = Vec::new();
    session
        .visit_app_attribution(
            &selected.unwrap(),
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
fn codes_read_back<T: Copy>(of: fn(u8) -> Result<T>, code: fn(T) -> u8) -> usize {
    let mut read = 0;
    for byte in u8::MIN..=u8::MAX {
        if let Ok(value) = of(byte) {
            assert_eq!(code(value), byte);
            read += 1;
        }
    }

    read
}
