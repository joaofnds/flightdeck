//! Typed optional columns share the original measured payload without numeric conversion.
use duckdb::types::{ToSqlOutput, ValueRef};

use crate::owned_identity::{OwnedIdentity, Programs};
use crate::window::{IdentityRows, StagedTick};
use crate::{Result, schema};

pub(crate) fn append_processes(
    appender: &mut duckdb::Appender<'_>,
    tick: &StagedTick,
    identities: &IdentityRows,
    programs: &Programs,
) -> Result<()> {
    let mut measured = 0;
    for identity in &identities.processes {
        let mut row = [ValueRef::Null; 28];
        row[0] = ValueRef::UBigInt(tick.observation.publication);
        row[1] = ValueRef::Int(identity.pid);
        if identity.usage == schema::availability::MEASURED {
            let process = &tick.processes[measured];
            assert_eq!(process.pid, identity.pid);
            usage_fields(&process.usage, &mut row[2..16]);
            row[16] = ValueRef::Int(process.task.threads);
            row[17] = ValueRef::Int(process.task.faults);
            row[18] = ValueRef::Int(process.task.context_switches);
            row[19] = ValueRef::Int(process.task.mach_syscalls);
            row[20] = ValueRef::Int(process.task.unix_syscalls);
            measured += 1;
        }
        identity_fields(identity, programs, &mut row[21..]);

        appender.append_row(duckdb::appender_params_from_iter(
            row.iter().copied().map(ToSqlOutput::Borrowed),
        ))?;
    }

    assert_eq!(measured, tick.processes.len());

    Ok(())
}

pub(crate) fn append_events(
    appender: &mut duckdb::Appender<'_>,
    tick: &StagedTick,
    identities: &IdentityRows,
    programs: &Programs,
) -> Result<()> {
    assert_eq!(tick.events.len(), identities.events.len());

    for (event, identity) in tick.events.iter().zip(&identities.events) {
        assert_eq!(event.pid, identity.pid);
        assert_eq!(event.availability, identity.usage);

        let mut row = [ValueRef::Null; 29];
        row[0] = ValueRef::UBigInt(tick.observation.publication);
        row[1] = ValueRef::UBigInt(event.number);
        row[2] = ValueRef::UBigInt(event.tick);
        row[3] = ValueRef::Int(event.pid);
        row[4] = ValueRef::UTinyInt(event.kind);
        row[5] = event.coalition.map_or(ValueRef::Null, ValueRef::UBigInt);
        row[6] = ValueRef::UTinyInt(event.availability);
        row[7] = event.noticed.map_or(ValueRef::Null, ValueRef::UTinyInt);
        if let Some(usage) = &event.usage {
            usage_fields(usage, &mut row[8..22]);
        }
        identity_fields(identity, programs, &mut row[22..]);

        appender.append_row(duckdb::appender_params_from_iter(
            row.iter().copied().map(ToSqlOutput::Borrowed),
        ))?;
    }

    Ok(())
}

fn identity_fields<'program>(
    identity: &OwnedIdentity,
    programs: &'program Programs,
    fields: &mut [ValueRef<'program>],
) {
    assert_eq!(fields.len(), 7);
    fields[0] = ValueRef::UTinyInt(schema::IDENTITY_VERSION);
    fields[1] = ValueRef::UTinyInt(identity.availability);
    fields[2] = ValueRef::UTinyInt(identity.usage);
    if let Some(creation) = identity.creation {
        fields[3] = ValueRef::UBigInt(creation.unique_id);
        fields[4] = ValueRef::Int(creation.pid_version);
    }
    if let Some(index) = identity.program {
        fields[5] = ValueRef::Blob(programs.name(index));
        fields[6] = programs.app(index).map_or(ValueRef::Null, ValueRef::Blob);
    }
}

fn usage_fields(usage: &collector::processes::Usage, fields: &mut [ValueRef<'_>]) {
    assert_eq!(fields.len(), 14);
    fields[0] = ValueRef::UBigInt(usage.awake_ns_at_start);
    fields[1] = ValueRef::UBigInt(usage.user_ns);
    fields[2] = ValueRef::UBigInt(usage.system_ns);
    fields[3] = ValueRef::UBigInt(usage.energy_nj);
    fields[4] = ValueRef::UBigInt(usage.instructions);
    fields[5] = ValueRef::UBigInt(usage.super_instructions);
    fields[6] = ValueRef::UBigInt(usage.cycles);
    fields[7] = ValueRef::UBigInt(usage.idle_wakeups);
    fields[8] = ValueRef::UBigInt(usage.interrupt_wakeups);
    fields[9] = ValueRef::UBigInt(usage.pageins);
    fields[10] = ValueRef::UBigInt(usage.resident_bytes);
    fields[11] = ValueRef::UBigInt(usage.footprint_bytes);
    fields[12] = ValueRef::UBigInt(usage.disk_read_bytes);
    fields[13] = ValueRef::UBigInt(usage.disk_written_bytes);
}
