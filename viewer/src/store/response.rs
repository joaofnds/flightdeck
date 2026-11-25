//! Presentations of the immutable Store context, without reading or deriving source facts.
mod bounds;
mod charge;
mod chart_points;
mod groups;
mod histories;
mod owned;
mod pages;
mod ranked;
mod trace;

pub(crate) use charge::{JSON_BYTES_MAX, json_charge};
pub use chart_points::ChartPoints;
pub use owned::Response;
pub use ranked::RankedSeconds;

use std::ops::Deref;
use std::sync::Arc;

use persistence::{Error, Result};
use serde_json::Value;

use super::{ByteBudget, ByteReservation, QueryContext, QuerySecond};
use crate::live_app_response::{KeyKind, Observation, Page, RANKS_MAX, Rank, Ranks};
use crate::session::ContextRange;
use crate::{ObservationKey, WindowRanks};

/// The bytes the ranks of one second take in a window at most: the value in its `Arc`, and its
/// five rankings at their bound of rows.
const APP_RANKS_SECOND_BYTES_MAX: usize =
    size_of::<Ranks>() + 2 * size_of::<usize>() + 5 * (RANKS_MAX + 1) * size_of::<Rank>();

/// An App's parts borrow their rows from this same admitted context, including its reservations.
pub struct ContextParts {
    context: Arc<QueryContext>,
    range: ContextRange,
    kind: KeyKind,
    bytes: Box<[u8]>,
    reservation: ByteReservation,
}

/// A ranking result keeps the canonical names' reservation owners while it is displayed.
#[must_use]
pub struct ContextRanks<T> {
    budget: Arc<ByteBudget>,
    ranks: T,
    reservation: ByteReservation,
}

/// One App at one exact retained observation. No process or name rows are copied into the view.
pub struct ContextPart<'context> {
    context: &'context QueryContext,
    second: &'context QuerySecond,
    kind: KeyKind,
    bytes: &'context [u8],
}

impl QueryContext {
    /// The selected observation's Apps, ranked by the same rule as every legacy Sample.
    pub fn selected_ranks(self: &Arc<Self>) -> Result<ContextRanks<Ranks>> {
        self.selected_ranks_for(&self.selected().observation)
    }

    /// The ranks of one exact retained hover identity, with independent names and admission.
    pub fn selected_ranks_for(
        self: &Arc<Self>,
        observation: &Observation,
    ) -> Result<ContextRanks<Ranks>> {
        self.selected_rank_status_for(observation)?
            .try_map(|ranks| ranks.map_err(|_| Error::Bound("selected ranks")))
    }

    /// Keeps supported facet refusal distinct from retained-byte admission and source errors.
    pub fn selected_rank_status_for(
        self: &Arc<Self>,
        observation: &Observation,
    ) -> Result<ContextRanks<std::result::Result<Ranks, super::Missing>>> {
        let second = self
            .inspect(observation)
            .map_err(|_| Error::Bound("selected ranks"))?;
        let reservation = self.reserve_response(
            size_of::<ContextRanks<std::result::Result<Ranks, super::Missing>>>()
                + 5 * (RANKS_MAX + 1) * size_of::<Rank>()
                + response_rank_display_bytes(second)
                + match second.apps().missing() {
                    Some(super::Missing::Refused(reason)) => reason.len(),
                    _ => 0,
                },
        )?;
        let ranks = second.apps().ranks();

        Ok(ContextRanks {
            budget: self.response_budget(),
            ranks,
            reservation,
        })
    }

    /// The ranks of each retained observation in the 900-second context, excluding its lead-in.
    /// A second `ranked` holds from the reading before, still in its block, is not ranked again,
    /// and `ranked` then holds this window alone. A reading that fails leaves `ranked` as it
    /// was.
    pub fn app_ranks(
        self: &Arc<Self>,
        ranked: &mut RankedSeconds,
    ) -> super::HistoryResult<Arc<ContextRanks<WindowRanks>>> {
        self.history_ready()?;
        let range = ContextRange::ending_at(self.context_last_second())?;
        let count = self
            .seconds()
            .iter()
            .filter(|second| response_in_range(second, range))
            .count();
        // A second `ranked` holds keeps the bytes it took, since they follow from its block.
        let seconds_display_bytes: Vec<usize> = self
            .seconds()
            .iter()
            .map(|second| {
                let key = ObservationKey::of(&second.observation);
                ranked
                    .display_bytes(&key, second.block())
                    .unwrap_or_else(|| response_rank_display_bytes(second))
            })
            .collect();
        let display_bytes = seconds_display_bytes.iter().copied().max().unwrap_or(0);
        let mut reservation = self.reserve_response(
            size_of::<ContextRanks<WindowRanks>>()
                + 2 * size_of::<usize>()
                + WindowRanks::metadata_bytes_max(count)
                + count * APP_RANKS_SECOND_BYTES_MAX
                + display_bytes,
        )?;
        let scratch = self.reserve_response(super::AppsCursor::WORKING_BYTES_MAX)?;
        let window = app_ranks_window(self.seconds(), range, ranked)?;
        drop(scratch);
        let held = size_of::<ContextRanks<WindowRanks>>()
            + 2 * size_of::<usize>()
            + WindowRanks::metadata_bytes_max(count)
            + display_bytes
            + window.held_values() * APP_RANKS_SECOND_BYTES_MAX;
        reservation.shrink_to(held);
        let window = Arc::new(ContextRanks {
            budget: self.response_budget(),
            ranks: window,
            reservation,
        });
        *ranked = app_ranks_ranked(self.seconds(), &seconds_display_bytes, &window);

        Ok(window)
    }

    /// A borrowed hover view of one App, sharing this context's existing admission lease.
    pub fn app_parts(
        self: &Arc<Self>,
        kind: KeyKind,
        bytes: &[u8],
    ) -> super::HistoryResult<Arc<ContextParts>> {
        self.history_ready()?;
        kind.checked_key(bytes)?;
        let reservation = self
            .reserve_response(size_of::<ContextParts>() + 2 * size_of::<usize>() + bytes.len())?;

        Ok(Arc::new(ContextParts {
            context: Arc::clone(self),
            range: ContextRange::ending_at(self.context_last_second())?,
            kind,
            bytes: bytes.into(),
            reservation,
        }))
    }
}

impl<T> ContextRanks<T> {
    pub fn map<U>(self, map: impl FnOnce(T) -> U) -> ContextRanks<U> {
        let Self {
            budget,
            ranks,
            reservation,
        } = self;

        ContextRanks {
            budget,
            ranks: map(ranks),
            reservation,
        }
    }

    pub fn try_map<U, E>(
        self,
        map: impl FnOnce(T) -> std::result::Result<U, E>,
    ) -> std::result::Result<ContextRanks<U>, E> {
        let Self {
            budget,
            ranks,
            reservation,
        } = self;
        let ranks = map(ranks)?;

        Ok(ContextRanks {
            budget,
            ranks,
            reservation,
        })
    }

    #[must_use]
    pub fn bytes(&self) -> usize {
        self.reservation.bytes()
    }
}

impl ContextRanks<WindowRanks> {
    /// A hover copies only its ranking and shares the independently admitted App names.
    pub fn selected_ranks_for(&self, observation: &Observation) -> Result<ContextRanks<Ranks>> {
        let ranks = self
            .ranks
            .get(&ObservationKey::of(observation))
            .filter(|ranks| ranks.selected == *observation)
            .ok_or(Error::Bound("selected ranks"))?;
        let rows: usize = ranks
            .metrics
            .iter()
            .map(|ranking| ranking.entries.len())
            .sum();
        let names = ranks
            .metrics
            .iter()
            .flat_map(|ranking| &ranking.entries)
            .map(|rank| rank.app.display_name.len() + 2 * rank.app.bytes.len())
            .max()
            .unwrap_or(0);
        let bytes = size_of::<ContextRanks<Ranks>>()
            + 5 * (RANKS_MAX + 1) * size_of::<Rank>()
            + 4096
            + rows * (512 + names);
        let reservation = self
            .budget
            .reserve(bytes)
            .ok_or(Error::Bound("Store presentation admission bytes"))?;

        Ok(ContextRanks {
            budget: Arc::clone(&self.budget),
            ranks: ranks.clone(),
            reservation,
        })
    }
}

impl<T> Deref for ContextRanks<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.ranks
    }
}

impl ContextParts {
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.reservation.bytes()
    }

    #[must_use]
    pub fn get(&self, key: &ObservationKey) -> Option<ContextPart<'_>> {
        let second = self.context.seconds().iter().find(|second| {
            if !response_in_range(second, self.range) {
                return false;
            }

            let stamp = &second.observation.stamp;
            (stamp.run, stamp.boot, stamp.publication) == (key.run, key.boot, key.publication)
        })?;

        Some(ContextPart {
            context: &self.context,
            second,
            kind: self.kind,
            bytes: &self.bytes,
        })
    }
}

impl ContextPart<'_> {
    #[must_use]
    pub fn is_of(&self, kind: KeyKind, bytes: &[u8]) -> bool {
        if self.kind != kind {
            return false;
        }

        self.bytes == bytes
    }

    /// The App's detail JSON, null when the complete observation contains no App of this key.
    pub fn detail(&self, page: Page) -> Result<Response<Value>> {
        self.context
            .response_app_detail(self.second, self.kind, self.bytes, page)
    }

    /// The surrounding Apps page's metadata, using the same shared JSON/page admission rules.
    pub fn apps_paging(&self, page: Page) -> Result<Response<Value>> {
        self.context.response_apps_paging(self.second, page)
    }

    #[must_use]
    pub fn selected(&self) -> &Observation {
        &self.second.observation
    }
}

/// The ranks of each second of `seconds` in `range`: the ones `ranked` holds for a second still
/// in its block, and the others ranked now.
fn app_ranks_window(
    seconds: &[QuerySecond],
    range: ContextRange,
    ranked: &RankedSeconds,
) -> Result<WindowRanks> {
    let mut window = WindowRanks::default();
    // One cursor walks the seconds in order, so a packed block's seconds take their steps
    // instead of each being rebuilt.
    let mut cursor = super::AppsCursor::default();
    assert!(seconds.len() <= super::SECONDS_MAX);
    for second in seconds {
        if response_in_range(second, range) {
            let key = ObservationKey::of(&second.observation);
            match ranked.get(&key, second.block()) {
                Some(ranks) => window.insert_shared(Arc::clone(ranks))?,
                None => match cursor.ranks(second.apps()) {
                    Ok(ranks) => window.insert(ranks)?,
                    Err(_) => window.insert_missing(&second.observation)?,
                },
            }
        }
    }

    Ok(window)
}

/// The window `window`, with the block of each second of `seconds` it holds ranks of and the
/// bytes the names of those ranks take at most, `display_bytes` in the order of `seconds`.
fn app_ranks_ranked(
    seconds: &[QuerySecond],
    display_bytes: &[usize],
    window: &Arc<ContextRanks<WindowRanks>>,
) -> RankedSeconds {
    let mut ranked = RankedSeconds::of(window);
    assert!(seconds.len() <= super::SECONDS_MAX);
    assert_eq!(display_bytes.len(), seconds.len());
    for (second, display_bytes) in seconds.iter().zip(display_bytes) {
        let key = ObservationKey::of(&second.observation);
        if window.shared(&key).is_some() {
            ranked.place(key, second.block(), *display_bytes);
        }
    }
    assert_eq!(ranked.second_count(), window.held_values());

    ranked
}

fn response_in_range(second: &QuerySecond, range: ContextRange) -> bool {
    second
        .observation
        .wall_ns
        .is_some_and(|wall| (range.first_ns..range.end_ns).contains(&wall))
}

/// The bytes the names of the ranks of `second` take at most, reading no App row.
fn response_rank_display_bytes(second: &QuerySecond) -> usize {
    let apps = second.apps();
    let app_count = apps.app_count();
    let rows = 5 * app_count.min(RANKS_MAX);
    let mut names = 0;
    apps.app_names_in(0..app_count, |name| {
        names = names.max(name.display_name.len() + 2 * name.bytes.len());
    });

    4096 + rows * (512 + names)
}

#[cfg(test)]
mod block_merge_tests;
#[cfg(test)]
mod history_tests;
#[cfg(test)]
mod ownership_tests;
#[cfg(test)]
mod ranked_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod trace_tests;
