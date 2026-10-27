//! Opt-in calling-thread work counts. A scope measures real subsystem entry points; outside
//! a scope the recorder leaves state untouched. A scope cannot move to another thread.
use std::cell::Cell;
use std::marker::PhantomData;
use std::rc::Rc;

pub mod ingestion;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub staged_ticks: u64,
    pub frames: u64,
    pub sql_plans: u64,
    pub collections: u64,
    pub string_cells: u64,
}

thread_local! {
    static COUNTS: Cell<Option<Counts>> = const { Cell::new(None) };
}

pub struct Observation {
    previous: Option<Counts>,
    thread: PhantomData<Rc<()>>,
}

/// Starts one operation's accounting. Drop restores the prior state, including on unwind.
#[must_use]
pub fn observe() -> Observation {
    Observation {
        previous: COUNTS.replace(Some(Counts::default())),
        thread: PhantomData,
    }
}

impl Observation {
    #[must_use]
    pub fn counts(&self) -> Counts {
        COUNTS
            .get()
            .unwrap_or_else(|| unreachable!("observation owns an active count scope"))
    }
}

impl Drop for Observation {
    fn drop(&mut self) {
        COUNTS.set(self.previous);
    }
}

pub fn record(update: impl FnOnce(&mut Counts)) {
    if let Some(mut value) = COUNTS.get() {
        update(&mut value);
        COUNTS.set(Some(value));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn leaves_counts_inactive_outside_an_observation_and_restores_nested_scopes() {
        // Goal: ordinary operations accumulate no telemetry and nested observations stay local.
        // Method: record outside, inside and after a nested scope on this test's own thread.
        assert!(COUNTS.get().is_none());
        record(|counts| counts.frames += 1);
        assert!(COUNTS.get().is_none());
        let outer = observe();
        record(|counts| counts.frames += 1);
        {
            let inner = observe();
            record(|counts| counts.frames += 2);
            assert_eq!(inner.counts().frames, 2);
        }
        assert_eq!(outer.counts().frames, 1);
        drop(outer);
        assert!(COUNTS.get().is_none());
    }
}
