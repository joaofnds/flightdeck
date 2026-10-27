use std::sync::{Arc, MutexGuard};
use std::time::Duration;

use super::{Shared, Worker};

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
        *self.shared.measurement_gate.lock().unwrap() = None;
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
