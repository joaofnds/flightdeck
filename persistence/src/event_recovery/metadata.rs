use std::fs::File;
use std::io::{self, Read, Write};

use collector::ring::{Identity, RunIdentity};

use super::{Batch, Recovery};
use crate::metadata::invalid;

pub(crate) const BATCHES_MAX: usize = super::batch::BATCHES_MAX;
const HEADER_WORDS: usize = 16;
const WORDS_MAX: usize = HEADER_WORDS + BATCHES_MAX * 3;
const MAGIC: u64 = u64::from_le_bytes(*b"FDEVENT1");

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Manifest {
    pub recovery: Recovery,
    pub identities: bool,
    pub batches: Vec<Batch>,
}

impl Manifest {
    pub(crate) fn validate(&self) -> io::Result<()> {
        let recovery = &self.recovery;
        if recovery.events_first == 0 || recovery.events_first > recovery.events_end {
            return Err(invalid("terminal event range"));
        }
        if self.batches.len() > BATCHES_MAX {
            return Err(invalid("terminal event batches"));
        }
        let mut previous = recovery.events_first;
        let mut rows = 0;
        for batch in &self.batches {
            if batch.first < previous || batch.first >= batch.end || batch.end > recovery.events_end
            {
                return Err(invalid("terminal batch range"));
            }
            if batch.rows == 0
                || batch.rows > collector::u64_from_usize(crate::event_observation::BATCH_ROWS_MAX)
            {
                return Err(invalid("terminal batch rows"));
            }
            if batch.rows > batch.end - batch.first {
                return Err(invalid("terminal batch coverage"));
            }
            rows += batch.rows;
            previous = batch.end;
        }
        if rows != recovery.rows
            || rows > collector::u64_from_usize(crate::event_observation::ROWS_MAX)
        {
            return Err(invalid("terminal event rows"));
        }
        let covered = rows
            .checked_add(recovery.losses.events)
            .ok_or_else(|| invalid("terminal event loss coverage"))?;
        if recovery.events_end - recovery.events_first != covered {
            return Err(invalid("terminal event loss coverage"));
        }
        Ok(())
    }

    pub(crate) fn write(&self, file: &mut File) -> io::Result<()> {
        self.validate()?;
        let value = &self.recovery;
        let words = [
            MAGIC,
            1,
            value.run.recording.0[0],
            value.run.recording.0[1],
            value.run.run.0[0],
            value.run.run.0[1],
            value.run.boot.0[0],
            value.run.boot.0[1],
            value.events_first,
            value.events_end,
            value.rows,
            value.losses.publications,
            value.losses.events,
            u64::from(value.losses.unknown),
            u64::from(self.identities),
            collector::u64_from_usize(self.batches.len()),
        ];
        for word in words {
            file.write_all(&word.to_le_bytes())?;
        }
        for batch in &self.batches {
            for word in [batch.first, batch.end, batch.rows] {
                file.write_all(&word.to_le_bytes())?;
            }
        }
        Ok(())
    }

    pub(crate) fn read(file: &mut File) -> io::Result<Self> {
        let mut bytes = Vec::with_capacity(WORDS_MAX * 8 + 1);
        file.take(collector::u64_from_usize(WORDS_MAX * 8 + 1))
            .read_to_end(&mut bytes)?;
        if bytes.len() < HEADER_WORDS * 8 || bytes.len() > WORDS_MAX * 8 || bytes.len() % 8 != 0 {
            return Err(invalid("terminal metadata size"));
        }
        let word = |index: usize| -> io::Result<u64> {
            let bytes = bytes
                .get(index * 8..(index + 1) * 8)
                .ok_or_else(|| invalid("terminal metadata word"))?;
            Ok(u64::from_le_bytes(bytes.try_into().map_err(invalid)?))
        };
        if word(0)? != MAGIC || word(1)? != 1 || word(13)? > 1 || word(14)? > 1 {
            return Err(invalid("terminal metadata protocol"));
        }
        let count = usize::try_from(word(15)?).map_err(invalid)?;
        if count > BATCHES_MAX || bytes.len() != (HEADER_WORDS + count * 3) * 8 {
            return Err(invalid("terminal metadata batches"));
        }
        let mut batches = Vec::with_capacity(count);
        for index in 0..count {
            let offset = HEADER_WORDS + index * 3;
            batches.push(Batch {
                first: word(offset)?,
                end: word(offset + 1)?,
                rows: word(offset + 2)?,
            });
        }
        let manifest = Self {
            recovery: Recovery {
                run: RunIdentity {
                    recording: Identity([word(2)?, word(3)?]),
                    run: Identity([word(4)?, word(5)?]),
                    boot: Identity([word(6)?, word(7)?]),
                },
                events_first: word(8)?,
                events_end: word(9)?,
                rows: word(10)?,
                losses: crate::Losses {
                    publications: word(11)?,
                    events: word(12)?,
                    unknown: word(13)? == 1,
                },
            },
            identities: word(14)? == 1,
            batches,
        };
        manifest.validate()?;
        Ok(manifest)
    }
}
