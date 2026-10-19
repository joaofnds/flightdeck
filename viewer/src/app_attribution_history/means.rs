use std::cmp::Ordering;

use persistence::{Error, Result};
use serde_json::{Value, json};

use crate::apps::AppKey;
use crate::cancellation::Operation;

pub(super) struct Means {
    entries: Vec<Entry>,
    ordered: Vec<u16>,
    /// The slot after the last App found, where the next App of a reading usually is.
    next: usize,
    key_bytes: usize,
}

struct Entry {
    key: Key,
    mean: Mean,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Key {
    bundle: bool,
    bytes: Box<[u8]>,
}

struct Mean {
    amount: u128,
    elapsed_ns: u128,
}

#[derive(Clone, Copy)]
struct Comparison<'mean> {
    before: &'mean Mean,
    after: &'mean Mean,
}

const APPS_MAX: usize = 24_000;
const KEY_BYTES_MAX: usize = 8 * 1024 * 1024;
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

    pub(super) fn observe(&mut self, app: &super::Reading<'_>, elapsed: Option<u64>) -> Result<()> {
        let found = crate::apps::search_ordered(&self.ordered, self.next, &app.key, |index| {
            self.entries[usize::from(index)].key.view()
        });
        let index = match found {
            Ok(position) => usize::from(self.ordered[position]),
            Err(position) => self.observe_insert(&app.key, position)?,
        };
        let (Ok(position) | Err(position)) = found;
        self.next = position + 1;

        let Some(elapsed) = elapsed.filter(|elapsed| *elapsed > 0) else {
            return Ok(());
        };

        let Some(amount) = app.measure.measured() else {
            return Ok(());
        };

        let mean = &mut self.entries[index].mean;
        mean.amount += u128::from(amount);
        mean.elapsed_ns += u128::from(elapsed);

        Ok(())
    }

    fn observe_insert(&mut self, key: &AppKey<'_>, position: usize) -> Result<usize> {
        if self.entries.len() == APPS_MAX {
            return Err(Error::Bound("attribution context Apps"));
        }

        let bytes = match key {
            AppKey::Bundle(bytes) | AppKey::Executable(bytes) => bytes,
        };
        if bytes.len() > collector::ring::PROGRAM_PATH_BYTES_MAX {
            return Err(Error::Bound("attribution context key"));
        }

        if bytes.len() > KEY_BYTES_MAX - self.key_bytes {
            return Err(Error::Bound("attribution context key bytes"));
        }

        let index = self.entries.len();
        self.entries.push(Entry {
            key: Key {
                bundle: matches!(key, AppKey::Bundle(_)),
                bytes: (*bytes).into(),
            },
            mean: Mean {
                amount: 0,
                elapsed_ns: 0,
            },
        });
        self.ordered.insert(
            position,
            u16::try_from(index).map_err(|_| Error::Bound("attribution context index"))?,
        );
        self.key_bytes += bytes.len();
        assert!(self.storage_bytes() < BYTES_MAX);

        Ok(index)
    }

    pub(super) fn choose(&self, operation: &Operation) -> Result<Vec<Key>> {
        let mut chosen: Vec<u16> = Vec::with_capacity(8);
        for index in &self.ordered {
            operation.check()?;
            let entry = &self.entries[usize::from(*index)];
            if entry.mean.elapsed_ns == 0 {
                continue;
            }
            let position = chosen.partition_point(|other| {
                let other = &self.entries[usize::from(*other)];
                compare(Comparison {
                    before: &other.mean,
                    after: &entry.mean,
                })
                .reverse()
                .then_with(|| other.key.view().cmp(&entry.key.view()))
                    != Ordering::Greater
            });
            if position < 8 {
                if chosen.len() == 8 {
                    chosen.truncate(7);
                }
                chosen.insert(position, *index);
            }
        }
        Ok(chosen
            .into_iter()
            .map(|index| self.entries[usize::from(index)].key.clone())
            .collect())
    }

    pub(super) fn count(&self) -> usize {
        self.entries.len()
    }
    pub(super) fn bytes(&self) -> usize {
        self.key_bytes
    }

    fn storage_bytes(&self) -> usize {
        size_of::<Self>()
            + self.entries.capacity() * size_of::<Entry>()
            + self.ordered.capacity() * size_of::<u16>()
            + self.key_bytes
    }
}

impl Key {
    pub(super) fn view(&self) -> AppKey<'_> {
        if self.bundle {
            AppKey::Bundle(&self.bytes)
        } else {
            AppKey::Executable(&self.bytes)
        }
    }

    pub(super) fn json(&self) -> Value {
        let mut display = String::with_capacity(self.bytes.len() * 4);
        crate::byte_display::append(&mut display, &self.bytes);
        let digits = b"0123456789abcdef";
        let bytes_hex: String = self
            .bytes
            .iter()
            .flat_map(|byte| {
                [
                    char::from(digits[usize::from(byte >> 4)]),
                    char::from(digits[usize::from(byte & 15)]),
                ]
            })
            .collect();
        json!({"key":{"kind":if self.bundle {"bundle"} else {"executable"},
            "bytes_hex":bytes_hex,"display":display}})
    }
}

fn compare(comparison: Comparison<'_>) -> Ordering {
    // Continued fractions compare exact weighted means without overflowing a cross product.
    let (mut first, mut left) = (comparison.before.amount, comparison.before.elapsed_ns);
    let (mut second, mut right) = (comparison.after.amount, comparison.after.elapsed_ns);
    let mut reversed = false;
    for _step in 0..256 {
        let ordering = (first / left).cmp(&(second / right));
        if ordering != Ordering::Equal {
            return if reversed {
                ordering.reverse()
            } else {
                ordering
            };
        }
        let (first_rest, second_rest) = (first % left, second % right);
        if [first_rest == 0, second_rest == 0].contains(&true) {
            let ordering = first_rest.cmp(&second_rest);
            return if reversed {
                ordering.reverse()
            } else {
                ordering
            };
        }
        (first, left) = (left, first_rest);
        (second, right) = (right, second_rest);
        reversed = !reversed;
    }
    unreachable!("Euclid's algorithm terminates within 256 steps for u128 inputs")
}
