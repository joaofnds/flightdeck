//! Finite startup work owned by the existing Store thread. Native batches never escape a step.
use std::mem;
use std::path::Path;
use std::sync::Arc;

use persistence::engine::{AdmissionRefusal, AdmittedTable};
use persistence::{Error, Manifest};

use super::{CheckedCatalog, Found, LoadWindow, MINUTES_MAX, catalog, payload};
use crate::byte_budget::{ByteBudget, ByteReservation};
use crate::cancellation::Operation;
use crate::live_app_response::Observation;
use crate::live_apps::committed::Endpoint;
use crate::recording::{
    Before, MachineMinute, Refused, ScopedSealedMinute, SealedSource, read_machine_and_endpoints,
};
use crate::store::apps::TickApps;
use crate::store::{AppsSecond, Held, Missing, SampledSecond};
use payload::replay::{Captured, Progress, Target as ReplayTarget};

#[cfg(test)]
#[path = "recording_startup_tests.rs"]
mod tests;

pub(in crate::store) struct Startup {
    minutes: Vec<MinuteWork>,
    phase: Phase,
    selected: Option<usize>,
    cursor: payload::Cursor,
    history_index: usize,
    pending: Option<Candidate>,
    checked: Option<CheckedCatalog>,
    control: Option<ByteReservation>,
}

struct MinuteWork {
    found: Found,
    witness: Option<Witness>,
    next_endpoint: usize,
    boundary_path: Option<ByteReservation>,
}

struct Witness {
    manifest: Manifest,
    endpoints: Vec<Endpoint>,
    reservation: ByteReservation,
}

struct Candidate {
    observation: Observation,
    apps: AppsSecond,
    sampled: Option<SampledSecond>,
    before: Option<Before>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Phase {
    Discover,
    Machine,
    SelectedBoundary,
    Selected,
    Terminal,
    Barrier,
    HistoryBoundary,
    History,
    Complete,
}

pub(in crate::store) struct Input<'a> {
    pub root: &'a Path,
    pub budget: &'a Arc<ByteBudget>,
    pub operation: &'a Operation,
}

pub(in crate::store) enum Advance {
    Progress,
    Blocked,
    Primary,
    Complete,
}

#[derive(Debug)]
pub(in crate::store) enum Failure {
    Blocked,
    Read(Refused),
    Catalog(crate::store::Failure),
}

impl From<Refused> for Failure {
    fn from(error: Refused) -> Self {
        if matches!(error, Refused::Input(AdmissionRefusal::Budget)) {
            Self::Blocked
        } else {
            Self::Read(error)
        }
    }
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self::Read(Refused::Decode(error))
    }
}

impl Startup {
    fn process(
        &mut self,
        input: &Input<'_>,
        held: &mut Held,
        index: usize,
    ) -> Result<bool, Failure> {
        process_minute(
            &mut self.minutes[index],
            &mut self.cursor.decoder,
            &mut self.pending,
            input,
            held,
        )
    }
    pub(in crate::store) fn new() -> Self {
        Self {
            minutes: Vec::new(),
            phase: Phase::Discover,
            selected: None,
            cursor: payload::Cursor::new(0),
            history_index: 0,
            pending: None,
            checked: None,
            control: None,
        }
    }

    pub(in crate::store) fn advance(
        &mut self,
        input: &Input<'_>,
        held: &mut Held,
        ring: &mut crate::store::Ring,
    ) -> Result<Advance, Failure> {
        input.operation.check()?;
        held.release_views();
        let result = match self.phase {
            Phase::Discover => self.discover(input),
            Phase::Machine => self.machine(input, held),
            Phase::SelectedBoundary => self.boundary(
                input,
                self.target_boundary(self.selected, held),
                Phase::Selected,
            ),
            Phase::Selected => self.selected(input, held),
            Phase::Terminal => self.terminal(input, ring),
            Phase::Barrier => return Ok(Advance::Primary),
            Phase::HistoryBoundary => self.history_boundary(input, held),
            Phase::History => self.history(input, held),
            Phase::Complete => return Ok(Advance::Complete),
        };
        match result {
            Ok(()) => Ok(Advance::Progress),
            Err(Failure::Blocked) => Ok(Advance::Blocked),
            Err(error) => Err(error),
        }
    }

    fn discover(&mut self, input: &Input<'_>) -> Result<(), Failure> {
        let initial = input
            .budget
            .reserve(size_of::<Self>())
            .ok_or(Failure::Blocked)?;
        let mut found = Vec::with_capacity(MINUTES_MAX);
        let checked = catalog::find(
            input.root,
            &mut found,
            &LoadWindow::all(None),
            input.operation,
        )
        .map_err(Failure::Catalog)?;
        found.reverse();
        let path_bytes: usize = found
            .iter()
            .map(|found| {
                found.marker.capacity()
                    + found
                        .previous_source
                        .as_ref()
                        .map_or(0, |key| key.marker.capacity())
            })
            .sum();
        let control = input
            .budget
            .reserve(size_of::<Self>() + MINUTES_MAX * size_of::<MinuteWork>() + path_bytes)
            .ok_or(Failure::Blocked)?;
        self.minutes = Vec::with_capacity(MINUTES_MAX);
        self.minutes
            .extend(found.into_iter().map(|found| MinuteWork {
                found,
                witness: None,
                next_endpoint: 0,
                boundary_path: None,
            }));
        self.cursor = payload::Cursor::new(self.minutes.len());
        self.checked = Some(checked);
        self.control = Some(control);
        drop(initial);
        self.phase = Phase::Machine;
        Ok(())
    }

    fn machine(&mut self, input: &Input<'_>, held: &mut Held) -> Result<(), Failure> {
        assert!(held.startup == crate::store::StartupState::Loading);
        if self.cursor.is_complete() {
            if let Some(checked) = &mut self.checked {
                if self.cursor.decoder.decoded_catalog.windows == checked.windows {
                    checked.first_known_second =
                        self.cursor.decoder.decoded_catalog.first_known_second;
                    checked.last_known_second =
                        self.cursor.decoder.decoded_catalog.last_known_second;
                    checked.unknown_clock_ticks =
                        Some(self.cursor.decoder.decoded_catalog.unknown_clock_ticks);
                }
            }
            self.phase = Phase::SelectedBoundary;
            return Ok(());
        }

        let minute = &mut self.minutes[self.cursor.next_minute_index];
        let sealed = payload::open_minute(
            input.root,
            &minute.found,
            input.budget,
            input.operation,
            &AdmittedTable::from_file_budgeted,
        )?;
        let mut machine = MachineMinute::default();
        let endpoints = read_machine_and_endpoints(&sealed, &mut machine)?;
        let machine_reservation = input
            .budget
            .reserve(machine.heap_bytes())
            .ok_or(Failure::Blocked)?;
        let mut seconds = Vec::with_capacity(crate::live_apps::committed::ENDPOINTS_MAX);
        let decoded = self
            .cursor
            .decoder
            .project_machine(&minute.found, &machine, input.budget, |second| {
                seconds.push(second);
            })
            .map_err(machine_failure)?;
        sealed.revalidate_full()?;
        input.operation.check()?;
        let witness = Witness::admit(sealed.into_manifest(), endpoints, input.budget)?;
        for second in &seconds {
            input.operation.check()?;
            let mut apps = AppsSecond {
                observation: Some(crate::store::take_observation(&minute.found.run, second)),
                ..AppsSecond::default()
            };
            apps.metadata.origin = crate::store::native::Origin::Saved(witness.manifest.losses);
            // No query or View borrows these pools before Primary; unread sampling cannot refuse.
            assert!(held.take(&minute.found.run, second, TickApps::Loaded(&mut apps)));
        }
        if !seconds.is_empty() {
            self.selected = Some(self.cursor.next_minute_index);
        }
        self.cursor.decoder.accept_machine(
            minute.found.run,
            machine,
            &decoded,
            Some(machine_reservation),
        );
        minute.witness = Some(witness);
        self.cursor.accept_minute();
        Ok(())
    }

    fn boundary(
        &mut self,
        input: &Input<'_>,
        index: Option<usize>,
        next: Phase,
    ) -> Result<(), Failure> {
        self.cursor.decoder.predecessor = None;
        if let Some(index) = index {
            if let Some(minute) = self.minutes.get(index) {
                if let Some(key) = &minute.found.previous_source {
                    let existing = self
                        .minutes
                        .iter()
                        .position(|minute| minute.found.marker == key.marker);
                    let boundary = if let Some(index) = existing {
                        index
                    } else {
                        if self.minutes.len() == MINUTES_MAX {
                            return Err(Error::Bound("startup boundary minutes").into());
                        }

                        let boundary_path = input
                            .budget
                            .reserve(key.marker.capacity())
                            .ok_or(Failure::Blocked)?;
                        let found = Found {
                            run: key.run,
                            marker: key.marker.clone(),
                            publication_end: key.publication_end,
                            wall_ns_max: None,
                            previous_source: None,
                        };
                        assert!(found.marker.capacity() <= boundary_path.bytes());
                        self.minutes.push(MinuteWork {
                            found,
                            witness: None,
                            next_endpoint: 0,
                            boundary_path: Some(boundary_path),
                        });
                        self.minutes.len() - 1
                    };
                    self.cursor.decoder.predecessor = self.predecessor(input, boundary)?;
                }
            }
        }
        self.phase = next;
        Ok(())
    }

    fn predecessor(&mut self, input: &Input<'_>, index: usize) -> Result<Option<Before>, Failure> {
        let _purpose = persistence::work::ingestion::purpose(
            persistence::work::ingestion::Purpose::Predecessor,
        );
        let minute = &mut self.minutes[index];
        if minute.witness.is_none() {
            let sealed = payload::open_minute(
                input.root,
                &minute.found,
                input.budget,
                input.operation,
                &AdmittedTable::from_file_budgeted,
            )?;
            let endpoints = sealed.endpoints()?;
            sealed.revalidate_full()?;
            minute.witness = Some(Witness::admit(
                sealed.into_manifest(),
                endpoints,
                input.budget,
            )?);
        }
        let witness = minute
            .witness
            .as_ref()
            .unwrap_or_else(|| unreachable!("boundary witness"));
        let sealed = witness.open(input, &minute.found)?;
        let batch = crate::recording::processes::read_processes_batch_with_endpoints(
            &sealed,
            witness.endpoints.clone(),
        )?;
        self.cursor.decoder.predecessor = None;
        let mut next = 0;
        self.cursor.decoder.replay(
            &batch,
            &mut payload::replay::Input {
                next: &mut next,
                operation: input.operation,
                budget: Some(input.budget),
            },
            &mut Predecessor,
        )?;
        sealed.revalidate_full()?;
        Ok(self.cursor.decoder.predecessor.take())
    }

    fn selected(&mut self, input: &Input<'_>, held: &mut Held) -> Result<(), Failure> {
        let _purpose =
            persistence::work::ingestion::purpose(persistence::work::ingestion::Purpose::Selection);
        if let Some(index) = self.selected {
            if !self.process(input, held, index)? {
                return Err(Failure::Blocked);
            }
        }
        self.phase = Phase::Terminal;
        Ok(())
    }

    fn terminal(
        &mut self,
        input: &Input<'_>,
        ring: &mut crate::store::Ring,
    ) -> Result<(), Failure> {
        let terminal_index = self.cursor.next_minute_index.checked_sub(1);
        if terminal_index != self.selected {
            self.cursor.decoder.predecessor = match terminal_index {
                Some(index) => self.predecessor(input, index)?,
                None => None,
            };
        }
        self.revalidate(input)?;
        input.operation.check()?;
        let terminal = self.cursor.decoder.terminal();
        ring.accept(terminal);
        self.phase = Phase::Barrier;
        Ok(())
    }

    pub(in crate::store) fn primary(&mut self, held: &mut Held) -> Result<(), Failure> {
        assert!(self.phase == Phase::Barrier);
        crate::store::coverage::accept_catalog(held, self.checked)?;
        held.covers_hour = true;
        held.startup = crate::store::StartupState::Primary;
        self.phase = Phase::HistoryBoundary;
        Ok(())
    }

    fn history(&mut self, input: &Input<'_>, held: &mut Held) -> Result<(), Failure> {
        let end = self.selected.unwrap_or(0);
        if self.history_index < end {
            if !self.process(input, held, self.history_index)? {
                return Err(Failure::Blocked);
            }

            self.history_index += 1;
            return Ok(());
        }

        self.revalidate(input)?;
        input.operation.check()?;
        held.context.rebuild(&held.span, &held.apps);
        held.startup = crate::store::StartupState::Complete;
        self.cursor.decoder.predecessor = None;
        self.phase = Phase::Complete;
        Ok(())
    }

    fn history_boundary(&mut self, input: &Input<'_>, held: &Held) -> Result<(), Failure> {
        let end = self.selected.unwrap_or(0);
        let first = self.minutes[..end].iter().position(|minute| {
            minute.witness.as_ref().is_some_and(|witness| {
                witness.endpoints.iter().any(|endpoint| {
                    !matches!(target(held, &endpoint.observation()), Target::Outside)
                })
            })
        });
        self.history_index = first.unwrap_or(end);
        self.boundary(input, self.target_boundary(first, held), Phase::History)
    }

    fn target_boundary(&self, index: Option<usize>, held: &Held) -> Option<usize> {
        index.filter(|index| {
            self.minutes[*index]
                .witness
                .as_ref()
                .and_then(|witness| witness.endpoints.first())
                .is_some_and(|endpoint| {
                    !matches!(target(held, &endpoint.observation()), Target::Outside)
                })
        })
    }

    pub(in crate::store) fn revalidate(&self, input: &Input<'_>) -> Result<(), Failure> {
        if let Some(checked) = &self.checked {
            catalog::revalidate(input.root, checked).map_err(Failure::Catalog)?;
        }
        for minute in &self.minutes {
            if let Some(reservation) = &minute.boundary_path {
                assert!(minute.found.marker.capacity() <= reservation.bytes());
            }
            if let Some(witness) = &minute.witness {
                witness.revalidate(input, &minute.found)?;
            }
        }
        input.operation.check()?;
        Ok(())
    }
}

impl Witness {
    fn admit(
        manifest: Manifest,
        endpoints: Vec<Endpoint>,
        budget: &Arc<ByteBudget>,
    ) -> Result<Self, Failure> {
        let bytes = size_of::<Self>()
            + manifest.data_directory.capacity()
            + manifest.tables.capacity() * size_of::<persistence::TableManifest>()
            + endpoints.capacity() * size_of::<Endpoint>();
        let reservation = budget.reserve(bytes).ok_or(Failure::Blocked)?;
        Ok(Self {
            manifest,
            endpoints,
            reservation,
        })
    }

    fn open<'a>(
        &self,
        input: &'a Input<'a>,
        found: &Found,
    ) -> Result<ScopedSealedMinute<'a>, Failure> {
        Ok(ScopedSealedMinute::open_processes_using(
            &SealedSource {
                root: input.root,
                marker: &found.marker,
            },
            &self.manifest,
            input.budget,
            input.operation,
            &AdmittedTable::from_file_budgeted,
        )?)
    }

    fn revalidate(&self, input: &Input<'_>, found: &Found) -> Result<(), Failure> {
        assert!(self.reservation.bytes() >= self.endpoints.capacity() * size_of::<Endpoint>());
        let sealed = self.open(input, found)?;
        sealed.revalidate_full()?;
        Ok(())
    }
}

fn machine_failure(error: crate::store::MachineCaptureError) -> Failure {
    match error {
        crate::store::MachineCaptureError::Admission(_) => Failure::Blocked,
        crate::store::MachineCaptureError::Projection(error) => error.into(),
    }
}

struct Predecessor;

impl ReplayTarget for Predecessor {
    type Error = Failure;

    fn derives(&self, _endpoint: &Endpoint) -> bool {
        false
    }

    fn consume(&mut self, captured: Captured<'_>) -> Result<Progress, Failure> {
        Ok(Progress::Accepted(captured.before))
    }
}

fn process_minute(
    minute: &mut MinuteWork,
    decoder: &mut payload::Decoder,
    pending: &mut Option<Candidate>,
    input: &Input<'_>,
    held: &mut Held,
) -> Result<bool, Failure> {
    let witness = minute
        .witness
        .as_ref()
        .unwrap_or_else(|| unreachable!("machine witness"));
    let sealed = witness.open(input, &minute.found)?;
    if let Some(mut candidate) = pending.take() {
        input.operation.check()?;
        if !install_ready(&mut candidate, held, input.budget)? {
            *pending = Some(candidate);
            return Ok(false);
        }

        decoder.accept_replay(candidate.before.take(), &mut minute.next_endpoint);
    }
    if minute.next_endpoint == witness.endpoints.len() {
        sealed.revalidate_full()?;
        input.operation.check()?;
        return Ok(true);
    }

    let batch = crate::recording::processes::read_processes_batch_with_endpoints(
        &sealed,
        witness.endpoints.clone(),
    )?;
    decoder.replay(
        &batch,
        &mut payload::replay::Input {
            next: &mut minute.next_endpoint,
            operation: input.operation,
            budget: Some(input.budget),
        },
        &mut InstallTarget {
            names: None,
            held,
            pending,
            input,
        },
    )?;
    sealed.revalidate_full()?;
    input.operation.check()?;
    Ok(pending.is_none())
}

#[derive(Clone, Copy)]
enum Target {
    Recent(usize),
    Older(usize),
    Outside,
}

fn target(held: &Held, observation: &Observation) -> Target {
    if let Some(index) = held
        .apps
        .held()
        .position(|apps| apps.observation == Some(*observation))
    {
        return Target::Recent(index);
    }

    if let Some(sampled) = &held.sampled {
        if let Some(index) = sampled
            .held()
            .position(|sample| sample.observation == *observation)
        {
            return Target::Older(index);
        }
    }
    Target::Outside
}

struct InstallTarget<'step, 'input> {
    names: Option<crate::store::payload::ProcessNames>,
    held: &'step mut Held,
    pending: &'step mut Option<Candidate>,
    input: &'step Input<'input>,
}

impl ReplayTarget for InstallTarget<'_, '_> {
    type Error = Failure;

    fn derives(&self, endpoint: &Endpoint) -> bool {
        !matches!(target(self.held, &endpoint.observation()), Target::Outside)
    }

    fn consume(&mut self, captured: Captured<'_>) -> Result<Progress, Failure> {
        let mut candidate = capture_candidate(captured, self.input, self.held, &mut self.names)?;
        self.input.operation.check()?;
        if !install_ready(&mut candidate, self.held, self.input.budget)? {
            *self.pending = Some(candidate);
            return Ok(Progress::Pending);
        }
        Ok(Progress::Accepted(candidate.before.take()))
    }
}

fn capture_candidate(
    captured: Captured<'_>,
    input: &Input<'_>,
    held: &Held,
    names: &mut Option<crate::store::payload::ProcessNames>,
) -> Result<Candidate, Failure> {
    let observation = captured.endpoint.observation();
    let mut apps = AppsSecond::default();
    if let Some(sample) = captured.sample {
        let sample = match sample {
            Err(crate::live_apps::AggregateError::Presentation(
                error @ (Error::Io(_) | Error::Engine(_)),
            )) => return Err(error.into()),
            sample => sample.map_err(crate::live_apps::AggregateError::legacy),
        };
        let target = target(held, &observation);
        let previous = match target {
            Target::Recent(index) => index.checked_sub(1).and_then(|index| held.apps.get(index)),
            Target::Older(_) | Target::Outside => None,
        };
        match target {
            Target::Recent(_) => {
                payload::load_minute_rows(&sample, previous, input.budget, &mut apps);
                payload::load_minute_payload(
                    &sample,
                    input.budget,
                    captured.names_capacity,
                    names,
                    &mut apps,
                );
            }
            Target::Older(_) => {
                payload::load_minute_rows(&sample, previous, input.budget, &mut apps);
            }
            Target::Outside => unreachable!("outside does not derive Apps"),
        }
        if apps.missing == Some(Missing::Budget) {
            return Err(Failure::Blocked);
        }

        if matches!(apps.processes, Err(Missing::Budget)) {
            return Err(Failure::Blocked);
        }
    }
    apps.observation = Some(observation);
    Ok(Candidate {
        observation,
        apps,
        sampled: None,
        before: captured.before,
    })
}

fn install(
    candidate: &mut Candidate,
    held: &mut Held,
    budget: &Arc<ByteBudget>,
) -> Result<bool, Failure> {
    held.release_views();
    match target(held, &candidate.observation) {
        Target::Outside => Ok(true),
        Target::Recent(index) => {
            assert!(candidate.sampled.is_none());
            let previous = held
                .apps
                .get(index)
                .unwrap_or_else(|| unreachable!("recent target"));
            candidate.apps.consecutive = previous.consecutive;
            candidate.apps.metadata.origin = previous.metadata.origin;
            Ok(held
                .apps
                .replace(index, |apps| mem::swap(apps, &mut candidate.apps)))
        }
        Target::Older(index) => {
            let sampled = held
                .sampled
                .as_mut()
                .unwrap_or_else(|| unreachable!("older target"));
            if candidate.sampled.is_none() {
                let machine = &sampled
                    .get(index)
                    .unwrap_or_else(|| unreachable!("older sample"))
                    .machine;
                candidate.sampled = Some(
                    SampledSecond::admitted(machine, &candidate.apps, budget)
                        .map_err(|_| Failure::Blocked)?,
                );
                candidate.apps = AppsSecond::default();
            }
            Ok(sampled.replace(index, |sample| {
                mem::swap(
                    sample,
                    candidate
                        .sampled
                        .as_mut()
                        .unwrap_or_else(|| unreachable!("ranking")),
                );
            }))
        }
    }
}

fn install_ready(
    candidate: &mut Candidate,
    held: &mut Held,
    budget: &Arc<ByteBudget>,
) -> Result<bool, Failure> {
    match install(candidate, held, budget) {
        Err(Failure::Blocked) => Ok(false),
        result => result,
    }
}
