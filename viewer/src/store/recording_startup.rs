//! Finite startup work owned by the existing Store thread. Native batches never escape a step.
use std::mem;
use std::path::Path;
use std::sync::Arc;

use persistence::engine::{AdmissionRefusal, AdmittedTable};
use persistence::{Error, Manifest};

use super::{CheckedCatalog, Found, LoadWindow, MINUTES_MAX, Owed, catalog, payload};
use crate::byte_budget::{ByteBudget, ByteReservation};
use crate::cancellation::Operation;
use crate::live_app_response::Observation;
use crate::live_apps::committed::Endpoint;
use crate::recording::processes::ProcessBatch;
use crate::recording::{
    Before, InputCapture, MachineMinute, Refused, ScopedSealedMinute, SealedSource,
    read_machine_and_endpoints,
};
use crate::store::apps::TickApps;
use crate::store::{AppsSecond, Held, Missing};
use payload::replay::{Captured, Target as ReplayTarget};

#[cfg(test)]
#[path = "recording_startup_tests.rs"]
mod tests;

/// The neighbouring minutes of the history the launch reads and decodes at once, a thread each.
/// It is the launch's own, apart from the bound of the load a returning window waits on, since
/// the launch holds each minute's Apps until it installs them.
const HISTORY_GROUP_MINUTES_MAX: usize = 3;

/// The most minutes a group of the history may hold, whatever `HISTORY_GROUP_MINUTES_MAX` is set
/// to, which bounds the threads one step of the launch starts.
const HISTORY_GROUP_MINUTES_BOUND: usize = 6;
const _: () = assert!(HISTORY_GROUP_MINUTES_MAX <= HISTORY_GROUP_MINUTES_BOUND);

pub(in crate::store) struct Startup {
    minutes: Vec<MinuteWork>,
    phase: Phase,
    selected: Option<usize>,
    cursor: payload::Cursor,
    history_index: usize,
    checked: Option<CheckedCatalog>,
    control: Option<ByteReservation>,
    /// How the launch reads each file of a minute it opens through the minute's witness: each
    /// minute of the history, the minute before the boundary and each minute the check before
    /// the launch completes. The first open of a minute, which finds its witness in `machine` or
    /// `predecessor`, reads its files directly.
    capture: &'static InputCapture<'static>,
    /// How many minutes of the history a step decodes at once. A group the budget refuses
    /// leaves the rest of the history to one minute a step.
    history_group_minutes_max: usize,
    /// How a thread decodes a later minute of a group, which a test swaps to refuse its work or
    /// to seed it from another frame.
    guessing: HistoryGuessing,
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
    /// Where the candidate's second stands among the seconds of the live span, or `None` where
    /// the launch derives no Apps of it.
    recent: Option<usize>,
    apps: AppsSecond,
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
    fn process(&mut self, input: &Input<'_>, held: &mut Held, index: usize) -> Result<(), Failure> {
        process_minute(
            &mut self.minutes[index],
            &mut self.cursor.decoder,
            input,
            self.capture,
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
            checked: None,
            control: None,
            capture: &AdmittedTable::from_file_budgeted,
            history_group_minutes_max: HISTORY_GROUP_MINUTES_MAX,
            guessing: history_group_guess,
        }
    }

    /// Whether the launch has taken nothing in, as `new` leaves it: it has found no minute,
    /// checked no catalog and left the discovery of its first step.
    pub(in crate::store) fn took_nothing_in(&self) -> bool {
        self.phase == Phase::Discover
            && self.minutes.is_empty()
            && self.checked.is_none()
            && self.control.is_none()
    }

    /// A launch whose history installs one minute a step, so a test can put a tick between two
    /// of its minutes.
    #[cfg(test)]
    pub(in crate::store) fn one_minute_a_step() -> Self {
        Self {
            history_group_minutes_max: 1,
            ..Self::new()
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
        let checked = catalog::find(input.root, &mut found, &LoadWindow::all(None))
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
                if checked.decoded_is_whole
                    && self.cursor.decoder.decoded_catalog.windows == checked.windows
                {
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
            assert!(held.take(&minute.found.run, second, TickApps::Awaiting(&mut apps)));
        }
        if !seconds.is_empty() {
            self.selected = Some(self.cursor.next_minute_index);
        }
        self.cursor.decoder.accept_machine(
            minute.found.run,
            &machine,
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
        let sealed = witness.open(input, &minute.found, self.capture)?;
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
            &mut payload::replay::Predecessor,
        )?;
        sealed.revalidate_full()?;
        Ok(self.cursor.decoder.predecessor.take())
    }

    fn selected(&mut self, input: &Input<'_>, held: &mut Held) -> Result<(), Failure> {
        let _purpose =
            persistence::work::ingestion::purpose(persistence::work::ingestion::Purpose::Selection);
        if let Some(index) = self.selected {
            self.process(input, held, index)?;
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
        held.resumes_from_newest = true;
        held.startup = crate::store::StartupState::Primary;
        self.phase = Phase::HistoryBoundary;
        Ok(())
    }

    fn history(&mut self, input: &Input<'_>, held: &mut Held) -> Result<(), Failure> {
        let end = self.selected.unwrap_or(0);
        if self.history_index < end {
            let count = (end - self.history_index).min(self.history_group_minutes_max);
            if count == 1 {
                self.process(input, held, self.history_index)?;
                self.history_index += 1;
                return Ok(());
            }

            // A group the budget refuses goes on at once, a minute a step, so it never waits on
            // memory a single minute would not need. So do the steps after a group whose decoder
            // thread's work the budget refused.
            return match self.history_group(input, held, count) {
                Ok(GroupEnd::Decoded) => Ok(()),
                Ok(GroupEnd::Refused) | Err(Failure::Blocked) => {
                    self.history_group_minutes_max = 1;
                    Ok(())
                }
                Err(failure) => Err(failure),
            };
        }

        self.revalidate(input)?;
        input.operation.check()?;
        held.startup = crate::store::StartupState::Complete;
        // A second whose Apps the load left out waits for none now, so its block packs.
        held.apps.end_awaiting();
        held.owed = self.history_owed();
        self.cursor.decoder.predecessor = None;
        self.phase = Phase::Complete;
        Ok(())
    }

    /// Installs the next `count` minutes of the history, read at once a thread a minute. The
    /// first goes on from the launch's decoder on this thread while each later one is decoded on
    /// a thread of its own from a decoder seeded with only the last frame of the minute before
    /// it. Each later minute installs what its thread decoded once the launch's decoder, having
    /// installed the minute before it, holds the seed that decoding began from, and is decoded
    /// again here otherwise, as a step of one minute decodes it. It says whether the budget
    /// refused what a decoder thread held.
    fn history_group(
        &mut self,
        input: &Input<'_>,
        held: &mut Held,
        count: usize,
    ) -> Result<GroupEnd, Failure> {
        assert!(count > 1);
        assert!(count <= HISTORY_GROUP_MINUTES_BOUND);
        let first = self.history_index;
        let reads = history_group_reads(input, self.capture, &self.minutes[first..first + count]);
        assert_eq!(reads.len(), count);
        // A step takes in no native tick, so the span's seconds stay where they are until it
        // ends.
        let recent: Vec<Option<Observation>> = held
            .apps
            .span()
            .seconds()
            .map(crate::store::block::Second::observation)
            .collect();

        let guessing = self.guessing;
        let settled = std::thread::scope(|scope| {
            let guesses: Vec<_> = (1..count)
                .map(|offset| {
                    let (before, read, recent) = (&reads[offset - 1], &reads[offset], &recent);
                    super::spawn_observed(scope, "flightdeck-decoder", move || {
                        guessing(input, before, read, recent)
                    })
                })
                .collect();
            self.history_group_settled(input, held, &reads, guesses)
        });
        match settled {
            Ok(end) => Ok(end),
            Err(GroupStop::Failure(failure)) => Err(failure),
            Err(GroupStop::Read(offset)) => Err(reads
                .into_iter()
                .nth(offset)
                .and_then(Result::err)
                .unwrap_or_else(|| unreachable!("a refused read"))),
        }
    }

    /// Installs the minutes `reads` read, in order, the first decoded here and each later one
    /// from what its thread in `guesses` decoded, up to the first that fails, and says whether
    /// the budget refused a thread's work.
    fn history_group_settled(
        &mut self,
        input: &Input<'_>,
        held: &mut Held,
        reads: &[Result<HistoryRead<'_>, Failure>],
        guesses: Vec<std::io::Result<std::thread::ScopedJoinHandle<'_, Option<HistoryGuess>>>>,
    ) -> Result<GroupEnd, GroupStop> {
        assert_eq!(guesses.len() + 1, reads.len());
        let Ok(read) = &reads[0] else {
            return Err(GroupStop::Read(0));
        };
        let minute = &mut self.minutes[self.history_index];
        process_minute_replay(minute, &mut self.cursor.decoder, input, held, read)
            .map_err(GroupStop::Failure)?;
        self.history_index += 1;

        let mut end = GroupEnd::Decoded;
        for (offset, guess) in (1..).zip(guesses) {
            let guess = match guess {
                Ok(handle) => super::join_or_resume(handle),
                // A thread that did not start decodes nothing, so its minute is decoded here.
                Err(_) => None,
            };
            let Ok(read) = &reads[offset] else {
                return Err(GroupStop::Read(offset));
            };
            let minute_end = self
                .history_group_settled_minute(input, held, guess, read)
                .map_err(GroupStop::Failure)?;
            if minute_end == GroupEnd::Refused {
                end = GroupEnd::Refused;
            }
            self.history_index += 1;
        }
        Ok(end)
    }

    /// Installs the next minute of the history, which `read` read: what `guess` decoded where
    /// the launch's decoder holds the seed it began from, and otherwise what decoding `read` here
    /// gives. It says whether the budget refused the thread's work.
    fn history_group_settled_minute(
        &mut self,
        input: &Input<'_>,
        held: &mut Held,
        guess: Option<HistoryGuess>,
        read: &HistoryRead<'_>,
    ) -> Result<GroupEnd, Failure> {
        let minute = &mut self.minutes[self.history_index];
        // A group starts only while no minute of it was begun, since a block ends grouping.
        assert_eq!(minute.next_endpoint, 0);
        let decoder = &mut self.cursor.decoder;
        if let Some((tentative, guessed)) = guess {
            match guessed {
                Ok(guessed) if guessed.seed == decoder.seed_key() => {
                    tentative.keep();
                    held.apps.launch_wait();
                    for mut candidate in guessed.candidates {
                        input.operation.check()?;
                        history_group_settled_minute_share(&mut candidate, held);
                        install(&mut candidate, held);
                    }
                    decoder.predecessor = guessed.predecessor;
                    minute.next_endpoint = guessed.endpoint_count;
                    // The thread checked the seal after decoding, and the install reads no file,
                    // so a seal that changes later fails the launch where it checks every minute
                    // before it completes.
                    input.operation.check()?;
                    return Ok(GroupEnd::Decoded);
                }
                // The budget refused what the thread held, so the minute is decoded here. The
                // later minutes of the group still install what their threads decoded, and the
                // steps after the group go one minute at a time.
                Err(Failure::Blocked) => {
                    tentative.discard();
                    process_minute_replay(minute, decoder, input, held, read)?;
                    return Ok(GroupEnd::Refused);
                }
                Ok(_) | Err(_) => tentative.discard(),
            }
        }

        process_minute_replay(minute, decoder, input, held, read)?;
        Ok(GroupEnd::Decoded)
    }

    /// The Apps the store owes its stretches, since a load installs a second's Apps only where
    /// the store holds that second alone. A boundary minute lies outside the loaded hour.
    fn history_owed(&mut self) -> Option<Owed> {
        let found: Vec<Found> = self
            .minutes
            .drain(..)
            .filter(|minute| minute.boundary_path.is_none())
            .map(|minute| minute.found)
            .collect();

        (!found.is_empty()).then(|| Owed::all(found))
    }

    /// Starts the history at the oldest minute that holds a second of the live span. The pass
    /// that adds the Apps the launch owes ranks the seconds of the minutes before it, from the
    /// Apps those minutes sealed, so the launch reads none of their processes.
    fn history_boundary(&mut self, input: &Input<'_>, held: &Held) -> Result<(), Failure> {
        let end = self.selected.unwrap_or(0);
        let first = self.minutes[..end].iter().position(|minute| {
            minute.witness.as_ref().is_some_and(|witness| {
                witness
                    .endpoints
                    .iter()
                    .any(|endpoint| recent_index(held, &endpoint.observation()).is_some())
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
                .is_some_and(|endpoint| recent_index(held, &endpoint.observation()).is_some())
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
                witness.revalidate(input, &minute.found, self.capture)?;
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
        capture: &'a InputCapture<'a>,
    ) -> Result<ScopedSealedMinute<'a>, Failure> {
        Ok(ScopedSealedMinute::open_processes_using(
            &SealedSource {
                root: input.root,
                marker: &found.marker,
            },
            &self.manifest,
            input.budget,
            input.operation,
            capture,
        )?)
    }

    fn revalidate(
        &self,
        input: &Input<'_>,
        found: &Found,
        capture: &InputCapture<'_>,
    ) -> Result<(), Failure> {
        assert!(self.reservation.bytes() >= self.endpoints.capacity() * size_of::<Endpoint>());
        let sealed = self.open(input, found, capture)?;
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

fn process_minute(
    minute: &mut MinuteWork,
    decoder: &mut payload::Decoder,
    input: &Input<'_>,
    capture: &InputCapture<'_>,
    held: &mut Held,
) -> Result<(), Failure> {
    let read = process_minute_read(minute, input, capture)?;
    process_minute_replay(minute, decoder, input, held, &read)
}

/// A minute of the history as the launch read it through its witness.
struct HistoryRead<'input> {
    sealed: ScopedSealedMinute<'input>,
    /// Every process frame the minute holds, or `None` where the launch installed every one of
    /// them. The replay skips the frames below the minute's next endpoint, and a thread seeds
    /// the decoder of the minute after this one from the last of them.
    batch: Option<ProcessBatch>,
}

/// Reads `minute` through its witness, its process frames too unless the launch installed every
/// one of them.
fn process_minute_read<'a>(
    minute: &MinuteWork,
    input: &'a Input<'a>,
    capture: &'a InputCapture<'a>,
) -> Result<HistoryRead<'a>, Failure> {
    let witness = minute
        .witness
        .as_ref()
        .unwrap_or_else(|| unreachable!("machine witness"));
    let sealed = witness.open(input, &minute.found, capture)?;
    if minute.next_endpoint == witness.endpoints.len() {
        return Ok(HistoryRead {
            sealed,
            batch: None,
        });
    }

    let batch = crate::recording::processes::read_processes_batch_with_endpoints(
        &sealed,
        witness.endpoints.clone(),
    )?;
    Ok(HistoryRead {
        sealed,
        batch: Some(batch),
    })
}

/// Decodes the frames `read` holds of `minute` with `decoder` and installs each second's Apps.
fn process_minute_replay(
    minute: &mut MinuteWork,
    decoder: &mut payload::Decoder,
    input: &Input<'_>,
    held: &mut Held,
    read: &HistoryRead<'_>,
) -> Result<(), Failure> {
    held.apps.launch_wait();
    if let Some(batch) = &read.batch {
        decoder.replay(
            batch,
            &mut payload::replay::Input {
                next: &mut minute.next_endpoint,
                operation: input.operation,
                budget: Some(input.budget),
            },
            &mut InstallTarget {
                names: None,
                held,
                input,
            },
        )?;
    }
    read.sealed.revalidate_full()?;
    input.operation.check()?;
    Ok(())
}

/// Why a group of the history stopped: a minute whose read was refused, at its offset in the
/// group, or a failure decoding or installing one.
enum GroupStop {
    Read(usize),
    Failure(Failure),
}

/// How a group of the history ended once it installed every minute: each minute decoded where
/// the launch meant, or one minute decoded on the store's thread because the budget refused
/// what a decoder thread held, after which the history goes on a minute a step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum GroupEnd {
    Decoded,
    Refused,
}

/// How a thread decodes the minute the second read read, from the minute the first read read,
/// capturing the Apps of each second of the live span the observations name.
type HistoryGuessing = fn(
    &Input<'_>,
    &Result<HistoryRead<'_>, Failure>,
    &Result<HistoryRead<'_>, Failure>,
    &[Option<Observation>],
) -> Option<HistoryGuess>;

/// What a thread decoding a minute of a group gives, and the work it recorded.
type HistoryGuess = (
    persistence::work::ingestion::Tentative,
    Result<HistoryGuessed, Failure>,
);

/// What a thread decoded of a minute of a group, from a decoder seeded with only the last frame
/// of the minute before it.
struct HistoryGuessed {
    /// The seed the decoding began from.
    seed: payload::SeedKey,
    /// What the decoder kept of the minute's last frame, which rates the minute after it.
    predecessor: Option<Before>,
    candidates: Vec<Candidate>,
    /// How many endpoints the minute holds, each of which the decoding replayed.
    endpoint_count: usize,
}

/// Reads `minutes` through their witnesses, a thread a minute, in order.
fn history_group_reads<'a>(
    input: &'a Input<'a>,
    capture: &'a InputCapture<'a>,
    minutes: &[MinuteWork],
) -> Vec<Result<HistoryRead<'a>, Failure>> {
    std::thread::scope(|scope| {
        let readers: Vec<_> = minutes
            .iter()
            .map(|minute| {
                super::spawn_observed(scope, "flightdeck-reader", move || {
                    process_minute_read(minute, input, capture)
                })
            })
            .collect();

        // A minute whose thread did not start is read here.
        minutes
            .iter()
            .zip(readers)
            .map(|(minute, reader)| match reader {
                Ok(handle) => super::join_or_resume(handle),
                Err(_) => process_minute_read(minute, input, capture),
            })
            .collect()
    })
}

/// Decodes the minute `read` read from a decoder seeded with only the last frame of `before`,
/// the minute before it, capturing the Apps of each second of the live span `recent` names. It
/// gives `None` where it could not seed a decoder from the minute before.
fn history_group_guess(
    input: &Input<'_>,
    before: &Result<HistoryRead<'_>, Failure>,
    read: &Result<HistoryRead<'_>, Failure>,
    recent: &[Option<Observation>],
) -> Option<HistoryGuess> {
    let (Ok(before), Ok(read)) = (before, read) else {
        return None;
    };
    let (Some(before), Some(batch)) = (&before.batch, &read.batch) else {
        return None;
    };
    let (scope, tentative) = persistence::work::ingestion::tentative();
    let guessed = history_group_guess_decode(input, before, batch, recent);
    let guessed = guessed.and_then(|guessed| {
        read.sealed.revalidate_full()?;
        input.operation.check()?;
        Ok(guessed)
    });
    drop(scope);
    Some((tentative, guessed))
}

/// Seeds a decoder from the last frame of `before` and decodes `batch` with it.
fn history_group_guess_decode(
    input: &Input<'_>,
    before: &ProcessBatch,
    batch: &ProcessBatch,
    recent: &[Option<Observation>],
) -> Result<HistoryGuessed, Failure> {
    assert!(!before.endpoints.is_empty());
    let mut decoder = payload::Decoder::default();
    {
        let _purpose = persistence::work::ingestion::purpose(
            persistence::work::ingestion::Purpose::Predecessor,
        );
        let mut next = before.endpoints.len() - 1;
        decoder.replay(
            before,
            &mut payload::replay::Input {
                next: &mut next,
                operation: input.operation,
                budget: Some(input.budget),
            },
            &mut payload::replay::Predecessor,
        )?;
    }
    let seed = decoder.seed_key();

    let mut target = CaptureTarget {
        recent,
        input,
        names: None,
        candidates: Vec::with_capacity(crate::live_apps::committed::ENDPOINTS_MAX),
    };
    let mut next = 0;
    decoder.replay(
        batch,
        &mut payload::replay::Input {
            next: &mut next,
            operation: input.operation,
            budget: Some(input.budget),
        },
        &mut target,
    )?;
    assert_eq!(next, batch.endpoints.len());
    Ok(HistoryGuessed {
        seed,
        predecessor: decoder.predecessor.take(),
        candidates: target.candidates,
        endpoint_count: next,
    })
}

/// Shares with the second the store holds before it each name, process row and App's additional
/// measures of `candidate` that read the same there. A thread captured the minute's first second
/// with no second before it and each later second against the thread's own, so every second of
/// the minute takes what the store holds, which leaves one allocation of each reading, as a step
/// of one minute leaves it. The pass costs a walk of each second's rows and Apps.
fn history_group_settled_minute_share(candidate: &mut Candidate, held: &Held) {
    let Some(before) = candidate.recent.and_then(|index| index.checked_sub(1)) else {
        return;
    };
    if let Some(previous) = held.apps.stored(before) {
        candidate.apps.share_with(previous);
    }
}

/// Captures the Apps of each second a thread decodes, for the second of the live span `recent`
/// names, touching nothing the store holds.
struct CaptureTarget<'step, 'input> {
    recent: &'step [Option<Observation>],
    input: &'step Input<'input>,
    names: Option<crate::store::payload::ProcessNames>,
    candidates: Vec<Candidate>,
}

impl CaptureTarget<'_, '_> {
    fn recent_index(&self, observation: &Observation) -> Option<usize> {
        self.recent
            .iter()
            .position(|recent| *recent == Some(*observation))
    }
}

impl ReplayTarget for CaptureTarget<'_, '_> {
    type Error = Failure;

    fn derives(&self, endpoint: &Endpoint) -> bool {
        self.recent_index(&endpoint.observation()).is_some()
    }

    fn consume(&mut self, captured: Captured<'_>) -> Result<Option<Before>, Failure> {
        assert!(self.candidates.len() < crate::live_apps::committed::ENDPOINTS_MAX);
        let recent = captured.sample.as_ref().map(|_| {
            self.recent_index(&captured.endpoint.observation())
                .unwrap_or_else(|| unreachable!("a second outside the span derives no Apps"))
        });
        // The first second of a minute a thread decodes shares nothing here, since the second
        // before it is another thread's, and shares with it once the store holds it.
        let previous = self
            .candidates
            .last()
            .filter(|last| last.recent.map(|index| index + 1) == recent)
            .map(|last| &last.apps);
        let mut candidate =
            capture_candidate(captured, self.input, recent, previous, &mut self.names)?;
        self.input.operation.check()?;
        let before = candidate.before.take();
        self.candidates.push(candidate);
        Ok(before)
    }
}

/// Where the second `observation` names stands among the seconds of the live span. The pass that
/// adds the Apps the launch owes ranks a second older than the span.
fn recent_index(held: &Held, observation: &Observation) -> Option<usize> {
    held.apps
        .span()
        .seconds()
        .position(|apps| apps.observation() == Some(*observation))
}

struct InstallTarget<'step, 'input> {
    names: Option<crate::store::payload::ProcessNames>,
    held: &'step mut Held,
    input: &'step Input<'input>,
}

impl ReplayTarget for InstallTarget<'_, '_> {
    type Error = Failure;

    fn derives(&self, endpoint: &Endpoint) -> bool {
        recent_index(self.held, &endpoint.observation()).is_some()
    }

    fn consume(&mut self, captured: Captured<'_>) -> Result<Option<Before>, Failure> {
        let held = &*self.held;
        let recent = captured.sample.as_ref().map(|_| {
            recent_index(held, &captured.endpoint.observation())
                .unwrap_or_else(|| unreachable!("a second outside the span derives no Apps"))
        });
        let previous = recent
            .and_then(|index| index.checked_sub(1))
            .and_then(|index| held.apps.stored(index));
        let mut candidate =
            capture_candidate(captured, self.input, recent, previous, &mut self.names)?;
        self.input.operation.check()?;
        install(&mut candidate, self.held);
        Ok(candidate.before.take())
    }
}

/// The Apps of the second `captured` holds, for the second at `recent` of the live span where the
/// launch derives its Apps, whose rows share what `previous`, the second before it, holds.
fn capture_candidate(
    captured: Captured<'_>,
    input: &Input<'_>,
    recent: Option<usize>,
    previous: Option<&AppsSecond>,
    names: &mut Option<crate::store::payload::ProcessNames>,
) -> Result<Candidate, Failure> {
    assert_eq!(captured.sample.is_some(), recent.is_some());
    let observation = captured.endpoint.observation();
    let mut apps = AppsSecond::default();
    if let Some(sample) = captured.sample {
        let sample = match sample {
            Err(crate::live_apps::AggregateError::Presentation(
                error @ (Error::Io(_) | Error::Engine(_)),
            )) => return Err(error.into()),
            sample => sample.map_err(crate::live_apps::AggregateError::legacy),
        };
        payload::load_minute_rows(&sample, previous, input.budget, &mut apps);
        payload::load_minute_payload(
            &sample,
            previous,
            input.budget,
            captured.names_capacity,
            names,
            &mut apps,
        );
        if apps.missing == Some(Missing::Budget) {
            return Err(Failure::Blocked);
        }

        if matches!(apps.processes, Err(Missing::Budget)) {
            return Err(Failure::Blocked);
        }
    }
    apps.observation = Some(observation);
    Ok(Candidate {
        recent,
        apps,
        before: captured.before,
    })
}

/// Installs `candidate` at the second of the live span it was captured for, in a new block that
/// takes a buffer's slot where every buffer is held.
fn install(candidate: &mut Candidate, held: &mut Held) {
    held.release_views();
    match candidate.recent {
        Some(index) => {
            let previous = held.apps.span().second(index);
            // A thread captured the candidate against the span as the step began.
            assert_eq!(previous.observation(), candidate.apps.observation);
            candidate.apps.consecutive = previous.consecutive();
            candidate.apps.metadata.origin = previous.origin();
            held.apps
                .replace(index, |apps| mem::swap(apps, &mut candidate.apps));
        }
        None => assert_eq!(candidate.apps.missing, Some(Missing::Unread)),
    }
}
