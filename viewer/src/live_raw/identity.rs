use collector::ring::{IdentityObservation, ProgramBuffers, RingReader};
use persistence::{Error, Result, schema};

use super::{Operation, PATH_BYTES, ProcessCreation, RAW_BYTES_MAX};

pub(super) struct Identity {
    pub pid: i32,
    pub creation: Option<ProcessCreation>,
    pub availability: u8,
    pub usage: u8,
    pub name: Option<std::ops::Range<usize>>,
    pub app: Option<std::ops::Range<usize>>,
}

impl Identity {
    pub fn capture(
        reader: &RingReader,
        source: &IdentityObservation,
        buffers: &mut ([u8; PATH_BYTES], [u8; PATH_BYTES]),
        arena: &mut Vec<u8>,
    ) -> Result<Self> {
        let mut identity = Self {
            pid: source.pid,
            creation: source.creation().copied(),
            availability: availability(source.availability()),
            usage: usage(source.usage),
            name: None,
            app: None,
        };
        if let Some(reference) = source.program() {
            let (name, app) = copy(reader, reference, buffers)?;
            if arena.len() + name + app > arena.capacity() {
                return Err(Error::Bound("live raw changed identity bytes"));
            }
            let start = arena.len();
            arena.extend_from_slice(&buffers.0[..name]);
            identity.name = Some(start..arena.len());
            let start = arena.len();
            arena.extend_from_slice(&buffers.1[..app]);
            identity.app = (app > 0).then_some(start..arena.len());
        }
        Ok(identity)
    }
}

pub(super) fn preflight(
    reader: &RingReader,
    processes: &[IdentityObservation],
    events: &[IdentityObservation],
    operation: &Operation,
) -> Result<usize> {
    let mut buffers = ([0; PATH_BYTES], [0; PATH_BYTES]);
    let mut bytes = 0_usize;
    for identity in processes.iter().chain(events) {
        operation.check()?;
        if let Some(reference) = identity.program() {
            let (name, app) = copy(reader, reference, &mut buffers)?;
            bytes = bytes
                .checked_add(name + app)
                .ok_or(Error::Bound("live raw identity bytes"))?;
            if bytes > RAW_BYTES_MAX {
                return Err(Error::Bound("live raw identity bytes"));
            }
        }
    }
    Ok(bytes)
}

pub(super) fn capture(
    reader: &RingReader,
    sources: &[IdentityObservation],
    arena: &mut Vec<u8>,
    operation: &Operation,
) -> Result<Vec<Identity>> {
    let mut buffers = ([0; PATH_BYTES], [0; PATH_BYTES]);
    let mut rows = Vec::with_capacity(sources.len());
    for source in sources {
        operation.check()?;
        rows.push(Identity::capture(reader, source, &mut buffers, arena)?);
    }
    Ok(rows)
}

fn copy(
    reader: &RingReader,
    reference: &collector::ring::ProgramReference,
    buffers: &mut ([u8; PATH_BYTES], [u8; PATH_BYTES]),
) -> Result<(usize, usize)> {
    let (name, app) = reader.copy_program(
        reference,
        &mut ProgramBuffers {
            executable_name: &mut buffers.0,
            app_path: &mut buffers.1,
        },
    )?;
    if name > PATH_BYTES {
        return Err(Error::Bound("captured path bytes"));
    }
    if app > PATH_BYTES {
        return Err(Error::Bound("captured path bytes"));
    }
    collector::ring::ProgramBytes {
        executable_name: &buffers.0[..name],
        app_path: (app > 0).then_some(&buffers.1[..app]),
    }
    .validate()
    .map_err(|_| Error::Bound("captured program bytes"))?;
    Ok((name, app))
}

fn availability(value: collector::ring::IdentityAvailability) -> u8 {
    use collector::ring::IdentityAvailability;
    match value {
        IdentityAvailability::Measured => schema::identity_availability::MEASURED,
        IdentityAvailability::Refused => schema::identity_availability::REFUSED,
        IdentityAvailability::Gone => schema::identity_availability::GONE,
        IdentityAvailability::Unsupported => schema::identity_availability::UNSUPPORTED,
        IdentityAvailability::Changed => schema::identity_availability::CHANGED,
        IdentityAvailability::Invalid => schema::identity_availability::INVALID,
        IdentityAvailability::Exhausted => schema::identity_availability::EXHAUSTED,
    }
}

fn usage(value: collector::ring::UsageAvailability) -> u8 {
    use collector::ring::UsageAvailability;
    match value {
        UsageAvailability::Measured => schema::availability::MEASURED,
        UsageAvailability::Refused => schema::availability::REFUSED,
        UsageAvailability::Gone => schema::availability::GONE,
        UsageAvailability::NotApplicable => schema::availability::NOT_APPLICABLE,
    }
}
