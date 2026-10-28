//! Immutable contexts built solely from already checked Store observations.
mod protocol;

pub(super) use protocol::Queries;
pub use protocol::{
    QueryHandle, QueryReply, QueryRequest, QuerySelection, QuerySubmitError, QueryToken,
};

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use collector::ring::Identity;

use super::apps::DetachedAppsSecond;
use super::selection;
use super::{AppsSecond, ByteBudget, ByteReservation, Failure, Held, MachineSecond, Missing};
use crate::live_app_response::Observation;
use crate::session::{ContextRange, ContextSelection};

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;

/// One displayed context and one replacement may retain detached data at the same time.
const CONTEXTS_MAX: usize = 2;

/// A checked source/catalog generation, independent of display publication versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceGeneration {
    pub recording: Identity,
    pub generation: u64,
}

/// A loader/source acceptance certificate. Only the Store producer may construct one.
#[derive(Clone, Copy)]
pub(crate) struct CheckedCoverage {
    source: SourceGeneration,
    range: Option<ContextRange>,
    requires_ring: bool,
}

/// A followed context briefly borrows Store pool owners; a fixed selection detaches them.
/// Neither retains raw frames or derived samples.
#[derive(Debug)]
pub struct QueryContext {
    readiness: QueryReadiness,
    pub source: SourceGeneration,
    catalog: Option<super::recording::CheckedCatalog>,
    status: Option<super::ring::Status>,
    latest: Observation,
    latest_origin: super::native::Origin,
    raw_count: u32,
    raw_bytes: usize,
    followed: bool,
    raw: Option<super::raw_cache::RawReading>,
    selection: ContextSelection,
    selected: Observation,
    seconds: Vec<QuerySecond>,
    reservation: ContextReservation,
}

/// Independent facets remain available when another facet was not retained.
#[derive(Debug)]
pub struct QuerySecond {
    pub observation: Observation,
    machine: Arc<MachineSecond>,
    apps: QueryApps,
    outside_cpu_predecessor: Option<ObservationHeader>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ObservationHeader {
    pub observation: Observation,
    pub origin: super::native::Origin,
}

#[derive(Debug)]
enum QueryApps {
    Held(Arc<AppsSecond>),
    Detached(Box<DetachedAppsSecond>),
}

#[derive(Clone, Copy)]
enum Retention {
    Followed,
    Fixed,
}

#[derive(Debug)]
pub enum QueryOutcome {
    Ready(Arc<QueryContext>),
    NeedsLoad(QueryNeedsLoad),
    Unavailable(QueryUnavailable),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryNeedsLoad {
    AppsPending,
    UncheckedCoverage,
    OutsideCoverage,
    FullDetailEvicted,
    IncompleteRing,
    SourceChanged,
    MissingObservation,
}

/// The Store has accepted machine/selected data, or all represented App history too.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryReadiness {
    Primary,
    Complete,
}

#[derive(Debug)]
pub enum HistoryError {
    Pending,
    Read(persistence::Error),
}

pub type HistoryResult<T> = Result<T, HistoryError>;

impl From<persistence::Error> for HistoryError {
    fn from(error: persistence::Error) -> Self {
        Self::Read(error)
    }
}

impl std::fmt::Display for HistoryError {
    fn fmt(&self, output: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => output.write_str("App history is still being read"),
            Self::Read(error) => error.fmt(output),
        }
    }
}

impl std::error::Error for HistoryError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueryUnavailable {
    NoReadings,
    NoObservation,
    InvalidSecond,
    InvalidContextEndpoint,
    InvalidAlternative,
    ContextLimit,
    Budget,
    Apps(Missing),
    SourceFailure(Failure),
    Closed,
}

#[derive(Debug)]
struct ContextReservation {
    bytes: ByteReservation,
    budget: Arc<ByteBudget>,
    contexts: Arc<AtomicUsize>,
}

impl CheckedCoverage {
    #[cfg(test)]
    pub(crate) fn new(source: SourceGeneration, range: ContextRange) -> Self {
        assert!(range.first_ns < range.end_ns);

        Self {
            source,
            range: Some(range),
            requires_ring: true,
        }
    }

    pub(crate) fn checked(
        source: SourceGeneration,
        range: Option<ContextRange>,
        requires_ring: bool,
    ) -> Self {
        if let Some(range) = range {
            assert!(range.first_ns < range.end_ns);
        }

        Self {
            source,
            range,
            requires_ring,
        }
    }
}

impl QueryContext {
    #[must_use]
    pub fn readiness(&self) -> QueryReadiness {
        self.readiness
    }

    pub(super) fn history_ready(&self) -> HistoryResult<()> {
        match self.readiness {
            QueryReadiness::Primary => Err(HistoryError::Pending),
            QueryReadiness::Complete => Ok(()),
        }
    }
    #[must_use]
    pub fn has_committed_history(&self) -> bool {
        self.catalog
            .as_ref()
            .is_some_and(|catalog| catalog.windows > 0)
    }

    pub(super) fn catalog_metadata(&self) -> Option<&super::recording::CheckedCatalog> {
        self.catalog.as_ref()
    }

    pub(super) fn source_status(&self) -> Option<&super::ring::Status> {
        self.status.as_ref()
    }

    pub(super) fn latest(&self) -> (&Observation, super::native::Origin) {
        (&self.latest, self.latest_origin)
    }

    pub(super) fn raw_retention(&self) -> (u32, usize) {
        (self.raw_count, self.raw_bytes)
    }

    pub(super) fn followed(&self) -> bool {
        self.followed
    }

    pub(super) fn raw_reading(
        &self,
        observation: &Observation,
    ) -> Option<&super::raw_cache::RawReading> {
        self.raw
            .as_ref()
            .filter(|raw| raw.observation == *observation)
    }

    pub(crate) fn reserve_response(&self, bytes: usize) -> persistence::Result<ByteReservation> {
        self.reservation
            .budget
            .reserve(bytes)
            .ok_or(persistence::Error::Bound(
                "Store presentation admission bytes",
            ))
    }

    pub(crate) fn response_budget(&self) -> Arc<ByteBudget> {
        Arc::clone(&self.reservation.budget)
    }

    #[must_use]
    pub fn selected_second(&self) -> u64 {
        self.selection.second()
    }

    #[must_use]
    pub fn context_last_second(&self) -> u64 {
        self.selection.context_last_second()
    }

    #[must_use]
    pub fn seconds(&self) -> &[QuerySecond] {
        &self.seconds
    }

    #[must_use]
    pub fn selected(&self) -> &QuerySecond {
        self.seconds
            .iter()
            .find(|second| second.observation == self.selected)
            .unwrap_or_else(|| unreachable!("a context retains its selected observation"))
    }

    /// The alternatives retain the same exact identity order as the Store's display View.
    pub fn alternatives(&self, second: u64) -> impl Iterator<Item = Observation> + '_ {
        self.seconds
            .iter()
            .map(|second| second.observation)
            .filter(move |observation| selection::at_second(observation, second))
    }

    /// Inspects an exact held identity without moving the selection or its fixed endpoint.
    pub fn inspect(&self, observation: &Observation) -> Result<&QuerySecond, QueryNeedsLoad> {
        self.seconds
            .iter()
            .find(|second| second.observation == *observation)
            .ok_or(QueryNeedsLoad::MissingObservation)
    }

    /// Admitted context bytes, excluding the shared immutable machine and process owners.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.reservation.bytes.bytes()
            + self
                .seconds
                .iter()
                .map(|second| match &second.apps {
                    QueryApps::Held(_) => 0,
                    QueryApps::Detached(apps) => apps.bytes(),
                })
                .sum::<usize>()
    }
}

impl QuerySecond {
    pub(super) fn outside_cpu_predecessor(&self) -> Option<&ObservationHeader> {
        self.outside_cpu_predecessor.as_ref()
    }

    #[must_use]
    pub fn machine(&self) -> &MachineSecond {
        &self.machine
    }

    #[must_use]
    pub fn apps(&self) -> &AppsSecond {
        match &self.apps {
            QueryApps::Held(apps) => apps,
            QueryApps::Detached(apps) => apps,
        }
    }

    /// Followed contexts borrow pool owners briefly; fixed selections always detach them.
    #[must_use]
    pub fn detached(&self) -> bool {
        matches!(self.apps, QueryApps::Detached(_))
    }
}

impl ContextReservation {
    fn admit(
        contexts: &Arc<AtomicUsize>,
        budget: &Arc<ByteBudget>,
        bytes: usize,
    ) -> Result<Self, QueryUnavailable> {
        contexts
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |held| {
                (held < CONTEXTS_MAX).then_some(held + 1)
            })
            .map_err(|_| QueryUnavailable::ContextLimit)?;
        let Some(bytes) = budget.reserve(bytes) else {
            assert!(contexts.fetch_sub(1, Ordering::AcqRel) > 0);

            return Err(QueryUnavailable::Budget);
        };

        Ok(Self {
            bytes,
            budget: Arc::clone(budget),
            contexts: Arc::clone(contexts),
        })
    }
}

impl Drop for ContextReservation {
    fn drop(&mut self) {
        assert!(self.contexts.fetch_sub(1, Ordering::AcqRel) > 0);
    }
}

fn build(
    held: &Held,
    coverage: Option<&CheckedCoverage>,
    request: &QueryRequest,
    budget: &Arc<ByteBudget>,
    contexts: &Arc<AtomicUsize>,
    current: impl Fn() -> bool,
) -> Option<QueryOutcome> {
    if !current() {
        return None;
    }
    if let Some(failure) = held.failure.or(held.left_out) {
        return Some(QueryOutcome::Unavailable(QueryUnavailable::SourceFailure(
            failure,
        )));
    }
    let Some(coverage) = coverage else {
        return Some(QueryOutcome::NeedsLoad(QueryNeedsLoad::UncheckedCoverage));
    };
    if let Err(outcome) = build_available(held, coverage, request) {
        return Some(outcome);
    }
    let Some(newest) = held.apps.newest().and_then(|apps| apps.observation) else {
        return Some(QueryOutcome::Unavailable(QueryUnavailable::NoReadings));
    };
    let selection = match build_selection(&newest, &request.selection) {
        Ok(selection) => selection,
        Err(unavailable) => return Some(QueryOutcome::Unavailable(unavailable)),
    };
    let Ok(range) = ContextRange::charted_ending_at(selection.context_last_second()) else {
        return Some(QueryOutcome::Unavailable(QueryUnavailable::InvalidSecond));
    };
    if let Err(outcome) = build_covered(held, coverage, range) {
        return Some(outcome);
    }
    let selected = match build_selected(held, &newest, &request.selection) {
        Ok(selected) => selected,
        Err(outcome) => return Some(outcome),
    };

    let target = match build_target(held, coverage.source, request, selection, selected, range) {
        Ok(target) => target,
        Err(outcome) => return Some(outcome),
    };
    match build_context(held, &target, budget, contexts, current) {
        Ok(Some(context)) => Some(QueryOutcome::Ready(Arc::new(context))),
        Ok(None) => None,
        Err(unavailable) => Some(QueryOutcome::Unavailable(unavailable)),
    }
}

fn build_target(
    held: &Held,
    source: SourceGeneration,
    request: &QueryRequest,
    selection: ContextSelection,
    selected: Observation,
    range: ContextRange,
) -> Result<BuildTarget, QueryOutcome> {
    if held.startup == super::StartupState::Primary {
        if held.apps.held().any(|apps| {
            if apps.observation != Some(selected) {
                return false;
            }

            apps.missing == Some(Missing::Unread)
        }) {
            return Err(QueryOutcome::NeedsLoad(QueryNeedsLoad::AppsPending));
        }
    }
    let followed = matches!(request.selection, QuerySelection::FollowLatest);
    let mut retention = match request.selection {
        QuerySelection::FollowLatest => Retention::Followed,
        QuerySelection::Second { .. } | QuerySelection::Exact { .. } => Retention::Fixed,
    };
    let readiness = if held.startup == super::StartupState::Primary {
        retention = Retention::Fixed;
        QueryReadiness::Primary
    } else {
        QueryReadiness::Complete
    };
    Ok(BuildTarget {
        followed,
        readiness,
        source,
        selection,
        selected,
        range,
        retention,
    })
}

fn build_selection(
    newest: &Observation,
    selection: &QuerySelection,
) -> Result<ContextSelection, QueryUnavailable> {
    let newest_second = newest.wall_ns.map(|wall| wall / 1_000_000_000);
    let (second, last) = match selection {
        QuerySelection::FollowLatest => (
            newest_second.ok_or(QueryUnavailable::InvalidSecond)?,
            newest_second,
        ),
        QuerySelection::Second { second, .. } => (*second, newest_second),
        QuerySelection::Exact {
            observation,
            context_last_second,
        } => (
            observation.wall_ns.ok_or(QueryUnavailable::InvalidSecond)? / 1_000_000_000,
            Some(*context_last_second),
        ),
    };
    let selected = ContextSelection::of(second, last);
    if let QuerySelection::Exact {
        context_last_second,
        ..
    } = selection
    {
        if selected.context_last_second() != *context_last_second {
            return Err(QueryUnavailable::InvalidContextEndpoint);
        }
    }

    Ok(selected)
}

fn build_available(
    held: &Held,
    coverage: &CheckedCoverage,
    request: &QueryRequest,
) -> Result<(), QueryOutcome> {
    if let Some(source) = request.source {
        if source != coverage.source {
            return Err(QueryOutcome::NeedsLoad(QueryNeedsLoad::SourceChanged));
        }
    }
    if let Some(failure) = held.failure.or(held.left_out) {
        return Err(QueryOutcome::Unavailable(QueryUnavailable::SourceFailure(
            failure,
        )));
    }
    if coverage.requires_ring {
        if held.newest.is_some() {
            if !held.ring_complete {
                return Err(QueryOutcome::NeedsLoad(QueryNeedsLoad::IncompleteRing));
            }
        }
    }
    if let Some(newest) = &held.newest {
        if newest.run.recording != coverage.source.recording {
            return Err(QueryOutcome::NeedsLoad(QueryNeedsLoad::SourceChanged));
        }
    }

    Ok(())
}

fn build_selected(
    held: &Held,
    newest: &Observation,
    selection: &QuerySelection,
) -> Result<Observation, QueryOutcome> {
    let (second, alternative) = match selection {
        QuerySelection::FollowLatest => return Ok(*newest),
        QuerySelection::Exact { observation, .. } => {
            return held
                .apps
                .held()
                .filter_map(|apps| apps.observation)
                .find(|held| held == observation)
                .ok_or(QueryOutcome::Unavailable(QueryUnavailable::NoObservation));
        }
        QuerySelection::Second {
            second,
            alternative,
        } => (*second, *alternative),
    };
    let mut alternatives = Vec::with_capacity(held.apps.len());
    alternatives.extend(
        held.apps
            .held()
            .filter_map(|apps| apps.observation)
            .filter(|observation| selection::at_second(observation, second)),
    );
    alternatives.sort_by(selection::compare);
    if alternatives.is_empty() {
        return Err(QueryOutcome::Unavailable(QueryUnavailable::NoObservation));
    }

    let index = usize::try_from(alternative)
        .unwrap_or_else(|_| unreachable!("a u32 alternative fits a supported pointer"));
    alternatives
        .get(index)
        .copied()
        .ok_or(QueryOutcome::Unavailable(
            QueryUnavailable::InvalidAlternative,
        ))
}

fn build_covered(
    held: &Held,
    coverage: &CheckedCoverage,
    range: ContextRange,
) -> Result<(), QueryOutcome> {
    let Some(checked) = coverage.range else {
        return Err(QueryOutcome::NeedsLoad(QueryNeedsLoad::UncheckedCoverage));
    };
    if range.first_ns < checked.first_ns {
        return Err(QueryOutcome::NeedsLoad(QueryNeedsLoad::OutsideCoverage));
    }
    if range.end_ns > checked.end_ns {
        return Err(QueryOutcome::NeedsLoad(QueryNeedsLoad::OutsideCoverage));
    }
    if let Some(evicted) = held.full_detail_evicted_through_ns {
        if range.first_ns <= evicted {
            return Err(QueryOutcome::NeedsLoad(QueryNeedsLoad::FullDetailEvicted));
        }
    }

    Ok(())
}

struct BuildTarget {
    followed: bool,
    readiness: QueryReadiness,
    source: SourceGeneration,
    selection: ContextSelection,
    selected: Observation,
    range: ContextRange,
    retention: Retention,
}

fn build_context(
    held: &Held,
    target: &BuildTarget,
    budget: &Arc<ByteBudget>,
    contexts: &Arc<AtomicUsize>,
    current: impl Fn() -> bool,
) -> Result<Option<QueryContext>, QueryUnavailable> {
    assert_eq!(held.span.len(), held.apps.len());
    assert!(held.span.len() <= super::SECONDS_MAX);
    let count = held
        .span
        .held()
        .filter(|second| in_range(second.wall_ns, target.range))
        .count();
    let second_bytes = match target.retention {
        Retention::Followed => size_of::<QuerySecond>(),
        Retention::Fixed => {
            size_of::<QuerySecond>() + size_of::<MachineSecond>() + 2 * size_of::<usize>()
        }
    };
    let reservation = ContextReservation::admit(
        contexts,
        budget,
        size_of::<QueryContext>() + 2 * size_of::<usize>() + count * second_bytes,
    )?;
    let Some(seconds) = build_seconds(held, target, budget, count, current)? else {
        return Ok(None);
    };

    Ok(Some(QueryContext {
        readiness: target.readiness,
        source: target.source,
        catalog: held.checked_catalog,
        status: held.source_status,
        latest: held
            .apps
            .newest()
            .and_then(|apps| apps.observation)
            .unwrap_or_else(|| unreachable!("a query selects an accepted observation")),
        latest_origin: held
            .apps
            .newest()
            .unwrap_or_else(|| unreachable!("a query selects accepted Apps"))
            .metadata
            .origin,
        raw_count: held.raw.as_ref().map_or(0, super::raw_cache::RawCache::len),
        raw_bytes: held
            .raw
            .as_ref()
            .map_or(0, super::raw_cache::RawCache::bytes),
        followed: target.followed,
        raw: held.raw.as_ref().and_then(|raw| raw.find(&target.selected)),
        selection: target.selection,
        selected: target.selected,
        seconds,
        reservation,
    }))
}

fn build_seconds(
    held: &Held,
    target: &BuildTarget,
    budget: &Arc<ByteBudget>,
    count: usize,
    current: impl Fn() -> bool,
) -> Result<Option<Vec<QuerySecond>>, QueryUnavailable> {
    let mut seconds = Vec::with_capacity(count);
    let mut previous: Option<ObservationHeader> = None;
    for (machine, apps) in held.span.held().zip(held.apps.held()) {
        if !current() {
            return Ok(None);
        }
        if in_range(machine.wall_ns, target.range) {
            let observation = apps
                .observation
                .unwrap_or_else(|| unreachable!("every accepted second has an identity"));
            assert_eq!(observation.stamp.publication, machine.publication);
            let (machine, apps) = build_owners(machine, apps, target.retention, budget)?;
            seconds.push(QuerySecond {
                observation,
                machine,
                apps,
                outside_cpu_predecessor: previous.filter(|header| {
                    header
                        .observation
                        .wall_ns
                        .is_some_and(|wall| !in_range(wall, target.range))
                }),
            });
        }
        previous = apps.observation.map(|observation| ObservationHeader {
            observation,
            origin: apps.metadata.origin,
        });
    }
    seconds.sort_by(|before, after| selection::compare(&before.observation, &after.observation));
    assert_eq!(seconds.len(), count);
    assert!(
        seconds
            .iter()
            .any(|second| second.observation == target.selected)
    );

    Ok(Some(seconds))
}

fn build_owners(
    machine: &Arc<MachineSecond>,
    apps: &Arc<AppsSecond>,
    retention: Retention,
    budget: &Arc<ByteBudget>,
) -> Result<(Arc<MachineSecond>, QueryApps), QueryUnavailable> {
    match retention {
        Retention::Followed => Ok((Arc::clone(machine), QueryApps::Held(Arc::clone(apps)))),
        Retention::Fixed => {
            let apps = apps.detached(budget).map_err(|missing| {
                if missing == Missing::Budget {
                    QueryUnavailable::Budget
                } else {
                    QueryUnavailable::Apps(missing)
                }
            })?;

            Ok((
                Arc::new(machine.as_ref().clone()),
                QueryApps::Detached(Box::new(apps)),
            ))
        }
    }
}

fn in_range(wall_ns: u64, range: ContextRange) -> bool {
    if wall_ns < range.first_ns {
        return false;
    }

    wall_ns < range.end_ns
}
