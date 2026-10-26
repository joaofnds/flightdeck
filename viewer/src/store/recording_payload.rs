//! The opt-in complete process decoder, sharing the recorded App rule and predecessor.
use std::mem;
use std::path::Path;
use std::sync::Arc;

use collector::ring::RunIdentity;

use super::readings;
use super::{AppsSecond, Batch, Failure, Found, Loaded, MachineSecond, Refused, TICKS_PER_MINUTE};
use crate::byte_budget::ByteBudget;
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
    let mut decoder = Decoder::default();
    let mut left_out = None;
    for found in minutes {
        operation.check()?;
        let completion = decoder.decode(root, found, budget, operation, capture);
        if let Some(failure) = load_consume(completion, operation, &mut take)? {
            left_out = Some(failure);
        }
    }
    operation.check()?;

    Ok(super::Outcome {
        left_out,
        terminal: decoder.terminal(),
    })
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
        let apps = match &mut loaded.apps {
            Ok(apps) => TickApps::Loaded(apps),
            Err(missing) => TickApps::Missing(missing),
        };
        take(&loaded.run, &loaded.second, apps);
    }
    operation.check()?;

    Ok(completion.left_out)
}

#[derive(Default)]
struct Decoder {
    predecessor: Option<Before>,
    previous: MachineMinute,
    previous_run: Option<RunIdentity>,
}

impl Decoder {
    fn terminal(self) -> Option<super::Terminal> {
        let run = self.previous_run?;

        super::Terminal::from_minute(run, self.previous, self.predecessor)
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
        let loaded = load_minute(
            root,
            found,
            &mut self.predecessor,
            budget,
            operation,
            capture,
        );
        let Ok((mut machine, mut apps)) = loaded else {
            batch.left_out = Some(Failure::Refused);
            self.predecessor = None;
            self.previous_run = None;
            return batch;
        };
        let mut before = if self.previous_run == Some(found.run) {
            readings(&self.previous).last()
        } else {
            None
        };
        for reading in readings(&machine) {
            if let Some(second) = MachineSecond::of(&reading, before.as_ref()) {
                let index = apps.iter().position(|apps| {
                    apps.observation.is_some_and(|observation| {
                        observation.stamp.publication == second.publication
                    })
                });
                let payload = index
                    .map(|index| mem::take(&mut apps[index]))
                    .ok_or_else(|| {
                        super::Missing::Refused(String::from(
                            "the minute has no process sample for this publication",
                        ))
                    });
                batch.seconds.push(Loaded {
                    run: found.run,
                    second,
                    apps: payload,
                });
            }
            before = Some(reading);
        }
        mem::swap(&mut machine, &mut self.previous);
        self.previous_run = Some(found.run);

        batch
    }
}

fn load_minute(
    root: &Path,
    found: &Found,
    before: &mut Option<Before>,
    budget: &Arc<ByteBudget>,
    operation: &Operation,
    capture: &crate::recording::InputCapture<'_>,
) -> Result<(MachineMinute, Vec<AppsSecond>), Refused> {
    let mut apps = Vec::with_capacity(crate::live_apps::committed::ENDPOINTS_MAX);
    let machine = load_minute_visit(
        root,
        found,
        before,
        budget,
        operation,
        capture,
        |endpoint, sample| {
            let mut second = match sample {
                Ok(sample) => {
                    let mut second = AppsSecond::with_room_for(sample.apps().len());
                    second.take(TickApps::Derived(sample), apps.last(), Some(budget));
                    second
                }
                Err(error) => {
                    let mut second = AppsSecond::default();
                    second.take(
                        TickApps::Refused(&error.to_string()),
                        apps.last(),
                        Some(budget),
                    );
                    second
                }
            };
            second.observation = Some(crate::live_app_response::Observation {
                recording: endpoint.identity.recording.0,
                stamp: endpoint.stamp,
                run_tick: endpoint.run_tick,
                wall_ns: endpoint.wall_ns,
            });
            apps.push(second);
            Ok(())
        },
    )?;

    Ok((machine, apps))
}

fn load_minute_visit(
    root: &Path,
    found: &Found,
    before: &mut Option<Before>,
    budget: &Arc<ByteBudget>,
    operation: &Operation,
    capture: &crate::recording::InputCapture<'_>,
    consume: impl FnMut(
        &crate::live_apps::committed::Endpoint,
        persistence::Result<&crate::live_app_response::Sample>,
    ) -> persistence::Result<()>,
) -> Result<MachineMinute, Refused> {
    operation.check()?;
    let sealed = open_minute(root, found, budget, operation, capture)?;
    let mut machine = MachineMinute::default();
    read_machine(&sealed, &mut machine)?;
    operation.check()?;

    sealed.visit_samples_with_operation(before, operation, consume)?;
    operation.check()?;

    sealed.revalidate_full()?;
    operation.check()?;

    Ok(machine)
}

fn open_minute<'input>(
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
