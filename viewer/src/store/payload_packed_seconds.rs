//! The seconds of a packed block: each second's header, App rows and App details as the plain
//! second holds them, and its processes read from the block's process table.

use std::sync::Arc;

use super::super::{Payload, ProcessRows};
use super::{Checked, ProcessTable, processes_read_as};
use crate::byte_budget::ByteBudget;
use crate::store::{AppRows, AppsSecond, Missing};

/// Why a block stays plain. Each is a state the data or the machine can bring about, and none
/// is a defect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeptPlain {
    /// The block holds more Apps or processes than a block holds.
    Bound,
    /// The budget refused the packed block.
    Budget,
}

/// The seconds of the block of plain `seconds`, packed, every one rebuilt equal to its plain
/// second, sharing each process's first row with the block before, `before`, where the two
/// read the same, and charged to `budget` before the caller lets go of anything. Or why the
/// block stays plain.
pub(in crate::store) fn pack_seconds(
    seconds: &[Arc<AppsSecond>],
    before: Option<&ProcessTable>,
    budget: &Arc<ByteBudget>,
) -> Result<Vec<Arc<AppsSecond>>, KeptPlain> {
    let table = ProcessTable::pack(seconds, before, budget)?;
    let mut packed = Vec::with_capacity(crate::store::block::BLOCK_SECONDS_MAX);
    for (second_index, plain) in (0..).zip(seconds) {
        packed.push(Arc::new(pack_second(plain, &table, second_index, budget)?));
    }

    // A rebuilt second that differs is a defect of the packer, and nothing the data can cause,
    // since what keeps a block plain is found before the block is built.
    let mut checked = table.checked();
    for (index, (packed, plain)) in packed.iter().zip(seconds).enumerate() {
        assert!(
            second_reads_as(packed, plain, &mut checked),
            "packed second {index} reads otherwise than its plain second"
        );
    }

    Ok(packed)
}

/// Whether the packed `second` reads as its `plain` second, as `==` would find, its processes
/// checked as `processes_read_as` checks them with `checked`.
fn second_reads_as(second: &AppsSecond, plain: &AppsSecond, checked: &mut [Checked]) -> bool {
    // Every field is named, so a field added to a second is one the check has to compare.
    let AppsSecond {
        metadata,
        processes,
        missing,
        observation,
        consecutive,
        elapsed_ns,
        membership_complete,
        ungrouped_members,
        live_ungrouped_members,
        usage_refused,
        usage_gone,
        apps,
        totals,
    } = second;
    let processes_read = match (processes, &plain.processes) {
        (Ok(packed), Ok(plain)) => payload_reads_as(packed, plain, checked),
        (Err(packed), Err(plain)) => packed == plain,
        (Ok(_), Err(_)) | (Err(_), Ok(_)) => false,
    };

    processes_read
        && *metadata == plain.metadata
        && *missing == plain.missing
        && *observation == plain.observation
        && *consecutive == plain.consecutive
        && *elapsed_ns == plain.elapsed_ns
        && *membership_complete == plain.membership_complete
        && *ungrouped_members == plain.ungrouped_members
        && *live_ungrouped_members == plain.live_ungrouped_members
        && *usage_refused == plain.usage_refused
        && *usage_gone == plain.usage_gone
        && *apps == plain.apps
        && *totals == plain.totals
}

/// Whether the packed `payload` reads as the plain `plain`, as `==` would find.
fn payload_reads_as(payload: &Payload, plain: &Payload, checked: &mut [Checked]) -> bool {
    // Every field is named, so a field added to a payload is one the check has to compare. The
    // rows are compared through the readers and the reservation is no reading.
    let Payload {
        selected,
        previous,
        rows: _,
        apps: _,
        reservation: _,
    } = payload;

    *selected == plain.selected
        && *previous == plain.previous
        && payload.apps().eq(plain.apps())
        && processes_read_as(payload, plain, checked)
}

/// The second at `second_index` of a packed block whose process table is `table`, as the plain
/// `second` holds it.
fn pack_second(
    second: &AppsSecond,
    table: &Arc<ProcessTable>,
    second_index: u32,
    budget: &Arc<ByteBudget>,
) -> Result<AppsSecond, KeptPlain> {
    let metadata = second
        .metadata
        .packed(pack_second_copied_bytes(second), budget)
        .ok_or(KeptPlain::Budget)?;
    let processes = match &second.processes {
        Ok(payload) => Ok(Arc::new(pack_payload(
            payload,
            table,
            second_index,
            budget,
        )?)),
        Err(missing) => Err(missing.clone()),
    };

    Ok(AppsSecond {
        metadata,
        processes,
        missing: second.missing.clone(),
        observation: second.observation,
        consecutive: second.consecutive,
        elapsed_ns: second.elapsed_ns,
        membership_complete: second.membership_complete,
        ungrouped_members: second.ungrouped_members,
        live_ungrouped_members: second.live_ungrouped_members,
        usage_refused: second.usage_refused,
        usage_gone: second.usage_gone,
        apps: pack_app_rows(&second.apps, budget)?,
        totals: second.totals,
    })
}

/// The bytes of the strings a packed second copies from `second`: the reason of each refusal.
fn pack_second_copied_bytes(second: &AppsSecond) -> usize {
    [second.missing.as_ref(), second.processes.as_ref().err()]
        .into_iter()
        .flatten()
        .map(|missing| match missing {
            Missing::Refused(reason) => reason.len(),
            Missing::Unread | Missing::TooMany | Missing::Budget => 0,
        })
        .sum()
}

/// A copy of `rows`, charged to `budget` before it is allocated.
fn pack_app_rows(rows: &AppRows, budget: &Arc<ByteBudget>) -> Result<AppRows, KeptPlain> {
    if rows.is_empty() {
        return Ok(AppRows::default());
    }

    let mut copied = AppRows::default();
    copied
        .replace_admitted(rows.len(), budget)
        .map_err(|missing| match missing {
            Missing::Budget => KeptPlain::Budget,
            Missing::TooMany | Missing::Unread | Missing::Refused(_) => {
                unreachable!("a second already holds its App rows")
            }
        })?;
    for row in rows {
        copied.push(row.clone());
    }

    Ok(copied)
}

/// The payload of `payload`'s second as the second at `second_index` of a packed block whose
/// process table is `table` holds it: the same header and App details, and no rows of its own.
fn pack_payload(
    payload: &Payload,
    table: &Arc<ProcessTable>,
    second_index: u32,
    budget: &Arc<ByteBudget>,
) -> Result<Payload, KeptPlain> {
    let reservation = budget
        .reserve(Payload::header_bytes(payload.apps.len()))
        .ok_or(KeptPlain::Budget)?;
    let mut apps = Vec::with_capacity(payload.apps.len());
    apps.extend(payload.apps.iter().map(Arc::clone));

    Ok(Payload {
        selected: payload.selected,
        previous: payload.previous,
        rows: ProcessRows::Packed {
            table: Arc::clone(table),
            second_index,
            count: payload.len(),
        },
        apps,
        reservation,
    })
}
