use std::rc::Rc;

use serde_json::Value;

use crate::data::Panel;
use crate::live_source::{AppKey, Request, identity_validate};

#[path = "live_process_groups.rs"]
pub(crate) mod groups;

pub enum ProcessHistory {
    All {
        app: AppKey,
        payload: Box<viewer::app_process_groups_history::AppProcessGroupsHistory>,
    },
    Ready {
        app: AppKey,
        executable: String,
        payload: Value,
    },
    Unavailable(String),
}

const POINTS_MAX: usize = 4096;
const NAME_BYTES_MAX: usize = 4095;
pub const PANEL_PREFIX: &str = "live-process-group-";
pub const DISK_PANEL: &str = "live-process-group-disk";

impl ProcessHistory {
    pub fn prepare(payload: Value, request: &Request, selection: &Value) -> Self {
        match prepare_validate(&payload, request, selection) {
            Ok((app, executable)) => Self::Ready {
                app,
                executable,
                payload,
            },
            Err(reason) => Self::Unavailable(reason),
        }
    }

    pub fn prepare_all(
        payload: Box<viewer::app_process_groups_history::AppProcessGroupsHistory>,
        request: &Request,
        selection: &Value,
    ) -> Self {
        groups::prepare(payload, request, selection).unwrap_or_else(Self::Unavailable)
    }

    pub fn all(&self) -> Option<&viewer::app_process_groups_history::AppProcessGroupsHistory> {
        match self {
            Self::All { payload, .. } => Some(payload),
            Self::Ready { .. } | Self::Unavailable(_) => None,
        }
    }

    pub fn payload(&self) -> Option<&Value> {
        match self {
            Self::Ready { payload, .. } => Some(payload),
            Self::All { .. } | Self::Unavailable(_) => None,
        }
    }

    pub fn matches(&self, request: &Request) -> bool {
        match self {
            Self::All { app, .. } => groups::matches(app, request),
            Self::Ready {
                app, executable, ..
            } => {
                if request.app.as_ref() != Some(app) {
                    return false;
                }

                request
                    .executable
                    .as_deref()
                    .is_some_and(|selected| selected.eq_ignore_ascii_case(executable))
            }
            Self::Unavailable(_) => false,
        }
    }

    pub fn panels(&self, selection: &Value) -> Result<Vec<Rc<Panel>>, String> {
        if let Self::All { payload, .. } = self {
            let last = crate::live_projection::context_last_second(selection)
                .ok_or("Process groups selection has no context end")?;
            return groups::panels(payload, last);
        }

        let Some(payload) = self.payload() else {
            return Ok(Vec::new());
        };

        let mut panels = crate::live_projection::panels("apps", selection, Some(payload))?;
        assert!(panels.len() <= 8);
        let group = crate::live_views::text(&payload["executable_name"]["display"]);
        for panel in &mut panels {
            let panel = Rc::get_mut(panel).ok_or("Process history projection is already shared")?;
            let suffix = panel
                .id
                .strip_prefix("live-app-")
                .ok_or("Unexpected process history projection")?;
            panel.id = format!("{PANEL_PREFIX}{suffix}");
            for line in &mut panel.lines {
                line.name = format!("{group}{}", line.name);
            }
        }
        Ok(panels)
    }

    pub fn filter_reason(filter: Option<&str>) -> Option<&'static str> {
        match filter {
            Some("hex:") => Some("Empty executable names have no captured named history"),
            Some("unavailable") => {
                Some("Executable identity unavailable; named history unavailable")
            }
            _ => None,
        }
    }
}

fn prepare_validate(
    payload: &Value,
    request: &Request,
    selection: &Value,
) -> Result<(AppKey, String), String> {
    if request.route != "apps" {
        return Err("Process history requires the selected App".into());
    }

    if payload["scope"] != "selected_named_process_group" {
        return Err("Process history has a different scope".into());
    }

    if payload["context_seconds"].as_u64() != Some(900) {
        return Err("Process history requires 900 seconds of context".into());
    }

    identity_validate(&payload["selected"], &selection["selected"])?;
    let app =
        crate::live_source::app_key_from_json(&serde_json::json!({"key":payload["app_key"]}))?;
    if request.app.as_ref() != Some(&app) {
        return Err("Process history belongs to another App".into());
    }

    let bytes = payload["executable_name"]["bytes_hex"]
        .as_str()
        .ok_or("Process history has no executable name bytes")?;
    let executable = format!("hex:{bytes}");
    name_bytes(&executable)?.ok_or("Process history requires a nonempty executable name")?;
    if !request
        .executable
        .as_deref()
        .is_some_and(|key| key.eq_ignore_ascii_case(&executable))
    {
        return Err("Process history belongs to another executable name".into());
    }

    for display in [
        &payload["app_key"]["display"],
        &payload["executable_name"]["display"],
    ] {
        if display
            .as_str()
            .ok_or("Process history display is missing")?
            .len()
            > NAME_BYTES_MAX * 4
        {
            return Err("Process history display exceeds the captured name bound".into());
        }
    }
    let points = payload["points"]
        .as_array()
        .ok_or("Process history points are missing")?;
    if points.len() > POINTS_MAX {
        return Err("Process history exceeds 4096 observations".into());
    }

    Ok((app, executable.to_ascii_lowercase()))
}

pub fn name_bytes(filter: &str) -> Result<Option<Vec<u8>>, String> {
    if filter == "unavailable" {
        return Ok(None);
    }

    let text = filter
        .strip_prefix("hex:")
        .ok_or("Invalid executable filter")?;
    let bytes = crate::live_source::app_key_bytes(text)?;
    if bytes.is_empty() {
        return Ok(None);
    }

    Ok(Some(bytes))
}

pub fn captured_reading(payload: &Value, panel: &str, index: Option<usize>) -> Option<String> {
    let (field, rate, unit, exact_unit) = match panel.strip_prefix(PANEL_PREFIX)? {
        "wakeups" => (
            "idle_wakeups",
            "idle_wakeups_per_second",
            "/s",
            "wakeups / interval",
        ),
        "pageins" => (
            "pageins",
            "pageins_per_second",
            "pages/s",
            "page-ins / interval",
        ),
        "threads" => ("threads", "", "threads", "threads"),
        _ => return None,
    };

    let Some(point) = index.and_then(|index| payload["points"].get(index)) else {
        return Some("Unavailable".into());
    };

    let measure = &point[field];
    if measure["overflowed"] == true {
        return Some("Unavailable · counter sum overflow".into());
    }

    let measured = measure["measured_members"].as_u64()?;
    let unavailable = measure["unavailable_members"].as_u64()?;
    if measured == 0 {
        return Some(format!("Unavailable · {unavailable} unavailable"));
    }

    let known = match measure["known"].as_str()?.parse::<u64>() {
        Ok(value) => value,
        Err(error) => return Some(format!("Unavailable · invalid exact count: {error}")),
    };

    let reading = if field == "threads" {
        format!("{known} threads")
    } else {
        let human = crate::live_app_views::event_rate(point[rate].as_f64(), unit);
        format!("{human} · {known} {exact_unit}")
    };
    let coverage = if unavailable > 0 {
        format!(" · known subtotal · {measured} measured · {unavailable} unavailable")
    } else {
        String::new()
    };

    Some(format!("{reading}{coverage}"))
}
