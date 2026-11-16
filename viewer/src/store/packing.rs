//! The packing thread, which packs the closed blocks the store's thread hands it, so that
//! neither the store's thread nor the window's packs a block. Two bounded queues join the
//! threads, one each way, and each holds every message that can be out at once, so neither
//! thread ever waits to put one in and every message is answered.

use std::io;
#[cfg(all(test, feature = "read-benchmark"))]
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvError, SyncSender, TryRecvError, TrySendError, sync_channel};
use std::sync::{Arc, Weak};
use std::thread::{self, JoinHandle, Thread};

use super::AppsSecond;
use super::block::{BLOCK_SECONDS_MAX, Block};
use super::payload::packed::AppRowsShare;
use crate::byte_budget::ByteBudget;

#[cfg(test)]
#[path = "packing_tests.rs"]
mod tests;

/// The blocks handed over and not yet answered, at most.
pub(super) const BLOCKS_OUT_MAX: usize = 2;

/// The batches of what to free handed over and not yet answered, at most.
pub(super) const BATCHES_OUT_MAX: usize = 2;

/// The buffers whose readings one batch frees, at most, as many as a block holds.
pub(super) const BATCH_BUFFERS_MAX: usize = BLOCK_SECONDS_MAX;

/// The messages each queue holds, every message that can be out at once.
pub(super) const QUEUE_MAX: usize = BLOCKS_OUT_MAX + BATCHES_OUT_MAX;

/// The messages the packing thread answers, which it never reaches, since it answers until the
/// store's end is gone.
const ANSWERS_MAX: u64 = u64::MAX;

/// What the store's thread hands the packing thread.
pub(super) enum Handed {
    /// A closed plain block to pack, and the block that stands before it in the row.
    Block { block: Arc<Block>, before: Before },
    /// What the store's thread let go of and the packing thread frees.
    Batch(Batch),
}

/// One batch of what the packing thread frees for the store's thread.
pub(super) enum Batch {
    /// What the store's thread moved out of buffers nothing else holds, at most
    /// `BATCH_BUFFERS_MAX` of them.
    Seconds(Vec<AppsSecond>),
    /// A packed block that left the row and that nothing else holds.
    Departed(Arc<Block>),
}

/// The block that stands before a block handed over, as the store's thread names it.
pub(super) enum Before {
    /// No block stands before it, or the one before was kept plain, so it shares no row.
    Unshared,
    /// The row holds the block before packed, which it shares its first rows with.
    Packed(Arc<Block>),
    /// The row holds the block before plain, still out or never handed over. The packing
    /// thread shares the first rows of the block it made from it where that is the block it
    /// made last.
    Plain(Weak<Block>),
}

/// What the packing thread answers each message with.
pub(super) enum Answer {
    /// The block `plain` the store handed over, and `replacement`, the same seconds packed or
    /// kept plain for a cause.
    Block {
        plain: Arc<Block>,
        replacement: Arc<Block>,
    },
    /// A batch handed over is freed.
    Freed,
}

/// The store's thread's end of the two queues.
pub(super) struct StoreEnd {
    handed: SyncSender<Handed>,
    answers: Receiver<Answer>,
    #[cfg(all(test, feature = "read-benchmark"))]
    tally: Arc<Tally>,
}

/// The packing thread's end of the two queues.
pub(super) struct PackingEnd {
    handed: Receiver<Handed>,
    answers: SyncSender<Answer>,
    budget: Arc<ByteBudget>,
    /// The share under which a block packs its App rows.
    share: AppRowsShare,
    /// The block the packing thread answered last and its replacement, held weakly so neither
    /// keeps its bytes alive and the address of neither passes to another block.
    last: Option<Last>,
    /// Wakes the store's thread, which may be parked for a hidden window.
    wake: Box<dyn Fn() + Send>,
    #[cfg(all(test, feature = "read-benchmark"))]
    tally: Arc<Tally>,
}

/// What the read benchmark counts of the packing thread's work: the messages handed over and
/// those answered, the time the packing of each block took and the time freeing took.
#[cfg(all(test, feature = "read-benchmark"))]
#[derive(Debug, Default)]
pub(super) struct Tally {
    handed_count: AtomicU64,
    answered_count: AtomicU64,
    block_count: AtomicU64,
    block_ns_sum: AtomicU64,
    block_ns_max: AtomicU64,
    freed_count: AtomicU64,
    freed_ns_sum: AtomicU64,
}

/// The packing of the blocks a tally counted.
#[cfg(all(test, feature = "read-benchmark"))]
pub(super) struct BlockTimes {
    pub(super) block_count: u64,
    pub(super) mean_ns: u64,
    pub(super) highest_ns: u64,
}

/// The freeing of the batches a tally counted.
#[cfg(all(test, feature = "read-benchmark"))]
pub(super) struct FreeingTimes {
    pub(super) batch_count: u64,
    pub(super) ns_sum: u64,
}

#[cfg(all(test, feature = "read-benchmark"))]
impl Tally {
    /// Whether the packing thread has answered every message handed over.
    pub(super) fn finished(&self) -> bool {
        let handed_count = self.handed_count.load(Ordering::Acquire);
        let answered_count = self.answered_count.load(Ordering::Acquire);
        assert!(answered_count <= handed_count);

        answered_count == handed_count
    }

    /// The blocks packed, with the mean, which floors, and the highest time one took.
    pub(super) fn block_times(&self) -> BlockTimes {
        let block_count = self.block_count.load(Ordering::Acquire);
        assert!(block_count > 0);

        BlockTimes {
            block_count,
            mean_ns: self.block_ns_sum.load(Ordering::Acquire) / block_count,
            highest_ns: self.block_ns_max.load(Ordering::Acquire),
        }
    }

    /// The batches freed and the time freeing them took.
    pub(super) fn freeing_times(&self) -> FreeingTimes {
        FreeingTimes {
            batch_count: self.freed_count.load(Ordering::Acquire),
            ns_sum: self.freed_ns_sum.load(Ordering::Acquire),
        }
    }

    /// Counts a block whose packing took `elapsed`.
    fn block(&self, elapsed: std::time::Duration) {
        let elapsed_ns = u64::try_from(elapsed.as_nanos())
            .unwrap_or_else(|_| unreachable!("a block packs in under 584 years"));
        self.block_ns_sum.fetch_add(elapsed_ns, Ordering::AcqRel);
        self.block_ns_max.fetch_max(elapsed_ns, Ordering::AcqRel);
        self.block_count.fetch_add(1, Ordering::AcqRel);
    }

    /// Counts a batch whose freeing took `elapsed`.
    fn freed(&self, elapsed: std::time::Duration) {
        let elapsed_ns = u64::try_from(elapsed.as_nanos())
            .unwrap_or_else(|_| unreachable!("a batch frees in under 584 years"));
        self.freed_ns_sum.fetch_add(elapsed_ns, Ordering::AcqRel);
        self.freed_count.fetch_add(1, Ordering::AcqRel);
    }
}

/// The block the packing thread answered last, `plain`, and its replacement.
struct Last {
    plain: Weak<Block>,
    replacement: Weak<Block>,
}

/// The two ends of the queues between the store's thread and the packing thread, which packs
/// within `budget`, packing a block's App rows only where `share` admits them.
pub(super) fn pair(budget: Arc<ByteBudget>, share: AppRowsShare) -> (StoreEnd, PackingEnd) {
    let (handed_sender, handed_receiver) = sync_channel(QUEUE_MAX);
    let (answer_sender, answer_receiver) = sync_channel(QUEUE_MAX);
    #[cfg(all(test, feature = "read-benchmark"))]
    let tally = Arc::new(Tally::default());

    let store = StoreEnd {
        handed: handed_sender,
        answers: answer_receiver,
        #[cfg(all(test, feature = "read-benchmark"))]
        tally: Arc::clone(&tally),
    };
    let packing = PackingEnd {
        handed: handed_receiver,
        answers: answer_sender,
        budget,
        share,
        last: None,
        wake: Box::new(|| {}),
        #[cfg(all(test, feature = "read-benchmark"))]
        tally,
    };
    (store, packing)
}

impl StoreEnd {
    /// Hands `handed` to the packing thread, which a queue holding every message that can be out
    /// always has room for.
    pub(super) fn hand(&self, handed: Handed) {
        match self.handed.try_send(handed) {
            Ok(()) => {
                #[cfg(all(test, feature = "read-benchmark"))]
                self.tally.handed_count.fetch_add(1, Ordering::AcqRel);
            }
            Err(TrySendError::Full(_)) => {
                unreachable!("the queue holds every message that can be out")
            }
            // A failed assertion there aborts the build people use, so only a test gets here.
            Err(TrySendError::Disconnected(_)) => panic!("the packing thread has ended"),
        }
    }

    /// The next answer, waiting for it to come back.
    pub(super) fn answer_waiting(&self) -> Answer {
        match self.answers.recv() {
            Ok(answer) => answer,
            Err(RecvError) => panic!("the packing thread has ended"),
        }
    }

    /// What the read benchmark counts of the packing thread's work.
    #[cfg(all(test, feature = "read-benchmark"))]
    pub(super) fn tally(&self) -> &Tally {
        &self.tally
    }

    /// The next answer that has come back, if one has.
    pub(super) fn answer(&self) -> Option<Answer> {
        match self.answers.try_recv() {
            Ok(answer) => Some(answer),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => panic!("the packing thread has ended"),
        }
    }
}

impl PackingEnd {
    /// The packing thread's loop body: packs `handed` and puts the answer in the queue. False
    /// where the store's end is gone and no answer can be given.
    fn start_loop_turn(&mut self, handed: Handed) -> bool {
        let answer = match handed {
            Handed::Block { block, before } => {
                #[cfg(all(test, feature = "read-benchmark"))]
                let started = std::time::Instant::now();
                let before = self.start_loop_turn_before(before);
                let table = before.as_deref().and_then(Block::table);
                let replacement = Arc::new(block.packing(table, &self.budget, self.share));
                #[cfg(all(test, feature = "read-benchmark"))]
                self.tally.block(started.elapsed());
                self.last = Some(Last {
                    plain: Arc::downgrade(&block),
                    replacement: Arc::downgrade(&replacement),
                });
                Answer::Block {
                    plain: block,
                    replacement,
                }
            }
            Handed::Batch(batch) => {
                #[cfg(all(test, feature = "read-benchmark"))]
                let started = std::time::Instant::now();
                start_loop_turn_batch(batch);
                #[cfg(all(test, feature = "read-benchmark"))]
                self.tally.freed(started.elapsed());
                Answer::Freed
            }
        };
        match self.answers.try_send(answer) {
            Ok(()) => {
                #[cfg(all(test, feature = "read-benchmark"))]
                self.tally.answered_count.fetch_add(1, Ordering::AcqRel);
                (self.wake)();
                true
            }
            Err(TrySendError::Full(_)) => {
                unreachable!("the queue holds every message that can be out")
            }
            Err(TrySendError::Disconnected(_)) => false,
        }
    }

    /// Starts the packing thread, which answers each message the store's thread `store` hands
    /// over, wakes it with each answer, and ends once the store's end is gone.
    pub(super) fn start(mut self, store: Thread) -> io::Result<JoinHandle<()>> {
        self.wake = Box::new(move || store.unpark());
        thread::Builder::new()
            .name(String::from("flightdeck-packing"))
            .spawn(move || self.start_loop())
    }

    /// The packing thread's loop, which waits for each message and runs as long as the
    /// store's end, whose queues hold every message that can be out.
    fn start_loop(mut self) {
        for _ in 0..ANSWERS_MAX {
            let Ok(handed) = self.handed.recv() else {
                return;
            };
            if !self.start_loop_turn(handed) {
                return;
            }
        }
        unreachable!("the packing thread answers until the store's end is gone");
    }

    /// The block whose first rows a block shares, as the store named it in `before`: the
    /// packed block the row holds, or the one made last where the block named is the one it
    /// was made from.
    fn start_loop_turn_before(&self, before: Before) -> Option<Arc<Block>> {
        match before {
            Before::Unshared => None,
            Before::Packed(block) => Some(block),
            Before::Plain(named) => {
                let last = self.last.as_ref()?;
                if !Weak::ptr_eq(&last.plain, &named) {
                    return None;
                }

                last.replacement.upgrade()
            }
        }
    }
}

/// Frees `batch`, which the store's thread let go of.
fn start_loop_turn_batch(batch: Batch) {
    match batch {
        Batch::Seconds(seconds) => {
            assert!(seconds.len() <= BATCH_BUFFERS_MAX);
            drop(seconds);
        }
        Batch::Departed(block) => {
            // The store's thread hands over a block only once nothing else holds it, and the
            // packing thread keeps no block but weakly.
            assert_eq!(Arc::strong_count(&block), 1);
            drop(block);
        }
    }
}

/// What a test of the store reads to run the packing thread's loop body itself.
#[cfg(test)]
impl PackingEnd {
    /// Makes `wake` what wakes the store's thread with each answer.
    pub(super) fn wake_with(&mut self, wake: Box<dyn Fn() + Send>) {
        self.wake = wake;
    }

    /// Answers every message the store's thread has handed over and the packing thread has not
    /// answered, one at a time, and says how many of them were blocks.
    pub(super) fn answer_handed(&mut self) -> usize {
        let mut block_count = 0;
        for _ in 0..QUEUE_MAX {
            let Ok(handed) = self.handed.try_recv() else {
                break;
            };
            if matches!(handed, Handed::Block { .. }) {
                block_count += 1;
            }
            assert!(self.start_loop_turn(handed));
        }

        block_count
    }
}
