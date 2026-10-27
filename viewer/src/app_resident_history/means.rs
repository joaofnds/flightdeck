use std::cmp::Ordering;

use persistence::{Error, Result};
use serde_json::{Value, json};

use crate::apps::AppKey;
use crate::live_app_response::Measure;

pub(super) struct Means {
    entries: Vec<Entry>,
    ordered: Vec<u16>,
    /// The slot after the last App found, where the next App of a reading usually is.
    next: usize,
    key_bytes: usize,
}

struct Entry {
    key: Key,
    sum: u128,
    samples: u32,
}

struct Key {
    bundle: bool,
    bytes: Box<[u8]>,
}

#[derive(Clone, Copy)]
struct Comparison {
    observed: u16,
    selected: u16,
}

pub(super) const APPS_MAX: usize = 24_000;
pub(super) const KEY_BYTES_MAX: usize = 8 * 1024 * 1024;
pub(super) const BYTES_MAX: usize = 10 * 1024 * 1024;
const _: () = {
    assert!(size_of::<Entry>() <= 64);
    assert!(APPS_MAX == crate::live_app_response::APPS_MAX);
    assert!(APPS_MAX < 65_536);
    assert!(KEY_BYTES_MAX + APPS_MAX * (size_of::<Entry>() + size_of::<u16>()) < BYTES_MAX);
};

impl Means {
    pub(super) fn new() -> Self {
        Self {
            entries: Vec::with_capacity(APPS_MAX),
            ordered: Vec::with_capacity(APPS_MAX),
            next: 0,
            key_bytes: 0,
        }
    }

    pub(super) fn observe(&mut self, key: &AppKey<'_>, measure: &Measure) -> Result<()> {
        let found = crate::apps::search_ordered(&self.ordered, self.next, key, |index| {
            self.entries[usize::from(index)].key.view()
        });
        let index = match found {
            Ok(position) => usize::from(self.ordered[position]),
            Err(position) => self.observe_insert(key, position)?,
        };
        let (Ok(position) | Err(position)) = found;
        self.next = position + 1;

        if let Some(known) = measure.measured() {
            let entry = &mut self.entries[index];
            if entry.samples == 4096 {
                return Err(Error::Bound("resident history mean samples"));
            }

            entry.sum += u128::from(known);
            entry.samples += 1;
        }

        Ok(())
    }

    fn observe_insert(&mut self, key: &AppKey<'_>, position: usize) -> Result<usize> {
        if self.entries.len() == APPS_MAX {
            return Err(Error::Bound("resident history context Apps"));
        }

        let bytes = match key {
            AppKey::Bundle(bytes) | AppKey::Executable(bytes) => bytes,
        };
        if bytes.len() > collector::ring::PROGRAM_PATH_BYTES_MAX {
            return Err(Error::Bound("resident history key"));
        }
        if bytes.len() > KEY_BYTES_MAX - self.key_bytes {
            return Err(Error::Bound("resident history context key bytes"));
        }

        let index = self.entries.len();
        self.entries.push(Entry {
            key: Key {
                bundle: matches!(key, AppKey::Bundle(_)),
                bytes: (*bytes).into(),
            },
            sum: 0,
            samples: 0,
        });
        self.ordered.insert(
            position,
            u16::try_from(index).map_err(|_| Error::Bound("resident history context index"))?,
        );
        self.key_bytes += bytes.len();
        assert!(self.bytes() < BYTES_MAX);

        Ok(index)
    }

    pub(super) fn choose(&self, operation: &crate::cancellation::Operation) -> Result<Vec<u16>> {
        let mut cohort = Vec::with_capacity(8);
        for index in &self.ordered {
            operation.check()?;
            if self.entries[usize::from(*index)].samples == 0 {
                continue;
            }

            let position = cohort.partition_point(|other| {
                self.compare(Comparison {
                    observed: *other,
                    selected: *index,
                }) != Ordering::Greater
            });
            if position < 8 {
                if cohort.len() == 8 {
                    cohort.truncate(7);
                }
                cohort.insert(position, *index);
            }
        }

        Ok(cohort)
    }

    fn compare(&self, comparison: Comparison) -> Ordering {
        let observed = &self.entries[usize::from(comparison.observed)];
        let selected = &self.entries[usize::from(comparison.selected)];
        let before = observed.sum * u128::from(selected.samples);
        let after = selected.sum * u128::from(observed.samples);

        after
            .cmp(&before)
            .then_with(|| observed.key.view().cmp(&selected.key.view()))
    }

    pub(super) fn key(&self, index: u16) -> AppKey<'_> {
        self.entries[usize::from(index)].key.view()
    }

    pub(super) fn json(&self, index: u16) -> Value {
        let entry = &self.entries[usize::from(index)];
        let mut display = String::with_capacity(entry.key.bytes.len() * 4);
        crate::byte_display::append(&mut display, &entry.key.bytes);
        let digits = b"0123456789abcdef";
        let bytes_hex: String = entry
            .key
            .bytes
            .iter()
            .flat_map(|byte| {
                [
                    char::from(digits[usize::from(byte >> 4)]),
                    char::from(digits[usize::from(byte & 15)]),
                ]
            })
            .collect();

        json!({"key":{"kind":if entry.key.bundle {"bundle"} else {"executable"},
            "bytes_hex":bytes_hex,"display":display},"sum_bytes":entry.sum.to_string(),
            "samples":entry.samples})
    }

    pub(super) fn count(&self) -> usize {
        self.entries.len()
    }

    pub(super) fn key_bytes(&self) -> usize {
        self.key_bytes
    }

    fn bytes(&self) -> usize {
        size_of::<Self>()
            + self.entries.capacity() * size_of::<Entry>()
            + self.ordered.capacity() * size_of::<u16>()
            + self.key_bytes
    }
}

impl Key {
    fn view(&self) -> AppKey<'_> {
        if self.bundle {
            AppKey::Bundle(&self.bytes)
        } else {
            AppKey::Executable(&self.bytes)
        }
    }
}
