use std::cell::RefCell;
use std::collections::BTreeMap;
use std::sync::{Arc, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::Value;

use super::{Shared, Worker};

thread_local! {
    static PAYLOADS: RefCell<BTreeMap<&'static str, Value>> =
        const { RefCell::new(BTreeMap::new()) };
}

pub(super) fn clear_payloads() {
    PAYLOADS.with(|payloads| payloads.borrow_mut().clear());
}

pub(super) fn payload(name: &'static str, payload: &serde_json::Value) {
    if !persistence::work::ingestion::is_active() {
        return;
    }
    assert!(serde_json::to_vec(payload).unwrap().len() <= 8 * 1024 * 1024);
    PAYLOADS.with(|payloads| {
        let mut payloads = payloads.borrow_mut();
        assert!(payloads.len() < 8);
        payloads.insert(name, payload.clone());
    });
}

pub(super) fn take_payloads() -> std::collections::BTreeMap<&'static str, serde_json::Value> {
    PAYLOADS.with(|payloads| std::mem::take(&mut *payloads.borrow_mut()))
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stage {
    Reading,
    Pending,
}

pub(super) struct Gate {
    stage: Option<Stage>,
    entered: Option<u64>,
    released: bool,
    finished: bool,
    interrupted: bool,
}

pub(crate) struct Hold {
    shared: Arc<Shared>,
}

impl Worker {
    pub(crate) fn frozen_payloads(
        &self,
        output: &mut std::collections::BTreeMap<&'static str, serde_json::Value>,
    ) {
        assert!(output.is_empty());
        output.clone_from(&self.mailbox().unwrap().taken_payloads);
    }

    pub(crate) fn hold_measurement_stage(&self, stage: Stage) -> Hold {
        let mut slot = self.shared.measurement_gate.lock().unwrap();
        assert!(slot.is_none());
        *slot = Some(Gate {
            stage: Some(stage),
            entered: None,
            released: false,
            finished: false,
            interrupted: false,
        });
        Hold {
            shared: Arc::clone(&self.shared),
        }
    }

    pub(crate) fn last_output_complete(&self) -> bool {
        self.shared
            .measurement_taken
            .load(std::sync::atomic::Ordering::Acquire)
    }
}

impl Hold {
    pub(crate) fn entered(&self) -> u64 {
        self.wait(|gate| gate.entered.is_some())
            .as_ref()
            .unwrap()
            .entered
            .unwrap()
    }

    pub(crate) fn observed_interruption(&self) -> bool {
        self.wait(|gate| gate.finished)
            .as_ref()
            .unwrap()
            .interrupted
    }

    pub(crate) fn release(&self) {
        let mut slot = self.shared.measurement_gate.lock().unwrap();
        slot.as_mut().unwrap().released = true;
        self.shared.measurement_wake.notify_all();
    }

    fn wait(&self, mut ready: impl FnMut(&Gate) -> bool) -> MutexGuard<'_, Option<Gate>> {
        let slot = self.shared.measurement_gate.lock().unwrap();
        let outcome = self
            .shared
            .measurement_wake
            .wait_timeout_while(slot, Duration::from_secs(120), |slot| {
                !slot.as_ref().is_some_and(&mut ready)
            })
            .unwrap();
        assert!(
            outcome.0.as_ref().is_some_and(ready),
            "worker checkpoint timed out"
        );
        outcome.0
    }
}

impl Drop for Hold {
    fn drop(&mut self) {
        let locked = self.shared.measurement_gate.lock();
        // A wait that failed panicked while it held the gate, which poisons its lock, and that
        // panic fails the test. A second one here, raised as the first unwinds, would abort the
        // whole test binary and hide every other test's result. Outside an unwind, a poisoned
        // gate is the worker's checkpoint failing on its own thread, which only this panic
        // reports to the test.
        let mut slot = if std::thread::panicking() {
            locked.unwrap_or_else(PoisonError::into_inner)
        } else {
            locked.expect("a panic poisoned the measurement gate")
        };

        *slot = None;
        self.shared.measurement_wake.notify_all();
    }
}

pub(super) fn checkpoint(shared: &Shared, generation: u64, stage: Stage) {
    let mut slot = shared.measurement_gate.lock().unwrap();
    let Some(gate) = slot.as_mut() else {
        return;
    };
    if gate.stage != Some(stage) {
        return;
    }

    gate.stage = None;
    gate.entered = Some(generation);
    shared.measurement_wake.notify_all();
    let outcome = shared
        .measurement_wake
        .wait_timeout_while(slot, Duration::from_secs(120), |slot| {
            slot.as_ref().is_some_and(|gate| {
                if gate.entered != Some(generation) {
                    return false;
                }

                !gate.released
            })
        })
        .unwrap();
    assert!(
        outcome.0.as_ref().is_none_or(|gate| {
            if gate.entered != Some(generation) {
                return true;
            }

            gate.released
        }),
        "worker checkpoint was not released"
    );
}

pub(super) fn finished(shared: &Shared, generation: u64) {
    if let Some(gate) = shared.measurement_gate.lock().unwrap().as_mut() {
        if gate.entered == Some(generation) {
            gate.finished = true;
            shared.measurement_wake.notify_all();
        }
    }
}

/// Records that a read of the Store gave up its generation once a newer request cancelled it.
pub(super) fn abandoned(shared: &Shared, generation: u64) {
    if let Some(gate) = shared.measurement_gate.lock().unwrap().as_mut() {
        if gate.entered == Some(generation) {
            gate.interrupted = true;
            gate.finished = true;
            shared.measurement_wake.notify_all();
        }
    }
}

pub(super) fn interrupted(shared: &Shared, generation: u64, error: &persistence::Error) {
    if !matches!(error, persistence::Error::Io(error)
        if error.kind() == std::io::ErrorKind::Interrupted)
    {
        return;
    }

    if let Some(gate) = shared.measurement_gate.lock().unwrap().as_mut() {
        if gate.entered == Some(generation) {
            gate.interrupted = true;
        }
    }
}

#[test]
fn a_hold_let_go_as_its_test_panics_clears_the_gate() {
    // Goal: a hold let go as its test's panic unwinds clears the gate without a second panic,
    // since that panic, raised as the first one unwinds, aborts every test of the binary rather
    // than failing one.
    // Method: let a hold over a gate a panic poisoned go as a second panic unwinds, as a failed
    // wait's does, then read the gate.
    let hold = poisoned_hold();
    let shared = Arc::clone(&hold.shared);

    let failed_test = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _hold = hold;
        panic!("worker checkpoint timed out");
    }));

    assert!(failed_test.is_err());
    assert!(
        shared
            .measurement_gate
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .is_none()
    );
}

#[test]
fn a_hold_let_go_outside_a_panic_fails_over_a_poisoned_gate() {
    // Goal: a hold let go outside a panic, over a gate a panic poisoned, panics in turn, since a
    // worker's checkpoint that was never released fails only on the worker's thread, and the
    // hold's panic is what fails the test that held it.
    // Method: let a hold over a gate a panic poisoned go while nothing panics.
    let hold = poisoned_hold();

    let let_go = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || drop(hold)));

    assert!(let_go.is_err());
}

/// A hold of the reading stage over a gate whose lock a panic poisoned while it held the gate,
/// as a failed wait or a checkpoint never released leaves it.
fn poisoned_hold() -> Hold {
    let shared = Arc::new(Shared::new());
    *shared.measurement_gate.lock().unwrap() = Some(Gate {
        stage: Some(Stage::Reading),
        entered: None,
        released: false,
        finished: false,
        interrupted: false,
    });
    let poisoned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _gate = shared.measurement_gate.lock().unwrap();
        panic!("the gate's holder failed");
    }));
    assert!(poisoned.is_err());
    assert!(shared.measurement_gate.is_poisoned());

    Hold { shared }
}
