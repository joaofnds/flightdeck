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
    pub fn prepare(request: &Request, snapshot: Box<Snapshot>) -> Result<Box<Self>, String> {
        let executables = crate::live_executables::groups(snapshot.detail.as_ref())?;
        let panels = crate::live_projection::panels(
            &request.route,
            &snapshot.selection,
            snapshot.history.as_ref(),
        )?;
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
