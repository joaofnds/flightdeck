//! Typed committed endpoints feed the same owned aggregate as live publications.
use collector::processes::Usage;
use duckdb::{Connection, Row, types::ValueRef};

use super::{
    Frame, IdentityUnavailable, Member, ProcessCreation, ProgramBytes, ProgramSpan, RunIdentity,
};
use crate::apps::{self, Stamp, Unavailable};
use crate::cancellation::Operation;
use persistence::{Error, Result};

pub(crate) struct Endpoint {
    pub identity: RunIdentity,
    pub stamp: Stamp,
    pub run_tick: u64,
    pub wall_ns: Option<u64>,
    pub usage_refused: u32,
    pub usage_gone: u32,
}

impl Frame {
    pub(crate) fn committed(
        endpoint: &Endpoint,
        connection: &Connection,
        source: &str,
        operation: &Operation,
        output: &mut Option<Box<Self>>,
    ) -> Result<()> {
        *output = None;
        operation.check()?;
        assert_eq!(endpoint.stamp.boot, endpoint.identity.boot.0);
        assert_eq!(endpoint.stamp.run, endpoint.identity.run.0);

        let publication = endpoint.stamp.publication;
        let (count, bytes): (u64, u64) = connection.query_row(
            &format!(
                "SELECT count(*), coalesce(sum(coalesce(octet_length(executable_name), 0) + \
                coalesce(octet_length(app_path), 0)), 0)::UBIGINT \
                FROM {source} WHERE publication={publication}"
            ),
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let count = usize::try_from(count).map_err(|_| Error::Bound("committed App count"))?;
        let bytes = usize::try_from(bytes).map_err(|_| Error::Bound("committed App bytes"))?;

        if count > apps::PROCESS_INTERVALS_MAX {
            return Err(Error::Bound("committed App process bound"));
        }

        if bytes > super::PROGRAM_FRAME_BYTES_MAX {
            return Err(Error::Bound("committed App byte bound"));
        }

        let mut frame = Box::new(Self {
            identity: endpoint.identity,
            stamp: endpoint.stamp,
            run_tick: endpoint.run_tick,
            wall_ns: endpoint.wall_ns,
            members: Vec::with_capacity(count),
            starts: Vec::new(),
            exits: Vec::new(),
            arena: Vec::with_capacity(bytes),
            usage_refused: endpoint.usage_refused,
            usage_gone: endpoint.usage_gone,
        });

        committed_members(connection, source, operation, &mut frame)?;
        if frame.members.len() != count {
            return Err(Error::Bound("committed App changed row count"));
        }

        if frame.arena.len() != bytes {
            return Err(Error::Bound("committed App changed byte count"));
        }

        assert!(frame.arena.len() <= super::PROGRAM_FRAME_BYTES_MAX);
        *output = Some(frame);

        Ok(())
    }
}

fn committed_members(
    connection: &Connection,
    source: &str,
    operation: &Operation,
    frame: &mut Frame,
) -> Result<()> {
    let publication = frame.stamp.publication;
    let limit = apps::PROCESS_INTERVALS_MAX + 1;
    let mut statement = connection.prepare(&format!(
        "SELECT pid, process_unique_id, pid_version, identity_availability, \
        executable_name, app_path, usage_availability, awake_ns_at_start, user_ns, system_ns, \
        energy_nj, instructions, super_instructions, cycles, idle_wakeups, interrupt_wakeups, \
        pageins, resident_bytes, footprint_bytes, disk_read_bytes, disk_written_bytes \
        FROM {source} WHERE publication={publication} ORDER BY pid LIMIT {limit}"
    ))?;
    let mut rows = statement.query([])?;
    for index in 0..=apps::PROCESS_INTERVALS_MAX {
        operation.check()?;
        let Some(row) = rows.next()? else {
            return Ok(());
        };

        if index == frame.members.capacity() {
            return Err(Error::Bound("committed App process bound"));
        }

        let pid = required(row, 0)?;
        if frame.members.last().is_some_and(|member| member.pid >= pid) {
            return Err(Error::Bound("duplicate committed App PID"));
        }

        frame.members.push(Member {
            pid,
            creation: committed_creation(row)?,
            program: committed_program(row, &mut frame.arena)?,
            usage: committed_usage(row)?,
        });
    }

    Err(Error::Bound("committed App process bound"))
}

fn required<T: duckdb::types::FromSql>(row: &Row<'_>, index: usize) -> Result<T> {
    row.get::<_, Option<T>>(index)?
        .ok_or(Error::Bound("missing committed App field"))
}

fn committed_creation(row: &Row<'_>) -> Result<Option<ProcessCreation>> {
    match (row.get(1)?, row.get(2)?) {
        (Some(unique_id), Some(pid_version)) => Ok(Some(ProcessCreation {
            unique_id,
            pid_version,
        })),
        (None, None) => Ok(None),
        _ => Err(Error::Bound("committed App creation")),
    }
}

fn committed_program(
    row: &Row<'_>,
    arena: &mut Vec<u8>,
) -> Result<std::result::Result<ProgramSpan, IdentityUnavailable>> {
    let reason = match required::<u8>(row, 3)? {
        0 => None,
        1 => Some(IdentityUnavailable::Refused),
        2 => Some(IdentityUnavailable::Gone),
        3 => Some(IdentityUnavailable::Unsupported),
        4 => Some(IdentityUnavailable::Changed),
        5 => Some(IdentityUnavailable::Invalid),
        6 => Some(IdentityUnavailable::Exhausted),
        _ => return Err(Error::Bound("committed App identity availability")),
    };

    if let Some(reason) = reason {
        return Ok(Err(reason));
    }

    let ValueRef::Blob(name) = row.get_ref(4)? else {
        return Err(Error::Bound("committed App executable bytes"));
    };

    let app_path = match row.get_ref(5)? {
        ValueRef::Null => None,
        ValueRef::Blob(bytes) => Some(bytes),
        _ => return Err(Error::Bound("committed App path bytes")),
    };
    let app_bytes = app_path.map_or(0, <[u8]>::len);

    if arena.len() + name.len() + app_bytes > arena.capacity() {
        return Err(Error::Bound("committed App changed byte count"));
    }

    Ok(Ok(ProgramSpan::capture(
        arena,
        &ProgramBytes {
            executable_name: name,
            app_path,
        },
    )))
}

fn committed_usage(row: &Row<'_>) -> Result<std::result::Result<Usage, Unavailable>> {
    match required::<u8>(row, 6)? {
        0 => Ok(Ok(Usage {
            awake_ns_at_start: required(row, 7)?,
            user_ns: required(row, 8)?,
            system_ns: required(row, 9)?,
            energy_nj: required(row, 10)?,
            instructions: required(row, 11)?,
            super_instructions: required(row, 12)?,
            cycles: required(row, 13)?,
            idle_wakeups: required(row, 14)?,
            interrupt_wakeups: required(row, 15)?,
            pageins: required(row, 16)?,
            resident_bytes: required(row, 17)?,
            footprint_bytes: required(row, 18)?,
            disk_read_bytes: required(row, 19)?,
            disk_written_bytes: required(row, 20)?,
        })),
        1 => Ok(Err(Unavailable::Refused)),
        2 => Ok(Err(Unavailable::Gone)),
        _ => Err(Error::Bound("committed App usage availability")),
    }
}

#[cfg(test)]
mod tests;
