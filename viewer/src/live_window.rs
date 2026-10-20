//! What the session keeps of every observation in the selected window, so a hover shows any
//! second at once. Live observations give theirs as the session polls them, and saved ones in
//! one replay of the window's saved minutes.
use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

use persistence::{Error, Result};
use serde_json::Value;

use super::{LiveObservation, NANOSECONDS_PER_SECOND, context_range};
use crate::LiveSession;
use crate::cpu_history_sample::Identity;
use crate::live_app_response::{Observation, Sample};
use crate::session::ContextRange;

/// What a window keeps of one observation's sample.
pub trait WindowValue {
    /// The bytes the values of one window may hold together.
    const BYTES_MAX: usize;

    fn selected(&self) -> &Observation;

    fn bytes(&self) -> usize;
}

/// The values of the window's observations. An observation the window holds no value for,
/// since the replay could not give one or its value would pass the window's bytes, stays held,
/// so the replay does not run again for it.
pub struct Window<T> {
    observations: BTreeMap<ObservationKey, Held<T>>,
    bytes: usize,
}

struct Held<T> {
    wall_ns: u64,
    bytes: usize,
    value: Option<Arc<T>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ObservationKey {
    pub run: [u64; 2],
    pub boot: [u64; 2],
    pub publication: u64,
}

/// A window and the live observations after it, at one or more observations a second.
const OBSERVATIONS_MAX: usize = 4096;
const _: () = {
    assert!(OBSERVATIONS_MAX == crate::session::OBSERVATIONS_MAX);
};

impl<T> Default for Window<T> {
    fn default() -> Self {
        Self {
            observations: BTreeMap::new(),
            bytes: 0,
        }
    }
}

impl<T> Clone for Window<T> {
    fn clone(&self) -> Self {
        Self {
            observations: self.observations.clone(),
            bytes: self.bytes,
        }
    }
}

impl<T> Clone for Held<T> {
    fn clone(&self) -> Self {
        Self {
            wall_ns: self.wall_ns,
            bytes: self.bytes,
            value: self.value.clone(),
        }
    }
}

impl<T: WindowValue> Window<T> {
    #[must_use]
    pub fn get(&self, key: &ObservationKey) -> Option<&T> {
        self.observations.get(key)?.value.as_deref()
    }

    fn holds(&self, key: &ObservationKey) -> bool {
        self.observations.contains_key(key)
    }

    fn insert(&mut self, value: T) -> Result<()> {
        // Only an observation with a wall clock lies in a window.
        let Some(wall_ns) = value.selected().wall_ns else {
            return Ok(());
        };
        let key = ObservationKey::of(value.selected());
        if let Some(replaced) = self.observations.remove(&key) {
            self.bytes -= replaced.bytes;
        }
        if self.observations.len() == OBSERVATIONS_MAX {
            return Err(Error::Bound("window values"));
        }

        let bytes = value.bytes();
        let held = if bytes <= T::BYTES_MAX - self.bytes {
            Held {
                wall_ns,
                bytes,
                value: Some(Arc::new(value)),
            }
        } else {
            Held {
                wall_ns,
                bytes: 0,
                value: None,
            }
        };
        self.bytes += held.bytes;
        self.observations.insert(key, held);
        assert!(self.observations.len() <= OBSERVATIONS_MAX);
        assert!(self.bytes <= T::BYTES_MAX);

        Ok(())
    }

    /// Drops the observations outside the wall clock range.
    fn retain(&mut self, range: ContextRange) {
        let mut freed = 0;
        self.observations.retain(|_key, held| {
            let kept = (range.first_ns..range.end_ns).contains(&held.wall_ns);
            if !kept {
                freed += held.bytes;
            }
            kept
        });
        self.bytes -= freed;
    }

    /// Whether these are the values of exactly the window `range` and its `context`.
    fn covers(&self, range: ContextRange, context: &[Identity]) -> Result<bool> {
        let outside = self
            .observations
            .values()
            .any(|held| !(range.first_ns..range.end_ns).contains(&held.wall_ns));
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

    /// Marks the context's observations the replay gave no value as holding none.
    fn settle(&mut self, context: &[Identity]) -> Result<()> {
        for identity in context {
            let key = ObservationKey::read(identity)?;
            if !self.holds(&key) {
                if self.observations.len() == OBSERVATIONS_MAX {
                    return Err(Error::Bound("window values"));
                }

                self.observations.insert(
                    key,
                    Held {
                        wall_ns: identity.observation.wall,
                        bytes: 0,
                        value: None,
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
    /// Gives every observation of the selected window that holds no value yet the value
    /// `project` takes from its sample: the live ones from their cached samples, and the saved
    /// ones in one replay of the window's saved minutes. A window it covers already stays the
    /// one its holders share.
    pub(super) fn fill_window<T: WindowValue>(
        &self,
        window: &mut Arc<Window<T>>,
        project: impl Fn(&Sample) -> Result<T>,
    ) -> Result<()> {
        let mut selected = None;
        self.with_selected_apps(|sample| {
            selected = Some(sample.selected);
            Ok(Value::Null)
        })?;
        let selected = selected.ok_or(Error::Bound("window selection"))?;
        let second = selected.wall_ns.ok_or(Error::Bound("window clock"))? / NANOSECONDS_PER_SECOND;
        let range = context_range(second)?;

        if window.covers(range, &self.selected_context)? {
            return Ok(());
        }

        let held = Arc::make_mut(window);
        held.retain(range);
        fill_window_live(&self.observations, range, held, &project)?;

        if let Some(history) = &self.history {
            if let Err(error) = fill_window_saved(history, &selected, held, &project) {
                // A cancelled replay goes on at the next read from the values it gave. A failed
                // one holds the rest without values, so the reads that follow replay nothing
                // and a hover reads its own second.
                if let Error::Io(problem) = &error {
                    if problem.kind() == std::io::ErrorKind::Interrupted {
                        return Err(error);
                    }
                }
                held.settle(&self.selected_context)?;
                return Err(error);
            }
        }

        held.settle(&self.selected_context)
    }
}

/// Keeps the value of the observation the session polled last, and drops those older than
/// its window.
pub(super) fn poll<T: WindowValue>(
    window: &mut Arc<Window<T>>,
    first_ns: u64,
    value: T,
) -> Result<()> {
    let held = Arc::make_mut(window);
    held.retain(ContextRange {
        first_ns,
        end_ns: u64::MAX,
    });

    held.insert(value)
}

/// Gives the live observations of the window `range` that hold no value yet theirs, from their
/// cached samples.
fn fill_window_live<T: WindowValue>(
    observations: &VecDeque<LiveObservation>,
    range: ContextRange,
    window: &mut Window<T>,
    project: &impl Fn(&Sample) -> Result<T>,
) -> Result<()> {
    for observation in observations {
        if (range.first_ns..range.end_ns).contains(&observation.observation.wall) {
            if let Some(sample) = &observation.app_sample {
                if !window.holds(&ObservationKey::of(&sample.selected)) {
                    window.insert(project(sample)?)?;
                }
            }
        }
    }

    Ok(())
}

fn fill_window_saved<T: WindowValue>(
    history: &crate::Session,
    selected: &Observation,
    window: &mut Window<T>,
    project: &impl Fn(&Sample) -> Result<T>,
) -> Result<()> {
    let window = RefCell::new(window);
    history.visit_app_attribution(
        selected,
        |observation| Ok(window.borrow().holds(&ObservationKey::of(observation))),
        |frame, previous, _windows| {
            let mut sample = None;
            Sample::capture(frame, previous, &mut sample)?;
            let sample = sample.ok_or(Error::Bound("window sample"))?;
            window.borrow_mut().insert(project(&sample)?)
        },
        |pin| {
            drop(pin);
            Ok(())
        },
    )?;

    Ok(())
}

#[cfg(test)]
#[path = "live_window_fixture.rs"]
pub(super) mod fixture;

#[cfg(test)]
#[path = "live_window_tests.rs"]
mod tests;
