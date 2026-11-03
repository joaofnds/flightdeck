//! Exact control replacement and typed reconciliation of deterministic slot scratch.
use crate::transaction::normal::NormalState;
use collector::recording::bootstrap::codec::{self, NamespaceState, Phase};
use collector::recording::private::{self, PrivateDirectory};
use collector::ring::Identity;
use std::io::{self, Read, Write};

pub(super) fn read_state(
    root: &PrivateDirectory,
    recording: Identity,
) -> io::Result<NamespaceState> {
    codec::decode_state(&read(root, "namespace-state", 256)?, recording)
}
pub(super) fn read_normal(root: &PrivateDirectory, recording: Identity) -> io::Result<NormalState> {
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
            ) && current.phase == Phase::Cleanup
            {
                return Ok(current);
            }

            if candidate == visible {
                replace(root, "foreground", &visible, &visible, &mut 0)?;
                return Ok(current);
            }

            let next = NormalState::decode(&candidate, recording)?;
            if !legal_next(&current, &next) {
                return Err(invalid());
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
fn legal_next(old: &NormalState, new: &NormalState) -> bool {
    if old.transaction != new.transaction || old.input != new.input {
        return false;
    }

    let phases = (old.phase, new.phase);
    if phases == (Phase::Registered, Phase::Ready) {
        return true;
    }

    if old.original_end != new.original_end || old.origin != new.origin || old.loss != new.loss {
        return false;
    }

    match phases {
        (Phase::Ready, Phase::Publishing) => {
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
        (Phase::Published, Phase::Cleanup) => old.sequence == new.sequence && old.next == new.next,
        _ => false,
    }
}
pub(super) fn visible(root: &PrivateDirectory, state: &NormalState) -> io::Result<()> {
    let bytes = read(root, "foreground", 4096)?;
    if matches!(
        codec::decode_slot(
            &bytes,
            state.input.run.recording,
            codec::SlotKind::Foreground
        )?,
        codec::Slot::Idle
    ) && state.phase == Phase::Cleanup
    {
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
    root: &PrivateDirectory,
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
    ) && state.phase == Phase::Cleanup
    {
        return Ok(None);
    }

    let candidate = NormalState::decode(&bytes, state.input.run.recording)?;
    if candidate != *state && !legal_next(state, &candidate) {
        return Err(invalid());
    }

    Ok(Some(candidate))
}
pub(super) fn check(root: &PrivateDirectory, name: &str, old: &[u8], new: &[u8]) -> io::Result<()> {
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
    root: &PrivateDirectory,
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
    file.write_all(new)?;
    *syncs += 1;
    private::full_sync(&file)?;
    drop(file);
    root.rename(&pending_name, name)?;
    sync_directory(root, syncs)
}
fn read(root: &PrivateDirectory, name: &str, size: usize) -> io::Result<Vec<u8>> {
    let bytes =
        optional_read(root, name, size)?.ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
    if bytes.len() != size {
        return Err(invalid());
    }

    Ok(bytes)
}
fn optional_read(
    root: &PrivateDirectory,
    name: &str,
    maximum: usize,
) -> io::Result<Option<Vec<u8>>> {
    let mut file = match root.open_file(name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::with_capacity(maximum + 1);
    (&mut file)
        .take(u64::try_from(maximum + 1).map_err(|_| invalid())?)
        .read_to_end(&mut bytes)?;
    if bytes.len() > maximum {
        return Err(invalid());
    }

    Ok(Some(bytes))
}
fn sync_file(root: &PrivateDirectory, name: &str, syncs: &mut u64) -> io::Result<()> {
    *syncs += 1;
    private::full_sync(&root.open_file(name)?)
}
fn sync_directory(root: &PrivateDirectory, syncs: &mut u64) -> io::Result<()> {
    *syncs += 1;
    root.full_sync()
}
fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}
