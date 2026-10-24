//! The opt-in complete process decoder, sharing the recorded App rule and predecessor.
use std::mem;
use std::path::Path;
use std::sync::Arc;

use super::readings;
use super::{AppsSecond, Batch, Failure, Found, Loaded, MachineSecond, Refused, TICKS_PER_MINUTE};
use crate::byte_budget::ByteBudget;
use crate::recording::{Before, MachineMinute, SealedMinute, read_machine};
use crate::store::apps::TickApps;

pub(super) fn load_batch(
    root: &Path,
    before: Option<&Found>,
    minutes: &[Found],
    budget: &Arc<ByteBudget>,
) -> Batch {
    let mut batch = Batch {
        seconds: Vec::with_capacity(minutes.len() * TICKS_PER_MINUTE),
        left_out: None,
    };
    let mut predecessor = None;
    let mut previous = MachineMinute::default();
    let mut previous_run = None;
    if let Some(before) = before {
        if let Ok(sealed) = SealedMinute::open(root, &before.marker) {
            if read_machine(&sealed, &mut previous).is_ok() {
                if sealed
                    .visit_samples(&mut predecessor, |_, _| Ok(()))
                    .is_ok()
                {
                    previous_run = Some(before.run);
                }
            }
        }
    }

    for found in minutes {
        let loaded = load_minute(root, found, &mut predecessor, budget);
        let Ok((mut machine, mut apps)) = loaded else {
            batch.left_out = Some(Failure::Refused);
            predecessor = None;
            previous_run = None;
            continue;
        };
        let mut before = if previous_run == Some(found.run) {
            readings(&previous).last()
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
        mem::swap(&mut machine, &mut previous);
        previous_run = Some(found.run);
    }
    batch
}

fn load_minute(
    root: &Path,
    found: &Found,
    before: &mut Option<Before>,
    budget: &Arc<ByteBudget>,
) -> Result<(MachineMinute, Vec<AppsSecond>), Refused> {
    let sealed = SealedMinute::open(root, &found.marker)?;
    let mut machine = MachineMinute::default();
    read_machine(&sealed, &mut machine)?;
    let mut apps = Vec::with_capacity(machine.ticks.len());
    sealed.visit_samples(before, |endpoint, sample| {
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
    })?;
    Ok((machine, apps))
}
