//! Closed48-byte durable evidence for one producer object, not an entire logical run.
use super::{get_identity, get32, get64, invalid, put_identity, put32, put64};
use collector::ring::ObjectBinding;
use std::io;

pub(crate) fn validate(binding: &ObjectBinding) -> io::Result<()> {
    if binding.identity.0 == [0, 0]
        || binding.generation == 0
        || binding.publication_first == 0
        || binding.event_first == 0
    {
        return Err(invalid());
    }
    Ok(())
}
pub(crate) fn encode(binding: &ObjectBinding, bytes: &mut [u8]) -> io::Result<()> {
    validate(binding)?;
    if bytes.len() != 48 {
        return Err(invalid());
    }
    bytes.fill(0);
    put32(bytes, 0, 1);
    put_identity(bytes, 8, binding.identity);
    put64(bytes, 24, binding.generation);
    put64(bytes, 32, binding.publication_first);
    put64(bytes, 40, binding.event_first);
    Ok(())
}
pub(crate) fn decode(bytes: &[u8]) -> io::Result<ObjectBinding> {
    if bytes.len() != 48 || get32(bytes, 0)? != 1 || get32(bytes, 4)? != 0 {
        return Err(invalid());
    }
    let binding = ObjectBinding {
        identity: get_identity(bytes, 8)?,
        generation: get64(bytes, 24)?,
        publication_first: get64(bytes, 32)?,
        event_first: get64(bytes, 40)?,
    };
    validate(&binding)?;
    Ok(binding)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn binding_matches_literal_bytes_and_refuses_missing_or_unknown_evidence() {
        // Goal: fixed object authority has a closed independent48-byte representation.
        // Method: compare literal offsets, then corrupt each discriminator and required value.
        let mut bytes = [0; 48];
        for (offset, value) in [(3, 1), (15, 17), (23, 18), (31, 2), (39, 3), (47, 4)] {
            bytes[offset] = value;
        }
        let binding = decode(&bytes).unwrap();
        assert_eq!(binding.identity.0, [17, 18]);
        assert_eq!(binding.generation, 2);
        assert_eq!(binding.publication_first, 3);
        assert_eq!(binding.event_first, 4);
        let mut encoded = [255; 48];
        encode(&binding, &mut encoded).unwrap();
        assert_eq!(encoded, bytes);
        for range in [0..4, 8..24, 24..32, 32..40, 40..48] {
            let mut damaged = bytes;
            damaged[range].fill(0);
            assert!(decode(&damaged).is_err());
        }
        for offset in [3, 4, 7] {
            let mut damaged = bytes;
            damaged[offset] = 2;
            assert!(decode(&damaged).is_err());
        }
        for length in [0, 47, 49] {
            assert!(decode(&[0; 49][..length]).is_err());
        }
    }
}
