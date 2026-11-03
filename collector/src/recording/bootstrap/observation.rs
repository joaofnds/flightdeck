use super::{
    codec::{NamespaceState, STATE_BYTES, decode_state},
    root::read_into,
};
use crate::{recording::private::PrivateDirectory, ring::Identity};
use std::os::unix::fs::MetadataExt;
use std::{fs::File, io};

/// Holds the first inode alive across a bounded reader walk; takes no writer lock.
pub struct StateObservation<'root, 'bytes> {
    root: &'root PrivateDirectory,
    file: File,
    bytes: ObservationBytes<'bytes>,
    recording: Identity,
}

enum ObservationBytes<'bytes> {
    Borrowed(&'bytes [u8; STATE_BYTES]),
    Owned(Box<[u8; STATE_BYTES]>),
}
impl ObservationBytes<'_> {
    fn value(&self) -> &[u8; STATE_BYTES] {
        match self {
            Self::Borrowed(bytes) => bytes,
            Self::Owned(bytes) => bytes,
        }
    }
}
impl<'root> StateObservation<'root, 'static> {
    /// Retains fixed immutable bytes for a consuming writer's bounded read continuation.
    /// This observes Quiet only and grants no transaction repair or mutation authority.
    pub fn begin_owned(
        root: &'root PrivateDirectory,
        recording: Identity,
        attempts: &mut u64,
    ) -> io::Result<Option<Self>> {
        if bootstrap_present(root, attempts)? {
            return Ok(None);
        }

        *attempts += 1;
        let mut file = root.open_file("namespace-state")?;
        let mut bytes = [0; STATE_BYTES];
        read_into(&mut file, &mut bytes)?;
        if !matches!(
            decode_state(&bytes, recording)?,
            NamespaceState::Quiet { .. }
        ) {
            return Ok(None);
        }

        Ok(Some(Self {
            root,
            file,
            bytes: ObservationBytes::Owned(Box::new(bytes)),
            recording,
        }))
    }
}

impl<'root, 'bytes> StateObservation<'root, 'bytes> {
    /// Initializes caller-owned storage without returning a large struct by value.
    pub fn begin(
        root: &'root PrivateDirectory,
        recording: Identity,
        bytes: &'bytes mut [u8; STATE_BYTES],
    ) -> io::Result<Option<Self>> {
        Self::begin_counted(root, recording, bytes, &mut 0)
    }

    pub fn begin_counted(
        root: &'root PrivateDirectory,
        recording: Identity,
        bytes: &'bytes mut [u8; STATE_BYTES],
        attempts: &mut u64,
    ) -> io::Result<Option<Self>> {
        if bootstrap_present(root, attempts)? {
            return Ok(None);
        }

        *attempts += 1;
        let mut file = root.open_file("namespace-state")?;
        read_into(&mut file, bytes)?;
        if !matches!(
            decode_state(bytes, recording)?,
            NamespaceState::Quiet { .. }
        ) {
            return Ok(None);
        }

        Ok(Some(Self {
            root,
            file,
            bytes: ObservationBytes::Borrowed(bytes),
            recording,
        }))
    }

    /// Independently opens the current name, comparing it while the first inode
    /// is retained. False means Pending/Retry, never an empty completed walk.
    pub fn unchanged(&self, scratch: &mut [u8; STATE_BYTES]) -> io::Result<bool> {
        self.unchanged_counted(scratch, &mut 0)
    }

    pub fn unchanged_counted(
        &self,
        scratch: &mut [u8; STATE_BYTES],
        attempts: &mut u64,
    ) -> io::Result<bool> {
        if bootstrap_present(self.root, attempts)? {
            return Ok(false);
        }

        *attempts += 1;
        let mut current = match self.root.open_file("namespace-state") {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let first = self.file.metadata()?;
        let second = current.metadata()?;
        if first.dev() != second.dev() || first.ino() != second.ino() {
            return Ok(false);
        }

        read_into(&mut current, scratch)?;
        if scratch != self.bytes.value() {
            return Ok(false);
        }

        Ok(matches!(
            decode_state(scratch, self.recording)?,
            NamespaceState::Quiet { .. }
        ))
    }
}

fn bootstrap_present(root: &PrivateDirectory, attempts: &mut u64) -> io::Result<bool> {
    *attempts += 1;
    match root.open_file("bootstrap") {
        Ok(file) => {
            drop(file);
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
