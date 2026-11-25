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
/// reservation, so the ranks stay charged for as long as the reader holds them. Beside each
/// block it keeps the bytes the names of the second's ranks take at most, which follow from the
/// same Apps.
#[derive(Default)]
pub struct RankedSeconds {
    window: Option<Arc<ContextRanks<WindowRanks>>>,
    blocks: BTreeMap<ObservationKey, RankedBlock>,
}

/// The block that held the Apps of a second the window ranks, and the bytes the names of its
/// ranks take at most.
struct RankedBlock {
    /// Weak, so the reader holds no block the store let go of, while no other block can take a
    /// block's address and pass for it.
    block: Weak<Block>,
    display_bytes: usize,
}

impl RankedBlock {
    /// Whether this is the block that holds the Apps of the second now, `block`.
    fn holds(&self, block: &Arc<Block>) -> bool {
        std::ptr::eq(self.block.as_ptr(), Arc::as_ptr(block))
    }
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
        self.get_ranked_block(key, block)?;

        self.window.as_ref()?.shared(key)
    }

    /// The bytes the names of the ranks of the second of `key` take at most, where the window
    /// ranked it while `block` held its Apps.
    pub(super) fn display_bytes(&self, key: &ObservationKey, block: &Arc<Block>) -> Option<usize> {
        self.get_ranked_block(key, block)
            .map(|ranked| ranked.display_bytes)
    }

    /// The entry of the second of `key`, where the window ranked it while `block` held its Apps.
    fn get_ranked_block(&self, key: &ObservationKey, block: &Arc<Block>) -> Option<&RankedBlock> {
        self.blocks.get(key).filter(|ranked| ranked.holds(block))
    }

    /// Notes that `block` holds the Apps of the second of `key`, which the window ranks, and
    /// that the names of its ranks take at most `display_bytes`. The key's run, boot and
    /// publication come from the collector, another program, which may repeat them, so a later
    /// second of the same key in one block replaces the earlier one's entry. The window holds
    /// the later second's ranks alone, since it too replaces the earlier second of a key, and
    /// `app_ranks_ranked` places the seconds in the order the window took them, so the bytes
    /// kept are those of the ranks the window holds.
    pub(super) fn place(&mut self, key: ObservationKey, block: &Arc<Block>, display_bytes: usize) {
        let ranked = RankedBlock {
            block: Arc::downgrade(block),
            display_bytes,
        };
        self.blocks.insert(key, ranked);
    }

    pub(super) fn second_count(&self) -> usize {
        self.blocks.len()
    }
}
