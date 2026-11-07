use persistence::{Error, Result};

use super::{Operation, PAGE_BYTES_MAX, Raw, View};
use crate::data::Table;

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
