//! Bounded single-line input using GPUI's Apache-2.0 input example's native input contract.

use std::ops::Range;

use gpui::{
    Bounds, ClipboardItem, Context, ElementInputHandler, EntityInputHandler, FocusHandle,
    IntoElement, Pixels, Point, Render, ShapedLine, UTF16Selection, Window, canvas, div, fill,
    point, prelude::*, px, rgb, size,
};

use crate::native_text::{Edit, Text};

pub struct TimeInput {
    pub text: Text,
    pub submission: Option<String>,
    focus: FocusHandle,
    line: Option<ShapedLine>,
    bounds: Option<Bounds<Pixels>>,
}

impl TimeInput {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            text: Text::default(),
            submission: None,
            focus: cx.focus_handle(),
            line: None,
            bounds: None,
        }
    }

    fn keyboard(
        &mut self,
        event: &gpui::KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;

        if modifiers.control {
            return;
        }

        if modifiers.alt {
            return;
        }

        if modifiers.function {
            return;
        }

        if self.text.marked.is_some() {
            return;
        }

        if modifiers.platform {
            match key {
                "a" => self.text.select(0..self.text.content.len(), false),
                "c" => self.copy(cx),
                "x" => {
                    self.copy(cx);
                    self.replace_text_in_range(None, "", window, cx);
                }
                "v" => {
                    if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
                        self.replace_text_in_range(None, &text, window, cx);
                    }
                }
                _ => return,
            }
        } else {
            match key {
                "enter" => self.submission = Some(self.text.content.clone()),
                "left" => self.text.move_cursor(-1, modifiers.shift),
                "right" => self.text.move_cursor(1, modifiers.shift),
                "home" => self.text.select(0..0, false),
                "end" => {
                    let end = self.text.content.len();
                    self.text.select(end..end, false);
                }
                "backspace" => self.delete(-1, window, cx),
                "delete" => self.delete(1, window, cx),
                _ => return,
            }
        }

        window.notify_text_selection_changed(&self.focus);
        window.prevent_default();
        cx.stop_propagation();
        cx.notify();
    }

    fn copy(&self, cx: &Context<Self>) {
        if !self.text.selection.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.text.content[self.text.selection.clone()].to_owned(),
            ));
        }
    }

    fn delete(&mut self, direction: i32, window: &mut Window, cx: &mut Context<Self>) {
        if self.text.selection.is_empty() {
            self.text.move_cursor(direction, true);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn paint(&mut self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let text: gpui::SharedString = if self.text.content.is_empty() {
            "Unix second / HH:MM:SS".into()
        } else {
            self.text.content.clone().into()
        };
        let style = window.text_style();
        let run = gpui::TextRun {
            len: text.len(),
            font: style.font(),
            color: style.color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window
            .text_system()
            .shape_line(text, px(11.0), &[run], None);
        let selection = &self.text.selection;
        if self.focus.is_focused(window) {
            let left = line.x_for_index(selection.start);
            let right = line.x_for_index(selection.end);
            let width = (right - left).max(px(1.0));
            window.paint_quad(fill(
                Bounds::new(
                    point(bounds.left() + left, bounds.top()),
                    size(width, bounds.size.height),
                ),
                rgb(0x32_46_61),
            ));
        }
        if let Err(error) = line.paint(
            bounds.origin,
            bounds.size.height,
            gpui::TextAlign::Left,
            None,
            window,
            cx,
        ) {
            eprintln!("Time entry text: {error}");
        }
        if let Some(marked) = &self.text.marked {
            window.paint_quad(fill(
                Bounds::new(
                    point(
                        bounds.left() + line.x_for_index(marked.start),
                        bounds.bottom() - px(1.0),
                    ),
                    size(
                        line.x_for_index(marked.end) - line.x_for_index(marked.start),
                        px(1.0),
                    ),
                ),
                rgb(crate::charts::BLUE),
            ));
        }
        self.line = Some(line);
        self.bounds = Some(bounds);
        window.handle_input(
            &self.focus,
            ElementInputHandler::new(bounds, cx.entity()),
            cx,
        );
    }
}

impl Render for TimeInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        div()
            .id("inspection-time")
            .debug_selector(|| "inspection-time".into())
            .accessibility_id("flightdeck.inspection-time")
            .role(gpui::Role::TextInput)
            .aria_label("Inspect recording time")
            .aria_value(self.text.content.clone())
            .aria_placeholder("Unix second or local HH:MM:SS")
            .track_focus(&self.focus)
            .tab_stop(true)
            .w(px(162.0))
            .h(px(29.0))
            .px(px(7.0))
            .py(px(6.0))
            .rounded(px(5.0))
            .bg(rgb(0x22_25_2b))
            .overflow_hidden()
            .on_key_down(cx.listener(Self::keyboard))
            .on_mouse_down(
                gpui::MouseButton::Left,
                cx.listener(|this, event: &gpui::MouseDownEvent, window, cx| {
                    window.focus(&this.focus, cx);
                    if let Some(line) = &this.line {
                        if let Some(bounds) = this.bounds {
                            let index = line.closest_index_for_x(event.position.x - bounds.left());
                            if this.text.select_from_geometry(line.text.as_ref(), index) {
                                window.notify_text_selection_changed(&this.focus);
                                cx.notify();
                            }
                        }
                    }
                }),
            )
            .child(
                canvas(
                    |_, _, _| (),
                    move |bounds, (), window, cx| {
                        entity.update(cx, |this, cx| this.paint(bounds, window, cx));
                    },
                )
                .size_full(),
            )
    }
}

impl EntityInputHandler for TimeInput {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        actual: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.text.utf8_range(&range);
        *actual = Some(self.text.utf16_range(&range));
        Some(self.text.content[range].to_owned())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.text.utf16_range(&self.text.selection),
            reversed: self.text.reversed,
        })
    }

    fn set_selected_text_range(
        &mut self,
        range: Range<usize>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.text.select(self.text.utf8_range(&range), false);
        window.notify_text_selection_changed(&self.focus);
        cx.notify();
    }

    fn text_length_utf16(
        &mut self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.text.content.encode_utf16().count())
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.text
            .marked
            .as_ref()
            .map(|range| self.text.utf16_range(range))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.text.marked = None;
        window.notify_text_input_changed(&self.focus);
        cx.notify();
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.text.replace(&Edit {
            replacement_range: range,
            text,
            composition_selection: None,
            composing: false,
        }) {
            self.line = None;
            self.bounds = None;
            window.notify_text_input_changed(&self.focus);
            cx.notify();
        } else {
            window.play_system_bell();
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.text.replace(&Edit {
            replacement_range: range,
            text,
            composition_selection: selected,
            composing: true,
        }) {
            self.line = None;
            self.bounds = None;
            window.notify_text_input_changed(&self.focus);
            cx.notify();
        } else {
            window.play_system_bell();
        }
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let line = self.line.as_ref()?;
        if line.text.as_ref() != self.text.content {
            return None;
        }

        let range = self.text.utf8_range(&range);

        Some(Bounds::from_corners(
            point(bounds.left() + line.x_for_index(range.start), bounds.top()),
            point(bounds.left() + line.x_for_index(range.end), bounds.bottom()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<usize> {
        let bounds = self.bounds?;
        let line = self.line.as_ref()?;
        if line.text.as_ref() != self.text.content {
            return None;
        }

        Some(
            self.text
                .utf16_offset(line.closest_index_for_x(point.x - bounds.left())),
        )
    }

    fn selection_bounds_for_range(
        &mut self,
        range: Range<usize>,
        bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Vec<Bounds<Pixels>> {
        self.bounds_for_range(range, bounds, window, cx)
            .into_iter()
            .collect()
    }
}
