use std::sync::atomic::{AtomicU16, Ordering};

use gpui::{Bounds, Pixels, Window};

use crate::charts::Inspection;
use crate::data::Panel;

pub fn chart(panel: &Panel, inspection: Inspection, bounds: Bounds<Pixels>, window: &Window) {
    static PAINTS: AtomicU16 = AtomicU16::new(0);
    assert!(inspection.upper < crate::data::LIVE_SAMPLE_LIMIT);
    assert!(inspection.lower <= inspection.upper);
    assert!(panel.lines.len() <= 128);
    let index = PAINTS.load(Ordering::Relaxed);
    if index >= 16 {
        return;
    }
    if PAINTS
        .compare_exchange(index, index + 1, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return;
    }
    let clipped = bounds.intersect(&window.content_mask().bounds);
    let points: usize = panel
        .lines
        .iter()
        .take(128)
        .map(|line| {
            line.values
                .iter()
                .take(inspection.upper + 1)
                .skip(inspection.lower)
                .filter(|value| value.is_some())
                .count()
        })
        .sum();
    println!(
        "{}",
        serde_json::json!({"event":"chart_paint", "panel":panel.id,
        "series":panel.lines.len(), "available_points":points,
        "width":f32::from(bounds.size.width), "height":f32::from(bounds.size.height),
        "clipped_width":f32::from(clipped.size.width).max(0.0),
        "clipped_height":f32::from(clipped.size.height).max(0.0),
        "window_active":window.is_window_active(),
        "metric":"App-owned canvas paint and source data; not presentation"})
    );
}
