//! The prototype's table in a bordered panel: a heading at the top, then rows of cells, each
//! column as wide as the table gives it. The table is wider than the page and scrolls sideways,
//! its heading and rows together.

use gpui::{SharedString, div, prelude::*, px, rgb};

use crate::charts::{BLUE, PANEL, TEXT};

const RADIUS: f32 = 7.0;
/// The heading's height with the panel's top border above it.
const HEADER_HEIGHT: f32 = 38.0;
const ROW_HEIGHT: f32 = 65.0;

/// The heading of a table, the top of the prototype's bordered panel, which `cells` fills.
pub fn header(id: &'static str, scroll: &gpui::ScrollHandle) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(HEADER_HEIGHT))
        .border_1()
        .border_b_0()
        .border_color(crate::charts::alpha(TEXT, 0.12))
        .rounded_t(px(RADIUS))
        .bg(rgb(0x2c_2f_35))
        .overflow_x_scroll()
        .restrict_scroll_to_axis()
        .track_scroll(scroll)
        .text_size(px(11.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(rgb(crate::charts::MUTED))
}

/// A row of a table with the panel's sides, and its bottom and corners when it is the last.
pub fn row(id: String, last: bool, scroll: &gpui::ScrollHandle) -> gpui::Stateful<gpui::Div> {
    div()
        .id(SharedString::from(id))
        .h(px(if last { ROW_HEIGHT + 0.5 } else { ROW_HEIGHT }))
        .min_w_0()
        .max_w_full()
        .overflow_x_scroll()
        .restrict_scroll_to_axis()
        .track_scroll(scroll)
        .border_l_1()
        .border_r_1()
        .border_b_1()
        .border_color(crate::charts::alpha(TEXT, 0.12))
        .bg(rgb(PANEL))
        .text_size(px(12.0))
        .text_color(rgb(TEXT))
        .when(last, |row| row.rounded_b(px(RADIUS)))
        .cursor_pointer()
        .focusable()
        .tab_stop(true)
        .focus(crate::charts::focus_ring)
}

/// The cells of one row of a table, each as wide as its column's entry in `widths`. Each cell
/// is found by the selector `selector` gives its column index and text. The name's cell, the
/// first, holds the prototype's button, which `pressed` marks as the row selected.
pub fn cells<const COLUMNS: usize>(
    cells: [String; COLUMNS],
    widths: &[f32; COLUMNS],
    selector: impl Fn(usize, &str) -> String,
    pressed: Option<bool>,
) -> gpui::Div {
    let mut row = cells_row(widths);
    for (index, cell) in cells.into_iter().enumerate() {
        let cell_selector = selector(index, &cell);
        let column = column(index, widths[index]).debug_selector(move || cell_selector.clone());
        row = row.child(match (index, pressed) {
            (0, Some(pressed)) => column.child(
                div()
                    .h(px(44.0))
                    .px(px(8.0))
                    .rounded(px(5.0))
                    .flex()
                    .items_center()
                    .when(pressed, |button| {
                        button
                            .bg(crate::charts::alpha(BLUE, 0.15))
                            .text_color(rgb(BLUE))
                    })
                    .child(cell),
            ),
            _ => column.child(cell),
        });
    }
    row
}

/// The row that holds a row's cells, as wide as all its columns.
pub fn cells_row(widths: &[f32]) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .min_w(px(widths.iter().sum()))
        .h_full()
}

/// The cell of column `index`, `width` wide: the name's, the first, starts at the left, and
/// the rest end at the right.
pub fn column(index: usize, width: f32) -> gpui::Div {
    div()
        .w(px(width))
        .flex_shrink_0()
        .flex()
        .items_center()
        .px(px(14.0))
        .when(index > 0, gpui::Styled::justify_end)
}
