//! The App seconds of the live span as a row of blocks of seconds, which readers reach only
//! through a second's header, one App or process of a second, and a whole second's Apps.

use std::collections::VecDeque;
use std::ops::Range;
use std::sync::Arc;

use persistence::{Error, Result};
use serde_json::Value;

use super::attribution::{Attribution, Completeness, Resident, Totals};
use super::native::Origin;
use collector::ring::ProcessCreation;

use super::apps::AppRows;
use super::buffers::Buffers;
use super::packing::{
    Answer, BATCH_BUFFERS_MAX, BATCHES_OUT_MAX, BLOCKS_OUT_MAX, Batch, Before, Handed, QUEUE_MAX,
    StoreEnd,
};
use super::payload::packed::{KeptPlain, ProcessTable, pack_seconds};
use super::{AppDetails, AppRow, AppsSecond, Missing, Payload, ProcessAt};
use crate::app_attribution_history::AppAttributionMetric;
use crate::byte_budget::{ByteBudget, ByteReservation};
use crate::live_app_response::json::{AppView, Header};
use crate::live_app_response::{Detail, KeyKind, Observation, Page, RankedApp, Ranks};

#[cfg(test)]
#[path = "block_tests.rs"]
mod tests;

/// The seconds a block holds at most.
pub(super) const BLOCK_SECONDS_MAX: usize = 64;

/// The seconds that have left the span and stay in its oldest block, which holds at least one
/// second of the span until its last second leaves, so each holds a buffer past the span's.
pub const BLOCK_DEPARTED_MAX: usize = BLOCK_SECONDS_MAX - 1;

/// The packed blocks that left the row the store keeps at most until nothing else holds them,
/// more than the store's four Views can hold between them, four spans' blocks.
pub(super) const DEPARTED_KEPT_MAX: usize = 256;

/// The wall seconds whose seconds one block holds, three of the stretches the store closes.
const BLOCK_WALL_NS: u64 = 30 * 1_000_000_000;

const _: () = {
    assert!(BLOCK_WALL_NS == 3 * crate::session::EARLIER_STRIDE_SECONDS * 1_000_000_000);
    // Two ticks can fall in one wall second, so a block of thirty wall seconds can hold more
    // than thirty seconds.
    assert!(BLOCK_SECONDS_MAX > 30);
    // A block marks the seconds that wait for their Apps with one bit each.
    // Compared as powers of two, since a constant changes integer type only through `as`.
    assert!(1_u128 << BLOCK_SECONDS_MAX <= 1_u128 << u64::BITS);
};

/// Seconds of the live span that follow one another within thirty wall seconds, oldest
/// first. A block never changes once made, so a View that holds it reads its seconds as they
/// were. A plain block holds each second as the store took it, and a packed block holds each
/// second's header and Apps with the processes of all its seconds in one table.
#[derive(Debug)]
pub struct Block {
    /// The index of the thirty wall seconds the block's seconds fall in, counted from the
    /// epoch.
    half_minute_index: u64,
    seconds: Vec<Arc<AppsSecond>>,
    form: Form,
    /// The seconds whose Apps a launch has still to install, bit `index` for the second at
    /// `index`. A block holding one is never packed, so an install always replaces a plain
    /// second and reads the plain second before it.
    awaiting: u64,
    /// The charge of a packed block's own allocation in its `Arc` and of its list of seconds,
    /// `None` for a plain block, which no budget charges. It is held only to be dropped with
    /// the block, as `BufferOwner` holds its reservations in the persistence crate.
    _reservation: Option<ByteReservation>,
}

/// How a block holds its seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    /// Each second as the store took it. The open block, a closed block waiting to be packed,
    /// and a block a launch still installs into are plain.
    Plain,
    /// Each second's header and Apps, with the processes of every second in one table.
    Packed,
    /// Plain for good, for a cause the data or the machine brought about, and never tried again.
    KeptPlain(KeptPlain),
}

impl Block {
    /// A plain block holding `second` alone, which the store took at wall time `wall_ns`, and
    /// which waits for its Apps where `awaiting` is true.
    #[must_use]
    fn starting(wall_ns: u64, second: Arc<AppsSecond>, awaiting: bool) -> Self {
        let mut seconds = Vec::with_capacity(BLOCK_SECONDS_MAX);
        seconds.push(second);

        Self {
            half_minute_index: half_minute_index(wall_ns),
            seconds,
            form: Form::Plain,
            awaiting: u64::from(awaiting),
            _reservation: None,
        }
    }

    /// A new block holding this block's seconds and `second` after them, which the store took
    /// at wall time `wall_ns` and which waits for its Apps where `awaiting` is true, or `None`
    /// where this block is closed to it: it holds `BLOCK_SECONDS_MAX` seconds, or `second`
    /// falls in other thirty wall seconds.
    #[must_use]
    fn joining(&self, wall_ns: u64, second: Arc<AppsSecond>, awaiting: bool) -> Option<Self> {
        if self.seconds.len() == BLOCK_SECONDS_MAX {
            return None;
        }
        if half_minute_index(wall_ns) != self.half_minute_index {
            return None;
        }
        // Only a closed block is packed, and a block open to a second is the newest.
        assert_eq!(self.form, Form::Plain);

        let mut seconds = Vec::with_capacity(BLOCK_SECONDS_MAX);
        seconds.extend(self.seconds.iter().map(Arc::clone));
        seconds.push(second);

        Some(Self {
            half_minute_index: self.half_minute_index,
            awaiting: self.awaiting | (u64::from(awaiting) << self.seconds.len()),
            seconds,
            form: Form::Plain,
            _reservation: None,
        })
    }

    /// A new plain block holding this block's seconds with `second` in place of the one at
    /// `index`, which no longer waits for its Apps.
    #[must_use]
    fn replacing(&self, index: usize, second: Arc<AppsSecond>) -> Self {
        assert!(index < self.seconds.len());
        // A block holding a second that waits for its Apps, or followed by one, never waits to
        // be packed, so the launch installs only into plain blocks.
        assert_eq!(self.form, Form::Plain);

        let mut seconds = Vec::with_capacity(BLOCK_SECONDS_MAX);
        seconds.extend(self.seconds.iter().map(Arc::clone));
        seconds[index] = second;

        Self {
            half_minute_index: self.half_minute_index,
            seconds,
            form: Form::Plain,
            awaiting: self.awaiting & !(1 << index),
            _reservation: None,
        }
    }

    /// This block's seconds, none of them waiting for its Apps any more.
    #[must_use]
    fn awaiting_none(&self) -> Self {
        assert_eq!(self.form, Form::Plain);

        let mut seconds = Vec::with_capacity(BLOCK_SECONDS_MAX);
        seconds.extend(self.seconds.iter().map(Arc::clone));

        Self {
            half_minute_index: self.half_minute_index,
            seconds,
            form: Form::Plain,
            awaiting: 0,
            _reservation: None,
        }
    }

    /// This block packed, sharing each process's first row with `before`, the process table of
    /// the block before, where the two read the same, and charged to `budget`. Or this block
    /// kept plain for good, with why.
    #[must_use]
    pub(super) fn packing(&self, before: Option<&ProcessTable>, budget: &Arc<ByteBudget>) -> Self {
        assert_eq!(self.form, Form::Plain);
        assert_eq!(self.awaiting, 0);

        let packed = budget
            .reserve(packing_bytes())
            .ok_or(KeptPlain::Budget)
            .and_then(|reservation| {
                let seconds = pack_seconds(&self.seconds, before, budget)?;
                Ok((seconds, reservation))
            });
        let (seconds, form, reservation) = match packed {
            Ok((seconds, reservation)) => (seconds, Form::Packed, Some(reservation)),
            Err(cause) => {
                let mut seconds = Vec::with_capacity(BLOCK_SECONDS_MAX);
                seconds.extend(self.seconds.iter().map(Arc::clone));
                (seconds, Form::KeptPlain(cause), None)
            }
        };
        assert_eq!(seconds.len(), self.seconds.len());

        Self {
            half_minute_index: self.half_minute_index,
            seconds,
            form,
            awaiting: 0,
            _reservation: reservation,
        }
    }

    /// The process table of a packed block, which a second holding processes carries, or
    /// `None` for a plain block or one with no processes at any second.
    #[must_use]
    pub(super) fn table(&self) -> Option<&ProcessTable> {
        if self.form != Form::Packed {
            return None;
        }

        self.seconds.iter().find_map(|second| {
            let payload = second.processes.as_ref().ok()?;
            payload.table().map(AsRef::as_ref)
        })
    }

    /// Whether the second at `index` waits for its Apps.
    #[must_use]
    fn awaits(&self, index: usize) -> bool {
        assert!(index < self.seconds.len());

        self.awaiting & (1 << index) != 0
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

/// The bytes of a packed block's own allocation in the `Arc` the packing thread puts it in, and
/// of its list of seconds.
fn packing_bytes() -> usize {
    2 * size_of::<usize>() + size_of::<Block>() + BLOCK_SECONDS_MAX * size_of::<Arc<AppsSecond>>()
}

/// The index of the thirty wall seconds `wall_ns` falls in. The division floors, so every
/// second of those thirty has one index.
fn half_minute_index(wall_ns: u64) -> u64 {
    wall_ns / BLOCK_WALL_NS
}

/// The buffer the exchange leaves in place of `apps` once it moves out what `apps` holds: the
/// room of its App rows where it holds room no launch reserved, and an empty buffer otherwise,
/// as clearing the buffer in place would leave it.
fn exchange_released(apps: &AppsSecond) -> AppsSecond {
    if apps.apps.reserved_bytes() == 0 {
        if apps.apps.capacity() > 0 {
            return AppsSecond {
                consecutive: apps.consecutive,
                apps: AppRows::with_capacity(apps.apps.capacity()),
                ..AppsSecond::default()
            };
        }
    }

    AppsSecond::default()
}

/// How the exchange names `before`, the block that stands before one it hands over: packed,
/// plain, or kept plain, which shares no row.
fn exchange_before(before: &Arc<Block>) -> Before {
    match before.form {
        Form::Packed => Before::Packed(Arc::clone(before)),
        Form::Plain => Before::Plain(Arc::downgrade(before)),
        Form::KeptPlain(_) => Before::Unshared,
    }
}

/// The seconds of the live span as the store holds them, oldest first, at most `capacity` of
/// them, each written into a buffer of its own and held in a row of blocks. The oldest block
/// may hold seconds that have left the span, which no reader reaches, until its last second
/// leaves. A closed block waits to be packed, which the packing thread does.
pub(super) struct BlockSpan {
    buffers: Buffers<AppsSecond>,
    span: AppsSpan,
    capacity: usize,
    /// The store's end of the queues to the packing thread.
    packing: StoreEnd,
    /// Each block handed over and not yet answered, oldest first, which the store keeps so an
    /// answer takes the place of the very block it was handed and of no other.
    out: Vec<Arc<Block>>,
    /// The batches of what to free handed over and not yet answered.
    batches_out: usize,
    /// The packed blocks that left the row, oldest first, which the store keeps until nothing
    /// else holds them and then hands over to be freed.
    departed: VecDeque<Arc<Block>>,
    /// How many packed blocks that left the row the store let go of past `DEPARTED_KEPT_MAX`,
    /// each then freed by its last holder.
    departed_let_go_count: u64,
    /// The packing thread's end, which a test keeps to run the packing loop's body itself.
    #[cfg(test)]
    packing_end: Option<super::packing::PackingEnd>,
}

impl BlockSpan {
    /// A span of at most `capacity` seconds over `buffers`, which holds more buffers than
    /// seconds and the seconds that have left them, so a View the main thread holds for a moment
    /// costs the store no new buffer, handing its blocks to be packed through `packing`.
    #[must_use]
    pub(super) fn new(capacity: usize, buffers: Buffers<AppsSecond>, packing: StoreEnd) -> Self {
        assert!(capacity > 0);
        assert!(buffers.count() > capacity + BLOCK_DEPARTED_MAX);

        Self {
            buffers,
            span: AppsSpan::with_capacity(capacity),
            capacity,
            packing,
            out: Vec::with_capacity(BLOCKS_OUT_MAX),
            batches_out: 0,
            departed: VecDeque::with_capacity(DEPARTED_KEPT_MAX),
            departed_let_go_count: 0,
            #[cfg(test)]
            packing_end: None,
        }
    }

    /// Takes each answer the packing thread has given and puts it in its plain block's place,
    /// then hands over the oldest ready blocks while fewer than `BLOCKS_OUT_MAX` are out, and
    /// what is to be freed. Waits for nothing.
    pub(super) fn exchange(&mut self) {
        for _ in 0..QUEUE_MAX {
            let Some(answer) = self.packing.answer() else {
                break;
            };
            self.exchange_answer(answer);
        }
        self.exchange_blocks();
        self.exchange_freed();
    }

    /// Acts on `answer`, which came back for a block or a batch that is out.
    fn exchange_answer(&mut self, answer: Answer) {
        match answer {
            Answer::Block { plain, replacement } => {
                let place = self.out.iter().position(|out| Arc::ptr_eq(out, &plain));
                let Some(place) = place else {
                    unreachable!("an answer comes for a block that is out");
                };
                drop(self.out.remove(place));
                // Packed or kept plain for a cause, the replacement takes the plain block's
                // place, so a block kept plain waits no more and is never handed over again.
                assert!(matches!(
                    replacement.form,
                    Form::Packed | Form::KeptPlain(_)
                ));
                // A block that has left the row since, or that a block of its seconds replaced,
                // takes no replacement, which goes as a block that left the row goes.
                let blocks = &mut self.span.blocks;
                let index = blocks.iter().position(|block| Arc::ptr_eq(block, &plain));
                if let Some(index) = index {
                    blocks[index] = replacement;
                } else {
                    self.keep_departed(replacement);
                }
            }
            Answer::Freed => {
                assert!(self.batches_out > 0);
                self.batches_out -= 1;
            }
        }
    }

    /// Hands over the oldest ready blocks not yet out while fewer than `BLOCKS_OUT_MAX` are,
    /// each with the block that stands before it.
    fn exchange_blocks(&mut self) {
        for block_index in 0..self.span.blocks.len() {
            if self.out.len() == BLOCKS_OUT_MAX {
                break;
            }
            if !self.span.waits(block_index) {
                continue;
            }
            let block = &self.span.blocks[block_index];
            if self.out.iter().any(|out| Arc::ptr_eq(out, block)) {
                continue;
            }
            let before = match block_index.checked_sub(1) {
                Some(before_index) => exchange_before(&self.span.blocks[before_index]),
                None => Before::Unshared,
            };
            self.out.push(Arc::clone(block));
            self.packing.hand(Handed::Block {
                block: Arc::clone(block),
                before,
            });
        }
    }

    /// Hands over what is to be freed while fewer than `BATCHES_OUT_MAX` batches are out: the
    /// readings of free buffers, then packed blocks that left the row and nothing else holds.
    fn exchange_freed(&mut self) {
        // A buffer the plain seconds leave free keeps no rows while it waits to be written, and
        // the packing thread frees what it held.
        for _ in self.batches_out..BATCHES_OUT_MAX {
            let batch = self
                .buffers
                .release_once_batch(BATCH_BUFFERS_MAX, exchange_released);
            if batch.is_empty() {
                break;
            }
            self.batches_out += 1;
            self.packing.hand(Handed::Batch(Batch::Seconds(batch)));
        }
        // A packed block that left the row goes once nothing else holds it, the oldest first.
        for _ in self.batches_out..BATCHES_OUT_MAX {
            let Some(index) = self
                .departed
                .iter()
                .position(|block| Arc::strong_count(block) == 1)
            else {
                break;
            };
            let Some(block) = self.departed.remove(index) else {
                unreachable!("the position is of a block the store keeps");
            };
            self.batches_out += 1;
            self.packing.hand(Handed::Batch(Batch::Departed(block)));
        }
    }

    /// Keeps `block`, which left the row, until nothing else holds it, where it is packed, so
    /// the store's thread frees no block's rows. Past `DEPARTED_KEPT_MAX` it lets go of the
    /// oldest it keeps, which its last holder then frees, and counts it. A plain block's
    /// seconds are the store's buffers, which go as any free buffer goes.
    fn keep_departed(&mut self, block: Arc<Block>) {
        if block.form != Form::Packed {
            return;
        }
        if self.departed.len() == DEPARTED_KEPT_MAX {
            drop(self.departed.pop_front());
            self.departed_let_go_count += 1;
        }

        self.departed.push_back(block);
    }

    /// Adds the second `fill` writes after the newest, which the store took at wall time
    /// `wall_ns` and which waits for a launch to install its Apps where `awaiting` is true,
    /// letting go of the oldest when the span is full. A View holding the blocks before keeps
    /// them, and stops nothing.
    pub(super) fn push(
        &mut self,
        wall_ns: u64,
        awaiting: bool,
        fill: impl FnOnce(&mut AppsSecond),
    ) {
        if self.span.len == self.capacity {
            self.remove_oldest();
        }
        assert!(self.span.len < self.capacity);
        // Every block holds a second of the span, the oldest too, so the row of blocks, which
        // has room for one block more than the span has seconds, never grows.
        assert!(self.span.blocks.len() <= self.span.len);
        assert!(self.span.blocks.len() < self.span.block_capacity());

        let second = self.buffers.write_replacing(fill);
        self.span.push_second(wall_ns, second, awaiting);
        self.exchange();
    }

    /// Before a launch installs a minute, waits for answers while more than `BLOCKS_OUT_MAX`
    /// ready blocks are plain, which keeps the launch's plain rows inside their bound. It waits
    /// only while a block is out, so an answer is always coming.
    pub(super) fn launch_wait(&mut self) {
        // Each turn takes one answer, for a block or a batch, and each is handed once.
        let bound = self.span.blocks.capacity() + self.buffers.count() + DEPARTED_KEPT_MAX + 1;
        for _ in 0..bound {
            self.exchange();
            if self.ready_plain_count() <= BLOCKS_OUT_MAX {
                return;
            }
            // The exchange hands over ready blocks until `BLOCKS_OUT_MAX` are out.
            assert_eq!(self.out.len(), BLOCKS_OUT_MAX);
            let answer = self.packing.answer_waiting();
            self.exchange_answer(answer);
        }
        unreachable!("each answer packs a ready block or frees a batch handed once");
    }

    /// How many closed blocks are ready and still plain, handed over or not.
    pub(super) fn ready_plain_count(&self) -> usize {
        (0..self.span.blocks.len())
            .filter(|&block_index| self.span.waits(block_index))
            .count()
    }

    /// Lets every second go on as any other, once a launch installs no more Apps, so its
    /// blocks wait to be packed.
    pub(super) fn end_awaiting(&mut self) {
        for block in &mut self.span.blocks {
            if block.awaiting != 0 {
                *block = Arc::new(block.awaiting_none());
            }
        }
        self.exchange();
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
            let departed = span.blocks.remove(0);
            span.departed_count = 0;
            self.keep_departed(departed);
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
        self.exchange();
    }

    /// Writes into `view` the blocks the span holds, each shared with the View, and how many
    /// seconds of the oldest have left.
    pub(super) fn publish(&self, view: &mut AppsSpan) {
        view.blocks.clear();
        view.blocks.extend(self.span.blocks.iter().map(Arc::clone));
        view.departed_count = self.span.departed_count;
        view.len = self.span.len;
        view.departed_let_go_count = self.departed_let_go_count;
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
    /// and the rows of the next second from. Only the store's writers read a second this way.
    #[must_use]
    pub(super) fn stored(&self, index: usize) -> Option<&AppsSecond> {
        if index >= self.span.len {
            return None;
        }
        let (block_index, second_index) = self.span.place(index);
        let block = &self.span.blocks[block_index];
        // The launch installs a second only into a block that cannot wait to be packed, the one
        // before it included, since a block is kept plain while the second after its last waits
        // for its Apps.
        assert!(matches!(block.form, Form::Plain | Form::KeptPlain(_)));

        Some(&block.seconds[second_index])
    }

    /// The newest second's buffer, which the store reads the next second's rates from. The
    /// newest block is the open one, so the store took this second as it is.
    #[must_use]
    pub(super) fn newest_held(&self) -> Option<&Arc<AppsSecond>> {
        let newest = self.span.blocks.last()?;

        newest.seconds.last()
    }

    /// Lets go of every second, keeping the buffers, and keeps each packed block until nothing
    /// else holds it.
    pub(super) fn clear(&mut self) {
        let mut blocks = std::mem::take(&mut self.span.blocks);
        for block in blocks.drain(..) {
            self.keep_departed(block);
        }
        self.span.clear();
        // The row's room goes back, so clearing allocates nothing.
        self.span.blocks = blocks;
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
    /// How many packed blocks that left the row the store let go of past the blocks it keeps
    /// until nothing else holds them, each freed by its last holder.
    departed_let_go_count: u64,
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
            departed_let_go_count: 0,
        }
    }

    /// How many seconds the span holds.
    #[must_use]
    pub fn len(&self) -> usize {
        self.len
    }

    /// How many packed blocks that left the row the store let go of past the blocks it keeps
    /// until nothing else holds them, each freed by whatever held it last.
    #[must_use]
    pub fn departed_let_go_count(&self) -> u64 {
        self.departed_let_go_count
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

    /// How many blocks of the span are kept plain for `cause`, which a View counts so a reader
    /// can tell a span that packs from one that does not.
    #[must_use]
    pub fn kept_plain_count(&self, cause: KeptPlain) -> usize {
        self.blocks
            .iter()
            .filter(|block| block.form == Form::KeptPlain(cause))
            .count()
    }

    /// How many blocks the span holds, the oldest counted while a second of it is in the span.
    #[must_use]
    pub fn block_count(&self) -> usize {
        self.blocks.len()
    }

    /// How many blocks of the span are packed.
    #[must_use]
    pub fn packed_count(&self) -> usize {
        self.blocks
            .iter()
            .filter(|block| block.form == Form::Packed)
            .count()
    }

    /// Whether the block at `block_index` waits to be packed: it is closed and plain, and
    /// neither one of its seconds nor the second after its last waits for its Apps, so a launch
    /// never installs into a packed block nor reads the second before an install from one.
    fn waits(&self, block_index: usize) -> bool {
        let block = &self.blocks[block_index];
        let Some(next) = self.blocks.get(block_index + 1) else {
            return false;
        };
        if block.form != Form::Plain {
            return false;
        }
        if block.awaiting != 0 {
            return false;
        }

        !next.awaits(0)
    }

    /// The second at `index` of the span, the oldest at 0, as the span holds it.
    #[cfg(any(test, feature = "fixtures"))]
    fn stored_at(&self, index: usize) -> &Arc<AppsSecond> {
        let (block_index, second_index) = self.place(index);

        &self.blocks[block_index].seconds[second_index]
    }

    /// Adds `second`, which the store took at wall time `wall_ns` and which waits for its Apps
    /// where `awaiting` is true, after the newest: in a new block in place of the newest where
    /// that block is open to it, and in a block of its own where not.
    fn push_second(&mut self, wall_ns: u64, second: Arc<AppsSecond>, awaiting: bool) {
        let joined = self
            .blocks
            .last()
            .and_then(|newest| newest.joining(wall_ns, Arc::clone(&second), awaiting));
        match (joined, self.blocks.last_mut()) {
            (Some(joined), Some(newest)) => *newest = Arc::new(joined),
            (Some(_), None) => unreachable!("only a block of the row joins a second"),
            (None, _) => self
                .blocks
                .push(Arc::new(Block::starting(wall_ns, second, awaiting))),
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

        Ok((
            header,
            Processes {
                payload,
                apps: self.second.apps.as_slice(),
            },
        ))
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
            Ok(payload) => Ok(Processes {
                payload,
                apps: self.second.apps.as_slice(),
            }),
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
    /// The second's Apps, which name the App a reader asks for the members of.
    apps: &'block [AppRow],
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

    /// Every process of the second, in the sample's order.
    #[must_use]
    pub fn iter(self) -> impl ExactSizeIterator<Item = ProcessAt<'block>> {
        self.payload.processes()
    }

    /// The processes of the App at `app_index` of the second's Apps, or of no App where
    /// `app_index` is `None`, in the sample's order.
    pub fn members(
        self,
        app_index: Option<u32>,
    ) -> impl Iterator<Item = ProcessAt<'block>> + Clone {
        self.payload.members(app_index, self.apps)
    }

    /// The process of pid `pid` created at `creation`, where the second holds it.
    #[must_use]
    pub fn find(self, pid: i32, creation: Option<ProcessCreation>) -> Option<ProcessAt<'block>> {
        self.payload.find(pid, creation)
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
            span.push_second(wall_ns, second, false);
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
            departed_let_go_count: self.departed_let_go_count,
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

        Self::keeping_packing_end(
            capacity,
            buffers,
            Arc::new(ByteBudget::new(256 * 1024 * 1024)),
        )
    }

    /// A span as `new` makes it, which keeps the packing thread's end for the test to run.
    pub(super) fn keeping_packing_end(
        capacity: usize,
        buffers: Buffers<AppsSecond>,
        budget: Arc<ByteBudget>,
    ) -> Self {
        let (store_end, packing_end) = super::packing::pair(budget);
        let mut span = Self::new(capacity, buffers, store_end);
        span.keep_packing_end(packing_end);

        span
    }

    /// A span as `new` makes it, whose blocks the packing thread packs on a thread of its own,
    /// which wakes the test's thread with each answer and ends once the span is gone.
    pub(super) fn with_packing_thread(
        capacity: usize,
        buffers: Buffers<AppsSecond>,
        budget: Arc<ByteBudget>,
    ) -> Self {
        let (store_end, packing_end) = super::packing::pair(budget);
        // The thread ends once the store's end is gone, so the test lets go of its handle.
        drop(packing_end.start(std::thread::current()).unwrap());

        Self::new(capacity, buffers, store_end)
    }

    /// Keeps `packing_end`, the other end of this span's queues, for the test to run.
    pub(super) fn keep_packing_end(&mut self, packing_end: super::packing::PackingEnd) {
        assert!(self.packing_end.is_none());
        self.packing_end = Some(packing_end);
    }

    /// Runs the packing loop's body on every message handed over, as the packing thread would,
    /// and says how many of them were blocks.
    pub(super) fn answer_handed(&mut self) -> usize {
        let Some(packing_end) = &mut self.packing_end else {
            unreachable!("the test runs a packing thread of its own");
        };

        packing_end.answer_handed()
    }

    /// Exchanges with the packing loop's body until nothing is out, which packs every ready
    /// block and frees what waits to be freed, as a store left alone with its packing thread
    /// ends.
    pub(super) fn pack_ready(&mut self) {
        let bound = self.span.blocks.capacity() + self.buffers.count() + DEPARTED_KEPT_MAX + 1;
        for _ in 0..bound {
            self.exchange();
            if self.out.is_empty() && self.batches_out == 0 {
                return;
            }
            self.answer_handed();
        }
        unreachable!("each exchange hands over a ready block, a batch, or nothing");
    }

    /// The span's buffers that nothing else holds.
    pub(super) fn unheld_buffers(&self) -> impl Iterator<Item = &AppsSecond> {
        self.buffers.unheld()
    }

    /// What the read benchmark counts of the packing thread's work.
    #[cfg(feature = "read-benchmark")]
    pub(super) fn packing_tally(&self) -> &super::packing::Tally {
        self.packing.tally()
    }

    /// Exchanges with a packing thread running on its own, parked between exchanges until an
    /// answer wakes it, until the span has settled, as the read benchmark does before it
    /// reports bytes or a footprint.
    pub(super) fn settle(&mut self) {
        // A turn waits at most a millisecond, so the bound is 100 seconds of packing.
        for _ in 0..100_000 {
            self.exchange();
            if self.settled() {
                return;
            }
            std::thread::park_timeout(std::time::Duration::from_millis(1));
        }
        panic!("the packing thread left the span unsettled");
    }

    /// Whether nothing is out and nothing waits: no ready block plain, no free buffer holding
    /// readings, and no packed block that left the row that nothing else holds.
    pub(super) fn settled(&self) -> bool {
        if !self.out.is_empty() {
            return false;
        }
        if self.batches_out > 0 {
            return false;
        }
        if self.ready_plain_count() > 0 {
            return false;
        }
        if self
            .departed
            .iter()
            .any(|block| Arc::strong_count(block) == 1)
        {
            return false;
        }

        !self.buffers.releasable()
    }

    /// How many packed blocks that left the row the store keeps.
    pub(super) fn departed_kept_count(&self) -> usize {
        self.departed.len()
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

    /// The second at `index` of the span as the span holds it, packed or plain.
    pub(super) fn get(&self, index: usize) -> Option<&AppsSecond> {
        (index < self.span.len).then(|| self.span.stored_at(index).as_ref())
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
