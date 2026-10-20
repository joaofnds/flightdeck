use std::ops::Range;

use gpui::{ListOffset, px};

use crate::live_dashboard::Row;

/// The run of rows a rebuild replaces, and where the row at the top of the viewport lands.
pub(crate) struct RowSplice {
    pub(crate) source: Range<usize>,
    pub(crate) target: Range<usize>,
    pub(crate) top: ListOffset,
}

/// Compares `source` rows with the `target` rows that replace them, keeping the rows both share
/// at the start and at the end, so the list keeps their measured heights, and maps `top` onto the
/// target rows so the row the user reads stays at the top of the viewport.
pub(crate) fn row_splice(source: &[Row], target: &[Row], top: ListOffset) -> RowSplice {
    assert!(source.len() <= 4096);
    assert!(target.len() <= 4096);
    let prefix = source
        .iter()
        .zip(target)
        .take_while(|(source, target)| row_splice_same(source, target))
        .count();
    let suffix = source[prefix..]
        .iter()
        .rev()
        .zip(target[prefix..].iter().rev())
        .take_while(|(source, target)| row_splice_same(source, target))
        .count();
    let source_range = prefix..source.len() - suffix;
    let target_range = prefix..target.len() - suffix;

    let top = if top.item_ix < source.len() {
        if top.item_ix < source_range.start {
            top
        } else {
            if top.item_ix < source_range.end {
                row_splice_top(&source[top.item_ix], target, target_range.clone(), top)
            } else {
                ListOffset {
                    item_ix: top.item_ix + target.len() - source.len(),
                    offset_in_item: top.offset_in_item,
                }
            }
        }
    } else {
        ListOffset {
            item_ix: 0,
            offset_in_item: px(0.0),
        }
    };
    if !target.is_empty() {
        assert!(top.item_ix < target.len());
    }

    RowSplice {
        source: source_range,
        target: target_range,
        top,
    }
}

/// Where the top lands when its row is among the replaced ones: on the same row where the
/// target still has it, on the row that now holds its first panel when the columns regrouped
/// it, and otherwise at the start of the rows that replace it.
fn row_splice_top(row: &Row, target: &[Row], range: Range<usize>, top: ListOffset) -> ListOffset {
    let key = row.layout_key();
    let rows = &target[range.clone()];
    if let Some(index) = rows
        .iter()
        .position(|candidate| candidate.layout_key() == key)
    {
        return ListOffset {
            item_ix: range.start + index,
            offset_in_item: top.offset_in_item,
        };
    }
    let holder = rows
        .iter()
        .position(|candidate| candidate.layout_members().contains(&key));
    ListOffset {
        item_ix: match holder {
            Some(index) => range.start + index,
            None => range.start.min(target.len().saturating_sub(1)),
        },
        offset_in_item: px(0.0),
    }
}

/// Two rows are the same row when they show the same content at the same height, which a spacer
/// shows only at its own height and a regrouped row only with the same panels.
fn row_splice_same(source: &Row, target: &Row) -> bool {
    match (source, target) {
        (Row::Spacer(source), Row::Spacer(target)) => source == target,
        _ => source.layout_members() == target.layout_members(),
    }
}

#[cfg(test)]
#[path = "live_row_splice_tests.rs"]
mod tests;
