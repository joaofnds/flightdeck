use std::io::{Read, Write};
use std::path::Path;

use collector::recording::private;
use collector::ring::{Identity, RunIdentity};

use crate::event_recovery::Batch;
use crate::{Error, Manifest, Result, WindowIdentity};

const HEADER_WORDS: usize = 14;
const BATCHES_MAX: usize = crate::event_recovery::metadata::BATCHES_MAX;
const BYTES_MAX: usize = (HEADER_WORDS + BATCHES_MAX * 3) * 8;
pub(crate) const WORKING_BYTES: usize = 2 * BYTES_MAX + 16 * 1024;
const MAGIC: u64 = u64::from_le_bytes(*b"FDEVTBL1");

pub(crate) struct Index {
    pub identity: WindowIdentity,
    pub first: u64,
    pub end: u64,
    pub rows: u64,
    pub identities: bool,
    pub batches: Vec<Batch>,
}

impl Index {
    pub(crate) fn read(path: &Path, expected: &Manifest) -> Result<Self> {
        Self::read_file(&mut private::open_private_file(path, false)?, expected)
    }

    pub(crate) fn read_file(file: &mut std::fs::File, expected: &Manifest) -> Result<Self> {
        let mut bytes = Vec::with_capacity(BYTES_MAX + 1);
        file.take(collector::u64_from_usize(BYTES_MAX + 1))
            .read_to_end(&mut bytes)?;
        if bytes.len() < HEADER_WORDS * 8
            || bytes.len() > BYTES_MAX
            || !bytes.len().is_multiple_of(8)
        {
            return Err(Error::Bound("event index size"));
        }
        let word = |index: usize| -> Result<u64> {
            Ok(u64::from_le_bytes(
                bytes
                    .get(index * 8..(index + 1) * 8)
                    .ok_or(Error::Bound("event index word"))?
                    .try_into()
                    .map_err(|_| Error::Bound("event index word"))?,
            ))
        };
        if word(0)? != MAGIC || word(1)? != 1 || word(12)? > 1 {
            return Err(Error::Bound("event index protocol"));
        }
        let count = usize::try_from(word(13)?).map_err(|_| Error::Bound("event index batches"))?;
        if count > BATCHES_MAX || bytes.len() != (HEADER_WORDS + count * 3) * 8 {
            return Err(Error::Bound("event index batches"));
        }
        let mut batches = Vec::with_capacity(count);
        for index in 0..count {
            let start = HEADER_WORDS + index * 3;
            batches.push(Batch {
                first: word(start)?,
                end: word(start + 1)?,
                rows: word(start + 2)?,
            });
        }
        let index = Self {
            identity: WindowIdentity {
                run: RunIdentity {
                    recording: Identity([word(2)?, word(3)?]),
                    run: Identity([word(4)?, word(5)?]),
                    boot: Identity([word(6)?, word(7)?]),
                },
                window: word(8)?,
            },
            first: word(9)?,
            end: word(10)?,
            rows: word(11)?,
            identities: word(12)? == 1,
            batches,
        };
        index.validate()?;
        let rows = expected
            .tables
            .iter()
            .find(|table| table.name == "events")
            .ok_or(Error::Bound("missing event table"))?
            .rows;
        if index.identity != expected.identity
            || index.first != expected.events_first
            || index.end != expected.events_end
            || index.rows != rows
            || expected.event_batches
                != Some(u32::try_from(count).map_err(|_| Error::Bound("event index batches"))?)
        {
            return Err(Error::Bound("event index commitment"));
        }
        Ok(index)
    }

    pub(crate) fn write(&self, path: &Path) -> Result<()> {
        self.validate()?;
        let mut file = private::open_private_file(path, true)?;
        let run = self.identity.run;
        let header = [
            MAGIC,
            1,
            run.recording.0[0],
            run.recording.0[1],
            run.run.0[0],
            run.run.0[1],
            run.boot.0[0],
            run.boot.0[1],
            self.identity.window,
            self.first,
            self.end,
            self.rows,
            u64::from(self.identities),
            collector::u64_from_usize(self.batches.len()),
        ];
        for word in header {
            file.write_all(&word.to_le_bytes())?;
        }
        for batch in &self.batches {
            for word in [batch.first, batch.end, batch.rows] {
                file.write_all(&word.to_le_bytes())?;
            }
        }
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        if self.first == 0 || self.first > self.end || self.batches.len() > BATCHES_MAX {
            return Err(Error::Bound("event index coverage"));
        }
        let mut end = self.first;
        let mut rows = 0;
        for batch in &self.batches {
            if batch.first < end
                || batch.first >= batch.end
                || batch.end > self.end
                || batch.rows == 0
                || batch.rows > collector::u64_from_usize(crate::event_observation::BATCH_ROWS_MAX)
                || batch.rows > batch.end - batch.first
            {
                return Err(Error::Bound("event index batch"));
            }
            end = batch.end;
            rows += batch.rows;
        }
        if rows != self.rows || rows > collector::u64_from_usize(crate::event_observation::ROWS_MAX)
        {
            return Err(Error::Bound("event index rows"));
        }
        Ok(())
    }
}
