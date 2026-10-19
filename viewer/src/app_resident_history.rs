//! Bounded resident gauges across the selected live and saved App context.
use persistence::{Error, Result};
use serde_json::Value;

use super::LiveSession;

impl LiveSession {
    pub fn app_resident_history(&self) -> Result<Value> {
        self.operation.run(|| {
            let mut selected = None;
            self.with_selected_apps(|sample| {
                selected = Some(sample.selected);
                Ok(Value::Null)
            })?;
            let selected = selected.ok_or(Error::Bound("resident history selection"))?;

            self.app_history
                .resident_response(&selected, self.history.as_ref(), &self.operation)
        })
    }
}

#[path = "app_resident_history/means.rs"]
mod means;
#[path = "app_resident_history/reduce.rs"]
mod reduce;
#[path = "app_resident_history/saved.rs"]
mod saved;
pub(crate) use reduce::{AppReading, Capture, Interval, Reduction, Source, consecutive, identity};
pub(crate) use saved::capture;

pub(crate) fn has_resident(measure: &crate::live_app_response::Measure) -> bool {
    if measure.known > 0 {
        return true;
    }
    if measure.measured_members > 0 {
        return true;
    }
    if measure.unavailable_members > 0 {
        return true;
    }
    measure.overflowed
}

#[cfg(test)]
#[path = "app_resident_history/tests.rs"]
mod tests;
