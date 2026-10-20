use serde_json::{Value, json};

use super::*;
use crate::live::identity_fixture::Fixture;
use crate::live::window::fixture::{damage, observation_key, saved_in_two_minutes, saved_only};

#[test]
fn rank_window_ranks_the_saved_observations_of_the_window() {
    // Goal: a hover finds the ranks of a saved second the session never saw live, the same
    // ranks the selected second's own sample gives.
    // Method: select the later of two seconds only a saved minute holds, and rank its window.
    let fixture = saved_only();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let selected = session.select(1001, 0).unwrap()["selected"].clone();
    let expected = selected_ranks(&session);
    assert!(
        session
            .app_ranks()
            .get(&observation_key(&selected))
            .is_none()
    );

    session.rank_window().unwrap();

    assert_eq!(
        summary(
            session
                .app_ranks()
                .get(&observation_key(&selected))
                .unwrap()
        ),
        expected
    );
}

#[test]
fn rank_window_does_not_replay_a_window_it_has_ranked() {
    // Goal: a window ranks once, so later reads cost no saved minutes.
    // Method: rank a saved window twice and count the saved source reads of the second time.
    let fixture = saved_only();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1001, 0).unwrap();
    session.rank_window().unwrap();
    let before = session.history.as_ref().unwrap().source_reads.get();

    session.rank_window().unwrap();

    assert_eq!(session.history.as_ref().unwrap().source_reads.get(), before);
}

#[test]
fn rank_window_does_not_replay_a_window_whose_replay_failed() {
    // Goal: a saved minute that cannot be read costs one replay, not one on every read.
    // Method: select a second of the later of two saved minutes, damage the earlier minute,
    // rank the window twice, and count the saved source reads of the second time.
    let (fixture, earlier) = saved_in_two_minutes();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1061, 0).unwrap();
    damage(&earlier);
    assert!(session.rank_window().is_err());
    let before = session.history.as_ref().unwrap().source_reads.get();

    session.rank_window().unwrap();

    assert_eq!(session.history.as_ref().unwrap().source_reads.get(), before);
}

#[test]
fn rank_window_keeps_sharing_the_ranks_of_a_covered_window() {
    // Goal: a read of a window already ranked leaves the ranks the desktop shares uncopied.
    // Method: rank a saved window, take its ranks, and rank it again.
    let fixture = saved_only();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1001, 0).unwrap();
    session.rank_window().unwrap();
    let shared = session.app_ranks();

    session.rank_window().unwrap();

    assert!(Arc::ptr_eq(&shared, &session.app_ranks()));
}

#[test]
fn rank_window_ranks_the_cached_live_observations_of_a_window_it_returns_to() {
    // Goal: returning from an earlier window ranks the live seconds no saved minute holds yet.
    // Method: rank an earlier saved window, which drops the live window's ranks, then follow
    // the live seconds again and rank their window.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.seal();
    fixture.publish_with_system_rows(2000);
    fixture.publish_with_system_rows(2001);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    session.select(1000, 0).unwrap();
    session.rank_window().unwrap();
    let latest = session.follow_latest().unwrap()["selected"].clone();
    assert!(session.app_ranks().get(&observation_key(&latest)).is_none());

    session.rank_window().unwrap();

    assert!(session.app_ranks().get(&observation_key(&latest)).is_some());
}

#[test]
fn poll_ranks_each_live_observation_from_its_sample() {
    // Goal: a live second has its ranks as soon as the session polls it.
    // Method: open a session over two live seconds and compare the latest one's ranks with the
    // ranks of its sample.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);

    let mut session = LiveSession::open(&fixture.root).unwrap();

    let selected = session.follow_latest().unwrap()["selected"].clone();
    assert_eq!(
        summary(
            session
                .app_ranks()
                .get(&observation_key(&selected))
                .unwrap()
        ),
        selected_ranks(&session)
    );
}

#[test]
fn poll_ranks_drops_observations_older_than_the_window() {
    // Goal: the ranks stay bounded to the window a hover can reach.
    // Method: poll a live second, then one 900 seconds later, and look the first one up.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let first = session.follow_latest().unwrap()["selected"].clone();
    assert!(session.app_ranks().get(&observation_key(&first)).is_some());
    fixture.publish_with_system_rows(1900);

    session.poll().unwrap();

    assert!(session.app_ranks().get(&observation_key(&first)).is_none());
}

fn selected_ranks(session: &LiveSession) -> Value {
    let ranks = summary(&session.selected_ranks().unwrap());
    assert!(
        !ranks["metrics"][0]["entries"]
            .as_array()
            .unwrap()
            .is_empty()
    );

    ranks
}

fn summary(ranks: &Ranks) -> Value {
    let metrics = ranks
        .metrics
        .iter()
        .map(|ranking| {
            let entries = ranking
                .entries
                .iter()
                .map(|rank| json!([rank.app.display_name, rank.amount, rank.rate]))
                .collect::<Vec<_>>();
            json!({"entries":entries,"unavailable":ranking.unavailable,
                "overflowed":ranking.overflowed,"partial":ranking.partial})
        })
        .collect::<Vec<_>>();

    json!({"app_count":ranks.app_count,"metrics":metrics})
}
