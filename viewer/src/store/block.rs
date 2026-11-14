//! The App seconds of the live span as a row of blocks of seconds, which readers reach only
//! through a second's header, one App or process of a second, and a whole second's Apps.

use std::ops::Range;
use std::sync::Arc;

use persistence::{Error, Result};
use serde_json::Value;

use super::attribution::{Attribution, Completeness, Resident, Totals};
use super::native::Origin;
use collector::ring::ProcessCreation;

use super::buffers::Buffers;
use super::{AppDetails, AppRow, AppsSecond, Missing, Payload, ProcessRow};
use crate::app_attribution_history::AppAttributionMetric;
use crate::live_app_response::json::{AppView, Header, ProcessView};
use crate::live_app_response::{Detail, KeyKind, Observation, Page, RankedApp, Ranks};

#[cfg(test)]
#[path = "block_tests.rs"]
mod tests;

/// The seconds a block holds at most.
pub(super) const BLOCK_SECONDS_MAX: usize = 64;

/// The seconds that have left the span and stay in its oldest block, which holds at least one
/// second of the span until its last second leaves, so each holds a buffer past the span's.
pub const BLOCK_DEPARTED_MAX: usize = BLOCK_SECONDS_MAX - 1;

/// The wall seconds whose seconds one block holds, three of the stretches the store closes.
const BLOCK_WALL_NS: u64 = 30 * 1_000_000_000;

const _: () = {
    assert!(BLOCK_WALL_NS == 3 * crate::session::EARLIER_STRIDE_SECONDS * 1_000_000_000);
    // Two ticks can fall in one wall second, so a block of thirty wall seconds can hold more
    // than thirty seconds.
    assert!(BLOCK_SECONDS_MAX > 30);
};

/// Seconds of the live span that follow one another within thirty wall seconds, oldest
/// first. A block never changes once made, so a View that holds it reads its seconds as they
/// were. This is the plain kind, which holds each second as the store took it.
#[derive(Debug, PartialEq)]
pub struct Block {
    /// The index of the thirty wall seconds the block's seconds fall in, counted from the
    /// epoch.
    half_minute_index: u64,
    seconds: Vec<Arc<AppsSecond>>,
}

impl Block {
    /// A block holding `second` alone, which the store took at wall time `wall_ns`.
    #[must_use]
    fn starting(wall_ns: u64, second: Arc<AppsSecond>) -> Self {
        let mut seconds = Vec::with_capacity(BLOCK_SECONDS_MAX);
        seconds.push(second);

        Self {
            half_minute_index: half_minute_index(wall_ns),
            seconds,
        }
    }

    /// A new block holding this block's seconds and `second` after them, which the store took
    /// at wall time `wall_ns`, or `None` where this block is closed to it: it holds
    /// `BLOCK_SECONDS_MAX` seconds, or `second` falls in other thirty wall seconds.
    #[must_use]
    fn joining(&self, wall_ns: u64, second: Arc<AppsSecond>) -> Option<Self> {
        if self.seconds.len() == BLOCK_SECONDS_MAX {
            return None;
        }
        if half_minute_index(wall_ns) != self.half_minute_index {
            return None;
        }

        let mut seconds = Vec::with_capacity(BLOCK_SECONDS_MAX);
        seconds.extend(self.seconds.iter().map(Arc::clone));
        seconds.push(second);

        Some(Self {
            half_minute_index: self.half_minute_index,
            seconds,
        })
    }

    /// A new block holding this block's seconds with `second` in place of the one at `index`.
    #[must_use]
    fn replacing(&self, index: usize, second: Arc<AppsSecond>) -> Self {
        assert!(index < self.seconds.len());

        let mut seconds = Vec::with_capacity(BLOCK_SECONDS_MAX);
        seconds.extend(self.seconds.iter().map(Arc::clone));
        seconds[index] = second;

        Self {
            half_minute_index: self.half_minute_index,
            seconds,
        }
    }

    /// How many seconds the block holds.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.seconds.len()
    }

    /// The second at `index` of the block, the oldest at 0.
    #[must_use]
    pub fn second(&self, index: usize) -> Second<'_> {
        assert!(index < self.seconds.len());

        Second {
            second: &self.seconds[index],
        }
    }
}

/// The index of the thirty wall seconds `wall_ns` falls in. The division floors, so every
/// second of those thirty has one index.
fn half_minute_index(wall_ns: u64) -> u64 {
    wall_ns / BLOCK_WALL_NS
}

/// The seconds of the live span as the store holds them, oldest first, at most `capacity` of
/// them, each written into a buffer of its own and held in a row of blocks. The oldest block
/// may hold seconds that have left the span, which no reader reaches, until its last second
/// leaves.
pub(super) struct BlockSpan {
    buffers: Buffers<AppsSecond>,
    span: AppsSpan,
    capacity: usize,
}

impl BlockSpan {
    /// A span of at most `capacity` seconds over `buffers`, which holds more buffers than
    /// seconds and the seconds that have left them, so a View the main thread holds for a moment
    /// costs the store no new buffer.
    #[must_use]
    pub(super) fn new(capacity: usize, buffers: Buffers<AppsSecond>) -> Self {
        assert!(capacity > 0);
        assert!(buffers.count() > capacity + BLOCK_DEPARTED_MAX);

        Self {
            buffers,
            span: AppsSpan::with_capacity(capacity),
            capacity,
        }
    }

    /// Adds the second `fill` writes after the newest, which the store took at wall time
    /// `wall_ns`, letting go of the oldest when the span is full. A View holding the blocks
    /// before keeps them, and stops nothing.
    pub(super) fn push(&mut self, wall_ns: u64, fill: impl FnOnce(&mut AppsSecond)) {
        if self.span.len == self.capacity {
            self.remove_oldest();
        }
        assert!(self.span.len < self.capacity);
        // Every block holds a second of the span, the oldest too, so the row of blocks, which
        // has room for one block more than the span has seconds, never grows.
        assert!(self.span.blocks.len() <= self.span.len);
        assert!(self.span.blocks.len() < self.span.block_capacity());

        let second = self.buffers.write_replacing(fill);
        self.span.push_second(wall_ns, second);
    }

    /// Lets go of the oldest second, which a View may go on holding. The oldest block counts
    /// it as departed, and the span lets go of the block once its last second has left.
    pub(super) fn remove_oldest(&mut self) {
        let span = &mut self.span;
        let Some(oldest) = span.blocks.first() else {
            return;
        };
        assert!(span.departed_count < oldest.len());
        assert!(span.len > 0);

        span.departed_count += 1;
        span.len -= 1;
        if span.departed_count == oldest.len() {
            // The row holds a block for each thirty wall seconds of the span, so moving it
            // down one place costs a few dozen pointers once a block.
            drop(span.blocks.remove(0));
            span.departed_count = 0;
        }
    }

    /// Puts the second `fill` writes in place of the second at `index` of the span, the oldest
    /// at 0, in a new block in its block's place, so a View or a query context holding the
    /// blocks before reads them unchanged. Where every buffer is held, the second takes the slot
    /// of the one written longest ago, as a take does, so a holder kept through the launch
    /// stops nothing and keeps alive only the seconds it holds.
    pub(super) fn replace(&mut self, index: usize, fill: impl FnOnce(&mut AppsSecond)) {
        assert!(index < self.span.len);
        let second = self.buffers.write_replacing(fill);

        self.span.replace_second(index, second);
    }

    /// Writes into `view` the blocks the span holds, each shared with the View, and how many
    /// seconds of the oldest have left.
    pub(super) fn publish(&self, view: &mut AppsSpan) {
        view.blocks.clear();
        view.blocks.extend(self.span.blocks.iter().map(Arc::clone));
        view.departed_count = self.span.departed_count;
        view.len = self.span.len;
    }

    /// The seconds of the span, as a View holds them.
    #[must_use]
    pub(super) fn span(&self) -> &AppsSpan {
        &self.span
    }

    /// The second the next `push` lets go of, the oldest where the span is full.
    #[must_use]
    pub(super) fn leaving(&self) -> Option<Second<'_>> {
        (self.span.len == self.capacity).then(|| self.span.second(0))
    }

    /// The second at `index` of the span as the store took it, which a launch reads the rates
    /// of the next second from. Only the store's writers read a second this way.
    #[must_use]
    pub(super) fn stored(&self, index: usize) -> Option<&AppsSecond> {
        (index < self.span.len).then(|| self.span.stored_at(index).as_ref())
    }

    /// The newest second's buffer, which the store reads the next second's rates from. The
    /// newest block is the open one, so the store took this second as it is.
    #[must_use]
    pub(super) fn newest_held(&self) -> Option<&Arc<AppsSecond>> {
        let newest = self.span.blocks.last()?;

        newest.seconds.last()
    }

    /// Lets go of every second, keeping the buffers.
    pub(super) fn clear(&mut self) {
        self.span.clear();
    }

    /// Clears with `clear` each buffer that nothing else holds.
    pub(super) fn release(&mut self, clear: impl FnMut(&mut AppsSecond)) {
        self.buffers.release(clear);
    }
}

/// The App seconds of the live span as a View holds them: the store's blocks, shared and not
/// copied, and how many seconds of the oldest have left the span, which no reader reaches.
#[derive(Debug, Default)]
pub struct AppsSpan {
    blocks: Vec<Arc<Block>>,
    /// How many seconds of the oldest block have left the span.
    departed_count: usize,
    /// How many seconds the span holds, those that have left not counted.
    len: usize,
}

/// The seconds of a View's span, oldest first, read from either end.
#[derive(Clone, Debug)]
pub struct Seconds<'span> {
    /// The blocks from the one holding the oldest second to read.
    blocks: &'span [Arc<Block>],
    /// The index in `blocks` of the block holding the next second from the front.
    front_block_index: usize,
    /// The index in that block of the next second from the front, or its length where the
    /// front has read the whole block.
    front_second_index: usize,
    /// The index in `blocks` of the block holding the next second from the back.
    back_block_index: usize,
    /// How many seconds of that block lie before the back, 0 where the back has read the whole
    /// block.
    back_second_count: usize,
    /// How many seconds are left to read.
    remaining: usize,
}

impl AppsSpan {
    /// A span holding no second, with room for the blocks of `seconds` seconds.
    #[must_use]
    pub fn with_capacity(seconds: usize) -> Self {
        Self {
            blocks: Vec::with_capacity(seconds + 1),
            departed_count: 0,
            len: 0,
        }
    }

    /// How many seconds the span holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The second at `index` of the span, the oldest at 0.
    #[must_use]
    pub fn second(&self, index: usize) -> Second<'_> {
        assert!(index < self.len);
        let (block_index, second_index) = self.place(index);

        self.blocks[block_index].second(second_index)
    }

    /// The newest second, where the span holds one.
    #[must_use]
    pub fn newest(&self) -> Option<Second<'_>> {
        let newest = self.blocks.last()?;

        Some(newest.second(newest.len() - 1))
    }

    /// Every second of the span, oldest first.
    #[must_use]
    pub fn seconds(&self) -> Seconds<'_> {
        self.seconds_from(0)
    }

    /// The seconds of the span from the one at `index`, the oldest at 0, oldest first.
    #[must_use]
    pub fn seconds_from(&self, index: usize) -> Seconds<'_> {
        if index >= self.len {
            return Seconds {
                blocks: &[],
                front_block_index: 0,
                front_second_index: 0,
                back_block_index: 0,
                back_second_count: 0,
                remaining: 0,
            };
        }
        let (block_index, second_index) = self.place(index);
        let blocks = &self.blocks[block_index..];
        let Some(newest) = blocks.last() else {
            unreachable!("the block holding a second of the span is in the row");
        };

        Seconds {
            blocks,
            front_block_index: 0,
            front_second_index: second_index,
            back_block_index: blocks.len() - 1,
            back_second_count: newest.len(),
            remaining: self.len - index,
        }
    }

    /// Each block of the span, oldest first, with the indices of its seconds the span holds.
    pub(super) fn block_ranges(&self) -> impl Iterator<Item = (&Arc<Block>, Range<usize>)> {
        let departed_count = self.departed_count;
        self.blocks
            .iter()
            .enumerate()
            .map(move |(block_index, block)| {
                let first = if block_index == 0 { departed_count } else { 0 };

                (block, first..block.len())
            })
    }

    /// Where each second of the span lies, oldest first: its block and its index there.
    pub(super) fn places(&self) -> impl Iterator<Item = (&Arc<Block>, usize)> {
        self.block_ranges()
            .flat_map(|(block, indices)| indices.map(move |index| (block, index)))
    }

    /// The block holding the second at `index` of the span, the oldest at 0, and the second's
    /// index in that block.
    fn place(&self, index: usize) -> (usize, usize) {
        assert!(index < self.len);

        let mut second_index = index + self.departed_count;
        for (block_index, block) in self.blocks.iter().enumerate() {
            if second_index < block.len() {
                return (block_index, second_index);
            }
            second_index -= block.len();
        }

        unreachable!("the blocks hold the span's seconds")
    }

    /// The second at `index` of the span, the oldest at 0, as the store took it.
    fn stored_at(&self, index: usize) -> &Arc<AppsSecond> {
        let (block_index, second_index) = self.place(index);

        &self.blocks[block_index].seconds[second_index]
    }

    /// Adds `second`, which the store took at wall time `wall_ns`, after the newest: in a new
    /// block in place of the newest where that block is open to it, and in a block of its own
    /// where not.
    fn push_second(&mut self, wall_ns: u64, second: Arc<AppsSecond>) {
        let joined = self
            .blocks
            .last()
            .and_then(|newest| newest.joining(wall_ns, Arc::clone(&second)));
        match (joined, self.blocks.last_mut()) {
            (Some(joined), Some(newest)) => *newest = Arc::new(joined),
            (Some(_), None) => unreachable!("only a block of the row joins a second"),
            (None, _) => self.blocks.push(Arc::new(Block::starting(wall_ns, second))),
        }
        self.len += 1;
    }

    /// Puts `second` in place of the second at `index`, the oldest at 0, in a new block in its
    /// block's place, so a holder of the blocks before reads them unchanged.
    fn replace_second(&mut self, index: usize, second: Arc<AppsSecond>) {
        let (block_index, second_index) = self.place(index);
        let block = &mut self.blocks[block_index];

        *block = Arc::new(block.replacing(second_index, second));
    }

    /// Lets go of every block, keeping the room for them.
    pub(super) fn clear(&mut self) {
        self.blocks.clear();
        self.departed_count = 0;
        self.len = 0;
    }

    /// How many blocks the span has room for.
    pub(super) fn block_capacity(&self) -> usize {
        self.blocks.capacity()
    }
}

impl<'span> Iterator for Seconds<'span> {
    type Item = Second<'span>;

    fn next(&mut self) -> Option<Second<'span>> {
        if self.remaining == 0 {
            return None;
        }
        // Seconds are left, so the front stands in a block of the row, and a block holds at
        // least one second, so one step past a block's end reaches a second.
        assert!(self.front_block_index < self.blocks.len());
        if self.front_second_index == self.blocks[self.front_block_index].len() {
            self.front_block_index += 1;
            self.front_second_index = 0;
        }
        assert!(self.front_block_index < self.blocks.len());
        let block = &self.blocks[self.front_block_index];
        assert!(self.front_second_index < block.len());

        let second = block.second(self.front_second_index);
        self.front_second_index += 1;
        self.remaining -= 1;
        Some(second)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        (self.remaining, Some(self.remaining))
    }
}

impl DoubleEndedIterator for Seconds<'_> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        // Seconds are left, so a block before the back holds one where the back has read the
        // whole of its own.
        if self.back_second_count == 0 {
            assert!(self.back_block_index > 0);
            self.back_block_index -= 1;
            self.back_second_count = self.blocks[self.back_block_index].len();
        }
        assert!(self.back_second_count > 0);

        self.back_second_count -= 1;
        self.remaining -= 1;
        Some(self.blocks[self.back_block_index].second(self.back_second_count))
    }
}

impl ExactSizeIterator for Seconds<'_> {}

/// Two spans are equal where they hold equal seconds in the same order, however their blocks
/// divide them.
impl PartialEq for AppsSpan {
    fn eq(&self, other: &Self) -> bool {
        if self.len != other.len {
            return false;
        }

        self.places()
            .zip(other.places())
            .all(|((block, index), (other_block, other_index))| {
                block.seconds[index] == other_block.seconds[other_index]
            })
    }
}

/// One second of a block, as a reader reaches it: its header, and its Apps and processes
/// through the block's own forms.
#[derive(Clone, Copy, Debug)]
pub struct Second<'block> {
    second: &'block AppsSecond,
}

impl<'block> Second<'block> {
    /// The tick the second shows, `None` only in a buffer the store has not filled.
    #[must_use]
    pub fn observation(self) -> Option<Observation> {
        self.second.observation
    }

    /// Whether the second's tick follows the tick of the second the store took before it.
    #[must_use]
    pub fn consecutive(self) -> bool {
        self.second.consecutive
    }

    /// The interval the second's rates are taken over, where it is known.
    #[must_use]
    pub fn elapsed_ns(self) -> Option<u64> {
        self.second.elapsed_ns
    }

    /// Why the second holds no Apps, or `None` where it holds every App of its tick.
    #[must_use]
    pub fn missing(self) -> Option<&'block Missing> {
        self.second.missing.as_ref()
    }

    /// How completely the second's readings found the Mac's processes.
    #[must_use]
    pub fn completeness(self) -> Completeness {
        self.second.completeness()
    }

    /// Every App of the second added up.
    #[must_use]
    pub fn totals(self) -> &'block Totals {
        &self.second.totals
    }

    /// Where the second's readings came from.
    #[must_use]
    pub fn origin(self) -> Origin {
        self.second.metadata.origin
    }

    /// How many Apps the second holds, read from no row.
    #[must_use]
    pub fn app_count(self) -> usize {
        self.second.apps.len()
    }

    /// How many App rows the second has room for, which a tool that measures the store reads.
    #[must_use]
    pub fn app_row_capacity(self) -> usize {
        self.second.apps.capacity()
    }

    /// A second no block holds, one a launch is about to install.
    #[must_use]
    pub(super) fn unheld(second: &'block AppsSecond) -> Self {
        Self { second }
    }

    /// The Apps that used the most of each resource at this second, or why the second holds no
    /// Apps to rank.
    pub fn ranks(self) -> std::result::Result<Ranks, Missing> {
        self.second.ranks()
    }

    /// What the second shows on the App chart of `metric` whose own lines draw `cohort`.
    #[must_use]
    pub fn attribution(
        self,
        metric: AppAttributionMetric,
        cohort: &[Arc<RankedApp>],
    ) -> Attribution {
        self.second.attribution(metric, cohort)
    }

    /// What the second shows on the chart of resident memory by App whose own lines draw
    /// `cohort`.
    #[must_use]
    pub fn resident(self, cohort: &[Arc<RankedApp>]) -> Resident {
        self.second.resident(cohort)
    }

    /// The requested page of the second's Apps.
    pub fn apps_page(self, page: Page) -> Result<Value> {
        self.second.apps_page(page)
    }

    /// The requested page of one App's members.
    pub fn app_detail(self, detail: &Detail<'_>) -> Result<Value> {
        self.second.app_detail(detail)
    }

    /// The requested page of the processes in no App.
    pub fn ungrouped_page(self, page: Page) -> Result<Value> {
        self.second.ungrouped_page(page)
    }

    /// The header a page of the second shows, and its processes, where it holds both.
    pub(crate) fn presentation(self) -> Result<(Header, Processes<'block>)> {
        let (header, payload) = self.second.presentation()?;

        Ok((header, Processes { payload }))
    }

    /// The App `name` names at this second, where the second holds it.
    #[must_use]
    pub fn app(self, name: &RankedApp) -> Option<AppAt<'block>> {
        self.app_of(name.kind, &name.bytes)
    }

    /// The App of kind `kind` named `bytes` at this second, where the second holds it.
    #[must_use]
    pub fn app_of(self, kind: KeyKind, bytes: &[u8]) -> Option<AppAt<'block>> {
        let key = kind.key(bytes);
        let rows = self.second.apps.as_slice();
        let index = rows
            .binary_search_by(|row| row.name.kind.key(&row.name.bytes).cmp(&key))
            .ok()?;

        Some(AppAt {
            row: &rows[index],
            index,
            second: self.second,
        })
    }

    /// The second's processes, or why the second holds none.
    pub fn processes(self) -> std::result::Result<Processes<'block>, &'block Missing> {
        match &self.second.processes {
            Ok(payload) => Ok(Processes { payload }),
            Err(missing) => Err(missing),
        }
    }
}

/// What a reader that walks whole seconds reads each second's Apps through, as a slice in key
/// order holding exactly the Apps of that second. A plain block's slice is the second's own
/// rows, so the plain kind keeps nothing here.
#[derive(Debug, Default)]
pub struct AppsCursor {
    _rows: (),
}

impl AppsCursor {
    /// Every App of `second`, once each, in key order.
    pub fn apps<'cursor>(&'cursor mut self, second: Second<'cursor>) -> &'cursor [AppRow] {
        second.second.apps.as_slice()
    }
}

/// One App at one second: its row, its place among the Apps of that second, and that second,
/// so a reader never pairs the App with another second's measures.
#[derive(Clone, Copy, Debug)]
pub struct AppAt<'block> {
    row: &'block AppRow,
    index: usize,
    second: &'block AppsSecond,
}

impl<'block> AppAt<'block> {
    /// The App's readings at the second.
    #[must_use]
    pub fn row(self) -> &'block AppRow {
        self.row
    }

    /// The App's place among the Apps of the second, in key order.
    #[must_use]
    pub fn index(self) -> usize {
        self.index
    }

    /// The App as a page shows it, with the additional measures its own second gives it, where
    /// that second holds its processes.
    pub(crate) fn presentation(self) -> Result<AppView<'block>> {
        let payload = self
            .second
            .processes
            .as_ref()
            .map_err(|_| Error::Bound("live App empty sample"))?;

        Ok(self.row.presentation(payload.app(self.index)))
    }
}

/// The processes of one second, read by place, by App or by pid, each with the App index of
/// that second.
#[derive(Clone, Copy, Debug)]
pub struct Processes<'block> {
    payload: &'block Payload,
}

impl<'block> Processes<'block> {
    /// How many processes the second holds.
    #[must_use]
    pub fn len(self) -> usize {
        self.payload.len()
    }

    #[must_use]
    pub fn is_empty(self) -> bool {
        self.payload.is_empty()
    }

    /// The process at `place` of the second's processes, in the sample's order.
    #[must_use]
    pub fn at(self, place: usize) -> ProcessAt<'block> {
        assert!(place < self.payload.len());

        ProcessAt {
            row: &self.payload[place],
            app_index: self.payload.app_index(place),
        }
    }

    /// Every process of the second, in the sample's order.
    #[must_use]
    pub fn iter(self) -> impl ExactSizeIterator<Item = ProcessAt<'block>> {
        (0..self.payload.len()).map(move |place| self.at(place))
    }

    /// The processes of the App at `app_index` of the second's Apps, or of no App where
    /// `app_index` is `None`, in the sample's order.
    pub fn members(
        self,
        app_index: Option<u32>,
    ) -> impl Iterator<Item = ProcessAt<'block>> + Clone {
        self.payload
            .members(app_index)
            .map(move |row| ProcessAt { row, app_index })
    }

    /// The process of pid `pid` created at `creation`, where the second holds it.
    #[must_use]
    pub fn find(self, pid: i32, creation: Option<ProcessCreation>) -> Option<ProcessAt<'block>> {
        self.iter()
            .find(|process| process.row.pid == pid && process.row.creation == creation)
    }

    /// The additional measures of the App at `app_index` of the second's Apps.
    #[must_use]
    pub fn app_details(self, app_index: usize) -> &'block AppDetails {
        self.payload.app(app_index)
    }

    /// The additional measures of every App of the second, in key order.
    #[must_use]
    pub fn details(self) -> impl ExactSizeIterator<Item = &'block AppDetails> {
        self.payload.apps()
    }

    /// How many Apps the second holds additional measures of.
    #[must_use]
    pub fn app_details_count(self) -> usize {
        self.payload.apps().len()
    }
}

/// One process at one second: its row, and the App index the second gives it, so a reader
/// never pairs a row with another row's index.
#[derive(Clone, Copy, Debug)]
pub struct ProcessAt<'block> {
    row: &'block ProcessRow,
    app_index: Option<u32>,
}

impl<'block> ProcessAt<'block> {
    /// The process's readings at the second.
    #[must_use]
    pub fn row(self) -> &'block ProcessRow {
        self.row
    }

    /// The App index the second gives the process: its App's place among the second's Apps, or
    /// `None` where the process belongs to no App.
    #[must_use]
    pub fn app_index(self) -> Option<u32> {
        self.app_index
    }

    /// The process as a page shows it, in its own App.
    #[must_use]
    pub(crate) fn presentation(self) -> ProcessView<'block> {
        self.row.presentation(self.app_index)
    }
}

/// What a test of a reader that builds a View by hand reads back to build another, in this
/// crate or, through the `fixtures` feature, in another. No release build holds these, since
/// they reach a second as the store took it.
#[cfg(any(test, feature = "fixtures"))]
impl AppsSpan {
    /// The second at `index`, the oldest at 0, as the store took it.
    #[must_use]
    pub fn stored(&self, index: usize) -> &Arc<AppsSecond> {
        assert!(index < self.len);

        self.stored_at(index)
    }

    /// Every second of the span, oldest first, as the store took it.
    pub fn stored_seconds(&self) -> impl Iterator<Item = &Arc<AppsSecond>> {
        self.places().map(|(block, index)| &block.seconds[index])
    }

    /// Puts `second` in place of the second at `index`, the oldest at 0, in a new block in its
    /// block's place, as a test of a reader changes a View it built by hand.
    pub fn replace(&mut self, index: usize, second: Arc<AppsSecond>) {
        assert!(index < self.len);

        self.replace_second(index, second);
    }
}

/// A span holding `seconds` in order, each joining the block before where it falls in the same
/// thirty wall seconds as the tick it shows, as a View built by hand for a test of a reader holds
/// them.
#[cfg(any(test, feature = "fixtures"))]
impl FromIterator<Arc<AppsSecond>> for AppsSpan {
    fn from_iter<Source: IntoIterator<Item = Arc<AppsSecond>>>(seconds: Source) -> Self {
        let mut span = Self::default();
        for second in seconds {
            let wall_ns = second
                .observation
                .and_then(|observation| observation.wall_ns)
                .unwrap_or(0);
            span.push_second(wall_ns, second);
        }

        span
    }
}

/// What a test of the store reads to check the seconds as the store took them, which no reader
/// of a View reaches.
#[cfg(test)]
impl AppsSpan {
    /// Each second of the span as the store took it, oldest first.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &Arc<AppsSecond>> {
        self.stored_seconds()
    }

    /// The newest second as the store took it.
    pub(crate) fn last(&self) -> Option<&Arc<AppsSecond>> {
        self.blocks.last()?.seconds.last()
    }

    /// The span without its `count` oldest seconds, sharing its blocks.
    pub(crate) fn skipping(&self, count: usize) -> Self {
        assert!(count < self.len);
        let (block_index, second_index) = self.place(count);

        Self {
            blocks: self.blocks[block_index..].to_vec(),
            departed_count: second_index,
            len: self.len - count,
        }
    }

    /// How many seconds of the oldest block have left the span.
    pub(super) fn departed_count(&self) -> usize {
        self.departed_count
    }

    /// The blocks the span holds, oldest first, the oldest holding `departed_count` seconds
    /// that have left.
    pub(super) fn blocks(&self) -> &[Arc<Block>] {
        &self.blocks
    }
}

#[cfg(test)]
impl<'block> Second<'block> {
    /// The second as the store took it, which a test of the store checks.
    pub(crate) fn stored(self) -> &'block AppsSecond {
        self.second
    }
}

#[cfg(test)]
impl std::ops::Index<usize> for AppsSpan {
    type Output = AppsSecond;

    fn index(&self, index: usize) -> &AppsSecond {
        self.stored(index)
    }
}

/// What a test of the store reads to check the seconds as the store took them.
#[cfg(test)]
impl BlockSpan {
    /// A span of at most `capacity` seconds over the fewest buffers `new` takes, one more than
    /// the span and the seconds that have left it hold.
    pub(super) fn with_fewest_buffers(capacity: usize) -> Self {
        let buffers = Buffers::new(capacity + BLOCK_DEPARTED_MAX + 1, AppsSecond::default);

        Self::new(capacity, buffers)
    }

    /// How many buffers nothing but the buffers holds.
    pub(super) fn free_count(&self) -> usize {
        self.buffers.free()
    }

    /// Holds every buffer nothing else holds, as a View or a query context would, so the next
    /// write finds every buffer held.
    pub(super) fn hold_every_free_buffer(&mut self) -> Vec<Arc<AppsSecond>> {
        let count = self.buffers.count();
        let mut held = Vec::with_capacity(count);
        for _ in 0..count {
            let Some(buffer) = self.buffers.write(|_| {}) else {
                break;
            };
            held.push(buffer);
        }
        assert_eq!(self.free_count(), 0);

        held
    }

    pub(super) fn get(&self, index: usize) -> Option<&AppsSecond> {
        self.stored(index)
    }

    pub(super) fn held(&self) -> impl Iterator<Item = &Arc<AppsSecond>> {
        self.span.iter()
    }

    pub(super) fn newest(&self) -> Option<&AppsSecond> {
        self.newest_held().map(AsRef::as_ref)
    }

    pub(super) fn len(&self) -> usize {
        self.span.len
    }

    pub(super) fn is_empty(&self) -> bool {
        self.span.len == 0
    }

    /// How many buffers there are, held or not.
    pub(super) fn buffer_count(&self) -> usize {
        self.buffers.count()
    }

    /// The blocks the span holds, oldest first.
    pub(super) fn blocks(
        &self,
    ) -> impl DoubleEndedIterator<Item = &Arc<Block>> + ExactSizeIterator {
        self.span.blocks.iter()
    }
}
