//! Retained, bounded native directory enumeration without a libc DIR allocation.
use super::directory::{DirectoryWork, PrivateDirectory, decode_entry};
use std::fs::File;
use std::io;
use std::os::fd::AsRawFd;

/// Owns one independently opened descriptor and a fixed1048-byte buffer.
/// A failed callback leaves the current entry pending for an explicit retry.
pub struct PrivateNames {
    file: File,
    bytes: [u8; 1048],
    length: usize,
    offset: usize,
    base: i64,
    ended: bool,
    failure: Option<io::ErrorKind>,
}
const _: () = assert!(size_of::<PrivateNames>() <= 1280);

impl PrivateDirectory {
    pub fn names(&self, work: &mut DirectoryWork) -> io::Result<PrivateNames> {
        work.open_attempts += 1;
        let directory = self.open_enumeration()?;
        Ok(PrivateNames {
            file: directory,
            bytes: [0; 1048],
            length: 0,
            offset: 0,
            base: 0,
            ended: false,
            failure: None,
        })
    }
}
impl PrivateNames {
    /// Consumes at most one native entry, including dot entries. False means EOF.
    pub fn step(
        &mut self,
        work: &mut DirectoryWork,
        visit: impl FnOnce(&str) -> io::Result<()>,
    ) -> io::Result<bool> {
        if let Some(kind) = self.failure {
            return Err(io::Error::from(kind));
        }

        if self.ended {
            return Ok(false);
        }

        if self.offset == self.length {
            work.read_calls += 1;
            let received = unsafe {
                super::directory::__getdirentries64(
                    self.file.as_raw_fd(),
                    self.bytes.as_mut_ptr().cast(),
                    self.bytes.len(),
                    &raw mut self.base,
                )
            };
            if received < 0 {
                let error = io::Error::last_os_error();
                self.failure = Some(error.kind());
                return Err(error);
            }

            if received == 0 {
                self.ended = true;
                return Ok(false);
            }

            self.length = usize::try_from(received).map_err(|_| invalid())?;
            self.offset = 0;
            if self.length > self.bytes.len() {
                return Err(invalid());
            }
        }

        if self.length > self.bytes.len() {
            return Err(invalid());
        }

        work.entries += 1;
        let record = &self.bytes[self.offset..self.length];
        let (length, name) = decode_entry(record)?;
        if name != "." && name != ".." {
            work.names += 1;
        }
        visit(name)?;
        self.offset += length;
        Ok(true)
    }
}
fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_entry_boundaries_refuse_truncation_and_invalid_names() {
        // Goal: both enumeration policies can share the exact native record grammar.
        // Method: characterize a valid record and change one framing or name constraint.
        let mut record = [0_u8; 24];
        record[16..18].copy_from_slice(&24_u16.to_ne_bytes());
        record[18..20].copy_from_slice(&1_u16.to_ne_bytes());
        record[21] = b'a';
        assert_eq!(decode_entry(&record).unwrap(), (24, "a"));
        for length in 0..24 {
            assert!(decode_entry(&record[..length]).is_err());
        }
        for (offset, value) in [(16, 21), (16, 25), (18, 0), (18, 3), (21, 255), (22, 1)] {
            let mut changed = record;
            changed[offset] = value;
            assert!(
                decode_entry(&changed).is_err(),
                "offset {offset}, value {value}"
            );
        }
        let mut largest = [0_u8; 280];
        largest[16..18].copy_from_slice(&280_u16.to_ne_bytes());
        largest[18..20].copy_from_slice(&255_u16.to_ne_bytes());
        largest[21..276].fill(b'x');
        assert_eq!(decode_entry(&largest).unwrap().1.len(), 255);
        largest[18..20].copy_from_slice(&256_u16.to_ne_bytes());
        assert!(decode_entry(&largest).is_err());
    }
}
