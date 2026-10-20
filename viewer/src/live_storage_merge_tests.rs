#[test]
fn restores_committed_storage_rates_over_a_cold_and_reopened_live_tail() {
    // Goal: cold overlapping live points retain intervals whose predecessor is only committed.
    // Method: retain raw publications2..3 after sealing1..3 and compare public saved/live results.
    let mut fixture = storage_fixture_with_slots(2);
    let mut window = storage_merge_window(&fixture, 0);
    storage_publish(&mut fixture, 1, Some(&storage_snapshot(1)));
    storage_merge_stage(&fixture, &mut window, 1);
    storage_publish(&mut fixture, 2, Some(&storage_snapshot(2)));
    storage_merge_stage(&fixture, &mut window, 2);
    storage_publish(&mut fixture, 3, Some(&storage_snapshot(3)));
    storage_merge_stage(&fixture, &mut window, 3);
    seal_window(&fixture.root, &window);
    let mut saved = Session::open(&fixture.root).unwrap();
    let expected = saved.select(1002, 0).unwrap()["summary"]["storage"].clone();

    let mut cold = LiveSession::open(&fixture.root).unwrap();
    let followed = cold.follow_latest().unwrap();
    let selected = cold.select(1002, 0).unwrap();
    drop(cold);
    let mut reopened = LiveSession::open(&fixture.root).unwrap();
    let followed_again = reopened.follow_latest().unwrap();
    let selected_again = reopened.select(1002, 0).unwrap();

    assert_eq!(expected["read_bps"], 4096.0);
    assert_eq!(storage_merge_trend(&followed, 2), &expected);
    assert_eq!(selected["summary"]["storage"], expected);
    assert_eq!(storage_merge_trend(&followed_again, 2), &expected);
    assert_eq!(selected_again["summary"]["storage"], expected);
}

fn storage_merge_window(fixture: &RingFixture, window: u64) -> Window {
    let reader = RingReader::open_generation(fixture.writer.name()).unwrap();
    Window::new(
        WindowIdentity {
            run: fixture.writer.identity().unwrap(),
            window,
        },
        Limits::from_shape(&reader.shape()).unwrap(),
    )
}

fn storage_merge_stage(fixture: &RingFixture, window: &mut Window, publication: u64) {
    let mut reader = RingReader::open_generation(fixture.writer.name()).unwrap();
    reader.seek_publication(publication).unwrap();
    let mut tick = persistence::tick_buffer(&reader.shape()).unwrap();
    assert!(matches!(
        reader.read_into(&mut tick).unwrap(),
        Found::Publication { .. }
    ));
    let times = tick.times.unwrap();
    window
        .push_tick(
            &persistence::Observation {
                publication,
                wall_ns: Some(times.wall_ns),
                continuous_ns: Some(times.continuous_ns),
                awake_ns: Some(times.awake_ns),
            },
            &tick,
            &[],
        )
        .unwrap();
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
fn restores_storage_intervals_when_a_predecessor_minute_commits_late() {
    // Goal: an older late seal restores nested rates in an already overlapping cold live tail.
    // Method: withhold publication1 in minute0, seal2..3 in minute1, then poll its predecessor in.
    let mut fixture = storage_fixture_with_slots(2);
    let mut predecessor = storage_merge_window(&fixture, 0);
    let mut current = storage_merge_window(&fixture, 1);
    storage_publish(&mut fixture, 59, Some(&storage_snapshot(59)));
    storage_merge_stage(&fixture, &mut predecessor, 1);
    storage_publish(&mut fixture, 60, Some(&storage_snapshot(60)));
    storage_merge_stage(&fixture, &mut current, 2);
    storage_publish(&mut fixture, 61, Some(&storage_snapshot(61)));
    storage_merge_stage(&fixture, &mut current, 3);
    seal_window(&fixture.root, &current);
    let mut live = LiveSession::open(&fixture.root).unwrap();
    let before = live.follow_latest().unwrap();
    seal_window(&fixture.root, &predecessor);

    for _poll in 0..256 {
        assert!(!live.poll().unwrap());
    }
    let followed = live.follow_latest().unwrap();
    let selected = live.select(1060, 0).unwrap();
    let mut saved = Session::open(&fixture.root).unwrap();
    let expected = saved.select(1060, 0).unwrap()["summary"]["storage"].clone();

    assert!(storage_merge_trend(&before, 2)["read_bps"].is_null());
    assert_eq!(expected["read_bps"], 4096.0);
    assert_eq!(storage_merge_trend(&followed, 2), &expected);
    assert_eq!(selected["summary"]["storage"], expected);
}

#[test]
fn retains_valid_live_storage_when_its_sealed_copy_has_no_baseline() {
    // Goal: a committed first point cannot replace measured live interval fields with nulls.
    // Method: acquire1..2 live, seal only2, and compare following/selection with the live values.
    let mut fixture = storage_fixture_with_slots(2);
    storage_publish(&mut fixture, 1, Some(&storage_snapshot(1)));
    let mut live = LiveSession::open(&fixture.root).unwrap();
    storage_publish(&mut fixture, 2, Some(&storage_snapshot(2)));
    assert!(live.poll().unwrap());
    let expected = live.follow_latest().unwrap()["summary"]["storage"].clone();
    let mut current = storage_merge_window(&fixture, 0);
    storage_merge_stage(&fixture, &mut current, 2);
    seal_window(&fixture.root, &current);
    for _poll in 0..256 {
        assert!(!live.poll().unwrap());
    }

    let followed = live.follow_latest().unwrap();
    let selected = live.select(1002, 0).unwrap();
    let mut saved = Session::open(&fixture.root).unwrap();
    let unavailable = saved.select(1002, 0).unwrap();

    assert!(unavailable["summary"]["storage"]["read_bps"].is_null());
    assert_eq!(expected["read_bps"], 4096.0);
    assert_eq!(followed["summary"]["storage"], expected);
    assert_eq!(selected["summary"]["storage"], expected);
}

#[test]
fn inspects_committed_storage_rates_over_a_cold_live_tail() {
    // Goal: a hover's read of the first live second holds the rates its saved minute keeps.
    // Method: retain raw publications2..3 after sealing1..3, then inspect 2 as select reads it.
    let mut fixture = storage_fixture_with_slots(2);
    let mut window = storage_merge_window(&fixture, 0);
    storage_publish(&mut fixture, 1, Some(&storage_snapshot(1)));
    storage_merge_stage(&fixture, &mut window, 1);
    storage_publish(&mut fixture, 2, Some(&storage_snapshot(2)));
    storage_merge_stage(&fixture, &mut window, 2);
    storage_publish(&mut fixture, 3, Some(&storage_snapshot(3)));
    storage_merge_stage(&fixture, &mut window, 3);
    seal_window(&fixture.root, &window);
    let mut saved = Session::open(&fixture.root).unwrap();
    let expected = saved.select(1002, 0).unwrap()["summary"]["storage"].clone();
    let mut cold = LiveSession::open(&fixture.root).unwrap();
    let identity = cold.select(1002, 0).unwrap()["selected"].clone();
    cold.select(1003, 0).unwrap();

    let inspected = cold.inspect(1002, &identity).unwrap();

    assert_eq!(expected["read_bps"], 4096.0);
    assert_eq!(inspected["summary"]["storage"], expected);
}
