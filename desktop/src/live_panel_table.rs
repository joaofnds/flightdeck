//! The prototype's table in a bordered panel: a heading at the top, then rows of cells, each
//! column as wide as the table gives it. The table is wider than the page and scrolls sideways,
//! its heading and rows together.

use gpui::{SharedString, div, prelude::*, px, rgb};

use crate::charts::{BLUE, PANEL, TEXT};

/// The width of a table's first column, which names each row, wide enough for a helper's
/// full name.
pub const NAME_WIDTH: f32 = 230.3;
const RADIUS: f32 = 7.0;
/// The heading's height with the panel's top border above it.
const HEADER_HEIGHT: f32 = 38.0;
const ROW_HEIGHT: f32 = 65.0;

/// The heading of a table, the top of the prototype's bordered panel, which `cells` fills. When
/// `last`, no row follows it, so it closes the panel too, and tests then find it by the selector
/// `panel-bottom:` and its id.
pub fn header(
    id: &'static str,
    last: bool,
    scroll: &gpui::ScrollHandle,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(HEADER_HEIGHT))
        .border_1()
        .border_color(crate::charts::alpha(TEXT, 0.12))
        .rounded_t(px(RADIUS))
        .when_else(
            last,
            |header| {
                header
                    .rounded_b(px(RADIUS))
                    .debug_selector(move || format!("panel-bottom:{id}"))
            },
            gpui::Styled::border_b_0,
        )
        .bg(rgb(0x2c_2f_35))
        .overflow_x_scroll()
        .restrict_scroll_to_axis()
        .track_scroll(scroll)
        .text_size(px(11.0))
        .font_weight(gpui::FontWeight::MEDIUM)
        .text_color(rgb(crate::charts::MUTED))
}

/// A row of a table with the panel's sides, and its bottom and corners when it is the last,
/// which tests then find by the selector `panel-bottom:` and the row's id.
pub fn row(id: String, last: bool, scroll: &gpui::ScrollHandle) -> gpui::Stateful<gpui::Div> {
    let id = SharedString::from(id);
    div()
        .id(id.clone())
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
        .when(last, |row| {
            row.rounded_b(px(RADIUS))
                .debug_selector(move || format!("panel-bottom:{id}"))
        })
        .cursor_pointer()
        .focusable()
        .tab_stop(true)
        .focus(crate::charts::focus_ring)
}

/// What the name's cell, the first of a row, draws its text in.
#[derive(Clone, Copy)]
pub enum NameCell {
    /// The text alone, as a heading draws it.
    Text,
    /// The prototype's button, pressed when its row is the one selected, which tests then find by
    /// the selector `selected:` and the name.
    Button { pressed: bool },
}

/// The cells of one row of a table, holding `texts`, each as wide as its column's entry in
/// `widths`, the name's cell drawn as `name_cell` says. Each cell is found by the selector
/// `selector` gives its column index and text.
pub fn cells<const COLUMNS: usize>(
    texts: [String; COLUMNS],
    widths: &[f32; COLUMNS],
    selector: impl Fn(usize, &str) -> String,
    name_cell: NameCell,
) -> gpui::Div {
    let mut row = cells_row(widths);
    for (index, text) in texts.into_iter().enumerate() {
        let cell_selector = selector(index, &text);
        let cell = column(index, widths[index]).debug_selector(move || cell_selector.clone());
        row = row.child(match (index, name_cell) {
            (0, NameCell::Button { pressed }) => cell.child(
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
                            .debug_selector(|| format!("selected:{text}"))
                    })
                    .child(text),
            ),
            _ => cell.child(text),
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
