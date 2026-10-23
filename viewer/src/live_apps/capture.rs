use collector::ring::{
    IdentityUnavailable, PROGRAM_PATH_BYTES_MAX, ProcessIdentity, ProgramBuffers, UsageAvailability,
};

use super::{
    Error, Frame, Member, PROGRAM_FRAME_BYTES_MAX, ProgramBytes, ProgramSpan, Publication, Result,
};
use crate::apps::Unavailable;

pub(super) fn program_bytes(
    source: &Publication<'_>,
    identities: &[collector::ring::IdentityObservation],
) -> Result<usize> {
    let mut size = 0_usize;
    let mut executable_name = [0; PROGRAM_PATH_BYTES_MAX];
    let mut app_path = [0; PROGRAM_PATH_BYTES_MAX];
    for identity in identities {
        if let Some(program) = identity.program() {
            let (name, app) = source.reader.copy_program(
                program,
                &mut ProgramBuffers {
                    executable_name: &mut executable_name,
                    app_path: &mut app_path,
                },
            )?;
            size = size
                .checked_add(name)
                .and_then(|size| size.checked_add(app))
                .ok_or(Error::Bound("live App byte bound"))?;
            if size > PROGRAM_FRAME_BYTES_MAX {
                return Err(Error::Bound("live App byte bound"));
            }
        }
    }

    Ok(size)
}

pub(super) fn members(source: &Publication<'_>, frame: &mut Frame) -> Result<()> {
    if source.reader.dictionary_capacity().is_none() {
        members_unidentified(source, frame);
        return Ok(());
    }

    let gpu_ns = source.tick.process_gpu_ns();
    if !gpu_ns.is_empty() {
        if gpu_ns.len() != source.identities.len() {
            return Err(Error::Bound("live App GPU time association"));
        }
    }

    let mut measured = 0;
    let mut previous_pid = None;
    let mut executable_name = [0; PROGRAM_PATH_BYTES_MAX];
    let mut app_path = [0; PROGRAM_PATH_BYTES_MAX];
    for (index, identity) in source.identities.iter().enumerate() {
        if previous_pid.is_some_and(|previous| previous >= identity.pid) {
            return Err(Error::Bound("live App identity order"));
        }
        previous_pid = Some(identity.pid);
        let mut threads = None;
        let usage = match identity.usage {
            UsageAvailability::Measured => {
                let process = source
                    .tick
                    .processes
                    .get(measured)
                    .ok_or(Error::Bound("live App missing numeric row"))?;
                if process.pid != identity.pid {
                    return Err(Error::Bound("live App numeric association"));
                }
                threads = super::threads(process.task.threads);
                measured += 1;
                Ok(process.usage)
            }
            UsageAvailability::Refused => Err(Unavailable::Refused),
            UsageAvailability::Gone => Err(Unavailable::Gone),
            UsageAvailability::NotApplicable => {
                return Err(Error::Bound("live App process usage availability"));
            }
        };
        frame.members.push(Member {
            pid: identity.pid,
            creation: identity.creation().copied(),
            program: Err(IdentityUnavailable::Unsupported),
            usage,
            gpu_ns: gpu_ns.get(index).copied().flatten(),
            threads,
        });
        let member = frame.members.last_mut().unwrap_or_else(|| unreachable!());
        member_program(
            source,
            &identity.identity,
            &mut ProgramBuffers {
                executable_name: &mut executable_name,
                app_path: &mut app_path,
            },
            &mut frame.arena,
            &mut member.program,
        )?;
    }
    if measured != source.tick.processes.len() {
        return Err(Error::Bound("live App unmatched numeric rows"));
    }

    Ok(())
}

/// The members of a ring that records no identities, each with its usage and no program.
fn members_unidentified(source: &Publication<'_>, frame: &mut Frame) {
    for process in &source.tick.processes {
        frame.members.push(Member {
            pid: process.pid,
            creation: None,
            program: Err(IdentityUnavailable::Unsupported),
            usage: Ok(process.usage),
            gpu_ns: None,
            threads: super::threads(process.task.threads),
        });
    }
}

/// Copies the program into `arena` through `buffers`. A caller passes one pair of buffers for
/// all the programs of a tick, since zeroing a pair takes longer than copying a program into it.
pub(super) fn member_program(
    source: &Publication<'_>,
    identity: &ProcessIdentity,
    buffers: &mut ProgramBuffers,
    arena: &mut Vec<u8>,
    output: &mut std::result::Result<ProgramSpan, IdentityUnavailable>,
) -> Result<()> {
    match identity {
        ProcessIdentity::Measured { program, .. } => {
            let (name, app) = source.reader.copy_program(program, buffers)?;
            if arena.len() + name + app > arena.capacity() {
                return Err(Error::Bound("live App changed byte size"));
            }

            *output = Ok(ProgramSpan::capture(
                arena,
                &ProgramBytes {
                    executable_name: &buffers.executable_name[..name],
                    app_path: if app == 0 {
                        None
                    } else {
                        Some(&buffers.app_path[..app])
                    },
                },
            ));
        }
        ProcessIdentity::Unavailable { reason, .. } => *output = Err(*reason),
    }

    Ok(())
}
