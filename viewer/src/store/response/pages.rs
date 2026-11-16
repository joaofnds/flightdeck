use std::sync::Arc;

use persistence::{Error, Result};
use serde_json::Value;

use super::{QueryContext, QuerySecond, Response};
use crate::Cancellation;
use crate::live_app_response::json;
use crate::live_app_response::{Detail, KeyKind, Observation, Page, RankedApp};
use crate::store::{ProcessAt, Processes};

const HEADER_BYTES: usize = 256 * size_of::<Value>();
const ROW_BYTES: usize = 768 * size_of::<Value>();

impl QueryContext {
    pub fn apps_page_for(
        &self,
        observation: &Observation,
        page: Page,
        cancellation: &Arc<Cancellation>,
        epoch: u64,
    ) -> Result<Response<Value>> {
        let operation = Arc::clone(cancellation).operation(epoch)?;
        let second = self
            .inspect(observation)
            .map_err(|_| Error::Bound("live App stale detail"))?;

        operation.run(|| self.response_apps_page(second, page))
    }

    pub fn app_detail_for(
        &self,
        observation: &Observation,
        kind: KeyKind,
        bytes: &[u8],
        page: Page,
        cancellation: &Arc<Cancellation>,
        epoch: u64,
    ) -> Result<Response<Value>> {
        kind.checked_key(bytes)?;
        let operation = Arc::clone(cancellation).operation(epoch)?;
        let second = self
            .inspect(observation)
            .map_err(|_| Error::Bound("live App stale detail"))?;

        operation.run(|| self.response_app_detail(second, kind, bytes, page))
    }

    pub fn ungrouped_page_for(
        &self,
        observation: &Observation,
        page: Page,
        cancellation: &Arc<Cancellation>,
        epoch: u64,
    ) -> Result<Response<Value>> {
        let operation = Arc::clone(cancellation).operation(epoch)?;
        let second = self
            .inspect(observation)
            .map_err(|_| Error::Bound("live App stale detail"))?;
        let (_, processes) = second.apps().presentation()?;
        let reservation = self.reserve_response(response_members_bytes(processes, None, page)?)?;

        operation.run(|| Response::json(reservation, second.apps().ungrouped_page(page)?))
    }

    fn response_apps_page(&self, second: &QuerySecond, page: Page) -> Result<Response<Value>> {
        second.apps().presentation()?;
        let range = json::page_range(
            page,
            second.apps().app_count(),
            crate::live_app_response::APPS_PAGE_ROWS_MAX,
        )?;
        let mut bytes = HEADER_BYTES;
        second
            .apps()
            .app_names_in(range, |name| bytes += response_app_name_bytes(name));
        let reservation = self.reserve_response(bytes)?;

        Response::json(reservation, second.apps().apps_page(page)?)
    }

    pub(super) fn response_app_detail(
        &self,
        second: &QuerySecond,
        kind: KeyKind,
        bytes: &[u8],
        page: Page,
    ) -> Result<Response<Value>> {
        let apps = second.apps();
        let (_, processes) = apps.presentation()?;
        let Some(app) = apps.app_of(kind, bytes) else {
            let reservation = self.reserve_response(size_of::<Response<Value>>())?;

            return Response::json(reservation, Value::Null);
        };
        let app_index = u32::try_from(app.index()).map_err(|_| Error::Bound("live App index"))?;
        let bytes = response_members_bytes(processes, Some(app_index), page)?
            + response_app_name_bytes(app.name());
        let reservation = self.reserve_response(bytes)?;

        Response::json(
            reservation,
            apps.app_detail(&Detail {
                selected: &second.observation,
                app_index,
                page,
            })?,
        )
    }

    pub(super) fn response_apps_paging(
        &self,
        second: &QuerySecond,
        page: Page,
    ) -> Result<Response<Value>> {
        let header = second.apps().presentation()?.0;
        let reservation = self.reserve_response(HEADER_BYTES)?;

        Response::json(reservation, json::apps_paging(&header, page)?)
    }
}

fn response_app_name_bytes(name: &RankedApp) -> usize {
    ROW_BYTES + 12 * name.bytes.len() + 2 * name.display_name.len()
}

fn response_members_bytes(
    processes: Processes<'_>,
    app_index: Option<u32>,
    page: Page,
) -> Result<usize> {
    let processes = processes.members(app_index);
    let range = json::page_range(
        page,
        processes.clone().count(),
        crate::live_app_response::PROCESSES_PAGE_ROWS_MAX,
    )?;

    Ok(HEADER_BYTES
        + processes
            .skip(range.start)
            .take(range.len())
            .map(|process| response_process_bytes(&process))
            .sum::<usize>())
}

fn response_process_bytes(process: &ProcessAt<'_>) -> usize {
    ROW_BYTES
        + process
            .executable_name()
            .map_or(0, |name| 4 * name.bytes.len() + 2 * name.display.len())
}
