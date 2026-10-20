use super::*;
use crate::live::identity_fixture::Fixture;
use crate::live_app_response::Metric;

#[test]
fn a_window_keeps_the_ranks_the_replay_of_its_minute_gives() {
    // Goal: the ranks a window keeps from its seal are those the replay of its minute gives.
    // Method: seal three observations with their ranks, and compare the ranks the window keeps
    // with those a replay of its minute gives.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    fixture.publish_with_system_rows(1002);
    fixture.seal_derived(&[], &SealedRanks);
    let session = Session::open(&fixture.root).unwrap();

    let kept = kept_ranks(&session, 1002);

    assert_eq!(kept.len(), 3);
    assert_eq!(kept, replayed_ranks(&session, &kept[2]));
}

#[test]
fn a_window_keeps_the_rates_its_first_observation_takes_from_the_minute_before() {
    // Goal: the first observation of a window ranks by the rates it takes from the last
    // observation of the sealed minute before it, as the replay ranks it.
    // Method: seal run ticks 59 and 60 in two minutes with their ranks, and compare the CPU
    // ranks of tick 60, which need a rate, with those a replay of both minutes gives.
    let mut fixture = Fixture::new();
    fixture.start_at_run_tick(59);
    fixture.publish_with_system_rows(1059);
    fixture.publish_with_system_rows(1060);
    fixture.seal_minute_derived(0, &SealedRanks);
    fixture.seal_minute_derived(1, &SealedRanks);
    let session = Session::open(&fixture.root).unwrap();

    let kept = kept_ranks(&session, 1060);

    assert_eq!(kept.len(), 2);
    assert_eq!(kept[1].metrics[0].metric, Metric::Cpu);
    assert!(!kept[1].metrics[0].entries.is_empty());
    assert_eq!(kept, replayed_ranks(&session, &kept[1]));
}

#[test]
fn a_window_sealed_without_ranks_keeps_none() {
    // Goal: a window sealed before the persister derived ranks leaves its observations to the
    // replay.
    // Method: seal two observations without their ranks and read the ranks their window keeps.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    fixture.seal();
    let session = Session::open(&fixture.root).unwrap();

    let kept = kept_ranks(&session, 1001);

    assert!(kept.is_empty());
}

#[test]
fn a_window_keeps_no_ranks_from_a_table_of_another_version() {
    // Goal: a build reads no ranks from a table a later build wrote in columns it may not know,
    // and leaves the window's observations to the replay.
    // Method: rewrite a sealed window's ranks with the next version, keeping its rows, and read
    // the ranks the window keeps.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    fixture.seal_derived(&[], &SealedRanks);
    let session = Session::open(&fixture.root).unwrap();
    rewrite_ranks(&session, |frame| {
        let version = vec![VERSION + 1; frame.height()];
        frame
            .replace("version", Column::new("version".into(), version))
            .unwrap();
    });

    let kept = kept_ranks(&session, 1001);

    assert!(kept.is_empty());
}

#[test]
fn a_window_whose_ranks_lack_a_row_its_seal_counted_is_refused() {
    // Goal: ranks that lost a row since the window sealed are an error, not a shorter ranking.
    // Method: rewrite a sealed window's ranks without their last row, and read them.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    fixture.seal_derived(&[], &SealedRanks);
    let session = Session::open(&fixture.root).unwrap();
    rewrite_ranks(&session, |frame| {
        *frame = frame.head(Some(frame.height() - 1));
    });

    let result = session.visit_sealed_ranks(range(1001), |_ranks| Ok(()));

    assert!(matches!(result, Err(Error::Bound("sealed ranks rows"))));
}

#[test]
fn a_window_whose_ranks_skip_a_position_is_refused() {
    // Goal: a ranking with a gap in its positions is an error, not a ranking that misplaces
    // its Apps.
    // Method: rewrite a sealed window's ranks with the first ranked App moved to the next
    // position, and read them.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    fixture.seal_derived(&[], &SealedRanks);
    let session = Session::open(&fixture.root).unwrap();
    rewrite_ranks(&session, |frame| {
        let positions: Vec<Option<u8>> = (0..frame.height())
            .map(|index| {
                let position: Option<u8> = Row::new(frame, index).unwrap().get(13).unwrap();
                position.map(|value| value + 1)
            })
            .collect();
        frame
            .replace("position", Column::new("position".into(), positions))
            .unwrap();
    });

    let result = session.visit_sealed_ranks(range(1001), |_ranks| Ok(()));

    assert!(matches!(result, Err(Error::Bound("sealed ranks position"))));
}

/// The ranks the windows of second `second`'s context keep, in publication order.
fn kept_ranks(session: &Session, second: u64) -> Vec<Ranks> {
    let mut kept = Vec::new();
    session
        .visit_sealed_ranks(range(second), |ranks| {
            kept.push(ranks);
            Ok(())
        })
        .unwrap();
    kept.sort_by_key(|ranks| ranks.selected.stamp.publication);

    kept
}

/// The ranks a replay of the saved minutes of `selected`'s context gives, in publication order.
fn replayed_ranks(session: &Session, selected: &Ranks) -> Vec<Ranks> {
    let mut replayed = Vec::new();
    session
        .visit_app_attribution(
            &selected.selected,
            |_observation| Ok(false),
            |frame, previous, _windows| {
                let mut sample = None;
                Sample::capture(frame, previous, &mut sample)?;
                replayed.push(sample.unwrap().ranks()?);
                Ok(())
            },
            |_pin| Ok(()),
        )
        .unwrap();

    replayed
}

fn range(second: u64) -> ContextRange {
    ContextRange {
        first_ns: (second - 899) * 1_000_000_000,
        end_ns: (second + 1) * 1_000_000_000,
    }
}

/// Rewrites the only sealed window's ranks in place through `change`.
fn rewrite_ranks(session: &Session, change: impl FnOnce(&mut DataFrame)) {
    let path = session
        .root
        .join(&session.windows[0].data_directory)
        .join(format!("{APP_RANKS}.parquet"));
    let mut frame = persistence::engine::scan(&path).unwrap().collect().unwrap();
    change(&mut frame);
    let staging = session.root.with_extension("ranks");
    std::fs::create_dir(&staging).unwrap();

    persistence::encode_derived(APP_RANKS, &mut frame, &staging).unwrap();

    std::fs::copy(staging.join(format!("{APP_RANKS}.parquet")), &path).unwrap();
    std::fs::remove_dir_all(&staging).unwrap();
}
