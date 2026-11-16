//! Pages of complete retained App facts, borrowing identities from their byte owners.

use persistence::{Error, Result};
use serde_json::Value;

use super::{AppRow, AppsSecond};
use crate::live_app_response::json::{self, AppView, Header};
use crate::live_app_response::{Detail, MEMBERS_MAX, Page, RankedApp};
use crate::store::payload::Members;
use crate::store::payload::packed::BlockTable;
use crate::store::{AppDetails, Payload};

impl AppsSecond {
    /// The requested Apps page, with no process or identity copies outside its JSON result.
    pub(in crate::store) fn apps_page(&self, page: Page) -> Result<Value> {
        let apps = self.apps.as_slice();
        let (header, payload) = self.presentation_of(apps.len())?;
        let mut result = json::apps_paging(&header, page)?;
        let range = json::page_range(
            page,
            apps.len(),
            crate::live_app_response::APPS_PAGE_ROWS_MAX,
        )?;
        let details = payload.apps().skip(range.start);
        let values = (range.start..)
            .zip(apps[range].iter().zip(details))
            .map(|(index, (app, details))| {
                json::app(&app.presentation(&details), index, self.elapsed_ns)
            })
            .collect();
        result["apps"] = Value::Array(values);

        json::bounded(result)
    }

    /// The requested Apps page, as `apps_page` gives it, of the second at `second_index` of the
    /// packed block whose table is `table`, which holds the second's Apps, each App's row and
    /// details read in one pass.
    pub(in crate::store) fn apps_page_packed(
        &self,
        table: &BlockTable,
        second_index: u32,
        page: Page,
    ) -> Result<Value> {
        let count = table.app_count(second_index);
        let (header, _) = self.presentation_of(count)?;
        let mut result = json::apps_paging(&header, page)?;
        let range = json::page_range(page, count, crate::live_app_response::APPS_PAGE_ROWS_MAX)?;
        let mut values = Vec::with_capacity(range.len());
        table.app_rows_with_details(second_index, range, |index, app, details| {
            values.push(json::app(
                &app.presentation(&details),
                index,
                self.elapsed_ns,
            ));
        });
        result["apps"] = Value::Array(values);

        json::bounded(result)
    }

    /// The requested member page, scoped to the exact captured observation and App index.
    pub(in crate::store) fn app_detail(&self, detail: &Detail<'_>) -> Result<Value> {
        let apps = self.apps.as_slice();
        let (header, payload) = self.presentation_of(apps.len())?;
        if detail.selected != &header.selected {
            return Err(Error::Bound("live App stale detail"));
        }

        let index =
            usize::try_from(detail.app_index).map_err(|_| Error::Bound("live App index"))?;
        let app = apps.get(index).ok_or(Error::Bound("live App index"))?;
        let mut result = presentation_members(
            &header,
            payload.members(Some(detail.app_index)),
            detail.page,
        )?;
        result["app"] = json::app(
            &app.presentation(&payload.app(index)),
            index,
            header.elapsed_ns,
        );

        json::bounded(result)
    }

    /// The requested member page, as `app_detail` gives it, where the packed block of `table`
    /// holds the second's Apps as the second at `second_index`, which reads the one App the
    /// page shows and rebuilds no row of the rest.
    pub(in crate::store) fn app_detail_packed(
        &self,
        table: &BlockTable,
        second_index: u32,
        detail: &Detail<'_>,
    ) -> Result<Value> {
        let (header, payload) = self.presentation_of(table.app_count(second_index))?;
        if detail.selected != &header.selected {
            return Err(Error::Bound("live App stale detail"));
        }

        let index =
            usize::try_from(detail.app_index).map_err(|_| Error::Bound("live App index"))?;
        let place = table
            .app_place_at(second_index, index)
            .ok_or(Error::Bound("live App index"))?;
        let mut result = presentation_members(
            &header,
            payload.members_of_place(place, detail.app_index),
            detail.page,
        )?;
        let app = table.app_row(place, second_index);
        result["app"] = json::app(
            &app.presentation(&table.app_details(place, second_index)),
            index,
            header.elapsed_ns,
        );

        json::bounded(result)
    }

    /// The requested page of processes whose captured identity placed them in no App.
    pub fn ungrouped_page(&self, page: Page) -> Result<Value> {
        self.ungrouped_page_of(self.apps.len(), page)
    }

    /// The requested page of processes in no App, as `ungrouped_page` gives it, of a second
    /// that holds `app_count` Apps.
    pub(in crate::store) fn ungrouped_page_of(
        &self,
        app_count: usize,
        page: Page,
    ) -> Result<Value> {
        let (header, payload) = self.presentation_of(app_count)?;

        presentation_members(&header, payload.members(None), page)
    }

    /// The header a page of the second shows and its payload, of a second that holds
    /// `app_count` Apps.
    pub(crate) fn presentation_of(&self, app_count: usize) -> Result<(Header, &Payload)> {
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
        if app_count != payload.apps().len() {
            return Err(Error::Bound("live App index"));
        }
        if app_count > super::APPS_PER_SECOND_MAX {
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
                app_count,
                process_count: payload.len(),
            },
            payload,
        ))
    }
}

fn presentation_members(header: &Header, processes: Members<'_>, page: Page) -> Result<Value> {
    let count = processes.clone().count();

    json::processes_page(
        header,
        processes.map(|process| process.presentation()),
        count,
        page,
    )
}

impl AppRow {
    pub(crate) fn presentation<'row>(&'row self, details: &AppDetails) -> AppView<'row> {
        self.presentation_named(&self.name, details)
    }

    /// The App as a page shows it, with the readings of this row and the key and name of
    /// `name`, which names the same App.
    pub(crate) fn presentation_named<'name>(
        &self,
        name: &'name RankedApp,
        details: &AppDetails,
    ) -> AppView<'name> {
        assert_eq!(name.kind, self.name.kind);
        assert_eq!(name.bytes, self.name.bytes);
        AppView {
            kind: name.kind,
            bytes: &name.bytes,
            display_name: &name.display_name,
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
