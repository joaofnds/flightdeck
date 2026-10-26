use super::{LoadBuffers, load_window};
use crate::live::identity_fixture::Fixture;
use crate::store::apps::TickApps;
use crate::store::{AppsSecond, MachineSecond};
use collector::ring::RunIdentity;

#[test]
fn reloading_reuses_app_decode_storage_without_retaining_previous_rows() {
    // Goal: repeated reconciliation reuses decoder capacity without keeping decoded rows alive.
    // Method: load real App parts twice through oversized, seeded storage and compare fresh output.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    fixture.seal_minute_derived(0, &persistence::sealed_apps::SealedApps::default());
    let expected = capture(&fixture, &mut LoadBuffers::default());
    assert_eq!(expected.len(), 2);
    assert!(!expected[1].2.apps.is_empty());
    let mut buffers = LoadBuffers::default();
    buffers.apps[0].apps.reserve_exact(4_096);
    buffers.apps[0].keys.reserve_exact(8_192);
    buffers.apps[0].keys.extend_from_slice(b"previous load");
    let rows = buffers.apps[0].apps.as_ptr();
    let keys = buffers.apps[0].keys.as_ptr();

    for _ in 0..2 {
        assert_eq!(capture(&fixture, &mut buffers), expected);
        assert!(buffers.apps[0].samples.is_empty());
        assert!(buffers.apps[0].samples.capacity() >= expected.len());
        assert!(buffers.apps[0].apps.is_empty());
        assert!(buffers.apps[0].keys.is_empty());
        assert_eq!(buffers.apps[0].apps.as_ptr(), rows);
        assert_eq!(buffers.apps[0].keys.as_ptr(), keys);
    }
}

#[test]
fn decoder_storage_does_not_carry_rows_or_rates_between_recordings() {
    // Goal: retained decode capacity cannot introduce facts from a previous recording.
    // Method: reuse it across two sealed sources with different identities and wall times.
    let mut first = Fixture::new();
    first.publish_with_system_rows(100);
    first.publish_with_system_rows(101);
    first.seal_minute_derived(0, &persistence::sealed_apps::SealedApps::default());
    let mut next = Fixture::new();
    next.publish_with_system_rows(9_000);
    next.seal_minute_derived(0, &persistence::sealed_apps::SealedApps::default());
    let mut buffers = LoadBuffers::default();

    for fixture in [&first, &next, &first] {
        let expected = capture(fixture, &mut LoadBuffers::default());
        assert_eq!(capture(fixture, &mut buffers), expected);
        assert!(!expected[0].1.follows);
        assert_eq!(expected[0].1.cpu_percent, None);
    }
}

#[test]
fn a_refused_minute_cannot_leave_decoded_rows_in_reused_storage() {
    // Goal: a failed reconciliation neither retains old rows nor contaminates its successor.
    // Method: load a real minute, invalidate its tick file, then load another valid recording.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.seal_minute_derived(0, &persistence::sealed_apps::SealedApps::default());
    let mut buffers = LoadBuffers::default();
    assert_eq!(capture(&fixture, &mut buffers).len(), 1);
    let mut found = Vec::new();
    super::find(&fixture.root, &mut found, None).unwrap();
    let manifest = persistence::read_committed(&fixture.root, &found[0].marker).unwrap();
    std::fs::write(
        fixture
            .root
            .join(manifest.data_directory)
            .join("ticks.parquet"),
        [],
    )
    .unwrap();
    let mut callbacks = 0;

    let failure = load_window(
        &fixture.root,
        None,
        crate::session::LOADED_SECONDS,
        &mut buffers,
        |_, _, _| callbacks += 1,
    );

    assert_eq!(failure, Some(crate::store::Failure::Refused));
    assert_eq!(callbacks, 0);
    assert!(buffers.apps[0].samples.is_empty());
    assert!(buffers.apps[0].apps.is_empty());
    assert!(buffers.apps[0].keys.is_empty());
    let mut next = Fixture::new();
    next.publish_with_system_rows(500);
    next.seal_minute_derived(0, &persistence::sealed_apps::SealedApps::default());
    assert_eq!(
        capture(&next, &mut buffers),
        capture(&next, &mut LoadBuffers::default())
    );
}

fn capture(
    fixture: &Fixture,
    buffers: &mut LoadBuffers,
) -> Vec<(RunIdentity, MachineSecond, AppsSecond)> {
    let mut captured = Vec::new();
    let failure = load_window(
        &fixture.root,
        None,
        crate::session::LOADED_SECONDS,
        buffers,
        |run, second, apps| {
            let TickApps::Loaded(apps) = apps else {
                panic!("the fixture's App parts must decode");
            };
            captured.push((*run, *second, std::mem::take(apps)));
        },
    );
    assert_eq!(failure, None);
    captured
}
