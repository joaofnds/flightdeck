use super::{LoadBuffers, Owed, load_owed};
use crate::live::identity_fixture::Fixture;
use crate::store::apps::TickApps;
use collector::ring::RunIdentity;

#[test]
fn reloading_reuses_app_decode_storage_without_retaining_previous_rows() {
    // Goal: repeated adding up of owed Apps reuses decoder capacity without keeping decoded rows
    // alive.
    // Method: add up real App parts twice through oversized, seeded storage and compare fresh
    // output.
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    fixture.seal_minute_derived(0, &persistence::sealed_apps::SealedApps::default());
    let expected = capture(&fixture, &mut LoadBuffers::default());
    assert_eq!(expected.len(), 1);
    assert!(expected[0].holds_apps);
    let mut buffers = LoadBuffers::default();
    buffers.apps[0].apps.reserve_exact(4_096);
    buffers.apps[0].keys.reserve_exact(8_192);
    buffers.apps[0].keys.extend_from_slice(b"previous load");
    let rows = buffers.apps[0].apps.as_ptr();
    let keys = buffers.apps[0].keys.as_ptr();

    for _ in 0..2 {
        assert_eq!(capture(&fixture, &mut buffers), expected);
        assert!(buffers.apps[0].samples.is_empty());
        assert!(buffers.apps[0].samples.capacity() >= 2);
        assert!(buffers.apps[0].apps.is_empty());
        assert!(buffers.apps[0].keys.is_empty());
        assert_eq!(buffers.apps[0].apps.as_ptr(), rows);
        assert_eq!(buffers.apps[0].keys.as_ptr(), keys);
    }
}

#[test]
fn a_store_shows_the_rates_of_a_minutes_first_second_after_a_restart() {
    // Goal: a persister restarted between two minutes costs the store's route no App rate.
    // Method: seal run ticks 59 and 60 in two minutes in two recordings, through one derivation and
    // through one for each minute, add up their Apps as a store does, and compare what each
    // second's stretch holds.
    let seconds_through = |derive: &dyn persistence::worker::Derive| {
        let mut fixture = Fixture::new();
        fixture.start_at_run_tick(59);
        fixture.publish_with_system_rows(1059);
        fixture.publish_with_system_rows(1060);
        fixture.seal_minute_derived(0, derive);
        fixture.seal_minute_derived(1, derive);
        capture(&fixture, &mut LoadBuffers::default())
    };
    let stayed = seconds_through(&persistence::sealed_apps::SealedApps::default());

    let restarted = seconds_through(&persistence::canonical::ready_fixture::Restarted);

    assert_eq!(restarted.len(), 2);
    assert_eq!(restarted[0].elapsed_ns, None);
    assert_eq!(restarted[1].elapsed_ns, Some(1_000_000_000));
    assert_eq!(rows_of(&restarted), rows_of(&stayed));
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

    for (fixture, elapsed_ns) in [
        (&first, Some(1_000_000_000)),
        (&next, None),
        (&first, Some(1_000_000_000)),
    ] {
        let expected = capture(fixture, &mut LoadBuffers::default());
        assert_eq!(capture(fixture, &mut buffers), expected);
        assert_eq!(expected[0].elapsed_ns, elapsed_ns);
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

    let failure = load_owed(
        &fixture.root,
        &owed_of(&fixture),
        &mut buffers,
        |_, _, _| {
            callbacks += 1;
        },
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

/// What adding up the Apps of a stretch handed over, at the tick that holds it.
#[derive(Debug, PartialEq)]
struct Captured {
    run: RunIdentity,
    publication: u64,
    holds_apps: bool,
    elapsed_ns: Option<u64>,
    /// The stretch's App rows, as `Debug` writes them out.
    rows: String,
}

/// What adding up the Apps every sealed minute of `fixture` owes hands over, oldest first.
fn capture(fixture: &Fixture, buffers: &mut LoadBuffers) -> Vec<Captured> {
    let mut captured = Vec::new();
    let failure = load_owed(
        &fixture.root,
        &owed_of(fixture),
        buffers,
        |run, tick, apps| {
            let TickApps::Folded(folded) = apps else {
                return;
            };
            captured.push(Captured {
                run: *run,
                publication: tick.publication,
                holds_apps: folded.apps.holds_apps(),
                elapsed_ns: folded.apps.elapsed_ns(),
                rows: format!("{:?}", folded.apps.apps()),
            });
        },
    );
    assert_eq!(failure, None);
    captured
}

/// The interval and the App rows of each stretch of `captured`, which name no recording.
fn rows_of(captured: &[Captured]) -> Vec<(Option<u64>, &str)> {
    captured
        .iter()
        .map(|stretch| (stretch.elapsed_ns, stretch.rows.as_str()))
        .collect()
}

/// The Apps of every sealed minute of `fixture`, which a load that took their ticks in without
/// them owes the store's stretches.
fn owed_of(fixture: &Fixture) -> Owed {
    let mut found = Vec::new();
    super::find(&fixture.root, &mut found, None).unwrap();
    found.reverse();
    Owed::all(found)
}
