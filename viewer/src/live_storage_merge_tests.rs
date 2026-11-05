#[test]
fn restores_committed_storage_rates_over_a_cold_and_reopened_live_tail() {
    // Goal: cold overlapping live points retain intervals whose predecessor is only committed.
    // Method: save publication 1 in its minute and 2..3 in the next, which leaves the ring holding
    // 2..3 and 2's predecessor only saved, then compare public saved and live results.
    let mut fixture = storage_fixture_with_slots(2);
    storage_publish_across_a_minute(&mut fixture);
    let mut saved = Session::open(&fixture.root).unwrap();
    let expected = saved.select(1060, 0).unwrap()["summary"]["storage"].clone();

    let mut cold = LiveSession::open(&fixture.root).unwrap();
    let followed = cold.follow_latest().unwrap();
    let selected = cold.select(1060, 0).unwrap();
    drop(cold);
    let mut reopened = LiveSession::open(&fixture.root).unwrap();
    let followed_again = reopened.follow_latest().unwrap();
    let selected_again = reopened.select(1060, 0).unwrap();

    assert_eq!(expected["read_bps"], 4096.0);
    assert_eq!(storage_merge_trend(&followed, 2), &expected);
    assert_eq!(selected["summary"]["storage"], expected);
    assert_eq!(storage_merge_trend(&followed_again, 2), &expected);
    assert_eq!(selected_again["summary"]["storage"], expected);
}

/// Publishes seconds 59, 60 and 61 into a ring of two slots, saving the second before the minute
/// turns, then what the ring holds of the next.
fn storage_publish_across_a_minute(fixture: &mut RingFixture) {
    storage_publish(fixture, 59, Some(&storage_snapshot(59)));
    fixture.seal_through(0);
    storage_publish(fixture, 60, Some(&storage_snapshot(60)));
    storage_publish(fixture, 61, Some(&storage_snapshot(61)));
    fixture.seal_through(1);
}

fn storage_merge_trend(response: &Value, publication: u64) -> &Value {
    &response["trends"]
        .as_array()
        .unwrap()
        .iter()
        .find(|point| point["publication"] == publication)
        .unwrap()["storage"]
}

#[test]
fn retains_valid_live_storage_when_its_sealed_copy_has_no_baseline() {
    // Goal: a committed first point cannot replace measured live interval fields with nulls.
    // Method: acquire 1..2 live, let the two-slot ring drop 1 before the persister saves 2..3, and
    // compare following and selection with the live values.
    let mut fixture = storage_fixture_with_slots(2);
    storage_publish(&mut fixture, 1, Some(&storage_snapshot(1)));
    let mut live = LiveSession::open(&fixture.root).unwrap();
    storage_publish(&mut fixture, 2, Some(&storage_snapshot(2)));
    assert!(live.poll().unwrap());
    let expected = live.select(1002, 0).unwrap()["summary"]["storage"].clone();
    storage_publish(&mut fixture, 3, Some(&storage_snapshot(3)));
    assert!(live.poll().unwrap());
    fixture.seal_held();
    for _poll in 0..256 {
        assert!(!live.poll().unwrap());
    }

    let followed = live.follow_latest().unwrap();
    let selected = live.select(1002, 0).unwrap();
    let mut saved = Session::open(&fixture.root).unwrap();
    let unavailable = saved.select(1002, 0).unwrap();

    assert!(unavailable["summary"]["storage"]["read_bps"].is_null());
    assert_eq!(expected["read_bps"], 4096.0);
    assert_eq!(storage_merge_trend(&followed, 2), &expected);
    assert_eq!(selected["summary"]["storage"], expected);
}

#[test]
fn inspects_committed_storage_rates_over_a_cold_live_tail() {
    // Goal: a hover's read of the first live second holds the rates its saved minute keeps.
    // Method: save publication 1 in its minute and 2..3 in the next, which leaves the ring holding
    // 2..3 and 2's predecessor only saved, then inspect 2 as select reads it.
    let mut fixture = storage_fixture_with_slots(2);
    storage_publish_across_a_minute(&mut fixture);
    let mut saved = Session::open(&fixture.root).unwrap();
    let expected = saved.select(1060, 0).unwrap()["summary"]["storage"].clone();
    let mut cold = LiveSession::open(&fixture.root).unwrap();
    let identity = cold.select(1060, 0).unwrap()["selected"].clone();
    cold.select(1061, 0).unwrap();

    let inspected = cold.inspect(1060, &identity).unwrap();

    assert_eq!(expected["read_bps"], 4096.0);
    assert_eq!(inspected["summary"]["storage"], expected);
}
