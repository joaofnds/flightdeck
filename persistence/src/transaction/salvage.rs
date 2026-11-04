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
    #[cfg(test)]
    pub fn encode(&self, bytes: &mut [u8; 256]) -> io::Result<()> {
        self.encode_context(bytes, false)
    }
    pub fn encode_context(&self, bytes: &mut [u8; 256], finite: bool) -> io::Result<()> {
        if self.first == 0
            || self.end < self.first
            || self.snapshot_first == 0
            || self.snapshot_end < self.snapshot_first
            || self.snapshot_end - self.snapshot_first
                > collector::u64_from_usize(crate::event_observation::ROWS_MAX)
            || (!finite && self.snapshot_first < self.end && self.snapshot_end > self.first)
            || (finite
                && (self.endpoint != 3
                    || self.terminal
                    || self.snapshot_first < self.first
                    || self.snapshot_end != self.end))
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
    #[cfg(test)]
    pub fn decode(bytes: &[u8; 256]) -> io::Result<Self> {
        Self::decode_context(bytes, false)
    }
    pub fn decode_context(bytes: &[u8; 256], finite: bool) -> io::Result<Self> {
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
        record.encode_context(&mut expected, finite)?;
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
        if self.region > 8 || self.ordinal > 4096 || self.table > 12 || self.directory > 4 {
            return Err(invalid());
        }
        if matches!(self.region, 0 | 8)
            && (self.ordinal != 0 || self.table != 0 || self.directory != 0)
        {
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

/// Fixed nonzero candidate intent. Counts and committed bits are hints, never gate authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RowPlan {
    pub count: u32,
    pub first: u64,
    pub end: u64,
    pub source_first: u64,
    pub source_end: u64,
    pub layout: u64,
    pub endpoint: u32,
    pub split: u64,
    pub rows: [u64; 2],
    pub committed: u32,
}
impl RowPlan {
    pub fn interval(&self, candidate: usize) -> std::ops::Range<u64> {
        assert!(candidate < 2);
        if candidate == 1 {
            assert!(self.split != 0);
        }
        let first = if candidate == 0 {
            self.first
        } else {
            self.split
        };
        let end = if candidate == 0 && self.split != 0 {
            self.split
        } else {
            self.end
        };
        first..end
    }
    pub fn encode(&self, bytes: &mut [u8; 256]) -> io::Result<()> {
        if self
            .rows
            .iter()
            .any(|rows| *rows > collector::u64_from_usize(crate::event_observation::ROWS_MAX))
            || self.committed > 3
            || (self.rows[1] > 0 && self.split == 0)
            || !matches!(self.count, 1 | 2)
            || (self.count == 1 && self.split != 0)
            || self.first == 0
            || self.end <= self.first
            || self.source_first < self.first
            || self.source_end < self.source_first
            || self.source_end > self.end
            || self.source_end - self.source_first
                > collector::u64_from_usize(crate::event_observation::ROWS_MAX)
            || !matches!(self.layout, 5 | 6)
            || !matches!(self.endpoint, 1..=3)
            || (self.split != 0 && !(self.first < self.split && self.split < self.end))
        {
            return Err(invalid());
        }
        bytes.fill(0);
        put32(bytes, 0, 1);
        put32(bytes, 4, self.count);
        for (offset, value) in [
            (8, self.first),
            (16, self.end),
            (24, self.source_first),
            (32, self.source_end),
            (40, self.layout),
            (56, self.split),
        ] {
            put64(bytes, offset, value);
        }
        put32(bytes, 48, if self.endpoint == 3 { 1 } else { 2 });
        put32(bytes, 52, self.endpoint);
        put64(bytes, 64, self.rows[0]);
        put64(bytes, 72, self.rows[1]);
        put32(bytes, 88, self.committed);
        Ok(())
    }
    pub fn decode(bytes: &[u8; 256]) -> io::Result<Self> {
        let plan = Self {
            count: get32(bytes, 4)?,
            first: get64(bytes, 8)?,
            end: get64(bytes, 16)?,
            source_first: get64(bytes, 24)?,
            source_end: get64(bytes, 32)?,
            layout: get64(bytes, 40)?,
            endpoint: get32(bytes, 52)?,
            split: get64(bytes, 56)?,
            rows: [get64(bytes, 64)?, get64(bytes, 72)?],
            committed: get32(bytes, 88)?,
        };
        let mut expected = [0; 256];
        plan.encode(&mut expected)?;
        if expected != *bytes {
            return Err(invalid());
        }
        Ok(plan)
    }
}

#[cfg(test)]
mod row_tests {
    use super::*;
    #[test]
    fn final_row_hints_literal_and_reserved_bytes() {
        // Goal: counts and the committed mask stay closed hints at their pinned wire offsets.
        // Method: independent byte literals, then forbidden reserved/count/mask mutations.
        let plan = RowPlan {
            count: 2,
            first: 7,
            end: 11,
            source_first: 9,
            source_end: 11,
            layout: 6,
            endpoint: 2,
            split: 10,
            rows: [2, 1],
            committed: 3,
        };
        let mut literal = [0; 256];
        for (offset, value) in [
            (3, 1),
            (7, 2),
            (15, 7),
            (23, 11),
            (31, 9),
            (39, 11),
            (47, 6),
            (51, 2),
            (55, 2),
            (63, 10),
            (71, 2),
            (79, 1),
            (91, 3),
        ] {
            literal[offset] = value;
        }
        let mut actual = [0; 256];
        plan.encode(&mut actual).unwrap();
        assert_eq!(actual, literal);
        assert_eq!(RowPlan::decode(&literal).unwrap(), plan);
        for (offset, value) in [
            (64, 1),
            (72, 1),
            (80, 1),
            (87, 1),
            (91, 4),
            (92, 1),
            (255, 1),
        ] {
            let mut changed = literal;
            changed[offset] = value;
            assert!(RowPlan::decode(&changed).is_err());
        }
    }
    #[test]
    fn nonzero_plan_literal_wire_and_closed_fields() {
        // Goal: pin nonzero plans independently while leaving count0 literals unchanged.
        // Method: literal byte offsets for both counts, including the immutable split.
        for count in [1, 2] {
            let plan = RowPlan {
                count,
                first: 7,
                end: 11,
                source_first: 9,
                source_end: 11,
                layout: 6,
                endpoint: 2,
                split: if count == 2 { 10 } else { 0 },
                rows: [0; 2],
                committed: 0,
            };
            let mut literal = [0; 256];
            literal[3] = 1;
            literal[7] = u8::try_from(count).unwrap();
            literal[15] = 7;
            literal[23] = 11;
            literal[31] = 9;
            literal[39] = 11;
            literal[47] = 6;
            literal[51] = 2;
            literal[55] = 2;
            if count == 2 {
                literal[63] = 10;
            }
            let mut actual = [0; 256];
            plan.encode(&mut actual).unwrap();
            assert_eq!(actual, literal);
            assert_eq!(RowPlan::decode(&literal).unwrap(), plan);
            for offset in [64, 72, 80, 88, 92, 96, 255] {
                let mut changed = literal;
                changed[offset] = 1;
                assert!(RowPlan::decode(&changed).is_err());
            }
        }
    }
}
