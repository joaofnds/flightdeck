use std::fmt;
use std::io;

use collector::ring::{Found, FoundEvent, IdentityObservation, RingReader, Tick};
use persistence::{
    Begin, CommitBoundary, Encoder, Error, IdentitySource, Limits, Losses, Manifest, NumberedEvent,
    Observation, Result, Store, TimeBounds, Window, WindowIdentity,
};

use super::Interruption;
use super::source::{self, Source};

#[derive(Debug)]
struct IntentionalInterruption;

impl fmt::Display for IntentionalInterruption {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("intentional compatibility fixture interruption")
    }
}

impl std::error::Error for IntentionalInterruption {}

pub fn prepare(store: &Store, source: &Source, interruption: Interruption) -> Result<()> {
    let mut staged = None;
    prepare_stage(source, &mut staged)?;
    let window = staged.ok_or(Error::Bound("fixture window absent"))?;
    let Begin::Pending(pending) = store.begin(&window.identity)? else {
        return Err(Error::Bound("fresh fixture window already committed"));
    };

    let tables = Encoder.encode(&window, pending.path())?;
    let mut manifest = None;
    prepare_manifest(source, tables, &mut manifest);
    let manifest = manifest.ok_or(Error::Bound("fixture manifest absent"))?;
    let target = match interruption {
        Interruption::PendingSynced => CommitBoundary::PendingSynced,
        Interruption::ReadyRenamed => CommitBoundary::ReadyRenamed,
    };

    let mut reached = false;
    let result = store.commit_observed(pending, manifest, &mut |boundary| {
        if boundary == target {
            reached = true;
            return Err(io::Error::other(IntentionalInterruption));
        }

        Ok(())
    });
    let Err(error) = result else {
        return Err(Error::Bound("fixture interruption was not reached"));
    };

    if !reached {
        return Err(error.into());
    }

    if !error
        .get_ref()
        .is_some_and(<dyn std::error::Error + Send + Sync + 'static>::is::<IntentionalInterruption>)
    {
        return Err(error.into());
    }

    Ok(())
}

fn prepare_stage(source: &Source, output: &mut Option<Box<Window>>) -> Result<()> {
    assert!(output.is_none());
    let mut reader = RingReader::open_generation(&source.ring)?;
    if reader.source_layout_version() != 4 {
        return Err(Error::Bound("interrupted fixture requires layout4"));
    }

    let mut tick = persistence::tick_buffer(&reader.shape())?;
    let mut processes = Vec::with_capacity(3);
    let window = output.insert(Box::new(Window::new(
        WindowIdentity {
            run: source.identity,
            window: 0,
        },
        Limits::from_shape(&reader.shape())?,
    )));
    for index in 0..2 {
        let Found::Publication { publication, .. } =
            reader.read_identity_into(&mut tick, &mut processes)?
        else {
            return Err(Error::Bound("fixture publication missing"));
        };

        if publication != index + 1 {
            return Err(Error::Bound("fixture publication order"));
        }

        prepare_stage_publication(&mut reader, &tick, &processes, window, publication)?;
    }

    if !reader.completed() {
        return Err(Error::Bound("fixture source incomplete"));
    }

    if reader.read_identity_into(&mut tick, &mut processes)? != Found::Nothing {
        return Err(Error::Bound("fixture source has extra publications"));
    }

    drop(reader);

    Ok(())
}

fn prepare_stage_publication(
    reader: &mut RingReader,
    tick: &Tick,
    processes: &[IdentityObservation],
    window: &mut Window,
    publication: u64,
) -> Result<()> {
    reader.seek_event(tick.events.start)?;
    let mut identity = IdentityObservation::EMPTY;
    let FoundEvent::Event {
        number,
        tick: run_tick,
        event,
    } = reader.read_event_identity(&mut identity)?
    else {
        return Err(Error::Bound("fixture event missing"));
    };

    let times = tick
        .times
        .ok_or(Error::Bound("fixture observation clocks"))?;
    window.push_tick_with_identity(
        &Observation {
            publication,
            wall_ns: Some(times.wall_ns),
            continuous_ns: Some(times.continuous_ns),
            awake_ns: Some(times.awake_ns),
        },
        tick,
        &[NumberedEvent {
            number,
            tick: run_tick,
            event: &event,
        }],
        &IdentitySource {
            reader,
            processes,
            events: &[identity],
        },
    )?;

    Ok(())
}

fn prepare_manifest(
    source: &Source,
    tables: Vec<persistence::TableManifest>,
    output: &mut Option<Manifest>,
) {
    assert!(output.is_none());
    *output = Some(Manifest {
        identity: WindowIdentity {
            run: source.identity,
            window: 0,
        },
        publication_first: 1,
        publication_end: 3,
        events_first: 1,
        events_end: 3,
        wall_ns: Some(TimeBounds {
            min: source.second_first * 1_000_000_000,
            max: (source.second_first + 1) * 1_000_000_000,
        }),
        continuous_ns: Some(TimeBounds {
            min: source::CONTINUOUS_FIRST_NS,
            max: source::CONTINUOUS_FIRST_NS + 1_000_000_000,
        }),
        awake_ns: Some(TimeBounds {
            min: source::AWAKE_FIRST_NS,
            max: source::AWAKE_FIRST_NS + 1_000_000_000,
        }),
        losses: Losses::default(),
        data_directory: String::new(),
        tables,
    });
}
