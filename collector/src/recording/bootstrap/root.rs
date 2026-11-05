use super::codec::{
    BOOTSTRAP_BYTES, NamespaceState, Slot, SlotKind, decode_bootstrap, decode_slot, decode_state,
    encode_bootstrap, encode_idle, encode_idle_header, encode_state, encode_state_header, invalid,
};
use crate::{
    recording::{
        format::{READER_PROTOCOL, WRITER_PROTOCOL},
        private::{self, FileLock, PrivateDirectory},
        random_identity,
    },
    ring::Identity,
};
use std::os::unix::fs::MetadataExt;
use std::{
    fs::File,
    io::{self, Read, Write},
};

const DIRECTORIES: [&str; 7] = [
    "data",
    "recovery-data",
    "ordinary",
    "observations-known",
    "observations-unknown",
    "terminal",
    "event-fragments",
];
const CONTROLS: [&str; 3] = ["foreground", "maintenance", "namespace-state"];
const ARTIFACTS: [&str; 5] = [
    "bootstrap",
    "foreground",
    "maintenance",
    "namespace-state",
    "identity",
];
const LOCKS: [&str; 3] = ["registration.lock", "collector.lock", "recorder.lock"];
const INVENTORY_MAX: usize = 20;
const CLASSIFY_ATTEMPTS_MAX: usize = 16;
const _: () = assert!(INVENTORY_MAX == 7 + 3 + 1 + 1 + 5 + 3);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Classification {
    Fresh,
    Initializing(Identity),
    Initialized(Identity),
    Unsupported,
    Corrupt,
}

/// Bounded read-only classification through an already admitted root. An absent
/// path is reported by `PrivateDirectory::open` as `NotFound`, before this boundary.
/// A changed observation returns `WouldBlock` (Pending/Retry), with no internal retry.
pub fn classify(root: &PrivateDirectory, scratch: &mut [u8; 4097]) -> io::Result<Classification> {
    match classify_inner(root, scratch) {
        Err(error) if error.kind() == io::ErrorKind::Unsupported => Ok(Classification::Unsupported),
        Err(error) if error.kind() == io::ErrorKind::InvalidData => Ok(Classification::Corrupt),
        result => result,
    }
}

/// Classifies through a change under way, which a read lands on only for the moment a publication
/// replaces a control file, so the writer starting beside that publication is not turned away.
pub fn classify_settled(
    root: &PrivateDirectory,
    scratch: &mut [u8; 4097],
) -> io::Result<Classification> {
    for _attempt in 0..CLASSIFY_ATTEMPTS_MAX {
        match classify(root, scratch) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
            result => return result,
        }
    }
    Err(pending())
}

fn classify_inner(root: &PrivateDirectory, scratch: &mut [u8; 4097]) -> io::Result<Classification> {
    // Identity and bootstrap are immutable, monotone initialization authority.
    // Retaining their initial inodes prevents a removed/replaced name from
    // authorizing a verdict assembled across a publication boundary.
    let mut identity_file = open_optional(root, "identity")?;
    let identity = match identity_file.as_mut() {
        Some(file) => {
            let length = read_bounded(file, scratch)?;
            Some(parse_envelope(&scratch[..length])?)
        }
        None => None,
    };
    let mut bootstrap_file = open_optional(root, "bootstrap")?;
    let bootstrap = match bootstrap_file.as_mut() {
        Some(file) => {
            let length = read_bounded(file, scratch)?;
            Some(decode_bootstrap(&scratch[..length])?)
        }
        None => None,
    };
    let validation = classify_observed(root, identity, bootstrap, scratch);
    if let Err(error) = &validation {
        if !validation_error(error) {
            return validation;
        }
    }
    if !name_unchanged(root, "bootstrap", bootstrap_file.as_ref())?
        || !name_unchanged(root, "identity", identity_file.as_ref())?
    {
        return Err(pending());
    }

    validation
}

fn classify_observed(
    root: &PrivateDirectory,
    identity: Option<Identity>,
    bootstrap: Option<Identity>,
    scratch: &mut [u8; 4097],
) -> io::Result<Classification> {
    if let (Some(identity), Some(bootstrap)) = (identity, bootstrap) {
        if identity != bootstrap {
            return Err(invalid());
        }
    }
    if let Some(identity) = identity {
        if bootstrap.is_none() {
            validate_runtime(root, identity, scratch)?;
            return Ok(Classification::Initialized(identity));
        }
    }
    validate_inventory(root, bootstrap, scratch)?;
    if let Some(identity) = identity {
        for name in CONTROLS {
            let length = read_optional(root, name, scratch)?.ok_or_else(invalid)?;
            validate_initial(name, identity, &scratch[..length], false)?;
        }
        for name in DIRECTORIES {
            drop(required_directory(root, name)?);
        }
    }
    if let Some(identity) = bootstrap {
        Ok(Classification::Initializing(identity))
    } else {
        Ok(Classification::Fresh)
    }
}

/// Future production initializer. Caller must own the exclusive registration
/// lock for this root throughout, with role lock acquired first where applicable.
/// A genuine `FileLock` is required and its inode must match this root's lock name.
/// This does not enable protocol-two selection in any existing creator.
pub fn initialize(
    root: &PrivateDirectory,
    registration: &FileLock,
    scratch: &mut [u8; 4097],
) -> io::Result<Identity> {
    let classification = classify_settled(root, scratch)?;
    match classification {
        Classification::Unsupported => return Err(io::Error::from(io::ErrorKind::Unsupported)),
        Classification::Corrupt => return Err(invalid()),
        _ => {}
    }
    let named = root.open_file("registration.lock")?.metadata()?;
    let held = registration.file().metadata()?;
    if named.dev() != held.dev() || named.ino() != held.ino() {
        return Err(invalid());
    }

    let identity = match classification {
        Classification::Initialized(identity) => {
            root.full_sync()?;
            return Ok(identity);
        }

        Classification::Initializing(identity) => identity,
        Classification::Fresh => {
            let identity = random_identity()?;
            remove_pending(root)?;
            let length = encode_initial("bootstrap", identity, scratch)?;
            private::atomic_write_in(root, "bootstrap", &scratch[..length])?;
            identity
        }
        _ => return Err(invalid()),
    };
    // A visible rename is authority only after restoring both durability barriers.
    private::full_sync(&root.open_file("bootstrap")?)?;
    root.full_sync()?;
    remove_pending(root)?;
    for name in DIRECTORIES {
        let directory = root.create_directory(name)?;
        directory.full_sync()?;
        root.full_sync()?;
    }
    for name in CONTROLS {
        match root.open_file(name) {
            Ok(file) => private::full_sync(&file)?,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                let length = encode_initial(name, identity, scratch)?;
                private::atomic_write_in(root, name, &scratch[..length])?;
            }
            Err(error) => return Err(error),
        }
        root.full_sync()?;
    }
    if read_optional(root, "identity", scratch)?.is_none() {
        let length = encode_initial("identity", identity, scratch)?;
        private::atomic_write_in(root, "identity", &scratch[..length])?;
    }
    // Revalidation catches any unexpected content before removing authority.
    validate_inventory(root, Some(identity), scratch)?;
    root.remove_file("bootstrap")?;
    root.full_sync()?;
    Ok(identity)
}

fn validate_inventory(
    root: &PrivateDirectory,
    identity: Option<Identity>,
    scratch: &mut [u8; 4097],
) -> io::Result<()> {
    root.visit_names(INVENTORY_MAX, |name| {
        if LOCKS.contains(&name) {
            drop(root.open_file(name)?);
            return Ok(());
        }

        if name == "bootstrap.pending" && identity.is_none() {
            let length = read_optional(root, name, scratch)?.ok_or_else(pending)?;
            if length > BOOTSTRAP_BYTES {
                return Err(invalid());
            }

            let mut expected = [0; 64];
            encode_bootstrap(Identity([1, 1]), &mut expected)?;
            for index in 0..length {
                if !(16..32).contains(&index) && scratch[index] != expected[index] {
                    return Err(invalid());
                }
            }
            if length == BOOTSTRAP_BYTES {
                decode_bootstrap(&scratch[..length])?;
            }
            return Ok(());
        }

        let identity = identity.ok_or_else(invalid)?;
        if DIRECTORIES.contains(&name) {
            return root
                .open_directory(name)?
                .visit_names(0, |_| Err(invalid()));
        }
        let (target, is_pending) = match name.strip_suffix(".pending") {
            Some(target) => (target, true),
            None => (name, false),
        };
        if !ARTIFACTS.contains(&target) {
            return Err(invalid());
        }

        let length = read_optional(root, name, scratch)?.ok_or_else(pending)?;
        validate_initial(target, identity, &scratch[..length], is_pending)
    })
}

fn validate_runtime(
    root: &PrivateDirectory,
    identity: Identity,
    scratch: &mut [u8; 4097],
) -> io::Result<()> {
    // Busy is legitimate here, unlike the Quiet-only reader walk. Hold the
    // initial inode until the independent current-name observation completes.
    let mut state_file = open_optional(root, "namespace-state")?;
    let mut state_bytes = [0; 257];
    let state_length = match state_file.as_mut() {
        Some(file) => read_bounded(file, &mut state_bytes)?,
        None => 0,
    };
    let validation =
        validate_runtime_controls(root, identity, &state_bytes[..state_length], scratch);
    if let Err(error) = &validation {
        if !validation_error(error) {
            return validation;
        }
    }
    let mut current = open_optional(root, "namespace-state")?;
    if !same_file(state_file.as_ref(), current.as_ref())? {
        return Err(pending());
    }

    if let Some(file) = current.as_mut() {
        let length = read_bounded(file, &mut scratch[..257])?;
        if scratch[..length] != state_bytes[..state_length] {
            return Err(pending());
        }
    }
    validation
}

fn validate_runtime_controls(
    root: &PrivateDirectory,
    identity: Identity,
    state_bytes: &[u8],
    scratch: &mut [u8; 4097],
) -> io::Result<()> {
    for name in DIRECTORIES {
        drop(required_directory(root, name)?);
    }
    let length = read_optional(root, "foreground", scratch)?.ok_or_else(invalid)?;
    let transaction = match decode_slot(&scratch[..length], identity, SlotKind::Foreground)? {
        Slot::Idle => None,
        Slot::Occupied { transaction, .. } => Some(transaction),
    };
    let length = read_optional(root, "maintenance", scratch)?.ok_or_else(invalid)?;
    decode_slot(&scratch[..length], identity, SlotKind::Maintenance)?;
    if let NamespaceState::Busy {
        transaction: busy, ..
    } = decode_state(state_bytes, identity)?
    {
        if transaction != Some(busy) {
            return Err(invalid());
        }
    }
    Ok(())
}

fn validation_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::InvalidData | io::ErrorKind::Unsupported | io::ErrorKind::WouldBlock
    )
}

fn pending() -> io::Error {
    io::Error::from(io::ErrorKind::WouldBlock)
}

fn open_optional(root: &PrivateDirectory, name: &str) -> io::Result<Option<File>> {
    match root.open_file(name) {
        Ok(file) => Ok(Some(file)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn name_unchanged(root: &PrivateDirectory, name: &str, first: Option<&File>) -> io::Result<bool> {
    let current = open_optional(root, name)?;
    same_file(first, current.as_ref())
}

fn same_file(first: Option<&File>, current: Option<&File>) -> io::Result<bool> {
    match (first, current) {
        (None, None) => Ok(true),
        (Some(first), Some(current)) => {
            let first = first.metadata()?;
            let current = current.metadata()?;
            Ok(first.dev() == current.dev() && first.ino() == current.ino())
        }
        _ => Ok(false),
    }
}

fn remove_pending(root: &PrivateDirectory) -> io::Result<()> {
    for name in [
        "bootstrap.pending",
        "foreground.pending",
        "maintenance.pending",
        "namespace-state.pending",
        "identity.pending",
    ] {
        match root.open_file(name) {
            Ok(file) => {
                drop(file);
                root.remove_file(name)?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

fn read_optional(
    root: &PrivateDirectory,
    name: &str,
    scratch: &mut [u8; 4097],
) -> io::Result<Option<usize>> {
    let mut file = match root.open_file(name) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let length = read_bounded(&mut file, scratch)?;
    if length == scratch.len() {
        return Err(invalid());
    }

    Ok(Some(length))
}

fn read_bounded(file: &mut File, bytes: &mut [u8]) -> io::Result<usize> {
    let mut length = 0;
    while length < bytes.len() {
        let count = file.read(&mut bytes[length..])?;
        if count == 0 {
            break;
        }
        length += count;
    }
    Ok(length)
}

pub(super) fn read_into(file: &mut File, bytes: &mut [u8]) -> io::Result<()> {
    file.read_exact(bytes)?;
    let mut extra = [0];
    if file.read(&mut extra)? != 0 {
        return Err(invalid());
    }

    Ok(())
}

fn encode_initial(name: &str, identity: Identity, scratch: &mut [u8; 4097]) -> io::Result<usize> {
    match name {
        "bootstrap" => {
            encode_bootstrap(
                identity,
                (&mut scratch[..64]).try_into().map_err(|_| invalid())?,
            )?;
            Ok(64)
        }
        "foreground" | "maintenance" => {
            let kind = if name == "foreground" {
                SlotKind::Foreground
            } else {
                SlotKind::Maintenance
            };
            encode_idle(
                identity,
                kind,
                (&mut scratch[..4096]).try_into().map_err(|_| invalid())?,
            )?;
            Ok(4096)
        }
        "namespace-state" => {
            encode_state(
                identity,
                &NamespaceState::Quiet { sequence: 0 },
                (&mut scratch[..256]).try_into().map_err(|_| invalid())?,
            )?;
            Ok(256)
        }
        "identity" => {
            encode_envelope(
                identity,
                (&mut scratch[..74]).try_into().map_err(|_| invalid())?,
            )?;
            Ok(74)
        }
        _ => Err(invalid()),
    }
}

fn validate_initial(name: &str, identity: Identity, bytes: &[u8], pending: bool) -> io::Result<()> {
    // Initial slots have only a nonzero header; their entire suffix is zero.
    let mut header = [0; 74];
    let length = match name {
        "bootstrap" => {
            encode_bootstrap(
                identity,
                (&mut header[..64]).try_into().map_err(|_| invalid())?,
            )?;
            64
        }
        "identity" => {
            encode_envelope(identity, &mut header)?;
            74
        }
        "foreground" | "maintenance" => {
            let kind = if name == "foreground" {
                SlotKind::Foreground
            } else {
                SlotKind::Maintenance
            };
            encode_idle_header(
                identity,
                kind,
                (&mut header[..64]).try_into().map_err(|_| invalid())?,
            )?;
            4096
        }
        "namespace-state" => {
            encode_state_header(
                identity,
                &NamespaceState::Quiet { sequence: 0 },
                (&mut header[..56]).try_into().map_err(|_| invalid())?,
            )?;
            256
        }
        _ => return Err(invalid()),
    };
    if bytes.len() > length || (!pending && bytes.len() != length) {
        return Err(invalid());
    }

    for (index, byte) in bytes.iter().enumerate() {
        let expected = if index < header.len() {
            header[index]
        } else {
            0
        };
        if *byte != expected {
            return Err(invalid());
        }
    }
    Ok(())
}

fn encode_envelope(identity: Identity, bytes: &mut [u8; 74]) -> io::Result<()> {
    let mut destination = &mut bytes[..];
    write!(
        destination,
        "flightdeck-recording-1\n{READER_PROTOCOL:08x}\n{WRITER_PROTOCOL:08x}\n{:016x}{:016x}\n",
        identity.0[0], identity.0[1]
    )
}

fn parse_envelope(bytes: &[u8]) -> io::Result<Identity> {
    if !bytes.is_ascii() {
        return Err(invalid());
    }

    if bytes.len() == 33 {
        crate::recording::parse_identity(
            std::str::from_utf8(&bytes[..32]).map_err(|_| invalid())?,
        )?;
        if bytes[32] != b'\n' {
            return Err(invalid());
        }

        return Err(io::Error::from(io::ErrorKind::Unsupported));
    }
    if bytes.len() != 74
        || !bytes.starts_with(b"flightdeck-recording-1\n")
        || bytes[31] != b'\n'
        || bytes[40] != b'\n'
        || bytes[73] != b'\n'
    {
        return Err(invalid());
    }

    let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
    let reader = crate::recording::format::parse_requirement(&bytes[23..32])?;
    let writer = crate::recording::format::parse_requirement(&bytes[32..41])?;
    let identity = crate::recording::parse_identity(&text[41..73])?;
    if identity == Identity([0, 0]) || reader == 0 || writer == 0 {
        return Err(invalid());
    }

    if reader != READER_PROTOCOL || writer != WRITER_PROTOCOL {
        return Err(io::Error::from(io::ErrorKind::Unsupported));
    }

    if !bytes[41..73]
        .iter()
        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
    {
        return Err(invalid());
    }

    Ok(identity)
}

fn required_directory(root: &PrivateDirectory, name: &str) -> io::Result<PrivateDirectory> {
    match root.open_directory(name) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Err(invalid()),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_artifacts_accept_only_exact_bytes_or_valid_pending_prefixes() {
        // Goal: extracting shared headers preserves strict scratch admission.
        // Method: check every truncation and single-byte contradiction of each initial artifact.
        let identity = Identity([1, 2]);
        let mut bytes = [0; 4097];
        for (name, expected_length) in [
            ("bootstrap", 64),
            ("foreground", 4096),
            ("maintenance", 4096),
            ("namespace-state", 256),
            ("identity", 74),
        ] {
            let length = encode_initial(name, identity, &mut bytes).unwrap();
            assert_eq!(length, expected_length);
            validate_initial(name, identity, &bytes[..length], false).unwrap();
            validate_initial(name, identity, &bytes[..length], true).unwrap();
            for prefix_length in 0..length {
                validate_initial(name, identity, &bytes[..prefix_length], true).unwrap();
                assert!(validate_initial(name, identity, &bytes[..prefix_length], false).is_err());
            }
            for index in 0..length {
                bytes[index] ^= 1;
                assert!(validate_initial(name, identity, &bytes[..length], false).is_err());
                assert!(validate_initial(name, identity, &bytes[..=index], true).is_err());
                bytes[index] ^= 1;
            }
            assert!(validate_initial(name, identity, &bytes[..=length], false).is_err());
            assert!(validate_initial(name, identity, &bytes[..=length], true).is_err());
        }
    }
}
