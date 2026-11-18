//! The CPU history as values. The Store hands them to the app as they are, and the saved minutes
//! turn them into JSON.
use serde_json::{Value, json};

use crate::cpu_history_sample::Identity;

/// The CPU history of one context: its observations in order, and a row of readings for each
/// logical CPU of the page asked for.
pub struct CpuHistory {
    /// The index in `observations` of the observation the context selects.
    pub selected_index: usize,
    pub first_ns: u64,
    pub end_ns: u64,
    pub offset: u32,
    pub total: u32,
    pub retention_limited: bool,
    pub observations: Vec<CpuHistoryObservation>,
    pub cpus: Vec<CpuHistoryRow>,
}

pub struct CpuHistoryObservation {
    pub identity: CpuHistoryIdentity,
    pub previous: Option<CpuHistoryIdentity>,
    pub interval_available: bool,
    pub cpu_count: u32,
}

pub struct CpuHistoryIdentity {
    pub run: String,
    pub boot: String,
    pub publication: u64,
    pub run_tick: u64,
    pub wall_ns: u64,
}

pub struct CpuHistoryRow {
    pub cpu: u32,
    /// One reading for each observation of the history, in the same order.
    pub busy_percent: Vec<Option<f64>>,
}

impl CpuHistory {
    /// `selected` is the selected observation as the caller's page names it, which carries more
    /// than its identity.
    pub(crate) fn json(&self, selected: &Value) -> Value {
        let observations = self
            .observations
            .iter()
            .map(|observation| {
                json!({"observation":observation.identity.json(),
                "previous_observation":observation.previous.as_ref()
                    .map(CpuHistoryIdentity::json),
                "interval_available":observation.interval_available,
                "cpu_count":observation.cpu_count})
            })
            .collect::<Vec<_>>();
        let cpus = self
            .cpus
            .iter()
            .map(|row| json!({"cpu":row.cpu,"busy_percent":row.busy_percent}))
            .collect::<Vec<_>>();

        json!({"observation":selected,"context_seconds":900,
            "first_ns":self.first_ns,"end_ns":self.end_ns,"offset":self.offset,
            "total":self.total,"retention_limited":self.retention_limited,
            "observations":observations,"cpus":cpus})
    }
}

impl CpuHistoryIdentity {
    pub(crate) fn of(identity: &Identity) -> Self {
        Self {
            run: identity.run.clone(),
            boot: identity.boot.clone(),
            publication: identity.observation.publication,
            run_tick: identity.observation.run_tick,
            wall_ns: identity.observation.wall,
        }
    }

    pub(super) fn json(&self) -> Value {
        json!({"run":self.run,"boot":self.boot,"publication":self.publication,
            "run_tick":self.run_tick,"wall_ns":self.wall_ns})
    }
}
