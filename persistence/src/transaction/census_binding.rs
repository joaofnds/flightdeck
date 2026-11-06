//! The occupied recovery commitment. This is not canonical row/loss authority.
use crate::store::FileIdentity;
use std::io;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct CensusBinding {
    pub kind: u32,
    pub digest: [u8; 32],
    pub file: Option<FileIdentity>,
}
impl CensusBinding {
    pub(crate) fn encode(&self, bytes: &mut [u8; 96]) -> io::Result<()> {
        if self.kind > 2
            || (self.kind == 0 && self.digest != [0; 32])
            || (self.kind < 2 && self.file.is_some())
            || (self.kind == 2 && self.file.is_none_or(|file| file.bytes() != 394_720))
        {
            return Err(invalid());
        }
        bytes.fill(0);
        bytes[..4].copy_from_slice(&self.kind.to_be_bytes());
        bytes[8..40].copy_from_slice(&self.digest);
        if let Some(file) = &self.file {
            file.census_encode((&mut bytes[40..96]).try_into().map_err(|_| invalid())?)?;
        }
        Ok(())
    }
    pub(crate) fn decode(bytes: &[u8; 96]) -> io::Result<Self> {
        let kind = u32::from_be_bytes(bytes[..4].try_into().map_err(|_| invalid())?);
        let binding = Self {
            kind,
            digest: bytes[8..40].try_into().map_err(|_| invalid())?,
            file: if kind == 2 {
                Some(FileIdentity::census_decode(
                    bytes[40..96].try_into().map_err(|_| invalid())?,
                )?)
            } else {
                None
            },
        };
        let mut expected = [0; 96];
        binding.encode(&mut expected)?;
        if expected != *bytes {
            return Err(invalid());
        }
        Ok(binding)
    }
}
fn invalid() -> io::Error {
    io::Error::from(io::ErrorKind::InvalidData)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn commitment_literal_has_no_digest_sentinel() {
        // Goal: pin selected commitments and reserved bytes independently, including zero digest.
        // Method: literal96 encoding and invalid kind/reserved/identity variants.
        let mut literal = [0; 96];
        literal[3] = 1;
        let selected = CensusBinding {
            kind: 1,
            ..CensusBinding::default()
        };
        let mut encoded = [0; 96];
        selected.encode(&mut encoded).unwrap();
        assert_eq!(encoded, literal);
        assert_eq!(CensusBinding::decode(&literal).unwrap(), selected);
        for (offset, value) in [(3, 3), (4, 1), (7, 1), (40, 1), (95, 1)] {
            let mut changed = literal;
            changed[offset] = value;
            assert!(CensusBinding::decode(&changed).is_err());
        }
        literal[3] = 0;
        literal[8] = 1;
        assert!(CensusBinding::decode(&literal).is_err());
    }
}
