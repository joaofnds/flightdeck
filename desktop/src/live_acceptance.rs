use std::rc::Rc;

use crate::data::Panel;
use crate::live_executables::Executable;
use crate::live_source::{AppKey, Request, Snapshot};

pub struct Presentation {
    pub request: Request,
    pub snapshot: Box<Snapshot>,
    pub panels: Vec<Rc<Panel>>,
    pub executables: Vec<Executable>,
}

impl Presentation {
    pub fn prepare(request: &Request, mut snapshot: Box<Snapshot>) -> Result<Box<Self>, String> {
        prepare_context(&snapshot.selection)?;
        let executables = crate::live_executables::groups(snapshot.detail.as_ref())?;
        let mut panels = crate::live_projection::panels(
            &request.route,
            &snapshot.selection,
            snapshot.history.as_ref(),
        )?;
        if request.route == "apps" {
            prepare_app_names(&mut panels, snapshot.detail.as_ref())?;
            let mut group_panels = prepare_process_panels(request, &mut snapshot);
            group_panels.append(&mut panels);
            panels = group_panels;
        }
        if request.route == "memory" {
            let payload = snapshot.history.take();
            if snapshot.resident_history.is_none() {
                if let Some(payload) = payload {
                    let resident = crate::live_resident_history::ResidentHistory::prepare(
                        &payload,
                        &snapshot.selection,
                    );
                    snapshot.resident_history = Some(std::sync::Arc::new(resident));
                }
            }
        }
        if matches!(request.route.as_str(), "memory" | "overview") {
            if let Some(resident) = &snapshot.resident_history {
                if let Some(panel) = resident.panel() {
                    panels.push(panel);
                }
            }
        }
        if let Some(history) = &snapshot.app_attribution_history {
            if let Some(panel) = history.panel() {
                panels.push(panel);
            }
        }
        if let Some(histories) = &snapshot.overview_app_histories {
            use crate::live_app_attribution_history::Metric;
            for metric in [Metric::Cpu, Metric::Power, Metric::Disk] {
                if let Some(panel) = histories.history(metric).panel() {
                    panels.push(panel);
                }
            }
        }
        if request.route == "overview" {
            prepare_overview_titles(&mut panels)?;
        }
        let mut presentation = Box::new(Self {
            request: request.clone(),
            snapshot,
            panels,
            executables,
        });

        normalize_offsets(&mut presentation.request, &presentation.snapshot)?;

        Ok(presentation)
    }

    pub fn selection_changed(&self, previous: Option<&Snapshot>) -> bool {
        let Some(previous) = previous else {
            return true;
        };

        if previous.catalog["path"] != self.snapshot.catalog["path"] {
            return true;
        }

        if previous.selection["second"] != self.snapshot.selection["second"] {
            return true;
        }

        for field in ["run", "boot", "publication"] {
            if previous.selection["selected"][field] != self.snapshot.selection["selected"][field] {
                return true;
            }
        }

        false
    }
}

/// Names each of the selected App's total charts and its line after the App, as
/// "CPU · Brave Browser", so the totals read apart from the process group charts above them.
/// Refuses a selection whose context ends before its second, since the charts are laid out
/// around the selected second within that context.
fn prepare_context(selection: &serde_json::Value) -> Result<(), String> {
    let Some(last) = crate::live_projection::context_last_second(selection) else {
        return Ok(());
    };
    let Some(second) = selection["second"].as_u64() else {
        return Ok(());
    };
    if last < second {
        return Err("Selection context ends before its second".into());
    }

    Ok(())
}

fn prepare_app_names(
    panels: &mut [Rc<Panel>],
    detail: Option<&serde_json::Value>,
) -> Result<(), String> {
    let Some(detail) = detail else {
        return Ok(());
    };
    let name = crate::live_app_views::app_name(&detail["app"]);
    for panel in panels {
        let panel = Rc::get_mut(panel).ok_or("App totals projection is already shared")?;
        assert!(panel.id.starts_with("live-app-"));
        panel.title = format!("{} · {name}", panel.title);
        for line in &mut panel.lines {
            line.name.clone_from(&name);
        }
    }
    Ok(())
}

/// Names the App history charts as the Overview does, which differs from the pages that own them.
fn prepare_overview_titles(panels: &mut [Rc<Panel>]) -> Result<(), String> {
    for panel in panels {
        let Some(title) = crate::live_overview_app_histories::overview_app_history_title(&panel.id)
        else {
            continue;
        };
        if panel.title == title {
            continue;
        }
        let panel =
            Rc::get_mut(panel).ok_or("Overview App history projection is already shared")?;
        panel.title = title.into();
    }

    Ok(())
}

fn prepare_process_panels(request: &Request, snapshot: &mut Snapshot) -> Vec<Rc<Panel>> {
    use crate::live_process_history::ProcessHistory;
    let Some(history) = snapshot.process_history.as_deref() else {
        return Vec::new();
    };

    if matches!(
        history,
        ProcessHistory::Ready { .. } | ProcessHistory::All { .. }
    ) {
        if !history.matches(request) {
            snapshot.process_history = Some(std::sync::Arc::new(ProcessHistory::Unavailable(
                "Process history belongs to another selected group".into(),
            )));
            return Vec::new();
        }
    }
    match history.panels(&snapshot.selection) {
        Ok(panels) => panels,
        Err(reason) => {
            snapshot.process_history = Some(std::sync::Arc::new(ProcessHistory::Unavailable(
                crate::bounded::bounded(&reason, 512),
            )));
            Vec::new()
        }
    }
}

pub(crate) fn normalize_offsets(request: &mut Request, snapshot: &Snapshot) -> Result<(), String> {
    if request.route == "cpu" {
        if let Some(history) = &snapshot.cpu_history {
            request.cpu_offset = history.offset();
        }
    }

    let table = if request.route == "cpu" {
        "processes"
    } else {
        &request.table
    };
    if let Some(page) = snapshot.pages.get(table) {
        request.offset = page["offset"].as_u64().ok_or("Missing raw page offset")?;
    }

    if let Some(apps) = &snapshot.apps {
        request.apps_offset = u32::try_from(
            apps["page"]["offset"]
                .as_u64()
                .ok_or("Missing Apps page offset")?,
        )
        .map_err(|error| error.to_string())?;
    }

    if let Some(detail) = &snapshot.detail {
        if !detail.is_null() {
            if request.app.is_none() {
                request.app = Some(AppKey {
                    kind: detail["app"]["key"]["kind"]
                        .as_str()
                        .ok_or("Missing selected App kind")?
                        .into(),
                    bytes_hex: detail["app"]["key"]["bytes_hex"]
                        .as_str()
                        .ok_or("Missing selected App bytes")?
                        .into(),
                });
            }
            request.members_offset = u32::try_from(
                detail["page"]["offset"]
                    .as_u64()
                    .ok_or("Missing process page offset")?,
            )
            .map_err(|error| error.to_string())?;
        }
    }

    Ok(())
}
