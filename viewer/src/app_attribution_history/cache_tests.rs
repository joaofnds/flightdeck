use std::sync::Arc;

use super::{App, Cache, Key, Point};
use crate::apps::Stamp;
use crate::cancellation::{Cancellation, Operation};
use crate::live::resident_history::Capture as Coverage;
use crate::live_app_history::CompactMeasure;
use crate::live_app_response::Observation;

const AVAILABLE: usize = 64 * 1024 * 1024;

/// Inserts a point whose Apps take the previous point's key where the name matches, as
/// `Point::capture` does, so the cache holds the only references to its keys.
fn insert(cache: &mut Cache, publication: u64, names: &[&str]) {
    let keys = names
        .iter()
        .map(|name| {
            let previous = cache.points.last().and_then(|point| {
                point
                    .apps
                    .iter()
                    .find(|app| *app.key.bytes == *name.as_bytes())
            });
            previous.map_or_else(
                || {
                    Arc::new(Key {
                        bundle: true,
                        bytes: name.as_bytes().into(),
                    })
                },
                |app| Arc::clone(&app.key),
            )
        })
        .collect();
    cache.insert(point(publication, keys), AVAILABLE).unwrap();
}

fn point(publication: u64, keys: Vec<Arc<Key>>) -> Point {
    Point {
        capture: super::Capture {
            coverage: Coverage {
                observation: Observation {
                    recording: [1, 2],
                    stamp: Stamp {
                        boot: [3, 4],
                        run: [5, 6],
                        publication,
                        continuous_ns: Some(publication),
                        awake_ns: Some(publication),
                    },
                    run_tick: publication,
                    wall_ns: Some(publication * 1_000_000_000),
                },
                membership_complete: true,
                ungrouped_members: 0,
                usage_refused: 0,
                usage_gone: 0,
            },
            elapsed_ns: Some(1_000_000_000),
        },
        windows: [None, None],
        apps: keys
            .into_iter()
            .map(|key| App {
                key,
                cpu: CompactMeasure::default(),
                power: CompactMeasure::default(),
                disk: CompactMeasure::default(),
                members: 1,
                membership_complete: true,
            })
            .collect(),
    }
}

fn operation() -> Operation {
    Arc::new(Cancellation::default()).operation(0).unwrap()
}

fn kept_alone(names: &[&str]) -> usize {
    let mut cache = Cache::new();
    insert(&mut cache, 9, names);
    cache.bytes()
}

#[test]
fn retain_releases_a_dropped_point_but_keeps_the_key_a_kept_point_shares() {
    // Goal: dropping a point frees its own key and keeps the one a later point shares.
    // Method: retain the second of two points sharing a key and compare with that point alone.
    let mut cache = Cache::new();
    insert(&mut cache, 1, &["shared", "first"]);
    insert(&mut cache, 2, &["shared", "second"]);

    cache
        .retain(&operation(), |observation| {
            Ok(observation.stamp.publication == 2)
        })
        .unwrap();

    assert_eq!(cache.count(), 1);
    assert_eq!(cache.bytes(), kept_alone(&["shared", "second"]));
}

#[test]
fn retain_releases_a_key_once_every_point_sharing_it_is_dropped() {
    // Goal: a key two dropped points share is freed once, whatever order they leave in.
    // Method: retain only the third of three points and compare with that point alone.
    let mut cache = Cache::new();
    insert(&mut cache, 1, &["shared"]);
    insert(&mut cache, 2, &["shared"]);
    insert(&mut cache, 3, &["last"]);

    cache
        .retain(&operation(), |observation| {
            Ok(observation.stamp.publication == 3)
        })
        .unwrap();

    assert_eq!(cache.count(), 1);
    assert_eq!(cache.bytes(), kept_alone(&["last"]));
}

#[test]
fn retain_that_keeps_every_point_leaves_the_bytes_unchanged() {
    // Goal: a retain that drops nothing reports the bytes the inserts counted.
    // Method: insert two points sharing a key, retain both, and compare before and after.
    let mut cache = Cache::new();
    insert(&mut cache, 1, &["shared"]);
    insert(&mut cache, 2, &["shared"]);
    let before = cache.bytes();

    cache.retain(&operation(), |_observation| Ok(true)).unwrap();

    assert_eq!(cache.bytes(), before);
}
