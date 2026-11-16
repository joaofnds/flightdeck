//! The process rows of a packed block: each process's first row and the changes of its fields,
//! read at any second of the block, and the list of the block's Apps whose places the processes
//! name their Apps by.

use std::cmp::Ordering;
use std::sync::Arc;

use super::{CompactAmount, Measured, ProcessAt, ProcessName, ProcessRow, Reading, Shared};
use crate::apps::{PROCESS_INTERVALS_MAX, ProcessState, Unavailable};
use crate::byte_budget::{ByteBudget, ByteReservation};
use crate::store::block::block_u32;
use crate::store::{APPS_PER_SECOND_MAX, AppRow, AppsSecond};
use collector::ring::{IdentityUnavailable, ProcessCreation};

#[path = "payload_packed_apps.rs"]
mod apps;
#[cfg(test)]
pub(in crate::store) use apps::APP_ROWS_PACKED_ALWAYS;
#[cfg(test)]
pub(in crate::store) use apps::AppRowsBytes;
pub(in crate::store) use apps::{APP_ROWS_PACKED_SHARE, AppRowsShare, PackedDetails};
use apps::{AppParts, AppStep, BlockApp};

#[path = "payload_packed_seconds.rs"]
mod seconds;
pub use seconds::KeptPlain;
pub(in crate::store) use seconds::pack_seconds;

#[cfg(test)]
#[path = "payload_packed_tests.rs"]
pub(in crate::store) mod tests;

/// The seconds a packed block holds at most, one bit of a mask each.
const SECONDS_MAX: usize = 64;

/// The App place of a process that belongs to no App, a place no App of a block takes.
const PLACE_NONE: u32 = u32::MAX;

/// The executable name word of a process that has none, a place no name of a block takes.
const NAME_NONE: u64 = u64::MAX;

/// The fields of a process that may change inside a block, each one bit of a process's mask
/// of changing fields and one word of each changed value. The pid and the creation's unique id
/// name the process, so they never change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    PidVersion,
    ExecutableName,
    ProgramUnavailable,
    Known(Measured),
    Unavailable(Measured),
    AppAttribution,
    State,
    AppPlace,
}

impl Field {
    const ALL: [Self; FIELD_COUNT] = [
        Self::PidVersion,
        Self::ExecutableName,
        Self::ProgramUnavailable,
        Self::Known(Measured::Cpu),
        Self::Known(Measured::Energy),
        Self::Known(Measured::Gpu),
        Self::Known(Measured::DiskRead),
        Self::Known(Measured::DiskWritten),
        Self::Known(Measured::DiskIo),
        Self::Known(Measured::Resident),
        Self::Known(Measured::Footprint),
        Self::Known(Measured::IdleWakeups),
        Self::Known(Measured::Pageins),
        Self::Known(Measured::Threads),
        Self::Unavailable(Measured::Cpu),
        Self::Unavailable(Measured::Energy),
        Self::Unavailable(Measured::Gpu),
        Self::Unavailable(Measured::DiskRead),
        Self::Unavailable(Measured::DiskWritten),
        Self::Unavailable(Measured::DiskIo),
        Self::Unavailable(Measured::Resident),
        Self::Unavailable(Measured::Footprint),
        Self::Unavailable(Measured::IdleWakeups),
        Self::Unavailable(Measured::Pageins),
        Self::Unavailable(Measured::Threads),
        Self::AppAttribution,
        Self::State,
        Self::AppPlace,
    ];

    /// The field's place in `ALL`. The match names every field and the assertion after
    /// `FIELD_COUNT` ties each place to `ALL`, so a field added here is one `ALL` has to list.
    const fn index(self) -> usize {
        const MEASURED_COUNT: usize = Measured::ALL.len();
        match self {
            Self::PidVersion => 0,
            Self::ExecutableName => 1,
            Self::ProgramUnavailable => 2,
            Self::Known(measured) => 3 + measured.index(),
            Self::Unavailable(measured) => 3 + MEASURED_COUNT + measured.index(),
            Self::AppAttribution => 3 + 2 * MEASURED_COUNT,
            Self::State => 4 + 2 * MEASURED_COUNT,
            Self::AppPlace => 5 + 2 * MEASURED_COUNT,
        }
    }

    /// The field's bit in a process's mask of changing fields, its place in `ALL`.
    fn bit(self) -> u32 {
        1 << self.index()
    }
}

/// The fields of a process that may change inside a block, the place of the last one plus one.
const FIELD_COUNT: usize = Field::AppPlace.index() + 1;

const _: () = {
    // Compared as powers of two, since a constant changes integer type only through `as`.
    assert!(1_u128 << SECONDS_MAX == 1_u128 << u64::BITS);
    assert!(crate::store::block::BLOCK_SECONDS_MAX <= SECONDS_MAX);
    assert!(1_u128 << FIELD_COUNT <= 1_u128 << u32::BITS);
    // Both bounds below are `u32::MAX`, the value of `PLACE_NONE`, written as a shift.
    assert!(PLACE_NONE == u32::MAX);
    assert!(APPS_PER_SECOND_MAX < (1 << u32::BITS) - 1);
    assert!(PROCESS_INTERVALS_MAX * FIELD_COUNT * SECONDS_MAX < (1 << u32::BITS) - 1);
    assert!(size_of::<PackedProcess>() <= 32);
    let mut index = 0;
    while index < FIELD_COUNT {
        assert!(Field::ALL[index].index() == index);
        index += 1;
    }
};

/// The process rows of one packed block.
#[derive(Debug)]
pub(in crate::store) struct BlockTable {
    /// The block's Apps in key order, each with the seconds that hold it, its first row and
    /// details and where the changes of their words start.
    apps: Box<[BlockApp]>,
    /// For each second, how many Apps it holds.
    app_counts: Box<[u32]>,
    /// For each second, where its steps start in `steps`, and after the last the end of its
    /// steps.
    step_starts: Box<[u32]>,
    /// The changes to the rows of the Apps of each second from the second before, in the order of
    /// the seconds and in each second in App place order.
    steps: Box<[AppStep]>,
    /// The seconds of the block that hold their process rows, and so their App details.
    detailed_seconds: u64,
    /// Whether the table holds its Apps' rows and details. Where packing them would not pay,
    /// each second holds its own, as a plain second does, and the table only places the Apps.
    app_rows_packed: bool,
    /// The bytes the block's Apps take in each form, which decided `app_rows_packed`, kept for
    /// a test and the benchmark that read them back.
    #[cfg(test)]
    app_rows_bytes: AppRowsBytes,
    /// The block's processes in the order of its seconds, which is the order of pid and then
    /// the creation's unique id.
    processes: Box<[PackedProcess]>,
    /// For each changing field of each process, the seconds at which it differs from the
    /// second before that holds the process.
    change_seconds: Box<[u64]>,
    /// For each changing field of each process, where its changed values start in `values`.
    change_starts: Box<[u32]>,
    values: Box<[u64]>,
    /// The executable names a changed value names by place.
    names: Box<[Arc<ProcessName>]>,
    /// For each App place, and after the last one for no App, where its processes start in
    /// `member_places`, and after those the end of the last list.
    member_starts: Box<[u32]>,
    /// For each App place, the places of the processes under it at any second of the block, and
    /// after them those outside any App at any second, each list in the block's process order,
    /// so one App's members, or those of no App, are found without walking the rest.
    member_places: Box<[u32]>,
    reservation: ByteReservation,
}

#[derive(Debug)]
struct PackedProcess {
    /// The seconds of the block that hold the process.
    seconds: u64,
    /// The fields that change inside the block, by `Field::bit`.
    changing: u32,
    /// The change record of the process's first changing field.
    changes_start: u32,
    /// The process's row at the first second that holds it.
    first: Arc<Shared<ProcessRow>>,
    first_app_place: u32,
}

/// One process at one second of a packed block.
#[derive(Clone, Copy, Debug)]
pub(in crate::store) struct PackedAt<'table> {
    table: &'table BlockTable,
    process: &'table PackedProcess,
    second_index: u32,
}

/// Every process of one second of a packed block, in the block's order, each with its App
/// index at that second.
#[derive(Debug)]
pub(in crate::store) struct Places<'table> {
    table: &'table BlockTable,
    processes: std::slice::Iter<'table, PackedProcess>,
    second_index: u32,
    remaining: usize,
    /// The App index at the second of each App place of the block.
    app_indexes: Vec<u32>,
}

/// The processes of one second of a packed block whose App is the App at one place, or that
/// belong to no App.
#[derive(Clone, Debug)]
pub(in crate::store) struct Members<'table> {
    table: &'table BlockTable,
    /// The places of the processes ever under that App, or ever outside any App.
    places: std::slice::Iter<'table, u32>,
    second_index: u32,
    app_place: u32,
}

/// The changes of one process in a block, for each field in the order of `Field::ALL` the
/// second of each change, oldest first, and the field's word from it on.
type FieldChanges = [Vec<(usize, u64)>; FIELD_COUNT];

/// What names a process inside a block: its pid and its creation's unique id, by which every
/// second lists its processes in order.
type ProcessKey = (i32, Option<u64>);

/// The word of each field of `row`, whose App is at `app_place` of its block, at the field's
/// place in `Field::ALL`. An executable name is compared by its allocation, so two names of
/// the same bytes count as a change, which reads the same.
fn pack_process_words(row: &ProcessRow, app_place: u32) -> [u64; FIELD_COUNT] {
    // Every field is named, so a field added to a row or a creation is one the packer has to
    // record. The pid and the creation's unique id name the process, so they never change.
    let ProcessRow {
        pid: _,
        creation,
        executable_name,
        program_unavailable,
        cpu_ns,
        energy_nj,
        gpu_ns,
        disk_read_bytes,
        disk_written_bytes,
        disk_io_bytes,
        resident_bytes,
        footprint_bytes,
        idle_wakeups,
        pageins,
        threads,
        app_attribution,
        state,
    } = row;
    let mut words = [0; FIELD_COUNT];
    words[Field::PidVersion.index()] = creation.map_or(
        0,
        |ProcessCreation {
             unique_id: _,
             pid_version,
         }| u64::from(u32::from_ne_bytes(pid_version.to_ne_bytes())),
    );
    words[Field::ExecutableName.index()] = executable_name.as_ref().map_or(NAME_NONE, |name| {
        collector::u64_from_usize(Arc::as_ptr(name).addr())
    });
    words[Field::ProgramUnavailable.index()] = pack_process_words_identity(*program_unavailable);
    for (measured, amount) in [
        (Measured::Cpu, cpu_ns),
        (Measured::Energy, energy_nj),
        (Measured::Gpu, gpu_ns),
        (Measured::DiskRead, disk_read_bytes),
        (Measured::DiskWritten, disk_written_bytes),
        (Measured::DiskIo, disk_io_bytes),
        (Measured::Resident, resident_bytes),
        (Measured::Footprint, footprint_bytes),
        (Measured::IdleWakeups, idle_wakeups),
        (Measured::Pageins, pageins),
        (Measured::Threads, threads),
    ] {
        words[Field::Known(measured).index()] = u64::from_le_bytes(amount.known);
        words[Field::Unavailable(measured).index()] =
            pack_process_words_unavailable(amount.unavailable);
    }
    words[Field::AppAttribution.index()] = pack_process_words_unavailable(*app_attribution);
    words[Field::State.index()] = pack_process_words_state(*state);
    words[Field::AppPlace.index()] = u64::from(app_place);

    words
}

fn pack_process_words_unavailable(unavailable: Option<Unavailable>) -> u64 {
    match unavailable {
        None => 0,
        Some(Unavailable::Refused) => 1,
        Some(Unavailable::Gone) => 2,
        Some(Unavailable::Unknown) => 3,
        Some(Unavailable::Interval) => 4,
        Some(Unavailable::Creation) => 5,
        Some(Unavailable::Baseline) => 6,
        Some(Unavailable::CounterReset) => 7,
        Some(Unavailable::Overflow) => 8,
        Some(Unavailable::Exec) => 9,
        Some(Unavailable::Program) => 10,
        Some(Unavailable::ExitUnverified) => 11,
    }
}

fn unavailable_of(word: u64) -> Option<Unavailable> {
    match word {
        0 => None,
        1 => Some(Unavailable::Refused),
        2 => Some(Unavailable::Gone),
        3 => Some(Unavailable::Unknown),
        4 => Some(Unavailable::Interval),
        5 => Some(Unavailable::Creation),
        6 => Some(Unavailable::Baseline),
        7 => Some(Unavailable::CounterReset),
        8 => Some(Unavailable::Overflow),
        9 => Some(Unavailable::Exec),
        10 => Some(Unavailable::Program),
        11 => Some(Unavailable::ExitUnverified),
        _ => unreachable!("a packed block holds only the words it wrote"),
    }
}

fn pack_process_words_identity(unavailable: Option<IdentityUnavailable>) -> u64 {
    match unavailable {
        None => 0,
        Some(IdentityUnavailable::Refused) => 1,
        Some(IdentityUnavailable::Gone) => 2,
        Some(IdentityUnavailable::Unsupported) => 3,
        Some(IdentityUnavailable::Changed) => 4,
        Some(IdentityUnavailable::Invalid) => 5,
        Some(IdentityUnavailable::Exhausted) => 6,
    }
}

fn identity_of(word: u64) -> Option<IdentityUnavailable> {
    match word {
        0 => None,
        1 => Some(IdentityUnavailable::Refused),
        2 => Some(IdentityUnavailable::Gone),
        3 => Some(IdentityUnavailable::Unsupported),
        4 => Some(IdentityUnavailable::Changed),
        5 => Some(IdentityUnavailable::Invalid),
        6 => Some(IdentityUnavailable::Exhausted),
        _ => unreachable!("a packed block holds only the words it wrote"),
    }
}

fn pack_process_words_state(state: ProcessState) -> u64 {
    match state {
        ProcessState::Live => 0,
        ProcessState::ExitedVerified => 1,
        ProcessState::ExitedUnverified => 2,
    }
}

fn state_of(word: u64) -> ProcessState {
    match word {
        0 => ProcessState::Live,
        1 => ProcessState::ExitedVerified,
        2 => ProcessState::ExitedUnverified,
        _ => unreachable!("a packed block holds only the words it wrote"),
    }
}

fn process_key(row: &ProcessRow) -> ProcessKey {
    (row.pid, row.creation.map(|creation| creation.unique_id))
}

/// The bits of a mask of seconds from the oldest through `second_index`.
fn seconds_through(second_index: u32) -> u64 {
    assert!(second_index < u64::BITS);

    u64::MAX >> (u64::BITS - 1 - second_index)
}

impl BlockTable {
    /// The bytes of the table's own allocation in its `Arc`, with the two reference counts.
    fn arc_bytes() -> usize {
        2 * size_of::<usize>() + size_of::<Self>()
    }

    /// The bytes of the parts of a table of `app_count` Apps over `second_count` seconds whose
    /// sizes are known before the table is built: its own allocation in its `Arc`, its list of
    /// Apps, and each second's App count and step start.
    fn pack_known_bytes(app_count: usize, second_count: usize) -> usize {
        Self::arc_bytes()
            + app_count * size_of::<BlockApp>()
            + second_count * size_of::<u32>()
            + (second_count + 1) * size_of::<u32>()
    }

    /// The table's charge: its own allocation with its two reference counts, and each list. A
    /// table whose seconds keep their App rows holds no step starts, so this can be less than
    /// `pack_known_bytes`.
    fn bytes(&self) -> usize {
        Self::arc_bytes()
            + size_of_val(&*self.apps)
            + size_of_val(&*self.app_counts)
            + size_of_val(&*self.step_starts)
            + size_of_val(&*self.steps)
            + size_of_val(&*self.processes)
            + size_of_val(&*self.change_seconds)
            + size_of_val(&*self.change_starts)
            + size_of_val(&*self.values)
            + size_of_val(&*self.names)
            + size_of_val(&*self.member_starts)
            + size_of_val(&*self.member_places)
    }

    /// The bytes the table is charged, which the tests and the read benchmark count.
    #[cfg(test)]
    pub(in crate::store) fn reserved_bytes(&self) -> usize {
        self.reservation.bytes()
    }

    /// The bytes the table holds, summed from its parts, which a test compares with what the
    /// budget was charged.
    #[cfg(test)]
    pub(in crate::store) fn held_bytes(&self) -> usize {
        self.bytes()
    }

    /// The App index at `second_index` of each App place of the block, `PLACE_NONE` where the
    /// second lacks the App.
    fn app_indexes(&self, second_index: u32) -> Vec<u32> {
        let mut indexes = Vec::with_capacity(self.apps.len());
        indexes.extend(self.app_indexes_by_place(second_index));

        indexes
    }

    /// The App index at `second_index` of each App place of the block in place order,
    /// `PLACE_NONE` where the second lacks the App.
    fn app_indexes_by_place(&self, second_index: u32) -> impl Iterator<Item = u32> + '_ {
        let bit = 1 << second_index;
        self.apps.iter().scan(0_u32, move |index, app| {
            if app.seconds & bit == 0 {
                Some(PLACE_NONE)
            } else {
                let at = *index;
                *index += 1;
                Some(at)
            }
        })
    }

    pub(super) fn places(&self, second_index: u32, count: usize) -> Places<'_> {
        Places {
            table: self,
            processes: self.processes.iter(),
            second_index,
            remaining: count,
            app_indexes: self.app_indexes(second_index),
        }
    }

    /// The processes at `second_index` of the App at `app_place` among the block's Apps, which
    /// that second holds, or of no App where `app_place` is `None`.
    pub(super) fn members(&self, second_index: u32, app_place: Option<u32>) -> Members<'_> {
        let app_place = app_place.map_or(PLACE_NONE, |place| {
            assert!(self.block_app(place).seconds & (1 << second_index) != 0);

            place
        });
        let list = members_list(app_place, self.apps.len());
        let start = collector::usize_from_u32(self.member_starts[list]);
        let end = collector::usize_from_u32(self.member_starts[list + 1]);

        Members {
            table: self,
            places: self.member_places[start..end].iter(),
            second_index,
            app_place,
        }
    }

    /// The process of pid `pid` created at `creation` at `second_index`, where that second
    /// holds it.
    pub(super) fn find(
        &self,
        second_index: u32,
        pid: i32,
        creation: Option<ProcessCreation>,
    ) -> Option<ProcessAt<'_>> {
        let place = self.process_place((pid, creation.map(|creation| creation.unique_id)))?;
        let process = &self.processes[place];
        if process.seconds & (1 << second_index) == 0 {
            return None;
        }
        let packed = PackedAt {
            table: self,
            process,
            second_index,
        };
        if packed.creation() != creation {
            return None;
        }

        let app_place = packed.app_place();
        let app_index = (app_place != PLACE_NONE).then(|| {
            let place = collector::usize_from_u32(app_place);
            let Some(index) = self.app_indexes_by_place(second_index).nth(place) else {
                unreachable!("a process's App place is one of the block's Apps");
            };
            assert!(
                index < PLACE_NONE,
                "the second holds the App of its process"
            );

            index
        });

        Some(ProcessAt {
            reading: Reading::Packed(packed),
            app_index,
        })
    }

    /// The first row the block holds for the process of `key`, which the next block shares
    /// where its own first row reads the same.
    fn first_row(&self, key: ProcessKey) -> Option<&Arc<Shared<ProcessRow>>> {
        self.process_place(key)
            .map(|place| &self.processes[place].first)
    }

    /// The place among the block's processes of the process of `key`, where the block holds it.
    fn process_place(&self, key: ProcessKey) -> Option<usize> {
        self.processes
            .binary_search_by(|process| process_key(&process.first.row).cmp(&key))
            .ok()
    }

    /// Room for what the check of the block's seconds holds of each of its processes.
    pub(in crate::store) fn checked(&self) -> Vec<Checked> {
        vec![None; self.processes.len()]
    }

    /// The block table of the block of `seconds`, which shares each first row with the block
    /// before, `before`, where the two read the same, or why the block stays plain.
    fn pack(
        seconds: &[Arc<AppsSecond>],
        before: Option<&Self>,
        budget: &Arc<ByteBudget>,
        share: AppRowsShare,
    ) -> Result<Arc<Self>, KeptPlain> {
        assert!(!seconds.is_empty());
        assert!(seconds.len() <= SECONDS_MAX);
        let mut parts = Parts {
            processes: Vec::new(),
            change_seconds: Vec::new(),
            change_starts: Vec::new(),
            values: Vec::new(),
            names: Vec::new(),
            members: Vec::new(),
        };

        let (places, app_count) = pack_apps(seconds)?;
        // The table's own allocation, its list of Apps and each second's App count and step
        // start have sizes known before they are built, so they are charged before they are
        // allocated, and the rest once it is built.
        let reservation = budget
            .reserve(Self::pack_known_bytes(app_count, seconds.len()))
            .ok_or(KeptPlain::Budget)?;
        let app_parts = apps::pack_app_rows(seconds, &places, app_count, &mut parts);
        let app_parts =
            apps::pack_app_rows_where_paying(app_parts, seconds, &places, &mut parts, share);
        let rows: Vec<SecondRows<'_>> = seconds
            .iter()
            .zip(places)
            .map(|(second, places)| pack_rows(second, &places))
            .collect();
        pack_processes(&rows, before, &mut parts)?;

        let mut table = Self::of_parts(app_parts, parts, reservation);
        table.charge_rest()?;

        Ok(Arc::new(table))
    }

    /// The table that holds `app_parts` and `parts`, as the packer built them, charged
    /// `reservation`.
    fn of_parts(app_parts: AppParts, parts: Parts, reservation: ByteReservation) -> Self {
        let AppParts {
            apps,
            counts,
            step_starts,
            steps,
            detailed_seconds,
            rows_packed,
            #[cfg(test)]
            bytes,
            #[cfg(not(test))]
                bytes: _,
        } = app_parts;
        let apps = apps.into_boxed_slice();
        let (member_starts, member_places) = pack_members(apps.len(), &parts.members);

        Self {
            apps,
            app_counts: counts.into_boxed_slice(),
            step_starts: step_starts.into_boxed_slice(),
            steps: steps.into_boxed_slice(),
            detailed_seconds,
            app_rows_packed: rows_packed,
            #[cfg(test)]
            app_rows_bytes: bytes,
            processes: parts.processes.into_boxed_slice(),
            change_seconds: parts.change_seconds.into_boxed_slice(),
            change_starts: parts.change_starts.into_boxed_slice(),
            values: parts.values.into_boxed_slice(),
            names: parts.names.into_boxed_slice(),
            member_starts,
            member_places,
            reservation,
        }
    }

    /// Charges the table the rest of its bytes, since the other lists' sizes are known only once
    /// they are built, or gives back what a block that holds its Apps plain let go of, or the
    /// budget's refusal.
    fn charge_rest(&mut self) -> Result<(), KeptPlain> {
        let table_bytes = self.bytes();
        let reserved_bytes = self.reservation.bytes();
        if table_bytes < reserved_bytes {
            self.reservation.shrink_to(table_bytes);
        } else if !self.reservation.grow(table_bytes - reserved_bytes) {
            return Err(KeptPlain::Budget);
        }

        Ok(())
    }
}

/// What the check of a block's seconds holds of one of its processes: the address of the plain
/// row it last checked the process against and the second it did, or `None` before the first.
pub(in crate::store) type Checked = Option<(usize, u32)>;

/// Whether the processes of the packed `packed` read through its block's table as the rows of
/// the plain `plain`, as many, in their order and with their App indexes. `checked` holds, for each
/// process of the block, the plain row it was last checked against and the second it was, and
/// the check moves it on. A packed process reads at a second the latest change of each field
/// at or before it, so a process whose plain row is the row it was last checked against, and
/// whose record changes nothing after that second, reads as it read then. Of such a process
/// only the App index is compared, which the second's Apps set. An address names one row for
/// the whole check, since the block's plain seconds hold every row they list until it ends.
pub(in crate::store) fn processes_read_as(
    packed: &super::Payload,
    plain: &super::Payload,
    checked: &mut [Checked],
) -> bool {
    let super::ProcessRows::Packed {
        table,
        second_index,
        count,
    } = &packed.rows
    else {
        unreachable!("the check reads a packed second");
    };
    let super::ProcessRows::Plain { rows, app_indexes } = &plain.rows else {
        unreachable!("the check compares a packed second with a plain one");
    };
    assert_eq!(checked.len(), table.processes.len());
    if *count != rows.len() {
        return false;
    }
    // The walk below stops at the count, so a process the table holds at the second past the
    // last the plain second lists is found only by counting them all.
    let bit = 1 << second_index;
    let present_count = table
        .processes
        .iter()
        .filter(|process| process.seconds & bit != 0)
        .count();
    if present_count != rows.len() {
        return false;
    }

    let mut places = table.places(*second_index, *count);
    for (shared, app_index) in rows.iter().zip(app_indexes) {
        let Some((packed_at, packed_app_index)) = places.next() else {
            return false;
        };
        if packed_app_index != app_index.get() {
            return false;
        }
        let process_index = table.processes.len() - places.processes.len() - 1;
        let address = std::ptr::from_ref(&**shared).addr();
        let unchanged = checked[process_index]
            .is_some_and(|(row, since)| row == address && !packed_at.changes_after(since));
        checked[process_index] = Some((address, *second_index));
        if unchanged {
            continue;
        }
        let packed_process = ProcessAt {
            reading: Reading::Packed(packed_at),
            app_index: packed_app_index,
        };
        let plain_process = ProcessAt {
            reading: Reading::Row(&shared.row),
            app_index: app_index.get(),
        };
        if packed_process != plain_process {
            return false;
        }
    }

    places.next().is_none()
}

impl<'table> PackedAt<'table> {
    fn first(&self) -> &'table ProcessRow {
        &self.process.first.row
    }

    /// Where the process's first row in the block lies in memory.
    pub(super) fn row_address(&self) -> usize {
        std::ptr::from_ref(self.first()).addr()
    }

    /// The value of `field` at the second, where it differs from the first row's.
    fn changed(&self, field: Field) -> Option<u64> {
        let bit = field.bit();
        let changing = self.process.changing;
        if changing & bit == 0 {
            return None;
        }

        let rank = (changing & (bit - 1)).count_ones();
        let record = collector::usize_from_u32(self.process.changes_start + rank);
        let changes = self.table.change_seconds[record] & seconds_through(self.second_index);
        let count = changes.count_ones();
        if count == 0 {
            return None;
        }

        let start = self.table.change_starts[record];
        Some(self.table.values[collector::usize_from_u32(start + count - 1)])
    }

    pub(super) fn pid(&self) -> i32 {
        self.first().pid
    }

    pub(super) fn creation(&self) -> Option<ProcessCreation> {
        let creation = self.first().creation?;
        let Some(word) = self.changed(Field::PidVersion) else {
            return Some(creation);
        };
        let bits = u32::try_from(word).unwrap_or_else(|_| unreachable!("a pid version word"));

        Some(ProcessCreation {
            unique_id: creation.unique_id,
            pid_version: i32::from_ne_bytes(bits.to_ne_bytes()),
        })
    }

    pub(super) fn executable_name(&self) -> Option<&'table ProcessName> {
        let Some(word) = self.changed(Field::ExecutableName) else {
            return self.first().executable_name();
        };
        if word == NAME_NONE {
            return None;
        }
        let place = usize::try_from(word).unwrap_or_else(|_| unreachable!("a name place"));

        Some(&self.table.names[place])
    }

    pub(super) fn program_unavailable(&self) -> Option<IdentityUnavailable> {
        self.changed(Field::ProgramUnavailable)
            .map_or(self.first().program_unavailable, identity_of)
    }

    pub(super) fn app_attribution(&self) -> Option<Unavailable> {
        self.changed(Field::AppAttribution)
            .map_or(self.first().app_attribution, unavailable_of)
    }

    pub(super) fn state(&self) -> ProcessState {
        self.changed(Field::State)
            .map_or(self.first().state, state_of)
    }

    pub(super) fn compact(&self, measured: Measured) -> CompactAmount {
        let first = self.first().compact(measured);
        let known = self
            .changed(Field::Known(measured))
            .map_or(first.known, u64::to_le_bytes);
        let unavailable = self
            .changed(Field::Unavailable(measured))
            .map_or(first.unavailable, unavailable_of);

        CompactAmount { known, unavailable }
    }

    /// Whether the process's record changes a field after the second `since` and at or before
    /// this one.
    fn changes_after(&self, since: u32) -> bool {
        assert!(since < self.second_index);
        let seconds = seconds_through(self.second_index) & !seconds_through(since);
        let start = collector::usize_from_u32(self.process.changes_start);
        let count = collector::usize_from_u32(self.process.changing.count_ones());

        self.table.change_seconds[start..start + count]
            .iter()
            .any(|record| record & seconds != 0)
    }

    fn app_place(&self) -> u32 {
        self.changed(Field::AppPlace)
            .map_or(self.process.first_app_place, |word| {
                u32::try_from(word).unwrap_or_else(|_| unreachable!("an App place word"))
            })
    }
}

impl<'table> Iterator for Places<'table> {
    type Item = (PackedAt<'table>, Option<u32>);

    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        let bit = 1 << self.second_index;
        let Some(process) = self.processes.find(|process| process.seconds & bit != 0) else {
            unreachable!("the block holds every process its second counts");
        };
        self.remaining -= 1;
        let packed = PackedAt {
            table: self.table,
            process,
            second_index: self.second_index,
        };

        let app_place = packed.app_place();
        let app_index = (app_place != PLACE_NONE).then(|| {
            let index = self.app_indexes[collector::usize_from_u32(app_place)];
            assert!(
                index < PLACE_NONE,
                "a process's App is an App of its second"
            );
            index
        });
        Some((packed, app_index))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl<'table> Iterator for Members<'table> {
    type Item = PackedAt<'table>;

    fn next(&mut self) -> Option<Self::Item> {
        let bit = 1 << self.second_index;
        let (table, second_index, app_place) = (self.table, self.second_index, self.app_place);
        // A process under the App at another second, or absent from this one, is passed over.
        let member = move |process: &'table PackedProcess| {
            if process.seconds & bit == 0 {
                return None;
            }
            let packed = PackedAt {
                table,
                process,
                second_index,
            };

            (packed.app_place() == app_place).then_some(packed)
        };
        self.places
            .find_map(|&place| member(&table.processes[collector::usize_from_u32(place)]))
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (0, self.places.size_hint().1)
    }
}

/// The parts of a table as the packer builds them, before they are charged.
struct Parts {
    processes: Vec<PackedProcess>,
    change_seconds: Vec<u64>,
    change_starts: Vec<u32>,
    values: Vec<u64>,
    names: Vec<Arc<ProcessName>>,
    /// Each App place a process was under at any second, with the process's place, in the
    /// block's process order.
    members: Vec<Member>,
}

/// An App place a process of the block was under at a second, or `PLACE_NONE` where it was
/// outside any App, with the process's place among the block's processes.
#[derive(Clone, Copy)]
struct Member {
    app_place: u32,
    process_place: u32,
}

/// The row of a process at one second of the block that holds it, with its App place there.
#[derive(Clone, Copy)]
struct PresentRow<'row> {
    second_index: usize,
    shared: &'row Arc<Shared<ProcessRow>>,
    app_place: u32,
}

/// One second's processes as the packer reads them: its rows and their App places.
struct SecondRows<'second> {
    rows: &'second [Arc<Shared<ProcessRow>>],
    app_places: Vec<u32>,
}

/// Fills `apps` with the Apps of `seconds` in key order, each with the seconds that hold it,
/// and returns the App place of each App of each second and how many Apps the block holds, or
/// why the block stays plain.
fn pack_apps(seconds: &[Arc<AppsSecond>]) -> Result<(Vec<Vec<u32>>, usize), KeptPlain> {
    let rows: Vec<&[AppRow]> = seconds
        .iter()
        .map(|second| second.apps.as_slice())
        .collect();
    let mut places: Vec<Vec<u32>> = rows
        .iter()
        .map(|rows| Vec::with_capacity(rows.len()))
        .collect();
    let mut cursors = vec![0; rows.len()];
    let total: usize = rows.iter().map(|rows| rows.len()).sum();
    // Each App found takes at least one row, so the rows run out within one more turn.
    for app_count in 0..=total {
        let least = rows
            .iter()
            .zip(&cursors)
            .filter_map(|(rows, &cursor)| rows.get(cursor))
            .min_by(|left, right| app_order(left, right));
        let Some(least) = least else {
            return Ok((places, app_count));
        };
        if app_count == APPS_PER_SECOND_MAX {
            return Err(KeptPlain::Bound);
        }

        let place = block_u32(app_count);
        for (rows, (places, cursor)) in rows.iter().zip(places.iter_mut().zip(&mut cursors)) {
            if let Some(row) = rows.get(*cursor) {
                if app_order(row, least) == Ordering::Equal {
                    places.push(place);
                    *cursor += 1;
                }
            }
        }
    }

    unreachable!("each step takes at least one App row")
}

/// The starts and the process places of the lists of each of the block's `app_count` App
/// places and of no App, from `members`, each App place a process was under, or `PLACE_NONE`,
/// with the process's place, in the block's process order, which each list keeps.
fn pack_members(app_count: usize, members: &[Member]) -> (Box<[u32]>, Box<[u32]>) {
    let list_count = app_count + 1;
    let mut starts = vec![0_u32; list_count + 1];
    for member in members {
        starts[members_list(member.app_place, app_count) + 1] += 1;
    }
    for list in 0..list_count {
        starts[list + 1] += starts[list];
    }
    let mut places = vec![0_u32; members.len()];
    let mut next = starts.clone();
    for member in members {
        let cursor = &mut next[members_list(member.app_place, app_count)];
        places[collector::usize_from_u32(*cursor)] = member.process_place;
        *cursor += 1;
    }
    assert_eq!(next[..list_count], starts[1..]);

    (starts.into_boxed_slice(), places.into_boxed_slice())
}

/// The list in `member_starts` of the App at `app_place` of a block of `app_count` Apps, the
/// list after the last App's where `app_place` is `PLACE_NONE`.
fn members_list(app_place: u32, app_count: usize) -> usize {
    if app_place == PLACE_NONE {
        app_count
    } else {
        let place = collector::usize_from_u32(app_place);
        assert!(place < app_count);
        place
    }
}

fn app_order(left: &AppRow, right: &AppRow) -> Ordering {
    if Arc::ptr_eq(&left.name, &right.name) {
        return Ordering::Equal;
    }

    let left = left.name.kind.key(&left.name.bytes);
    left.cmp(&right.name.kind.key(&right.name.bytes))
}

/// The process rows of `second` with the App place of each, `places` being the place of each
/// of its Apps. A second without process rows holds none.
fn pack_rows<'second>(second: &'second AppsSecond, places: &[u32]) -> SecondRows<'second> {
    let Ok(payload) = &second.processes else {
        return SecondRows {
            rows: &[],
            app_places: Vec::new(),
        };
    };
    let super::ProcessRows::Plain { rows, app_indexes } = &payload.rows else {
        unreachable!("the packer packs plain seconds");
    };
    assert_eq!(places.len(), payload.apps.len());

    let app_places = app_indexes
        .iter()
        .map(|index| {
            index
                .get()
                .map_or(PLACE_NONE, |index| places[collector::usize_from_u32(index)])
        })
        .collect();
    SecondRows { rows, app_places }
}

/// Fills `parts` with the processes of `seconds` in the order every second lists them, pid and
/// then the creation's unique id, each with its first row and the changes of its fields after
/// it, or says why the block stays plain.
fn pack_processes(
    seconds: &[SecondRows<'_>],
    before: Option<&BlockTable>,
    parts: &mut Parts,
) -> Result<(), KeptPlain> {
    let mut cursors = vec![0; seconds.len()];
    let mut present = Vec::with_capacity(SECONDS_MAX);
    let mut changes: FieldChanges = std::array::from_fn(|_| Vec::with_capacity(SECONDS_MAX));
    let total: usize = seconds.iter().map(|second| second.rows.len()).sum();
    for _ in 0..=total {
        let least = seconds
            .iter()
            .zip(&cursors)
            .filter_map(|(second, &cursor)| second.rows.get(cursor))
            .map(|shared| process_key(&shared.row))
            .min();
        let Some(key) = least else {
            return Ok(());
        };
        if parts.processes.len() == PROCESS_INTERVALS_MAX {
            return Err(KeptPlain::Bound);
        }

        present.clear();
        for (second_index, (second, cursor)) in seconds.iter().zip(&mut cursors).enumerate() {
            let Some(shared) = second.rows.get(*cursor) else {
                continue;
            };
            if process_key(&shared.row) != key {
                continue;
            }
            present.push(PresentRow {
                second_index,
                shared,
                app_place: second.app_places[*cursor],
            });
            *cursor += 1;
            if let Some(next) = second.rows.get(*cursor) {
                assert!(
                    process_key(&next.row) > key,
                    "a second lists its processes in order"
                );
            }
        }
        pack_process(&present, before, &mut changes, parts);
    }

    unreachable!("each step takes at least one process row")
}

/// Adds to `parts` the process whose row and App place at each second that holds it are
/// `present`, oldest first. `changes` is room for the process's changes.
fn pack_process(
    present: &[PresentRow<'_>],
    before: Option<&BlockTable>,
    changes: &mut FieldChanges,
    parts: &mut Parts,
) {
    let Some(&first) = present.first() else {
        unreachable!("a process of the block is held at a second");
    };
    for field_changes in changes.iter_mut() {
        field_changes.clear();
    }
    let mut seconds = 0;
    let mut previous = first;
    // The words of the row before, worked out only once a row differs from it.
    let mut previous_words = None;
    for &row in present {
        seconds |= 1 << row.second_index;
        if Arc::ptr_eq(row.shared, previous.shared) && row.app_place == previous.app_place {
            continue;
        }
        let words = pack_process_words(&row.shared.row, row.app_place);
        let before = previous_words
            .unwrap_or_else(|| pack_process_words(&previous.shared.row, previous.app_place));
        for ((word, before), field_changes) in words.iter().zip(&before).zip(changes.iter_mut()) {
            if word != before {
                field_changes.push((row.second_index, *word));
            }
        }
        previous = row;
        previous_words = Some(words);
    }

    let (changing, changes_start) = pack_process_record(present, changes, parts);
    pack_process_members(present, parts);
    let first_row = before
        .and_then(|before| before.first_row(process_key(&first.shared.row)))
        .filter(|shared| shared.row == first.shared.row)
        .unwrap_or(first.shared);
    parts.processes.push(PackedProcess {
        seconds,
        changing,
        changes_start,
        first: Arc::clone(first_row),
        first_app_place: first.app_place,
    });
}

/// Adds to `parts` the record of `changes`, each field's changes of the process `present`
/// names, and gives the bits of the fields that change and the record's first index.
fn pack_process_record(
    present: &[PresentRow<'_>],
    changes: &FieldChanges,
    parts: &mut Parts,
) -> (u32, u32) {
    let changes_start = u32::try_from(parts.change_seconds.len())
        .unwrap_or_else(|_| unreachable!("the change records are bounded"));
    let mut changing = 0;
    // Each field's changes go together, oldest first, in the order of the fields' bits.
    for (field, field_changes) in Field::ALL.iter().zip(changes.iter()) {
        if field_changes.is_empty() {
            continue;
        }
        changing |= field.bit();
        let start = u32::try_from(parts.values.len())
            .unwrap_or_else(|_| unreachable!("the changed values are bounded"));
        parts.change_starts.push(start);
        let mut record = 0;
        for &(second_index, word) in field_changes {
            record |= 1 << second_index;
            let value = match field {
                Field::ExecutableName => {
                    pack_name(&pack_process_row(present, second_index).row, parts)
                }
                _ => word,
            };
            parts.values.push(value);
        }
        parts.change_seconds.push(record);
    }

    (changing, changes_start)
}

/// Adds to the members of `parts` each App place the process `present` names was under at any
/// second, once, with the place the process takes next among the block's processes.
fn pack_process_members(present: &[PresentRow<'_>], parts: &mut Parts) {
    let process_place = u32::try_from(parts.processes.len())
        .unwrap_or_else(|_| unreachable!("a block's processes are bounded"));
    let members_start = parts.members.len();
    for present_row in present {
        let app_place = present_row.app_place;
        let listed = parts.members[members_start..]
            .iter()
            .any(|member| member.app_place == app_place);
        if !listed {
            parts.members.push(Member {
                app_place,
                process_place,
            });
        }
    }
}

fn pack_process_row<'row>(
    present: &[PresentRow<'row>],
    second_index: usize,
) -> &'row Arc<Shared<ProcessRow>> {
    let found = present
        .iter()
        .find(|present_row| present_row.second_index == second_index);
    let Some(present_row) = found else {
        unreachable!("a change is at a second that holds the process");
    };

    present_row.shared
}

/// The word of `row`'s executable name: its place in the block's names, which it joins where
/// it is not there yet.
fn pack_name(row: &ProcessRow, parts: &mut Parts) -> u64 {
    let Some(name) = &row.executable_name else {
        return NAME_NONE;
    };
    let place = if let Some(place) = parts.names.iter().position(|held| Arc::ptr_eq(held, name)) {
        place
    } else {
        parts.names.push(Arc::clone(name));
        parts.names.len() - 1
    };

    u64::try_from(place).unwrap_or_else(|_| unreachable!("the names are bounded"))
}
