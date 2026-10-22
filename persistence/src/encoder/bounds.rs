use crate::owned_identity::Programs;
use crate::schema::Table;
use crate::window::StagedTick;
use crate::{Error, Result, Window};

const FRAME_BYTES_MAX: usize = 512 * 1024 * 1024;
const ROW_REFERENCES_BYTES_MAX: usize = 40;
const VALIDITY_BYTES_PER_ROW_MAX: usize = 8;
const SORT_INDEX_BYTES_MAX: usize = 8;

pub(super) fn frame(table: Table, window: &Window) -> Result<()> {
    // A system column holds at most a 16-byte string view, beside the text it points to.
    let width = match table {
        Table::Ticks => 224 + crate::schema::SYSTEM_FIELDS.len() * 16,
        Table::Memory => 153,
        Table::Cpus | Table::CoreStates => 28,
        Table::Interfaces => 82,
        Table::GpuStates => 20,
        Table::Fans => 16,
        Table::Processes | Table::Events => 200,
        Table::Coalitions => 88,
    };
    let overhead = ROW_REFERENCES_BYTES_MAX + VALIDITY_BYTES_PER_ROW_MAX + SORT_INDEX_BYTES_MAX;
    let fixed = rows(table, window)
        .checked_mul(width + overhead)
        .ok_or(Error::Bound("encoding frame bytes"))?;
    if fixed > FRAME_BYTES_MAX {
        return Err(Error::Bound("encoding frame bytes"));
    }

    let mut bytes = fixed;
    for tick in &window.ticks {
        let extra = frame_variable(table, tick, window.programs.as_deref())?;
        bytes = bytes
            .checked_add(extra)
            .ok_or(Error::Bound("encoding frame bytes"))?;
        if bytes > FRAME_BYTES_MAX {
            return Err(Error::Bound("encoding frame bytes"));
        }
    }

    Ok(())
}

pub(super) fn rows(table: Table, window: &Window) -> usize {
    window
        .ticks
        .iter()
        .map(|tick| match table {
            Table::Ticks | Table::Memory => 1,
            Table::Cpus => tick.cpus.len(),
            Table::Interfaces => tick.interfaces.len(),
            Table::GpuStates => {
                if tick.ioreport_read {
                    tick.gpu_active_ns.len()
                } else {
                    0
                }
            }
            Table::CoreStates => {
                if tick.ioreport_read {
                    tick.cores.iter().map(Vec::len).sum()
                } else {
                    0
                }
            }
            Table::Fans => {
                if tick.smc_read {
                    tick.fan_rpm.len()
                } else {
                    0
                }
            }
            Table::Processes => tick
                .identities
                .as_ref()
                .map_or(tick.processes.len(), |identity| identity.processes.len()),
            Table::Coalitions => tick.coalitions.len(),
            Table::Events => tick.events.len(),
        })
        .sum()
}

/// The bytes of the machine's fact texts a tick holds.
fn frame_variable_system(tick: &StagedTick) -> usize {
    let Some(system) = tick.system.as_ref() else {
        return 0;
    };

    let machine = &system.machine;
    let mut texts = [
        machine.model,
        machine.chip,
        machine.architecture,
        None,
        None,
    ];
    if let Some(levels) = machine.levels {
        texts[3] = Some(levels[0].name);
        texts[4] = Some(levels[1].name);
    }
    texts.iter().flatten().map(|text| text.as_str().len()).sum()
}

fn frame_variable(table: Table, tick: &StagedTick, programs: Option<&Programs>) -> Result<usize> {
    match table {
        Table::Ticks => {
            let disks = tick
                .disks
                .as_ref()
                .map(crate::disks::encode)
                .transpose()?
                .map_or(0, |text| text.len());
            Ok(disks + frame_variable_system(tick))
        }
        Table::Interfaces => Ok(tick
            .interfaces
            .iter()
            .map(|value| value.name.as_str().len())
            .sum()),
        Table::Processes | Table::Events => {
            let Some(identities) = &tick.identities else {
                return Ok(0);
            };
            let Some(programs) = programs else {
                unreachable!("window identity rows have a program dictionary");
            };
            let identities = if matches!(table, Table::Processes) {
                &identities.processes
            } else {
                &identities.events
            };
            let mut bytes = 0_usize;
            for identity in identities {
                if let Some(index) = identity.program {
                    let added =
                        programs.name(index).len() + programs.app(index).map_or(0, <[u8]>::len);
                    bytes = bytes
                        .checked_add(added)
                        .ok_or(Error::Bound("encoding identity bytes"))?;
                    if bytes > FRAME_BYTES_MAX {
                        return Err(Error::Bound("encoding identity bytes"));
                    }
                }
            }

            Ok(bytes)
        }
        _ => Ok(0),
    }
}
