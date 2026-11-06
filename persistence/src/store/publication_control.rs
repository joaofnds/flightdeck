//! Exact control replacement and typed reconciliation of deterministic slot scratch.
use crate::transaction::AttemptOperation;
use crate::transaction::normal::NormalState;
use collector::recording::bootstrap::codec::{self, NamespaceState, Phase};
use collector::recording::private::{self, PrivateDirectory};
use collector::ring::Identity;
use std::io::{self, Read, Write};

/// Restore the validated visible controls before a reopened owner performs any effect.
pub(super) fn adopt(
    root: &impl ControlDirectory,
    state: &NormalState,
    work: &mut super::PublicationWork,
) -> io::Result<()> {
    work.control_reads += 1;
    work.canonical.namespace_attempts += 1;
    visible(root, state)?;
    work.canonical.namespace_attempts += 1;
    sync_file(root, "foreground", &mut work.full_syncs)?;
    sync_directory(root, &mut work.full_syncs)?;
    work.control_reads += 1;
    work.canonical.namespace_attempts += 1;
    read_state(root, state.input.run.recording)?;
    work.canonical.namespace_attempts += 1;
    sync_file(root, "namespace-state", &mut work.full_syncs)?;
    sync_directory(root, &mut work.full_syncs)
}
pub(super) fn read_state(
    root: &impl ControlDirectory,
    recording: Identity,
) -> io::Result<NamespaceState> {
    codec::decode_state(&read(root, "namespace-state", 256)?, recording)
}
pub(super) fn read_optional_normal(
    root: &impl ControlDirectory,
    recording: Identity,
    work: &mut super::PublicationWork,
) -> io::Result<Option<NormalState>> {
    let visible = read(root, "foreground", 4096)?;
    if matches!(
        codec::decode_slot(&visible, recording, codec::SlotKind::Foreground)?,
        codec::Slot::Idle
    ) {
        sync_file(root, "foreground", &mut work.full_syncs)?;
        sync_directory(root, &mut work.full_syncs)?;
        Ok(None)
    } else {
        read_normal(root, recording).map(Some)
    }
}
/// A complete fresh finite registration scratch keeps its endpoint even before rename.
pub(super) fn pending_finite(
    root: &impl ControlDirectory,
    recording: Identity,
) -> io::Result<Option<NormalState>> {
    let Some(bytes) = optional_read(root, "foreground.pending", 4096)? else {
        return Ok(None);
    };
    if bytes.len() != 4096 {
        return Ok(None);
    }
    if matches!(
        codec::decode_slot(&bytes, recording, codec::SlotKind::Foreground)?,
        codec::Slot::Idle
    ) {
        return Ok(None);
    }
    let state = NormalState::decode(&bytes, recording)?;
    if !state.input.is_finite()
        || state != NormalState::registered(state.transaction, state.input.clone())
    {
        return Err(invalid());
    }
    Ok(Some(state))
}
pub(super) fn read_normal(
    root: &impl ControlDirectory,
    recording: Identity,
) -> io::Result<NormalState> {
    let visible = read(root, "foreground", 4096)?;
    let current = NormalState::decode(&visible, recording)?;
    match optional_read(root, "foreground.pending", 4096) {
        Ok(Some(candidate)) => {
            if candidate.len() < 4096 {
                return Ok(current);
            }

            if matches!(
                codec::decode_slot(&candidate, recording, codec::SlotKind::Foreground)?,
                codec::Slot::Idle
            ) && clears_to_idle(&current)
            {
                if current.census.kind == 2 && current.cleanup.region != 8 {
                    return Err(invalid());
                }
                return Ok(current);
            }

            if candidate == visible {
                let mut syncs = 0;
                let result = replace(root, "foreground", &visible, &visible, &mut syncs);
                root.record_reconcile_syncs(syncs);
                result?;
                return Ok(current);
            }

            let next = NormalState::decode(&candidate, recording)?;
            if !legal_next(&current, &next) {
                return Err(invalid());
            }

            if current.phase == Phase::Registered
                && current.census.kind == 0
                && next.census.kind == 1
            {
                let mut syncs = 0;
                let result = replace(root, "foreground", &visible, &candidate, &mut syncs);
                root.record_reconcile_syncs(syncs);
                result?;
                return Ok(next);
            }
            // Recompute this exact successor from admitted raw/effect evidence before
            // adopting it. A complete scratch file is still intent, not effect proof.
            Ok(current)
        }
        Ok(None) => Ok(current),
        // A short write is not authority. Retain it until a fully validated owner recomputes
        // the exact next target, so arbitrary bytes cannot authorize a transition.
        Err(error) => Err(error),
    }
}
/// Whether a whole Idle state may wait beside this one: the end of cleanup, the end of an ordinary
/// or terminal attempt given up before any proof, which has chosen no census, or the end of a
/// terminal given up because no source remains to prove what it lost.
pub(super) fn clears_to_idle(state: &NormalState) -> bool {
    match state.phase {
        Phase::Cleanup | Phase::SalvageCommitted => true,
        Phase::Registered => match state.census.kind {
            0 => !state.input.is_finite(),
            1 => matches!(state.input.operation, AttemptOperation::Terminal { .. }),
            _ => false,
        },
        _ => false,
    }
}
/// Clears a slot whose attempt is given up with its census begun. A next state waiting beside it,
/// whole or short, is intent only that attempt's proof could have adopted.
pub(super) fn clear_beside_intent(
    root: &impl ControlDirectory,
    idle: &[u8],
    syncs: &mut u64,
) -> io::Result<()> {
    let old = read(root, "foreground", idle.len())?;
    if optional_read(root, "foreground.pending", idle.len())?.is_some_and(|bytes| bytes != idle) {
        root.remove_file("foreground.pending")?;
        sync_directory(root, syncs)?;
    }

    replace(root, "foreground", &old, idle, syncs)
}
fn legal_next(old: &NormalState, new: &NormalState) -> bool {
    if old.transaction != new.transaction || old.input != new.input {
        return false;
    }

    let phases = (old.phase, new.phase);
    if phases == (Phase::Registered, Phase::Registered) {
        let mut expected = old.clone();
        expected.census = new.census;
        return *new == expected
            && ((old.census.kind == 0 && new.census.kind == 1) || old.census == new.census);
    }
    if !census_next(old, new) {
        return false;
    }
    if phases == (Phase::Registered, Phase::SalvagePlanned) {
        return old.salvage.is_none()
            && (old.row_plan.is_none() || (old.input.is_finite() && old.row_plan == new.row_plan))
            && (new.salvage.is_some() || new.row_plan.is_some());
    }
    if let Some(legal) = legal_next_finite(old, new) {
        return legal;
    }
    if old.row_plan != new.row_plan && !legal_next_rows(old, new) {
        return false;
    }
    if old.salvage != new.salvage {
        return false;
    }
    if !legal_next_cleanup(old, new) {
        return false;
    }
    if phases == (Phase::Registered, Phase::Ready) {
        return true;
    }

    if phases == (Phase::SalvagePlanned, Phase::SalvagePlanned) && old.row_plan.is_some() {
        return true;
    }
    if old.original_end != new.original_end || old.origin != new.origin || old.loss != new.loss {
        return false;
    }

    legal_next_phase(old, new)
}
fn legal_next_finite(old: &NormalState, new: &NormalState) -> Option<bool> {
    let phases = (old.phase, new.phase);
    if old.input.is_finite()
        && matches!(
            phases,
            (Phase::SalvagePlanned, Phase::SalvagePlanned)
                | (Phase::UnresolvedExtent, Phase::UnresolvedExtent)
        )
        && old.row_plan.is_some()
        && new.salvage.is_some()
    {
        return Some(
            new.row_plan.is_none()
                && old.row_plan.is_some_and(|plan| {
                    plan.rows == [0, 0]
                        && plan.split == 0
                        && plan.committed == 0
                        && new.salvage.is_some_and(|loss| {
                            loss.first == plan.first
                                && loss.end == plan.end
                                && loss.snapshot_first == plan.source_first
                                && loss.snapshot_end == plan.source_end
                                && loss.layout == plan.layout
                                && loss.endpoint == plan.endpoint
                                && !loss.terminal
                        })
                }),
        );
    }
    if old.input.is_finite()
        && old.census.kind == 2
        && new.census == old.census
        && old.sequence == 0
        && new.next == 0
        && new.structure.is_none()
    {
        if matches!(
            phases,
            (
                Phase::SalvagePlanned | Phase::UnresolvedExtent,
                Phase::UnresolvedExtent
            )
        ) {
            return Some(new.sequence == 0 && legal_next_rows(old, new));
        }
        if phases == (Phase::UnresolvedExtent, Phase::Publishing) {
            return Some(
                new.sequence > 0
                    && old.row_plan == new.row_plan
                    && old.salvage == new.salvage
                    && old.loss == new.loss,
            );
        }
    }
    None
}
fn legal_next_phase(old: &NormalState, new: &NormalState) -> bool {
    match (old.phase, new.phase) {
        (Phase::Ready | Phase::SalvagePlanned, Phase::Publishing) => {
            new.sequence > 0 && new.structure.is_none() && new.next == 0
        }
        (Phase::Publishing, Phase::Publishing) => {
            old.sequence == new.sequence
                && match (&old.structure, &new.structure) {
                    (None, Some(plan)) => plan.stage == 1 && plan.serial == new.next + 1,
                    (Some(before), Some(after)) => {
                        let mut expected = before.clone();
                        expected.stage = after.stage;
                        expected.cleanup = after.cleanup;
                        expected.children = after.children;
                        expected == *after
                            && after.stage >= before.stage
                            && after.stage <= before.stage + 1
                            && old.next == new.next
                    }
                    (Some(plan), None) => plan.stage >= 4 && old.next == new.next,
                    (None, None) => old.next == new.next,
                }
        }
        (Phase::Publishing, Phase::Published) => {
            old.sequence == new.sequence && old.structure.is_none() && new.structure.is_none()
        }
        (Phase::Published, Phase::Cleanup | Phase::SalvageCommitted)
        | (Phase::SalvageCommitted, Phase::SalvageCommitted) => {
            old.sequence == new.sequence && old.next == new.next
        }
        _ => false,
    }
}
pub(super) fn census_next(old: &NormalState, new: &NormalState) -> bool {
    if old.transaction != new.transaction || old.input != new.input {
        return false;
    }
    old.census == new.census
        || match (old.phase, new.phase, old.census.kind, new.census.kind) {
            (Phase::Registered, Phase::Registered, 0, 1) => true,
            (Phase::Registered, Phase::SalvagePlanned, 1, 2) => {
                old.census.digest == new.census.digest
            }
            _ => false,
        }
}
fn legal_next_cleanup(old: &NormalState, new: &NormalState) -> bool {
    let phases = (old.phase, new.phase);
    old.cleanup == new.cleanup
        || (phases == (Phase::SalvageCommitted, Phase::SalvageCommitted)
            && (
                old.cleanup.region,
                old.cleanup.ordinal,
                old.cleanup.table,
                old.cleanup.directory,
            ) < (
                new.cleanup.region,
                new.cleanup.ordinal,
                new.cleanup.table,
                new.cleanup.directory,
            ))
}
fn legal_next_rows(old: &NormalState, new: &NormalState) -> bool {
    let (Some(before), Some(after)) = (old.row_plan, new.row_plan) else {
        return false;
    };
    let mut expected = before;
    expected.split = after.split;
    expected.rows = after.rows;
    expected.committed = after.committed;
    expected == after
        && (before.split == after.split || before.split == 0)
        && (before.rows == after.rows || before.rows == [0, 0])
        && before.committed & after.committed == before.committed
}
pub(super) fn visible(root: &impl ControlDirectory, state: &NormalState) -> io::Result<()> {
    let bytes = read(root, "foreground", 4096)?;
    if matches!(
        codec::decode_slot(
            &bytes,
            state.input.run.recording,
            codec::SlotKind::Foreground
        )?,
        codec::Slot::Idle
    ) && matches!(state.phase, Phase::Cleanup | Phase::SalvageCommitted)
    {
        if state.census.kind == 2 && state.cleanup.region != 8 {
            return Err(invalid());
        }
        return Ok(());
    }

    let current = NormalState::decode(&bytes, state.input.run.recording)?;
    if current == *state || legal_next(state, &current) {
        Ok(())
    } else {
        Err(invalid())
    }
}
pub(super) fn pending(
    root: &impl ControlDirectory,
    state: &NormalState,
) -> io::Result<Option<NormalState>> {
    let Some(bytes) = optional_read(root, "foreground.pending", 4096)? else {
        return Ok(None);
    };
    if bytes.len() < 4096 {
        return Ok(None);
    }

    if matches!(
        codec::decode_slot(
            &bytes,
            state.input.run.recording,
            codec::SlotKind::Foreground
        )?,
        codec::Slot::Idle
    ) && clears_to_idle(state)
    {
        return Ok(None);
    }

    let candidate = NormalState::decode(&bytes, state.input.run.recording)?;
    if candidate != *state && !legal_next(state, &candidate) {
        return Err(invalid());
    }

    Ok(Some(candidate))
}
pub(super) fn check(
    root: &impl ControlDirectory,
    name: &str,
    old: &[u8],
    new: &[u8],
) -> io::Result<()> {
    let current = read(root, name, old.len())?;
    if current != old && current != new {
        return Err(invalid());
    }

    if let Some(pending) = optional_read(root, &format!("{name}.pending"), old.len())? {
        if pending != old && pending != new && !new.starts_with(&pending) {
            return Err(invalid());
        }
    }
    Ok(())
}
pub(super) fn replace(
    root: &impl ControlDirectory,
    name: &str,
    old: &[u8],
    new: &[u8],
    syncs: &mut u64,
) -> io::Result<()> {
    if old.len() != new.len() || !matches!(old.len(), 256 | 4096) {
        return Err(invalid());
    }

    let current = read(root, name, old.len())?;
    if current != old && current != new {
        return Err(invalid());
    }

    let pending_name = format!("{name}.pending");
    let pending = optional_read(root, &pending_name, old.len())?;
    if current == new {
        sync_file(root, name, syncs)?;
        sync_directory(root, syncs)?;
        if let Some(bytes) = pending {
            if bytes != new && bytes != old {
                return Err(invalid());
            }

            root.remove_file(&pending_name)?;
            sync_directory(root, syncs)?;
        }
        return Ok(());
    }

    if let Some(bytes) = pending {
        if bytes == new {
            sync_file(root, &pending_name, syncs)?;
            root.rename(&pending_name, name)?;
            return sync_directory(root, syncs);
        }

        // Only a prefix of this recomputed legal target, or an exact old duplicate,
        // is disposable scratch. The intact old control is restored first.
        if bytes != old && !new.starts_with(&bytes) {
            return Err(invalid());
        }

        sync_file(root, name, syncs)?;
        sync_directory(root, syncs)?;
        root.remove_file(&pending_name)?;
        sync_directory(root, syncs)?;
    }
    let mut file = root.create_file(&pending_name)?;
    root.record_write(u64::try_from(new.len()).map_err(|_| invalid())?);
    file.write_all(new)?;
    *syncs += 1;
    root.record_file_sync();
    private::full_sync(&file)?;
    drop(file);
    root.rename(&pending_name, name)?;
    sync_directory(root, syncs)
}
pub(super) fn read(root: &impl ControlDirectory, name: &str, size: usize) -> io::Result<Vec<u8>> {
    let bytes =
        optional_read(root, name, size)?.ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
    if bytes.len() != size {
        return Err(invalid());
    }

    Ok(bytes)
}
pub(super) fn optional_read(
    root: &impl ControlDirectory,
    name: &str,
    maximum: usize,
) -> io::Result<Option<Vec<u8>>> {
    let mut file = match root.open_file(name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::with_capacity(maximum + 1);
    root.record_read();
    let result = (&mut file)
        .take(u64::try_from(maximum + 1).map_err(|_| invalid())?)
        .read_to_end(&mut bytes);
    root.record_read_bytes(u64::try_from(bytes.len()).map_err(|_| invalid())?);
    result?;
    if bytes.len() > maximum {
        return Err(invalid());
    }

    Ok(Some(bytes))
}
fn sync_file(root: &impl ControlDirectory, name: &str, syncs: &mut u64) -> io::Result<()> {
    *syncs += 1;
    let file = root.open_file(name)?;
    root.record_file_sync();
    private::full_sync(&file)
}
fn sync_directory(root: &impl ControlDirectory, syncs: &mut u64) -> io::Result<()> {
    *syncs += 1;
    root.full_sync()
}
fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}

pub(super) trait ControlDirectory {
    fn open_file(&self, name: &str) -> io::Result<std::fs::File>;
    fn create_file(&self, name: &str) -> io::Result<std::fs::File>;
    fn remove_file(&self, name: &str) -> io::Result<()>;
    fn rename(&self, old: &str, new: &str) -> io::Result<()>;
    fn full_sync(&self) -> io::Result<()>;
    fn record_read(&self) {}
    fn record_read_bytes(&self, _bytes: u64) {}
    fn record_write(&self, _bytes: u64) {}
    fn record_file_sync(&self) {}
    fn record_reconcile_syncs(&self, _syncs: u64) {}
}
impl ControlDirectory for PrivateDirectory {
    fn open_file(&self, name: &str) -> io::Result<std::fs::File> {
        self.open_file(name)
    }
    fn create_file(&self, name: &str) -> io::Result<std::fs::File> {
        self.create_file(name)
    }
    fn remove_file(&self, name: &str) -> io::Result<()> {
        self.remove_file(name)
    }
    fn rename(&self, old: &str, new: &str) -> io::Result<()> {
        self.rename(old, new)
    }
    fn full_sync(&self) -> io::Result<()> {
        self.full_sync()
    }
}
pub(super) struct CountedControl<'a> {
    pub root: &'a PrivateDirectory,
    pub ledger: Option<&'a super::PublicationLedger>,
}
impl CountedControl<'_> {
    fn update(&self, update: impl FnOnce(&mut super::PublicationWork)) {
        if let Some(ledger) = self.ledger {
            ledger.update(update);
        }
    }
}
impl ControlDirectory for CountedControl<'_> {
    fn open_file(&self, name: &str) -> io::Result<std::fs::File> {
        self.update(|work| work.control_file_opens += 1);
        self.root.open_file(name)
    }
    fn create_file(&self, name: &str) -> io::Result<std::fs::File> {
        self.update(|work| work.control_file_creates += 1);
        self.root.create_file(name)
    }
    fn remove_file(&self, name: &str) -> io::Result<()> {
        self.update(|work| work.control_remove_attempts += 1);
        self.root.remove_file(name)
    }
    fn rename(&self, old: &str, new: &str) -> io::Result<()> {
        self.update(|work| work.control_rename_attempts += 1);
        self.root.rename(old, new)
    }
    fn full_sync(&self) -> io::Result<()> {
        self.update(|work| work.control_directory_sync_attempts += 1);
        self.root.full_sync()
    }
    fn record_read(&self) {
        self.update(|work| work.control_read_operations += 1);
    }
    fn record_read_bytes(&self, bytes: u64) {
        self.update(|work| work.control_read_bytes += bytes);
    }
    fn record_write(&self, bytes: u64) {
        self.update(|work| {
            work.control_write_operations += 1;
            work.control_write_requested_bytes += bytes;
        });
    }
    fn record_reconcile_syncs(&self, syncs: u64) {
        self.update(|work| work.full_syncs += syncs);
    }
    fn record_file_sync(&self) {
        self.update(|work| work.control_file_sync_attempts += 1);
    }
}
