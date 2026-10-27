//! Raw formatting is explicit demand over the original selected raw owner.
use std::sync::Arc;

use persistence::{Error, Result};
use serde_json::{Value, json};

use super::{QueryContext, Response};
use crate::Cancellation;
use crate::live_app_response::Observation;

impl QueryContext {
    pub fn raw_page(
        &self,
        table: &str,
        offset: u64,
        cancellation: &Arc<Cancellation>,
        epoch: u64,
    ) -> Result<Response<Value>> {
        self.raw_page_for(
            &self.selected().observation,
            table,
            offset,
            cancellation,
            epoch,
        )
    }

    pub fn raw_page_for(
        &self,
        observation: &Observation,
        table: &str,
        offset: u64,
        cancellation: &Arc<Cancellation>,
        epoch: u64,
    ) -> Result<Response<Value>> {
        let operation = Arc::clone(cancellation).operation(epoch)?;
        operation.check()?;
        if !offset.is_multiple_of(512) {
            return Err(Error::Bound("page offset"));
        }
        let reading = self
            .raw_reading(observation)
            .ok_or(Error::Bound("selected raw observation is not retained"))?;
        let second = self
            .inspect(observation)
            .map_err(|_| Error::Bound("raw selection"))?;
        let mut admitted = None;
        reading
            .raw()
            .page_admitted(table, offset, &operation, &mut admitted, |bytes| {
                self.reserve_response(bytes)
            })?;
        let crate::live_raw::AdmittedPage {
            table: page,
            total,
            mut reservation,
        } = admitted.ok_or(Error::Bound("raw page admission"))?;
        let mut value = json!({"name":table,"offset":offset,"total":total,
            "observation":super::context_response::identity(
                observation, second.apps().metadata.origin)});
        raw_page_rows(page, &mut value)?;
        crate::live_app_response::json::bounded_bytes(&value, crate::live_raw::PAGE_BYTES_MAX)
            .map_err(|_| Error::Bound("raw response bytes"))?;
        let bytes = size_of::<Response<Value>>() + super::response::json_charge(&value)?;
        if bytes > reservation.bytes() {
            return Err(Error::Bound("raw response admission bytes"));
        }
        reservation.shrink_to(bytes);
        operation.check()?;

        Ok(Response::reserved(reservation, value))
    }
}

fn raw_page_rows(page: crate::data::Table, value: &mut Value) -> Result<()> {
    let object = value
        .as_object_mut()
        .ok_or(Error::Bound("raw response shape"))?;
    let columns = page.columns.into_iter().map(Value::String).collect();
    object.insert("columns".to_owned(), Value::Array(columns));
    let rows = page
        .rows
        .into_iter()
        .map(|row| {
            Value::Array(
                row.into_iter()
                    .map(|cell| cell.map_or(Value::Null, Value::String))
                    .collect(),
            )
        })
        .collect();
    object.insert("rows".to_owned(), Value::Array(rows));

    Ok(())
}
