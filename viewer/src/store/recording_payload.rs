//! The opt-in complete process decoder, sharing the recorded App rule and predecessor.
use std::mem;
use std::path::Path;
use std::sync::Arc;

use collector::ring::RunIdentity;

use super::readings;
use super::{AppsSecond, Batch, Failure, Found, Loaded, MachineSecond, Refused, TICKS_PER_MINUTE};

#[path = "recording_replay.rs"]
pub(super) mod replay;
use crate::byte_budget::{ByteBudget, ByteReservation};
use crate::cancellation::Operation;
use crate::recording::{Before, MachineMinute, ScopedSealedMinute, SealedSource, read_machine};
use crate::store::apps::TickApps;

#[cfg(test)]
#[path = "recording_payload_tests.rs"]
mod tests;

pub(super) fn terminal(
    root: &Path,
    found: &Found,
    budget: &Arc<ByteBudget>,
    operation: &Operation,
    capture: &crate::recording::InputCapture<'_>,
) -> Result<Option<super::Terminal>, Refused> {
    let _purpose =
        persistence::work::ingestion::purpose(persistence::work::ingestion::Purpose::Predecessor);
    operation.check()?;
    let sealed = open_minute(root, found, budget, operation, capture)?;
    let mut machine = MachineMinute::default();
    read_machine(&sealed, &mut machine)?;
    operation.check()?;
    let process = sealed.terminal_processes(operation)?;
    operation.check()?;
    sealed.revalidate_full()?;
    operation.check()?;

    Ok(super::Terminal::from_minute(found.run, machine, process))
}

pub(super) fn load(
    root: &Path,
    minutes: &[Found],
    budget: &Arc<ByteBudget>,
    operation: &Operation,
    capture: &crate::recording::InputCapture<'_>,
    mut take: impl FnMut(&RunIdentity, &MachineSecond, TickApps<'_>),
) -> persistence::Result<super::Outcome> {
    let input = LoadInput {
        root,
        budget,
        operation,
        capture,
    };
    let mut cursor = Cursor::new(minutes.len());
    for found in minutes {
        cursor.step(&input, found, &mut take)?;
    }

    cursor.finish(operation)
}

pub(super) struct Cursor {
    minute_count: usize,
    pub(super) next_minute_index: usize,
    pub(super) decoder: Decoder,
    left_out: Option<Failure>,
}

struct LoadInput<'a> {
    root: &'a Path,
    budget: &'a Arc<ByteBudget>,
    operation: &'a Operation,
    capture: &'a crate::recording::InputCapture<'a>,
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

    fn step(
        &mut self,
        input: &LoadInput<'_>,
        found: &Found,
        take: impl FnMut(&RunIdentity, &MachineSecond, TickApps<'_>),
    ) -> persistence::Result<()> {
        input.operation.check()?;
        if self.next_minute_index == self.minute_count {
            return Err(persistence::Error::Bound("sealed payload cursor exhausted"));
        }

        assert!(self.next_minute_index < self.minute_count);

        let completion = self.decoder.decode(
            input.root,
            found,
            input.budget,
            input.operation,
            input.capture,
        );
        if let Some(failure) = load_consume(completion, input.operation, take)? {
            self.left_out = Some(failure);
        }
        self.accept_minute();

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

impl Decoder {
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

    pub(super) fn accept_machine(
        &mut self,
        run: RunIdentity,
        machine: MachineMinute,
        decoded: &super::DecodedCatalog,
        reservation: Option<ByteReservation>,
    ) {
        self.decoded_catalog.extend(decoded);
        self.previous = machine;
        self.machine_reservation = reservation;
        self.previous_run = Some(run);
    }

    fn decode(
        &mut self,
        root: &Path,
        found: &Found,
        budget: &Arc<ByteBudget>,
        operation: &Operation,
        capture: &crate::recording::InputCapture<'_>,
    ) -> Batch {
        let mut batch = Batch {
            seconds: Vec::with_capacity(TICKS_PER_MINUTE),
            left_out: None,
        };
        let loaded = load_minute(root, found, self, budget, operation, capture);
        let Ok((machine, mut apps)) = loaded else {
            batch.left_out = Some(Failure::Refused);
            self.predecessor = None;
            self.previous_run = None;
            return batch;
        };
        let decoded = match self.project_machine(found, &machine, budget, |second| {
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
        self.accept_machine(found.run, machine, &decoded, None);

        batch
    }
}

fn load_minute(
    root: &Path,
    found: &Found,
    decoder: &mut Decoder,
    budget: &Arc<ByteBudget>,
    operation: &Operation,
    capture: &crate::recording::InputCapture<'_>,
) -> Result<(MachineMinute, Vec<AppsSecond>), Refused> {
    let mut apps = Vec::with_capacity(crate::live_apps::committed::ENDPOINTS_MAX);
    let mut names = None;
    let machine = load_minute_visit(
        root,
        found,
        decoder,
        budget,
        operation,
        capture,
        |endpoint, sample, losses, count| {
            let mut second = AppsSecond::default();
            load_minute_rows(&sample, apps.last(), budget, &mut second);
            load_minute_payload(&sample, budget, count, &mut names, &mut second);
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

    Ok((machine, apps))
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
        second.take(
            TickApps::Missing(&super::Missing::Budget),
            previous,
            Some(budget),
        );
        return;
    };
    if let Ok(sample) = sample {
        let count = if sample.apps().len() <= crate::store::APPS_PER_SECOND_MAX {
            sample.apps().len()
        } else {
            0
        };
        if let Err(missing) = second.apps.replace_admitted(count, budget) {
            second.take(TickApps::Missing(&missing), previous, Some(budget));
            return;
        }
        second.fill(sample, previous, Some(budget));
    } else {
        second.take(
            TickApps::Refused(reason.as_deref().unwrap_or_else(|| unreachable!("refusal"))),
            previous,
            Some(budget),
        );
    }
    second.metadata = metadata;
}

pub(super) fn load_minute_payload(
    sample: &persistence::Result<&crate::live_app_response::Sample>,
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
    second.processes = match super::super::Payload::capture_with_names(sample, names) {
        Ok((payload, additions)) => {
            names.commit(additions);
            Ok(Arc::new(payload))
        }
        Err(missing) => Err(missing),
    };
}

fn load_minute_visit(
    root: &Path,
    found: &Found,
    decoder: &mut Decoder,
    budget: &Arc<ByteBudget>,
    operation: &Operation,
    capture: &crate::recording::InputCapture<'_>,
    mut consume: impl FnMut(
        &crate::live_apps::committed::Endpoint,
        persistence::Result<&crate::live_app_response::Sample>,
        &persistence::Losses,
        usize,
    ) -> persistence::Result<()>,
) -> Result<MachineMinute, Refused> {
    operation.check()?;
    let sealed = open_minute(root, found, budget, operation, capture)?;
    let mut machine = MachineMinute::default();
    read_machine(&sealed, &mut machine)?;
    operation.check()?;

    decoder.visit_samples(&sealed, operation, |endpoint, sample, count| {
        consume(endpoint, sample, sealed.losses(), count)
    })?;
    operation.check()?;

    sealed.revalidate_full()?;
    operation.check()?;

    Ok(machine)
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
    let mut decoded = super::DecodedCatalog {
        windows: 1,
        ..super::DecodedCatalog::default()
    };
    for reading in readings(machine) {
        decoded.observe(reading.tick.wall_ns);
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
                    persistence::work::ingestion::Kind::MachineReduction,
                );
            }
            take(second);
        }
        before = Some(reading);
    }
    Ok(decoded)
}
