use super::{Error, Exit, Frame, IdentityUnavailable, ProgramBytes, Result};
use crate::apps::{Aggregate, Interval, ProcessInterval, ProcessObservation, ProcessState};

pub(super) fn with_aggregate(
    selected: &Frame,
    previous: Option<&Frame>,
    consume: impl FnOnce(&Aggregate<'_>) -> Result<()>,
) -> Result<()> {
    let previous = previous.unwrap_or(selected);
    let mut previous_programs = Vec::with_capacity(previous.members.len());
    with_aggregate_programs(previous, &mut previous_programs);
    let mut previous_observations = Vec::with_capacity(previous.members.len());
    with_aggregate_observations(previous, &previous_programs, &mut previous_observations);
    let compatible = with_aggregate_compatible(previous, selected);

    let mut selected_programs = Vec::with_capacity(selected.members.len());
    with_aggregate_programs(selected, &mut selected_programs);
    let count = selected.members.len() + previous.members.len().min(selected.exits.len());
    let mut selected_observations =
        Vec::with_capacity(count.min(crate::apps::PROCESS_INTERVALS_MAX));
    with_aggregate_observations(selected, &selected_programs, &mut selected_observations);
    if compatible {
        with_aggregate_exits(selected, &previous_observations, &mut selected_observations)?;
    }

    let mut processes = Vec::with_capacity(selected_observations.len());
    for observation in &selected_observations {
        let previous = if compatible {
            with_aggregate_predecessor(&previous_observations, observation)
                .map(|index| &previous_observations[index])
        } else {
            None
        };
        processes.push(ProcessInterval {
            previous,
            selected: observation,
            start: None,
        });
    }

    let interval = Interval {
        previous: previous.stamp,
        selected: selected.stamp,
        observations_complete: compatible,
        events_complete: false,
    };
    let view_bytes = (selected_programs.capacity() + previous_programs.capacity())
        * size_of::<Option<ProgramBytes<'_>>>()
        + (selected_observations.capacity() + previous_observations.capacity())
            * size_of::<ProcessObservation<'_>>()
        + processes.capacity() * size_of::<ProcessInterval<'_>>();
    assert!(view_bytes <= super::TRANSIENT_VIEW_BYTES_MAX);

    let mut aggregate = None;
    Aggregate::initialize(&mut aggregate);
    let aggregate = aggregate.as_mut().unwrap_or_else(|| unreachable!());
    aggregate
        .compute(&interval, &processes)
        .map_err(|_| Error::Bound("live App aggregate input"))?;

    consume(aggregate)
}

fn with_aggregate_programs<'frame>(
    frame: &'frame Frame,
    output: &mut Vec<Option<ProgramBytes<'frame>>>,
) {
    for member in &frame.members {
        output.push(match member.program {
            Ok(span) => Some(ProgramBytes {
                executable_name: &frame.arena[span.start..span.name_end],
                app_path: if span.name_end < span.end {
                    Some(&frame.arena[span.name_end..span.end])
                } else {
                    None
                },
            }),
            Err(_) => None,
        });
    }
}

fn with_aggregate_observations<'frame>(
    frame: &'frame Frame,
    programs: &'frame [Option<ProgramBytes<'frame>>],
    output: &mut Vec<ProcessObservation<'frame>>,
) {
    for (member, program) in frame.members.iter().zip(programs) {
        let program = match program {
            Some(program) => Ok(program),
            None => match member.program {
                Err(reason) => Err(reason),
                Ok(_) => unreachable!("captured program span has a program view"),
            },
        };
        output.push(ProcessObservation {
            stamp: frame.stamp,
            pid: member.pid,
            creation: member.creation,
            program,
            usage: member.usage.as_ref().map_err(|reason| *reason),
            state: ProcessState::Live,
        });
    }
}

fn with_aggregate_compatible(previous: &Frame, selected: &Frame) -> bool {
    if previous.identity.recording != selected.identity.recording {
        return false;
    }
    if previous.stamp.boot != selected.stamp.boot {
        return false;
    }
    if previous.stamp.run != selected.stamp.run {
        return false;
    }
    if previous.stamp.publication.checked_add(1) != Some(selected.stamp.publication) {
        return false;
    }
    let (Some(previous), Some(selected)) =
        (previous.stamp.continuous_ns, selected.stamp.continuous_ns)
    else {
        return false;
    };

    previous < selected
}

fn with_aggregate_predecessor(
    previous: &[ProcessObservation<'_>],
    selected: &ProcessObservation<'_>,
) -> Option<usize> {
    with_aggregate_index(previous, selected.pid, selected.creation.as_ref())
}

fn with_aggregate_index(
    observations: &[ProcessObservation<'_>],
    pid: i32,
    creation: Option<&super::ProcessCreation>,
) -> Option<usize> {
    let creation = creation?;

    match observations.binary_search_by_key(&(pid, Some(creation.unique_id)), |member| {
        (
            member.pid,
            member.creation.map(|creation| creation.unique_id),
        )
    }) {
        Ok(index) => Some(index),
        Err(insertion) => {
            assert!(insertion <= observations.len());
            None
        }
    }
}

fn with_aggregate_exits<'frame>(
    selected: &'frame Frame,
    previous: &[ProcessObservation<'frame>],
    output: &mut Vec<ProcessObservation<'frame>>,
) -> Result<()> {
    let scheduled_count = output.len();

    for exit in &selected.exits {
        if exit.ambiguous {
            continue;
        }

        let Some(index) = with_aggregate_index(previous, exit.pid, exit.creation.as_ref()) else {
            continue;
        };

        let mut observation = with_aggregate_exit_observation(selected, exit, &previous[index]);

        if let Some(index) =
            with_aggregate_index(&output[..scheduled_count], exit.pid, exit.creation.as_ref())
        {
            with_aggregate_exit_reconcile(&mut observation, &output[index]);
            output[index] = observation;
        } else {
            if output.len() >= crate::apps::PROCESS_INTERVALS_MAX {
                return Err(Error::Bound("live App exit interval bound"));
            }

            output.push(observation);
        }
    }

    output.sort_by_key(|observation| {
        (
            observation.pid,
            observation.creation.map(|creation| creation.unique_id),
        )
    });

    Ok(())
}

fn with_aggregate_exit_observation<'frame>(
    selected: &'frame Frame,
    exit: &'frame Exit,
    previous: &ProcessObservation<'frame>,
) -> ProcessObservation<'frame> {
    let program = if exit.creation == previous.creation {
        previous.program
    } else {
        Err(IdentityUnavailable::Changed)
    };

    ProcessObservation {
        stamp: selected.stamp,
        pid: exit.pid,
        creation: exit.creation,
        program,
        usage: exit.usage.as_ref().map_err(|reason| *reason),
        state: if exit.usage.is_ok() {
            ProcessState::ExitedVerified
        } else {
            ProcessState::ExitedUnverified
        },
    }
}

fn with_aggregate_exit_reconcile<'frame>(
    exit: &mut ProcessObservation<'frame>,
    scheduled: &ProcessObservation<'frame>,
) {
    if let Ok(before) = scheduled.usage {
        if let Ok(after) = exit.usage {
            if after.awake_ns_at_start == before.awake_ns_at_start {
                if after.user_ns >= before.user_ns {
                    if after.system_ns >= before.system_ns {
                        if after.energy_nj >= before.energy_nj {
                            return;
                        }
                    }
                }
            }
        }

        exit.usage = scheduled.usage;
        exit.state = ProcessState::ExitedUnverified;
    }
}
