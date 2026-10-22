use super::{Error, Exit, Frame, IdentityUnavailable, ProgramBytes, Result};
use crate::apps::{Aggregate, Interval, ProcessInterval, ProcessObservation, ProcessState};

#[cfg(test)]
mod tests;

pub(super) fn with_aggregate(
    selected: &Frame,
    previous: Option<&Frame>,
    consume: impl FnOnce(&Aggregate<'_>) -> Result<()>,
) -> Result<()> {
    let previous = previous.unwrap_or(selected);
    let compatible = with_aggregate_compatible(previous, selected);
    let mut previous_programs =
        Vec::with_capacity(previous.members.len() + previous.starts.len() + previous.execs.len());
    let mut previous_observations = Vec::with_capacity(previous.members.len());
    if compatible {
        with_aggregate_programs(previous, &mut previous_programs);
        with_aggregate_observations(previous, &previous_programs, &mut previous_observations);
    }

    let mut selected_programs =
        Vec::with_capacity(selected.members.len() + selected.starts.len() + selected.execs.len());
    with_aggregate_programs(selected, &mut selected_programs);
    let count = selected.members.len() + selected.exits.len();
    let mut selected_observations =
        Vec::with_capacity(count.min(crate::apps::PROCESS_INTERVALS_MAX));
    with_aggregate_observations(selected, &selected_programs, &mut selected_observations);
    with_aggregate_exits(
        selected,
        &Predecessor {
            frame: if compatible { Some(previous) } else { None },
            programs: &previous_programs,
            observations: &previous_observations,
        },
        &selected_programs,
        &mut selected_observations,
    )?;

    let processes = with_aggregate_intervals(&previous_observations, &selected_observations);

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

struct Predecessor<'frame> {
    frame: Option<&'frame Frame>,
    programs: &'frame [Option<ProgramBytes<'frame>>],
    observations: &'frame [ProcessObservation<'frame>],
}

fn with_aggregate_intervals<'frame>(
    previous: &'frame [ProcessObservation<'frame>],
    selected: &'frame [ProcessObservation<'frame>],
) -> Vec<ProcessInterval<'frame>> {
    let mut processes = Vec::with_capacity(selected.len());
    for observation in selected {
        let previous =
            with_aggregate_predecessor(previous, observation).map(|index| &previous[index]);
        processes.push(ProcessInterval {
            previous,
            selected: observation,
            start: None,
        });
    }

    processes
}

fn with_aggregate_programs<'frame>(
    frame: &'frame Frame,
    output: &mut Vec<Option<ProgramBytes<'frame>>>,
) {
    for member in &frame.members {
        output.push(with_aggregate_program(frame, &member.program));
    }

    with_aggregate_start_programs(frame, output);
    with_aggregate_exec_programs(frame, output);
}

fn with_aggregate_start_programs<'frame>(
    frame: &'frame Frame,
    output: &mut Vec<Option<ProgramBytes<'frame>>>,
) {
    for start in &frame.starts {
        output.push(with_aggregate_program(frame, &start.program));
    }
}

fn with_aggregate_exec_programs<'frame>(
    frame: &'frame Frame,
    output: &mut Vec<Option<ProgramBytes<'frame>>>,
) {
    for exec in &frame.execs {
        output.push(with_aggregate_program(frame, &exec.program));
    }
}

fn with_aggregate_program<'frame>(
    frame: &'frame Frame,
    program: &std::result::Result<super::ProgramSpan, IdentityUnavailable>,
) -> Option<ProgramBytes<'frame>> {
    match program {
        Ok(span) => Some(ProgramBytes {
            executable_name: &frame.arena[span.start..span.name_end],
            app_path: if span.name_end < span.end {
                Some(&frame.arena[span.name_end..span.end])
            } else {
                None
            },
        }),
        Err(_) => None,
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
            gpu_ns: member.gpu_ns,
            threads: member.threads,
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
    previous: &Predecessor<'frame>,
    programs: &'frame [Option<ProgramBytes<'frame>>],
    output: &mut Vec<ProcessObservation<'frame>>,
) -> Result<()> {
    let scheduled_count = output.len();

    for exit in &selected.exits {
        if exit.ambiguous {
            continue;
        }

        let Some(program) = with_aggregate_exit_program(selected, exit, programs, previous) else {
            continue;
        };
        let mut observation = with_aggregate_exit_observation(selected, exit, program);

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

fn with_aggregate_exit_program<'frame>(
    selected: &'frame Frame,
    exit: &Exit,
    programs: &'frame [Option<ProgramBytes<'frame>>],
    previous: &Predecessor<'frame>,
) -> Option<std::result::Result<&'frame ProgramBytes<'frame>, IdentityUnavailable>> {
    let index = with_aggregate_index(previous.observations, exit.pid, exit.creation.as_ref());
    let event_program = if index.is_none() {
        previous.frame.and_then(|frame| {
            with_aggregate_executed_exit(frame, exit, previous.programs, false)
                .or_else(|| with_aggregate_started_exit(frame, exit, previous.programs, false))
        })
    } else {
        None
    };

    if let Some(program) = with_aggregate_executed_exit(
        selected,
        exit,
        programs,
        index.is_some() || event_program.is_some(),
    ) {
        return Some(program);
    }

    if let Some(index) = index {
        return Some(if exit.creation == previous.observations[index].creation {
            previous.observations[index].program
        } else {
            Err(IdentityUnavailable::Changed)
        });
    }

    with_aggregate_started_exit(selected, exit, programs, event_program.is_some()).or(event_program)
}

fn with_aggregate_executed_exit<'frame>(
    selected: &'frame Frame,
    exit: &Exit,
    programs: &'frame [Option<ProgramBytes<'frame>>],
    previous_known: bool,
) -> Option<std::result::Result<&'frame ProgramBytes<'frame>, IdentityUnavailable>> {
    let creation = exit.creation?;
    let end = selected
        .execs
        .partition_point(|exec| (exec.pid, exec.number) < (exit.pid, exit.number));
    let index = end.checked_sub(1)?;
    let exec = &selected.execs[index];
    if exec.pid != exit.pid {
        return None;
    }

    if exec.creation != Some(creation) {
        if let Some(program) = with_aggregate_started_exit(selected, exit, programs, previous_known)
        {
            return Some(program);
        }
        if previous_known {
            return Some(Err(IdentityUnavailable::Changed));
        }
        return None;
    }

    let offset = selected.members.len() + selected.starts.len() + index;

    Some(match &programs[offset] {
        Some(program) => Ok(program),
        None => match exec.program {
            Err(reason) => Err(reason),
            Ok(_) => unreachable!("captured Exec program span has a program view"),
        },
    })
}

fn with_aggregate_started_exit<'frame>(
    selected: &'frame Frame,
    exit: &Exit,
    programs: &'frame [Option<ProgramBytes<'frame>>],
    previous_known: bool,
) -> Option<std::result::Result<&'frame ProgramBytes<'frame>, IdentityUnavailable>> {
    let creation = exit.creation?;
    let key = (exit.pid, Some(creation.unique_id));
    // The stable creation sort retains event order, so the first Start is the earliest.
    let index = selected.starts.partition_point(|start| {
        (start.pid, start.creation.map(|creation| creation.unique_id)) < key
    });
    let start = selected.starts.get(index)?;
    if (start.pid, start.creation.map(|creation| creation.unique_id)) != key {
        return None;
    }

    if start.number >= exit.number {
        return None;
    }

    if start.ambiguous {
        return previous_known.then_some(Err(IdentityUnavailable::Changed));
    }

    if start.exec_number.is_some_and(|number| number < exit.number) {
        return previous_known.then_some(Err(IdentityUnavailable::Changed));
    }

    if start.creation != exit.creation {
        return previous_known.then_some(Err(IdentityUnavailable::Changed));
    }

    Some(match &programs[selected.members.len() + index] {
        Some(program) => Ok(program),
        None => match start.program {
            Err(reason) => Err(reason),
            Ok(_) => unreachable!("captured Start program span has a program view"),
        },
    })
}

fn with_aggregate_exit_observation<'frame>(
    selected: &'frame Frame,
    exit: &'frame Exit,
    program: std::result::Result<&'frame ProgramBytes<'frame>, IdentityUnavailable>,
) -> ProcessObservation<'frame> {
    ProcessObservation {
        stamp: selected.stamp,
        pid: exit.pid,
        creation: exit.creation,
        program,
        usage: exit.usage.as_ref().map_err(|reason| *reason),
        gpu_ns: None,
        threads: None,
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
