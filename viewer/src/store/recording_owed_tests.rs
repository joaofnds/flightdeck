use super::{Owed, load_owed};
use crate::live::identity_fixture::Fixture;
use crate::store::apps::TickApps;

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
        capture(&fixture)
    };
    let stayed = seconds_through(&persistence::sealed_apps::SealedApps::default());

    let restarted = seconds_through(&persistence::canonical::ready_fixture::Restarted);

    assert_eq!(restarted.len(), 2);
    assert_eq!(restarted[0].elapsed_ns, None);
    assert_eq!(restarted[1].elapsed_ns, Some(1_000_000_000));
    assert_eq!(restarted, stayed);
}

/// What adding up the Apps of a stretch handed over, which names no recording.
#[derive(Debug, PartialEq)]
struct Captured {
    elapsed_ns: Option<u64>,
    /// The stretch's App rows, as `Debug` writes them out.
    rows: String,
}

/// What adding up the Apps every sealed minute of `fixture` owes hands over, oldest first.
fn capture(fixture: &Fixture) -> Vec<Captured> {
    let mut captured = Vec::new();
    let failure = load_owed(&fixture.root, &owed_of(fixture), |_, _, apps| {
        let TickApps::Folded(folded) = apps else {
            return;
        };
        captured.push(Captured {
            elapsed_ns: folded.apps.elapsed_ns(),
            rows: format!("{:?}", folded.apps.apps()),
        });
    });
    assert_eq!(failure, None);
    captured
}

/// The Apps of every sealed minute of `fixture`, which a load that took their ticks in without
/// them owes the store's stretches.
fn owed_of(fixture: &Fixture) -> Owed {
    let mut found = Vec::new();
    super::find(&fixture.root, &mut found, None).unwrap();
    found.reverse();
    Owed::all(found)
}
