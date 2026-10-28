//! One finite input namespace owns reusable captured names until that input retires.
use std::cmp::Ordering;
use std::sync::Arc;

use super::ProcessName;
use crate::byte_budget::{ByteBudget, ByteReservation};
use crate::live_app_response::{Bytes, Sample};
use crate::store::Missing;

#[cfg(test)]
#[path = "payload_name_work.rs"]
mod work;
#[cfg(test)]
pub(in crate::store) use work::Trace as NameTrace;
#[cfg(test)]
use work::{Comparison, NameWork};

pub(in crate::store) struct ProcessNames {
    entries: Vec<Arc<ProcessName>>,
    reservation: ByteReservation,
    budget: Arc<ByteBudget>,
    #[cfg(test)]
    work: std::cell::Cell<NameWork>,
}

pub(in crate::store) struct NameAdditions {
    entries: Vec<Arc<ProcessName>>,
    reservation: ByteReservation,
    base_count: usize,
    budget: Arc<ByteBudget>,
    #[cfg(test)]
    work: Option<PendingWork>,
}

#[cfg(test)]
#[derive(Clone, Copy)]
struct PendingWork {
    observation: crate::live_app_response::Observation,
    before: NameWork,
}

impl ProcessNames {
    pub(in crate::store) fn new(count: usize, budget: &Arc<ByteBudget>) -> Result<Self, Missing> {
        let bytes = count
            .checked_mul(size_of::<Arc<ProcessName>>())
            .ok_or(Missing::Budget)?;
        let reservation = budget.reserve(bytes).ok_or(Missing::Budget)?;
        let entries = Vec::with_capacity(count);
        if entries.capacity() != count {
            return Err(Missing::Budget);
        }

        Ok(Self {
            entries,
            reservation,
            budget: Arc::clone(budget),
            #[cfg(test)]
            work: std::cell::Cell::new(NameWork::default()),
        })
    }

    pub(in crate::store) fn budget(&self) -> &Arc<ByteBudget> {
        &self.budget
    }

    pub(in crate::store) fn additions(&self, sample: &Sample) -> Result<NameAdditions, Missing> {
        #[cfg(test)]
        let before = self.work.get();
        let (mut names, scratch) = self.additions_scratch(sample)?;
        Self::additions_sort(
            &mut names,
            #[cfg(test)]
            &self.work,
        );
        let missing = names
            .iter()
            .filter(|name| self.find(name).is_none())
            .count();
        if missing > self.entries.capacity() - self.entries.len() {
            return Err(Missing::Budget);
        }

        #[cfg(test)]
        self.record_work(|work| {
            work.distinct_names += u64::try_from(names.len()).unwrap();
            work.missing_names += u64::try_from(missing).unwrap();
        });
        let bytes = missing
            .checked_mul(size_of::<Arc<ProcessName>>())
            .ok_or(Missing::Budget)?;
        let reservation = self.budget.reserve(bytes).ok_or(Missing::Budget)?;
        #[cfg(test)]
        self.record_work(|work| {
            work.pending_bytes += u64::try_from(bytes).unwrap();
            work.allocation_requests += u64::from(bytes > 0);
            work.allocation_bytes += u64::try_from(bytes).unwrap();
        });
        let mut entries = Vec::with_capacity(missing);
        if entries.capacity() != missing {
            return Err(Missing::Budget);
        }

        self.additions_capture(&names, &mut entries)?;
        drop(names);
        drop(scratch);

        Ok(NameAdditions {
            entries,
            reservation,
            base_count: self.entries.len(),
            budget: Arc::clone(&self.budget),
            #[cfg(test)]
            work: Some(PendingWork {
                observation: sample.selected,
                before,
            }),
        })
    }

    fn additions_scratch<'sample>(
        &self,
        sample: &'sample Sample,
    ) -> Result<(Vec<&'sample Bytes>, ByteReservation), Missing> {
        let count = sample
            .processes()
            .iter()
            .filter_map(|row| row.executable_name.as_ref())
            .count();
        let bytes = count
            .checked_mul(size_of::<&Bytes>())
            .ok_or(Missing::Budget)?;
        let scratch = self.budget.reserve(bytes).ok_or(Missing::Budget)?;
        #[cfg(test)]
        self.record_work(|work| {
            work.named_rows += u64::try_from(count).unwrap();
            work.scratch_bytes += u64::try_from(bytes).unwrap();
            work.allocation_requests += u64::from(bytes > 0);
            work.allocation_bytes += u64::try_from(bytes).unwrap();
        });
        let mut names = Vec::with_capacity(count);
        if names.capacity() != count {
            return Err(Missing::Budget);
        }

        names.extend(
            sample
                .processes()
                .iter()
                .filter_map(|row| row.executable_name.as_ref()),
        );

        Ok((names, scratch))
    }

    fn additions_sort(names: &mut Vec<&Bytes>, #[cfg(test)] work: &std::cell::Cell<NameWork>) {
        names.sort_unstable_by(|left, right| {
            #[cfg(test)]
            work::comparison(work, Comparison::ScratchSort, left, right);
            compare(left, right)
        });
        names.dedup_by(|left, right| {
            #[cfg(test)]
            work::comparison(work, Comparison::ScratchDedup, left, right);
            compare(left, right).is_eq()
        });
    }

    fn additions_capture(
        &self,
        names: &[&Bytes],
        entries: &mut Vec<Arc<ProcessName>>,
    ) -> Result<(), Missing> {
        for name in names {
            if self.find(name).is_none() {
                let captured = ProcessName::capture(name, &self.budget)?;
                #[cfg(test)]
                self.record_work(|work| {
                    work.new_names += 1;
                    work.allocation_requests += 1
                        + u64::from(captured.bytes.capacity() > 0)
                        + u64::from(captured.display.capacity() > 0);
                    work.allocation_bytes += u64::try_from(captured.reserved_bytes()).unwrap();
                });
                entries.push(captured);
            }
        }

        Ok(())
    }

    #[cfg(test)]
    pub(in crate::store) fn work_snapshot(&self) -> NameWork {
        self.work.get()
    }

    #[cfg(test)]
    pub(in crate::store) fn record_receipt(
        &self,
        observation: crate::live_app_response::Observation,
        before: NameWork,
        committed: bool,
    ) {
        work::record(&work::Receipt {
            observation,
            committed,
            index_count: self.entries.len(),
            index_capacity: self.entries.capacity(),
            index_bytes: self.reservation.bytes(),
            work: self.work.get().since(before),
        });
    }

    pub(in crate::store) fn name(
        &self,
        name: &Bytes,
        additions: &NameAdditions,
    ) -> Arc<ProcessName> {
        let held = self.find(name).or_else(|| {
            #[cfg(test)]
            self.record_work(|work| work.lookups += 1);
            match additions.entries.binary_search_by(|held| {
                #[cfg(test)]
                work::comparison(&self.work, Comparison::PendingSearch, held, name);
                compare(held, name)
            }) {
                Ok(index) => Some(&additions.entries[index]),
                Err(_) => None,
            }
        });
        Arc::clone(held.unwrap_or_else(|| unreachable!("planned captured name")))
    }

    pub(in crate::store) fn commit(&mut self, mut additions: NameAdditions) {
        assert!(Arc::ptr_eq(&self.budget, &additions.budget));
        assert_eq!(self.entries.len(), additions.base_count);
        assert!(additions.entries.len() <= self.entries.capacity() - self.entries.len());
        assert_eq!(
            self.reservation.bytes(),
            self.entries.capacity() * size_of::<Arc<ProcessName>>()
        );
        assert_eq!(
            additions.reservation.bytes(),
            additions.entries.capacity() * size_of::<Arc<ProcessName>>()
        );
        if !additions.entries.is_empty() {
            #[cfg(test)]
            self.record_work(|work| {
                work.sorts += 1;
                work.sorted_entries += u64::try_from(self.entries.len() + additions.entries.len())
                    .unwrap_or_else(|_| unreachable!("bounded name count"));
            });
            self.entries.append(&mut additions.entries);
            #[cfg(test)]
            let work = &self.work;
            self.entries.sort_unstable_by(|left, right| {
                #[cfg(test)]
                work::comparison(work, Comparison::AcceptedSort, left, right);
                compare(left, right)
            });
        }
        #[cfg(test)]
        if let Some(pending) = additions.work.take() {
            self.record_receipt(pending.observation, pending.before, true);
        }
    }

    #[cfg(test)]
    pub(in crate::store) fn observation(&self) -> (usize, String, usize) {
        (
            self.entries.len(),
            format!(
                "index_count={} index_capacity={} index_bytes={} work={:?}",
                self.entries.len(),
                self.entries.capacity(),
                self.reservation.bytes(),
                self.work.get()
            ),
            self.entries.iter().map(|name| name.reserved_bytes()).sum(),
        )
    }

    #[cfg(test)]
    fn record_work(&self, change: impl FnOnce(&mut NameWork)) {
        let mut work = self.work.get();
        change(&mut work);
        self.work.set(work);
    }

    fn find(&self, name: &Bytes) -> Option<&Arc<ProcessName>> {
        #[cfg(test)]
        self.record_work(|work| work.lookups += 1);
        match self.entries.binary_search_by(|held| {
            #[cfg(test)]
            work::comparison(&self.work, Comparison::AcceptedSearch, held, name);
            compare(held, name)
        }) {
            Ok(index) => Some(&self.entries[index]),
            Err(_) => None,
        }
    }
}

fn compare(left: &Bytes, right: &Bytes) -> Ordering {
    left.bytes
        .cmp(&right.bytes)
        .then_with(|| left.display.cmp(&right.display))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_capture_counts_all_comparison_phases_and_its_own_allocation_work() {
        // Goal: per-observation work includes scratch, accepted, pending and commit comparisons.
        // Method: capture a real two-name Sample twice under a separately admitted scalar trace.
        let sample = super::super::name_tests::native_sample();
        let budget = Arc::new(ByteBudget::new(64 * 1024));
        let observer = Arc::new(ByteBudget::new(64 * 1024));
        let trace = NameTrace::new(2, &observer);
        let mut names = ProcessNames::new(2, &budget).unwrap();

        let (first, additions) =
            super::super::Payload::capture_with_names(&sample, &names).unwrap();
        names.commit(additions);
        let (second, additions) =
            super::super::Payload::capture_with_names(&sample, &names).unwrap();
        names.commit(additions);
        let records = trace.finish();

        let first_work = records.values()[0].work;
        assert_eq!(records.values()[0].observation, sample.selected);
        assert!(records.values()[0].committed);
        assert_eq!(records.values()[0].index_count, 2);
        assert_eq!(records.values()[0].index_capacity, 2);
        assert_eq!(
            records.values()[0].index_bytes,
            2 * size_of::<Arc<ProcessName>>()
        );
        assert_eq!(first_work.named_rows, 2);
        assert_eq!(first_work.distinct_names, 2);
        assert_eq!(first_work.new_names, 2);
        assert_eq!(first_work.comparisons[2], 0);
        for index in [0, 1, 3, 4] {
            assert!(first_work.comparisons[index] > 0);
            assert!(first_work.logical_bytes[index] > 0);
        }
        assert_eq!(first_work.allocation_requests, 8);
        let second_work = records.values()[1].work;
        assert_eq!(second_work.new_names, 0);
        assert!(second_work.comparisons[2] > 0);
        assert_eq!(second_work.comparisons[3], 0);
        assert_eq!(second_work.comparisons[4], 0);
        assert_eq!(second_work.allocation_requests, 1);
        assert_eq!(second_work.pending_bytes, 0);
        println!("name_work_receipts={:?}", records.values());
        drop((first, second, names, records));
        assert_eq!(budget.used(), 0);
        assert_eq!(observer.used(), 0);
    }

    #[test]
    fn pending_names_cannot_transfer_their_charge_to_a_foreign_budget_index() {
        // Goal: the index commit guard binds pending owners to their original budget.
        // Method: submit additions from a real captured Sample to another admitted index.
        let sample = super::super::name_tests::native_sample();
        let original_budget = Arc::new(ByteBudget::new(64 * 1024));
        let other_budget = Arc::new(ByteBudget::new(64 * 1024));
        let original = ProcessNames::new(2, &original_budget).unwrap();
        let mut other = ProcessNames::new(2, &other_budget).unwrap();
        let additions = original.additions(&sample).unwrap();
        let original_index = original.reservation.bytes();
        let other_index = other.reservation.bytes();

        let refusal = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            other.commit(additions);
        }));

        assert!(refusal.is_err());
        assert_eq!(other.entries.len(), 0);
        assert_eq!(original_budget.used(), original_index);
        assert_eq!(other_budget.used(), other_index);
        drop((original, other));
        assert_eq!(original_budget.used(), 0);
        assert_eq!(other_budget.used(), 0);
    }

    #[test]
    fn names_with_equal_raw_bytes_keep_their_distinct_captured_display_spelling() {
        // Goal: lookup equality preserves the complete captured value.
        // Method: admit equal raw values with distinct captured display strings, then find both.
        let budget = Arc::new(ByteBudget::new(64 * 1024));
        let mut names = ProcessNames::new(2, &budget).unwrap();
        let first = Bytes {
            bytes: vec![0xff],
            display: String::from("first"),
        };
        let second = Bytes {
            bytes: vec![0xff],
            display: String::from("second"),
        };
        let reservation = budget.reserve(2 * size_of::<Arc<ProcessName>>()).unwrap();
        let entries = vec![
            ProcessName::capture(&first, &budget).unwrap(),
            ProcessName::capture(&second, &budget).unwrap(),
        ];
        let additions = NameAdditions {
            entries,
            reservation,
            base_count: 0,
            budget: Arc::clone(&budget),
            work: None,
        };

        names.commit(additions);

        assert_eq!(names.find(&first).unwrap().display, "first");
        assert_eq!(names.find(&second).unwrap().display, "second");
        assert!(!Arc::ptr_eq(
            names.find(&first).unwrap(),
            names.find(&second).unwrap()
        ));
        drop(names);
        assert_eq!(budget.used(), 0);
    }
}
