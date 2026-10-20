//! The ranks of every observation in the selected window, so a hover shows any second's top
//! Apps at once.
use std::sync::Arc;

use persistence::{Error, Result};

use super::window::{self, Window, WindowValue};
use crate::LiveSession;
use crate::live_app_response::{Observation, Rank, RankedApp, Ranks, Sample};

pub type WindowRanks = Window<Ranks>;

impl WindowValue for Ranks {
    /// A few kibibytes of ranks for each of the most observations a window holds.
    const BYTES_MAX: usize = 32 * 1024 * 1024;

    fn selected(&self) -> &Observation {
        &self.selected
    }

    /// Counts an App each time it ranks, though its rankings share it.
    fn bytes(&self) -> usize {
        let mut bytes = size_of::<Self>();
        for ranking in &self.metrics {
            bytes += ranking.entries.capacity() * size_of::<Rank>();
            for rank in &ranking.entries {
                bytes += size_of::<RankedApp>()
                    + rank.app.bytes.capacity()
                    + rank.app.display_name.capacity();
            }
        }

        bytes
    }
}

impl LiveSession {
    /// The ranks of the selected window's observations, as far as they are ranked.
    #[must_use]
    pub fn app_ranks(&self) -> Arc<WindowRanks> {
        Arc::clone(&self.app_ranks)
    }

    /// Ranks every observation of the selected window that has no ranks yet.
    pub fn rank_window(&mut self) -> Result<()> {
        let operation = self.operation.begin(self.operation.epoch())?;
        operation.run(|| {
            let mut ranks = std::mem::take(&mut self.app_ranks);
            let ranked = self.fill_window(&mut ranks, Sample::ranks);
            self.app_ranks = ranks;

            ranked
        })
    }

    /// Ranks the observation the session polled last, and drops those older than its window.
    pub(super) fn poll_ranks(&mut self, first_ns: u64) -> Result<()> {
        let current = self
            .observations
            .back()
            .ok_or(Error::Bound("ranked live observation"))?;
        let ranks = current
            .app_sample
            .as_deref()
            .ok_or(Error::Bound("ranked live sample"))?
            .ranks()?;

        window::poll(&mut self.app_ranks, first_ns, ranks)
    }
}

#[cfg(test)]
#[path = "live_app_ranks_tests.rs"]
mod tests;
