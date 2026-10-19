//! Bounded CPU, process-power and disk attribution over the selected App context.
use persistence::{Error, Result};
use serde_json::Value;

use crate::LiveSession;

#[path = "app_attribution_history/cache.rs"]
pub(crate) mod cache;
#[path = "app_attribution_history/means.rs"]
mod means;
#[path = "app_attribution_history/reduce.rs"]
mod reduce;
#[path = "app_attribution_history/sums.rs"]
pub(crate) mod sums;
pub(crate) use reduce::{Capture, Reading, Reduction};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AppAttributionMetric {
    Cpu,
    Power,
    Disk,
}

pub struct AppAttributionHistories {
    pub cpu: Result<Value>,
    pub power: Result<Value>,
    pub disk: Result<Value>,
}

pub const APP_ATTRIBUTION_HISTORIES_RESPONSE_BYTES_MAX: usize =
    3 * crate::live_app_response::RESPONSE_BYTES_MAX;

const _: () = {
    assert!(APP_ATTRIBUTION_HISTORIES_RESPONSE_BYTES_MAX == 24 * 1024 * 1024);
    assert!(size_of::<Box<AppAttributionHistories>>() <= 16);
    assert!(3 * reduce::WORKING_BYTES_MAX + 512 * 1024 < 40 * 1024 * 1024);
};

impl LiveSession {
    /// Counts saved App-history context preparation attempts, not disk reads or bytes.
    /// Resets when the saved interpreter is replaced and saturates at `u64::MAX`.
    #[must_use]
    pub fn saved_history_context_preparations(&self) -> u64 {
        self.history.as_ref().map_or(0, |history| {
            history.saved_history_context_preparations.get()
        })
    }

    pub fn app_attribution_histories(&self) -> Result<Box<AppAttributionHistories>> {
        self.operation.run(|| {
            let mut selected = None;
            self.with_selected_apps(|sample| {
                selected = Some(sample.selected);
                Ok(Value::Null)
            })?;
            let selected = selected.ok_or(Error::Bound("App attribution selection"))?;
            self.app_history.attribution_batch_response(
                &selected,
                self.history.as_ref(),
                &self.operation,
            )
        })
    }

    pub fn app_attribution_history(&self, metric: AppAttributionMetric) -> Result<Value> {
        self.operation.run(|| {
            let mut selected = None;
            self.with_selected_apps(|sample| {
                selected = Some(sample.selected);
                Ok(Value::Null)
            })?;
            let selected = selected.ok_or(Error::Bound("App attribution selection"))?;
            self.app_history.attribution_response(
                &selected,
                metric,
                self.history.as_ref(),
                &self.operation,
            )
        })
    }
}

impl AppAttributionMetric {
    pub(crate) fn name(self) -> &'static str {
        match self {
            Self::Cpu => "cpu",
            Self::Power => "power",
            Self::Disk => "disk",
        }
    }

    pub(crate) fn amount(self) -> &'static str {
        match self {
            Self::Cpu => "cpu_ns",
            Self::Power => "energy_nj",
            Self::Disk => "disk_io_bytes",
        }
    }

    pub(crate) fn rate_name(self) -> &'static str {
        match self {
            Self::Cpu => "cpu_percent_one_core",
            Self::Power => "energy_watts",
            Self::Disk => "disk_io_bps",
        }
    }

    pub(crate) fn rate(self, amount: u64, elapsed_ns: Option<u64>) -> Option<f64> {
        match self {
            Self::Cpu => crate::apps::cpu_percent_one_core(crate::apps::CpuRate {
                cpu_ns: amount,
                elapsed_ns: elapsed_ns?,
            }),
            Self::Power => crate::apps::energy_watts(crate::apps::EnergyRate {
                energy_nj: amount,
                elapsed_ns: elapsed_ns?,
            }),
            Self::Disk => crate::apps::bytes_per_second(crate::apps::BytesRate {
                bytes: amount,
                elapsed_ns: elapsed_ns?,
            }),
        }
    }
}

#[cfg(test)]
#[path = "app_attribution_history/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "app_attribution_history/cohort_tests.rs"]
mod cohort_tests;

#[cfg(test)]
#[path = "app_attribution_history/batch_tests.rs"]
mod batch_tests;
