//! Source-owned reference serialization, independent of encoder identity/numeric mappings.
use std::io::Write;

use collector::ring::{
    IdentityObservation, IdentityUnavailable, ProcessIdentity, ProgramBuffers, RingReader, Tick,
    UsageAvailability,
};
use persistence::{Error, Result};

use super::rows::{self, Row};

pub const PROCESS_COLUMNS: &str = "publication,pid,awake_ns_at_start,user_ns,system_ns,energy_nj,\
    instructions,super_instructions,cycles,idle_wakeups,interrupt_wakeups,pageins,resident_bytes,\
    footprint_bytes,disk_read_bytes,disk_written_bytes,threads,faults,context_switches,\
    mach_syscalls,unix_syscalls,identity_version,identity_availability,usage_availability,\
    process_unique_id,pid_version,executable_name,app_path";
pub const EVENT_COLUMNS: &str = "publication,event,pid,identity_version,identity_availability,\
    usage_availability,process_unique_id,pid_version,executable_name,app_path";

#[derive(Clone, Copy)]
pub struct EventPosition {
    pub publication: u64,
    pub number: u64,
}

pub fn processes(
    writer: &mut impl Write,
    reader: &RingReader,
    publication: u64,
    tick: &Tick,
    identities: &[IdentityObservation],
) -> Result<()> {
    let mut measured_count = 0;
    for identity in identities {
        let mut row = Row::new(writer, "process_identity", publication)?;
        row.signed(i64::from(identity.pid))?;
        if identity.usage == UsageAvailability::Measured {
            let process = tick
                .processes
                .get(measured_count)
                .ok_or(Error::Bound("reference measured process absent"))?;
            if process.pid != identity.pid {
                return Err(Error::Bound("reference process association"));
            }

            rows::process_payload(&mut row, process)?;
            measured_count += 1;
        } else {
            for _ in 0..19 {
                row.null()?;
            }
        }
        fields(&mut row, reader, identity)?;
        row.end()?;
    }
    if measured_count != tick.processes.len() {
        return Err(Error::Bound("reference identity coverage"));
    }

    Ok(())
}

pub fn event(
    writer: &mut impl Write,
    reader: &RingReader,
    position: EventPosition,
    identity: &IdentityObservation,
) -> Result<()> {
    let mut row = Row::new(writer, "event_identity", position.publication)?;
    row.unsigned(position.number)?;
    row.signed(i64::from(identity.pid))?;
    fields(&mut row, reader, identity)?;

    row.end()
}

fn fields<W: Write>(
    row: &mut Row<'_, W>,
    reader: &RingReader,
    identity: &IdentityObservation,
) -> Result<()> {
    // These literals specify the reference contract independently of disk encoder constants.
    row.unsigned(1)?;
    row.unsigned(match identity.identity {
        ProcessIdentity::Measured { .. } => 0,
        ProcessIdentity::Unavailable { reason, .. } => match reason {
            IdentityUnavailable::Refused => 1,
            IdentityUnavailable::Gone => 2,
            IdentityUnavailable::Unsupported => 3,
            IdentityUnavailable::Changed => 4,
            IdentityUnavailable::Invalid => 5,
            IdentityUnavailable::Exhausted => 6,
        },
    })?;
    row.unsigned(match identity.usage {
        UsageAvailability::Measured => 0,
        UsageAvailability::Refused => 1,
        UsageAvailability::Gone => 2,
        UsageAvailability::NotApplicable => 3,
    })?;
    if let Some(creation) = identity.creation() {
        row.unsigned(creation.unique_id)?;
        row.signed(i64::from(creation.pid_version))?;
    } else {
        row.null()?;
        row.null()?;
    }

    fields_program(row, reader, identity)
}

fn fields_program<W: Write>(
    row: &mut Row<'_, W>,
    reader: &RingReader,
    identity: &IdentityObservation,
) -> Result<()> {
    if let Some(program) = identity.program() {
        let mut executable_name = [0; 4095];
        let mut app_path = [0; 4095];
        let lengths = reader.copy_program(
            program,
            &mut ProgramBuffers {
                executable_name: &mut executable_name,
                app_path: &mut app_path,
            },
        )?;
        row.blob(&executable_name[..lengths.0])?;
        if lengths.1 == 0 {
            row.null()?;
        } else {
            row.blob(&app_path[..lengths.1])?;
        }
    } else {
        row.null()?;
        row.null()?;
    }

    Ok(())
}
