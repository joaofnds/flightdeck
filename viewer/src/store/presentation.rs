//! Pages of complete retained App facts, borrowing identities from their byte owners.

use persistence::{Error, Result};
use serde_json::Value;

use super::{AppRow, AppsSecond};
use crate::live_app_response::json::{self, AppView, Header};
use crate::live_app_response::{Detail, MEMBERS_MAX, Page};
use crate::store::{AppDetails, Payload};

impl AppsSecond {
    /// The requested Apps page, with no process or identity copies outside its JSON result.
    pub fn apps_page(&self, page: Page) -> Result<Value> {
        let (header, payload) = self.presentation()?;
        let mut result = json::apps_paging(&header, page)?;
        let range = json::page_range(
            page,
            self.apps.len(),
            crate::live_app_response::APPS_PAGE_ROWS_MAX,
        )?;
        let values = range
            .map(|index| {
                json::app(
                    &self.apps[index].presentation(payload.app(index)),
                    index,
                    self.elapsed_ns,
                )
            })
            .collect();
        result["apps"] = Value::Array(values);

        json::bounded(result)
    }

    /// The requested member page, scoped to the exact captured observation and App index.
    pub fn app_detail(&self, detail: &Detail<'_>) -> Result<Value> {
        let (header, payload) = self.presentation()?;
        if detail.selected != &header.selected {
            return Err(Error::Bound("live App stale detail"));
        }

        let index =
            usize::try_from(detail.app_index).map_err(|_| Error::Bound("live App index"))?;
        let app = self.apps.get(index).ok_or(Error::Bound("live App index"))?;
        let mut result =
            presentation_members(&header, payload, Some(detail.app_index), detail.page)?;
        result["app"] = json::app(
            &app.presentation(payload.app(index)),
            index,
            header.elapsed_ns,
        );

        json::bounded(result)
    }

    /// The requested page of processes whose captured identity placed them in no App.
    pub fn ungrouped_page(&self, page: Page) -> Result<Value> {
        let (header, payload) = self.presentation()?;

        presentation_members(&header, payload, None, page)
    }

    pub(crate) fn presentation(&self) -> Result<(Header, &Payload)> {
        if self.missing.is_some() {
            return Err(Error::Bound("live App empty sample"));
        }

        let payload = self
            .processes
            .as_ref()
            .map_err(|_| Error::Bound("live App empty sample"))?;
        let selected = self
            .observation
            .ok_or(Error::Bound("live App empty sample"))?;
        if selected != payload.selected {
            return Err(Error::Bound("live App stale detail"));
        }
        if self.apps.len() != payload.apps().len() {
            return Err(Error::Bound("live App index"));
        }
        if self.apps.len() > super::APPS_PER_SECOND_MAX {
            return Err(Error::Bound("live App index"));
        }
        if payload.len() > MEMBERS_MAX {
            return Err(Error::Bound("live App member count"));
        }

        Ok((
            Header {
                selected,
                previous: payload.previous,
                elapsed_ns: self.elapsed_ns,
                membership_complete: self.membership_complete,
                ungrouped_members: self.ungrouped_members,
                live_ungrouped_members: self.live_ungrouped_members,
                usage_refused: self.usage_refused,
                usage_gone: self.usage_gone,
                app_count: self.apps.len(),
                process_count: payload.len(),
            },
            payload,
        ))
    }
}

fn presentation_members(
    header: &Header,
    payload: &Payload,
    app_index: Option<u32>,
    page: Page,
) -> Result<Value> {
    let processes = payload.members(app_index);
    let count = processes.clone().count();

    json::processes_page(
        header,
        processes.map(|process| process.presentation(app_index)),
        count,
        page,
    )
}

impl AppRow {
    pub(crate) fn presentation<'row>(&'row self, details: &AppDetails) -> AppView<'row> {
        AppView {
            kind: self.name.kind,
            bytes: &self.name.bytes,
            display_name: &self.name.display_name,
            members: self.members,
            live_members: details.live_members,
            membership_complete: self.membership_complete,
            cpu_ns: self.cpu_ns.measure(),
            energy_nj: self.energy_nj.measure(),
            gpu_ns: self.gpu_ns.measure(),
            disk_read_bytes: details.disk_read_bytes,
            disk_written_bytes: details.disk_written_bytes,
            disk_io_bytes: self.disk_io_bytes.measure(),
            resident_bytes: self.resident_bytes.measure(),
            footprint_bytes: details.footprint_bytes,
        }
    }
}
