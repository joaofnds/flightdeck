use collector::processes::Unreadable;
use collector::tracker::{EVENTS_PER_TICK_MAX, Event};

use super::{Error, Exit, Frame, Publication, PublicationEvents, Result, Unavailable};

pub(super) fn capture(
    frame: &mut Frame,
    source: &Publication<'_>,
    events: &PublicationEvents<'_>,
) -> Result<()> {
    frame.exits.clear();
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
    capture_events(frame, events)?;
    frame
        .exits
        .sort_by_key(|exit| (exit.pid, exit.creation.map(|creation| creation.unique_id)));
    capture_duplicates(&mut frame.exits);

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

fn capture_events(frame: &mut Frame, events: &PublicationEvents<'_>) -> Result<()> {
    for (numbered, identity) in events.events.iter().zip(events.identities) {
        if let Event::Exit { pid, last, .. } = numbered.event {
            if *pid != identity.pid {
                return Err(Error::Bound("live App exit PID association"));
            }

            frame.exits.push(Exit {
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
    }

    Ok(())
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
