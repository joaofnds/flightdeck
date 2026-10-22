//! Top Apps a test seeds a page with, in the shape the viewer's ranks give the pages.
use std::sync::Arc;

use serde_json::{Value, json};

use super::{App, Entry, Metric, Ranking, Rankings, TopApps};
use crate::live_source::{AppKey, identity_signature};

/// The top Apps of `selected` when one App, Current, is all that ran: its amount and rate in
/// CPU, resident memory, power, disk and GPU time, in that order.
pub fn current_only(selected: &Value, readings: [(u64, Option<f64>); 5]) -> Rankings {
    one_bundle(selected, "Current", "41", readings)
}

/// The top Apps of `selected` when one bundle, named `name` with key bytes `bytes_hex`, is all
/// that ran, with its readings as `current_only` takes them.
pub fn one_bundle(
    selected: &Value,
    name: &str,
    bytes_hex: &str,
    readings: [(u64, Option<f64>); 5],
) -> Rankings {
    let app = Arc::new(App {
        key: AppKey {
            kind: "bundle".into(),
            bytes_hex: bytes_hex.into(),
        },
        name: name.into(),
    });
    let metrics = std::array::from_fn(|index| {
        let (amount, rate) = readings[index];
        let entry = Entry {
            app: Arc::clone(&app),
            amount,
            rate,
            measured_members: 1,
            unavailable_members: 0,
        };
        ranking(METRICS[index], vec![entry])
    });

    top_apps(selected, 1, metrics)
}

/// The top Apps of `selected` when no App ran.
pub fn empty(selected: &Value) -> Rankings {
    top_apps(
        selected,
        0,
        METRICS.map(|metric| ranking(metric, Vec::new())),
    )
}

/// The Apps a ranking could not rank, by why: no reading, or a sum past what it holds.
pub struct Unranked {
    pub unavailable: u32,
    pub overflowed: u32,
}

/// The top Apps of `selected` when the `unranked` Apps that ran have no reading of any metric,
/// as in a window sealed before the collector recorded each process's GPU time.
pub fn unranked(selected: &Value, unranked: &Unranked) -> Rankings {
    let metrics = METRICS.map(|metric| {
        let mut ranking = ranking(metric, Vec::new());
        ranking.unavailable = unranked.unavailable;
        ranking.overflowed = unranked.overflowed;
        ranking
    });

    top_apps(
        selected,
        unranked.unavailable + unranked.overflowed,
        metrics,
    )
}

const METRICS: [Metric; 5] = [
    Metric::Cpu,
    Metric::Resident,
    Metric::Power,
    Metric::Disk,
    Metric::Gpu,
];

fn top_apps(selected: &Value, app_count: u32, metrics: [Ranking; 5]) -> Rankings {
    let (run, boot, publication) = identity_signature(selected).unwrap();

    Rankings::Ready(Box::new(TopApps {
        selected: json!({"run": run, "boot": boot, "publication": publication}),
        app_count,
        membership_complete: true,
        ungrouped_members: 0,
        metrics,
    }))
}

fn ranking(metric: Metric, entries: Vec<Entry>) -> Ranking {
    Ranking {
        metric,
        entries,
        unavailable: 0,
        overflowed: 0,
        partial: 0,
    }
}
