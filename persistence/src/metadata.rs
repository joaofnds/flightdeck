use std::fs::File;
use std::io::{self, Read, Write};

pub use collector::recording::discovery::TimeBounds;
use collector::ring::{Identity, RunIdentity};

use crate::{TableManifest, WindowIdentity};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub identity: WindowIdentity,
    pub publication_first: u64,
    pub publication_end: u64,
    pub events_first: u64,
    pub events_end: u64,
    pub wall_ns: Option<TimeBounds>,
    pub continuous_ns: Option<TimeBounds>,
    pub awake_ns: Option<TimeBounds>,
    pub losses: Losses,
    pub data_directory: String,
    pub tables: Vec<TableManifest>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Losses {
    pub publications: u64,
    pub events: u64,
    pub unknown: bool,
}

const MAGIC: &[u8; 8] = b"FDSEAL01";
const ENCODING_VERSION: u64 = 1;
const METADATA_BYTES_MAX: u64 = 4096;
const TABLES_MAX: usize = 16;
const DIRECTORY_BYTES_MAX: usize = 256;

impl Manifest {
    pub(crate) fn write(&self, file: &mut File) -> io::Result<()> {
        self.validate()?;
        file.write_all(MAGIC)?;
        file.write_all(&u64::from(crate::SCHEMA_VERSION).to_le_bytes())?;
        file.write_all(&ENCODING_VERSION.to_le_bytes())?;
        for identity in [
            self.identity.run.recording,
            self.identity.run.run,
            self.identity.run.boot,
        ] {
            for word in identity.0 {
                file.write_all(&word.to_le_bytes())?;
            }
        }
        let words = [
            self.identity.window,
            self.publication_first,
            self.publication_end,
            self.events_first,
            self.events_end,
            self.losses.publications,
            self.losses.events,
            u64::from(self.losses.unknown),
        ];
        for word in words {
            file.write_all(&word.to_le_bytes())?;
        }
        for bounds in [self.wall_ns, self.continuous_ns, self.awake_ns] {
            write_bounds(file, bounds)?;
        }
        write_string(file, &self.data_directory)?;
        file.write_all(
            &u64::try_from(self.tables.len())
                .map_err(invalid)?
                .to_le_bytes(),
        )?;
        for table in &self.tables {
            write_string(file, table.name)?;
            file.write_all(&table.rows.to_le_bytes())?;
        }

        Ok(())
    }

    pub(crate) fn read(file: &mut File) -> io::Result<Self> {
        let mut bytes = Vec::with_capacity(usize::try_from(METADATA_BYTES_MAX).map_err(invalid)?);
        file.take(METADATA_BYTES_MAX + 1).read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len()).map_err(invalid)? > METADATA_BYTES_MAX {
            return Err(invalid("metadata size"));
        }
        let mut decoder = Decoder {
            bytes: &bytes,
            position: 0,
        };
        if decoder.take(8)? != MAGIC {
            return Err(invalid("metadata version"));
        }
        if decoder.word()? != u64::from(crate::SCHEMA_VERSION) {
            return Err(invalid("schema version"));
        }
        if decoder.word()? != ENCODING_VERSION {
            return Err(invalid("encoding version"));
        }
        let identity = WindowIdentity {
            run: RunIdentity {
                recording: decoder.identity()?,
                run: decoder.identity()?,
                boot: decoder.identity()?,
            },
            window: decoder.word()?,
        };
        let mut manifest = Self {
            identity,
            publication_first: decoder.word()?,
            publication_end: decoder.word()?,
            events_first: decoder.word()?,
            events_end: decoder.word()?,
            losses: Losses {
                publications: decoder.word()?,
                events: decoder.word()?,
                unknown: decoder.boolean()?,
            },
            wall_ns: decoder.bounds()?,
            continuous_ns: decoder.bounds()?,
            awake_ns: decoder.bounds()?,
            data_directory: decoder.string()?.to_owned(),
            tables: Vec::with_capacity(TABLES_MAX),
        };
        let count = decoder.word()?;
        if count > u64::try_from(TABLES_MAX).map_err(invalid)? {
            return Err(invalid("table count"));
        }
        for _index in 0..count {
            let name = decoder.string()?;
            let name = crate::schema::TABLES
                .iter()
                .find(|table| table.0.name() == name)
                .map(|table| table.0.name())
                .ok_or_else(|| invalid("unknown table"))?;
            manifest.tables.push(TableManifest {
                name,
                rows: decoder.word()?,
            });
        }
        if decoder.position != bytes.len() {
            return Err(invalid("trailing metadata"));
        }
        manifest.validate()?;
        Ok(manifest)
    }

    pub(crate) fn validate(&self) -> io::Result<()> {
        if self.publication_first >= self.publication_end {
            return Err(invalid("publication range"));
        }
        if self.events_first > self.events_end {
            return Err(invalid("event range"));
        }
        for bounds in [self.wall_ns, self.continuous_ns, self.awake_ns]
            .into_iter()
            .flatten()
        {
            if bounds.min > bounds.max {
                return Err(invalid("time bounds"));
            }
        }
        if self.data_directory.len() > DIRECTORY_BYTES_MAX {
            return Err(invalid("directory length"));
        }
        if self.tables.len() != crate::schema::TABLES.len() {
            return Err(invalid("incomplete tables"));
        }
        if self.tables.len() > TABLES_MAX {
            return Err(invalid("table count"));
        }
        for (index, table) in self.tables.iter().enumerate() {
            if !crate::schema::TABLES
                .iter()
                .any(|known| known.0.name() == table.name)
            {
                return Err(invalid("unknown table"));
            }
            if self.tables[..index]
                .iter()
                .any(|earlier| earlier.name == table.name)
            {
                return Err(invalid("duplicate table"));
            }
        }
        Ok(())
    }
}

struct Decoder<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> Decoder<'a> {
    fn take(&mut self, count: usize) -> io::Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| invalid("metadata size"))?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| invalid("truncated metadata"))?;
        self.position = end;
        Ok(value)
    }

    fn word(&mut self) -> io::Result<u64> {
        let mut word = [0; 8];
        word.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(word))
    }

    fn boolean(&mut self) -> io::Result<bool> {
        match self.word()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid("boolean")),
        }
    }

    fn identity(&mut self) -> io::Result<Identity> {
        Ok(Identity([self.word()?, self.word()?]))
    }

    fn bounds(&mut self) -> io::Result<Option<TimeBounds>> {
        let present = self.boolean()?;
        let min = self.word()?;
        let max = self.word()?;
        if present {
            Ok(Some(TimeBounds { min, max }))
        } else {
            if min != 0 {
                return Err(invalid("absent bound payload"));
            }
            if max != 0 {
                return Err(invalid("absent bound payload"));
            }
            Ok(None)
        }
    }

    fn string(&mut self) -> io::Result<&'a str> {
        let length = usize::try_from(self.word()?).map_err(invalid)?;
        if length > DIRECTORY_BYTES_MAX {
            return Err(invalid("string size"));
        }
        std::str::from_utf8(self.take(length)?).map_err(invalid)
    }
}

fn write_bounds(file: &mut File, bounds: Option<TimeBounds>) -> io::Result<()> {
    let words = match bounds {
        Some(bounds) => [1, bounds.min, bounds.max],
        None => [0, 0, 0],
    };
    for word in words {
        file.write_all(&word.to_le_bytes())?;
    }
    Ok(())
}

fn write_string(file: &mut File, value: &str) -> io::Result<()> {
    file.write_all(&u64::try_from(value.len()).map_err(invalid)?.to_le_bytes())?;
    file.write_all(value.as_bytes())
}

pub(crate) fn invalid(message: impl std::fmt::Display) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.to_string())
}
