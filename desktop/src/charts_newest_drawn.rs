//! The newest point each line of a chart drew, which a test that puts nothing on screen reads
//! to learn what a frame showed.
use std::cell::RefCell;
use std::collections::BTreeMap;

use super::{Geometry, RunPoint};
use crate::data::{Line, Panel};

thread_local! {
    /// The value each line's newest point stood at when this thread last drew it, by panel id
    /// and line name.
    static NEWEST: RefCell<BTreeMap<(String, String), f32>> =
        const { RefCell::new(BTreeMap::new()) };
}

/// Notes the newest of `points`, a curve of `line` on `panel` drawn in time order, as the
/// line's newest point, in the chart's own units.
pub(super) fn remember(panel: &Panel, line: &Line, points: &[RunPoint], geometry: &Geometry<'_>) {
    let Some(newest) = points.last() else {
        return;
    };
    let height = f32::from(geometry.bounds.size.height);
    assert!(0.0 < height);
    let above_bottom = f32::from(geometry.bounds.bottom() - newest.top.y) / height;
    let value = geometry.minimum + above_bottom * (geometry.maximum - geometry.minimum);

    NEWEST.with_borrow_mut(|newest| newest.insert((panel.id.clone(), line.name.clone()), value));
}

/// The value the newest point of the line `line` on the panel `panel` stood at when this
/// thread last drew it, or none before it drew one.
pub(crate) fn value(panel: &str, line: &str) -> Option<f32> {
    NEWEST.with_borrow(|newest| newest.get(&(panel.to_owned(), line.to_owned())).copied())
}

#[test]
fn a_drawn_point_reads_as_the_value_it_was_drawn_at() {
    // Goal: the value a test reads is the one the chart drew, on the chart's own scale. Method:
    // draw a point of a line at 70 on a 0 to 100 chart, then read it back.
    let panel: Panel = serde_json::from_value(serde_json::json!({
        "id": "live-cpu", "title": "CPU", "unit": "%", "kind": "line",
        "lines": [{"name": "CPU", "color": "#5794f2", "values": [10.0, 70.0]}],
    }))
    .unwrap();
    let bounds = gpui::Bounds::new(
        gpui::point(gpui::px(20.0), gpui::px(30.0)),
        gpui::size(gpui::px(300.0), gpui::px(120.0)),
    );
    let geometry = Geometry {
        bounds,
        minimum: 0.0,
        maximum: 100.0,
        count: 2,
        positions: None,
        segments: None,
        lower: 0,
        clock: None,
    };
    let points = [10.0, 70.0].map(|value| RunPoint {
        top: geometry.point(0, value),
        bottom: geometry.point(0, 0.0),
    });

    remember(&panel, &panel.lines[0], &points, &geometry);

    let drawn = value("live-cpu", "CPU").unwrap();
    assert!((drawn - 70.0).abs() < 0.001, "{drawn}");
    assert_eq!(value("live-cpu", "GPU"), None);
}
