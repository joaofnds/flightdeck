//! Typed process payloads and shared names retain their own admissions, including View owners.
use std::fmt;
use std::ops::Deref;
use std::sync::Arc;

#[path = "payload_names.rs"]
mod names;
#[path = "payload_packed.rs"]
pub(super) mod packed;
#[cfg(test)]
pub(super) use names::NameTrace;
pub(super) use names::{NameAdditions, ProcessNames};

use super::{AppRow, AppsSecond, Missing};
use crate::apps::{Amount, ProcessState, Stamp, Unavailable};
use crate::byte_budget::{ByteBudget, ByteReservation};
use crate::live_app_response::{Bytes, Measure, Observation, Process, Sample};
use collector::ring::{IdentityUnavailable, ProcessCreation};

/// A complete observation's processes and additional App measures.
///
/// The list of a second's App details cannot be detached from the payload's byte reservation.
/// ```compile_fail
/// use viewer::store::Payload;
/// fn escape(payload: &mut Payload) {
///     drop(std::mem::take(&mut payload.apps));
/// }
/// ```
#[derive(Debug)]
pub struct Payload {
    pub(crate) selected: Observation,
    pub previous: Option<Stamp>,
    rows: ProcessRows,
    apps: Vec<Arc<Shared<AppDetails>>>,
    reservation: ByteReservation,
}

/// A second's processes: rows of its own, each shared with the second before where the two
/// read the same, or its place in the block table of the packed block that holds it.
#[derive(Debug)]
enum ProcessRows {
    Plain {
        rows: Vec<Arc<Shared<ProcessRow>>>,
        /// The App index of each of `rows`, at the same place. It is held apart from the shared
        /// rows because an App arriving or leaving moves the index of every process of the Apps
        /// after it, and a process whose readings repeat keeps sharing its row whatever its
        /// index.
        app_indexes: Vec<AppIndex>,
    },
    Packed {
        table: Arc<packed::BlockTable>,
        /// The second's index in its block, the oldest at 0.
        second_index: u32,
        /// How many processes the second holds.
        count: usize,
    },
}

/// One process at one second, read from the second's own row or from its block's process
/// table, with the App index that second gives it, so a reader never pairs a process with
/// another second's readings or another process's index.
#[derive(Clone, Copy, Debug)]
pub struct ProcessAt<'payload> {
    reading: Reading<'payload>,
    app_index: Option<u32>,
}

#[derive(Clone, Copy, Debug)]
enum Reading<'payload> {
    Row(&'payload ProcessRow),
    Packed(packed::PackedAt<'payload>),
}

/// Every process of one payload, in the sample's order, each with its App index.
#[derive(Debug)]
pub struct Places<'payload> {
    places: PlacesKind<'payload>,
}

#[derive(Debug)]
enum PlacesKind<'payload> {
    Plain(
        std::iter::Zip<
            std::slice::Iter<'payload, Arc<Shared<ProcessRow>>>,
            std::slice::Iter<'payload, AppIndex>,
        >,
    ),
    Packed(packed::Places<'payload>),
}

/// A process's App as the App's place among the Apps of its second, or no App, in four bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct AppIndex(u32);

/// One row and its admission, held by every second whose reading equals it.
#[derive(Debug)]
struct Shared<Row> {
    row: Row,
    _reservation: ByteReservation,
}

/// The rows of one kind of one payload in its sample's order, borrowed from their owners.
#[cfg(any(test, feature = "fixtures"))]
#[derive(Debug)]
pub struct Rows<'payload, Row> {
    rows: std::slice::Iter<'payload, Arc<Shared<Row>>>,
}

/// The additional measures of each App of one payload, in the sample's order.
#[derive(Clone, Debug)]
pub struct AppDetailsOf<'payload> {
    details: DetailsKind<'payload>,
}

#[derive(Clone, Debug)]
enum DetailsKind<'payload> {
    Plain(std::slice::Iter<'payload, Arc<Shared<AppDetails>>>),
    Packed(packed::PackedDetails<'payload>),
}

impl Iterator for AppDetailsOf<'_> {
    type Item = AppDetails;

    fn next(&mut self) -> Option<AppDetails> {
        match &mut self.details {
            DetailsKind::Plain(details) => details.next().map(|shared| shared.row),
            DetailsKind::Packed(details) => details.next(),
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.details {
            DetailsKind::Plain(details) => details.size_hint(),
            DetailsKind::Packed(details) => details.size_hint(),
        }
    }
}

impl ExactSizeIterator for AppDetailsOf<'_> {}

/// The processes of one payload whose App index is one index, in the sample's order. A plain
/// payload's reads each row's index from the payload's indexes and no row it passes over, and a
/// packed one's compares each process's App place with the App's.
#[derive(Clone, Debug)]
pub struct Members<'payload> {
    members: MembersKind<'payload>,
    app_index: Option<u32>,
}

#[derive(Clone, Debug)]
enum MembersKind<'payload> {
    Plain {
        rows: std::iter::Zip<
            std::slice::Iter<'payload, Arc<Shared<ProcessRow>>>,
            std::slice::Iter<'payload, AppIndex>,
        >,
        app_index: AppIndex,
    },
    Packed(packed::Members<'payload>),
}

/// Captured executable bytes and their display spelling, shared across observations.
///
/// Buffers cannot be detached from their reservation, even with an exclusive owner.
/// ```compile_fail
/// use viewer::store::ProcessName;
/// fn escape(name: &mut ProcessName) -> Vec<u8> {
///     std::mem::take(&mut name.bytes)
/// }
/// ```
pub struct ProcessName {
    name: Bytes,
    reservation: ByteReservation,
}

/// Every process of the sample, including unavailable identity and usage.
///
/// Executable ownership is shared privately; public access remains borrowed.
/// ```compile_fail
/// use std::sync::Arc;
/// use viewer::store::{ProcessName, ProcessRow};
/// fn escape(row: &ProcessRow) -> Option<Arc<ProcessName>> {
///     row.executable_name.clone()
/// }
/// ```
#[derive(Debug, PartialEq, Eq)]
pub struct ProcessRow {
    pub pid: i32,
    pub creation: Option<ProcessCreation>,
    executable_name: Option<Arc<ProcessName>>,
    pub program_unavailable: Option<IdentityUnavailable>,
    cpu_ns: CompactAmount,
    energy_nj: CompactAmount,
    gpu_ns: CompactAmount,
    disk_read_bytes: CompactAmount,
    disk_written_bytes: CompactAmount,
    disk_io_bytes: CompactAmount,
    resident_bytes: CompactAmount,
    footprint_bytes: CompactAmount,
    idle_wakeups: CompactAmount,
    pageins: CompactAmount,
    threads: CompactAmount,
    pub app_attribution: Option<Unavailable>,
    pub state: ProcessState,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CompactAmount {
    known: [u8; 8],
    unavailable: Option<Unavailable>,
}

/// The eleven amounts a process row holds, in the order its fields are declared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Measured {
    Cpu,
    Energy,
    Gpu,
    DiskRead,
    DiskWritten,
    DiskIo,
    Resident,
    Footprint,
    IdleWakeups,
    Pageins,
    Threads,
}

impl Measured {
    const ALL: [Self; 11] = [
        Self::Cpu,
        Self::Energy,
        Self::Gpu,
        Self::DiskRead,
        Self::DiskWritten,
        Self::DiskIo,
        Self::Resident,
        Self::Footprint,
        Self::IdleWakeups,
        Self::Pageins,
        Self::Threads,
    ];

    /// The amount's place in `ALL`. The match names every amount and the assertion after it
    /// ties each place to `ALL`, so an amount added here is one `ALL` has to list.
    const fn index(self) -> usize {
        match self {
            Self::Cpu => 0,
            Self::Energy => 1,
            Self::Gpu => 2,
            Self::DiskRead => 3,
            Self::DiskWritten => 4,
            Self::DiskIo => 5,
            Self::Resident => 6,
            Self::Footprint => 7,
            Self::IdleWakeups => 8,
            Self::Pageins => 9,
            Self::Threads => 10,
        }
    }
}

const _: () = {
    let mut index = 0;
    while index < Measured::ALL.len() {
        assert!(Measured::ALL[index].index() == index);
        index += 1;
    }
};

/// Measures displayed in App detail, aligned with the observation's App rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppDetails {
    pub live_members: u32,
    pub disk_read_bytes: Measure,
    pub disk_written_bytes: Measure,
    pub footprint_bytes: Measure,
}

/// The measures of App details, each three words of a packed block.
const DETAILS_MEASURE_COUNT: usize = 3;

/// The words of App details a packed block holds: the live members, then each measure's
/// amount, its member counts and whether it overflowed.
const APP_DETAILS_WORDS: usize = 1 + 3 * DETAILS_MEASURE_COUNT;

impl AppDetails {
    /// The details' fields as words, in the order `of_words` reads them. Every field is named,
    /// so a field added to the details is one a packed block has to hold.
    fn words(&self) -> [u64; APP_DETAILS_WORDS] {
        let Self {
            live_members,
            disk_read_bytes,
            disk_written_bytes,
            footprint_bytes,
        } = self;
        let measures: [&Measure; DETAILS_MEASURE_COUNT] =
            [disk_read_bytes, disk_written_bytes, footprint_bytes];
        let mut words = [0; APP_DETAILS_WORDS];
        words[0] = u64::from(*live_members);
        for (index, measure) in measures.into_iter().enumerate() {
            let Measure {
                known,
                measured_members,
                unavailable_members,
                overflowed,
            } = *measure;
            words[1 + 3 * index] = known;
            words[2 + 3 * index] =
                u64::from(measured_members) | u64::from(unavailable_members) << 32;
            words[3 + 3 * index] = u64::from(overflowed);
        }

        words
    }

    /// The details whose fields `words` gives, as `words` gave them.
    fn of_words(words: &[u64; APP_DETAILS_WORDS]) -> Self {
        let measure = |index: usize| {
            let members = words[2 + 3 * index];
            Measure {
                known: words[1 + 3 * index],
                measured_members: details_word_low(members),
                unavailable_members: details_word_low(members >> 32),
                overflowed: words[3 + 3 * index] != 0,
            }
        };

        Self {
            live_members: details_word_low(words[0]),
            disk_read_bytes: measure(0),
            disk_written_bytes: measure(1),
            footprint_bytes: measure(2),
        }
    }
}

/// The low 32 bits of `word`, a count the details' words hold.
fn details_word_low(word: u64) -> u32 {
    let Ok(low) = u32::try_from(word & u64::from(u32::MAX)) else {
        unreachable!("the low 32 bits of a word fit a u32");
    };

    low
}

/// What a capture shares with: the payload the second before holds, and that second's App
/// rows, which name the App of each of the payload's App details.
struct Previous<'second> {
    payload: Option<&'second Payload>,
    app_rows: &'second [AppRow],
}

/// The names a capture adds, its process rows, their App indexes and its App details.
type Parts = (
    NameAdditions,
    Vec<Arc<Shared<ProcessRow>>>,
    Vec<AppIndex>,
    Vec<Arc<Shared<AppDetails>>>,
);

impl Payload {
    /// The charge of each distinct process row, however many seconds hold it.
    pub const ROW_BYTES: usize = Shared::<ProcessRow>::BYTES;

    /// The charge of each process a second lists, whether its row is shared or the second's own:
    /// the reference to its row and its App index.
    pub const ROW_REFERENCE_BYTES: usize =
        size_of::<Arc<Shared<ProcessRow>>>() + size_of::<AppIndex>();

    /// The charge of each distinct row of an App's additional measures, however many seconds
    /// hold it.
    pub const APP_DETAILS_BYTES: usize = Shared::<AppDetails>::BYTES;

    /// The charge of each App a second lists, whether its additional measures are shared or
    /// the second's own.
    pub const APP_DETAILS_REFERENCE_BYTES: usize = size_of::<Arc<Shared<AppDetails>>>();

    /// The additional measures of each App in the sample's order, read from the second's own
    /// details or from its block's App table.
    #[must_use]
    pub fn apps(&self) -> AppDetailsOf<'_> {
        let details = match &self.rows {
            ProcessRows::Packed {
                table,
                second_index,
                ..
            } if table.app_rows_packed() => {
                assert!(self.apps.is_empty());
                DetailsKind::Packed(table.app_details_of(*second_index))
            }
            ProcessRows::Plain { .. } | ProcessRows::Packed { .. } => {
                DetailsKind::Plain(self.apps.iter())
            }
        };

        AppDetailsOf { details }
    }

    /// The additional measures the payload holds as allocations of its own, which a packed
    /// payload's block holds in its table instead.
    pub(super) fn own_apps(&self) -> impl ExactSizeIterator<Item = &AppDetails> {
        self.apps.iter().map(|shared| &shared.row)
    }

    /// The additional measures of the App at `index` of a plain payload's Apps, as the payload
    /// holds them, which a test of sharing compares by address.
    #[cfg(test)]
    pub(super) fn own_app(&self, index: usize) -> &AppDetails {
        &self.apps[index].row
    }

    /// The additional measures of the App at `index` of the sample's Apps, of a payload that
    /// holds them of its own. A reader of a packed second reaches its Apps through `AppAt`.
    #[must_use]
    pub fn app(&self, index: usize) -> AppDetails {
        let Some(app) = self.apps.get(index) else {
            unreachable!("a reader asks a payload for an App it holds of its own");
        };
        app.row
    }

    /// Every process of the second, in the sample's order, each with its App index.
    #[must_use]
    pub fn processes(&self) -> Places<'_> {
        let places = match &self.rows {
            ProcessRows::Plain { rows, app_indexes } => {
                PlacesKind::Plain(rows.iter().zip(app_indexes.iter()))
            }
            ProcessRows::Packed {
                table,
                second_index,
                count,
            } => PlacesKind::Packed(table.places(*second_index, *count)),
        };

        Places { places }
    }

    /// The processes whose App is the App at `app_index` of the second's Apps, or that belong to
    /// no App where `app_index` is `None`, in the sample's order.
    #[must_use]
    pub fn members(&self, app_index: Option<u32>) -> Members<'_> {
        let members = match &self.rows {
            ProcessRows::Plain { rows, app_indexes } => MembersKind::Plain {
                rows: rows.iter().zip(app_indexes.iter()),
                app_index: AppIndex::of(app_index),
            },
            ProcessRows::Packed {
                table,
                second_index,
                ..
            } => {
                let place = app_index.map(|index| {
                    let place = table.app_place_at(*second_index, collector::usize_from_u32(index));
                    let Some(place) = place else {
                        unreachable!("a reader asks for the members of an App the second holds");
                    };
                    place
                });
                MembersKind::Packed(table.members(*second_index, place))
            }
        };

        Members { members, app_index }
    }

    /// The processes of the App at `app_place` among the Apps of the packed block that holds
    /// the second, whose index at the second is `app_index`, in the sample's order.
    pub(super) fn members_of_place(&self, app_place: u32, app_index: u32) -> Members<'_> {
        let ProcessRows::Packed {
            table,
            second_index,
            ..
        } = &self.rows
        else {
            unreachable!("an App place names an App of a packed block");
        };

        Members {
            members: MembersKind::Packed(table.members(*second_index, Some(app_place))),
            app_index: Some(app_index),
        }
    }

    /// The process of pid `pid` created at `creation`, where the second holds it.
    #[must_use]
    pub fn find(&self, pid: i32, creation: Option<ProcessCreation>) -> Option<ProcessAt<'_>> {
        match &self.rows {
            ProcessRows::Plain { .. } => self
                .processes()
                .find(|process| process.pid() == pid && process.creation() == creation),
            ProcessRows::Packed {
                table,
                second_index,
                ..
            } => table.find(*second_index, pid, creation),
        }
    }

    /// The count of processes.
    #[must_use]
    pub fn len(&self) -> usize {
        match &self.rows {
            ProcessRows::Plain { rows, .. } => rows.len(),
            ProcessRows::Packed { count, .. } => *count,
        }
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Whether the second's processes are read from its packed block's table.
    #[cfg(test)]
    #[must_use]
    pub(in crate::store) fn packed(&self) -> bool {
        matches!(self.rows, ProcessRows::Packed { .. })
    }

    /// Captures `sample`, sharing each process row and each App's additional measures that equal
    /// what `previous`, the second before, holds for the same process or App.
    pub(super) fn capture_with_names(
        sample: &Sample,
        names: &ProcessNames,
        previous: Option<&AppsSecond>,
    ) -> Result<(Self, NameAdditions), Missing> {
        let previous = Previous::of(previous);
        if let Some(payload) = previous.payload {
            assert!(
                payload.reservation.charged_to(names.budget()),
                "the second before is charged to another budget"
            );
            assert_eq!(
                payload.apps.len(),
                previous.app_rows.len(),
                "the second before's App rows and App details differ in count"
            );
        }

        let reservation = names
            .budget()
            .reserve(Self::capture_bytes(sample))
            .ok_or(Missing::Budget)?;
        #[cfg(test)]
        let before = names.work_snapshot();
        let (additions, rows, app_indexes, apps) =
            match Self::capture_with_names_parts(sample, names, &previous) {
                Ok(parts) => parts,
                Err(missing) => {
                    #[cfg(test)]
                    names.record_receipt(sample.selected, before, false);
                    return Err(missing);
                }
            };

        Ok((
            Self {
                selected: sample.selected,
                previous: sample.previous,
                rows: ProcessRows::Plain { rows, app_indexes },
                apps,
                reservation,
            },
            additions,
        ))
    }

    fn capture_with_names_parts(
        sample: &Sample,
        names: &ProcessNames,
        previous: &Previous<'_>,
    ) -> Result<Parts, Missing> {
        let previous_rows = previous.payload.map_or(&[][..], Payload::plain_rows);
        let additions = names.additions(sample)?;
        let rows = Self::capture_with_names_rows(sample, names, &additions, previous_rows)?;
        let app_indexes = Self::capture_with_names_app_indexes(sample);
        assert_eq!(rows.len(), app_indexes.len());
        let apps = Self::capture_with_names_apps(sample, names.budget(), previous)?;

        Ok((additions, rows, app_indexes, apps))
    }

    /// The additional measures of each App of `sample`, which are the ones the second before
    /// holds for the App where that second read the same. One pass over the Apps of the second
    /// before finds each App, since both lists ascend by key.
    fn capture_with_names_apps(
        sample: &Sample,
        budget: &Arc<ByteBudget>,
        previous: &Previous<'_>,
    ) -> Result<Vec<Arc<Shared<AppDetails>>>, Missing> {
        let previous_details = previous.payload.map_or(&[][..], |payload| &payload.apps);
        let mut apps = Vec::with_capacity(sample.apps().len());
        let mut previous_index = 0;
        for app in sample.apps() {
            let details = AppDetails {
                live_members: app.live_members,
                disk_read_bytes: app.disk_read_bytes,
                disk_written_bytes: app.disk_written_bytes,
                footprint_bytes: app.footprint_bytes,
            };
            let key = &app.key;
            let held = super::apps::previous_app(
                key.kind,
                &key.bytes,
                previous.app_rows,
                &mut previous_index,
            )
            .and_then(|index| previous_details.get(index));
            apps.push(Shared::share(details, held, budget)?);
        }

        Ok(apps)
    }

    /// The row of each process of `sample`, which is the row of `previous_rows` where that
    /// second read the same. One pass over `previous_rows` finds each process, since both lists
    /// ascend by pid and then the creation's unique id, the order the packer asserts of every
    /// second it packs.
    fn capture_with_names_rows(
        sample: &Sample,
        names: &ProcessNames,
        additions: &NameAdditions,
        previous_rows: &[Arc<Shared<ProcessRow>>],
    ) -> Result<Vec<Arc<Shared<ProcessRow>>>, Missing> {
        let mut rows = Vec::with_capacity(sample.processes().len());
        let mut previous_index = 0;
        for process in sample.processes() {
            let name = process
                .executable_name
                .as_ref()
                .map(|name| names.name(name, additions));
            let row = ProcessRow::capture(process, name);
            previous_index += previous_rows[previous_index..]
                .iter()
                .take_while(|shared| shared.row.pid < row.pid)
                .count();
            let held = previous_rows.get(previous_index);
            rows.push(Shared::share(row, held, names.budget())?);
        }

        Ok(rows)
    }

    /// The App index of each process of `sample`, in the sample's order.
    fn capture_with_names_app_indexes(sample: &Sample) -> Vec<AppIndex> {
        let app_count = sample.apps().len();
        let mut app_indexes = Vec::with_capacity(sample.processes().len());
        for process in sample.processes() {
            if let Some(index) = process.app_index {
                assert!(collector::usize_from_u32(index) < app_count);
            }
            app_indexes.push(AppIndex::of(process.app_index));
        }

        app_indexes
    }

    /// Names, rows and App details own separate reservations; this admission precedes the
    /// allocation of the payload's containers.
    fn capture_bytes(sample: &Sample) -> usize {
        Self::header_bytes(sample.apps().len())
            + sample.processes().len() * Self::ROW_REFERENCE_BYTES
    }

    /// The charge of a payload's own allocation in its `Arc` with its list of `app_count` App
    /// details, which a captured payload and a packed one share.
    fn header_bytes(app_count: usize) -> usize {
        2 * size_of::<usize>() + size_of::<Self>() + app_count * Self::APP_DETAILS_REFERENCE_BYTES
    }

    /// This payload's header and containers, excluding separately admitted shared names, rows
    /// and App details.
    #[must_use]
    pub fn bytes(&self) -> usize {
        self.reservation.bytes()
    }

    /// The second's own process rows, which a capture shares from. The second before a capture
    /// is always in a plain block: the live path's is the newest, in the open block, and a
    /// launch keeps a block plain while the second after its last still waits for its Apps.
    fn plain_rows(&self) -> &[Arc<Shared<ProcessRow>>] {
        match &self.rows {
            ProcessRows::Plain { rows, .. } => rows,
            ProcessRows::Packed { .. } => unreachable!("a capture shares rows of a packed block"),
        }
    }

    /// The process rows in the sample's order, which a test reads of a payload whose rows are
    /// its own. No release build holds it, since a second of a packed block has no row of its
    /// own to hand out.
    #[cfg(any(test, feature = "fixtures"))]
    #[must_use]
    pub fn iter(&self) -> Rows<'_, ProcessRow> {
        let ProcessRows::Plain { rows, .. } = &self.rows else {
            panic!("a test reads the rows of a packed payload, which reads through `processes`");
        };

        Rows { rows: rows.iter() }
    }

    /// The App index of the process row at `row_index`: its App's place among the sample's Apps,
    /// or `None` where the process belongs to no App. Only a test reads it, as `iter`.
    #[cfg(any(test, feature = "fixtures"))]
    #[must_use]
    pub fn app_index(&self, row_index: usize) -> Option<u32> {
        let ProcessRows::Plain { app_indexes, .. } = &self.rows else {
            panic!("a test reads the App indexes of a packed payload");
        };

        app_indexes[row_index].get()
    }
}

impl<'second> Previous<'second> {
    fn of(second: Option<&'second AppsSecond>) -> Self {
        Self {
            payload: second.and_then(|second| second.processes.as_deref().ok()),
            app_rows: second.map_or(&[][..], |second| second.apps.as_slice()),
        }
    }
}

impl AppIndex {
    /// No App, a value no App index takes.
    const NONE: Self = Self(u32::MAX);

    fn of(app_index: Option<u32>) -> Self {
        match app_index {
            Some(index) => {
                assert!(index < Self::NONE.0);
                Self(index)
            }
            None => Self::NONE,
        }
    }

    fn get(self) -> Option<u32> {
        if self.0 < Self::NONE.0 {
            Some(self.0)
        } else {
            None
        }
    }
}

impl<Row> Shared<Row> {
    /// One allocation's charge: its two reference counts, the row and this reservation.
    const BYTES: usize = 2 * size_of::<usize>() + size_of::<Self>();

    /// `held` where it holds a row equal to `row`, and otherwise `row` admitted as a row of its
    /// own, so a second never takes a reading other than the one it read.
    fn share(
        row: Row,
        held: Option<&Arc<Self>>,
        budget: &Arc<ByteBudget>,
    ) -> Result<Arc<Self>, Missing>
    where
        Row: PartialEq,
    {
        match held {
            Some(shared) if shared.row == row => Ok(Arc::clone(shared)),
            _ => Self::admit(row, budget),
        }
    }

    fn admit(row: Row, budget: &Arc<ByteBudget>) -> Result<Arc<Self>, Missing> {
        let reservation = budget.reserve(Self::BYTES).ok_or(Missing::Budget)?;

        Ok(Arc::new(Self {
            row,
            _reservation: reservation,
        }))
    }
}

/// What a test of a tool outside the store builds a second's payload by hand through.
#[cfg(any(test, feature = "fixtures"))]
impl Payload {
    /// The payload of the second `selected` that holds no process rows and `details`, the
    /// additional measures of each of the second's Apps in key order, each shared with the one
    /// at the same index of `previous` where the two read the same, charged to `budget`.
    ///
    /// # Errors
    ///
    /// `Missing::Budget` where `budget` has no room for the payload.
    pub fn of_app_details(
        selected: Observation,
        details: Vec<AppDetails>,
        previous: Option<&Self>,
        budget: &Arc<ByteBudget>,
    ) -> Result<Self, Missing> {
        let reservation = budget
            .reserve(Self::header_bytes(details.len()))
            .ok_or(Missing::Budget)?;
        let held = previous.map_or(&[][..], |previous| previous.apps.as_slice());
        let apps = details
            .into_iter()
            .enumerate()
            .map(|(index, details)| Shared::share(details, held.get(index), budget))
            .collect::<Result<_, _>>()?;

        Ok(Self {
            selected,
            previous: None,
            rows: ProcessRows::Plain {
                rows: Vec::new(),
                app_indexes: Vec::new(),
            },
            apps,
            reservation,
        })
    }
}

#[cfg(any(test, feature = "fixtures"))]
impl<Row> Clone for Rows<'_, Row> {
    fn clone(&self) -> Self {
        Self {
            rows: self.rows.clone(),
        }
    }
}

#[cfg(any(test, feature = "fixtures"))]
impl<'payload, Row> Iterator for Rows<'payload, Row> {
    type Item = &'payload Row;

    fn next(&mut self) -> Option<Self::Item> {
        self.rows.next().map(|shared| &shared.row)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        self.rows.size_hint()
    }
}

#[cfg(any(test, feature = "fixtures"))]
impl<Row> ExactSizeIterator for Rows<'_, Row> {}

impl<'payload> Iterator for Members<'payload> {
    type Item = ProcessAt<'payload>;

    fn next(&mut self) -> Option<Self::Item> {
        let reading = match &mut self.members {
            MembersKind::Plain { rows, app_index } => {
                let app_index = *app_index;
                Reading::Row(
                    rows.find_map(|(shared, index)| (*index == app_index).then_some(&shared.row))?,
                )
            }
            MembersKind::Packed(members) => Reading::Packed(members.next()?),
        };

        Some(ProcessAt {
            reading,
            app_index: self.app_index,
        })
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.members {
            MembersKind::Plain { rows, .. } => (0, rows.size_hint().1),
            MembersKind::Packed(members) => members.size_hint(),
        }
    }
}

impl<'payload> Iterator for Places<'payload> {
    type Item = ProcessAt<'payload>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.places {
            PlacesKind::Plain(rows) => {
                let (shared, app_index) = rows.next()?;
                Some(ProcessAt {
                    reading: Reading::Row(&shared.row),
                    app_index: app_index.get(),
                })
            }
            PlacesKind::Packed(places) => {
                let (packed, app_index) = places.next()?;
                Some(ProcessAt {
                    reading: Reading::Packed(packed),
                    app_index,
                })
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match &self.places {
            PlacesKind::Plain(rows) => rows.size_hint(),
            PlacesKind::Packed(places) => places.size_hint(),
        }
    }
}

impl ExactSizeIterator for Places<'_> {}

#[cfg(any(test, feature = "fixtures"))]
impl<'payload> IntoIterator for &'payload Payload {
    type Item = &'payload ProcessRow;
    type IntoIter = Rows<'payload, ProcessRow>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

#[cfg(any(test, feature = "fixtures"))]
impl std::ops::Index<usize> for Payload {
    type Output = ProcessRow;

    fn index(&self, index: usize) -> &Self::Output {
        let ProcessRows::Plain { rows, .. } = &self.rows else {
            panic!("a test reads a row of a packed payload, which reads through `processes`");
        };

        &rows[index].row
    }
}

impl<'payload> ProcessAt<'payload> {
    #[must_use]
    pub fn pid(&self) -> i32 {
        match &self.reading {
            Reading::Row(row) => row.pid,
            Reading::Packed(packed) => packed.pid(),
        }
    }

    #[must_use]
    pub fn creation(&self) -> Option<ProcessCreation> {
        match &self.reading {
            Reading::Row(row) => row.creation,
            Reading::Packed(packed) => packed.creation(),
        }
    }

    #[must_use]
    pub fn executable_name(&self) -> Option<&'payload ProcessName> {
        match &self.reading {
            Reading::Row(row) => row.executable_name(),
            Reading::Packed(packed) => packed.executable_name(),
        }
    }

    #[must_use]
    pub fn program_unavailable(&self) -> Option<IdentityUnavailable> {
        match &self.reading {
            Reading::Row(row) => row.program_unavailable,
            Reading::Packed(packed) => packed.program_unavailable(),
        }
    }

    #[must_use]
    pub fn app_attribution(&self) -> Option<Unavailable> {
        match &self.reading {
            Reading::Row(row) => row.app_attribution,
            Reading::Packed(packed) => packed.app_attribution(),
        }
    }

    #[must_use]
    pub fn state(&self) -> ProcessState {
        match &self.reading {
            Reading::Row(row) => row.state,
            Reading::Packed(packed) => packed.state(),
        }
    }

    /// The App index the second gives the process: its App's place among the second's Apps, or
    /// `None` where the process belongs to no App.
    #[must_use]
    pub fn app_index(&self) -> Option<u32> {
        self.app_index
    }

    #[must_use]
    pub fn cpu_ns(&self) -> Amount {
        self.amount(Measured::Cpu)
    }

    #[must_use]
    pub fn energy_nj(&self) -> Amount {
        self.amount(Measured::Energy)
    }

    #[must_use]
    pub fn gpu_ns(&self) -> Amount {
        self.amount(Measured::Gpu)
    }

    #[must_use]
    pub fn disk_read_bytes(&self) -> Amount {
        self.amount(Measured::DiskRead)
    }

    #[must_use]
    pub fn disk_written_bytes(&self) -> Amount {
        self.amount(Measured::DiskWritten)
    }

    #[must_use]
    pub fn disk_io_bytes(&self) -> Amount {
        self.amount(Measured::DiskIo)
    }

    #[must_use]
    pub fn resident_bytes(&self) -> Amount {
        self.amount(Measured::Resident)
    }

    #[must_use]
    pub fn footprint_bytes(&self) -> Amount {
        self.amount(Measured::Footprint)
    }

    #[must_use]
    pub fn idle_wakeups(&self) -> Amount {
        self.amount(Measured::IdleWakeups)
    }

    #[must_use]
    pub fn pageins(&self) -> Amount {
        self.amount(Measured::Pageins)
    }

    #[must_use]
    pub fn threads(&self) -> Amount {
        self.amount(Measured::Threads)
    }

    fn amount(&self, measured: Measured) -> Amount {
        match &self.reading {
            Reading::Row(row) => row.compact(measured).amount(),
            Reading::Packed(packed) => packed.compact(measured).amount(),
        }
    }

    /// Where the row the process reads from lies in memory: its own row in a plain second, and
    /// its first row in the block in a packed one. A tool that measures the store tells the rows
    /// it holds apart by it.
    #[must_use]
    pub fn row_address(&self) -> usize {
        match &self.reading {
            Reading::Row(row) => std::ptr::from_ref(*row).addr(),
            Reading::Packed(packed) => packed.row_address(),
        }
    }

    /// The process as a page shows it, in its own App.
    #[must_use]
    pub(crate) fn presentation(&self) -> crate::live_app_response::json::ProcessView<'payload> {
        crate::live_app_response::json::ProcessView {
            pid: self.pid(),
            creation: self.creation(),
            app_index: self.app_index,
            executable_name: self
                .executable_name()
                .map(|name| (name.bytes.as_slice(), name.display.as_str())),
            program_unavailable: self.program_unavailable(),
            cpu_ns: self.cpu_ns(),
            energy_nj: self.energy_nj(),
            gpu_ns: self.gpu_ns(),
            disk_read_bytes: self.disk_read_bytes(),
            disk_written_bytes: self.disk_written_bytes(),
            disk_io_bytes: self.disk_io_bytes(),
            resident_bytes: self.resident_bytes(),
            footprint_bytes: self.footprint_bytes(),
            idle_wakeups: self.idle_wakeups(),
            pageins: self.pageins(),
            threads: self.threads(),
            app_attribution: self.app_attribution(),
            state: self.state(),
        }
    }
}

/// Two processes at a second are equal where every reading and the App index are, whether
/// each is read from a row or from a packed block.
impl PartialEq for ProcessAt<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.readings() == other.readings()
    }
}

/// Every reading of a process at a second and its App index, by which two processes compare.
#[derive(PartialEq)]
struct ProcessReadings<'payload> {
    pid: i32,
    creation: Option<ProcessCreation>,
    executable_name: Option<&'payload ProcessName>,
    program_unavailable: Option<IdentityUnavailable>,
    /// Each amount at its place in `Measured::ALL`.
    amounts: [Amount; Measured::ALL.len()],
    app_attribution: Option<Unavailable>,
    state: ProcessState,
    app_index: Option<u32>,
}

impl<'payload> ProcessAt<'payload> {
    /// Every reading of the process, read from its row field by field or through its packed
    /// block's readers.
    fn readings(&self) -> ProcessReadings<'payload> {
        let Reading::Row(row) = self.reading else {
            return ProcessReadings {
                pid: self.pid(),
                creation: self.creation(),
                executable_name: self.executable_name(),
                program_unavailable: self.program_unavailable(),
                amounts: Measured::ALL.map(|measured| self.amount(measured)),
                app_attribution: self.app_attribution(),
                state: self.state(),
                app_index: self.app_index,
            };
        };
        // Every field is named, so a field added to a row is one the comparison has to read,
        // and so one the packed readers have to give.
        let ProcessRow {
            pid,
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
        let mut amounts = [Amount::Known(0); Measured::ALL.len()];
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
            amounts[measured.index()] = amount.amount();
        }

        ProcessReadings {
            pid: *pid,
            creation: *creation,
            executable_name: executable_name.as_deref(),
            program_unavailable: *program_unavailable,
            amounts,
            app_attribution: *app_attribution,
            state: *state,
            app_index: self.app_index,
        }
    }
}

impl ProcessRow {
    /// The amount of `measured` as the row holds it.
    fn compact(&self, measured: Measured) -> CompactAmount {
        match measured {
            Measured::Cpu => self.cpu_ns,
            Measured::Energy => self.energy_nj,
            Measured::Gpu => self.gpu_ns,
            Measured::DiskRead => self.disk_read_bytes,
            Measured::DiskWritten => self.disk_written_bytes,
            Measured::DiskIo => self.disk_io_bytes,
            Measured::Resident => self.resident_bytes,
            Measured::Footprint => self.footprint_bytes,
            Measured::IdleWakeups => self.idle_wakeups,
            Measured::Pageins => self.pageins,
            Measured::Threads => self.threads,
        }
    }

    #[cfg(any(test, feature = "fixtures"))]
    #[must_use]
    pub fn cpu_ns(&self) -> Amount {
        self.cpu_ns.amount()
    }

    #[cfg(any(test, feature = "fixtures"))]
    #[must_use]
    pub fn energy_nj(&self) -> Amount {
        self.energy_nj.amount()
    }

    #[cfg(any(test, feature = "fixtures"))]
    #[must_use]
    pub fn gpu_ns(&self) -> Amount {
        self.gpu_ns.amount()
    }

    #[cfg(any(test, feature = "fixtures"))]
    #[must_use]
    pub fn disk_read_bytes(&self) -> Amount {
        self.disk_read_bytes.amount()
    }

    #[cfg(any(test, feature = "fixtures"))]
    #[must_use]
    pub fn disk_written_bytes(&self) -> Amount {
        self.disk_written_bytes.amount()
    }

    #[cfg(any(test, feature = "fixtures"))]
    #[must_use]
    pub fn disk_io_bytes(&self) -> Amount {
        self.disk_io_bytes.amount()
    }

    #[cfg(any(test, feature = "fixtures"))]
    #[must_use]
    pub fn resident_bytes(&self) -> Amount {
        self.resident_bytes.amount()
    }

    #[cfg(any(test, feature = "fixtures"))]
    #[must_use]
    pub fn footprint_bytes(&self) -> Amount {
        self.footprint_bytes.amount()
    }

    #[cfg(any(test, feature = "fixtures"))]
    #[must_use]
    pub fn idle_wakeups(&self) -> Amount {
        self.idle_wakeups.amount()
    }

    #[cfg(any(test, feature = "fixtures"))]
    #[must_use]
    pub fn pageins(&self) -> Amount {
        self.pageins.amount()
    }

    #[cfg(any(test, feature = "fixtures"))]
    #[must_use]
    pub fn threads(&self) -> Amount {
        self.threads.amount()
    }

    /// The name remains borrowed from its shared reservation owner.
    /// ```compile_fail
    /// use viewer::store::{ProcessName, ProcessRow};
    /// fn escape(row: &ProcessRow) -> Option<&'static ProcessName> {
    ///     row.executable_name()
    /// }
    /// ```
    #[must_use]
    pub fn executable_name(&self) -> Option<&ProcessName> {
        self.executable_name.as_deref()
    }

    fn capture(process: &Process, executable_name: Option<Arc<ProcessName>>) -> Self {
        Self {
            pid: process.pid,
            creation: process.creation,
            executable_name,
            program_unavailable: process.program_unavailable,
            cpu_ns: CompactAmount::capture(process.cpu_ns),
            energy_nj: CompactAmount::capture(process.energy_nj),
            gpu_ns: CompactAmount::capture(process.gpu_ns),
            disk_read_bytes: CompactAmount::capture(process.disk_read_bytes),
            disk_written_bytes: CompactAmount::capture(process.disk_written_bytes),
            disk_io_bytes: CompactAmount::capture(process.disk_io_bytes),
            resident_bytes: CompactAmount::capture(process.resident_bytes),
            footprint_bytes: CompactAmount::capture(process.footprint_bytes),
            idle_wakeups: CompactAmount::capture(process.idle_wakeups),
            pageins: CompactAmount::capture(process.pageins),
            threads: CompactAmount::capture(process.threads),
            app_attribution: process.app_attribution,
            state: process.state,
        }
    }
}

impl ProcessName {
    const OWNER_BYTES: usize = 2 * size_of::<usize>() + size_of::<Self>();

    /// Adopts buffers allocated under this reservation, including their spare capacity.
    /// The caller reserves before allocating the buffers and the enclosing Arc.
    #[must_use]
    pub fn from_reserved(name: Bytes, mut reservation: ByteReservation) -> Self {
        let Some(bytes) = Self::OWNER_BYTES
            .checked_add(name.bytes.capacity())
            .and_then(|bytes| bytes.checked_add(name.display.capacity()))
        else {
            panic!("process name capacity exceeds address space");
        };

        assert!(bytes <= reservation.bytes());
        reservation.shrink_to(bytes);

        Self { name, reservation }
    }

    fn capture(name: &Bytes, budget: &Arc<ByteBudget>) -> Result<Arc<Self>, Missing> {
        let bytes = Self::OWNER_BYTES
            .checked_add(name.bytes.len())
            .and_then(|bytes| bytes.checked_add(name.display.len()))
            .ok_or(Missing::Budget)?;
        let reservation = budget.reserve(bytes).ok_or(Missing::Budget)?;

        Ok(Arc::new(Self::from_reserved(name.clone(), reservation)))
    }

    /// This shared allocation's charge, counted once regardless of its retaining payloads.
    #[must_use]
    pub fn reserved_bytes(&self) -> usize {
        self.reservation.bytes()
    }
}

impl Deref for ProcessName {
    type Target = Bytes;

    fn deref(&self) -> &Self::Target {
        &self.name
    }
}

impl fmt::Debug for ProcessName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ProcessName")
            .field("bytes", &self.bytes)
            .field("display", &self.display)
            .finish()
    }
}

impl PartialEq for ProcessName {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes && self.display == other.display
    }
}

impl Eq for ProcessName {}

impl CompactAmount {
    fn capture(amount: Amount) -> Self {
        match amount {
            Amount::Known(value) => Self {
                known: value.to_le_bytes(),
                unavailable: None,
            },
            Amount::Unavailable(reason) => Self {
                known: [0; 8],
                unavailable: Some(reason),
            },
        }
    }

    fn amount(self) -> Amount {
        match self.unavailable {
            Some(reason) => Amount::Unavailable(reason),
            None => Amount::Known(u64::from_le_bytes(self.known)),
        }
    }
}

impl PartialEq for Payload {
    fn eq(&self, other: &Self) -> bool {
        self.selected == other.selected
            && self.previous == other.previous
            && self.len() == other.len()
            && self.processes().eq(other.processes())
            && self.apps().eq(other.apps())
    }
}

const _: () = {
    assert!(size_of::<AppIndex>() == 4);
    // Every App index is below `AppIndex::NONE`, compared by bit length, since a `const` cannot
    // widen a `u32` to a `usize` but by a cast.
    assert!(super::APPS_PER_SECOND_MAX.ilog2() < AppIndex::NONE.0.ilog2());
    assert!(size_of::<ProcessRow>() <= 144);
    assert!(size_of::<Shared<ProcessRow>>() <= 160);
    assert!(size_of::<CompactAmount>() <= 9);
    assert!(align_of::<CompactAmount>() == 1);
    assert!(size_of::<AppDetails>() <= 80);
    assert!(size_of::<Shared<AppDetails>>() <= 96);
};

#[cfg(test)]
#[path = "payload_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "payload_name_tests.rs"]
mod name_tests;

#[cfg(test)]
#[path = "payload_namespace_tests.rs"]
mod namespace_tests;

#[cfg(test)]
#[path = "payload_row_tests.rs"]
pub(super) mod row_tests;

#[cfg(test)]
#[path = "payload_app_tests.rs"]
mod app_tests;
