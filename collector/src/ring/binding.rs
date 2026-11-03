//! Immutable per-object evidence, separate from a logical run and mutable stream progress.
use super::{Identity, layout};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ObjectBinding {
    pub identity: Identity,
    pub generation: u64,
    pub publication_first: u64,
    pub event_first: u64,
}
pub(super) fn snapshot(words: &[AtomicU64], planned: &layout::Layout) -> io::Result<[u64; 64]> {
    if !layout::bound(planned.version) || words.len() < 44 {
        return Err(io::Error::from(io::ErrorKind::Unsupported));
    }
    if words[layout::HEADER_MAGIC].load(Ordering::Acquire) != layout::MAGIC {
        return Err(io::Error::from(io::ErrorKind::WouldBlock));
    }
    let mut fields = [0; 64];
    for index in 0..44 {
        if !matches!(index, 4 | 7 | 29 | 34 | 35) {
            fields[index] = words[index].load(Ordering::Relaxed);
        }
    }
    if fields[1] != planned.version
        || fields[40..42] == [0, 0]
        || fields[42] != 1
        || fields[43] != 0
        || fields[2] == 0
        || fields[3] == 0
        || fields[6] == 0
    {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    let mut start = planned.base_word_count();
    let mut output = 44;
    if let Some(battery) = &planned.battery {
        snapshot_extension(words, start, 4, &mut fields[output..output + 4])?;
        if fields[output..output + 4] != battery.header() {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        output += 4;
        start = battery.word_count();
    }
    if let Some(disks) = &planned.disks {
        snapshot_extension(words, start, 4, &mut fields[output..output + 4])?;
        if fields[output..output + 4] != disks.header() {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
        output += 4;
        start = disks.word_count();
    }
    if let Some(system) = &planned.system {
        snapshot_extension(words, start, 7, &mut fields[output..output + 7])?;
        if fields[output..output + 7] != system.header() {
            return Err(io::Error::from(io::ErrorKind::InvalidData));
        }
    }
    if words[layout::HEADER_MAGIC].load(Ordering::Acquire) != layout::MAGIC {
        return Err(io::Error::from(io::ErrorKind::WouldBlock));
    }
    Ok(fields)
}
fn snapshot_extension(
    words: &[AtomicU64],
    start: usize,
    count: usize,
    output: &mut [u64],
) -> io::Result<()> {
    let source = words
        .get(start..start + count)
        .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
    for (target, source) in output.iter_mut().zip(source) {
        *target = source.load(Ordering::Relaxed);
    }
    Ok(())
}
pub(super) fn read(
    words: &[AtomicU64],
    planned: &layout::Layout,
    admitted: &[u64; 64],
) -> io::Result<ObjectBinding> {
    let fields = snapshot(words, planned)?;
    if fields != *admitted || snapshot(words, planned)? != fields {
        return Err(io::Error::from(io::ErrorKind::InvalidData));
    }
    Ok(ObjectBinding {
        identity: Identity([fields[40], fields[41]]),
        generation: fields[2],
        publication_first: fields[3],
        event_first: fields[6],
    })
}
