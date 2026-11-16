//! The seconds of a packed block: each second's header, App rows and App details as the plain
//! second holds them, and its processes read from the block's table.

use std::sync::Arc;

use super::super::{Payload, ProcessRows};
use super::{AppRowsShare, BlockTable, Checked, processes_read_as};
use crate::byte_budget::ByteBudget;
use crate::store::{AppRow, AppRows, AppsSecond, Missing};

/// Why a block stays plain. Each is a state the data or the machine can bring about, and none
/// is a defect.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeptPlain {
    /// The block holds more Apps or processes than a block holds.
    Bound,
    /// The budget refused the packed block.
    Budget,
}

/// The seconds of the block of plain `seconds`, packed, and the table that holds their Apps and
/// processes, every second read through the table equal to its plain second, sharing each
/// process's first row with the block before, `before`, where the two read the same, its App rows
/// packed only where `share` admits them, and charged to `budget` before the caller lets go of
/// anything. Or why the block stays plain.
pub(in crate::store) fn pack_seconds(
    seconds: &[Arc<AppsSecond>],
    before: Option<&BlockTable>,
    budget: &Arc<ByteBudget>,
    share: AppRowsShare,
) -> Result<(Vec<Arc<AppsSecond>>, Arc<BlockTable>), KeptPlain> {
    let table = BlockTable::pack(seconds, before, budget, share)?;
    let mut packed = Vec::with_capacity(crate::store::block::BLOCK_SECONDS_MAX);
    for (second_index, plain) in (0..).zip(seconds) {
        packed.push(Arc::new(pack_second(plain, &table, second_index, budget)?));
    }

    // A rebuilt second that differs is a defect of the packer, and nothing the data can cause,
    // since what keeps a block plain is found before the block is built. A second's Apps are
    // read both ways a reader reads whole seconds, walked from the second before through the
    // second's steps and rebuilt from the first rows and their changes over the rows of the
    // second before, since the two are held apart and a defect in one does not show through the
    // other. Every other read of a packed App is built on the rebuild's reading of one App, but
    // each App over the block's seconds, which is read on its own after the seconds.
    let mut checked = table.checked();
    let mut walked = AppRowsRead::default();
    let mut rebuilt = AppRowsRead::default();
    for (index, (packed, plain)) in (0..).zip(packed.iter().zip(seconds)) {
        let reads_as = if table.app_rows_packed() {
            if index == 0 {
                walked.rebuild(&table, index);
            } else {
                table.step_app_rows(index, &mut walked.rows, &mut walked.places);
            }
            rebuilt.rebuild(&table, index);
            let apps_read = [walked.rows.as_slice(), rebuilt.rows.as_slice()];
            // The table holds the second's Apps, so the second holds no row of its own.
            packed.apps.is_empty() && second_reads_as(packed, plain, &apps_read, &mut checked)
        } else {
            second_reads_as(packed, plain, &[packed.apps.as_slice()], &mut checked)
        };
        assert!(
            reads_as,
            "packed second {index} reads otherwise than its plain second"
        );
    }
    if table.app_rows_packed() {
        assert!(
            pack_seconds_apps_over(&table, seconds),
            "a packed App over its block's seconds reads otherwise than its plain seconds"
        );
    }

    Ok((packed, table))
}

/// Whether each App of the block of `table`, read over the block's seconds as the readers that
/// sum over a block read it, reads as the plain `seconds` the block was built from.
pub(super) fn pack_seconds_apps_over(table: &BlockTable, seconds: &[Arc<AppsSecond>]) -> bool {
    // The index the next App held at each second takes among that second's Apps, which are in
    // key order, the order of their places.
    let mut index_at = [0_usize; crate::store::block::BLOCK_SECONDS_MAX];
    let mut reads_as = true;
    for place in 0..table.block_app_count() {
        table.app_rows_over(place, u64::MAX, |second_index, row| {
            let second_index = collector::usize_from_u32(second_index);
            let index = &mut index_at[second_index];
            reads_as &= seconds[second_index].apps.get(*index) == Some(row);
            *index += 1;
        });
    }

    reads_as
        && seconds
            .iter()
            .zip(index_at)
            .all(|(second, index)| second.apps.len() == index)
}

/// The rows of a packed second's Apps as one way of reading them leaves them.
#[derive(Default)]
struct AppRowsRead {
    rows: Vec<AppRow>,
    places: Vec<u32>,
    spare: Vec<AppRow>,
}

impl AppRowsRead {
    /// Rebuilds the rows as the Apps of the second at `second_index` of the block of `table`.
    fn rebuild(&mut self, table: &BlockTable, second_index: u32) {
        table.rebuild_app_rows(
            second_index,
            &mut self.rows,
            &mut self.places,
            &mut self.spare,
        );
    }
}

/// Whether the packed `second` reads as its `plain` second, as `==` would find, its Apps read
/// as each of `apps_read`, and its processes checked as `processes_read_as` checks them with
/// `checked`.
pub(super) fn second_reads_as(
    second: &AppsSecond,
    plain: &AppsSecond,
    apps_read: &[&[AppRow]],
    checked: &mut [Checked],
) -> bool {
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
        apps: _,
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
        && apps_read.iter().all(|read| **read == *plain.apps)
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

/// The second at `second_index` of a packed block whose block table is `table`, as the plain
/// `second` holds it.
pub(super) fn pack_second(
    second: &AppsSecond,
    table: &Arc<BlockTable>,
    second_index: u32,
    budget: &Arc<ByteBudget>,
) -> Result<AppsSecond, KeptPlain> {
    let metadata = second
        .metadata
        .packed(pack_second_copied_bytes(second), budget)
        .ok_or(KeptPlain::Budget)?;
    // The second's Apps and their details are read from the block's table where it packs them.
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
        apps: if table.app_rows_packed() {
            AppRows::default()
        } else {
            pack_second_app_rows(&second.apps, budget)?
        },
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
fn pack_second_app_rows(rows: &AppRows, budget: &Arc<ByteBudget>) -> Result<AppRows, KeptPlain> {
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
/// table is `table` holds it: the same header, no process rows of its own, and its App details
/// only where the table does not hold them.
fn pack_payload(
    payload: &Payload,
    table: &Arc<BlockTable>,
    second_index: u32,
    budget: &Arc<ByteBudget>,
) -> Result<Payload, KeptPlain> {
    let app_count = if table.app_rows_packed() {
        0
    } else {
        payload.apps.len()
    };
    let reservation = budget
        .reserve(Payload::header_bytes(app_count))
        .ok_or(KeptPlain::Budget)?;
    let mut apps = Vec::with_capacity(app_count);
    apps.extend(payload.apps.iter().take(app_count).map(Arc::clone));

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
