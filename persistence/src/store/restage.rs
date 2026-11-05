//! An interrupted ordinary attempt, finished from what it saved and the source's retained window.

use super::attempt::TABLE_FILES;
use super::inventory::{BatchFact, InventoryStep};
use super::{RegisteredAttempt, Store, publication_control};
use crate::event_recovery::{Batch, batch_name};
use crate::resources::ByteBudget;
use crate::transaction::AttemptOperation;
use collector::recording::bootstrap::codec::Phase;
use collector::recording::private::PrivateDirectory;
use collector::ring::RunIdentity;
use std::io;
use std::sync::Arc;

/// More inventory passes than the name scan and every numbered position take.
const INVENTORY_STEPS_MAX: u32 = 1 << 16;

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

impl RegisteredAttempt<'_> {
    /// Grants append back to an interrupted ordinary attempt and returns the event batches that
    /// stay. A saved row stays as it was, since reading it again may no longer find the program
    /// the collector's dictionary held when it was saved. Only a batch no row can be read from
    /// and the index that staging writes again are removed.
    pub(crate) fn restage(&mut self, budget: &Arc<ByteBudget>) -> crate::Result<Vec<Batch>> {
        assert!(self.resumed);
        if !matches!(self.input.operation, AttemptOperation::Ordinary { .. }) {
            return Err(io::Error::from(io::ErrorKind::InvalidData).into());
        }

        let (saved, torn) = self.restage_inventory(budget)?;
        let events = self.events_directory_resumed()?;
        restage_remove(&events, "index")?;
        if let Some(ordinal) = torn {
            restage_remove(&events, &batch_name(ordinal))?;
        }
        self.resumed = false;

        Ok(saved)
    }

    fn restage_inventory(
        &mut self,
        budget: &Arc<ByteBudget>,
    ) -> crate::Result<(Vec<Batch>, Option<usize>)> {
        let mut inventory = self.original_inventory(budget)?;
        for _step in 0..INVENTORY_STEPS_MAX {
            if inventory.step()? != InventoryStep::Enumerated {
                continue;
            }
            if inventory.has_overlaps() || inventory.has_unordered_extents() {
                return Err(restage_refused().into());
            }
            return Ok(restage_saved(inventory.batches())?);
        }

        Err(io::Error::from(io::ErrorKind::InvalidData).into())
    }

    /// Removes every table. Called only with the tables' window held in memory, so the ring
    /// wrapping past that window from here on loses nothing that is removed.
    pub(crate) fn restage_tables(&self) -> io::Result<()> {
        for name in TABLE_FILES {
            restage_remove(self.original_directory(), name)?;
        }

        Ok(())
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
