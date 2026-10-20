use super::{App, AppKey, Bytes, Error, Key, KeyKind, Measure, Process, Result, SAMPLE_BYTES_MAX};
use crate::apps;

pub(super) fn validate(aggregate: &apps::Aggregate<'_>) -> Result<()> {
    if aggregate.apps.len() > super::APPS_MAX {
        return Err(Error::Bound("live App count"));
    }
    if aggregate.processes.len() > super::MEMBERS_MAX {
        return Err(Error::Bound("live App member count"));
    }
    let mut bytes = size_of::<super::Sample>()
        + aggregate.apps.len() * size_of::<App>()
        + aggregate.processes.len() * size_of::<Process>();
    for key in aggregate.apps.keys() {
        let raw = key_bytes(*key);
        validate_bytes(raw)?;
        bytes += raw.len() * 5 + app_name(*key).len() * 4;
    }
    for process in &aggregate.processes {
        if let Some(name) = process.executable_name {
            validate_bytes(name)?;
            bytes += name.len() * 5;
        }
    }
    if bytes > SAMPLE_BYTES_MAX {
        return Err(Error::Bound("live App sample bytes"));
    }

    Ok(())
}

pub(super) fn apps(aggregate: &apps::Aggregate<'_>, output: &mut Vec<App>) -> Result<()> {
    for (key, totals) in &aggregate.apps {
        let bytes = key_bytes(*key);
        output.push(App {
            key: Key {
                kind: match key {
                    AppKey::Bundle(_) => KeyKind::Bundle,
                    AppKey::Executable(_) => KeyKind::Executable,
                },
                bytes: bytes.to_vec(),
                display: display(bytes)?,
            },
            display_name: display(app_name(*key))?,
            members: totals.members,
            live_members: totals.live_members,
            membership_complete: totals.membership_complete,
            cpu_ns: measure(&totals.cpu_ns),
            energy_nj: measure(&totals.energy_nj),
            disk_read_bytes: measure(&totals.disk_read_bytes),
            disk_written_bytes: measure(&totals.disk_written_bytes),
            disk_io_bytes: measure(&totals.disk_io_bytes),
            resident_bytes: measure(&totals.resident_bytes),
            footprint_bytes: measure(&totals.footprint_bytes),
        });
    }

    Ok(())
}

pub(super) fn processes(
    aggregate: &apps::Aggregate<'_>,
    apps: &[App],
    output: &mut Vec<Process>,
) -> Result<()> {
    for process in &aggregate.processes {
        output.push(Process {
            pid: process.pid,
            creation: process.creation,
            app_index: process.app.map(|key| {
                let index = apps
                    .binary_search_by(|app| key_view(&app.key).cmp(&key))
                    .unwrap_or_else(|_| unreachable!("aggregate member App exists"));
                u32::try_from(index).unwrap_or_else(|_| unreachable!())
            }),
            executable_name: process
                .executable_name
                .map(|name| -> Result<Bytes> {
                    Ok(Bytes {
                        bytes: name.to_vec(),
                        display: display(name)?,
                    })
                })
                .transpose()?,
            program_unavailable: process.program_unavailable,
            cpu_ns: process.cpu_ns,
            energy_nj: process.energy_nj,
            disk_read_bytes: process.disk_read_bytes,
            disk_written_bytes: process.disk_written_bytes,
            disk_io_bytes: process.disk_io_bytes,
            app_attribution: process.app_attribution,
            resident_bytes: process.resident_bytes_last.map_or(
                apps::Amount::Unavailable(apps::Unavailable::Unknown),
                apps::Amount::Known,
            ),
            footprint_bytes: process.footprint_bytes_last.map_or(
                apps::Amount::Unavailable(apps::Unavailable::Unknown),
                apps::Amount::Known,
            ),
            idle_wakeups: process.idle_wakeups,
            pageins: process.pageins,
            threads: process.threads.map_or(
                apps::Amount::Unavailable(apps::Unavailable::Unknown),
                apps::Amount::Known,
            ),
            state: process.state,
        });
    }

    Ok(())
}

fn measure(source: &apps::Subtotal) -> Measure {
    Measure {
        known: source.known,
        measured_members: source.measured_members,
        unavailable_members: source.unavailable_members,
        overflowed: source.overflowed,
    }
}

fn key_bytes(key: AppKey<'_>) -> &[u8] {
    match key {
        AppKey::Bundle(bytes) | AppKey::Executable(bytes) => bytes,
    }
}

fn validate_bytes(bytes: &[u8]) -> Result<()> {
    if bytes.len() > collector::ring::PROGRAM_PATH_BYTES_MAX {
        return Err(Error::Bound("live App captured bytes"));
    }

    Ok(())
}

fn display(bytes: &[u8]) -> Result<String> {
    validate_bytes(bytes)?;

    let mut text = String::with_capacity(bytes.len() * 4);
    crate::byte_display::append(&mut text, bytes);
    assert!(text.len() <= bytes.len() * 4);

    Ok(text)
}

/// The name an App whose key is `bytes` shows.
pub(super) fn display_name(kind: KeyKind, bytes: &[u8]) -> Result<String> {
    let key = match kind {
        KeyKind::Bundle => AppKey::Bundle(bytes),
        KeyKind::Executable => AppKey::Executable(bytes),
    };

    display(app_name(key))
}

fn key_view(key: &Key) -> AppKey<'_> {
    match key.kind {
        KeyKind::Bundle => AppKey::Bundle(&key.bytes),
        KeyKind::Executable => AppKey::Executable(&key.bytes),
    }
}

fn app_name(key: AppKey<'_>) -> &[u8] {
    match key {
        AppKey::Bundle(bytes) => bytes
            .rsplit(|byte| *byte == b'/')
            .next()
            .unwrap_or_else(|| unreachable!()),
        AppKey::Executable(bytes) => bytes,
    }
}
