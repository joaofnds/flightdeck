use super::{Error, Frame, Member, ProgramBytes, Result};
use crate::apps::{Aggregate, Interval, ProcessInterval, ProcessObservation, ProcessState};

pub(super) fn with_aggregate(
    selected: &Frame,
    previous: Option<&Frame>,
    consume: impl FnOnce(&Aggregate<'_>) -> Result<()>,
) -> Result<()> {
    let mut selected_programs = Vec::with_capacity(selected.members.len());
    with_aggregate_programs(selected, &mut selected_programs);
    let mut selected_observations = Vec::with_capacity(selected.members.len());
    with_aggregate_observations(selected, &selected_programs, &mut selected_observations);
    let previous = previous.unwrap_or(selected);
    let mut previous_programs = Vec::with_capacity(previous.members.len());
    with_aggregate_programs(previous, &mut previous_programs);
    let mut previous_observations = Vec::with_capacity(previous.members.len());
    with_aggregate_observations(previous, &previous_programs, &mut previous_observations);
    let compatible = with_aggregate_compatible(previous, selected);
    let mut processes = Vec::with_capacity(selected.members.len());
    for (member, observation) in selected.members.iter().zip(&selected_observations) {
        let previous = if compatible {
            with_aggregate_predecessor(previous, member).map(|index| &previous_observations[index])
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

fn with_aggregate_predecessor(previous: &Frame, member: &Member) -> Option<usize> {
    let creation = member.creation?;
    match previous
        .members
        .binary_search_by_key(&(member.pid, Some(creation.unique_id)), |member| {
            (
                member.pid,
                member.creation.map(|creation| creation.unique_id),
            )
        }) {
        Ok(index) => Some(index),
        Err(insertion) => {
            assert!(insertion <= previous.members.len());
            None
        }
    }
}
