use std::ops::Range;

pub const TEXT_BYTES_MAX: usize = 128;

#[derive(Default)]
pub struct Text {
    pub content: String,
    pub selection: Range<usize>,
    pub marked: Option<Range<usize>>,
    pub reversed: bool,
}

pub struct Edit<'a> {
    pub replacement_range: Option<Range<usize>>,
    pub text: &'a str,
    pub composition_selection: Option<Range<usize>>,
    pub composing: bool,
}

impl Text {
    pub fn utf8_offset(&self, offset: usize) -> usize {
        let mut count = 0;
        for (index, character) in self.content.char_indices().take(TEXT_BYTES_MAX) {
            if count >= offset {
                return index;
            }

            count += character.len_utf16();
        }

        self.content.len()
    }

    pub fn utf16_offset(&self, offset: usize) -> usize {
        self.content[..offset].encode_utf16().count()
    }

    pub fn utf8_range(&self, range: &Range<usize>) -> Range<usize> {
        let start = self.utf8_offset(range.start);
        let end = self.utf8_offset(range.end.max(range.start));
        start..end
    }

    pub fn utf16_range(&self, range: &Range<usize>) -> Range<usize> {
        self.utf16_offset(range.start)..self.utf16_offset(range.end)
    }

    pub fn replace(&mut self, edit: &Edit<'_>) -> bool {
        let range = match &edit.replacement_range {
            Some(range) => self.utf8_range(range),
            None => self
                .marked
                .clone()
                .unwrap_or_else(|| self.selection.clone()),
        };
        let retained = self.content.len() - (range.end - range.start);
        if edit.text.len() > TEXT_BYTES_MAX.saturating_sub(retained) {
            return false;
        }

        let end = range.start + edit.text.len();

        self.content.replace_range(range.clone(), edit.text);
        self.selection = end..end;
        self.reversed = false;
        self.marked = None;
        if edit.composing {
            if !edit.text.is_empty() {
                self.marked = Some(range.start..end);
            }
            if let Some(selected) = &edit.composition_selection {
                let start = self.utf16_offset(range.start);
                let length = edit.text.encode_utf16().count();

                self.selection = self.utf8_range(
                    &(start + selected.start.min(length)..start + selected.end.min(length)),
                );
            }
        }

        assert!(self.content.len() <= TEXT_BYTES_MAX);
        assert!(self.selection.start <= self.selection.end);
        assert!(self.selection.end <= self.content.len());

        true
    }

    pub fn move_cursor(&mut self, direction: i32, extend: bool) {
        if !extend {
            if !self.selection.is_empty() {
                let next = if direction < 0 {
                    self.selection.start
                } else {
                    self.selection.end
                };

                self.select(next..next, false);
                return;
            }
        }

        let cursor = if self.reversed {
            self.selection.start
        } else {
            self.selection.end
        };
        let anchor = if self.reversed {
            self.selection.end
        } else {
            self.selection.start
        };
        let next = if direction < 0 {
            self.content[..cursor]
                .char_indices()
                .next_back()
                .map_or(0, |(index, _)| index)
        } else {
            self.content[cursor..]
                .chars()
                .next()
                .map_or(cursor, |character| cursor + character.len_utf8())
        };

        if extend {
            self.select(anchor.min(next)..anchor.max(next), next < anchor);
        } else {
            self.select(next..next, false);
        }
    }

    pub fn select(&mut self, range: Range<usize>, reversed: bool) {
        assert!(range.start <= range.end);
        assert!(range.end <= self.content.len());
        assert!(self.content.is_char_boundary(range.start));
        assert!(self.content.is_char_boundary(range.end));
        self.selection = range;
        self.reversed = reversed;
    }

    pub fn select_from_geometry(&mut self, painted_text: &str, index: usize) -> bool {
        if painted_text != self.content {
            return false;
        }

        if index > self.content.len() {
            return false;
        }

        if !self.content.is_char_boundary(index) {
            return false;
        }

        self.select(index..index, false);

        true
    }
}

pub fn inspection_second(text: &str, seconds: &[u64]) -> Result<u64, String> {
    if text.len() > TEXT_BYTES_MAX {
        return Err("Time entry is too long".into());
    }

    if text.is_empty() {
        return Err("Enter a Unix second or local HH:MM:SS".into());
    }

    if text.bytes().all(|byte| byte.is_ascii_digit()) {
        return text
            .parse()
            .map_err(|_| "Unix second is out of range".into());
    }

    let bytes = text.as_bytes();
    if bytes.len() != 8 {
        return Err("Use a Unix second or local HH:MM:SS".into());
    }

    if bytes[2] != b':' {
        return Err("Use a Unix second or local HH:MM:SS".into());
    }

    if bytes[5] != b':' {
        return Err("Use a Unix second or local HH:MM:SS".into());
    }

    let mut found = None;
    for second in seconds.iter().take(4096) {
        if crate::live_presentation::clock(*second, true) == text {
            if let Some(previous) = found {
                if previous != *second {
                    return Err("This clock time is ambiguous; enter its Unix second".into());
                }
            }
            found = Some(*second);
        }
    }
    found.ok_or_else(|| "That local time is outside the loaded history".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_ascii_geometry_cannot_split_a_new_emoji_after_native_replacement() {
        // A click can arrive between IME replacement and paint, carrying the old byte index.
        let mut text = Text::default();
        assert!(text.replace(&Edit {
            replacement_range: None,
            text: "ab",
            composition_selection: None,
            composing: false
        }));
        assert!(text.replace(&Edit {
            replacement_range: Some(0..2),
            text: "🙂",
            composition_selection: None,
            composing: false
        }));
        assert!(!text.select_from_geometry("ab", 2));
        assert_eq!(text.selection, 4..4);
        assert_eq!(text.utf16_range(&text.selection), 2..2);
        assert!(!text.select_from_geometry("🙂", 2));
        assert!(text.select_from_geometry("🙂", 4));
        assert!(text.replace(&Edit {
            replacement_range: None,
            text: "9",
            composition_selection: None,
            composing: false
        }));
        assert_eq!(text.content, "🙂9");
    }

    #[test]
    fn shift_arrows_extend_from_the_active_end_and_plain_arrows_collapse() {
        // Repeated left selection keeps its original anchor, including direction reversal.
        let mut text = Text::default();
        assert!(text.replace(&Edit {
            replacement_range: None,
            text: "abc",
            composition_selection: None,
            composing: false
        }));
        text.move_cursor(-1, true);
        text.move_cursor(-1, true);
        assert_eq!(text.selection, 1..3);
        assert!(text.reversed);
        text.move_cursor(1, true);
        assert_eq!(text.selection, 2..3);
        text.move_cursor(-1, false);
        assert_eq!(text.selection, 2..2);
        assert!(!text.reversed);
        text.move_cursor(1, true);
        text.move_cursor(1, false);
        assert_eq!(text.selection, 3..3);
    }

    #[test]
    fn native_composition_replaces_marked_utf16_range_without_splitting_unicode() {
        // IME updates use UTF16 offsets, including the surrogate pair before the composition.
        let mut text = Text::default();
        assert!(text.replace(&Edit {
            replacement_range: None,
            text: "A🙂Z",
            composition_selection: None,
            composing: false
        }));
        assert!(text.replace(&Edit {
            replacement_range: Some(1..3),
            text: "é🙂",
            composition_selection: Some(1..3),
            composing: true
        }));
        assert_eq!(text.content, "Aé🙂Z");
        assert_eq!(text.selection, 3..7);
        assert_eq!(text.marked, Some(1..7));
        assert!(text.replace(&Edit {
            replacement_range: None,
            text: "漢",
            composition_selection: Some(1..1),
            composing: true
        }));
        assert_eq!(text.content, "A漢Z");
        assert_eq!(text.selection, 4..4);
        assert!(text.replace(&Edit {
            replacement_range: None,
            text: "2",
            composition_selection: None,
            composing: false
        }));
        assert_eq!(text.content, "A2Z");
        assert_eq!(text.marked, None);
    }

    #[test]
    fn oversized_paste_preserves_content_selection_and_composition() {
        // Refusing an external paste must leave the ongoing IME transaction untouched.
        let mut text = Text::default();
        assert!(text.replace(&Edit {
            replacement_range: None,
            text: "漢",
            composition_selection: Some(0..1),
            composing: true
        }));
        assert!(!text.replace(&Edit {
            replacement_range: None,
            text: &"x".repeat(TEXT_BYTES_MAX + 1),
            composition_selection: None,
            composing: false
        }));
        assert_eq!(text.content, "漢");
        assert_eq!(text.selection, 0..3);
        assert_eq!(text.marked, Some(0..3));
    }

    #[test]
    fn second_entry_refuses_overflow_and_unloaded_clock_times() {
        // Exact epoch input can inspect gaps; clock input resolves only the loaded context.
        assert_eq!(inspection_second("1700000001", &[]).unwrap(), 1_700_000_001);
        assert!(inspection_second("18446744073709551616", &[]).is_err());
        assert!(inspection_second("25:99:99", &[1_700_000_001]).is_err());
        assert!(inspection_second("1:02:03", &[]).is_err());
        assert!(inspection_second("🙂", &[]).is_err());
        let local = crate::live_presentation::clock(1_700_000_001, true);
        assert_eq!(
            inspection_second(&local, &[1_700_000_001]).unwrap(),
            1_700_000_001
        );
    }

    #[test]
    fn ambiguous_clock_entry_requires_an_exact_unix_second() {
        // Equal local clocks on separate days cannot silently choose one observation.
        let first = 1_700_000_001;
        let following = first + 86_400;
        let local = crate::live_presentation::clock(first, true);
        assert_eq!(local, crate::live_presentation::clock(following, true));

        let ambiguous = inspection_second(&local, &[first, following]);

        assert_eq!(
            ambiguous,
            Err("This clock time is ambiguous; enter its Unix second".into())
        );
        assert_eq!(
            inspection_second(&following.to_string(), &[first, following]).unwrap(),
            following
        );
    }
}
