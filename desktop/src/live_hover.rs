use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{Value, json};
use viewer::store::MachineSecond;

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
#[path = "live_hover_timings.rs"]
pub(crate) mod timings;

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
    /// The hover kept a reply to the page's request from the page, or had the worker read its
    /// second in place of that request, so the page asks again when the hover ends.
    pub page_reply_kept: bool,
    /// When the app began handling what put the pointer over the target, while the hover's
    /// timing runs.
    pub hovered_at: Option<std::time::Instant>,
    /// The hovered second as the store's View held it, where the page draws from the View, or
    /// none where the worker's readings show the hover.
    pub held: Option<HeldSecond>,
}

/// A hovered second as the store's View held it when the pointer reached it.
pub struct HeldSecond {
    /// The second's readings, or none where the View holds no reading of it.
    pub second: Option<Arc<MachineSecond>>,
    /// The second's top Apps, or none where the View holds no Apps of it, as for a second of the
    /// older tail.
    pub rankings: Option<crate::live_rankings::Rankings>,
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
            let second = crate::live_projection::inspection_second(&snapshot.selection, second);
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
            earlier: crate::live_source::Earlier::Skip,
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
            page_reply_kept: false,
            hovered_at: None,
            held: None,
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
        if let Some(preview) = self.shown_preview() {
            let accepted = self.snapshot.as_deref()?;
            if preview.target.matches(&accepted.selection["selected"]) {
                return Some(accepted);
            }

            return Some(&preview.reading);
        }

        self.snapshot.as_deref()
    }

    pub(crate) fn reading_target(&self) -> Option<&Target> {
        self.shown_preview().map(|preview| &preview.target)
    }

    pub(crate) fn reading_target_source(&self) -> Option<&gpui::SharedString> {
        self.shown_preview().map(|preview| &preview.source)
    }

    pub(crate) fn reading_origin(&self) -> Option<Origin> {
        self.shown_preview().map(|preview| preview.origin)
    }

    pub(crate) fn reading_request(&self) -> &crate::live_source::Request {
        self.shown_preview()
            .map_or(&self.request, |preview| &preview.request)
    }

    pub(crate) fn reading_executables(&self) -> &[crate::live_executables::Executable] {
        if let Some(preview) = self.shown_preview() {
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
        selection: crate::live_source::Presentation::Legacy(json!({"second":target.second,
            "context_last_second":snapshot.selection["context_last_second"],"selected":selected,
            "summary":summary,"alternatives":[],"trends":[]})),
        pages: BTreeMap::new(),
        apps: None,
        detail: None,
        history: None,
        cpu_detail: resident_cpu_detail(snapshot, target)
            .map(crate::live_source::Presentation::Legacy),
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

    if crate::live_source::route_ranks_apps(route) {
        if reading.rankings.is_none() {
            return false;
        }
    }
    match route {
        "cpu" => reading.cpu_detail.is_some(),
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
    let apps = crate::live_source::clamped_presentation_page(request.apps_offset, |offset| {
        part.apps_paging(page(offset))
            .map_err(|error| error.to_string())
    })?;
    let detail = crate::live_source::clamped_presentation_page(request.members_offset, |offset| {
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
) -> Result<Option<crate::live_source::Part<'a>>, String> {
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
) -> Option<Arc<crate::live_source::Presentation<crate::live_rankings::Rankings>>> {
    let window = snapshot.app_ranks.as_ref()?;
    let identity = target.identity.as_ref()?;
    match crate::live_rankings::observation_key(identity) {
        Ok(key) => {
            let ranks = window.get(&key)?;
            let projected = match window.as_ref() {
                crate::live_source::RanksWindow::Legacy(_) => {
                    crate::live_source::Presentation::Legacy(crate::live_rankings::Rankings::Ready(
                        Box::new(crate::live_rankings::from_ranks(ranks)),
                    ))
                }
                crate::live_source::RanksWindow::Store(window) => {
                    let retained = match window.selected_ranks_for(&ranks.selected) {
                        Ok(retained) => retained,
                        Err(error) => {
                            eprintln!("Hover App ranks unavailable: {error}");
                            return None;
                        }
                    };
                    crate::live_source::Presentation::Canonical(retained.map(|ranks| {
                        crate::live_rankings::Rankings::Ready(Box::new(
                            crate::live_rankings::from_ranks(&ranks),
                        ))
                    }))
                }
            };

            Some(Arc::new(projected))
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
