//! An interrupted ordinary attempt, finished from what it saved and the source's retained window.

use super::attempt::TABLE_FILES;
use super::inventory::{BatchFact, InventoryStep, OriginalFile};
use super::{RegisteredAttempt, Store, publication_control};
use crate::event_recovery::{Batch, batch_name};
use crate::resources::ByteBudget;
use crate::transaction::AttemptOperation;
use collector::recording::bootstrap::codec::{self, Phase};
use collector::recording::private::PrivateDirectory;
use collector::ring::RunIdentity;
use std::io;
use std::sync::Arc;

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

/// What a process left in the foreground slot, by what can finish it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Interrupted {
    /// No proof is durable, so finishing it takes the run's source.
    Staging(RunIdentity),
    /// The proof is durable, so it publishes without the source.
    Publication,
}

impl Store {
    /// Names what the foreground slot holds, or nothing when it is idle. Routes the worker does
    /// not start yet refuse here, so nothing they left is taken for an ordinary attempt.
    pub(crate) fn interrupted(&self) -> io::Result<Option<Interrupted>> {
        let mut work = super::PublicationWork::default();
        let Some(state) =
            publication_control::read_optional_normal(&self.directory, self.recording, &mut work)?
        else {
            return Ok(None);
        };
        if state.census.kind != 0
            || state.row_plan.is_some()
            || state.salvage.is_some()
            || state.input.is_finite()
            || !matches!(state.input.operation, AttemptOperation::Ordinary { .. })
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "interrupted attempt has no worker route",
            ));
        }
        if state.phase != Phase::Registered {
            return Ok(Some(Interrupted::Publication));
        }

        match self.resume_attempt()?.open_file("metadata") {
            Ok(_) => Ok(Some(Interrupted::Publication)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Ok(Some(Interrupted::Staging(state.input.run)))
            }
            Err(error) => Err(error),
        }
    }
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
}

impl RegisteredAttempt<'_> {
    pub(crate) fn saved(&mut self, budget: &Arc<ByteBudget>) -> crate::Result<Saved> {
        let mut inventory = self.original_inventory(budget)?;
        for _step in 0..INVENTORY_STEPS_MAX {
            if inventory.step()? != InventoryStep::Enumerated {
                continue;
            }
            if inventory.has_overlaps() || inventory.has_unordered_extents() {
                return Err(restage_refused().into());
            }
            let (batches, torn) = restage_saved(inventory.batches())?;
            let tables = TABLE_KINDS
                .iter()
                .any(|kind| inventory.file(*kind).is_some());
            return Ok(Saved {
                batches,
                torn,
                tables,
            });
        }

        Err(io::Error::from(io::ErrorKind::InvalidData).into())
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
    /// wrapping past that window from here on loses nothing that is removed.
    pub(crate) fn restage_tables(&self) -> io::Result<()> {
        for name in TABLE_FILES {
            restage_remove(self.original_directory(), name)?;
        }

        Ok(())
    }

    /// Removes the attempt's empty directories before its slot, so a process that ends between
    /// the two finds the same empty attempt again rather than a directory nothing names.
    fn abandon(self, saved: &Saved) -> io::Result<()> {
        assert!(saved.nothing());
        let events = self.events_directory_resumed()?;
        restage_remove(&events, "index")?;
        if let Some(ordinal) = saved.torn {
            restage_remove(&events, &batch_name(ordinal))?;
        }
        events.full_sync()?;
        drop(events);
        self.original_directory().remove_directory("events")?;
        self.original_directory().full_sync()?;
        let base = self.root.open_directory("data")?;
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

/// The complete batches before the first that is not, and the torn batch after them. The writer
/// creates batches in order, so it can leave nothing else, and anything else is not its work.
fn restage_saved(facts: &[BatchFact]) -> io::Result<(Vec<Batch>, Option<usize>)> {
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
    let torn = matches!(after.first(), Some(BatchFact::Malformed { .. }));
    let rest = after.get(usize::from(torn)..).unwrap_or_default();
    if rest.iter().any(|fact| *fact != BatchFact::Missing) {
        return Err(restage_refused());
    }

    let torn = torn.then_some(saved.len());

    Ok((saved, torn))
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
        "saved event batches are not the writer's own order",
    )
}
