//! An interrupted ordinary attempt, finished from what it saved and the source's retained window.

use super::attempt::{TABLE_FILES, validate_terminal_source};
use super::inventory::{BatchFact, InventoryStep, OriginalFile};
use super::{RegisteredAttempt, StagingPermit, Store, publication_control};
use crate::canonical::CanonicalMetadata;
use crate::event_recovery::{Batch, batch_name};
use crate::resources::ByteBudget;
use crate::transaction::AttemptOperation;
use collector::recording::bootstrap::codec::{self, Phase};
use collector::recording::private::PrivateDirectory;
use collector::ring::RunIdentity;
use std::io;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// More inventory passes than the name scan and every numbered position take.
const INVENTORY_STEPS_MAX: u32 = 1 << 16;

/// Every original an ordinary attempt writes other than its event batches and index.
const TABLE_KINDS: [OriginalFile; 12] = [
    OriginalFile::Ticks,
    OriginalFile::Memory,
    OriginalFile::Cpus,
    OriginalFile::Interfaces,
    OriginalFile::GpuStates,
    OriginalFile::CoreStates,
    OriginalFile::Fans,
    OriginalFile::Processes,
    OriginalFile::Coalitions,
    OriginalFile::AppRanks,
    OriginalFile::AppParts,
    OriginalFile::Metadata,
];

const _: () = assert!(TABLE_KINDS.len() == TABLE_FILES.len() + 1);

/// What a process left in the foreground slot, by what can finish it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Interrupted {
    /// No proof is durable, so finishing it takes the run's source.
    Staging(RunIdentity),
    /// The proof is durable, so it publishes without the source.
    Publication,
    /// A run's end with nothing but what it lost, which its source may still have to prove.
    Loss(RunIdentity),
    /// Saved rows on their way to a fragment, which takes no source.
    Rows,
}

impl Store {
    /// Whether any attempt holds the foreground slot, which reads the slot and nothing it names.
    pub(crate) fn slot_occupied(&self) -> io::Result<bool> {
        let mut work = super::PublicationWork::default();
        let state =
            publication_control::read_optional_normal(&self.directory, self.recording, &mut work)?;

        Ok(state.is_some())
    }

    /// Names what the foreground slot holds, or nothing when it is idle. Routes the worker does
    /// not start yet refuse here, so nothing they left is taken for an ordinary attempt.
    pub(crate) fn interrupted(&self) -> io::Result<Option<Interrupted>> {
        let mut work = super::PublicationWork::default();
        let Some(state) =
            publication_control::read_optional_normal(&self.directory, self.recording, &mut work)?
        else {
            return Ok(None);
        };
        if state.input.is_finite() {
            return Err(interrupted_without_route());
        }
        if state.row_plan.is_some() {
            return Ok(Some(Interrupted::Rows));
        }
        if state.census.kind != 0 || state.salvage.is_some() {
            return if matches!(state.input.operation, AttemptOperation::Terminal { .. }) {
                Ok(Some(Interrupted::Loss(state.input.run)))
            } else if state.salvage.is_none() {
                Ok(Some(Interrupted::Rows))
            } else {
                Err(interrupted_without_route())
            };
        }
        if state.phase != Phase::Registered {
            return Ok(Some(Interrupted::Publication));
        }

        let staging = Some(Interrupted::Staging(state.input.run));
        let mut file = match self.resume_attempt()?.open_file("metadata") {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(staging),
            Err(error) => return Err(error),
        };
        // A process that ended while writing its proof left it short, so it proves nothing. A file
        // of its full length that does not read is damage, which stops the worker.
        if CanonicalMetadata::short(&mut file)? {
            return Ok(staging);
        }
        CanonicalMetadata::read(&mut file)?;

        Ok(Some(Interrupted::Publication))
    }
}

fn interrupted_without_route() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "interrupted attempt has no worker route",
    )
}

/// What is left to publish of an attempt whose source can no longer supply its window.
pub(crate) enum Kept {
    Nothing,
    /// Saved rows on their way to a fragment.
    Rows,
    /// A terminal's saved rows, whose proof is now whole.
    Proof,
}

/// What an interrupted ordinary attempt left in its registered directories.
pub(crate) struct Saved {
    /// The complete event batches, in the order the writer creates them.
    pub(crate) batches: Vec<Batch>,
    /// The batch after them that no row can be read from, if there is one.
    torn: Option<usize>,
    /// Whether any table reached the directory, whole or not.
    tables: bool,
}

impl Saved {
    fn nothing(&self) -> bool {
        self.batches.is_empty() && !self.tables
    }

    /// The events from `first` to the end of the saved batches that no batch holds. Those are the
    /// ones the writer counted as lost before the process ended, and it will not read them again.
    pub(crate) fn events_lost(&self, first: u64) -> io::Result<u64> {
        let Some(last) = self.batches.last() else {
            return Ok(0);
        };
        let rows: u64 = self.batches.iter().map(|batch| batch.rows).sum();
        last.end
            .checked_sub(first)
            .and_then(|covered| covered.checked_sub(rows))
            .ok_or_else(restage_refused)
    }
}

impl Store {
    /// Clears the slot of an attempt that saved no row and no table, and says whether it did.
    /// This claims nothing about the attempt's window: the frontier stays where it was, so the
    /// next scan finds again whatever the ring still holds and counts what it no longer does,
    /// and a source that is gone stays an unknown extent. It takes no source for that reason.
    pub(crate) fn abandon_empty_attempt(&self, budget: &Arc<ByteBudget>) -> crate::Result<bool> {
        let mut attempt = self.resume_attempt()?;
        let saved = attempt.saved(budget)?;
        if !saved.nothing() {
            return Ok(false);
        }

        attempt.abandon(&saved)?;
        Ok(true)
    }

    /// Takes the tables and any short proof from an attempt whose source can no longer supply its
    /// window, which no reader could reach without a proof, and clears the slot if that leaves
    /// nothing. Says whether rows are left to keep. The tables go first, so a process that ends
    /// part way finds the same attempt again and this converges.
    pub(crate) fn keep_only_rows(&self, budget: &Arc<ByteBudget>) -> crate::Result<Kept> {
        let mut attempt = self.resume_attempt()?;
        let terminal = matches!(attempt.input.operation, AttemptOperation::Terminal { .. });
        if !terminal {
            attempt.restage_tables()?;
        }
        restage_remove(attempt.original_directory(), "metadata")?;
        attempt.original_directory().full_sync()?;
        let saved = attempt.saved(budget)?;
        if saved.nothing() {
            attempt.abandon(&saved)?;
            return Ok(Kept::Nothing);
        }
        if !terminal {
            return Ok(Kept::Rows);
        }
        if let Some(ordinal) = saved.torn {
            restage_remove(attempt.original_directory(), &batch_name(ordinal))?;
        }
        attempt.write_kept_terminal(&saved.batches)?;

        Ok(Kept::Proof)
    }

    /// Clears the slot of a terminal that began counting what it lost and whose source is gone, so
    /// that nothing can prove the count. Only a terminal with no rows goes that way, so all it can
    /// leave is its census, a torn first batch and a short proof. They go before the slot, so a
    /// process that ends between the two finds the same attempt again.
    pub(crate) fn abandon_loss_attempt(&self) -> io::Result<()> {
        if self
            .staging
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        let _permit = StagingPermit(Arc::clone(&self.staging));
        let state = publication_control::read_normal(&self.directory, self.recording)?;
        if state.phase != Phase::Registered
            || !matches!(state.input.operation, AttemptOperation::Terminal { .. })
        {
            return Err(restage_refused());
        }
        let base = self.directory.open_directory("recovery-data")?;
        let name = collector::recording::identity_text(state.transaction).to_string();
        match base.open_directory(&name) {
            Ok(object) => {
                abandon_loss_object(&object)?;
                drop(object);
                base.remove_directory(&name)?;
                base.full_sync()?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let mut idle = [0; 4096];
        codec::encode_idle(self.recording, codec::SlotKind::Foreground, &mut idle)?;
        publication_control::clear_beside_intent(&self.directory, &idle, &mut 0)
    }

    /// Clears the slot of a terminal attempt that has rows but no proof and says whether it did. A
    /// finished source reads the same again, so the next scan writes them. `reader` must be the
    /// source the attempt registered, ended at the end the attempt names, or nothing goes.
    pub(crate) fn abandon_terminal_attempt(
        &self,
        reader: &collector::ring::RingReader,
        budget: &Arc<ByteBudget>,
    ) -> crate::Result<bool> {
        let mut attempt = self.resume_attempt()?;
        if !matches!(attempt.input.operation, AttemptOperation::Terminal { .. }) {
            return Ok(false);
        }

        validate_terminal_source(&attempt.input, reader)?;
        let saved = attempt.saved(budget)?;
        attempt.abandon(&saved)?;
        Ok(true)
    }
}

impl RegisteredAttempt<'_> {
    pub(crate) fn saved(&mut self, budget: &Arc<ByteBudget>) -> crate::Result<Saved> {
        let mut inventory = self.original_inventory(budget)?;
        for _step in 0..INVENTORY_STEPS_MAX {
            if inventory.step()? != InventoryStep::Enumerated {
                continue;
            }
            if inventory.has_overlaps()
                || inventory.has_unordered_extents()
                || inventory.index_conflicts()
            {
                return Err(restage_refused().into());
            }
            let (batches, torn) = saved_batches(inventory.batches())?;
            let tables = TABLE_KINDS
                .iter()
                .any(|kind| inventory.file(*kind).is_some());
            return Ok(Saved {
                batches,
                torn,
                tables,
            });
        }

        Err(crate::Error::Bound("attempt inventory steps"))
    }

    /// Grants append back to an interrupted ordinary attempt. A saved row stays as it was, since
    /// reading it again may no longer find the program the collector's dictionary held when it
    /// was saved. Only a batch no row can be read from and the index staging writes again go.
    pub(crate) fn restage(&mut self, saved: &Saved) -> io::Result<()> {
        assert!(self.resumed);
        if !matches!(self.input.operation, AttemptOperation::Ordinary { .. }) {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }

        let events = self.events_directory_resumed()?;
        restage_remove(&events, "index")?;
        if let Some(ordinal) = saved.torn {
            restage_remove(&events, &batch_name(ordinal))?;
        }
        self.resumed = false;

        Ok(())
    }

    /// Removes every table. Called only with the tables' window held in memory, so the ring
    /// wrapping past that window from here on loses nothing that is removed. The required tables
    /// go first, since recovery accepts a minute with no derived tables and a process may end
    /// part way.
    pub(crate) fn restage_tables(&self) -> io::Result<()> {
        for name in TABLE_FILES {
            restage_remove(self.original_directory(), name)?;
        }

        Ok(())
    }

    /// Removes the attempt's files and directories before its slot, so a process that ends between
    /// the two finds the same attempt again rather than a directory nothing names. An ordinary
    /// attempt comes here with nothing saved, and a terminal one with what it reads again.
    fn abandon(self, saved: &Saved) -> io::Result<()> {
        let terminal = matches!(self.input.operation, AttemptOperation::Terminal { .. });
        if terminal {
            // The newest first, so a process that ends part way leaves the writer's own order.
            for ordinal in (0..saved.batches.len() + usize::from(saved.torn.is_some())).rev() {
                restage_remove(self.original_directory(), &batch_name(ordinal))?;
            }
            // A process that ended while writing its proof left it short.
            restage_remove(self.original_directory(), "metadata")?;
        } else {
            assert!(saved.nothing());
            let events = self.events_directory_resumed()?;
            restage_remove(&events, "index")?;
            if let Some(ordinal) = saved.torn {
                restage_remove(&events, &batch_name(ordinal))?;
            }
            events.full_sync()?;
            drop(events);
            self.original_directory().remove_directory("events")?;
        }
        self.original_directory().full_sync()?;
        let base = self
            .root
            .open_directory(if terminal { "recovery-data" } else { "data" })?;
        let name = collector::recording::identity_text(self.transaction).to_string();
        let object = base.open_directory(&name)?;
        object.remove_directory("pending")?;
        object.full_sync()?;
        drop(object);
        base.remove_directory(&name)?;
        base.full_sync()?;

        let mut before = [0; 4096];
        self.input
            .encode_registered(self.transaction, &mut before)?;
        let mut idle = [0; 4096];
        codec::encode_idle(
            self.input.run.recording,
            codec::SlotKind::Foreground,
            &mut idle,
        )?;
        publication_control::replace(self.root, "foreground", &before, &idle, &mut 0)
    }
}

/// The complete batches before the first that is not, and the ordinal of the torn batch after them.
/// The writer creates batches in order and a process that ends leaves a file with no footer, so
/// anything else is not its work, including a whole file whose rows do not read.
fn saved_batches(facts: &[BatchFact]) -> io::Result<(Vec<Batch>, Option<usize>)> {
    let mut saved = Vec::new();
    for fact in facts {
        let BatchFact::Complete {
            first, end, rows, ..
        } = *fact
        else {
            break;
        };
        saved.push(Batch { first, end, rows });
    }
    let after = &facts[saved.len()..];
    let has_torn = matches!(
        after.first(),
        Some(BatchFact::Malformed { framed: false, .. })
    );
    let rest = &after[usize::from(has_torn)..];
    if rest.iter().any(|fact| *fact != BatchFact::Missing) {
        return Err(restage_refused());
    }

    Ok((saved.clone(), has_torn.then_some(saved.len())))
}

fn abandon_loss_object(object: &PrivateDirectory) -> io::Result<()> {
    match object.open_directory("pending") {
        Ok(pending) => {
            restage_remove(&pending, "metadata")?;
            restage_remove(&pending, &batch_name(0))?;
            pending.full_sync()?;
            drop(pending);
            object.remove_directory("pending")?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    restage_remove(object, "original-census")?;

    object.full_sync()
}

fn restage_remove(directory: &PrivateDirectory, name: &str) -> io::Result<()> {
    match directory.remove_file(name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        result => result,
    }
}

fn restage_refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "saved event batches are not what the writer leaves",
    )
}
