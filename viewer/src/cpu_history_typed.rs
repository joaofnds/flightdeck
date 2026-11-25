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

/// The logical CPUs of one second as the CPU page's bars draw them: the busy share of each over
/// the interval since the reading before, or why the interval has none.
pub struct CpuDetail {
    pub identity: CpuHistoryIdentity,
    /// The reading the interval runs from, where it is held.
    pub previous: Option<CpuHistoryIdentity>,
    pub unavailable_reason: Option<&'static str>,
    /// One reading for each logical CPU, in the order of their indices.
    pub busy_percent: Vec<Option<f64>>,
}

pub struct CpuHistoryRow {
    pub cpu: u32,
    /// One reading for each observation of the history, in the same order.
    pub busy_percent: Vec<Option<f64>>,
}

impl CpuHistory {
    /// What the history holds by capacity, its own bytes and those it allocated.
    pub(crate) fn held_bytes(&self) -> usize {
        size_of::<Self>()
            + self.observations.capacity() * size_of::<CpuHistoryObservation>()
            + self
                .observations
                .iter()
                .map(CpuHistoryObservation::heap_bytes)
                .sum::<usize>()
            + self.cpus.capacity() * size_of::<CpuHistoryRow>()
            + self
                .cpus
                .iter()
                .map(|row| row.busy_percent.capacity() * size_of::<Option<f64>>())
                .sum::<usize>()
    }

    /// The history as the saved minutes write it. `selected` is the selected observation as the
    /// caller's page names it, which carries more than its identity.
    #[must_use]
    pub fn json(&self, selected: &Value) -> Value {
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

impl CpuHistoryObservation {
    fn heap_bytes(&self) -> usize {
        self.identity.heap_bytes()
            + self
                .previous
                .as_ref()
                .map_or(0, CpuHistoryIdentity::heap_bytes)
    }
}

impl CpuHistoryIdentity {
    fn heap_bytes(&self) -> usize {
        self.run.capacity() + self.boot.capacity()
    }

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
