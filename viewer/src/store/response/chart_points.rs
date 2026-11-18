//! What a reader keeps of the chart points it last read, so its next reading builds only the
//! points of the seconds whose complete reading it did not build one from.
use std::collections::BTreeMap;
use std::sync::{Arc, Weak};

use crate::store::CompleteMachine;
use crate::{ChartPoint, ObservationKey};

#[cfg(test)]
#[path = "chart_points_tests.rs"]
mod tests;

/// The chart point a reader last read of each second, and the complete reading it was built
/// from. A second's point follows from its complete reading and its observation alone, and the
/// store never changes a complete reading: it puts a new one in a second's place. So a point
/// built from the complete reading a second still holds shows what building it again would,
/// while a second with no complete reading is built again at each reading. Each point keeps its
/// reservation, so it stays charged for as long as anything holds it.
///
/// Three things the cache keeps are charged to no budget, each for an entry of the last
/// reading's chart, until a reading lets go of the entry: the allocation of the complete reading
/// a `Weak` points at, though not its contents, which their own reservation charges; the map's
/// nodes; and, while the cache alone holds a point, the point's header. At the Store's bound of
/// 1,080 seconds they come to at most `UNCHARGED_BYTES_MAX`, under 2 MB.
#[derive(Debug, Default)]
pub struct ChartPoints {
    points: BTreeMap<ObservationKey, CachedPoint>,
}

#[derive(Debug)]
struct CachedPoint {
    /// Weak, so the reader holds no complete reading the store let go of, while no other
    /// complete reading can take its address and pass for it.
    complete: Weak<CompleteMachine>,
    point: Arc<ChartPoint>,
}

impl ChartPoints {
    /// The point of the second of `key`, where the reader built it from `complete`.
    pub(in crate::store) fn get(
        &self,
        key: &ObservationKey,
        complete: &Arc<CompleteMachine>,
    ) -> Option<&Arc<ChartPoint>> {
        let cached = self.points.get(key)?;
        if !std::ptr::eq(cached.complete.as_ptr(), Arc::as_ptr(complete)) {
            return None;
        }

        Some(&cached.point)
    }

    /// Notes that `point` is the point of the second of `key`, built from `complete`. The key's
    /// run, boot and publication come from the collector, another program, which may repeat
    /// them, so a later second of the same key replaces the earlier one's entry. A replaced
    /// entry shows no second's point in place of another's, since `get` returns a point only to
    /// the second whose complete reading it was built from.
    pub(in crate::store) fn place(
        &mut self,
        key: ObservationKey,
        complete: &Arc<CompleteMachine>,
        point: &Arc<ChartPoint>,
    ) {
        let cached = CachedPoint {
            complete: Arc::downgrade(complete),
            point: Arc::clone(point),
        };
        self.points.insert(key, cached);
    }

    #[cfg(any(test, feature = "fixtures"))]
    #[must_use]
    pub fn point_count(&self) -> usize {
        self.points.len()
    }

    #[cfg(any(test, feature = "fixtures"))]
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.points.is_empty()
    }
}

/// The bytes of a complete reading's allocation that an entry's `Weak` keeps once the store
/// lets go of the reading: the reading and the `Arc`'s two counts.
const KEPT_COMPLETE_BYTES: usize = size_of::<CompleteMachine>() + 2 * size_of::<usize>();
/// The bytes of one node of the map, which holds eleven entries and twelve edges, as
/// `json_charge` counts a node of a JSON object.
const NODE_BYTES: usize =
    11 * (size_of::<ObservationKey>() + size_of::<CachedPoint>()) + 12 * size_of::<usize>() + 32;
/// The most nodes the map takes for the Store's bound on seconds, since every node but the root
/// holds at least five entries.
const NODE_COUNT_MAX: usize = 1 + crate::store::SECONDS_MAX.div_ceil(5);
/// The most bytes the cache keeps uncharged.
const UNCHARGED_BYTES_MAX: usize = crate::store::SECONDS_MAX
    * (KEPT_COMPLETE_BYTES + ChartPoint::HEADER_BYTES)
    + NODE_COUNT_MAX * NODE_BYTES;
const _: () = assert!(size_of::<CompleteMachine>() == 1_472);
const _: () = assert!(KEPT_COMPLETE_BYTES == 1_488);
const _: () = assert!(crate::store::SECONDS_MAX == 1_080);
// The desktop builds serde_json with its map in insertion order, which makes a `Value` larger,
// so the bound is stated for both builds: 1,846,248 bytes in the viewer's and more in the
// desktop's.
const _: () = assert!(UNCHARGED_BYTES_MAX < 2_000_000);
