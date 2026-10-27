use persistence::{Error, Result};
use serde_json::Value;

use super::{Operation, PAGE_BYTES_MAX, Raw, View};
use crate::byte_budget::ByteReservation;
use crate::data::Table;

pub(crate) struct AdmittedPage {
    pub table: Table,
    pub total: usize,
    pub reservation: ByteReservation,
}

const ENVELOPE_BYTES_MAX: usize = 8 * 1024;

impl Raw {
    pub fn page(&self, name: &str, offset: u64, operation: &Operation) -> Result<Table> {
        let mut native_view = None;
        self.view(name, &mut native_view)?;
        let view = native_view
            .as_ref()
            .ok_or(Error::Bound("live raw page view"))?;
        let range = page_range(offset, view.len())?;
        let order = view.order(operation)?;

        page_format(view, &order[range], view.columns(), operation)
    }

    pub(crate) fn page_admitted(
        &self,
        name: &str,
        offset: u64,
        operation: &Operation,
        output: &mut Option<AdmittedPage>,
        reserve: impl Fn(usize) -> Result<ByteReservation>,
    ) -> Result<()> {
        assert!(output.is_none());
        operation.check()?;
        let view_reservation = reserve(View::native_scratch_bytes(self, name))?;
        let mut native_view = None;
        self.view(name, &mut native_view)?;
        let view = native_view
            .as_ref()
            .ok_or(Error::Bound("live raw page view"))?;
        let range = page_range(offset, view.len())?;
        let order_reservation =
            reserve(view.len() * size_of::<usize>() + view.column_storage_bytes())?;
        let order = view.order(operation)?;
        let columns = view.columns();
        let rows = &order[range];
        let bytes = page_capacity(view, rows, &columns, operation)?;
        let reservation = reserve(bytes)?;
        let table = page_format(view, rows, columns, operation)?;
        *output = Some(AdmittedPage {
            table,
            total: view.len(),
            reservation,
        });
        drop(order);
        drop(order_reservation);
        drop(native_view);
        drop(view_reservation);

        Ok(())
    }
}

fn page_range(offset: u64, count: usize) -> Result<std::ops::Range<usize>> {
    if count > crate::data::ROWS_MAX {
        return Err(Error::Bound("live raw rows"));
    }
    let start = usize::try_from(offset).map_err(|_| Error::Bound("page offset"))?;
    if start > count {
        return Err(Error::Bound("page past end"));
    }

    Ok(start..(start + 512).min(count))
}

fn page_capacity(
    view: &View<'_>,
    rows: &[usize],
    columns: &[String],
    operation: &Operation,
) -> Result<usize> {
    let mut strings = columns.iter().map(String::capacity).sum::<usize>();
    for row in rows {
        operation.check()?;
        for column in columns {
            strings += view.cell(*row, column)?.formatted_capacity(column)?;
        }
    }
    let table = size_of::<Table>()
        + 2 * columns.len() * size_of::<String>()
        + rows.len() * size_of::<Vec<Option<String>>>()
        + rows.len() * columns.len() * size_of::<Option<String>>()
        + strings;
    if table > PAGE_BYTES_MAX {
        return Err(Error::Bound("live raw page admission bytes"));
    }
    let json = columns.len() * size_of::<Value>()
        + rows.len() * (size_of::<Value>() + size_of::<Vec<Value>>())
        + rows.len() * columns.len() * size_of::<Value>();

    Ok(table + json + ENVELOPE_BYTES_MAX)
}

fn page_format(
    view: &View<'_>,
    order: &[usize],
    columns: Vec<String>,
    operation: &Operation,
) -> Result<Table> {
    assert!(order.len() <= 512);
    let mut rows = Vec::with_capacity(order.len());
    let mut bytes = columns.capacity() * size_of::<String>()
        + rows.capacity() * size_of::<Vec<Option<String>>>();
    for column in &columns {
        bytes += column.capacity();
    }
    for index in order {
        operation.check()?;
        let mut row = Vec::with_capacity(columns.len());
        bytes += row.capacity() * size_of::<Option<String>>();
        for column in &columns {
            let cell = view.cell(*index, column)?.format(column)?;
            bytes += cell.as_ref().map_or(0, String::capacity);
            if bytes > PAGE_BYTES_MAX {
                return Err(Error::Bound("live raw page bytes"));
            }
            row.push(cell);
        }
        rows.push(row);
        #[cfg(test)]
        operation.page_row()?;
    }
    operation.check()?;

    Ok(Table { columns, rows })
}
