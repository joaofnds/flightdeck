use std::io;
use std::path::{Path, PathBuf};

use collector::recording::discovery::{DIRECTORY_BUFFER_WORDS, DirectoryCursor, DirectoryEntry};
use collector::ring::{Identity, RunIdentity};

use super::Recovery;
use crate::{Error, Result};

/// A native directory cursor, with bounded caller-owned page storage and no run accumulation.
pub struct Discovery<'buffer> {
    root: PathBuf,
    recording: Identity,
    cursor: Option<DirectoryCursor<'buffer>>,
}

pub struct DiscoveryPage {
    pub runs: Vec<Recovery>,
    /// True only after actual directory EOF, including a root with no terminal namespace.
    pub complete: bool,
}

impl<'buffer> Discovery<'buffer> {
    pub fn open(root: &Path, buffer: &'buffer mut [u64; DIRECTORY_BUFFER_WORDS]) -> Result<Self> {
        let recording = collector::recording::read_identity(root)?;
        collector::recording::private::verify_directory(root)?;
        let cursor = match DirectoryCursor::open(&super::base(root), buffer) {
            Ok(cursor) => Some(cursor),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        Ok(Self {
            root: root.to_owned(),
            recording,
            cursor,
        })
    }

    /// Visits at most `entries` canonical keys (1..=128); incomplete pages resume this cursor.
    pub fn next_page(&mut self, entries: usize) -> Result<DiscoveryPage> {
        if entries == 0 || entries > 128 {
            return Err(Error::Bound("terminal discovery entries"));
        }
        let mut runs = Vec::with_capacity(entries);
        let Some(cursor) = &mut self.cursor else {
            return Ok(DiscoveryPage {
                runs,
                complete: true,
            });
        };
        for _entry in 0..entries {
            let name = match cursor.next_name()? {
                DirectoryEntry::Complete => {
                    return Ok(DiscoveryPage {
                        runs,
                        complete: true,
                    });
                }
                DirectoryEntry::Name(name) => name,
            };
            let (run, boot) = name
                .split_once('-')
                .ok_or(Error::Bound("terminal discovery key"))?;
            crate::layout::validate_run(run)?;
            crate::layout::validate_run(boot)?;
            let identity = RunIdentity {
                recording: self.recording,
                run: collector::recording::parse_identity(run)?,
                boot: collector::recording::parse_identity(boot)?,
            };
            if let Some(manifest) = crate::store::events::read_committed(&self.root, &identity)? {
                runs.push(manifest.recovery);
            }
        }
        Ok(DiscoveryPage {
            runs,
            complete: false,
        })
    }
}
