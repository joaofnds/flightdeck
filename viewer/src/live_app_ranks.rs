//! The ranks of every observation in the selected window, so a hover shows any second's top
//! Apps at once. Live observations rank as the session polls them, and saved ones rank in one
//! replay of their saved minutes.
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::Arc;

use persistence::{Error, Result};
use serde_json::Value;

use crate::LiveSession;
use crate::cpu_history_sample::Identity;
use crate::live_app_response::{Observation, Ranks, Sample};

/// The ranks of the window's observations. An observation the window's replay could not rank
/// holds none, so the replay does not run again for it.
#[derive(Clone, Default)]
pub struct WindowRanks {
    observations: BTreeMap<ObservationKey, Ranked>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ObservationKey {
    pub run: [u64; 2],
    pub boot: [u64; 2],
    pub publication: u64,
}

#[derive(Clone)]
struct Ranked {
    wall_ns: u64,
    ranks: Option<Arc<Ranks>>,
}

/// A window and the live observations after it, at one or more observations a second.
const OBSERVATIONS_MAX: usize = 4096;
const NS: u64 = 1_000_000_000;
const _: () = {
    assert!(OBSERVATIONS_MAX == crate::session::OBSERVATIONS_MAX);
};

impl WindowRanks {
    #[must_use]
    pub fn get(&self, key: &ObservationKey) -> Option<&Ranks> {
        self.observations.get(key)?.ranks.as_deref()
    }

    fn holds(&self, key: &ObservationKey) -> bool {
        self.observations.contains_key(key)
    }

    fn insert(&mut self, ranks: Ranks) -> Result<()> {
        // Only an observation with a wall clock lies in a window.
        let Some(wall_ns) = ranks.selected.wall_ns else {
            return Ok(());
        };
        let key = ObservationKey::of(&ranks.selected);
        if !self.observations.contains_key(&key) {
            if self.observations.len() == OBSERVATIONS_MAX {
                return Err(Error::Bound("window ranks"));
            }
        }

        self.observations.insert(
            key,
            Ranked {
                wall_ns,
                ranks: Some(Arc::new(ranks)),
            },
        );
        assert!(self.observations.len() <= OBSERVATIONS_MAX);

        Ok(())
    }

    /// Drops the observations outside the wall clock range `first_ns..end_ns`.
    fn retain(&mut self, first_ns: u64, end_ns: u64) {
        self.observations
            .retain(|_key, ranked| (first_ns..end_ns).contains(&ranked.wall_ns));
    }

    /// Whether these are the ranks of exactly the window `first_ns..end_ns` and its `context`.
    fn covers(&self, first_ns: u64, end_ns: u64, context: &[Identity]) -> Result<bool> {
        let outside = self
            .observations
            .values()
            .any(|ranked| !(first_ns..end_ns).contains(&ranked.wall_ns));
        if outside {
            return Ok(false);
        }

        for identity in context {
            if !self.holds(&ObservationKey::read(identity)?) {
                return Ok(false);
            }
        }

        Ok(true)
    }

    /// Marks the context's observations the replay did not rank as having no ranks.
    fn settle(&mut self, context: &[Identity]) -> Result<()> {
        for identity in context {
            let key = ObservationKey::read(identity)?;
            if !self.holds(&key) {
                if self.observations.len() == OBSERVATIONS_MAX {
                    return Err(Error::Bound("window ranks"));
                }

                self.observations.insert(
                    key,
                    Ranked {
                        wall_ns: identity.observation.wall,
                        ranks: None,
                    },
                );
            }
        }

        Ok(())
    }
}

impl ObservationKey {
    fn of(observation: &Observation) -> Self {
        Self {
            run: observation.stamp.run,
            boot: observation.stamp.boot,
            publication: observation.stamp.publication,
        }
    }

    fn read(identity: &Identity) -> Result<Self> {
        Ok(Self {
            run: collector::recording::parse_identity(&identity.run)?.0,
            boot: collector::recording::parse_identity(&identity.boot)?.0,
            publication: identity.observation.publication,
        })
    }
}

impl LiveSession {
    /// The ranks of the selected window's observations, as far as they are ranked.
    #[must_use]
    pub fn app_ranks(&self) -> Arc<WindowRanks> {
        Arc::clone(&self.app_ranks)
    }

    /// Ranks every observation of the selected window that has no ranks yet: the live ones
    /// from their cached samples, and the saved ones in one replay of the window's saved
    /// minutes.
    pub fn rank_window(&mut self) -> Result<()> {
        let operation = self.operation.begin(self.operation.epoch())?;
        operation.run(|| self.rank_window_inner())
    }

    fn rank_window_inner(&mut self) -> Result<()> {
        let mut selected = None;
        self.with_selected_apps(|sample| {
            selected = Some(sample.selected);
            Ok(Value::Null)
        })?;
        let selected = selected.ok_or(Error::Bound("ranked window selection"))?;
        let second = selected
            .wall_ns
            .ok_or(Error::Bound("ranked window clock"))?
            / NS;
        let first_ns = second.saturating_sub(899) * NS;
        let end_ns = second
            .checked_add(1)
            .and_then(|value| value.checked_mul(NS))
            .ok_or(Error::Bound("ranked window end"))?;
        // A covered window leaves the ranks the caller already shares untouched.
        if self
            .app_ranks
            .covers(first_ns, end_ns, &self.selected_context)?
        {
            return Ok(());
        }

        let window = Arc::make_mut(&mut self.app_ranks);
        window.retain(first_ns, end_ns);
        for observation in &self.observations {
            if (first_ns..end_ns).contains(&observation.observation.wall) {
                if let Some(sample) = &observation.app_sample {
                    if !window.holds(&ObservationKey::of(&sample.selected)) {
                        window.insert(sample.ranks()?)?;
                    }
                }
            }
        }
        if let Some(history) = &self.history {
            rank_window_saved(history, &selected, window)?;
        }

        window.settle(&self.selected_context)
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
        let window = Arc::make_mut(&mut self.app_ranks);
        window.retain(first_ns, u64::MAX);

        window.insert(ranks)
    }
}

fn rank_window_saved(
    history: &crate::Session,
    selected: &Observation,
    window: &mut WindowRanks,
) -> Result<()> {
    let window = RefCell::new(window);
    history.visit_app_attribution(
        selected,
        |observation| Ok(window.borrow().holds(&ObservationKey::of(observation))),
        |frame, previous, _windows| {
            let mut sample = None;
            Sample::capture(frame, previous, &mut sample)?;
            let sample = sample.ok_or(Error::Bound("ranked window sample"))?;
            window.borrow_mut().insert(sample.ranks()?)
        },
        |pin| {
            drop(pin);
            Ok(())
        },
    )?;

    Ok(())
}

#[cfg(test)]
#[path = "live_app_ranks_tests.rs"]
mod tests;
