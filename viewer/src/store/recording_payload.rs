//! The Store's complete process decoder, sharing the recorded App rule and predecessor.
use std::mem;
use std::path::Path;
use std::sync::Arc;
use std::thread;

use collector::ring::RunIdentity;

use super::readings;
use super::{AppsSecond, Batch, Failure, Found, Loaded, MachineSecond, Refused, TICKS_PER_MINUTE};

#[path = "recording_replay.rs"]
pub(super) mod replay;
use crate::byte_budget::{ByteBudget, ByteReservation};
use crate::cancellation::Operation;
use crate::recording::{Before, MachineMinute, ScopedSealedMinute, SealedSource, read_machine};
use crate::store::apps::TickApps;
use persistence::engine::AdmissionRefusal;

#[cfg(test)]
#[path = "recording_payload_tests.rs"]
mod tests;

/// The neighbouring minutes a load that continues the store's seconds decodes at once, a thread
/// each, beside a thread that seeds the first from the predecessor. A load of the whole hour
/// decodes one minute at a time.
const GROUP_MINUTES_MAX: usize = 6;

/// Hands `take` the seconds of `minutes`, oldest first, after seeding their rates from
/// `predecessor`, the minute before them whose seconds the store holds, where the load
/// continues the store's seconds. A load that continues them, which a returning window waits on,
/// decodes up to `GROUP_MINUTES_MAX` neighbouring minutes at once, each seeding its rates from the
/// minute before it. A load of the whole hour decodes one minute at a time, so it needs the memory
/// of one minute however many it reads.
pub(super) fn load(
    input: &LoadInput<'_>,
    predecessor: Option<&Found>,
    minutes: &[Found],
    take: impl FnMut(&RunIdentity, &MachineSecond, TickApps<'_>),
) -> persistence::Result<super::Outcome> {
    start(input, predecessor, minutes)?.finish(input, take)
}

/// A load whose first minutes are decoded and wait to be handed over.
pub(super) struct Started<'minutes> {
    cursor: Cursor,
    first: Vec<persistence::Result<DecodedMinute>>,
    rest: std::slice::Chunks<'minutes, Found>,
}

/// Starts the load `load` makes of `minutes`, decoding the first of them and handing none over.
pub(super) fn start<'minutes>(
    input: &LoadInput<'_>,
    predecessor: Option<&Found>,
    minutes: &'minutes [Found],
) -> persistence::Result<Started<'minutes>> {
    let mut cursor = Cursor::new(minutes.len());
    let group_minutes = match predecessor {
        Some(_) => GROUP_MINUTES_MAX,
        None => 1,
    };
    let mut rest = minutes.chunks(group_minutes);
    let first = match rest.next() {
        Some(group) => cursor.decode_group(input, predecessor, group)?,
        None => Vec::new(),
    };

    Ok(Started {
        cursor,
        first,
        rest,
    })
}

impl Started<'_> {
    /// Hands `take` the seconds of the minutes decoded, then decodes the others a group at a time
    /// and hands over theirs.
    pub(super) fn finish(
        mut self,
        input: &LoadInput<'_>,
        mut take: impl FnMut(&RunIdentity, &MachineSecond, TickApps<'_>),
    ) -> persistence::Result<super::Outcome> {
        self.cursor
            .hand_over(self.first, input.operation, &mut take)?;
        for group in self.rest {
            self.cursor.step(input, group, &mut take)?;
        }

        self.cursor.finish(input.operation)
    }
}

/// Decodes `group`, neighbouring minutes oldest first, a thread a minute, or on this thread where
/// it holds one. The first goes on from `carried`, the decoder of the minute before it, or where
/// the load continues from `predecessor`, seeds its own decoder from it. Each minute is read once,
/// so a decoder seeds from the read that the decoder of the minute before it decodes. Each result
/// is a minute's decoder, its seconds and why its decoder could not be seeded, in the order of
/// `group`, up to the first error, which ends the load.
///
/// Where there are several, a thread decoding a minute that goes on from no decoder seeds from
/// only the last frame of the minute before it, which rates its first second. Its decoding counts
/// once the decoder that minute ended with, or for the first minute a seed of all the
/// predecessor's frames, since an earlier frame can refuse the minute, holds the same seed. A
/// decoding that began from another seed is done again from that decoder, as a load of one minute
/// at a time decodes it.
fn load_group(
    input: &LoadInput<'_>,
    carried: Decoder,
    predecessor: Option<&Found>,
    group: &[Found],
) -> Vec<persistence::Result<DecodedMinute>> {
    assert!(!group.is_empty());
    assert!(group.len() <= GROUP_MINUTES_MAX);
    let mut carried = match predecessor {
        Some(_) => None,
        None => Some(carried),
    };
    let reads = load_group_reads(input, predecessor, group);
    // The predecessor's read, where there is one, comes before the reads of `group`.
    let offset = usize::from(predecessor.is_some());
    assert_eq!(reads.len(), group.len() + offset);
    let before_of = |index: usize| match (index.checked_sub(1), predecessor) {
        (Some(before), _) => Some((&group[before], &reads[before + offset])),
        (None, Some(predecessor)) => Some((predecessor, &reads[0])),
        (None, None) => None,
    };
    if let [found] = group {
        let seeded = load_group_seeded(input, carried, before_of(0), 0);
        return vec![seeded.and_then(|(decoder, unseeded)| {
            load_group_decode(input, decoder, unseeded, found, &reads[offset])
        })];
    }

    thread::scope(|scope| {
        let seeder = predecessor.map(|predecessor| {
            super::spawn_observed(scope, "flightdeck-seeder", || {
                load_group_seed(input, predecessor, &reads[0], 0)
            })
        });
        let decoders: Vec<_> = group
            .iter()
            .enumerate()
            .map(|(index, found)| {
                let carried = carried.take();
                let before = before_of(index);
                let read = &reads[index + offset];
                super::spawn_observed(scope, "flightdeck-decoder", move || {
                    let (tentative_scope, tentative) = persistence::work::ingestion::tentative();
                    let guessed = load_group_guess(input, carried, before, found, read);
                    drop(tentative_scope);
                    (tentative, guessed)
                })
            })
            .collect();

        let whole = seeder.map(|seeder| seeder.map(super::join_or_resume));
        load_group_settled_minutes(input, group, &reads[offset..], whole, decoders)
    })
}

/// What the thread seeding a minute from all the frames of the predecessor gives.
type Seed = persistence::Result<(Decoder, Option<Failure>)>;

/// The decodings `decoders` give of `group`, which `reads` read, each settled in order against the
/// decoder the minute before it ended with, or the first against `whole`, the seed of all the
/// predecessor's frames, where the load continues from one, up to the first error.
fn load_group_settled_minutes(
    input: &LoadInput<'_>,
    group: &[Found],
    reads: &[Result<MinuteRead<'_>, Failure>],
    mut whole: Option<std::io::Result<Seed>>,
    decoders: Vec<std::io::Result<thread::ScopedJoinHandle<'_, Guess>>>,
) -> Vec<persistence::Result<DecodedMinute>> {
    assert_eq!(decoders.len(), group.len());
    assert_eq!(reads.len(), group.len());
    let mut settled: Vec<persistence::Result<DecodedMinute>> = Vec::with_capacity(group.len());
    for ((found, read), decoder) in group.iter().zip(reads).zip(decoders) {
        let guessed = match decoder {
            Ok(handle) => super::join_or_resume(handle),
            Err(error) => {
                settled.push(Ok(DecodedMinute::unstarted(&error)));
                continue;
            }
        };
        let decoded = match (settled.last_mut(), whole.take()) {
            (Some(Ok(before)), _) => {
                load_group_settled(input, &mut before.decoder, None, guessed, found, read)
            }
            (Some(Err(_)), _) => unreachable!("an error ends the load"),
            // The first minute went on from the decoder of the minute before it.
            (None, None) => {
                let (tentative, guessed) = guessed;
                tentative.keep();
                guessed.map(|(_, decoded)| decoded)
            }
            (None, Some(Ok(Ok((mut seeded, unseeded))))) => {
                load_group_settled(input, &mut seeded, unseeded, guessed, found, read)
            }
            (None, Some(Ok(Err(error)))) => Err(error),
            (None, Some(Err(error))) => Ok(DecodedMinute::unstarted(&error)),
        };
        let ends = decoded.is_err();
        settled.push(decoded);
        if ends {
            break;
        }
    }

    settled
}

/// What the thread decoding a minute from a guessed seed gives, and the work it recorded.
type Guess = (
    persistence::work::ingestion::Tentative,
    persistence::Result<(SeedKey, DecodedMinute)>,
);

/// Decodes `found`, which `read` read, with `carried`, the decoder of the minute before it, or
/// with a decoder seeded from only the last frame of `before`, that minute and its read, and
/// gives the seed the decoding began from.
fn load_group_guess(
    input: &LoadInput<'_>,
    carried: Option<Decoder>,
    before: Option<(&Found, &Result<MinuteRead<'_>, Failure>)>,
    found: &Found,
    read: &Result<MinuteRead<'_>, Failure>,
) -> persistence::Result<(SeedKey, DecodedMinute)> {
    let last_frame_index = before.map_or(0, |(_, before_read)| {
        load_group_last_frame_index(before_read)
    });
    let (decoder, unseeded) = load_group_seeded(input, carried, before, last_frame_index)?;
    let seed = decoder.seed_key();

    load_group_decode(input, decoder, unseeded, found, read).map(|decoded| (seed, decoded))
}

/// `carried`, the decoder of the minute before the one it decodes, or a decoder seeded from the
/// frames of `before`, that minute and its read, from the one at `first_frame_index`, and why it
/// could not be seeded, where it could not.
fn load_group_seeded(
    input: &LoadInput<'_>,
    carried: Option<Decoder>,
    before: Option<(&Found, &Result<MinuteRead<'_>, Failure>)>,
    first_frame_index: usize,
) -> persistence::Result<(Decoder, Option<Failure>)> {
    match (carried, before) {
        (Some(decoder), _) => Ok((decoder, None)),
        (None, Some((before, read))) => load_group_seed(input, before, read, first_frame_index),
        (None, None) => unreachable!("a minute with none before it in the load has a decoder"),
    }
}

/// The index of the last process frame `read` holds, the one that rates the minute after it.
fn load_group_last_frame_index(read: &Result<MinuteRead<'_>, Failure>) -> usize {
    read.as_ref()
        .map_or(0, |read| read.processes.endpoints.len().saturating_sub(1))
}

/// The decoding of `found`, which `read` read: `guessed`, which began from a seed of only the last
/// frame of the minute before it, where `before`, the decoder that minute ended with, holds the
/// same seed, and otherwise a decoding from `before`, which it takes, with `unseeded`, why
/// `before` could not be seeded, where it could not. The work of a decoding thrown away counts as
/// discarded.
fn load_group_settled(
    input: &LoadInput<'_>,
    before: &mut Decoder,
    unseeded: Option<Failure>,
    guessed: Guess,
    found: &Found,
    read: &Result<MinuteRead<'_>, Failure>,
) -> persistence::Result<DecodedMinute> {
    let (tentative, guessed) = guessed;
    let (seed, guess) = guessed?;
    if seed == before.seed_key() {
        tentative.keep();
        return Ok(DecodedMinute { unseeded, ..guess });
    }
    tentative.discard();

    load_group_decode(input, mem::take(before), unseeded, found, read)
}

/// Reads `predecessor`, where the load continues from it, and then the minutes of `group`, a
/// thread a minute where there are several. A decoder seeds from each of them but the last of
/// `group`, so each of those is checked once read, as a decoder checks its own once it decoded it.
fn load_group_reads<'input>(
    input: &LoadInput<'input>,
    predecessor: Option<&Found>,
    group: &[Found],
) -> Vec<Result<MinuteRead<'input>, Failure>> {
    let minutes: Vec<&Found> = predecessor.into_iter().chain(group).collect();
    assert!(minutes.len() <= GROUP_MINUTES_MAX + 1);
    let predecessors = minutes.len() - group.len();
    let last_index = minutes.len() - 1;
    let read = |index: usize, found: &Found| {
        let _purpose = (index < predecessors).then(|| {
            persistence::work::ingestion::purpose(
                persistence::work::ingestion::Purpose::Predecessor,
            )
        });
        let mut read = read_minute(
            input.root,
            found,
            input.budget,
            input.operation,
            input.capture,
        );
        if index < last_index {
            read = read.and_then(|read| read.check(input.operation).map(|()| read));
        }
        read.map_err(|refused| load_refusal(&refused))
    };
    if let [found] = minutes.as_slice() {
        return vec![read(0, found)];
    }

    thread::scope(|scope| {
        let readers: Vec<_> = minutes
            .iter()
            .enumerate()
            .map(|(index, found)| {
                let read = &read;
                super::spawn_observed(scope, "flightdeck-reader", move || read(index, found))
            })
            .collect();

        readers
            .into_iter()
            .map(|reader| match reader {
                Ok(handle) => super::join_or_resume(handle),
                Err(error) => Err(Failure::Loader(error.kind())),
            })
            .collect()
    })
}

/// What decoding one minute of a group gives.
struct DecodedMinute {
    /// The decoder after the minute, which rates the minute after it.
    decoder: Decoder,
    completion: Batch,
    /// Why the decoder could not be seeded from the minute before it, where it could not. That
    /// minute's first second then has no rates.
    unseeded: Option<Failure>,
}

impl DecodedMinute {
    /// What a minute gives whose thread `error` kept from starting: none of its seconds.
    fn unstarted(error: &std::io::Error) -> Self {
        Self {
            decoder: Decoder::default(),
            completion: Batch {
                seconds: Vec::new(),
                left_out: Some(Failure::Loader(error.kind())),
            },
            unseeded: None,
        }
    }
}

/// Decodes `found`, which `read` read, with `decoder`, the decoder of the minute before it or one
/// seeded from that minute, which `unseeded` says why it could not be seeded from, where it could
/// not.
fn load_group_decode(
    input: &LoadInput<'_>,
    mut decoder: Decoder,
    unseeded: Option<Failure>,
    found: &Found,
    read: &Result<MinuteRead<'_>, Failure>,
) -> persistence::Result<DecodedMinute> {
    input.operation.check()?;

    let completion = decoder.decode_read(found, read, input.budget, input.operation);
    Ok(DecodedMinute {
        decoder,
        completion,
        unseeded,
    })
}

/// A decoder holding what rates the minute after `before`, which `read` read, as the decoder of
/// `before` holds after decoding it, without the catalog of `before`, which that decoder counts,
/// and why it could not take in `before`, where it could not. It replays the frames of `before`
/// from the one at `first_frame_index`, so a seed from a later frame than the first is a guess.
fn load_group_seed(
    input: &LoadInput<'_>,
    before: &Found,
    read: &Result<MinuteRead<'_>, Failure>,
    first_frame_index: usize,
) -> persistence::Result<(Decoder, Option<Failure>)> {
    let _purpose =
        persistence::work::ingestion::purpose(persistence::work::ingestion::Purpose::Predecessor);
    input.operation.check()?;
    let mut decoder = Decoder::default();

    let seeded = decoder.seed(before, read, first_frame_index, input.operation);
    if seeded.is_err() {
        assert!(decoder.predecessor.is_none());
        assert!(decoder.previous_run.is_none());
    }
    input.operation.check()?;

    decoder.decoded_catalog = super::DecodedCatalog::default();
    Ok((decoder, seeded.err()))
}

pub(super) struct Cursor {
    minute_count: usize,
    pub(super) next_minute_index: usize,
    pub(super) decoder: Decoder,
    left_out: Option<Failure>,
}

/// What a load needs to read the recording.
pub(super) struct LoadInput<'a> {
    pub(super) root: &'a Path,
    pub(super) budget: &'a Arc<ByteBudget>,
    pub(super) operation: &'a Operation,
    pub(super) capture: &'a crate::recording::InputCapture<'a>,
}

impl Cursor {
    pub(super) fn new(minute_count: usize) -> Self {
        assert!(minute_count <= super::MINUTES_MAX);

        Self {
            minute_count,
            next_minute_index: 0,
            decoder: Decoder::default(),
            left_out: None,
        }
    }

    /// Decodes `group`, the next neighbouring minutes oldest first, at once, going on from the
    /// minute before them, and hands `take` their seconds in order.
    fn step(
        &mut self,
        input: &LoadInput<'_>,
        group: &[Found],
        take: impl FnMut(&RunIdentity, &MachineSecond, TickApps<'_>),
    ) -> persistence::Result<()> {
        let decoded = self.decode_group(input, None, group)?;
        self.hand_over(decoded, input.operation, take)
    }

    /// Decodes `group`, the next neighbouring minutes oldest first, at once, and hands none of
    /// their seconds over. Where the load continues from `predecessor`, the minute before the first
    /// of them, whose seconds the store holds, it seeds their rates from it and leaves its seconds
    /// unread.
    fn decode_group(
        &mut self,
        input: &LoadInput<'_>,
        predecessor: Option<&Found>,
        group: &[Found],
    ) -> persistence::Result<Vec<persistence::Result<DecodedMinute>>> {
        input.operation.check()?;
        if self.minute_count - self.next_minute_index < group.len() {
            return Err(persistence::Error::Bound("sealed payload cursor exhausted"));
        }
        if predecessor.is_some() {
            assert_eq!(self.next_minute_index, 0);
        }

        Ok(load_group(
            input,
            mem::take(&mut self.decoder),
            predecessor,
            group,
        ))
    }

    /// Hands `take` the seconds of `decoded`, the minutes `decode_group` decoded, in order.
    fn hand_over(
        &mut self,
        decoded: Vec<persistence::Result<DecodedMinute>>,
        operation: &Operation,
        mut take: impl FnMut(&RunIdentity, &MachineSecond, TickApps<'_>),
    ) -> persistence::Result<()> {
        let mut decoded_catalog = super::DecodedCatalog::default();
        for (index, decoded) in decoded.into_iter().enumerate() {
            let mut decoded = decoded?;
            // Only the predecessor, which no decoder in this load decodes, leaves a minute
            // unseeded.
            if let Some(failure) = decoded.unseeded {
                assert_eq!(index, 0);
                self.left_out = Some(failure);
            }
            decoded_catalog.extend(&mem::take(&mut decoded.decoder.decoded_catalog));
            if let Some(failure) = load_consume(decoded.completion, operation, &mut take)? {
                self.left_out = Some(failure);
            }
            self.accept_minute();
            self.decoder = decoded.decoder;
        }
        self.decoder.decoded_catalog = decoded_catalog;

        Ok(())
    }

    pub(super) fn accept_minute(&mut self) {
        assert!(self.next_minute_index < self.minute_count);
        self.next_minute_index += 1;
    }

    pub(super) fn is_complete(&self) -> bool {
        self.next_minute_index == self.minute_count
    }

    fn finish(mut self, operation: &Operation) -> persistence::Result<super::Outcome> {
        operation.check()?;
        if self.next_minute_index < self.minute_count {
            return Err(persistence::Error::Bound(
                "sealed payload cursor unfinished",
            ));
        }

        assert_eq!(self.next_minute_index, self.minute_count);
        let decoded_catalog = mem::take(&mut self.decoder.decoded_catalog);

        Ok(super::Outcome {
            left_out: self.left_out,
            terminal: self.decoder.terminal(),
            checked_catalog: None,
            continued_catalog: None,
            decoded_catalog,
        })
    }
}

/// Owns the completion so displaced pool payloads swapped into it drop before the next decode.
fn load_consume(
    mut completion: Batch,
    operation: &Operation,
    mut take: impl FnMut(&RunIdentity, &MachineSecond, TickApps<'_>),
) -> persistence::Result<Option<Failure>> {
    operation.check()?;
    for loaded in &mut completion.seconds {
        operation.check()?;
        take(&loaded.run, &loaded.second, loaded.apps.tick());
    }
    operation.check()?;

    Ok(completion.left_out)
}

#[derive(Default)]
pub(super) struct Decoder {
    pub(super) predecessor: Option<Before>,
    previous: MachineMinute,
    previous_run: Option<RunIdentity>,
    pub(super) decoded_catalog: super::DecodedCatalog,
    machine_reservation: Option<ByteReservation>,
}

/// What a decoder seeded from one minute rates the first second of the next with: the run and
/// publication of the frame it keeps, where it keeps one, and the run of the machine readings it
/// keeps, where it keeps them. Two seeds of one read that agree on it hold the same.
#[derive(Clone, Copy, PartialEq, Eq)]
struct SeedKey {
    predecessor: Option<(RunIdentity, u64)>,
    previous_run: Option<RunIdentity>,
}

impl Decoder {
    fn seed_key(&self) -> SeedKey {
        SeedKey {
            predecessor: self.predecessor.as_ref().map(|before| {
                let (endpoint, _) = before.previous();
                (endpoint.identity, endpoint.stamp.publication)
            }),
            previous_run: self.previous_run,
        }
    }

    pub(super) fn terminal(&mut self) -> Option<super::Terminal> {
        let run = self.previous_run.take()?;
        let terminal = super::Terminal::from_minute(
            run,
            mem::take(&mut self.previous),
            self.predecessor.take(),
        );
        terminal.map(|mut terminal| {
            terminal.machine_reservation = self.machine_reservation.take();
            terminal
        })
    }

    pub(super) fn project_machine(
        &self,
        found: &Found,
        machine: &MachineMinute,
        budget: &Arc<ByteBudget>,
        take: impl FnMut(MachineSecond),
    ) -> Result<super::DecodedCatalog, crate::store::MachineCaptureError> {
        let before = if self.previous_run == Some(found.run) {
            readings(&self.previous).last()
        } else {
            None
        };
        project_machine(found, machine, before, budget, take)
    }

    /// Takes in `machine`, the machine readings of a minute of `run` that `decoded` describes,
    /// keeping its last tick, the one that rates the minute after it.
    pub(super) fn accept_machine(
        &mut self,
        run: RunIdentity,
        machine: &MachineMinute,
        decoded: &super::DecodedCatalog,
        reservation: Option<ByteReservation>,
    ) {
        self.decoded_catalog.extend(decoded);
        self.previous.keep_last_tick_of(machine);
        self.machine_reservation = reservation;
        self.previous_run = Some(run);
    }

    /// Decodes `found`, which `read` read.
    fn decode_read(
        &mut self,
        found: &Found,
        read: &Result<MinuteRead<'_>, Failure>,
        budget: &Arc<ByteBudget>,
        operation: &Operation,
    ) -> Batch {
        let mut batch = Batch {
            seconds: Vec::with_capacity(TICKS_PER_MINUTE),
            left_out: None,
        };
        let loaded = match read {
            Ok(read) => load_minute_apps(read, self, budget, operation)
                .map(|apps| (read, apps))
                .map_err(|refused| load_refusal(&refused)),
            Err(failure) => Err(*failure),
        };
        let (read, mut apps) = match loaded {
            Ok(loaded) => loaded,
            Err(failure) => {
                batch.left_out = Some(failure);
                self.predecessor = None;
                self.previous_run = None;
                return batch;
            }
        };
        let decoded = match self.project_machine(found, &read.machine, budget, |second| {
            project_machine_apps(found.run, second, &mut apps, &mut batch.seconds);
        }) {
            Ok(decoded) => decoded,
            Err(error) => {
                batch.seconds.clear();
                batch.left_out = Some(Failure::Refused);
                self.predecessor = None;
                self.previous_run = None;
                self.previous.clear();
                drop(error);
                return batch;
            }
        };
        self.accept_machine(found.run, &read.machine, &decoded, None);

        batch
    }

    /// Takes in of `found`, which `read` read, only its machine readings and the last of its
    /// process frames, which give the first second of the minute after it its rates, replaying
    /// its frames from the one at `first_frame_index`, and leaves its seconds unread.
    fn seed(
        &mut self,
        found: &Found,
        read: &Result<MinuteRead<'_>, Failure>,
        first_frame_index: usize,
        operation: &Operation,
    ) -> Result<(), Failure> {
        let visited = match read {
            Ok(read) => self
                .visit_predecessor(&read.processes, first_frame_index, operation)
                .map(|()| read)
                .map_err(|refused| load_refusal(&refused)),
            Err(failure) => Err(*failure),
        };
        match visited {
            Ok(read) => {
                let decoded = decoded_of(&read.machine);
                self.accept_machine(found.run, &read.machine, &decoded, None);
                Ok(())
            }
            Err(failure) => {
                self.predecessor = None;
                self.previous_run = None;
                Err(failure)
            }
        }
    }
}

/// What a load leaves out for a minute it could not read.
fn load_refusal(refused: &Refused) -> Failure {
    match refused {
        Refused::Input(AdmissionRefusal::Budget) => Failure::Budget,
        _ => Failure::Refused,
    }
}

/// The Apps of each second of `read`, which `decoder` replays.
fn load_minute_apps(
    read: &MinuteRead<'_>,
    decoder: &mut Decoder,
    budget: &Arc<ByteBudget>,
    operation: &Operation,
) -> Result<Vec<AppsSecond>, Refused> {
    let mut apps = Vec::with_capacity(crate::live_apps::committed::ENDPOINTS_MAX);
    let mut names = None;
    load_minute_visit_read(
        read,
        decoder,
        operation,
        |endpoint, sample, losses, count| {
            let mut second = AppsSecond::default();
            load_minute_rows(&sample, apps.last(), budget, &mut second);
            load_minute_payload(&sample, apps.last(), budget, count, &mut names, &mut second);
            second.observation = Some(crate::live_app_response::Observation {
                recording: endpoint.identity.recording.0,
                stamp: endpoint.stamp,
                run_tick: endpoint.run_tick,
                wall_ns: endpoint.wall_ns,
            });
            second.metadata.origin = super::super::native::Origin::Saved(*losses);
            apps.push(second);
            Ok(())
        },
    )?;

    Ok(apps)
}

pub(super) fn load_minute_rows(
    sample: &persistence::Result<&crate::live_app_response::Sample>,
    previous: Option<&AppsSecond>,
    budget: &Arc<ByteBudget>,
    second: &mut AppsSecond,
) {
    let reason = match sample {
        Ok(_) => None,
        Err(error) => Some(error.to_string()),
    };
    let metadata = super::super::native::AppMetadata::admitted(budget, reason.as_deref());
    let Ok(metadata) = metadata else {
        second.take(TickApps::Missing(&super::Missing::Budget), previous);
        return;
    };
    if let Ok(sample) = sample {
        let count = if sample.apps().len() <= crate::store::APPS_PER_SECOND_MAX {
            sample.apps().len()
        } else {
            0
        };
        if let Err(missing) = second.apps.replace_admitted(count, budget) {
            second.take(TickApps::Missing(&missing), previous);
            return;
        }
        second.fill(sample, previous, Some(budget));
    } else {
        second.take(
            TickApps::Refused(reason.as_deref().unwrap_or_else(|| unreachable!("refusal"))),
            previous,
        );
    }
    second.metadata = metadata;
}

pub(super) fn load_minute_payload(
    sample: &persistence::Result<&crate::live_app_response::Sample>,
    previous: Option<&AppsSecond>,
    budget: &Arc<ByteBudget>,
    count: usize,
    names: &mut Option<super::super::payload::ProcessNames>,
    second: &mut AppsSecond,
) {
    if second.missing.is_some() {
        return;
    }

    let Ok(sample) = sample else {
        return;
    };

    if names.is_none() {
        match super::super::payload::ProcessNames::new(count, budget) {
            Ok(captured) => *names = Some(captured),
            Err(missing) => {
                second.processes = Err(missing);
                return;
            }
        }
    }
    let names = names
        .as_mut()
        .unwrap_or_else(|| unreachable!("sealed name namespace"));
    if !Arc::ptr_eq(names.budget(), budget) {
        second.processes = Err(super::super::Missing::Budget);
        return;
    }
    let previous_payload = previous.and_then(|apps| apps.processes.as_deref().ok());
    second.processes =
        match super::super::Payload::capture_with_names(sample, names, previous_payload) {
            Ok((payload, additions)) => {
                names.commit(additions);
                Ok(Arc::new(payload))
            }
            Err(missing) => Err(missing),
        };
}

/// Replays the process frames of `read` into `decoder`, handing `consume` each second's sample,
/// and checks the minute was not replaced while it was read and replayed.
fn load_minute_visit_read(
    read: &MinuteRead<'_>,
    decoder: &mut Decoder,
    operation: &Operation,
    mut consume: impl FnMut(
        &crate::live_apps::committed::Endpoint,
        persistence::Result<&crate::live_app_response::Sample>,
        &persistence::Losses,
        usize,
    ) -> persistence::Result<()>,
) -> Result<(), Refused> {
    decoder.visit_samples(&read.processes, operation, |endpoint, sample, count| {
        consume(endpoint, sample, read.sealed.losses(), count)
    })?;
    operation.check()?;

    read.check(operation)
}

/// A sealed minute as a load reads it, its machine readings and its process frames, before any of
/// them is decoded.
struct MinuteRead<'input> {
    sealed: ScopedSealedMinute<'input>,
    machine: MachineMinute,
    processes: crate::recording::processes::ProcessBatch,
}

impl MinuteRead<'_> {
    /// Checks the minute was not replaced since it was opened.
    fn check(&self, operation: &Operation) -> Result<(), Refused> {
        self.sealed.revalidate_full()?;
        operation.check()?;

        Ok(())
    }
}

/// Opens `found` and reads its machine readings and its process frames.
fn read_minute<'input>(
    root: &Path,
    found: &Found,
    budget: &'input Arc<ByteBudget>,
    operation: &'input Operation,
    capture: &'input crate::recording::InputCapture<'input>,
) -> Result<MinuteRead<'input>, Refused> {
    operation.check()?;
    let sealed = open_minute(root, found, budget, operation, capture)?;
    let mut machine = MachineMinute::default();
    read_machine(&sealed, &mut machine)?;
    operation.check()?;

    let processes = crate::recording::processes::read_processes_batch(&sealed)?;
    operation.check()?;

    Ok(MinuteRead {
        sealed,
        machine,
        processes,
    })
}

pub(super) fn open_minute<'input>(
    root: &Path,
    found: &Found,
    budget: &'input Arc<ByteBudget>,
    operation: &'input Operation,
    capture: &'input crate::recording::InputCapture<'input>,
) -> Result<ScopedSealedMinute<'input>, Refused> {
    let sealed = ScopedSealedMinute::open_with_inputs_using(
        &SealedSource {
            root,
            marker: &found.marker,
        },
        budget,
        operation,
        capture,
    )?;
    if sealed.identity() != &found.run {
        return Err(Refused::Commitment(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "sealed minute identity differs from the registry",
        )));
    }

    Ok(sealed)
}

fn project_machine_apps(
    run: RunIdentity,
    second: MachineSecond,
    apps: &mut [AppsSecond],
    seconds: &mut Vec<Loaded>,
) {
    let index = apps.iter().position(|apps| {
        apps.observation
            .is_some_and(|observation| observation.stamp.publication == second.publication)
    });
    let payload = index
        .map(|index| mem::take(&mut apps[index]))
        .ok_or_else(|| {
            super::Missing::Refused(String::from(
                "the minute has no process sample for this publication",
            ))
        });
    seconds.push(Loaded {
        run,
        second,
        apps: payload.into(),
    });
}

/// Attempts count before fallible capture; refused compatibility summaries count their fallback.
pub(super) fn project_machine<'reading>(
    found: &Found,
    machine: &'reading MachineMinute,
    mut before: Option<super::Reading<'reading>>,
    budget: &Arc<ByteBudget>,
    mut take: impl FnMut(MachineSecond),
) -> Result<super::DecodedCatalog, crate::store::MachineCaptureError> {
    for reading in readings(machine) {
        persistence::work::ingestion::record_publication(
            found.run,
            reading.tick.publication,
            persistence::work::ingestion::Kind::MachineReduction,
        );
        if let Some(second) = MachineSecond::complete_of(&reading, before.as_ref(), budget)? {
            if matches!(
                second.complete,
                crate::store::CompleteMachineState::Refused(_)
            ) {
                persistence::work::ingestion::record_publication(
                    found.run,
                    reading.tick.publication,
                    persistence::work::ingestion::Kind::MachineProjectionRefused,
                );
            }
            take(second);
        }
        before = Some(reading);
    }
    Ok(decoded_of(machine))
}

/// What the catalog learns from decoding `machine`, one sealed minute.
fn decoded_of(machine: &MachineMinute) -> super::DecodedCatalog {
    let mut decoded = super::DecodedCatalog {
        windows: 1,
        ..super::DecodedCatalog::default()
    };
    for reading in readings(machine) {
        decoded.observe(reading.tick.wall_ns);
    }
    decoded
}
