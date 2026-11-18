//! What a reader keeps of the window of ranks it last read, so its next reading ranks only the
//! seconds that window does not answer for.
use std::collections::BTreeMap;
use std::sync::{Arc, Weak};

use super::ContextRanks;
use crate::live_app_response::Ranks;
use crate::store::Block;
use crate::{ObservationKey, WindowRanks};

/// The window of ranks a reader last read, and the block that held the Apps of each second it
/// ranks. A second's ranks follow from its Apps alone, and the store never changes a block: it
/// puts a new one in a block's place when a second joins it, when one of its seconds changes and
/// when it is packed. So a second still in its block ranks as it did, while a second of the
/// block the store still adds to is ranked again at each reading. The window keeps its
/// reservation, so the ranks stay charged for as long as the reader holds them.
#[derive(Default)]
pub struct RankedSeconds {
    window: Option<Arc<ContextRanks<WindowRanks>>>,
    /// Weak, so the reader holds no block the store let go of, while no other block can take a
    /// block's address and pass for it.
    blocks: BTreeMap<ObservationKey, Weak<Block>>,
}

impl RankedSeconds {
    /// The ranks of `window`, with no second's block known yet.
    pub(super) fn of(window: &Arc<ContextRanks<WindowRanks>>) -> Self {
        Self {
            window: Some(Arc::clone(window)),
            blocks: BTreeMap::new(),
        }
    }

    /// The ranks of the second of `key`, where the window ranked it while `block` held its Apps.
    pub(super) fn get(&self, key: &ObservationKey, block: &Arc<Block>) -> Option<&Arc<Ranks>> {
        let ranked = self.blocks.get(key)?;
        if !std::ptr::eq(ranked.as_ptr(), Arc::as_ptr(block)) {
            return None;
        }

        self.window.as_ref()?.shared(key)
    }

    /// Notes that `block` holds the Apps of the second of `key`, which the window ranks.
    pub(super) fn place(&mut self, key: ObservationKey, block: &Arc<Block>) {
        self.blocks.insert(key, Arc::downgrade(block));
    }

    pub(super) fn second_count(&self) -> usize {
        self.blocks.len()
    }
}
