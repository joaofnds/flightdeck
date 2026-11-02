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
    bytes: &'bytes [u8; STATE_BYTES],
    recording: Identity,
}

impl<'root, 'bytes> StateObservation<'root, 'bytes> {
    /// Initializes caller-owned storage without returning a large struct by value.
    pub fn begin(
        root: &'root PrivateDirectory,
        recording: Identity,
        bytes: &'bytes mut [u8; STATE_BYTES],
    ) -> io::Result<Option<Self>> {
        if bootstrap_present(root)? {
            return Ok(None);
        }

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
            bytes,
            recording,
        }))
    }

    /// Independently opens the current name, comparing it while the first inode
    /// is retained. False means Pending/Retry, never an empty completed walk.
    pub fn unchanged(&self, scratch: &mut [u8; STATE_BYTES]) -> io::Result<bool> {
        if bootstrap_present(self.root)? {
            return Ok(false);
        }

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
        if scratch != self.bytes {
            return Ok(false);
        }

        Ok(matches!(
            decode_state(scratch, self.recording)?,
            NamespaceState::Quiet { .. }
        ))
    }
}

fn bootstrap_present(root: &PrivateDirectory) -> io::Result<bool> {
    match root.open_file("bootstrap") {
        Ok(file) => {
            drop(file);
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}
