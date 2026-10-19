use crate::AppAttributionMetric;
use crate::live::identity_fixture::Fixture;

const METRICS: [AppAttributionMetric; 3] = [
    AppAttributionMetric::Cpu,
    AppAttributionMetric::Power,
    AppAttributionMetric::Disk,
];

#[test]
fn batch_warm_gathers_saved_ticks_once_and_preserves_all_payloads() {
    // One real saved window gives an exact gather boundary rather than a timing assertion.
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
    let mut session = crate::LiveSession::open(&fixture.root).unwrap();
    session.select(1001, 0).unwrap();
    let expected = METRICS.map(|metric| session.app_attribution_history(metric).unwrap());
    let before = session.history.as_ref().unwrap().source_reads.get();
    for metric in METRICS {
        session.app_attribution_history(metric).unwrap();
    }
    let control_reads = session.history.as_ref().unwrap().source_reads.get() - before;
    let before = session.history.as_ref().unwrap().source_reads.get();

    let batch = session.app_attribution_histories().unwrap();

    let batch_reads = session.history.as_ref().unwrap().source_reads.get() - before;
    assert_eq!(control_reads, 3);
    assert_eq!(batch_reads, 1);
    for (actual, expected) in [&batch.cpu, &batch.power, &batch.disk]
        .into_iter()
        .zip(expected)
    {
        let actual = actual.as_ref().unwrap();
        assert_eq!(actual["selected"], expected["selected"]);
        assert_eq!(actual["cohort"], expected["cohort"]);
        assert_eq!(actual["points"], expected["points"]);
    }
}

#[test]
fn batch_public_supersession_refuses_old_output_and_accepts_a_new_selection() {
    // A canceled public operation cannot use its warm cache until a fresh epoch is selected.
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
    let mut session = crate::LiveSession::open(&fixture.root).unwrap();
    session.select(1001, 0).unwrap();
    session.app_attribution_histories().unwrap();
    session.cancellation.cancel().unwrap();

    let result = session.app_attribution_histories();

    assert!(matches!(result, Err(persistence::Error::Io(error))
        if error.kind() == std::io::ErrorKind::Interrupted));
    session.begin(session.cancellation.epoch()).unwrap();
    session.select(1001, 0).unwrap();
    let retry = session.app_attribution_histories().unwrap();
    assert!(retry.cpu.is_ok());
    assert!(retry.power.is_ok());
    assert!(retry.disk.is_ok());
}

#[test]
fn batch_public_overlap_keeps_live_precedence_and_missing_selection_withholds_output() {
    // Sealing retained live facts must not duplicate rates; selecting a gap clears the input.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    let mut session = crate::LiveSession::open(&fixture.root).unwrap();
    session.follow_latest().unwrap();
    let before = session.app_attribution_histories().unwrap();
    fixture.seal();
    session.poll().unwrap();
    session.follow_latest().unwrap();

    let after = session.app_attribution_histories().unwrap();

    for (before, after) in [&before.cpu, &before.power, &before.disk].into_iter().zip([
        &after.cpu,
        &after.power,
        &after.disk,
    ]) {
        let before = before.as_ref().unwrap();
        let after = after.as_ref().unwrap();
        assert_eq!(before["selected"], after["selected"]);
        assert_eq!(before["cohort"], after["cohort"]);
        assert_eq!(before["points"], after["points"]);
        assert_eq!(after["coverage"]["committed_observations"], 2);
    }
    session.select(2000, 0).unwrap();
    assert!(session.app_attribution_histories().is_err());
}
