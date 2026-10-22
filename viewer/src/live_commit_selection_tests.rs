use crate::live::identity_fixture::Fixture;
use polars::prelude::{Column, DataFrame, ParquetCompression, ParquetWriter, StatisticsOptions};

#[test]
fn a_commit_restores_the_selected_tick_without_rebuilding_scalar_context() {
    // Goal: a commit handoff reads only its exact selected tick, preserving raw page identity.
    // Method: select a real live observation, seal it with known clock bounds, then poll publicly.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    let mut live = LiveSession::open(&fixture.root).unwrap();
    let selection = live.follow_latest().unwrap();
    let page = live.page("ticks", 0).unwrap();
    fixture.seal();

    assert!(!live.poll().unwrap());

    assert_eq!(live.history.as_ref().unwrap().source_reads.get(), 1);
    live.poll_evict(u64::MAX).unwrap();
    assert_eq!(live.page("ticks", 0).unwrap()["rows"], page["rows"]);
    assert!(same(
        &live.page("ticks", 0).unwrap()["observation"],
        &selection["selected"]
    ));
}

#[test]
fn a_commit_before_the_selection_loads_no_selected_tick_or_scalar_context() {
    // Goal: a selection newer than all commits needs no saved-row handoff queries.
    // Method: seal the first observation, then select the next live observation before polling.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.seal();
    let marker = persistence::layout::WindowKey::from(&WindowIdentity {
        run: fixture.writer.identity().unwrap(),
        window: 0,
    })
    .marker(&fixture.root);
    let hidden = fixture.root.join("withheld.seal");
    std::fs::rename(&marker, &hidden).unwrap();
    let mut live = LiveSession::open(&fixture.root).unwrap();
    fixture.publish_with_system_rows(1001);
    live.poll().unwrap();
    live.follow_latest().unwrap();
    std::fs::rename(hidden, marker).unwrap();

    assert!(!live.poll().unwrap());

    assert_eq!(live.history.as_ref().unwrap().source_reads.get(), 0);
    live.poll_evict(u64::MAX).unwrap();
    assert!(live.page("ticks", 0).is_err());
}

#[test]
fn repeated_commit_handoffs_preserve_the_nonfirst_same_second_observation() {
    // Goal: paused raw fallback must retain an exact repeated-second identity across refreshes.
    // Method: seal two siblings, evict raw rows, then seal a later minute and compare tick pages.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1000);
    let mut live = LiveSession::open(&fixture.root).unwrap();
    let selected = live.select(1000, 1).unwrap();
    assert_eq!(selected["selected"]["publication"], 2);
    let before = live.page("ticks", 0).unwrap();
    fixture.seal();

    assert!(!live.poll().unwrap());
    live.poll_evict(u64::MAX).unwrap();
    let first = live.page("ticks", 0).unwrap();
    fixture.skip_run_ticks(58);
    fixture.publish_with_system_rows(1060);
    fixture.seal_minute(1);
    assert!(live.poll().unwrap());
    let second = live.page("ticks", 0).unwrap();

    assert_eq!(first["rows"], before["rows"]);
    assert_eq!(second["rows"], before["rows"]);
    assert!(same(&first["observation"], &selected["selected"]));
    assert!(same(&second["observation"], &selected["selected"]));
    assert_eq!(
        second["observation"]["run_tick"],
        selected["selected"]["run_tick"]
    );
}

#[test]
fn a_covering_commit_with_changed_wall_time_preserves_the_previous_history() {
    // Goal: changed stored clocks must fail a handoff instead of appearing not yet committed.
    // Method: override only the selected stored wall clock and compare the retained history.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    drop(persistence::Store::open(&fixture.root).unwrap());
    let mut live = LiveSession::open(&fixture.root).unwrap();
    let before = live.follow_latest().unwrap();
    fixture.seal_with_observations(&[persistence::Observation {
        publication: 2,
        wall_ns: Some(1002 * NANOSECONDS_PER_SECOND),
        continuous_ns: Some(NANOSECONDS_PER_SECOND),
        awake_ns: Some(NANOSECONDS_PER_SECOND),
    }]);

    assert!(!live.poll().unwrap());

    assert_eq!(live.history.as_ref().unwrap().catalog()["windows"], 0);
    assert!(live.history_error.is_some());
    assert_eq!(live.selected.as_ref(), Some(&before["selected"]));
    assert_eq!(
        live.page("ticks", 0).unwrap()["observation"],
        before["selected"]
    );
}

#[test]
fn a_covering_commit_with_a_clockless_selected_tick_preserves_the_previous_history() {
    // Goal: a covering publication with no stored wall clock must fail atomically.
    // Method: seal a clockless selected tick beside a known-clock predecessor.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    drop(persistence::Store::open(&fixture.root).unwrap());
    let mut live = LiveSession::open(&fixture.root).unwrap();
    let selected = live.follow_latest().unwrap();
    fixture.seal_with_observations(&[persistence::Observation {
        publication: 2,
        wall_ns: None,
        continuous_ns: Some(NANOSECONDS_PER_SECOND),
        awake_ns: Some(NANOSECONDS_PER_SECOND),
    }]);

    assert!(!live.poll().unwrap());

    assert_eq!(live.history.as_ref().unwrap().catalog()["windows"], 0);
    assert!(live.history_error.is_some());
    assert_eq!(live.selected.as_ref(), Some(&selected["selected"]));
}

#[test]
fn a_covering_commit_with_a_missing_selected_tick_preserves_the_previous_history() {
    // Goal: a covering manifest cannot turn a missing saved row into an unselected success.
    // Method: remove only the selected row from a real commit, then compare public state.
    let mut handoff = CommitHandoff::new();
    handoff.corrupt_ticks(|ticks| *ticks = ticks.slice(0, 1));

    assert!(!handoff.live.poll().unwrap());

    handoff.assert_previous_history();
}

#[test]
fn a_covering_commit_with_a_duplicate_selected_tick_preserves_the_previous_history() {
    // Goal: an ambiguous saved publication cannot silently select its first duplicate.
    // Method: duplicate only the selected saved row, then compare public state.
    let mut handoff = CommitHandoff::new();
    handoff.corrupt_ticks(|ticks| {
        let selected = ticks.slice(1, 1);
        ticks.vstack_mut(&selected).unwrap();
    });

    assert!(!handoff.live.poll().unwrap());

    handoff.assert_previous_history();
}

#[test]
fn a_covering_commit_with_a_changed_run_tick_preserves_the_previous_history() {
    // Goal: publication identity also requires the original scheduled run tick.
    // Method: change only the selected saved run tick, then compare public state.
    let mut handoff = CommitHandoff::new();
    handoff.corrupt_ticks(|ticks| {
        ticks
            .with_column(Column::new("run_tick".into(), [0_u64, 2]))
            .unwrap();
    });

    assert!(!handoff.live.poll().unwrap());

    handoff.assert_previous_history();
}

#[test]
fn a_covering_commit_with_a_changed_subsecond_clock_preserves_the_previous_history() {
    // Goal: matching whole seconds cannot conceal a changed selected wall clock.
    // Method: change only the selected saved clock by one nanosecond, then compare public state.
    let mut handoff = CommitHandoff::new();
    handoff.corrupt_ticks(|ticks| {
        ticks
            .with_column(Column::new(
                "wall_ns".into(),
                [
                    1000 * NANOSECONDS_PER_SECOND,
                    1001 * NANOSECONDS_PER_SECOND + 1,
                ],
            ))
            .unwrap();
    });

    assert!(!handoff.live.poll().unwrap());

    handoff.assert_previous_history();
}

#[test]
fn duplicate_covering_commits_preserve_the_previous_history() {
    // Goal: two saved windows covering the same publication cannot pick one by directory order.
    // Method: commit a second table set covering that publication, then compare public state.
    let mut handoff = CommitHandoff::new();
    handoff.duplicate_commit();

    assert!(!handoff.live.poll().unwrap());

    handoff.assert_previous_history();
}

#[test]
fn selecting_a_second_a_listed_minute_holds_leaves_the_listing_as_it_is() {
    // Goal: a second whose readings a listed minute holds needs no new listing to be read, so
    // a launch, whose ring has nothing yet, does not list every saved minute a second time.
    // Method: seal a minute the session has not listed, empty the ring's readings, then select
    // a second of the minute the session listed and count the windows it lists.
    let (_fixture, mut live) = open_before_an_unlisted_minute();

    live.select(1001, 0).unwrap();

    assert_eq!(live.catalog()["windows"], 1);
}

#[test]
fn selecting_a_second_no_listed_minute_holds_lists_the_saved_minutes_again() {
    // Goal: a second the ring no longer holds is looked for in minutes sealed since the listing.
    // Method: seal a minute the session has not listed, empty the ring's readings, then select
    // a second of that minute and count the windows it lists.
    let (_fixture, mut live) = open_before_an_unlisted_minute();

    live.select(1061, 0).unwrap();

    assert_eq!(live.catalog()["windows"], 2);
}

#[test]
fn selecting_the_second_before_a_listed_minute_lists_the_saved_minutes_again() {
    // Goal: a listed minute holds only the seconds its readings fall in, not the one before.
    // Method: seal a minute the session has not listed, empty the ring's readings, then select
    // the second before the listed minute's only reading and count the windows it lists.
    let (_fixture, mut live) = open_before_an_unlisted_minute();

    live.select(1000, 0).unwrap();

    assert_eq!(live.catalog()["windows"], 2);
}

/// A session that listed minute 0, whose ring holds no readings, with minute 1 sealed since.
fn open_before_an_unlisted_minute() -> (Fixture, LiveSession) {
    let mut fixture = Fixture::new();
    fixture.publish(1001);
    fixture.seal_minute(0);
    let mut live = LiveSession::open(&fixture.root).unwrap();
    fixture.skip_run_ticks(59);
    fixture.publish(1061);
    fixture.seal_minute(1);
    live.poll_evict(u64::MAX).unwrap();
    assert_eq!(live.catalog()["windows"], 1);
    (fixture, live)
}

struct CommitHandoff {
    fixture: Fixture,
    live: LiveSession,
    catalog: Value,
    page: Value,
}

impl CommitHandoff {
    fn new() -> Self {
        let mut fixture = Fixture::new();
        fixture.publish_with_system_rows(1000);
        fixture.publish_with_system_rows(1001);
        drop(persistence::Store::open(&fixture.root).unwrap());
        let mut live = LiveSession::open(&fixture.root).unwrap();
        let selected = live.follow_latest().unwrap();
        assert_eq!(selected["selected"]["publication"], 2);
        let catalog = live.catalog();
        let page = live.page("ticks", 0).unwrap();
        fixture.seal();

        Self {
            fixture,
            live,
            catalog,
            page,
        }
    }

    fn manifest(&self) -> persistence::Manifest {
        let marker = persistence::layout::WindowKey::from(&WindowIdentity {
            run: self.fixture.writer.identity().unwrap(),
            window: 0,
        })
        .marker(&self.fixture.root);
        persistence::read_committed(&self.fixture.root, &marker).unwrap()
    }

    fn corrupt_ticks(&self, corrupt: impl FnOnce(&mut DataFrame)) {
        let manifest = self.manifest();
        let directory = self.fixture.root.join(&manifest.data_directory);
        let path = directory.join("ticks.parquet");
        let mut ticks = {
            let connection = Engine::new();
            let source = connection.parquet_source(&path).unwrap();
            connection
                .query(&format!(
                    "SELECT * FROM {source} ORDER BY publication LIMIT 3"
                ))
                .unwrap()
        };
        assert_eq!(ticks.height(), 2);
        corrupt(&mut ticks);
        assert!(ticks.height() <= 3);
        let replacement = directory.join("corrupt-ticks.parquet");
        let bytes = ParquetWriter::new(private::open_private_file(&replacement, true).unwrap())
            .with_compression(ParquetCompression::Uncompressed)
            .with_statistics(StatisticsOptions {
                min_value: true,
                max_value: true,
                distinct_count: false,
                null_count: true,
                binary_statistics_truncate_length: None,
            })
            .with_row_group_size(Some(3))
            .with_data_page_size(None)
            .set_parallel(false)
            .with_key_value_metadata(None)
            .with_context_info(None)
            .finish(&mut ticks)
            .unwrap();
        assert!(bytes > 0);
        std::fs::rename(replacement, path).unwrap();
        assert_eq!(self.manifest(), manifest);
    }

    fn duplicate_commit(&self) {
        let mut manifest = self.manifest();
        let directory = self.fixture.root.join(&manifest.data_directory);
        manifest.identity.window = 1;
        let store = persistence::Store::open(&self.fixture.root).unwrap();
        let persistence::Begin::Pending(pending) = store.begin(&manifest.identity).unwrap() else {
            panic!("second fixture window already committed");
        };
        assert_eq!(manifest.tables.len(), persistence::schema::TABLES.len());
        for table in &manifest.tables {
            let name = format!("{}.parquet", table.name);
            std::fs::copy(directory.join(&name), pending.path().join(name)).unwrap();
        }
        store.commit(pending, manifest).unwrap();
    }

    fn assert_previous_history(&self) {
        let mut catalog = self.live.catalog();
        assert!(catalog["history_error"].is_string());
        catalog["history_error"] = Value::Null;
        assert_eq!(catalog, self.catalog);
        assert_eq!(self.live.page("ticks", 0).unwrap(), self.page);
    }
}
