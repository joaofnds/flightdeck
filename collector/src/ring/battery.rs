//! A fixed V4 tail, after the complete original dictionary arena capacity.

use std::ops::Range;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::battery::BatteryReading;
use crate::u64_from_usize;

pub(super) const MAGIC: u64 = u64::from_be_bytes(*b"FDBATT01");
pub(super) const HEADER_WORDS: usize = 4;
const VERSION: u64 = 1;
const RECORD_WORDS: usize = 12;
const FIELD_COUNT: usize = RECORD_WORDS - 2;
const PRESENT: u64 = 1;
const FLAGS: u64 = (1 << (FIELD_COUNT + 1)) - 1;
const _: () = assert!(FIELD_COUNT == 10);
const _: () = assert!(HEADER_WORDS == 4);

#[derive(Debug, Clone)]
pub(super) struct BatteryLayout {
    start: usize,
    slot_count: usize,
    end: usize,
}

impl BatteryLayout {
    pub(super) fn plan(start: usize, slot_count: u64) -> Option<Self> {
        if slot_count == 0 {
            return None;
        }

        let slot_count = usize::try_from(slot_count).ok()?;
        let end = start
            .checked_add(HEADER_WORDS)?
            .checked_add(slot_count.checked_mul(RECORD_WORDS)?)?;
        let bytes = end.checked_mul(size_of::<u64>())?;
        i64::try_from(bytes).ok()?;

        Some(Self {
            start,
            slot_count,
            end,
        })
    }

    pub(super) fn word_count(&self) -> usize {
        self.end
    }

    pub(super) fn record(&self, publication: u64) -> Range<usize> {
        assert!(publication > 0);

        let index = (publication - 1) % u64_from_usize(self.slot_count);
        let index = match usize::try_from(index) {
            Ok(index) => index,
            Err(error) => unreachable!("{error}"),
        };
        let start = self.start + HEADER_WORDS + index * RECORD_WORDS;

        start..start + RECORD_WORDS
    }

    pub(super) fn store_header(&self, words: &[AtomicU64]) {
        let header = [
            MAGIC,
            VERSION,
            u64_from_usize(self.slot_count),
            u64_from_usize(RECORD_WORDS),
        ];

        for (index, value) in header.iter().enumerate() {
            words[self.start + index].store(*value, Ordering::Relaxed);
        }
    }

    pub(super) fn admit(
        header: &[AtomicU64],
        start: usize,
        slot_count: u64,
        size_bytes: usize,
    ) -> Option<Self> {
        if header.len() < HEADER_WORDS {
            return None;
        }

        let expected = [MAGIC, VERSION, slot_count, u64_from_usize(RECORD_WORDS)];

        for (word, expected) in header.iter().zip(expected) {
            if word.load(Ordering::Relaxed) != expected {
                return None;
            }
        }

        let layout = Self::plan(start, slot_count)?;
        if layout.end.checked_mul(size_of::<u64>())? > size_bytes {
            return None;
        }

        Some(layout)
    }
}

pub(super) fn store(words: &[AtomicU64], reading: Option<&BatteryReading>) {
    assert_eq!(words.len(), RECORD_WORDS);
    let mut encoded = [0; RECORD_WORDS];

    if let Some(reading) = reading {
        encoded[0] = PRESENT;
        encoded[1] = reading.source_continuous_ns;
        let values = [
            reading.charge_current,
            reading.charge_maximum,
            reading.full_charge_capacity,
            reading.design_capacity,
            reading.cycle_count,
            reading.charging.map(u64::from),
            reading.external_power.map(u64::from),
            reading.voltage_mv,
            reading
                .current_ma
                .map(|value| u64::from_ne_bytes(value.to_ne_bytes())),
            reading.adapter_watts,
        ];
        for (index, value) in values.iter().enumerate() {
            if let Some(value) = value {
                encoded[0] |= 1 << (index + 1);
                encoded[index + 2] = *value;
            }
        }
    }

    for (word, value) in words.iter().zip(encoded) {
        word.store(value, Ordering::Relaxed);
    }
}

/// Invalid encodings fail; a valid unavailable reading is `Ok(None)`.
pub(super) fn load(words: &[AtomicU64]) -> Result<Option<BatteryReading>, std::io::ErrorKind> {
    assert_eq!(words.len(), RECORD_WORDS);
    let encoded: [u64; RECORD_WORDS] =
        std::array::from_fn(|index| words[index].load(Ordering::Relaxed));
    let flags = encoded[0];
    if flags & !FLAGS != 0 {
        return Err(std::io::ErrorKind::InvalidData);
    }

    if flags & PRESENT == 0 {
        if encoded.iter().any(|value| *value != 0) {
            return Err(std::io::ErrorKind::InvalidData);
        }

        return Ok(None);
    }

    for index in 0..FIELD_COUNT {
        if flags & (1 << (index + 1)) == 0 {
            if encoded[index + 2] != 0 {
                return Err(std::io::ErrorKind::InvalidData);
            }
        }
    }

    if encoded[7] > 1 {
        return Err(std::io::ErrorKind::InvalidData);
    }

    if encoded[8] > 1 {
        return Err(std::io::ErrorKind::InvalidData);
    }

    let field = |index: usize| (flags & (1 << (index - 1)) != 0).then_some(encoded[index]);

    Ok(Some(BatteryReading {
        source_continuous_ns: encoded[1],
        charge_current: field(2),
        charge_maximum: field(3),
        full_charge_capacity: field(4),
        design_capacity: field(5),
        cycle_count: field(6),
        charging: field(7).map(|value| value == 1),
        external_power: field(8).map(|value| value == 1),
        voltage_mv: field(9),
        current_ma: field(10).map(|value| i64::from_ne_bytes(value.to_ne_bytes())),
        adapter_watts: field(11),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header() -> [AtomicU64; HEADER_WORDS] {
        [MAGIC, VERSION, 2, u64_from_usize(RECORD_WORDS)].map(AtomicU64::new)
    }

    #[test]
    fn retains_every_combination_of_independent_optional_fields() {
        // Goal: each presence bit preserves its lane without borrowing another field's state.
        // Method: round trip all 1024 field-presence masks with extreme integer values.
        let words = std::array::from_fn::<_, RECORD_WORDS, _>(|_| AtomicU64::new(0));
        for mask in 0_u64..1024 {
            let present = |index: u64| mask & (1 << index) != 0;
            let reading = BatteryReading {
                source_continuous_ns: u64::MAX,
                charge_current: present(0).then_some(0),
                charge_maximum: present(1).then_some(u64::MAX),
                full_charge_capacity: present(2).then_some(0),
                design_capacity: present(3).then_some(u64::MAX),
                cycle_count: present(4).then_some(0),
                charging: present(5).then_some(false),
                external_power: present(6).then_some(true),
                voltage_mv: present(7).then_some(u64::MAX),
                current_ma: present(8).then_some(i64::MIN),
                adapter_watts: present(9).then_some(0),
            };

            store(&words, Some(&reading));

            assert_eq!(load(&words), Ok(Some(reading)), "presence mask {mask}");
        }
    }

    #[test]
    fn refuses_a_truncated_recognized_battery_header() {
        // Goal: a recognized magic alone never admits a tail.
        // Method: present only three of its four immutable header words.
        assert!(BatteryLayout::admit(&header()[..3], 314, 2, 16384).is_none());
    }

    #[test]
    fn refuses_truncated_battery_records() {
        // Goal: a valid header cannot map records beyond object size.
        // Method: shorten the available object by one byte.
        let size = (314 + HEADER_WORDS + 2 * RECORD_WORDS) * 8;
        assert!(BatteryLayout::admit(&header(), 314, 2, size - 1).is_none());
    }

    #[test]
    fn refuses_overflowing_battery_tail_offsets() {
        // Goal: malformed bounds never wrap the mapping length.
        // Method: plan from the maximum memory offset.
        assert!(BatteryLayout::plan(usize::MAX, 2).is_none());
    }

    #[test]
    fn refuses_overflowing_battery_slot_counts() {
        // Goal: untrusted slot counts cannot overflow a tail size.
        // Method: request the maximum wire count.
        assert!(BatteryLayout::plan(314, u64::MAX).is_none());
    }
}
