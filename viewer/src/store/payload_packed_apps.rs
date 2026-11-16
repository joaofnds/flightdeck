//! The Apps of a packed block: each App's first row and first details, the changes of their
//! fields as the processes' changes are held, and the list of each second's changes that a
//! reader walking the block's seconds in order applies to the rows it keeps. A block packs its
//! App rows only where packed they take less than the share the packer is handed of what its
//! seconds hold them in plain, and otherwise each second keeps its own rows and details.

use std::cmp::Ordering;
use std::sync::Arc;

use super::super::{APP_DETAILS_WORDS, AppDetails, Payload, Shared};
use super::{BlockTable, Parts, seconds_through};
use crate::live_app_response::{KeyKind, RankedApp};
use crate::store::apps::{APP_ROW_WORD_MEMBERS, APP_ROW_WORDS, app_row_word_members};
use crate::store::block::block_u32;
use crate::store::{AppRow, AppsSecond};

/// One App of a packed block, at the place its key takes among the block's Apps.
#[derive(Debug)]
pub(super) struct BlockApp {
    /// The App's row at the first second that holds it, held whole. Its name is the one
    /// allocation every second of the block names the App by.
    pub(super) first: AppRow,
    /// The seconds of the block that hold the App.
    pub(super) seconds: u64,
    /// The words of the row that change inside the block, one bit each.
    changing: u32,
    /// The change record of the row's first changing word.
    changes_start: u32,
    /// The App's details at the first second that holds the App and its process rows, and their
    /// changes, or `None` where no such second does.
    details: Option<BlockAppDetails>,
}

#[derive(Debug)]
struct BlockAppDetails {
    first: AppDetails,
    changing: u32,
    changes_start: u32,
}

/// One change a reader that walks the block's seconds in order makes to the rows it keeps as it
/// moves to the next second.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AppStep {
    /// The App at the place comes at the second, absent from the second before.
    Arrive(u32),
    /// The App at the place is absent from the second, held at the second before.
    Leave(u32),
    /// The word `word` of the row of the App at `place`, held at both seconds and at `index`
    /// among the Apps of the second, reads `value`. A second lists its changes after every App
    /// that came or went, so `index` names the row once those are applied.
    Change {
        place: u32,
        index: u32,
        word: u32,
        value: u64,
    },
}

const _: () = {
    assert!(size_of::<AppStep>() <= 24);
};

/// The Apps of a block as the packer adds them to the table's parts.
pub(super) struct AppParts {
    pub(super) apps: Vec<BlockApp>,
    /// For each second, how many Apps it holds.
    pub(super) counts: Vec<u32>,
    /// For each second, where its steps start in `steps`, and after the last the end of its
    /// steps.
    pub(super) step_starts: Vec<u32>,
    pub(super) steps: Vec<AppStep>,
    /// The seconds of the block that hold their process rows, and so their App details.
    pub(super) detailed_seconds: u64,
    /// Whether the parts hold the Apps' rows and details, or each second holds its own.
    pub(super) rows_packed: bool,
    /// The bytes the block's Apps take in each form, which the packer compared.
    pub(super) bytes: AppRowsBytes,
}

/// The bytes a block's Apps take packed and held plain, each counting what its form holds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(in crate::store) struct AppRowsBytes {
    pub(in crate::store) packed: usize,
    pub(in crate::store) plain: usize,
}

/// The share of the bytes a block's Apps take held plain under which the block packs them: it
/// packs them only where the bytes they take packed are less than `numerator / denominator` of
/// the bytes they take plain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::store) struct AppRowsShare {
    pub(in crate::store) numerator: u64,
    pub(in crate::store) denominator: u64,
}

/// The share the store packs a block's App rows under. A reader that walks packed rows pays for
/// every changed word, so a block whose Apps change at most seconds saves too little to pay for
/// it, and a block packs them only where they take under a third of their plain bytes.
pub(in crate::store) const APP_ROWS_PACKED_SHARE: AppRowsShare = AppRowsShare {
    numerator: 1,
    denominator: 3,
};

/// The share a test hands the packer so that every block packs its App rows, whatever they save.
#[cfg(test)]
pub(in crate::store) const APP_ROWS_PACKED_ALWAYS: AppRowsShare = AppRowsShare {
    numerator: 1,
    denominator: 0,
};

impl AppRowsShare {
    /// Whether a block whose Apps take `bytes` packs them under this share.
    pub(in crate::store) fn admits(self, bytes: AppRowsBytes) -> bool {
        let wide = |bytes: usize| {
            u128::try_from(bytes).unwrap_or_else(|_| unreachable!("a size fits 128 bits"))
        };

        wide(bytes.packed) * u128::from(self.denominator)
            < wide(bytes.plain) * u128::from(self.numerator)
    }
}

/// One changing word of an App's row, as `app_rows_over` walks the App's seconds.
#[derive(Clone, Copy, Default)]
struct AppRowsOverWord {
    /// The seconds of the block at which the word changes.
    changes: u64,
    /// Where the word's next value is among the table's values.
    next: usize,
    /// The word's index in the row.
    word: usize,
}

/// One App of the block at one second.
#[derive(Clone, Copy)]
struct PresentApp {
    /// The second's index in the block.
    second_index: u32,
    /// The App's index among the second's Apps.
    index: u32,
    /// The words of the App's row at the second.
    words: [u64; APP_ROW_WORDS],
    /// The words of the App's details, where the second holds its process rows.
    details: Option<[u64; APP_DETAILS_WORDS]>,
}

/// Adds to `parts` the Apps of `seconds` in key order, each with its first row and the changes of
/// its words, and the steps of each second, from `places`, the place of each App of each second,
/// as `pack_apps` found them.
pub(super) fn pack_app_rows(
    seconds: &[Arc<AppsSecond>],
    places: &[Vec<u32>],
    app_count: usize,
    parts: &mut Parts,
) -> AppParts {
    let mut present: Vec<Vec<PresentApp>> = (0..app_count).map(|_| Vec::new()).collect();
    let mut firsts: Vec<Option<&AppRow>> = vec![None; app_count];
    let mut detailed_seconds = 0;
    for (second_index, (second, places)) in (0..).zip(seconds.iter().zip(places)) {
        let payload = second.processes.as_ref().ok();
        if payload.is_some() {
            detailed_seconds |= 1 << second_index;
        }
        for (index, (row, &place)) in second.apps.iter().zip(places).enumerate() {
            let place = collector::usize_from_u32(place);
            let details = payload.map(|payload| payload.app(index).words());
            present[place].push(PresentApp {
                second_index,
                index: block_u32(index),
                words: row.words(),
                details,
            });
            firsts[place].get_or_insert(row);
        }
    }

    let second_count = seconds.len();
    let mut steps: Vec<Vec<AppStep>> = (0..second_count).map(|_| Vec::new()).collect();
    let mut apps = Vec::with_capacity(app_count);
    for (place, (present, first)) in (0..).zip(present.iter().zip(firsts)) {
        let Some(first) = first else {
            unreachable!("every App of the block is held at a second");
        };
        let mut app_seconds = 0;
        for app in present {
            app_seconds |= 1 << app.second_index;
        }
        pack_app_rows_presence(place, app_seconds, second_count, &mut steps);
        let rows: Vec<(u32, [u64; APP_ROW_WORDS])> = present
            .iter()
            .map(|app| (app.second_index, app.words))
            .collect();
        let (changing, changes_start) = pack_app_rows_record(&rows, parts);
        if changing != 0 {
            pack_app_rows_steps(place, present, &mut steps);
        }
        apps.push(BlockApp {
            first: first.clone(),
            seconds: app_seconds,
            changing,
            changes_start,
            details: pack_app_rows_details(present, parts),
        });
    }
    let (step_starts, flat) = pack_app_rows_flat(steps);

    AppParts {
        apps,
        counts: seconds
            .iter()
            .map(|second| block_u32(second.apps.len()))
            .collect(),
        step_starts,
        steps: flat,
        detailed_seconds,
        rows_packed: true,
        bytes: AppRowsBytes::default(),
    }
}

/// The details of the App `present` at each second that holds it, oldest first, as their first
/// words and the record of their changes added to `parts`, or `None` where no second that holds
/// the App holds its process rows.
fn pack_app_rows_details(present: &[PresentApp], parts: &mut Parts) -> Option<BlockAppDetails> {
    let details: Vec<(u32, [u64; APP_DETAILS_WORDS])> = present
        .iter()
        .filter_map(|app| Some((app.second_index, app.details?)))
        .collect();

    details.first().map(|&(_, first_words)| {
        let (changing, changes_start) = pack_app_rows_record(&details, parts);
        BlockAppDetails {
            first: AppDetails::of_words(&first_words),
            changing,
            changes_start,
        }
    })
}

/// The start of each second's steps in one list, and that list, from `steps`, each second's
/// steps, where a second's arrivals and departures come before its changes, so a change's index
/// is the row's position at that second.
fn pack_app_rows_flat(steps: Vec<Vec<AppStep>>) -> (Vec<u32>, Vec<AppStep>) {
    let mut step_starts = Vec::with_capacity(steps.len() + 1);
    let mut flat = Vec::with_capacity(steps.iter().map(Vec::len).sum());
    for second_steps in steps {
        step_starts.push(block_u32(flat.len()));
        let (changes, moves): (Vec<AppStep>, Vec<AppStep>) = second_steps
            .into_iter()
            .partition(|step| matches!(step, AppStep::Change { .. }));
        flat.extend(moves);
        flat.extend(changes);
    }
    step_starts.push(block_u32(flat.len()));

    (step_starts, flat)
}

/// The App parts `packed`, as `pack_app_rows` added them to `parts`, where they pay for
/// themselves against the App rows and details the plain `seconds` hold, the place of each of
/// their Apps in `places`. Otherwise the parts only place the Apps, each second keeps its own
/// rows and details, and the records `pack_app_rows` added to `parts` are taken out.
pub(super) fn pack_app_rows_where_paying(
    mut packed: AppParts,
    seconds: &[Arc<AppsSecond>],
    places: &[Vec<u32>],
    parts: &mut Parts,
    share: AppRowsShare,
) -> AppParts {
    assert!(packed.rows_packed);
    // The App records are the first `pack_app_rows` adds, before any process's, and the list of
    // Apps and each second's count of them are held in either form.
    assert!(parts.processes.is_empty());
    let held_bytes = size_of_val(&*packed.apps) + size_of_val(&*packed.counts);
    packed.bytes = AppRowsBytes {
        packed: held_bytes
            + size_of_val(&*packed.step_starts)
            + size_of_val(&*packed.steps)
            + size_of_val(&*parts.change_seconds)
            + size_of_val(&*parts.change_starts)
            + size_of_val(&*parts.values),
        plain: held_bytes + pack_app_rows_plain_bytes(seconds, places, packed.apps.len()),
    };
    if share.admits(packed.bytes) {
        return packed;
    }

    parts.change_seconds.clear();
    parts.change_starts.clear();
    parts.values.clear();
    for app in &mut packed.apps {
        app.changing = 0;
        app.changes_start = 0;
        app.details = None;
    }
    packed.step_starts.clear();
    packed.steps.clear();
    packed.rows_packed = false;

    packed
}

/// The bytes of the App rows and details of the plain `seconds` as a packed block that does not
/// pack them holds them: a copy of each second's rows, a reference to each App's details, and
/// each details allocation once for the seconds in a row that share it. `places` holds the
/// place of each App of each second among the block's `app_count` Apps.
fn pack_app_rows_plain_bytes(
    seconds: &[Arc<AppsSecond>],
    places: &[Vec<u32>],
    app_count: usize,
) -> usize {
    let mut held: Vec<Option<&Arc<Shared<AppDetails>>>> = vec![None; app_count];
    let mut bytes = 0;
    for (second, places) in seconds.iter().zip(places) {
        bytes += second.apps.len() * size_of::<AppRow>();
        let Ok(payload) = &second.processes else {
            continue;
        };
        bytes += payload.apps.len() * Payload::APP_DETAILS_REFERENCE_BYTES;
        for (details, &place) in payload.apps.iter().zip(places) {
            let last = &mut held[collector::usize_from_u32(place)];
            if !last.is_some_and(|last| Arc::ptr_eq(last, details)) {
                bytes += Payload::APP_DETAILS_BYTES;
            }
            *last = Some(details);
        }
    }

    bytes
}

/// Adds to `steps` the coming and going of the App at `place`, held at `app_seconds` of the
/// block's `second_count` seconds.
fn pack_app_rows_presence(
    place: u32,
    app_seconds: u64,
    second_count: usize,
    steps: &mut [Vec<AppStep>],
) {
    for (second_index, steps) in steps.iter_mut().enumerate().take(second_count).skip(1) {
        let held = app_seconds & (1 << second_index) != 0;
        let held_before = app_seconds & (1 << (second_index - 1)) != 0;
        if held && !held_before {
            steps.push(AppStep::Arrive(place));
        }
        if !held && held_before {
            steps.push(AppStep::Leave(place));
        }
    }
}

/// Adds to `steps` each change of the App at `place` between two seconds in a row that hold it,
/// from `present`, the App at each second that holds it, oldest first.
fn pack_app_rows_steps(place: u32, present: &[PresentApp], steps: &mut [Vec<AppStep>]) {
    for pair in present.windows(2) {
        let (before, app) = (pair[0], pair[1]);
        if before.second_index + 1 != app.second_index {
            continue;
        }
        for (word, (value, before)) in (0..).zip(app.words.iter().zip(&before.words)) {
            if value != before {
                steps[collector::usize_from_u32(app.second_index)].push(AppStep::Change {
                    place,
                    index: app.index,
                    word,
                    value: *value,
                });
            }
        }
    }
}

/// Adds to `parts` the change records of the words `rows` holds at each second, oldest first,
/// each record the seconds at which its word differs from the second before that holds the
/// App, and returns the mask of the words that change and where their records start.
fn pack_app_rows_record<const WORDS: usize>(
    rows: &[(u32, [u64; WORDS])],
    parts: &mut Parts,
) -> (u32, u32) {
    let changes_start = block_u32(parts.change_seconds.len());
    let mut changing = 0;
    for word in 0..WORDS {
        let start = block_u32(parts.values.len());
        let mut record = 0;
        for pair in rows.windows(2) {
            let ((_, before), (second_index, words)) = (&pair[0], &pair[1]);
            if words[word] != before[word] {
                record |= 1 << second_index;
                parts.values.push(words[word]);
            }
        }
        if record != 0 {
            changing |= 1 << word;
            parts.change_seconds.push(record);
            parts.change_starts.push(start);
        }
    }

    (changing, changes_start)
}

impl BlockTable {
    /// Whether the table holds its Apps' rows and details, which each second otherwise holds.
    pub(in crate::store) fn app_rows_packed(&self) -> bool {
        self.app_rows_packed
    }

    /// The bytes the block's Apps take in each form, which the packer compared.
    #[cfg(test)]
    pub(in crate::store) fn app_rows_bytes(&self) -> AppRowsBytes {
        self.app_rows_bytes
    }

    /// How many Apps the second at `second_index` holds.
    pub(in crate::store) fn app_count(&self, second_index: u32) -> usize {
        collector::usize_from_u32(self.app_counts[collector::usize_from_u32(second_index)])
    }

    /// Whether the second at `second_index` holds its process rows, and so its Apps' details.
    pub(in crate::store) fn detailed(&self, second_index: u32) -> bool {
        self.detailed_seconds & (1 << second_index) != 0
    }

    /// The place of the App of kind `kind` named `bytes` among the block's Apps, where the
    /// second at `second_index` holds it.
    pub(in crate::store) fn app_place_of(
        &self,
        second_index: u32,
        kind: KeyKind,
        bytes: &[u8],
    ) -> Option<u32> {
        let key = kind.key(bytes);
        let place = self
            .apps
            .binary_search_by(|app| app.first.name.kind.key(&app.first.name.bytes).cmp(&key))
            .ok()?;
        if self.apps[place].seconds & (1 << second_index) == 0 {
            return None;
        }

        Some(block_u32(place))
    }

    /// The key and name of the App at `place` among the block's Apps.
    pub(in crate::store) fn app_name(&self, place: u32) -> &Arc<RankedApp> {
        &self.block_app(place).first.name
    }

    /// The App at `place` among the block's Apps.
    pub(super) fn block_app(&self, place: u32) -> &BlockApp {
        &self.apps[collector::usize_from_u32(place)]
    }

    /// The place of the App at `index` among the Apps of the second at `second_index`.
    pub(in crate::store) fn app_place_at(&self, second_index: u32, index: usize) -> Option<u32> {
        let bit = 1 << second_index;
        let place = self
            .apps
            .iter()
            .enumerate()
            .filter(|(_, app)| app.seconds & bit != 0)
            .nth(index)?
            .0;

        Some(block_u32(place))
    }

    /// The index among the Apps of the second at `second_index` of the App at `place`, which
    /// the second holds.
    pub(in crate::store) fn app_index_of(&self, second_index: u32, place: u32) -> usize {
        let bit = 1 << second_index;
        assert!(self.block_app(place).seconds & bit != 0);

        self.apps[..collector::usize_from_u32(place)]
            .iter()
            .filter(|app| app.seconds & bit != 0)
            .count()
    }

    /// The row of the App at `place` at the second at `second_index`, which holds it.
    pub(in crate::store) fn app_row(&self, place: u32, second_index: u32) -> AppRow {
        let app = self.block_app(place);
        assert!(app.seconds & (1 << second_index) != 0);
        let mut row = app.first.clone();
        self.app_readings(app, second_index, &mut row);

        row
    }

    /// How many processes the App at `place` holds at the second at `second_index`, which holds
    /// the App, read from its members word alone with no row rebuilt.
    pub(in crate::store) fn app_members(&self, place: u32, second_index: u32) -> u32 {
        let app = self.block_app(place);
        assert!(app.seconds & (1 << second_index) != 0);
        let mut members = app.first.members;
        // A record holds one changing word each, in the order of the words, so the members
        // word's record, where it changes, is the App's first.
        self.app_changes(
            app.changing & (1 << APP_ROW_WORD_MEMBERS),
            app.changes_start,
            second_index,
            |word, value| {
                assert_eq!(word, APP_ROW_WORD_MEMBERS);
                members = app_row_word_members(value);
            },
        );

        members
    }

    /// The details of the App at `place` at the second at `second_index`, which holds the App
    /// and its process rows.
    pub(in crate::store) fn app_details(&self, place: u32, second_index: u32) -> AppDetails {
        assert!(self.detailed(second_index));
        let app = self.block_app(place);
        assert!(app.seconds & (1 << second_index) != 0);
        let Some(details) = &app.details else {
            unreachable!("an App held at a second with process rows has details");
        };
        let mut words = details.first.words();
        self.app_changes(
            details.changing,
            details.changes_start,
            second_index,
            |word, value| words[word] = value,
        );

        AppDetails::of_words(&words)
    }

    /// The details of every App of the second at `second_index`, in key order, or none where
    /// the second holds no process rows.
    pub(in crate::store) fn app_details_of(&self, second_index: u32) -> PackedDetails<'_> {
        let remaining = if self.detailed(second_index) {
            self.app_count(second_index)
        } else {
            0
        };

        PackedDetails {
            table: self,
            second_index,
            place: 0,
            remaining,
        }
    }

    /// Calls `set` with each word, and its value, of a record whose changing words are
    /// `changing`, from `changes_start`, that differs at the second at `second_index` from the
    /// first row's.
    fn app_changes(
        &self,
        changing: u32,
        changes_start: u32,
        second_index: u32,
        mut set: impl FnMut(usize, u64),
    ) {
        let through = seconds_through(second_index);
        let mut remaining = changing;
        let first_record = collector::usize_from_u32(changes_start);
        // A record has at most 32 changing words, one bit each of `changing`.
        for record in first_record..first_record + collector::usize_from_u32(u32::BITS) {
            if remaining == 0 {
                return;
            }
            let word = remaining.trailing_zeros();
            remaining &= remaining - 1;
            let count = (self.change_seconds[record] & through).count_ones();
            if count > 0 {
                let start = self.change_starts[record];
                set(
                    collector::usize_from_u32(word),
                    self.values[collector::usize_from_u32(start + count - 1)],
                );
            }
        }
        assert_eq!(remaining, 0);
    }

    /// Calls `each` with the index, the row and the details of each App of the second at
    /// `second_index`, which holds its process rows, whose index among the second's Apps is in
    /// `range`, in key order, reading the block's Apps once.
    pub(in crate::store) fn app_rows_with_details(
        &self,
        second_index: u32,
        range: std::ops::Range<usize>,
        mut each: impl FnMut(usize, &AppRow, AppDetails),
    ) {
        assert!(self.detailed(second_index));
        let bit = 1 << second_index;
        let mut index = 0;
        for (place, app) in (0..).zip(&*self.apps) {
            if index == range.end {
                return;
            }
            if app.seconds & bit == 0 {
                continue;
            }
            if index >= range.start {
                let row = self.app_row(place, second_index);
                each(index, &row, self.app_details(place, second_index));
            }
            index += 1;
        }
    }

    /// Calls `each` with the name of each App of the second at `second_index` whose index among
    /// the second's Apps is in `range`, in key order, reading no row.
    pub(in crate::store) fn app_names_in(
        &self,
        second_index: u32,
        range: std::ops::Range<usize>,
        each: impl FnMut(&RankedApp),
    ) {
        assert!(self.app_rows_packed);
        assert!(range.end <= self.app_count(second_index));
        let bit = 1 << second_index;
        self.apps
            .iter()
            .filter(|app| app.seconds & bit != 0)
            .skip(range.start)
            .take(range.len())
            .map(|app| &*app.first.name)
            .for_each(each);
    }

    /// Calls `each` with the name and the readings of each App of the second at `second_index`,
    /// in key order, as `rebuild_app_rows` reads them, into one row whose name is not the
    /// App's, so no reference count is touched.
    pub(in crate::store) fn app_readings_of(
        &self,
        second_index: u32,
        mut each: impl FnMut(&Arc<RankedApp>, &AppRow),
    ) {
        assert!(self.app_rows_packed);
        let bit = 1 << second_index;
        let mut held = self.apps.iter().filter(|app| app.seconds & bit != 0);
        let Some(first) = held.next() else {
            return;
        };
        let mut readings = first.first.clone();
        for app in std::iter::once(first).chain(held) {
            self.app_readings(app, second_index, &mut readings);
            each(&app.first.name, &readings);
        }
    }

    /// Sets every reading of `row` but its name to the readings of `app`, an App of the block,
    /// at the second at `second_index`: its first row's with each change through that second.
    fn app_readings(&self, app: &BlockApp, second_index: u32, row: &mut AppRow) {
        row.copy_readings(&app.first);
        self.app_changes(
            app.changing,
            app.changes_start,
            second_index,
            |word, value| {
                row.set_word(word, value);
            },
        );
    }

    /// How many Apps the block holds over all its seconds.
    pub(in crate::store) fn block_app_count(&self) -> u32 {
        block_u32(self.apps.len())
    }

    /// Calls `each` with each second of `seconds`, a mask of the block's seconds, that holds the
    /// App at `place`, oldest first, and the App's row at that second, reading the App's first
    /// row and its changes once.
    pub(in crate::store) fn app_rows_over(
        &self,
        place: u32,
        seconds: u64,
        mut each: impl FnMut(u32, &AppRow),
    ) {
        assert!(self.app_rows_packed);
        let app = self.block_app(place);
        let read = app.seconds & seconds;
        if read == 0 {
            return;
        }
        let mut records = [AppRowsOverWord::default(); APP_ROW_WORDS];
        let mut record_count = 0;
        let mut remaining = app.changing;
        let first_record = collector::usize_from_u32(app.changes_start);
        for record in first_record..first_record + collector::usize_from_u32(u32::BITS) {
            if remaining == 0 {
                break;
            }
            let word = remaining.trailing_zeros();
            remaining &= remaining - 1;
            records[record_count] = AppRowsOverWord {
                changes: self.change_seconds[record],
                next: collector::usize_from_u32(self.change_starts[record]),
                word: collector::usize_from_u32(word),
            };
            record_count += 1;
        }
        assert_eq!(remaining, 0);
        let records = &mut records[..record_count];

        let mut row = app.first.clone();
        // The App's seconds through the last one read, since a change at a second not read is a
        // change of every second after it.
        let mut held = app.seconds & seconds_through(read.ilog2());
        for _ in 0..u64::BITS {
            if held == 0 {
                return;
            }
            let second_index = held.trailing_zeros();
            held &= held - 1;
            for record in records.iter_mut() {
                if record.changes & (1 << second_index) != 0 {
                    row.set_word(record.word, self.values[record.next]);
                    record.next += 1;
                }
            }
            if read & (1 << second_index) != 0 {
                each(second_index, &row);
            }
        }
        assert_eq!(held, 0);
    }

    /// Makes `rows`, with the App place of each in `places`, the Apps of the second at
    /// `second_index` in key order. A row whose name is the allocation of the App it is to show
    /// is written in place, so no App that stays touches a reference count, and the rows of
    /// other Apps are taken out or put in.
    pub(in crate::store) fn rebuild_app_rows(
        &self,
        second_index: u32,
        rows: &mut Vec<AppRow>,
        places: &mut Vec<u32>,
        spare: &mut Vec<AppRow>,
    ) {
        let bit = 1 << second_index;
        spare.clear();
        spare.reserve(self.app_count(second_index));
        places.clear();
        let mut held = rows.drain(..).peekable();
        for (place, app) in (0..).zip(&*self.apps) {
            if app.seconds & bit == 0 {
                continue;
            }
            let name = &app.first.name;
            // The rows held are in key order, so a row before the App's key shows an App the
            // second does not hold.
            while held
                .next_if(|row| rebuild_app_rows_before(row, &app.first))
                .is_some()
            {}
            let mut row = match held.next_if(|row| Arc::ptr_eq(&row.name, name)) {
                Some(row) => row,
                None => app.first.clone(),
            };
            self.app_readings(app, second_index, &mut row);
            spare.push(row);
            places.push(place);
        }
        drop(held);
        std::mem::swap(rows, spare);
        spare.clear();
        assert_eq!(rows.len(), self.app_count(second_index));
    }

    /// Moves `rows`, the Apps of the second before `second_index` in key order with the App
    /// place of each in `places`, to the Apps of the second at `second_index`, through that
    /// second's steps.
    pub(in crate::store) fn step_app_rows(
        &self,
        second_index: u32,
        rows: &mut Vec<AppRow>,
        places: &mut Vec<u32>,
    ) {
        assert!(second_index > 0);
        let index = collector::usize_from_u32(second_index);
        let start = collector::usize_from_u32(self.step_starts[index]);
        let end = collector::usize_from_u32(self.step_starts[index + 1]);
        for step in &self.steps[start..end] {
            match *step {
                AppStep::Arrive(place) => {
                    let Err(at) = places.binary_search(&place) else {
                        unreachable!("an App that arrives is not held at the second before");
                    };
                    rows.insert(at, self.app_row(place, second_index));
                    places.insert(at, place);
                }
                AppStep::Leave(place) => {
                    let Ok(at) = places.binary_search(&place) else {
                        unreachable!("an App that leaves is held at the second before");
                    };
                    rows.remove(at);
                    places.remove(at);
                }
                AppStep::Change {
                    place,
                    index,
                    word,
                    value,
                } => {
                    let index = collector::usize_from_u32(index);
                    assert_eq!(places[index], place);
                    rows[index].set_word(collector::usize_from_u32(word), value);
                }
            }
        }
        assert_eq!(rows.len(), self.app_count(second_index));
    }
}

#[cfg(test)]
impl BlockTable {
    /// Where in the table's values the first change of the row of the App at `place` is held,
    /// and then the first change of its details, each where it changes inside the block.
    pub(super) fn app_first_changes(&self, place: u32) -> [Option<usize>; 2] {
        let app = self.block_app(place);
        let first = |changing: u32, changes_start: u32| {
            let record = collector::usize_from_u32(changes_start);
            (changing != 0).then(|| collector::usize_from_u32(self.change_starts[record]))
        };
        let details = app
            .details
            .as_ref()
            .and_then(|details| first(details.changing, details.changes_start));

        [first(app.changing, app.changes_start), details]
    }
}

/// The details of each App of one second of a packed block, in key order.
#[derive(Clone, Debug)]
pub(in crate::store) struct PackedDetails<'table> {
    table: &'table BlockTable,
    second_index: u32,
    /// The place of the next App to look at.
    place: u32,
    remaining: usize,
}

impl Iterator for PackedDetails<'_> {
    type Item = AppDetails;

    fn next(&mut self) -> Option<AppDetails> {
        if self.remaining == 0 {
            return None;
        }
        let bit = 1 << self.second_index;
        for place in self.place..block_u32(self.table.apps.len()) {
            if self.table.block_app(place).seconds & bit != 0 {
                self.place = place + 1;
                self.remaining -= 1;
                return Some(self.table.app_details(place, self.second_index));
            }
        }

        unreachable!("a second holds as many Apps as it counts")
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl ExactSizeIterator for PackedDetails<'_> {}

/// Whether `row` shows an App whose key comes before the key of `app`.
fn rebuild_app_rows_before(row: &AppRow, app: &AppRow) -> bool {
    if Arc::ptr_eq(&row.name, &app.name) {
        return false;
    }
    let held = row.name.kind.key(&row.name.bytes);
    let key = app.name.kind.key(&app.name.bytes);

    held.cmp(&key) == Ordering::Less
}
