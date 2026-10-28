use std::sync::Arc;

use super::{HeldRow, add_merged};
use crate::live_app_response::{KeyKind, Measure, Metric, Rankable, RankedApp};
use crate::store::attribution::PointRow;

/// One App of a point, which adds what it used to the row of its App.
struct Point {
    name: Arc<RankedApp>,
    used: u64,
}

impl Rankable for Point {
    fn measure(&self, _metric: Metric) -> Measure {
        unreachable!("a merge measures no App")
    }

    fn ranked(&self) -> Arc<RankedApp> {
        Arc::clone(&self.name)
    }
}

impl PointRow for Point {
    fn name(&self) -> &RankedApp {
        &self.name
    }

    fn membership_complete(&self) -> bool {
        true
    }
}

/// The row of one App, which holds what its points used, added up.
struct Row {
    name: Arc<RankedApp>,
    used: u64,
}

impl HeldRow<Point> for Row {
    fn name(&self) -> &RankedApp {
        &self.name
    }

    fn starting(point: &Point) -> Self {
        Self {
            name: point.ranked(),
            used: 0,
        }
    }
}

fn app(key: &[u8]) -> Arc<RankedApp> {
    Arc::new(RankedApp::new(KeyKind::Bundle, key.to_vec()).unwrap())
}

fn point(key: &[u8], used: u64) -> Point {
    Point {
        name: app(key),
        used,
    }
}

fn row(key: &[u8], used: u64) -> Row {
    Row {
        name: app(key),
        used,
    }
}

fn add(row: &mut Row, point: &Point) {
    row.used += point.used;
}

/// Each row's App key and what it holds, in the rows' order.
fn held(rows: &[Row]) -> Vec<(Vec<u8>, u64)> {
    rows.iter()
        .map(|row| (row.name.bytes.clone(), row.used))
        .collect()
}

#[test]
fn a_merge_adds_each_app_to_its_own_row_in_key_order() {
    // Goal: an App the rows hold adds to its row, and one they lack takes a row of its own in
    // its place in key order. Method: merge Apps b and c into rows of a and c.
    let mut rows = vec![row(b"a", 1), row(b"c", 2)];
    let incoming = [point(b"b", 10), point(b"c", 20)];
    let mut merged = Vec::with_capacity(3);

    let fits = add_merged(&mut rows, &incoming, &mut merged, 3, add);

    assert!(fits);
    assert_eq!(
        held(&merged),
        [(b"a".to_vec(), 1), (b"b".to_vec(), 10), (b"c".to_vec(), 22)]
    );
}

#[test]
fn a_merge_past_the_most_apps_it_holds_does_not_fit() {
    // Goal: a merge that comes to more Apps than its rows hold at most says so. Method: merge
    // Apps b and c into a row of a, holding two Apps at most.
    let mut rows = vec![row(b"a", 1)];
    let incoming = [point(b"b", 10), point(b"c", 20)];
    let mut merged = Vec::with_capacity(3);

    let fits = add_merged(&mut rows, &incoming, &mut merged, 2, add);

    assert!(!fits);
}
