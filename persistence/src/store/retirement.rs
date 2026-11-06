//! Physical removal of what has outlived the retention period, oldest first and bounded per pass.
//!
//! A removal unlinks a proof's names under the namespace's Busy, so a reader retries rather than
//! meeting a vanished name as damage. It then removes the raw data, and last the order entry, whose
//! hard link keeps the proof a repeat after a crash starts from. Every step takes a missing target
//! as done. A proof or table that does not read is reported and removal goes on with what it can
//! name, so damage cannot stop expiry.

use super::Store;
use super::publication_control::{CountedControl, read_optional_normal, read_state, replace};
use crate::canonical::catalog::window_at_or_after;
use crate::canonical::metadata::{Body, CanonicalMetadata};
use crate::canonical::removal;
use crate::canonical::rows;
use crate::canonical::tree::{CanonicalKey, Namespace, ReadWork};
use crate::metadata::TimeBounds;
use crate::resources::ByteBudget;
use crate::retention::{self, Clock, ClockReading, Cutoff, ListedEntry, Period};
use crate::transaction::Predecessor;
use crate::transaction::normal::NormalState;
use collector::recording::bootstrap::codec::{self, NamespaceState, RETIREMENT_TRANSACTION};
use collector::recording::private::PrivateDirectory;
use collector::ring::{Identity, RunIdentity};
use std::io;
use std::sync::Arc;

/// How many publications one pass removes.
const RETIRED_PER_PASS: u64 = 16;
/// How many expired entries one pass lists, which leaves room to pass over those that must wait.
const ENTRIES_PER_PASS: usize = 256;
const RAW_NAMES_MAX: usize = 64;
const RAW_EVENT_NAMES_MAX: usize = 8_192;
const RAW_BASES: [&str; 2] = ["data", "recovery-data"];

impl Store {
    /// One pass of physical removal at `now`. `runs` are the registered runs, whose newest
    /// publication the next save reads. Returns how many publications it removed.
    pub fn retire_expired(
        &self,
        now: &ClockReading,
        runs: &[RunIdentity],
        budget: &Arc<ByteBudget>,
    ) -> io::Result<u64> {
        let clock = Clock::read_for_removal(&self.directory, now)?;
        let period = Period::read(&self.directory);
        self.retire_expired_room(&clock, period)?;
        if let Err(error) = retention::boot_records_settle(&self.directory, &clock) {
            collector::diagnostics::error(format_args!("retention boot records: {error}"));
        }
        self.retire_expired_settle()?;

        self.retire_expired_entries(&clock, period, runs, budget)
    }

    /// Saves the clock, which loses nothing if a full disk refuses it. What that refuses, the state
    /// a removal writes meets too, so a disk with something to remove gives up the room set aside
    /// for this. With nothing to remove the room stays, since a save would take it and leave a
    /// later removal with none. A disk that takes the clock has room to set aside again.
    fn retire_expired_room(&self, clock: &Clock, period: Period) -> io::Result<()> {
        if let Err(error) = clock.write(&self.directory) {
            collector::diagnostics::error(format_args!("retention clock save: {error}"));
            if retention::expired_entries(&self.directory, clock.removal_cutoff(period), 1)?
                .is_empty()
            {
                return Ok(());
            }

            return retention::reserve_release(&self.directory);
        }
        if let Err(error) = retention::reserve_keep(&self.directory) {
            if error.kind() != io::ErrorKind::StorageFull {
                collector::diagnostics::error(format_args!("retention reserve: {error}"));
            }
        }

        Ok(())
    }

    fn retire_expired_entries(
        &self,
        clock: &Clock,
        period: Period,
        runs: &[RunIdentity],
        budget: &Arc<ByteBudget>,
    ) -> io::Result<u64> {
        let mut work = super::PublicationWork::default();
        let under_way = read_optional_normal(&self.retire_control(), self.recording, &mut work)?;
        // Once a publication has taken the namespace it expects the sequence it left there. Before
        // that it reads the namespace when it takes it.
        if under_way.as_ref().is_some_and(|state| state.sequence > 0) {
            return Ok(0);
        }

        let cutoff = clock.removal_cutoff(period);
        let entries = retention::expired_entries(&self.directory, cutoff, ENTRIES_PER_PASS)?;
        if entries.is_empty() {
            return Ok(0);
        }

        let held = self.retire_expired_held(under_way.as_ref(), runs, budget)?;
        let mut retired = 0;
        for entry in &entries {
            if retired == RETIRED_PER_PASS {
                break;
            }
            if self.retire_one(entry, &held, cutoff, budget)? {
                retired += 1;
            }
        }

        Ok(retired)
    }

    /// Ends the Busy a crash left, since any set of removed names is a window that a reader treats
    /// as expired, and the entry repeats the rest.
    fn retire_expired_settle(&self) -> io::Result<()> {
        let state = read_state(&self.retire_control(), self.recording)?;
        if let NamespaceState::Busy {
            sequence,
            transaction,
        } = state
        {
            if transaction == RETIREMENT_TRANSACTION {
                self.retire_one_end(sequence)?;
            }
        }

        Ok(())
    }

    /// The transactions a pass must leave: the newest publication of each registered run and of
    /// the run of a publication under way, which the next save reads as its predecessor.
    fn retire_expired_held(
        &self,
        under_way: Option<&NormalState>,
        runs: &[RunIdentity],
        budget: &Arc<ByteBudget>,
    ) -> io::Result<Vec<Identity>> {
        let mut held = Vec::with_capacity(runs.len() + 1);
        if let Some(state) = under_way {
            held.extend(self.retire_expired_newest(&state.input.run, budget)?);
        }
        for run in runs {
            held.extend(self.retire_expired_newest(run, budget)?);
        }

        Ok(held)
    }

    fn retire_expired_newest(
        &self,
        run: &RunIdentity,
        budget: &Arc<ByteBudget>,
    ) -> io::Result<Option<Identity>> {
        let frontier = self.source_frontier(run, budget, &mut ReadWork::default())?;
        let predecessor = frontier
            .as_ref()
            .map(crate::canonical::SourceFrontier::predecessor);
        let Some(Predecessor::Canonical { transaction, .. }) = predecessor else {
            return Ok(None);
        };

        Ok(Some(*transaction))
    }

    /// Removes one publication unless it must wait, and says whether it did.
    fn retire_one(
        &self,
        entry: &ListedEntry,
        held: &[Identity],
        cutoff: Cutoff,
        budget: &Arc<ByteBudget>,
    ) -> io::Result<bool> {
        let metadata = entry
            .open_proof(&self.directory)
            .and_then(|mut proof| CanonicalMetadata::read(&mut proof));
        let metadata = match metadata {
            Ok(metadata) => metadata,
            Err(error) => {
                collector::diagnostics::error(format_args!(
                    "retention removal of an unreadable proof: {error}"
                ));
                self.retire_one_unreadable(entry)?;
                return Ok(true);
            }
        };
        if held.contains(&metadata.transaction)
            || self.retire_one_waits(&metadata, cutoff, budget)?
        {
            return Ok(false);
        }

        let names = self.retire_one_names(&metadata, budget)?;
        let sequence = self.retire_one_begin()?;
        let unlinked = self.retire_one_unlink(&names);
        self.retire_one_end(sequence)?;
        unlinked?;

        retire_one_raw(
            &self.directory.open_directory(metadata.base.name())?,
            &collector::recording::identity_text(metadata.transaction).to_string(),
            metadata.child.name(),
        )?;
        entry.remove(&self.directory)?;

        Ok(true)
    }

    /// Whether a window must stay for the reading after it: that reading's rates need this window
    /// until it has expired too, and past that by the grace a read under way has. A window with no
    /// later window waits for nothing.
    fn retire_one_waits(
        &self,
        metadata: &CanonicalMetadata,
        cutoff: Cutoff,
        budget: &Arc<ByteBudget>,
    ) -> io::Result<bool> {
        let Body::Ordinary(manifest) = &metadata.body else {
            return Ok(false);
        };
        let successor = window_at_or_after(
            &self.directory,
            self.recording,
            &metadata.run,
            manifest.publication_end,
            budget,
        );
        match successor {
            Ok(None) => Ok(false),
            Ok(Some(successor)) => {
                let boot = successor.identity.run.boot;
                let offset_ns = retention::boot_offset_ns(&self.directory, boot)?;

                Ok(retire_one_waits_for(
                    successor.continuous_ns,
                    offset_ns,
                    cutoff,
                ))
            }
            Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                collector::diagnostics::error(format_args!(
                    "retention removal past an unreadable window: {error}"
                ));
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    /// Every name the proof holds in the trees. A window's readings are named from its ticks, and
    /// after a crash past its name removal those are gone too, which leaves nothing to find.
    fn retire_one_names(
        &self,
        metadata: &CanonicalMetadata,
        budget: &Arc<ByteBudget>,
    ) -> io::Result<Vec<(Namespace, CanonicalKey)>> {
        let mut names = Vec::with_capacity(62);
        if matches!(metadata.body, Body::Ordinary(_)) {
            let readings = self
                .directory
                .open_directory(metadata.base.name())
                .and_then(|base| {
                    base.open_directory(
                        &collector::recording::identity_text(metadata.transaction).to_string(),
                    )
                })
                .and_then(|object| object.open_directory(metadata.child.name()))
                .and_then(|raw| {
                    rows::observation_keys(&raw, metadata, budget, &mut ReadWork::default(), None)
                });
            match readings {
                Ok(readings) => names = readings,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => collector::diagnostics::error(format_args!(
                    "retention removal of a window whose ticks do not read: {error}"
                )),
            }
        }
        names.extend(metadata.source_names()?);

        Ok(names)
    }

    /// Holds the namespace Busy. The settled pass is this recording's only writer, so it is Quiet.
    fn retire_one_begin(&self) -> io::Result<u64> {
        let control = self.retire_control();
        let NamespaceState::Quiet { sequence } = read_state(&control, self.recording)? else {
            return Err(invalid());
        };
        let busy = sequence.checked_add(1).ok_or_else(invalid)?;
        self.retire_replace(
            &NamespaceState::Quiet { sequence },
            &NamespaceState::Busy {
                sequence: busy,
                transaction: RETIREMENT_TRANSACTION,
            },
        )?;

        Ok(busy)
    }

    fn retire_one_unlink(&self, names: &[(Namespace, CanonicalKey)]) -> io::Result<()> {
        let mut work = ReadWork::default();
        for (namespace, key) in names {
            removal::remove(&self.directory, *namespace, key, &mut work)?;
        }

        Ok(())
    }

    fn retire_one_end(&self, sequence: u64) -> io::Result<()> {
        let quiet = sequence.checked_add(1).ok_or_else(invalid)?;
        self.retire_replace(
            &NamespaceState::Busy {
                sequence,
                transaction: RETIREMENT_TRANSACTION,
            },
            &NamespaceState::Quiet { sequence: quiet },
        )
    }

    fn retire_replace(&self, old: &NamespaceState, new: &NamespaceState) -> io::Result<()> {
        let mut old_bytes = [0; 256];
        let mut new_bytes = [0; 256];
        codec::encode_state(self.recording, old, &mut old_bytes)?;
        codec::encode_state(self.recording, new, &mut new_bytes)?;

        replace(
            &self.retire_control(),
            "namespace-state",
            &old_bytes,
            &new_bytes,
            &mut 0,
        )
    }

    /// Removes what the entry's name can say of a proof that does not read. Its names stay as links
    /// to that proof, which a reader meets as it met them before.
    fn retire_one_unreadable(&self, entry: &ListedEntry) -> io::Result<()> {
        if let Some((transaction, child)) = entry.raw_names() {
            for base in RAW_BASES {
                retire_one_raw(&self.directory.open_directory(base)?, transaction, child)?;
            }
        }

        entry.remove(&self.directory)
    }

    fn retire_control(&self) -> CountedControl<'_> {
        CountedControl {
            root: &self.directory,
            ledger: None,
        }
    }
}

/// Whether a window must wait for a successor with these clock bounds, whose boot has this offset
/// on the retention clock: its first reading still has a place inside the period. One with no place
/// on the clock has to go first, by its own entry, for this window to go.
pub(super) fn retire_one_waits_for(
    bounds: Option<TimeBounds>,
    offset_ns: Option<u64>,
    cutoff: Cutoff,
) -> bool {
    let (Some(bounds), Some(offset_ns)) = (bounds, offset_ns) else {
        return true;
    };

    !cutoff.expired(bounds.min.saturating_add(offset_ns))
}

/// Removes one proof's raw directory, the events inside it first, and its transaction's directory
/// once no sibling proof is left in it.
fn retire_one_raw(base: &PrivateDirectory, transaction: &str, child: &str) -> io::Result<()> {
    let object = match base.open_directory(transaction) {
        Ok(object) => object,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    retire_one_raw_child(&object, child)?;
    object.full_sync()?;
    drop(object);
    match base.remove_directory(transaction) {
        Ok(()) => {}
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::NotFound
            ) =>
        {
            return Ok(());
        }
        Err(error) => return Err(error),
    }

    base.full_sync()
}

fn retire_one_raw_child(object: &PrivateDirectory, child: &str) -> io::Result<()> {
    let raw = match object.open_directory(child) {
        Ok(raw) => raw,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    let mut events = Vec::new();
    match raw.open_directory("events") {
        Ok(directory) => {
            directory.visit_names(RAW_EVENT_NAMES_MAX, |name| {
                events.push(name.to_owned());
                Ok(())
            })?;
            for name in &events {
                directory.remove_file(name)?;
            }
            raw.remove_directory("events")?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let mut names = Vec::with_capacity(RAW_NAMES_MAX);
    raw.visit_names(RAW_NAMES_MAX, |name| {
        names.push(name.to_owned());
        Ok(())
    })?;
    for name in &names {
        raw.remove_file(name)?;
    }
    drop(raw);

    object.remove_directory(child)
}

fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}
