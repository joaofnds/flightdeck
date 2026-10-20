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
        let mut reading = resident_reading(snapshot, &target);
        let mut preview_request = crate::live_source::Request {
            second: Some(target.second),
            identity: target.identity.clone(),
            load_resident_history: false,
            load_app_attribution_history: false,
            load_process_history: false,
            ..request.clone()
        };
        let mut executables = Vec::new();
        if request.route == "apps" {
            match resident_app(snapshot, &target, &mut preview_request, &mut reading) {
                Ok(groups) => executables = groups,
                Err(error) => {
                    eprintln!("Hover App details unavailable: {error}");
                    reading.apps = None;
                    reading.detail = None;
                }
            }
        }
        let matched = target.matches(&snapshot.selection["selected"]);
        let complete = resident_complete(&request.route, matched, &reading);

        Self {
            source: gpui::SharedString::default(),
            origin: Origin::Pointer,
            reading,
            complete,
            request: preview_request,
            base_request: request.clone(),
            target,
            generation: None,
            failure: None,
            executables,
            layout: None,
            header_height: None,
            retarget_geometry: false,
        }
    }

    /// Leaves the preview to a read of its target, since the open App picker lists the
    /// target's Apps and only a read gives them.
    pub fn list_apps(&mut self, committed: &Snapshot) {
        if !self.target.matches(&committed.selection["selected"]) {
            self.complete = false;
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
        app_parts: None,
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
        "apps" => reading.detail.is_some(),
        _ => true,
    }
}

/// Reads the hovered second's App detail and Apps page from the selected App's part the
/// committed snapshot holds, paged as a read of that second pages them, and gives its process
/// groups. Leaves `reading` without them where no part of that App is held.
fn resident_app(
    snapshot: &Snapshot,
    target: &Target,
    request: &mut crate::live_source::Request,
    reading: &mut Snapshot,
) -> Result<Vec<crate::live_executables::Executable>, String> {
    let Some(part) = resident_app_part(snapshot, target, request)? else {
        return Ok(Vec::new());
    };

    let page = |offset| viewer::live_app_response::Page {
        offset,
        limit: viewer::live_app_response::PAGE_ROWS_MAX,
    };
    let apps = crate::live_source::clamped_page(request.apps_offset, |offset| {
        part.apps_paging(page(offset))
            .map_err(|error| error.to_string())
    })?;
    let detail = crate::live_source::clamped_page(request.members_offset, |offset| {
        part.detail(page(offset)).map_err(|error| error.to_string())
    })?;
    let executables = crate::live_executables::groups(Some(&detail))?;
    reading.apps = Some(apps);
    reading.detail = Some(detail);
    crate::live_acceptance::normalize_offsets(request, reading)?;

    Ok(executables)
}

/// The selected App's part of the hovered second, where the committed snapshot holds it.
fn resident_app_part<'a>(
    snapshot: &'a Snapshot,
    target: &Target,
    request: &crate::live_source::Request,
) -> Result<Option<&'a viewer::live_app_response::AppPart>, String> {
    let Some(parts) = snapshot.app_parts.as_deref() else {
        return Ok(None);
    };
    let Some(identity) = target.identity.as_ref() else {
        return Ok(None);
    };
    let Some(key) = request.app.as_ref() else {
        return Ok(None);
    };

    let Some(part) = parts.get(&crate::live_rankings::observation_key(identity)?) else {
        return Ok(None);
    };
    let kind = crate::live_source::app_key_kind(&key.kind)?;
    let bytes = crate::live_source::app_key_bytes(&key.bytes_hex)?;
    if !part.is_of(kind, &bytes) {
        return Ok(None);
    }

    Ok(Some(part))
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
