//! Fixed zero-candidate salvage intent. This codec alone grants no Store authority.
use super::{get32, get64, invalid, put32, put64};
use std::io;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LossPlan {
    pub first: u64,
    pub end: u64,
    pub snapshot_first: u64,
    pub snapshot_end: u64,
    pub layout: u64,
    pub endpoint: u32,
    pub terminal: bool,
}
impl LossPlan {
    pub fn encode(&self, bytes: &mut [u8; 256]) -> io::Result<()> {
        if self.first == 0
            || self.end < self.first
            || self.snapshot_first == 0
            || self.snapshot_end < self.snapshot_first
            || self.snapshot_end - self.snapshot_first
                > collector::u64_from_usize(crate::event_observation::ROWS_MAX)
            || (self.snapshot_first < self.end && self.snapshot_end > self.first)
            || !matches!(self.layout, 5 | 6)
            || !matches!(self.endpoint, 1 | 3 | 4)
            || (self.terminal && self.endpoint != 3)
        {
            return Err(invalid());
        }
        bytes.fill(0);
        put32(bytes, 0, 1);
        // Candidate count remains zero. Actual Missed evidence is the only supported variant.
        for (offset, value) in [
            (8, self.first),
            (16, self.end),
            (24, self.snapshot_first),
            (32, self.snapshot_end),
            (40, self.layout),
        ] {
            put64(bytes, offset, value);
        }
        put32(bytes, 48, 2);
        put32(bytes, 52, self.endpoint);
        put32(bytes, 92, u32::from(self.terminal));
        Ok(())
    }
    pub fn decode(bytes: &[u8; 256]) -> io::Result<Self> {
        match get32(bytes, 4)? {
            0 => {}
            1 | 2 => return Err(io::Error::from(io::ErrorKind::Unsupported)),
            _ => return Err(invalid()),
        }
        let record = Self {
            first: get64(bytes, 8)?,
            end: get64(bytes, 16)?,
            snapshot_first: get64(bytes, 24)?,
            snapshot_end: get64(bytes, 32)?,
            layout: get64(bytes, 40)?,
            endpoint: get32(bytes, 52)?,
            terminal: match get32(bytes, 92)? {
                0 => false,
                1 => true,
                _ => return Err(invalid()),
            },
        };
        let mut expected = [0; 256];
        record.encode(&mut expected)?;
        if expected != *bytes {
            return Err(invalid());
        }
        Ok(record)
    }
}

/// Separate cleanup coordinates; this is not the publication-next field.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Cleanup {
    pub region: u32,
    pub ordinal: u32,
    pub table: u32,
    pub directory: u32,
}
impl Cleanup {
    pub fn encode(&self, bytes: &mut [u8; 64]) -> io::Result<()> {
        if self.region > 7 || self.ordinal > 4096 || self.table > 12 || self.directory > 4 {
            return Err(invalid());
        }
        if self.region == 0 && (self.ordinal != 0 || self.table != 0 || self.directory != 0) {
            return Err(invalid());
        }
        bytes.fill(0);
        put32(bytes, 0, self.region);
        put32(bytes, 4, self.ordinal);
        put32(bytes, 8, self.table);
        put32(bytes, 12, self.directory);
        Ok(())
    }
    pub fn decode(bytes: &[u8; 64]) -> io::Result<Self> {
        let record = Self {
            region: get32(bytes, 0)?,
            ordinal: get32(bytes, 4)?,
            table: get32(bytes, 8)?,
            directory: get32(bytes, 12)?,
        };
        let mut expected = [0; 64];
        record.encode(&mut expected)?;
        if expected != *bytes {
            return Err(invalid());
        }
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_zero_candidate_plan_matches_literal_offsets_and_rejects_reserved_bytes() {
        // Goal: pin the count0 wire independently, including closed unused plan fields.
        // Method: construct literal BE fields and corrupt each otherwise reserved byte.
        let plan = LossPlan {
            first: 7,
            end: 11,
            snapshot_first: 12,
            snapshot_end: 13,
            layout: 6,
            endpoint: 1,
            terminal: false,
        };
        let mut literal = [0; 256];
        literal[3] = 1;
        literal[15] = 7;
        literal[23] = 11;
        literal[31] = 12;
        literal[39] = 13;
        literal[47] = 6;
        literal[51] = 2;
        literal[55] = 1;
        let mut actual = [0; 256];
        plan.encode(&mut actual).unwrap();
        assert_eq!(actual, literal);
        assert_eq!(LossPlan::decode(&literal).unwrap(), plan);
        for offset in [4, 7, 56, 63, 64, 72, 80, 88, 91, 96, 255] {
            let mut changed = literal;
            changed[offset] = 1;
            assert!(LossPlan::decode(&changed).is_err(), "offset {offset}");
        }
    }
    #[test]
    fn cleanup_wire_and_bounds_are_independent_of_publication_progress() {
        // Goal: fix cleanup coordinates and reserved bytes without an active Store decoder.
        // Method: literal-wire comparison and individual bound/zero-region violations.
        let cursor = Cleanup {
            region: 1,
            ordinal: 4096,
            table: 12,
            directory: 4,
        };
        let mut literal = [0; 64];
        literal[3] = 1;
        literal[6] = 16;
        literal[11] = 12;
        literal[15] = 4;
        let mut bytes = [0; 64];
        cursor.encode(&mut bytes).unwrap();
        assert_eq!(bytes, literal);
        assert_eq!(Cleanup::decode(&literal).unwrap(), cursor);
        for (offset, value) in [(3, 8), (7, 1), (11, 13), (15, 5), (16, 1), (63, 1)] {
            let mut changed = literal;
            changed[offset] = value;
            assert!(Cleanup::decode(&changed).is_err());
        }
        let mut absent = literal;
        absent[3] = 0;
        assert!(Cleanup::decode(&absent).is_err());
    }
}
