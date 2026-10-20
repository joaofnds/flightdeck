use gpui::{ListOffset, Pixels, px};

use super::{RowSplice, row_splice};
use crate::live_dashboard::Row;

fn top(item_ix: usize, offset: f32) -> ListOffset {
    ListOffset {
        item_ix,
        offset_in_item: px(offset),
    }
}

fn splice(source: &[Row], target: &[Row], at: ListOffset) -> (usize, Pixels, RowSplice) {
    let splice = row_splice(source, target, at);
    (splice.top.item_ix, splice.top.offset_in_item, splice)
}

#[test]
fn identical_rows_replace_nothing_and_keep_the_top() {
    // Goal: a live tick that keeps the page's rows touches no row. Method: splice equal rows.
    let rows = [Row::Summary, Row::Inventory, Row::App(0)];

    let (item, offset, splice) = splice(&rows, &rows, top(1, 12.0));

    assert_eq!((item, offset), (1, px(12.0)));
    assert!(splice.source.is_empty());
    assert!(splice.target.is_empty());
}

#[test]
fn rows_added_above_the_top_move_its_index_and_keep_its_offset() {
    // Goal: rows appearing above the viewport leave the row the user reads in place.
    // Method: add two rows before the top row and read where the top lands.
    let source = [Row::Summary, Row::Inventory, Row::App(0)];
    let target = [
        Row::Summary,
        Row::App(7),
        Row::App(8),
        Row::Inventory,
        Row::App(0),
    ];

    let (item, offset, splice) = splice(&source, &target, top(2, 30.0));

    assert_eq!((item, offset), (4, px(30.0)));
    assert_eq!(splice.source, 1..1);
    assert_eq!(splice.target, 1..3);
}

#[test]
fn rows_removed_below_the_top_keep_it() {
    // Goal: rows leaving below the viewport do not move it. Method: drop the last row.
    let source = [Row::Summary, Row::Inventory, Row::App(0)];
    let target = [Row::Summary, Row::Inventory];

    let (item, offset, splice) = splice(&source, &target, top(1, 5.0));

    assert_eq!((item, offset), (1, px(5.0)));
    assert_eq!(splice.source, 2..3);
    assert_eq!(splice.target, 2..2);
}

#[test]
fn a_regrouped_top_row_lands_on_the_row_that_holds_its_first_panel() {
    // Goal: crossing a column breakpoint keeps the panel the user reads on screen.
    // Method: regroup three columns into two and put the top on the second row of three.
    let source = [
        Row::Summary,
        Row::OverviewRankings(vec![0, 1, 2]),
        Row::OverviewRankings(vec![3, 4, 5]),
        Row::Inventory,
    ];
    let target = [
        Row::Summary,
        Row::OverviewRankings(vec![0, 1]),
        Row::OverviewRankings(vec![2, 3]),
        Row::OverviewRankings(vec![4, 5]),
        Row::Inventory,
    ];

    let (item, offset, _) = splice(&source, &target, top(2, 40.0));

    assert_eq!((item, offset), (2, px(0.0)));
}

#[test]
fn a_top_panel_regrouped_into_the_row_above_lands_on_that_row() {
    // Goal: widening the window moves the page to the row that now holds the panel the user
    // reads, even when that row keeps its name. Method: merge one-column rows into one row.
    let source = [
        Row::Summary,
        Row::OverviewRankings(vec![0]),
        Row::OverviewRankings(vec![1]),
        Row::Inventory,
    ];
    let target = [
        Row::Summary,
        Row::OverviewRankings(vec![0, 1]),
        Row::Inventory,
    ];

    let (item, offset, splice) = splice(&source, &target, top(2, 40.0));

    assert_eq!((item, offset), (1, px(0.0)));
    assert_eq!(splice.target, 1..2);
}

#[test]
fn a_replaced_top_row_still_in_the_target_keeps_its_offset() {
    // Goal: a row that moves within the replaced run stays the top row.
    // Method: reorder the middle rows and put the top on one of them.
    let source = [Row::Summary, Row::App(1), Row::App(2), Row::Inventory];
    let target = [
        Row::Summary,
        Row::App(2),
        Row::App(3),
        Row::App(1),
        Row::Inventory,
    ];

    let (item, offset, _) = splice(&source, &target, top(1, 9.0));

    assert_eq!((item, offset), (3, px(9.0)));
}

#[test]
fn a_spacer_of_another_height_is_a_different_row() {
    // Goal: a spacer that changes height is spliced so the list measures it again.
    // Method: change the last spacer's height.
    let source = [Row::Summary, Row::Spacer(px(100.0))];
    let target = [Row::Summary, Row::Spacer(px(40.0))];

    let (_, _, splice) = splice(&source, &target, top(0, 0.0));

    assert_eq!(splice.source, 1..2);
    assert_eq!(splice.target, 1..2);
}

#[test]
fn an_empty_list_starts_at_the_top() {
    // Goal: the first rows of a page start at its top. Method: splice into an empty list.
    let target = [Row::Summary, Row::Inventory];

    let (item, offset, splice) = splice(&[], &target, top(0, 0.0));

    assert_eq!((item, offset), (0, px(0.0)));
    assert_eq!(splice.target, 0..2);
}

mod when_the_top_row_is_removed {
    use super::*;

    #[test]
    fn the_top_moves_to_the_start_of_the_rows_replacing_it() {
        // Goal: a vanished top row hands the viewport to the row now in its place.
        // Method: replace the top row with another and read where the top lands.
        let source = [Row::Summary, Row::App(1), Row::Inventory];
        let target = [Row::Summary, Row::App(9), Row::Inventory];

        let (item, offset, _) = splice(&source, &target, top(1, 14.0));

        assert_eq!((item, offset), (1, px(0.0)));
    }

    #[test]
    fn the_top_moves_to_the_next_kept_row_when_nothing_replaces_it() {
        // Goal: a removed top row hands the viewport to the row after it.
        // Method: remove the top row and read where the top lands.
        let source = [Row::Summary, Row::App(1), Row::Inventory];
        let target = [Row::Summary, Row::Inventory];

        let (item, offset, _) = splice(&source, &target, top(1, 14.0));

        assert_eq!((item, offset), (1, px(0.0)));
    }

    #[test]
    fn the_top_stays_on_the_last_row_when_the_page_ends_there() {
        // Goal: removing the last rows never leaves the top past the end.
        // Method: remove the top row at the page's end.
        let source = [Row::Summary, Row::App(1)];
        let target = [Row::Summary];

        let (item, offset, _) = splice(&source, &target, top(1, 14.0));

        assert_eq!((item, offset), (0, px(0.0)));
    }
}
