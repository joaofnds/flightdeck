use super::*;
use crate::apps::Stamp;
use crate::cancellation::{Cancellation, Operation};
use crate::live::identity_fixture::Fixture;
use crate::live_apps::{Publication, PublicationEvents};
use collector::ring::{Found, RingReader};

#[test]
fn an_evicted_raw_reading_keeps_its_owned_facts_and_reservation() {
    // Goal: cache pressure cannot invalidate a raw page already handed to a reader.
    // Method: evict one native publication while retaining a reader for its exact identity.
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    let (first, first_raw) = capture(&fixture, 1, &budget);
    let (second, second_raw) = capture(&fixture, 2, &budget);
    let fixed = size_of::<RawCache>() + OBSERVATIONS_MAX * size_of::<RawReading>();
    let maximum = fixed + first_raw.bytes() + ARC_BYTES;
    let mut cache = RawCache::with_limit(&budget, maximum).unwrap();
    cache.insert(first, first_raw).unwrap();
    let reading = cache.find(&first).unwrap();

    cache.insert(second, second_raw).unwrap();
    assert!(cache.find(&first).is_none());
    drop(cache);

    let operation = operation();
    let page = reading.raw().page("ticks", 0, &operation).unwrap();
    assert_eq!(page.integer(&page.rows[0], "publication").unwrap(), Some(1));
    assert_eq!(budget.used(), reading.raw().bytes() + ARC_BYTES);
    drop(reading);
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_refused_raw_candidate_preserves_the_previously_accepted_cache() {
    // Goal: an inadmissible identity cannot evict or replace an accepted observation.
    // Method: relabel a native publication's clock and attempt admission at a full limit.
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    fixture.publish_with_system_rows(101);
    let (first, first_raw) = capture(&fixture, 1, &budget);
    let (mut second, second_raw) = capture(&fixture, 2, &budget);
    let mut cache = RawCache::new(&budget).unwrap();
    cache.insert(first, first_raw).unwrap();
    let before = cache.bytes();
    second.wall_ns = Some(0);

    let refused = cache.insert(second, second_raw);

    assert_eq!(refused, Err(Admission::Observation));
    assert_eq!(cache.bytes(), before);
    assert!(cache.find(&first).is_some());
    assert!(cache.find(&second).is_none());
}

#[test]
fn clearing_raw_retention_releases_its_entries_but_keeps_fixed_storage() {
    // Goal: reconciliation discards facts while retaining only the allocated bounded slots.
    // Method: accept one native reading and clear its recording retention.
    let budget = Arc::new(ByteBudget::new(64 * 1024 * 1024));
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(100);
    let (observation, raw) = capture(&fixture, 1, &budget);
    let mut cache = RawCache::new(&budget).unwrap();
    cache.insert(observation, raw).unwrap();

    cache.clear();

    assert!(cache.find(&observation).is_none());
    assert_eq!(budget.used(), cache.bytes());
    drop(cache);
    assert_eq!(budget.used(), 0);
}

fn capture(
    fixture: &Fixture,
    publication: u64,
    budget: &Arc<ByteBudget>,
) -> (Observation, Arc<Raw>) {
    let mut reader = RingReader::open_generation(fixture.writer.name()).unwrap();
    reader.seek_publication(publication).unwrap();
    let mut tick = persistence::tick_buffer(&reader.shape()).unwrap();
    let mut identities =
        Vec::with_capacity(usize::try_from(reader.shape().process_capacity).unwrap());
    assert!(
        matches!(reader.read_identity_into(&mut tick, &mut identities).unwrap(),
        Found::Publication { publication: found, .. } if found == publication)
    );
    let run = reader.identity().unwrap();
    let observation = Observation {
        recording: run.recording.0,
        stamp: Stamp {
            boot: run.boot.0,
            run: run.run.0,
            publication,
            continuous_ns: tick.times.map(|times| times.continuous_ns),
            awake_ns: tick.times.map(|times| times.awake_ns),
        },
        run_tick: tick.run_tick,
        wall_ns: tick.times.map(|times| times.wall_ns),
    };
    let mut raw = None;
    Raw::capture_budgeted(
        &Publication {
            reader: &reader,
            tick: &tick,
            identities: &identities,
            publication,
        },
        &PublicationEvents {
            events: &[],
            identities: &[],
        },
        &operation(),
        Some(budget),
        &mut raw,
    )
    .unwrap();

    (observation, Arc::new(raw.unwrap()))
}

fn operation() -> Operation {
    Arc::new(Cancellation::default()).operation(0).unwrap()
}
