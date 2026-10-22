use std::sync::Arc;

use super::*;
use crate::Cancellation;
use crate::apps::Stamp;
use crate::live::identity_fixture::Fixture;
use crate::live_app_history::{App, CompactMeasure, compact};
use crate::live_app_response::{Key, KeyKind, Measure};
use collector::processes::Usage;

struct SavedFixture {
    fixture: Fixture,
    saved: Session,
    selected: Observation,
    cancellation: Arc<Cancellation>,
    operation: Operation,
    expected: [Value; 3],
    limited_bytes: usize,
}

#[test]
fn batch_keeps_distinct_eight_app_cohorts_other_and_missing_observations() {
    // Rotated leaders and a missing middle publication forbid sharing CPU's cohort or zeros.
    let history = distinct_history();
    let selected = observation(3);
    let cancellation = Arc::new(Cancellation::default());
    let operation = cancellation.operation(0).unwrap();
    let expected = METRICS.map(|metric| {
        history
            .attribution_response(
                &Selected::ending_at(selected).unwrap(),
                metric,
                None,
                &operation,
            )
            .unwrap()
    });

    let batch = history
        .attribution_batch_response(&Selected::ending_at(selected).unwrap(), None, &operation)
        .unwrap();

    assert_batch(&batch, &expected);
    let cpu = batch.cpu.as_ref().unwrap();
    let power = batch.power.as_ref().unwrap();
    let disk = batch.disk.as_ref().unwrap();
    assert_ne!(cpu["cohort"], power["cohort"]);
    assert_ne!(cpu["cohort"], disk["cohort"]);
    assert_ne!(power["cohort"], disk["cohort"]);
    assert_eq!(cpu["cohort"].as_array().unwrap().len(), 8);
    assert_eq!(cpu["points"][0]["other"]["app_count"], 4);
    assert_eq!(cpu["points"][1]["source_available"], false);
    assert_eq!(cpu["points"][1]["other"], Value::Null);
}

#[test]
fn batch_pressure_replays_the_uncached_suffix_twice_for_all_three_metrics() {
    // The same explicitly limited budget exercises the two physical streamed data passes.
    let fixture = SavedFixture::new();
    let history = History::new();
    PROBE.with(|probe| *probe.borrow_mut() = Probe::default());

    let batch = fixture.query(&history, fixture.limited_bytes).unwrap();

    assert_batch(&batch, &fixture.expected);
    let cpu = batch.cpu.as_ref().unwrap();
    let uncached = cpu["coverage"]["saved_observations_uncached"]
        .as_u64()
        .unwrap();
    assert!(uncached > 0);
    assert_eq!(cpu["coverage"]["saved_cache_limited"], true);
    PROBE.with(|probe| {
        assert_eq!(u64::from(probe.borrow().observed), uncached);
        assert_eq!(u64::from(probe.borrow().projected), uncached);
    });
    assert!(history.saved_residents.borrow().bytes() <= fixture.limited_bytes);
    let warm = fixture.query(&history, fixture.limited_bytes).unwrap();
    assert_batch(&warm, &fixture.expected);
}

#[test]
fn batch_zero_cache_budget_keeps_context_and_aggregate_response_bound() {
    // No saved allocation remains available; every uncached endpoint must still be represented.
    let fixture = SavedFixture::new();
    let history = History::new();

    let batch = fixture.query(&history, 0).unwrap();

    assert_batch(&batch, &fixture.expected);
    let cpu = batch.cpu.as_ref().unwrap();
    assert_eq!(cpu["coverage"]["saved_observations_cached"], 0);
    assert_eq!(cpu["coverage"]["saved_observations_uncached"], 6);
    let encoded: usize = [&batch.cpu, &batch.power, &batch.disk]
        .into_iter()
        .map(|value| serde_json::to_vec(value.as_ref().unwrap()).unwrap().len())
        .sum();
    assert!(encoded <= crate::APP_ATTRIBUTION_HISTORIES_RESPONSE_BYTES_MAX);
    assert!(history.total_bytes() <= BYTES_MAX);
}

#[test]
fn batch_cancellation_restores_warm_facts_and_refuses_every_metric() {
    // Cancellation after choosing leaders cannot lose the taken cache or return a partial batch.
    let fixture = SavedFixture::new();
    let history = History::new();
    fixture.query(&history, BYTES_MAX).unwrap();
    let before = history.total_bytes();
    let cancellation = Arc::clone(&fixture.cancellation);
    PROBE.with(|probe| {
        probe.borrow_mut().before_project = Some(Box::new(move || cancellation.cancel().unwrap()));
    });

    let result = fixture.query(&history, BYTES_MAX);

    assert!(matches!(result, Err(Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
    assert_eq!(history.total_bytes(), before);
    let operation = Arc::clone(&fixture.cancellation)
        .operation(fixture.cancellation.epoch())
        .unwrap();
    let retry = history
        .attribution_batch_saved(
            &SavedQuery {
                selected: &Selected::ending_at(fixture.selected).unwrap(),
                saved: &fixture.saved,
                operation: &operation,
                available: BYTES_MAX.saturating_sub(history.bytes),
            },
            &mut Batch::new(
                &history,
                &Selected::ending_at(fixture.selected).unwrap(),
                &operation,
            )
            .unwrap(),
        )
        .unwrap();
    assert_batch(&retry, &fixture.expected);
}

#[test]
fn batch_final_validation_rejects_replaced_cached_files_and_restores_ownership() {
    // Replacing an admitted file between phases must fail even when no raw replay is needed.
    let fixture = SavedFixture::new();
    let history = History::new();
    fixture.query(&history, BYTES_MAX).unwrap();
    let mut resident = history.saved_residents.borrow_mut();
    let rates = resident.take_attribution();
    let path = fixture
        .fixture
        .root
        .join(&rates.pins()[0].manifest.data_directory)
        .join("events.parquet");
    let retained = rates.count();
    resident.install_attribution(rates);
    drop(resident);
    let original = fixture.fixture.root.join("batch-events-original.parquet");
    PROBE.with(|probe| {
        probe.borrow_mut().before_project = Some(Box::new(move || {
            std::fs::rename(&path, &original).unwrap();
            std::fs::copy(&original, &path).unwrap();
        }));
    });

    let result = fixture.query(&history, BYTES_MAX);

    assert!(matches!(
        result,
        Err(Error::Bound("attribution cache file changed"))
    ));
    let mut resident = history.saved_residents.borrow_mut();
    let rates = resident.take_attribution();
    assert_eq!(rates.count(), retained);
    resident.install_attribution(rates);
    assert!(history.bytes + resident.bytes() <= BYTES_MAX);
}

#[test]
fn batch_cpu_failure_preserves_power_and_disk() {
    // A local reduction refusal must leave both sibling payloads byte-identical.
    assert_metric_failure(0);
}

#[test]
fn batch_power_failure_preserves_cpu_and_disk() {
    // Each position in the fixed coordinator needs its own independently reporting case.
    assert_metric_failure(1);
}

#[test]
fn batch_disk_failure_preserves_cpu_and_power() {
    // The final result slot must retain its own failure rather than borrowing a sibling.
    assert_metric_failure(2);
}

fn assert_metric_failure(failed: usize) {
    let history = distinct_history();
    let selected = observation(3);
    let cancellation = Arc::new(Cancellation::default());
    let operation = cancellation.operation(0).unwrap();
    let expected = METRICS.map(|metric| {
        history
            .attribution_response(
                &Selected::ending_at(selected).unwrap(),
                metric,
                None,
                &operation,
            )
            .unwrap()
    });
    let mut batch = Batch::new(
        &history,
        &Selected::ending_at(selected).unwrap(),
        &operation,
    )
    .unwrap();
    batch.states[failed].apply(|_reduction| Err(Error::Bound("batch metric test failure")));
    batch.choose(&operation).unwrap();
    batch.project_live(&history, &operation).unwrap();

    let result = batch.finish(&history, None, &operation).unwrap();

    for (index, value) in [&result.cpu, &result.power, &result.disk]
        .into_iter()
        .enumerate()
    {
        if index == failed {
            assert!(matches!(
                value,
                Err(Error::Bound("batch metric test failure"))
            ));
        } else {
            assert_payload(value.as_ref().unwrap(), &expected[index]);
        }
    }
}

#[test]
fn batch_keeps_a_saved_predecessor_outside_the_context_across_minutes() {
    // The visible endpoint needs its real predecessor in the preceding committed minute.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_system_rows(940);
    fixture.publish_with_system_rows(1900);
    fixture.seal_minute(0);
    fixture.seal_minute(1);
    retire(&fixture);
    let mut session = crate::LiveSession::open(&fixture.root).unwrap();
    session.select(1900, 0).unwrap();
    let expected = METRICS.map(|metric| session.app_attribution_history(metric).unwrap());

    let batch = session.app_attribution_histories().unwrap();

    assert_batch(&batch, &expected);
    assert_eq!(
        batch.cpu.as_ref().unwrap()["points"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        batch.cpu.as_ref().unwrap()["points"][0]["elapsed_ns"],
        "1000000000"
    );
}

#[test]
fn batch_from_the_parts_sealed_windows_keep_is_the_batch_their_replay_gives() {
    // Goal: the three histories from the parts each sealed window keeps are the histories a
    // replay of the windows' minutes gives, and read none of the windows' saved sources.
    // Method: seal four minutes with their Apps' parts, take the batch, and compare it with
    // the batch a session gives once the parts are of a version this build does not read.
    let fixture = kept_fixture([true; 4]);
    let kept_session = Session::open(&fixture.root).unwrap();

    let kept = saved_batch(&kept_session);

    assert_eq!(kept_session.source_reads.get(), 0);
    assert_ne!(kept.cpu.as_ref().unwrap()["cohort"], json!([]));
    assert_ne!(
        kept.power.as_ref().unwrap()["points"],
        kept.disk.as_ref().unwrap()["points"]
    );
    assert_batch(&kept, &replayed_batch(kept_session, &fixture, 6).0);
}

#[test]
fn batch_replays_the_windows_sealed_without_parts_before_those_that_keep_them() {
    // Goal: windows sealed without parts before windows that keep them give the batch a
    // replay of them all gives, and the windows that keep them are not replayed.
    // Method: seal the first two minutes without parts and the rest with them, and compare the
    // batch and the saved sources it read with those a replay of every window gives.
    let fixture = kept_fixture([false, false, true, true]);
    let kept_session = Session::open(&fixture.root).unwrap();

    let kept = saved_batch(&kept_session);

    let reads = kept_session.source_reads.get();
    let (replayed, replayed_reads) = replayed_batch(kept_session, &fixture, 6);
    assert_batch(&kept, &replayed);
    assert!(reads < replayed_reads);
}

#[test]
fn batch_replays_a_window_sealed_without_parts_with_rates_from_one_that_keeps_them() {
    // Goal: the first observation of a window sealed without parts takes its rates from the
    // last observation of the window before it, which keeps its parts, as a replay of them
    // all gives it, and the windows that keep them are not replayed.
    // Method: seal the first two minutes with parts and the rest without, and compare the
    // batch and the saved sources it read with those a replay of every window gives.
    let fixture = kept_fixture([true, true, false, false]);
    let kept_session = Session::open(&fixture.root).unwrap();

    let kept = saved_batch(&kept_session);

    let reads = kept_session.source_reads.get();
    let (replayed, replayed_reads) = replayed_batch(kept_session, &fixture, 6);
    assert_batch(&kept, &replayed);
    assert!(reads < replayed_reads);
}

#[test]
fn batch_from_the_parts_of_an_earlier_second_is_the_batch_its_replay_gives() {
    // Goal: a batch ending inside a window that keeps its parts leaves out the observations
    // after its end, as a replay does.
    // Method: seal four minutes with their parts, take the batch at the fourth of six
    // observations, and compare it with the replay's batch at the same observation.
    let fixture = kept_fixture([true; 4]);
    let kept_session = Session::open(&fixture.root).unwrap();

    let kept = kept_batch(&kept_session, &History::new(), 4, BYTES_MAX).unwrap();

    assert_batch(&kept, &replayed_batch(kept_session, &fixture, 4).0);
}

#[test]
fn batch_from_parts_whose_plotted_observations_have_a_gap_is_the_batch_their_replay_gives() {
    // Goal: a window that keeps its parts, whose observations in the context are not adjacent,
    // gives the batch a replay of its minute gives.
    // Method: seal one minute of three observations whose second has a wall clock that stepped
    // back before the context, so the batch at the third plots the first and the third only,
    // and compare it with the replay's batch at the same observation.
    let mut fixture = Fixture::new();
    for (index, second) in [1000, 10, 1002].into_iter().enumerate() {
        fixture.publish_with_usage(second, &kept_usage(u64::try_from(index).unwrap()));
    }
    fixture.seal_minute_derived(0, &crate::SealedApps::default());
    let kept_session = Session::open(&fixture.root).unwrap();
    // The third observation is publication 3, at run tick 2 of the run's first minute.
    let selected = kept_selected(&kept_session, 3, 2, 1002);

    let kept = saved_batch_at(&kept_session, &History::new(), &selected, BYTES_MAX).unwrap();

    assert_eq!(kept_session.source_reads.get(), 0);
    assert_eq!(
        kept.cpu.as_ref().unwrap()["points"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_batch(
        &kept,
        &replayed_batch_at(kept_session, &fixture, &selected).0,
    );
}

#[test]
fn batch_from_points_cached_from_parts_is_the_batch_the_first_query_gave() {
    // Goal: a second query takes the points the first took from the windows' parts out of the
    // cache and gives the same batch.
    // Method: take the batch twice with one history over windows that keep their parts.
    let fixture = kept_fixture([true; 4]);
    let session = Session::open(&fixture.root).unwrap();
    let history = History::new();
    let first = kept_batch(&session, &history, 6, BYTES_MAX).unwrap();

    let second = kept_batch(&session, &history, 6, BYTES_MAX).unwrap();

    assert_batch(
        &second,
        &[first.cpu, first.power, first.disk].map(Result::unwrap),
    );
}

#[test]
fn batch_reads_the_facts_of_windows_that_keep_their_parts_once_per_session() {
    // Goal: a sealed window's facts never change, so a session reads them from its parts once
    // and every later batch of any history takes them from the session.
    // Method: take the batch with one history, then with a fresh one, and count the queries
    // of the windows' facts each ran.
    let fixture = kept_fixture([true; 4]);
    let session = Session::open(&fixture.root).unwrap();
    let first = saved_batch(&session);
    let queries = session.kept_header_queries.get();

    let second = saved_batch(&session);

    assert_eq!(queries, 4);
    assert_eq!(session.kept_header_queries.get(), queries);
    assert_batch(
        &second,
        &[first.cpu, first.power, first.disk].map(Result::unwrap),
    );
}

#[test]
fn batch_reads_again_the_facts_of_parts_rewritten_since_the_session_read_them() {
    // Goal: the session's facts of a window's parts never outlive the file they came from.
    // Method: take the batch, rewrite each window's parts so their facts count one App more
    // than they keep, and take the batch again on the same session.
    let fixture = kept_fixture([true; 4]);
    let session = Session::open(&fixture.root).unwrap();
    saved_batch(&session);
    Session::open(&fixture.root).unwrap().overcount_kept_apps();

    let result = kept_batch(&session, &History::new(), 6, BYTES_MAX);

    assert!(matches!(result, Err(Error::Bound("sealed parts Apps"))));
}

#[test]
fn batch_from_parts_past_the_cache_budget_is_the_batch_their_replay_gives() {
    // Goal: windows that keep their parts give the replay's batch when the cache cannot hold
    // all their points, so the batch reads them twice.
    // Method: find the budget one pin short of the cache a full batch fills, take the batch
    // within it, and compare it with the replay's batch.
    let fixture = kept_fixture([true; 4]);
    let kept_session = Session::open(&fixture.root).unwrap();
    let full = History::new();
    kept_batch(&kept_session, &full, 6, BYTES_MAX).unwrap();
    let rates = full.saved_residents.borrow_mut().take_attribution();
    let limited = rates.bytes() - rates.pins().last().unwrap().bytes();

    let kept = kept_batch(&kept_session, &History::new(), 6, limited).unwrap();

    assert_eq!(
        kept.cpu.as_ref().unwrap()["coverage"]["saved_cache_limited"],
        true
    );
    assert_batch(&kept, &replayed_batch(kept_session, &fixture, 6).0);
}

#[test]
fn batch_refuses_a_replaced_file_a_cached_point_took_its_rates_from() {
    // Goal: a cached point whose rates came from a window that keeps its parts, replayed for
    // its last observation, fails the next batch once that window's processes are replaced.
    // Method: seal two minutes with parts and two without, take the batch, replace the
    // second window's processes file with a copy, and take the batch again.
    let fixture = kept_fixture([true, true, false, false]);
    let session = Session::open(&fixture.root).unwrap();
    let history = History::new();
    kept_batch(&session, &history, 6, BYTES_MAX).unwrap();
    let directory = fixture.root.join(&session.windows[1].data_directory);
    let original = fixture.root.join("processes-original.parquet");
    std::fs::rename(directory.join("processes.parquet"), &original).unwrap();
    std::fs::copy(&original, directory.join("processes.parquet")).unwrap();

    let result = kept_batch(&session, &history, 6, BYTES_MAX);

    assert!(matches!(
        result,
        Err(Error::Bound("attribution cache file changed"))
    ));
}

#[test]
fn batch_refuses_replaced_parts_a_cached_point_was_taken_from() {
    // Goal: a cached point taken from a window's parts fails the next batch once those parts
    // are replaced.
    // Method: seal four minutes with their parts, take the batch, replace the first window's
    // parts with a copy, and take the batch again with the same history.
    let fixture = kept_fixture([true; 4]);
    let session = Session::open(&fixture.root).unwrap();
    let history = History::new();
    kept_batch(&session, &history, 6, BYTES_MAX).unwrap();
    let directory = fixture.root.join(&session.windows[0].data_directory);
    let original = fixture.root.join("app_parts-original.parquet");
    std::fs::rename(directory.join("app_parts.parquet"), &original).unwrap();
    std::fs::copy(&original, directory.join("app_parts.parquet")).unwrap();

    let result = kept_batch(&session, &history, 6, BYTES_MAX);

    assert!(matches!(
        result,
        Err(Error::Bound("attribution cache file changed"))
    ));
}

#[test]
fn batch_refuses_parts_whose_facts_count_more_apps_than_the_window_keeps() {
    // Goal: an observation whose facts count more Apps than its window's parts keep for it
    // fails the batch rather than giving a point that misses an App.
    // Method: seal the minutes with their parts, raise each observation's App count, and take
    // the batch.
    let fixture = kept_fixture([true; 4]);
    Session::open(&fixture.root).unwrap().overcount_kept_apps();
    let session = Session::open(&fixture.root).unwrap();

    let result = kept_batch(&session, &History::new(), 6, BYTES_MAX);

    assert!(matches!(result, Err(Error::Bound("sealed parts Apps"))));
}

#[test]
fn batch_shows_the_membership_completeness_the_parts_keep_for_each_app() {
    // Goal: each App of a batch from the parts says what its row in the parts says of whether
    // its membership is complete, and takes it from no other column.
    // Method: take the batch, invert what every App row of the parts says, and take it again.
    let fixture = kept_fixture([true; 4]);
    let kept = memberships(&saved_batch(&Session::open(&fixture.root).unwrap()));
    Session::open(&fixture.root)
        .unwrap()
        .invert_kept_memberships();
    let inverted_session = Session::open(&fixture.root).unwrap();

    let inverted = memberships(&saved_batch(&inverted_session));

    assert!(!kept.is_empty());
    assert_eq!(
        inverted,
        kept.iter().map(|complete| !complete).collect::<Vec<_>>()
    );
}

#[test]
fn batch_refuses_parts_holding_a_null_app_cell() {
    // Goal: an App row of the parts without its members fails the batch.
    // Method: seal the minutes with their parts, empty the members of every App row, and take
    // the batch.
    let fixture = kept_fixture([true; 4]);
    Session::open(&fixture.root)
        .unwrap()
        .null_kept_column("members");
    let session = Session::open(&fixture.root).unwrap();

    let result = kept_batch(&session, &History::new(), 6, BYTES_MAX);

    assert!(matches!(result, Err(Error::Bound("query null cell"))));
}

#[test]
fn batch_refuses_parts_holding_an_app_column_in_another_type() {
    // Goal: parts that hold an App column in a type no build writes it in fail the batch,
    // even when each of its values fits the type the reader takes.
    // Method: seal the minutes with their parts, widen the members column to 64 bits, and take
    // the batch.
    let fixture = kept_fixture([true; 4]);
    Session::open(&fixture.root)
        .unwrap()
        .widen_kept_column("members");
    let session = Session::open(&fixture.root).unwrap();

    let result = kept_batch(&session, &History::new(), 6, BYTES_MAX);

    assert!(matches!(result, Err(Error::Bound("query column type"))));
}

#[test]
fn batch_refuses_parts_whose_apps_are_out_of_order() {
    // Goal: an App row of the parts in another place than its observation's next App fails
    // the batch rather than giving a point whose Apps are shuffled.
    // Method: seal the minutes with their parts, move each App row one place on, and take the
    // batch.
    let fixture = kept_fixture([true; 4]);
    Session::open(&fixture.root).unwrap().misorder_kept_apps();
    let session = Session::open(&fixture.root).unwrap();

    let result = kept_batch(&session, &History::new(), 6, BYTES_MAX);

    assert!(matches!(result, Err(Error::Bound("sealed parts App"))));
}

#[test]
fn batch_reports_the_cell_of_an_app_row_out_of_order() {
    // Goal: an App row both out of order and without a cell reports its cell, as the read of
    // one App's parts does, so one malformed row gives one error whichever read meets it.
    // Method: seal the minutes with their parts, move each App row one place on and empty its
    // members, and take the batch.
    let fixture = kept_fixture([true; 4]);
    let rewriting = Session::open(&fixture.root).unwrap();
    rewriting.misorder_kept_apps();
    rewriting.null_kept_column("members");
    drop(rewriting);
    let session = Session::open(&fixture.root).unwrap();

    let result = kept_batch(&session, &History::new(), 6, BYTES_MAX);

    assert!(matches!(result, Err(Error::Bound("query null cell"))));
}

#[test]
fn app_histories_are_kept_where_every_minute_of_their_range_keeps_its_parts() {
    // Goal: a session tells a page that it reads a selection's App histories without replaying
    // a saved minute where every minute of their range keeps its Apps' parts.
    // Method: seal every minute with its parts and ask at the last observation.
    let fixture = kept_fixture([true; 4]);
    let saved = Session::open(&fixture.root).unwrap();

    let kept = saved.app_histories_replay_nothing(&kept_selected(&saved, 6, 180, 1005));

    assert!(kept.unwrap());
}

#[test]
fn app_histories_are_not_kept_where_a_minute_of_their_range_keeps_no_parts() {
    // Goal: App histories that would replay a saved minute are not kept.
    // Method: seal the middle minute without its parts and ask at the last observation.
    let fixture = kept_fixture([true, false, true, true]);
    let saved = Session::open(&fixture.root).unwrap();

    let kept = saved.app_histories_replay_nothing(&kept_selected(&saved, 6, 180, 1005));

    assert!(!kept.unwrap());
}

#[test]
fn app_histories_are_not_kept_where_a_minute_keeps_parts_this_build_does_not_read() {
    // Goal: parts sealed at an earlier version, which reading the App histories replays, hold
    // them back as a minute sealed without parts does.
    // Method: seal every minute with its parts, rewrite them to the earlier version, and ask
    // at the last observation of a session opened afterwards.
    let fixture = kept_fixture([true; 4]);
    Session::open(&fixture.root)
        .unwrap()
        .previous_version_kept_parts();
    let saved = Session::open(&fixture.root).unwrap();

    let kept = saved.app_histories_replay_nothing(&kept_selected(&saved, 6, 180, 1005));

    assert!(!kept.unwrap());
}

#[test]
fn app_histories_are_kept_where_only_a_minute_past_their_range_keeps_no_parts() {
    // Goal: a minute the App histories do not plot never holds them back.
    // Method: seal the two minutes after the second observation without their parts, and ask
    // at that observation, whose range ends before them.
    let fixture = kept_fixture([true, true, false, false]);
    let saved = Session::open(&fixture.root).unwrap();

    let kept = saved.app_histories_replay_nothing(&kept_selected(&saved, 2, 60, 1001));

    assert!(kept.unwrap());
}

#[test]
fn app_histories_are_kept_where_only_a_minute_before_their_range_keeps_no_parts() {
    // Goal: a minute older than the App histories' range never holds them back.
    // Method: seal a minute without its parts more than the charted seconds before the last
    // observation, seal the minutes after it with their parts, and ask at that observation.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_usage(1000, &kept_usage(0));
    fixture.publish_with_usage(1001, &kept_usage(1));
    fixture.skip_run_ticks(58);
    fixture.publish_with_usage(3000, &kept_usage(2));
    fixture.publish_with_usage(3001, &kept_usage(3));
    fixture.seal_minute(0);
    fixture.seal_minute_derived(1, &crate::SealedApps::default());
    fixture.seal_minute_derived(2, &crate::SealedApps::default());
    let saved = Session::open(&fixture.root).unwrap();

    let kept = saved.app_histories_replay_nothing(&kept_selected(&saved, 4, 120, 3001));

    assert!(kept.unwrap());
}

#[test]
fn app_histories_are_kept_where_a_minute_of_unknown_bounds_holds_no_second_of_their_range() {
    // Goal: a minute whose wall clock bounds are unknown holds the App histories back only
    // where one of its observations falls in their range, since the replay reads no other.
    // Method: seal a minute without its parts, one of its observations of unknown wall time
    // and the other more than the charted seconds before the last observation, seal the
    // minutes after it with their parts, and ask at that observation.
    let fixture = unbounded_fixture(1000);
    fixture.seal_minute_derived(1, &crate::SealedApps::default());
    fixture.seal_minute_derived(2, &crate::SealedApps::default());
    let saved = Session::open(&fixture.root).unwrap();

    let kept = saved.app_histories_replay_nothing(&kept_selected(&saved, 5, 120, 3001));

    assert!(kept.unwrap());
}

#[test]
fn app_histories_are_not_kept_where_a_minute_of_unknown_bounds_holds_a_second_of_their_range() {
    // Goal: a minute whose wall clock bounds are unknown and that holds a second of the App
    // histories' range holds them back.
    // Method: seal the first minute without its parts, one of its observations of unknown wall
    // time and the other in the range of the last observation, seal the minutes after it with
    // their parts, and ask at that observation.
    let fixture = unbounded_fixture(2400);
    fixture.seal_minute_derived(1, &crate::SealedApps::default());
    fixture.seal_minute_derived(2, &crate::SealedApps::default());
    let saved = Session::open(&fixture.root).unwrap();

    let kept = saved.app_histories_replay_nothing(&kept_selected(&saved, 5, 120, 3001));

    assert!(!kept.unwrap());
}

#[test]
fn batch_rejects_more_than_the_observation_bound_in_each_metric() {
    // A full ledger is accepted; the next observation fails instead of growing a result.
    let cancellation = Arc::new(Cancellation::default());
    let operation = cancellation.operation(0).unwrap();
    let mut selected = observation(4097);
    selected.wall_ns = Some(1_000_000_000_000);
    let mut states = METRICS.map(|metric| {
        MetricState::Ready(Box::new(Reduction::new(
            &Selected::ending_at(selected).unwrap(),
            metric,
            &operation,
        )))
    });
    for publication in 1..=4097 {
        let mut selected = observation(publication);
        selected.wall_ns = Some(1_000_000_000_000);
        let capture = crate::app_attribution_history::Capture {
            coverage: crate::live::resident_history::Capture {
                observation: selected,
                membership_complete: true,
                ungrouped_members: 0,
                usage_refused: 0,
                usage_gone: 0,
            },
            elapsed_ns: Some(1_000_000_000),
        };
        for state in &mut states {
            state.apply(|reduction| reduction.observe(&capture, std::iter::empty(), Source::Saved));
        }
    }
    for state in states {
        assert!(matches!(
            state,
            MetricState::Failed(Error::Bound("attribution observations"))
        ));
    }
}

#[test]
fn batch_accepts_the_app_population_bound_and_rejects_one_more_key() {
    // Increasing exact byte keys fill the shared reductions without repeated-key shortcuts.
    let keys: Vec<_> = (0_u64..24_000)
        .map(|index| index.to_be_bytes().to_vec())
        .collect();

    assert_context_boundary(&keys, &24_000_u64.to_be_bytes(), "attribution context Apps");
}

#[test]
fn batch_accepts_the_key_arena_bound_and_rejects_one_more_byte() {
    // Legal individual keys fill exactly eight MiB before a distinct one-byte key is refused.
    let mut keys: Vec<_> = (0_u64..2048)
        .map(|index| {
            let mut key = vec![0; collector::ring::PROGRAM_PATH_BYTES_MAX];
            key[..8].copy_from_slice(&index.to_be_bytes());
            key
        })
        .collect();
    keys.push(vec![254; 2048]);
    assert_eq!(keys.iter().map(Vec::len).sum::<usize>(), 8 * 1024 * 1024);

    assert_context_boundary(&keys, &[255], "attribution context key bytes");
}

fn assert_context_boundary(keys: &[Vec<u8>], extra: &[u8], error: &'static str) {
    let history = History::new();
    let cancellation = Arc::new(Cancellation::default());
    let operation = cancellation.operation(0).unwrap();
    let mut batch = Batch::new(
        &history,
        &Selected::ending_at(observation(3)).unwrap(),
        &operation,
    )
    .unwrap();
    let mut capture = crate::app_attribution_history::Capture {
        coverage: crate::live::resident_history::Capture {
            observation: observation(1),
            membership_complete: true,
            ungrouped_members: 0,
            usage_refused: 0,
            usage_gone: 0,
        },
        elapsed_ns: Some(1_000_000_000),
    };
    for state in &mut batch.states {
        state.apply(|reduction| {
            reduction.observe(
                &capture,
                keys.iter().map(|key| boundary_reading(key)),
                Source::Saved,
            )
        });
        assert!(matches!(state, MetricState::Ready(_)));
    }
    capture.coverage.observation = observation(2);

    for state in &mut batch.states {
        state.apply(|reduction| {
            reduction.observe(
                &capture,
                std::iter::once(boundary_reading(extra)),
                Source::Saved,
            )
        });
    }
    let result = batch.finish(&history, None, &operation).unwrap();

    for value in [&result.cpu, &result.power, &result.disk] {
        assert!(matches!(value, Err(Error::Bound(actual)) if *actual == error));
    }
}

fn boundary_reading(key: &[u8]) -> crate::app_attribution_history::Reading<'_> {
    crate::app_attribution_history::Reading {
        key: crate::apps::AppKey::Bundle(key),
        measure: amount(1).measure(),
        members: 1,
        membership_complete: true,
    }
}

impl SavedFixture {
    fn new() -> Box<Self> {
        let mut fixture = Fixture::new();
        fixture.start_at_run_tick(59);
        for minute in 0..3 {
            fixture.publish_with_system_rows(1000 + minute * 2);
            fixture.publish_with_system_rows(1001 + minute * 2);
            if minute < 2 {
                fixture.skip_run_ticks(58);
            }
        }
        for minute in 0..4 {
            fixture.seal_minute(minute);
        }
        let saved = Session::open(&fixture.root).unwrap();
        let run = saved.windows[0].identity.run;
        let selected = Observation {
            recording: run.recording.0,
            stamp: Stamp {
                boot: run.boot.0,
                run: run.run.0,
                publication: 6,
                continuous_ns: Some(180_000_000_000),
                awake_ns: Some(180_000_000_000),
            },
            run_tick: 180,
            wall_ns: Some(1_005_000_000_000),
        };
        let cancellation = Arc::new(Cancellation::default());
        let operation = Arc::clone(&cancellation).operation(0).unwrap();
        let control = History::new();
        let expected = METRICS.map(|metric| {
            control
                .attribution_response(
                    &Selected::ending_at(selected).unwrap(),
                    metric,
                    Some(&saved),
                    &operation,
                )
                .unwrap()
        });
        let rates = control.saved_residents.borrow_mut().take_attribution();
        let limited_bytes = rates.bytes() - rates.pins().last().unwrap().bytes();

        Box::new(Self {
            fixture,
            saved,
            selected,
            cancellation,
            operation,
            expected,
            limited_bytes,
        })
    }

    fn query(&self, history: &History, available: usize) -> Result<Box<AppAttributionHistories>> {
        let mut batch = Batch::new(
            history,
            &Selected::ending_at(self.selected).unwrap(),
            &self.operation,
        )?;
        let available = available.min(BYTES_MAX.saturating_sub(history.bytes));
        history.attribution_batch_saved(
            &SavedQuery {
                selected: &Selected::ending_at(self.selected).unwrap(),
                saved: &self.saved,
                operation: &self.operation,
                available,
            },
            &mut batch,
        )
    }
}

fn distinct_history() -> Box<History> {
    let mut history = Box::new(History::new());
    for publication in [1, 3] {
        let mut point = Box::new(Point {
            observation: observation(publication),
            segment: 0,
            elapsed_ns: Some(1_000_000_000),
            apps: Vec::with_capacity(12),
            groups: None,
            membership_complete: true,
            live_ungrouped_members: 0,
            ungrouped_members: 0,
            usage_refused: 0,
            usage_gone: 0,
        });
        for index in 0..12 {
            let bytes = format!("/Apps/{index:02}.app").into_bytes();
            let key = Key {
                kind: KeyKind::Bundle,
                display: "App".into(),
                bytes,
            };
            let mut previous = 0;
            let zero = amount(0);
            point.apps.push(App {
                key: compact::capture_key(&key, &[], &mut previous),
                members: 1,
                membership_complete: true,
                cpu_ns: amount(100 - index),
                energy_nj: amount(100 - (index + 4) % 12),
                gpu_ns: amount(100 - (index + 6) % 12),
                disk_io_bytes: amount(100 - (index + 8) % 12),
                disk_read_bytes: zero,
                disk_written_bytes: zero,
                resident_bytes: zero,
                footprint_bytes: zero,
            });
        }
        history.push(point, 0);
    }
    history.observations.insert(1, observation(2));

    history
}

fn amount(known: u64) -> CompactMeasure {
    CompactMeasure::capture(&Measure {
        known,
        measured_members: 1,
        unavailable_members: 0,
        overflowed: false,
    })
}

fn observation(publication: u64) -> Observation {
    Observation {
        recording: [1, 2],
        stamp: Stamp {
            boot: [3, 4],
            run: [5, 6],
            publication,
            continuous_ns: Some(publication * 1_000_000_000),
            awake_ns: Some(publication * 1_000_000_000),
        },
        run_tick: publication,
        wall_ns: Some((1000 + publication) * 1_000_000_000),
    }
}

fn assert_batch(batch: &AppAttributionHistories, expected: &[Value; 3]) {
    for (actual, expected) in [&batch.cpu, &batch.power, &batch.disk]
        .into_iter()
        .zip(expected)
    {
        assert_payload(actual.as_ref().unwrap(), expected);
    }
}

fn assert_payload(actual: &Value, expected: &Value) {
    let mut actual = actual.clone();
    let mut expected = expected.clone();
    for payload in [&mut actual, &mut expected] {
        for field in [
            "cache_bytes",
            "saved_cache_limited",
            "saved_observations_cached",
            "saved_observations_uncached",
        ] {
            drop(payload["coverage"].as_object_mut().unwrap().remove(field));
        }
    }
    assert_eq!(
        serde_json::to_vec(&actual).unwrap(),
        serde_json::to_vec(&expected).unwrap()
    );
}

/// Six observations, publications 1 to 6, whose adjacent pairs cross four run minutes, each
/// minute sealed with its Apps' parts where `kept` says so. The measured process's energy and
/// disk bytes grow at different paces, so its power and disk histories differ.
fn kept_fixture(kept: [bool; 4]) -> Fixture {
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    for minute in 0..3 {
        for second in 0..2 {
            let index = minute * 2 + second;
            fixture.publish_with_usage(1000 + index, &kept_usage(index));
        }
        if minute < 2 {
            fixture.skip_run_ticks(58);
        }
    }
    for (minute, kept) in (0..).zip(kept) {
        if kept {
            fixture.seal_minute_derived(minute, &crate::SealedApps::default());
        } else {
            fixture.seal_minute(minute);
        }
    }

    fixture
}

/// Five observations, publications 1 to 5 at run ticks 58 to 60, 119 and 120, with run minute
/// 0, the first two, sealed without its parts. Its first observation has no wall time, so its
/// wall clock bounds are unknown, and its second falls at `second`. The others fall at seconds
/// 2500, 3000 and 3001 and are left to seal.
fn unbounded_fixture(second: u64) -> Fixture {
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(58);
    fixture.publish_with_usage(1, &kept_usage(0));
    fixture.publish_with_usage(second, &kept_usage(1));
    fixture.seal_with_observations(&[no_wall_time(1)]);
    fixture.publish_with_usage(2500, &kept_usage(2));
    fixture.skip_run_ticks(58);
    fixture.publish_with_usage(3000, &kept_usage(3));
    fixture.publish_with_usage(3001, &kept_usage(4));

    fixture
}

/// The observation of `publication` as a collector that knew none of its times sealed it.
fn no_wall_time(publication: u64) -> persistence::Observation {
    persistence::Observation {
        publication,
        wall_ns: None,
        continuous_ns: None,
        awake_ns: None,
    }
}

/// The measured process's usage at the observation of a fixture whose index is `index`, which
/// differs from every other observation's.
fn kept_usage(index: u64) -> Usage {
    Usage {
        awake_ns_at_start: 0,
        user_ns: 1_000_000 * (index + 1),
        system_ns: 3,
        energy_nj: 4 + 7 * index * index,
        instructions: 5,
        super_instructions: 6,
        cycles: 7,
        idle_wakeups: 8,
        interrupt_wakeups: 9,
        pageins: 10,
        resident_bytes: 11,
        footprint_bytes: 12,
        disk_read_bytes: 13 + 3 * index,
        disk_written_bytes: 14 + 2 * index,
    }
}

/// The batch at the last observation of `kept_fixture`, from its saved windows alone.
fn saved_batch(saved: &Session) -> AppAttributionHistories {
    *kept_batch(saved, &History::new(), 6, BYTES_MAX).unwrap()
}

/// The batch `history` gives at publication `publication` of `kept_fixture` from its saved
/// windows alone, with at most `available` bytes of cache, or the error that refuses it.
fn kept_batch(
    saved: &Session,
    history: &History,
    publication: u64,
    available: usize,
) -> Result<Box<AppAttributionHistories>> {
    // The observation of index `index` is the second `1000 + index`, at run tick 59 plus 60
    // for each minute the pairs before it crossed.
    let index = publication - 1;
    let run_tick = 59 + index / 2 * 60 + index % 2;
    let selected = kept_selected(saved, publication, run_tick, 1000 + index);

    saved_batch_at(saved, history, &selected, available)
}

/// The selection ending at the observation of `saved`'s first run whose publication is
/// `publication`, at run tick `run_tick` and wall second `second`.
fn kept_selected(saved: &Session, publication: u64, run_tick: u64, second: u64) -> Selected {
    let run = saved.windows[0].identity.run;
    Selected::ending_at(Observation {
        recording: run.recording.0,
        stamp: Stamp {
            boot: run.boot.0,
            run: run.run.0,
            publication,
            continuous_ns: Some(run_tick * 1_000_000_000),
            awake_ns: Some(run_tick * 1_000_000_000),
        },
        run_tick,
        wall_ns: Some(second * 1_000_000_000),
    })
    .unwrap()
}

/// The batch `history` gives at `selected` from the saved windows of `saved` alone, with at
/// most `available` bytes of cache, or the error that refuses it.
fn saved_batch_at(
    saved: &Session,
    history: &History,
    selected: &Selected,
    available: usize,
) -> Result<Box<AppAttributionHistories>> {
    let cancellation = Arc::new(Cancellation::default());
    let operation = cancellation.operation(0).unwrap();
    let mut batch = Batch::new(history, selected, &operation).unwrap();

    history.attribution_batch_saved(
        &SavedQuery {
            selected,
            saved,
            operation: &operation,
            available: available.min(BYTES_MAX.saturating_sub(history.bytes)),
        },
        &mut batch,
    )
}

/// The batch at publication `publication` of `kept_fixture` from a session opened in place of
/// `session` once each window's parts are of a version this build does not read, and the saved
/// sources that session read.
fn replayed_batch(session: Session, fixture: &Fixture, publication: u64) -> ([Value; 3], u64) {
    let index = publication - 1;
    let run_tick = 59 + index / 2 * 60 + index % 2;
    let selected = kept_selected(&session, publication, run_tick, 1000 + index);

    replayed_batch_at(session, fixture, &selected)
}

/// The batch at `selected` from a session opened in place of `session` once each window's
/// parts are of a version this build does not read, and the saved sources that session read.
fn replayed_batch_at(
    session: Session,
    fixture: &Fixture,
    selected: &Selected,
) -> ([Value; 3], u64) {
    session.unread_kept_parts();
    drop(session);
    let replaying = Session::open(&fixture.root).unwrap();

    let replayed = saved_batch_at(&replaying, &History::new(), selected, BYTES_MAX).unwrap();

    let reads = replaying.source_reads.get();
    assert!(reads > 0);
    (
        [replayed.cpu, replayed.power, replayed.disk].map(Result::unwrap),
        reads,
    )
}

/// What each App of each point of `batch`'s CPU history says of whether its membership is
/// complete, in order.
fn memberships(batch: &AppAttributionHistories) -> Vec<bool> {
    let points = batch.cpu.as_ref().unwrap()["points"].as_array().unwrap();
    points
        .iter()
        .flat_map(|point| point["apps"].as_array().unwrap())
        .filter_map(|app| app["membership_complete"].as_bool())
        .collect()
}

fn retire(fixture: &Fixture) {
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();
}
