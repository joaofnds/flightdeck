use std::sync::Arc;

use serde_json::{Value, json};

use collector::recording::{identity_text, parse_identity};
use collector::ring::Identity;
use viewer::apps::Stamp;
use viewer::live_app_response::{KeyKind, Rank, Ranks};

use crate::live_source::{AppKey, app_key_bytes_hex, identity_signature};

#[path = "live_rankings_view.rs"]
pub(crate) mod view;

/// The viewer's bound, so the selected second and a hover show as many Apps.
pub const RANKS_MAX: usize = viewer::live_app_response::RANKS_MAX;
const _: () = {
    assert!(RANKS_MAX <= 10);
};

#[derive(Debug, PartialEq)]
pub enum Rankings {
    Ready(Box<TopApps>),
    Unavailable(String),
}

#[derive(Debug, PartialEq)]
pub struct TopApps {
    pub selected: Value,
    pub app_count: u32,
    pub membership_complete: bool,
    pub ungrouped_members: u32,
    pub metrics: [Ranking; 5],
}

#[derive(Debug, PartialEq)]
pub struct Ranking {
    pub metric: Metric,
    pub entries: Vec<Entry>,
    pub unavailable: u32,
    pub overflowed: u32,
    pub partial: u32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Metric {
    Cpu,
    Resident,
    Power,
    Disk,
    Gpu,
}

#[derive(Debug, PartialEq)]
pub struct Entry {
    pub app: Arc<App>,
    pub amount: u64,
    pub rate: Option<f64>,
    pub measured_members: u32,
    pub unavailable_members: u32,
}

#[derive(Debug, PartialEq)]
pub struct App {
    pub key: AppKey,
    pub name: String,
}

/// The selected observation's top Apps, or a bounded notice of why the viewer could not rank
/// them.
pub fn from_selected(ranks: Result<Ranks, String>) -> Rankings {
    match ranks {
        Ok(ranks) => Rankings::Ready(Box::new(from_ranks(&ranks))),
        Err(error) => Rankings::Unavailable(crate::bounded::bounded(&error, 512)),
    }
}

/// The top Apps of an observation the viewer ranked, in the shape the pages draw.
pub fn from_ranks(ranks: &Ranks) -> TopApps {
    TopApps {
        selected: stamp_identity(&ranks.selected.stamp),
        app_count: ranks.app_count,
        membership_complete: ranks.membership_complete,
        ungrouped_members: ranks.ungrouped_members,
        metrics: ranks.metrics.each_ref().map(|ranking| Ranking {
            metric: match ranking.metric {
                viewer::live_app_response::Metric::Cpu => Metric::Cpu,
                viewer::live_app_response::Metric::Resident => Metric::Resident,
                viewer::live_app_response::Metric::Power => Metric::Power,
                viewer::live_app_response::Metric::Disk => Metric::Disk,
                viewer::live_app_response::Metric::Gpu => Metric::Gpu,
            },
            entries: ranking.entries.iter().map(from_ranks_entry).collect(),
            unavailable: ranking.unavailable,
            overflowed: ranking.overflowed,
            partial: ranking.partial,
        }),
    }
}

/// The identity the pages name the tick of `stamp` by, as the worker names it.
pub fn stamp_identity(stamp: &Stamp) -> Value {
    json!({
        "run": identity_text(Identity(stamp.run)).to_string(),
        "boot": identity_text(Identity(stamp.boot)).to_string(),
        "publication": stamp.publication,
    })
}

fn from_ranks_entry(rank: &Rank) -> Entry {
    Entry {
        app: Arc::new(App {
            key: AppKey {
                kind: match rank.app.kind {
                    KeyKind::Bundle => "bundle",
                    KeyKind::Executable => "executable",
                }
                .into(),
                bytes_hex: app_key_bytes_hex(&rank.app.bytes),
            },
            name: rank.app.display_name.clone(),
        }),
        amount: rank.amount,
        rate: rank.rate,
        measured_members: rank.measured_members,
        unavailable_members: rank.unavailable_members,
    }
}

/// The key the viewer ranks an observation under.
pub fn observation_key(identity: &Value) -> Result<viewer::ObservationKey, String> {
    let (run, boot, publication) = identity_signature(identity)?;

    Ok(viewer::ObservationKey {
        run: parse_identity(&run).map_err(|error| error.to_string())?.0,
        boot: parse_identity(&boot).map_err(|error| error.to_string())?.0,
        publication,
    })
}

impl Metric {
    pub fn title(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Resident => "Resident memory",
            Self::Power => "Process power",
            Self::Disk => "Disk I/O",
            Self::Gpu => "GPU time",
        }
    }
}

#[cfg(test)]
#[path = "live_rankings_fixture.rs"]
pub(crate) mod fixture;

#[cfg(test)]
#[path = "live_rankings_tests.rs"]
mod tests;
