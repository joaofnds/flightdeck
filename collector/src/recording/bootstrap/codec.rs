//! Exact common envelopes. Occupied payload bytes grant no execution authority.
use crate::ring::Identity;
use std::io;

pub const BOOTSTRAP_BYTES: usize = 64;
pub const SLOT_BYTES: usize = 4096;
pub const STATE_BYTES: usize = 256;
pub const PAYLOAD_BYTES: usize = 2048;
const _: () = assert!(64 + PAYLOAD_BYTES <= SLOT_BYTES);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum SlotKind {
    Foreground = 1,
    Maintenance = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Operation {
    Ordinary = 1,
    Terminal = 2,
    ExceptionalFragment = 3,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Phase {
    Registered = 1,
    Ready = 2,
    Publishing = 3,
    Published = 4,
    Cleanup = 5,
    SalvagePlanned = 6,
    SalvageCommitted = 7,
    UnresolvedExtent = 8,
}

/// Structural metadata only. Persistence must validate the operation payload separately.
#[derive(Debug)]
pub enum Slot<'bytes> {
    Idle,
    Occupied {
        transaction: Identity,
        phase: Phase,
        operation: Operation,
        payload: &'bytes [u8],
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NamespaceState {
    Quiet {
        sequence: u64,
    },
    Busy {
        sequence: u64,
        transaction: Identity,
    },
}

pub fn encode_bootstrap(identity: Identity, bytes: &mut [u8; BOOTSTRAP_BYTES]) -> io::Result<()> {
    nonzero(identity)?;
    bytes.fill(0);
    bytes[..8].copy_from_slice(b"FDBOOT02");
    put_word(bytes, 8, 1);
    put_word(bytes, 12, 2);
    put_identity(bytes, 16, identity);
    put_word(bytes, 32, 2);
    put_word(bytes, 36, 2);
    Ok(())
}

pub fn decode_bootstrap(bytes: &[u8]) -> io::Result<Identity> {
    if bytes.len() != BOOTSTRAP_BYTES || &bytes[..8] != b"FDBOOT02" {
        return Err(invalid());
    }

    if word(bytes, 8) != 1 || word(bytes, 12) != 2 || word(bytes, 32) != 2 || word(bytes, 36) != 2 {
        return Err(invalid());
    }

    zeros(&bytes[40..])?;
    let identity = identity(bytes, 16);
    nonzero(identity)?;
    Ok(identity)
}

pub fn encode_idle(
    recording: Identity,
    kind: SlotKind,
    bytes: &mut [u8; SLOT_BYTES],
) -> io::Result<()> {
    encode_idle_header(
        recording,
        kind,
        (&mut bytes[..64]).try_into().map_err(|_| invalid())?,
    )?;
    bytes[64..].fill(0);
    Ok(())
}

pub(super) fn encode_idle_header(
    recording: Identity,
    kind: SlotKind,
    bytes: &mut [u8; 64],
) -> io::Result<()> {
    nonzero(recording)?;
    bytes.fill(0);
    bytes[..8].copy_from_slice(b"FDTXN002");
    put_word(bytes, 8, 1);
    put_word(bytes, 12, slot_kind_word(kind));
    put_identity(bytes, 16, recording);
    Ok(())
}

pub fn decode_slot(bytes: &[u8], recording: Identity, kind: SlotKind) -> io::Result<Slot<'_>> {
    if bytes.len() != SLOT_BYTES || &bytes[..8] != b"FDTXN002" {
        return Err(invalid());
    }

    nonzero(recording)?;
    if word(bytes, 8) != 1
        || word(bytes, 12) != slot_kind_word(kind)
        || identity(bytes, 16) != recording
    {
        return Err(invalid());
    }

    zeros(&bytes[60..64])?;
    let transaction = identity(bytes, 32);
    let phase = word(bytes, 48);
    let operation = word(bytes, 52);
    let length = usize::try_from(word(bytes, 56)).map_err(|_| invalid())?;
    if phase == 0 && operation == 0 && length == 0 && transaction == Identity([0, 0]) {
        zeros(&bytes[64..])?;
        return Ok(Slot::Idle);
    }

    if kind == SlotKind::Maintenance {
        return Err(io::Error::from(io::ErrorKind::Unsupported));
    }

    nonzero(transaction)?;
    let phase = match phase {
        1 => Phase::Registered,
        2 => Phase::Ready,
        3 => Phase::Publishing,
        4 => Phase::Published,
        5 => Phase::Cleanup,
        6 => Phase::SalvagePlanned,
        7 => Phase::SalvageCommitted,
        8 => Phase::UnresolvedExtent,
        _ => return Err(invalid()),
    };
    let operation = match operation {
        1 => Operation::Ordinary,
        2 => Operation::Terminal,
        3 => Operation::ExceptionalFragment,
        _ => return Err(invalid()),
    };
    if length != PAYLOAD_BYTES {
        return Err(invalid());
    }

    zeros(&bytes[64 + length..])?;
    Ok(Slot::Occupied {
        transaction,
        phase,
        operation,
        payload: &bytes[64..64 + length],
    })
}

pub fn encode_state(
    recording: Identity,
    state: &NamespaceState,
    bytes: &mut [u8; STATE_BYTES],
) -> io::Result<()> {
    encode_state_header(
        recording,
        state,
        (&mut bytes[..56]).try_into().map_err(|_| invalid())?,
    )?;
    bytes[56..].fill(0);
    Ok(())
}

pub(super) fn encode_state_header(
    recording: Identity,
    state: &NamespaceState,
    bytes: &mut [u8; 56],
) -> io::Result<()> {
    nonzero(recording)?;
    bytes.fill(0);
    bytes[..8].copy_from_slice(b"FDNSP002");
    put_word(bytes, 8, 1);
    put_identity(bytes, 16, recording);
    let sequence = match state {
        NamespaceState::Quiet { sequence } => sequence,
        NamespaceState::Busy {
            sequence,
            transaction,
        } => {
            nonzero(*transaction)?;
            put_word(bytes, 12, 1);
            put_identity(bytes, 40, *transaction);
            sequence
        }
    };
    bytes[32..40].copy_from_slice(&sequence.to_be_bytes());
    Ok(())
}

/// The transaction a namespace's Busy names while physical removal holds it, which no publication
/// can be, since each draws its identity at random from 128 bits.
pub const RETIREMENT_TRANSACTION: Identity = Identity([u64::MAX, u64::MAX]);

pub fn decode_state(bytes: &[u8], recording: Identity) -> io::Result<NamespaceState> {
    if bytes.len() != STATE_BYTES || &bytes[..8] != b"FDNSP002" {
        return Err(invalid());
    }

    nonzero(recording)?;
    if word(bytes, 8) != 1 || identity(bytes, 16) != recording {
        return Err(invalid());
    }

    zeros(&bytes[56..])?;
    let sequence = u64::from_be_bytes(bytes[32..40].try_into().map_err(|_| invalid())?);
    let transaction = identity(bytes, 40);
    match word(bytes, 12) {
        0 if transaction == Identity([0, 0]) => Ok(NamespaceState::Quiet { sequence }),
        1 => {
            nonzero(transaction)?;
            Ok(NamespaceState::Busy {
                sequence,
                transaction,
            })
        }
        _ => Err(invalid()),
    }
}

pub(super) fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}
fn nonzero(identity: Identity) -> io::Result<()> {
    if identity == Identity([0, 0]) {
        return Err(invalid());
    }

    Ok(())
}
fn zeros(bytes: &[u8]) -> io::Result<()> {
    if bytes.iter().any(|byte| *byte != 0) {
        return Err(invalid());
    }

    Ok(())
}
fn word(bytes: &[u8], start: usize) -> u32 {
    u32::from_be_bytes([
        bytes[start],
        bytes[start + 1],
        bytes[start + 2],
        bytes[start + 3],
    ])
}
fn identity(bytes: &[u8], start: usize) -> Identity {
    let mut words = [0; 2];
    for (index, word) in words.iter_mut().enumerate() {
        let offset = start + index * 8;
        let mut encoded = [0; 8];
        encoded.copy_from_slice(&bytes[offset..offset + 8]);
        *word = u64::from_be_bytes(encoded);
    }
    Identity(words)
}
fn put_word(bytes: &mut [u8], start: usize, word: u32) {
    bytes[start..start + 4].copy_from_slice(&word.to_be_bytes());
}
fn put_identity(bytes: &mut [u8], start: usize, identity: Identity) {
    for (index, word) in identity.0.iter().enumerate() {
        let offset = start + index * 8;
        bytes[offset..offset + 8].copy_from_slice(&word.to_be_bytes());
    }
}

fn slot_kind_word(kind: SlotKind) -> u32 {
    match kind {
        SlotKind::Foreground => 1,
        SlotKind::Maintenance => 2,
    }
}
