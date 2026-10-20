use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};

use crate::live_dashboard::LiveDashboard;
use crate::live_source::{Snapshot, identity_publication, identity_signature};

#[cfg(test)]
#[path = "live_identity_tests.rs"]
mod identity_tests;

#[path = "live_hover_controller.rs"]
pub(crate) mod controller;
#[path = "live_hover_layout.rs"]
pub(crate) mod layout;
#[path = "live_hover_rows.rs"]
mod rows;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Pointer,
    Keyboard,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Target {
    pub second: u64,
    pub identity: Option<Value>,
}

pub struct Preview {
    pub source: gpui::SharedString,
    pub origin: Origin,
    pub target: Target,
    pub reading: Box<Snapshot>,
    pub base_request: crate::live_source::Request,
    pub request: crate::live_source::Request,
    pub generation: Option<u64>,
    pub complete: bool,
    pub failure: Option<String>,
    pub executables: Vec<crate::live_executables::Executable>,
    pub layout: Option<layout::Layout>,
    pub header_height: Option<gpui::Pixels>,
    pub retarget_geometry: bool,
}

impl Target {
    pub fn exact(second: u64, identity: &Value) -> Result<Self, String> {
        let (run, boot, publication) = identity_signature(identity)?;
        Ok(Self {
            second,
            identity: Some(json!({"run":run,"boot":boot,"publication":publication})),
        })
    }

    pub fn regular(snapshot: &Snapshot, second: u64) -> Self {
        if let Some(identity) =
            crate::live_projection::inspection_identity(&snapshot.selection, second)
        {
            match Self::exact(second, &identity) {
                Ok(target) => return target,
                Err(error) => eprintln!("Hover observation identity unavailable: {error}"),
            }
        }

        Self {
            second,
            identity: None,
        }
    }

    pub fn matches(&self, observation: &Value) -> bool {
        self.identity.as_ref().is_some_and(|identity| {
            match (
                identity_publication(identity),
                identity_publication(observation),
            ) {
                (Ok(first), Ok(second)) if first == second => {}
                _ => return false,
            }

            match (
                identity_signature(identity),
                identity_signature(observation),
            ) {
                (Ok(first), Ok(second)) => first == second,
                _ => false,
            }
        })
    }
}

impl Preview {
    pub fn resident(
        snapshot: &Snapshot,
        request: &crate::live_source::Request,
        target: Target,
    ) -> Self {
        let reading = resident_reading(snapshot, &target);
        let matched = target.matches(&snapshot.selection["selected"]);
        let complete = resident_complete(&request.route, matched, &reading);

        Self {
            source: gpui::SharedString::default(),
            origin: Origin::Pointer,
            reading,
            complete,
            request: crate::live_source::Request {
                second: Some(target.second),
                identity: target.identity.clone(),
                load_resident_history: false,
                load_app_attribution_history: false,
                load_process_history: false,
                ..request.clone()
            },
            base_request: request.clone(),
            target,
            generation: None,
            failure: None,
            executables: Vec::new(),
            layout: None,
            header_height: None,
            retarget_geometry: false,
        }
    }
}

impl LiveDashboard {
    pub(crate) fn reading_snapshot(&self) -> Option<&Snapshot> {
        if let Some(preview) = &self.preview {
            let accepted = self.snapshot.as_deref()?;
            if preview.target.matches(&accepted.selection["selected"]) {
                return Some(accepted);
            }

            return Some(&preview.reading);
        }

        self.snapshot.as_deref()
    }

    pub(crate) fn reading_target(&self) -> Option<&Target> {
        self.preview.as_ref().map(|preview| &preview.target)
    }

    pub(crate) fn reading_target_source(&self) -> Option<&gpui::SharedString> {
        self.preview.as_ref().map(|preview| &preview.source)
    }

    pub(crate) fn reading_origin(&self) -> Option<Origin> {
        self.preview.as_ref().map(|preview| preview.origin)
    }

    pub(crate) fn reading_request(&self) -> &crate::live_source::Request {
        self.preview
            .as_ref()
            .map_or(&self.request, |preview| &preview.request)
    }

    pub(crate) fn reading_executables(&self) -> &[crate::live_executables::Executable] {
        if let Some(preview) = &self.preview {
            if !preview.target.matches(
                self.snapshot
                    .as_ref()
                    .map_or(&Value::Null, |snapshot| &snapshot.selection["selected"]),
            ) {
                return &preview.executables;
            }
        }

        &self.executables
    }
}

fn resident_cpu_detail(
    snapshot: &Snapshot,
    target: &Target,
) -> Option<crate::live_cpu::CPUActivity> {
    let history = snapshot.cpu_history.as_ref()?;
    let identity = target.identity.as_ref()?;
    match history.activity(identity) {
        Ok(activity) => activity,
        Err(error) => {
            eprintln!("Hover CPU readings unavailable: {error}");
            None
        }
    }
}

/// The readings of `target` the committed snapshot already holds.
fn resident_reading(snapshot: &Snapshot, target: &Target) -> Box<Snapshot> {
    let summary = resident_summary(snapshot, target).clone();
    let selected = target.identity.clone().unwrap_or(Value::Null);
    Box::new(Snapshot {
        generation: snapshot.generation,
        catalog: snapshot.catalog.clone(),
        selection: json!({"second":target.second,"selected":selected,
            "summary":summary,"alternatives":[],"trends":[]}),
        pages: BTreeMap::new(),
        apps: None,
        detail: None,
        history: None,
        cpu_detail: resident_cpu_detail(snapshot, target),
        cpu_history: None,
        resident_history: snapshot.resident_history.clone(),
        app_attribution_history: snapshot.app_attribution_history.clone(),
        overview_app_histories: snapshot.overview_app_histories.clone(),
        process_history: snapshot.process_history.clone(),
        histories_pending: snapshot.histories_pending,
        rankings: resident_rankings(snapshot, target),
        app_ranks: None,
        read_ms: 0.0,
    })
}

/// Whether the preview holds every reading of its target the page shows, so the page reads
/// nothing for it.
fn resident_complete(route: &str, matched: bool, reading: &Snapshot) -> bool {
    if matched {
        return true;
    }

    match route {
        "overview" | "storage" | "memory" | "energy" => reading.rankings.is_some(),
        "cpu" => {
            if reading.rankings.is_none() {
                return false;
            }

            reading.cpu_detail.is_some()
        }
        "apps" => false,
        _ => true,
    }
}

fn resident_rankings(
    snapshot: &Snapshot,
    target: &Target,
) -> Option<Arc<crate::live_rankings::Rankings>> {
    let window = snapshot.app_ranks.as_ref()?;
    let identity = target.identity.as_ref()?;
    match crate::live_rankings::observation_key(identity) {
        Ok(key) => {
            let ranks = window.get(&key)?;
            Some(Arc::new(crate::live_rankings::Rankings::Ready(Box::new(
                crate::live_rankings::from_ranks(ranks),
            ))))
        }
        Err(error) => {
            eprintln!("Hover App ranks unavailable: {error}");
            None
        }
    }
}

fn resident_summary<'a>(snapshot: &'a Snapshot, target: &Target) -> &'a Value {
    if target.matches(&snapshot.selection["selected"]) {
        return &snapshot.selection["summary"];
    }

    snapshot.selection["trends"]
        .as_array()
        .and_then(|points| {
            points
                .iter()
                .take(4096)
                .rfind(|point| target.matches(point))
        })
        .unwrap_or(&Value::Null)
}
