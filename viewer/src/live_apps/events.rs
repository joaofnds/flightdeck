use collector::processes::Unreadable;
use collector::tracker::{EVENTS_PER_TICK_MAX, Event};

use super::{Error, Exec, Exit, Frame, Publication, PublicationEvents, Result, Start, Unavailable};

pub(super) fn capture(
    frame: &mut Frame,
    source: &Publication<'_>,
    events: &PublicationEvents<'_>,
) -> Result<()> {
    frame.exits.clear();
    frame.starts.clear();
    frame.execs.clear();
    let scheduled_bytes = frame
        .members
        .iter()
        .filter_map(|member| match member.program {
            Ok(span) => Some(span.end),
            Err(_) => None,
        })
        .max()
        .unwrap_or_default();
    frame.arena.truncate(scheduled_bytes);
    super::validate(source)?;
    if source.reader.identity().as_ref() != Some(&frame.identity) {
        return Err(Error::Bound("live App exit run association"));
    }

    if source.publication != frame.stamp.publication {
        return Err(Error::Bound("live App exit publication association"));
    }

    if events.events.len() > EVENTS_PER_TICK_MAX {
        return Err(Error::Bound("live App exit event bound"));
    }

    if source.reader.dictionary_capacity().is_none() {
        return Ok(());
    }

    if events.events.len() != events.identities.len() {
        return Err(Error::Bound("live App exit identity count"));
    }

    capture_validate(source, events)?;

    let count = events
        .events
        .iter()
        .filter(|numbered| matches!(numbered.event, Event::Exit { .. }))
        .count();

    frame.exits.reserve_exact(count);
    capture_programs_reserve(frame, source, events)?;
    capture_events(frame, source, events)?;
    frame
        .exits
        .sort_by_key(|exit| (exit.pid, exit.creation.map(|creation| creation.unique_id)));
    capture_duplicates(&mut frame.exits);
    frame
        .starts
        .sort_by_key(|start| (start.pid, start.creation.map(|creation| creation.unique_id)));
    capture_start_duplicates(&mut frame.starts);
    capture_start_interruptions(&mut frame.starts, events);
    frame.execs.sort_by_key(|exec| (exec.pid, exec.number));

    Ok(())
}

fn capture_validate(source: &Publication<'_>, events: &PublicationEvents<'_>) -> Result<()> {
    let mut next = source.tick.events.start;

    for numbered in events.events {
        if numbered.tick != source.tick.run_tick {
            return Err(Error::Bound("live App exit tick association"));
        }

        if numbered.number < next {
            return Err(Error::Bound("live App exit event order"));
        }

        if numbered.number >= source.tick.events.end {
            return Err(Error::Bound("live App exit event range"));
        }

        next = numbered
            .number
            .checked_add(1)
            .ok_or(Error::Bound("live App exit event number"))?;
    }

    // Gaps leave membership uncertain; each retained exit still has its own source identity.
    Ok(())
}

fn capture_programs_reserve(
    frame: &mut Frame,
    source: &Publication<'_>,
    events: &PublicationEvents<'_>,
) -> Result<()> {
    let mut starts = 0;
    let mut execs = 0;
    let mut size = frame.arena.len();
    for (numbered, identity) in events.events.iter().zip(events.identities) {
        if matches!(numbered.event, Event::Start { .. } | Event::Exec { .. }) {
            if matches!(numbered.event, Event::Start { .. }) {
                starts += 1;
            } else {
                execs += 1;
            }
            size = size
                .checked_add(super::capture::program_bytes(
                    source,
                    std::slice::from_ref(identity),
                )?)
                .ok_or(Error::Bound("live App program byte bound"))?;
            if size > super::PROGRAM_FRAME_BYTES_MAX {
                return Err(Error::Bound("live App program byte bound"));
            }
        }
    }
    if frame.members.len() + starts + execs > crate::apps::PROCESS_INTERVALS_MAX {
        return Err(Error::Bound("live App program interval bound"));
    }

    frame.starts.reserve_exact(starts);
    frame.execs.reserve_exact(execs);
    frame.arena.reserve_exact(size - frame.arena.len());

    Ok(())
}

fn capture_events(
    frame: &mut Frame,
    source: &Publication<'_>,
    events: &PublicationEvents<'_>,
) -> Result<()> {
    for (numbered, identity) in events.events.iter().zip(events.identities) {
        if let Event::Exit { pid, last, .. } = numbered.event {
            if *pid != identity.pid {
                return Err(Error::Bound("live App exit PID association"));
            }

            frame.exits.push(Exit {
                number: numbered.number,
                pid: *pid,
                creation: identity.creation().copied(),
                usage: match last {
                    Ok(usage) => Ok(*usage),
                    Err(Unreadable::Refused) => Err(Unavailable::Refused),
                    Err(Unreadable::Gone) => Err(Unavailable::Gone),
                },
                ambiguous: false,
            });
        }
        if let Event::Start { pid, .. } = numbered.event {
            capture_start(frame, source, identity, numbered.number, *pid)?;
        }
        if let Event::Exec { pid } = numbered.event {
            capture_exec(frame, source, identity, numbered.number, *pid)?;
        }
    }

    Ok(())
}

fn capture_exec(
    frame: &mut Frame,
    source: &Publication<'_>,
    identity: &collector::ring::IdentityObservation,
    number: u64,
    pid: i32,
) -> Result<()> {
    if identity.pid != pid {
        return Err(Error::Bound("live App Exec PID association"));
    }

    if identity.usage != collector::ring::UsageAvailability::NotApplicable {
        return Err(Error::Bound("live App Exec usage availability"));
    }

    frame.execs.push(Exec {
        number,
        pid,
        creation: identity.creation().copied(),
        program: Err(super::IdentityUnavailable::Unsupported),
    });
    let exec = frame.execs.last_mut().unwrap_or_else(|| unreachable!());

    super::capture::member_program(
        source,
        &identity.identity,
        &mut frame.arena,
        &mut exec.program,
    )
}

fn capture_start(
    frame: &mut Frame,
    source: &Publication<'_>,
    identity: &collector::ring::IdentityObservation,
    number: u64,
    pid: i32,
) -> Result<()> {
    if identity.pid != pid {
        return Err(Error::Bound("live App Start PID association"));
    }

    if identity.usage != collector::ring::UsageAvailability::NotApplicable {
        return Err(Error::Bound("live App Start usage availability"));
    }

    frame.starts.push(Start {
        number,
        pid,
        creation: identity.creation().copied(),
        program: Err(super::IdentityUnavailable::Unsupported),
        ambiguous: false,
        exec_number: None,
    });
    let start = frame.starts.last_mut().unwrap_or_else(|| unreachable!());

    super::capture::member_program(
        source,
        &identity.identity,
        &mut frame.arena,
        &mut start.program,
    )
}

fn capture_start_duplicates(starts: &mut [Start]) {
    for index in 1..starts.len() {
        let (before, after) = starts.split_at_mut(index);
        let previous = &mut before[index - 1];
        let selected = &mut after[0];

        if previous.pid == selected.pid {
            if previous.creation.map(|creation| creation.unique_id)
                == selected.creation.map(|creation| creation.unique_id)
            {
                previous.ambiguous = true;
                selected.ambiguous = true;
            }
        }
    }
}

fn capture_start_interruptions(starts: &mut [Start], events: &PublicationEvents<'_>) {
    // The bounded temporary list avoids scanning all events once for every captured Start.
    let count = events
        .events
        .iter()
        .filter(|numbered| matches!(numbered.event, Event::Exec { .. }))
        .count();
    if count == 0 {
        return;
    }

    let mut replacements = Vec::with_capacity(count);
    for numbered in events.events {
        if let Event::Exec { pid } = numbered.event {
            replacements.push((*pid, numbered.number));
        }
    }
    replacements.sort_unstable();
    assert!(replacements.capacity() <= EVENTS_PER_TICK_MAX);

    for start in starts {
        let index =
            replacements.partition_point(|replacement| *replacement <= (start.pid, start.number));
        if let Some((pid, number)) = replacements.get(index) {
            if *pid == start.pid {
                start.exec_number = Some(*number);
            }
        }
    }
}

fn capture_duplicates(exits: &mut [Exit]) {
    for index in 1..exits.len() {
        let (before, after) = exits.split_at_mut(index);
        let previous = &mut before[index - 1];
        let selected = &mut after[0];

        if previous.pid == selected.pid {
            if previous.creation.map(|creation| creation.unique_id)
                == selected.creation.map(|creation| creation.unique_id)
            {
                previous.ambiguous = true;
                selected.ambiguous = true;
            }
        }
    }
}
