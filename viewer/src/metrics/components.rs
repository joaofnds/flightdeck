//! Saved and native component pairing borrows keys and retains legacy textual order.
use std::collections::BTreeMap;

use persistence::{Error, Result};

use super::reduction::{CounterTotal, CounterValue};
use crate::metric_view::{Key, View};

// Native section validation caps interfaces at 16,384; CPUs and GPU states are smaller.
// Two Rust 1.98 ordered trees fit the capture owner's 8 MiB metric scratch reservation.
const COMPONENTS_MAX: usize = 16_384;
const NODE_BYTES: usize = 11 * (size_of::<[Option<Key<'static>>; 2]>() + size_of::<usize>())
    + 12 * size_of::<usize>()
    + 32;
const _: () = assert!(2 * (COMPONENTS_MAX.div_ceil(5) + 1) * NODE_BYTES < 8 * 1024 * 1024);

type Rows<'a> = BTreeMap<[Option<Key<'a>>; 2], usize>;

pub(super) fn sum(
    current: &View<'_>,
    previous: &View<'_>,
    keys: &[&str],
    columns: &[&str],
) -> Result<Option<u64>> {
    assert!(keys.len() <= 2);
    assert!(columns.len() <= 4);
    if current.len() != previous.len() {
        return Ok(None);
    }
    let before = keyed(previous, keys)?;
    let after = keyed(current, keys)?;
    let mut total = CounterTotal::new();
    for (key, row) in after {
        let Some(old) = before.get(&key) else {
            return Ok(None);
        };
        for column in columns {
            total.add(&CounterValue {
                current: current.integer(row, column)?,
                previous: previous.integer(*old, column)?,
            })?;
            if total.finish().is_none() {
                return Ok(None);
            }
        }
    }
    Ok(total.finish())
}

fn keyed<'a>(table: &'a View<'_>, keys: &[&str]) -> Result<Rows<'a>> {
    if table.len() > COMPONENTS_MAX {
        return Err(Error::Bound("metric component rows"));
    }
    let mut rows = BTreeMap::new();
    for row in table.rows() {
        let mut key = [None, None];
        for (index, name) in keys.iter().enumerate() {
            key[index] = Some(
                table
                    .key(row, name)?
                    .ok_or(Error::Bound("missing component key"))?,
            );
        }
        if rows.insert(key, row).is_some() {
            return Err(Error::Bound("duplicate component"));
        }
    }
    Ok(rows)
}
