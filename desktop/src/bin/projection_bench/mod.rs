use std::alloc::{GlobalAlloc, Layout, System};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde_json::{Value, json};

use crate::data::{Line, Panel};

#[global_allocator]
static ALLOCATOR: Allocator = Allocator;
static ACTIVE: AtomicBool = AtomicBool::new(false);
static CALLS: AtomicU64 = AtomicU64::new(0);
static BYTES: AtomicU64 = AtomicU64::new(0);

struct Allocator;

pub struct Counter {
    active: bool,
}

impl Counter {
    pub fn start() -> Self {
        assert!(!ACTIVE.load(Ordering::Relaxed));
        CALLS.store(0, Ordering::Relaxed);
        BYTES.store(0, Ordering::Relaxed);
        assert!(!ACTIVE.swap(true, Ordering::Relaxed));
        Self { active: true }
    }

    pub fn finish(mut self) -> (u64, u64) {
        self.active = false;
        ACTIVE.store(false, Ordering::Relaxed);

        let calls = CALLS.load(Ordering::Relaxed);
        let bytes = BYTES.load(Ordering::Relaxed);

        assert!(calls < 64 * 1024 * 1024);
        assert!(bytes < 4 * 1024 * 1024 * 1024);

        (calls, bytes)
    }
}

impl Drop for Counter {
    fn drop(&mut self) {
        if self.active {
            ACTIVE.store(false, Ordering::Relaxed);
        }
    }
}

// The opt-in binary measures a synchronous pure projection with no worker or application running.
unsafe impl GlobalAlloc for Allocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        unsafe { System.dealloc(pointer, layout) };
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        record(size);
        unsafe { System.realloc(pointer, layout, size) }
    }
}

fn record(size: usize) {
    if ACTIVE.load(Ordering::Relaxed) {
        let size = u64::try_from(size).unwrap_or_else(|_| unreachable!("usize fits u64"));
        CALLS.fetch_add(1, Ordering::Relaxed);
        BYTES.fetch_add(size, Ordering::Relaxed);
    }
}

pub fn bits(panels: &[Rc<Panel>]) -> Value {
    assert!(panels.len() <= crate::live_projection::OVERVIEW_PANELS_MAX);
    json!(
        panels
            .iter()
            .map(|panel| panel_bits(panel))
            .collect::<Vec<_>>()
    )
}

fn panel_bits(panel: &Panel) -> Value {
    assert!(panel.lines.len() <= 128);
    json!({"selected_index": panel.selected_index,
        "positions": panel.positions.as_ref().map(|values| values.iter()
            .copied().map(f32::to_bits).collect::<Vec<_>>()), "segments": panel.segments,
        "id": panel.id, "title": panel.title, "unit": panel.unit, "kind": panel.kind,
        "lines": panel.lines.iter().map(line_bits).collect::<Vec<_>>(),
        "minimum": panel.minimum.map(f32::to_bits), "maximum": panel.maximum.map(f32::to_bits),
        "stack": panel.stack, "fill": panel.fill.map(f32::to_bits), "caption": panel.caption,
        "help": panel.help, "source": panel.source, "quantities": panel.quantities,
        "states": panel.states, "table_headings": panel.table_headings,
        "table_rows": panel.table_rows})
}

fn line_bits(line: &Line) -> Value {
    assert!(line.values.len() <= crate::data::LIVE_SAMPLE_LIMIT);
    json!({"name": line.name, "color": line.color,
        "values": line.values.iter().map(|value| value.map(f32::to_bits)).collect::<Vec<_>>(),
        "stack": line.stack, "negative": line.negative, "dashed": line.dashed,
        "axis": line.axis, "unit": line.unit, "fill": line.fill.map(f32::to_bits)})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_overview_input_retains_all_panels_in_its_benchmark_transcript() {
        // Encode a real projection with the full Overview workload and exact GPU reading bits.
        let run = "00000000000000010000000000000002";
        let boot = "00000000000000030000000000000004";
        let selection = json!({"second":1000,"selected":{"publication":1,"run":run,
            "boot":boot},"trends":[{"wall_ns":1_000_000_000_000_u64,"publication":1,
                "run":run,"boot":boot,"segment":0,"gpu_active_percent":37.5}]});
        let panels = crate::live_projection::panels("overview", &selection, None).unwrap();

        let transcript = bits(&panels);

        let panels = transcript.as_array().unwrap();
        assert_eq!(panels.len(), 8);
        let activity = panels
            .iter()
            .find(|panel| panel["id"] == "live-gpu-activity")
            .unwrap();
        assert_eq!(activity["lines"][0]["values"][0], 37.5_f32.to_bits());
        assert!(
            panels
                .iter()
                .any(|panel| panel["id"] == "live-storage-throughput")
        );
    }
}
