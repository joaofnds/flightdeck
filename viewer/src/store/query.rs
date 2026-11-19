//! Immutable contexts built solely from already checked Store observations.
mod protocol;

pub(super) use protocol::Queries;
pub use protocol::{
    QueryHandle, QueryReply, QueryRequest, QuerySelection, QuerySubmitError, QueryToken,
};

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use collector::ring::Identity;

use super::selection;
use super::{Block, ByteBudget, ByteReservation, Failure, Held, MachineSecond, Missing, Second};
use crate::live_app_response::Observation;
use crate::session::{ContextRange, ContextSelection};

#[cfg(test)]
#[path = "query_tests.rs"]
mod tests;

/// One displayed context and one replacement may hold blocks the store has let go of at the
/// same time.
const CONTEXTS_MAX: usize = 2;

/// A checked source/catalog generation, independent of display publication versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceGeneration {
    pub recording: Identity,
    pub generation: u64,
}

/// What the Store held when it built a context: the version of the last View it published and
/// the newest second it took. A second taken moves the newest second, also while the main thread
/// holds every View, and every other change of what a context holds moves the version at the
/// View that shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct QueryKey {
    version: u64,
    newest: Option<super::Published>,
}

impl QueryKey {
    fn of(held: &Held) -> Self {
        Self {
            version: held.version,
            newest: held.newest_published(),
        }
    }
}

/// A loader/source acceptance certificate. Only the Store producer may construct one.
#[derive(Clone, Copy)]
pub(crate) struct CheckedCoverage {
    source: SourceGeneration,
    range: Option<ContextRange>,
    requires_ring: bool,
}

/// A followed context shares the store's machine buffers and the blocks holding its Apps. A
/// fixed context copies its machine readings and shares the blocks, which the store never
/// writes again. Neither holds raw frames or derived samples.
#[derive(Debug)]
pub struct QueryContext {
    readiness: QueryReadiness,
    pub source: SourceGeneration,
    catalog: Option<super::recording::CheckedCatalog>,
    status: Option<super::ring::Status>,
    latest: Observation,
    latest_origin: super::native::Origin,
    followed: bool,
    selection: ContextSelection,
    selected: Observation,
    seconds: Vec<QuerySecond>,
    reservation: ContextReservation,
    key: QueryKey,
}

/// Independent facets remain available when another facet was not retained.
#[derive(Debug)]
pub struct QuerySecond {
    pub observation: Observation,
    machine: Arc<MachineSecond>,
    apps: QueryApps,
    retention: Retention,
    outside_cpu_predecessor: Option<ObservationHeader>,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct ObservationHeader {
    pub observation: Observation,
    pub origin: super::native::Origin,
}

/// The block holding a context's second, shared with the store and every View that holds it,
/// and the second's index in it.
#[derive(Debug)]
struct QueryApps {
    block: Arc<Block>,
    index: usize,
}

#[derive(Clone, Copy, Debug)]
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
    #[cfg(any(test, feature = "fixtures"))]
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

    pub(super) fn followed(&self) -> bool {
        self.followed
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

    /// What the Store held when it built the context, which a follow question that names it
    /// waits on to change.
    #[must_use]
    pub fn key(&self) -> QueryKey {
        self.key
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

    /// The bytes admitted for the context, the machine readings a fixed context copies
    /// included, leaving out the machine buffers and the blocks of Apps it shares.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.reservation.bytes.bytes()
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
    pub fn apps(&self) -> Second<'_> {
        self.apps.block.second(self.apps.index)
    }

    /// Whether the context is fixed on its second, so it answers a read of that second whatever
    /// the store's span has done since. A followed context moves with the newest second.
    #[must_use]
    pub fn fixed(&self) -> bool {
        matches!(self.retention, Retention::Fixed)
    }

    /// The block holding the second's Apps.
    pub(super) fn block(&self) -> &Arc<Block> {
        &self.apps.block
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
    let coverage = match build_coverage(held, coverage) {
        Ok(coverage) => coverage,
        Err(outcome) => return Some(outcome),
    };
    if let Err(outcome) = build_available(held, coverage, request) {
        return Some(outcome);
    }
    let Some(newest) = held.apps.span().newest().and_then(Second::observation) else {
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
        if held.apps.span().seconds().any(|apps| {
            if apps.observation() != Some(selected) {
                return false;
            }

            apps.missing() == Some(&Missing::Unread)
        }) {
            return Err(QueryOutcome::NeedsLoad(QueryNeedsLoad::AppsPending));
        }
    }
    let followed = matches!(
        request.selection,
        QuerySelection::FollowLatest | QuerySelection::FollowLatestAfter(_)
    );
    let mut retention = match request.selection {
        QuerySelection::FollowLatest | QuerySelection::FollowLatestAfter(_) => Retention::Followed,
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
        QuerySelection::FollowLatest | QuerySelection::FollowLatestAfter(_) => (
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
    if let Some(failure) = refusal(held) {
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

/// Why the Store answers no query: what it reads failed, or it was left with nothing to show. A
/// minute left out beside other readings costs only the seconds it holds.
fn refusal(held: &Held) -> Option<Failure> {
    held.failure
        .or(held.left_out.filter(|_| held.newest.is_none()))
}

/// The checked coverage a query is answered from, or the Store's first answer when it refuses
/// every query or has not checked what it holds.
fn build_coverage<'coverage>(
    held: &Held,
    coverage: Option<&'coverage CheckedCoverage>,
) -> Result<&'coverage CheckedCoverage, QueryOutcome> {
    if let Some(failure) = refusal(held) {
        return Err(QueryOutcome::Unavailable(QueryUnavailable::SourceFailure(
            failure,
        )));
    }
    coverage.ok_or(QueryOutcome::NeedsLoad(QueryNeedsLoad::UncheckedCoverage))
}

/// Whether the Store would answer a query only that it has not checked what it holds.
pub(super) fn unchecked(held: &Held, coverage: Option<&CheckedCoverage>) -> bool {
    matches!(
        build_coverage(held, coverage),
        Err(QueryOutcome::NeedsLoad(QueryNeedsLoad::UncheckedCoverage))
    )
}

fn build_selected(
    held: &Held,
    newest: &Observation,
    selection: &QuerySelection,
) -> Result<Observation, QueryOutcome> {
    let (second, alternative) = match selection {
        QuerySelection::FollowLatest | QuerySelection::FollowLatestAfter(_) => return Ok(*newest),
        QuerySelection::Exact { observation, .. } => {
            return held
                .apps
                .span()
                .seconds()
                .filter_map(Second::observation)
                .find(|held| held == observation)
                .ok_or(QueryOutcome::Unavailable(QueryUnavailable::NoObservation));
        }
        QuerySelection::Second {
            second,
            alternative,
        } => (*second, *alternative),
    };
    let mut alternatives = Vec::with_capacity(held.apps.span().len());
    alternatives.extend(
        held.apps
            .span()
            .seconds()
            .filter_map(Second::observation)
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
    assert_eq!(held.span.len(), held.apps.span().len());
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
    let Some(seconds) = build_seconds(held, target, count, current) else {
        return Ok(None);
    };

    Ok(Some(QueryContext {
        readiness: target.readiness,
        source: target.source,
        catalog: held.checked_catalog,
        status: held.source_status,
        latest: held
            .apps
            .span()
            .newest()
            .and_then(Second::observation)
            .unwrap_or_else(|| unreachable!("a query selects an accepted observation")),
        latest_origin: held
            .apps
            .span()
            .newest()
            .unwrap_or_else(|| unreachable!("a query selects accepted Apps"))
            .origin(),
        followed: target.followed,
        selection: target.selection,
        selected: target.selected,
        seconds,
        reservation,
        key: QueryKey::of(held),
    }))
}

fn build_seconds(
    held: &Held,
    target: &BuildTarget,
    count: usize,
    current: impl Fn() -> bool,
) -> Option<Vec<QuerySecond>> {
    let mut seconds = Vec::with_capacity(count);
    let mut previous: Option<ObservationHeader> = None;
    let mut machines = held.span.held();
    for (block, indices) in held.apps.span().block_ranges() {
        for index in indices {
            let machine = machines
                .next()
                .unwrap_or_else(|| unreachable!("both spans hold the same seconds"));
            if !current() {
                return None;
            }
            let apps = block.second(index);
            if in_range(machine.wall_ns, target.range) {
                let observation = apps
                    .observation()
                    .unwrap_or_else(|| unreachable!("every accepted second has an identity"));
                assert_eq!(observation.stamp.publication, machine.publication);
                let (machine, apps) = build_owners(machine, block, index, target.retention);
                seconds.push(QuerySecond {
                    observation,
                    machine,
                    apps,
                    retention: target.retention,
                    outside_cpu_predecessor: previous.filter(|header| {
                        header
                            .observation
                            .wall_ns
                            .is_some_and(|wall| !in_range(wall, target.range))
                    }),
                });
            }
            previous = apps.observation().map(|observation| ObservationHeader {
                observation,
                origin: apps.origin(),
            });
        }
    }
    assert!(machines.next().is_none());
    seconds.sort_by(|before, after| selection::compare(&before.observation, &after.observation));
    assert_eq!(seconds.len(), count);
    assert!(
        seconds
            .iter()
            .any(|second| second.observation == target.selected)
    );

    Some(seconds)
}

/// The second's machine readings and Apps as a context of `retention` holds them. A fixed
/// context copies the machine readings so it holds no machine buffer, and shares the block
/// holding the Apps, which the store never writes again.
fn build_owners(
    machine: &Arc<MachineSecond>,
    block: &Arc<Block>,
    index: usize,
    retention: Retention,
) -> (Arc<MachineSecond>, QueryApps) {
    let machine = match retention {
        Retention::Followed => Arc::clone(machine),
        Retention::Fixed => Arc::new(machine.as_ref().clone()),
    };
    let apps = QueryApps {
        block: Arc::clone(block),
        index,
    };

    (machine, apps)
}

fn in_range(wall_ns: u64, range: ContextRange) -> bool {
    if wall_ns < range.first_ns {
        return false;
    }

    wall_ns < range.end_ns
}
