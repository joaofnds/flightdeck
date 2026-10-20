use serde_json::{Value, json};

use super::*;
use crate::live::identity_fixture::Fixture;

#[test]
fn rank_window_ranks_the_saved_observations_of_the_window() {
    // Goal: a hover finds the ranks of a saved second the session never saw live, the same
    // ranks the selected second's own sample gives.
    // Method: select the later of two seconds only a saved minute holds, and rank its window.
    let fixture = saved_only();
    let mut session = LiveSession::open(&fixture.root).unwrap();
    let selected = session.select(1001, 0).unwrap()["selected"].clone();
    let expected = selected_ranks(&session);
    assert!(session.app_ranks().get(&key(&selected)).is_none());

    session.rank_window().unwrap();

    assert_eq!(
        summary(session.app_ranks().get(&key(&selected)).unwrap()),
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
    assert!(session.app_ranks().get(&key(&latest)).is_none());

    session.rank_window().unwrap();

    assert!(session.app_ranks().get(&key(&latest)).is_some());
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
        summary(session.app_ranks().get(&key(&selected)).unwrap()),
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
    assert!(session.app_ranks().get(&key(&first)).is_some());
    fixture.publish_with_system_rows(1900);

    session.poll().unwrap();

    assert!(session.app_ranks().get(&key(&first)).is_none());
}

/// Two sealed seconds of a recording whose registry lists no live run, so a session reads
/// them only from their saved minute.
fn saved_only() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    fixture.seal();
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();

    fixture
}

fn selected_ranks(session: &LiveSession) -> Value {
    let ranks = session
        .with_selected_apps(|sample| Ok(summary(&sample.ranks()?)))
        .unwrap();
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

fn key(selected: &Value) -> ObservationKey {
    ObservationKey {
        run: collector::recording::parse_identity(selected["run"].as_str().unwrap())
            .unwrap()
            .0,
        boot: collector::recording::parse_identity(selected["boot"].as_str().unwrap())
            .unwrap()
            .0,
        publication: selected["publication"].as_u64().unwrap(),
    }
}
