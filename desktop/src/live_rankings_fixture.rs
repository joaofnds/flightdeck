//! Top Apps a test seeds a page with, in the shape the viewer's ranks give the pages.
use std::sync::Arc;

use serde_json::{Value, json};

use super::{App, Entry, Metric, Ranking, Rankings, TopApps};
use crate::live_source::{AppKey, identity_signature};

/// The top Apps of `selected` when one App, Current, is all that ran: its amount and rate in
/// CPU, resident memory, power and disk, in that order.
pub fn current_only(selected: &Value, readings: [(u64, Option<f64>); 4]) -> Rankings {
    one_bundle(selected, "Current", "41", readings)
}

/// The top Apps of `selected` when one bundle, named `name` with key bytes `bytes_hex`, is all
/// that ran, with its readings as `current_only` takes them.
pub fn one_bundle(
    selected: &Value,
    name: &str,
    bytes_hex: &str,
    readings: [(u64, Option<f64>); 4],
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

const METRICS: [Metric; 4] = [Metric::Cpu, Metric::Resident, Metric::Power, Metric::Disk];

fn top_apps(selected: &Value, app_count: u32, metrics: [Ranking; 4]) -> Rankings {
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
