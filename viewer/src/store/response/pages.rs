use std::sync::Arc;

use persistence::{Error, Result};
use serde_json::Value;

use super::{QueryContext, QuerySecond, Response};
use crate::Cancellation;
use crate::live_app_response::json;
use crate::live_app_response::{Detail, KeyKind, Observation, Page};
use crate::store::{AppRow, Payload, ProcessRow};

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
        let (_, payload) = second.apps().presentation()?;
        let reservation = self.reserve_response(response_members_bytes(payload, None, page)?)?;

        operation.run(|| Response::json(reservation, second.apps().ungrouped_page(page)?))
    }

    fn response_apps_page(&self, second: &QuerySecond, page: Page) -> Result<Response<Value>> {
        second.apps().presentation()?;
        let apps = &second.apps().apps;
        let range = json::page_range(page, apps.len())?;
        let bytes = HEADER_BYTES + apps[range].iter().map(response_app_bytes).sum::<usize>();
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
        let (_, payload) = apps.presentation()?;
        let index = apps
            .apps
            .iter()
            .position(|app| app.name.kind.key(&app.name.bytes) == kind.key(bytes));
        let Some(index) = index else {
            let reservation = self.reserve_response(size_of::<Response<Value>>())?;

            return Response::json(reservation, Value::Null);
        };
        let app_index = u32::try_from(index).map_err(|_| Error::Bound("live App index"))?;
        let bytes = response_members_bytes(payload, Some(app_index), page)?
            + response_app_bytes(&apps.apps[index]);
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

fn response_app_bytes(app: &AppRow) -> usize {
    ROW_BYTES + 12 * app.name.bytes.len() + 2 * app.name.display_name.len()
}

fn response_members_bytes(payload: &Payload, app_index: Option<u32>, page: Page) -> Result<usize> {
    let processes = payload
        .iter()
        .filter(|process| process.app_index == app_index);
    let range = json::page_range(page, processes.clone().count())?;

    Ok(HEADER_BYTES
        + processes
            .skip(range.start)
            .take(range.len())
            .map(response_process_bytes)
            .sum::<usize>())
}

fn response_process_bytes(process: &ProcessRow) -> usize {
    ROW_BYTES
        + process
            .executable_name()
            .map_or(0, |name| 4 * name.bytes.len() + 2 * name.display.len())
}
