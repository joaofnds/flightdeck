//! WCAG 2.2 contrast checks for the colours the app paints.

use gpui::Rgba;
use num_traits::ToPrimitive;

use crate::charts::{
    BLUE, FOCUS_RING, GREEN, GRID, MUTED, ORANGE, PANEL, PURPLE, RED, TEXT, YELLOW,
    utilization_color,
};
use crate::live_cpu_history::view::{CURSOR_CORE, CURSOR_EDGE, CURSOR_EDGE_OPACITY};

/// Text and the parts of a control that carry meaning must reach this ratio (WCAG 1.4.3, 1.4.11
/// at 3:1 for graphics).
pub(crate) const GRAPHIC_RATIO: f64 = 3.0;
/// Text below 18.66 pixels bold or 24 pixels regular must reach this ratio (WCAG 1.4.3).
pub(crate) const TEXT_RATIO: f64 = 4.5;
/// The page background behind the cards, and the tooltip's fill.
const PAGE: u32 = 0x0011_1217;

fn channel_byte(value: f32) -> u32 {
    let Some(byte) = (value.clamp(0.0, 1.0) * 255.0).round().to_u32() else {
        panic!("A clamped channel fits u32");
    };
    byte
}

pub(crate) fn hex(color: Rgba) -> u32 {
    channel_byte(color.r) << 16 | channel_byte(color.g) << 8 | channel_byte(color.b)
}

/// Relative luminance, as WCAG defines it.
pub(crate) fn luminance(color: u32) -> f64 {
    let channel = |shift: u32| {
        let Ok(byte) = u8::try_from((color >> shift) & 0xff) else {
            panic!("A masked channel fits u8");
        };
        let level = f64::from(byte) / 255.0;
        if level <= 0.039_28 {
            level / 12.92
        } else {
            ((level + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
}

pub(crate) fn contrast(first: u32, second: u32) -> f64 {
    let (high, low) = if luminance(first) >= luminance(second) {
        (luminance(first), luminance(second))
    } else {
        (luminance(second), luminance(first))
    };
    (high + 0.05) / (low + 0.05)
}

/// The colour a translucent foreground leaves over an opaque background.
pub(crate) fn over(foreground: u32, opacity: f32, background: u32) -> u32 {
    let mix = |shift: u32| {
        let byte = |color: u32| {
            let Ok(byte) = u8::try_from((color >> shift) & 0xff) else {
                panic!("A masked channel fits u8");
            };
            f32::from(byte)
        };
        let Some(blended) = (byte(foreground) * opacity + byte(background) * (1.0 - opacity))
            .round()
            .to_u32()
        else {
            panic!("A blend of two bytes fits u32");
        };
        blended
    };
    mix(16) << 16 | mix(8) << 8 | mix(0)
}

#[test]
fn the_heatmap_cursor_holds_three_to_one_over_every_cell_colour_and_the_gaps() {
    // Goal: the cursor over the CPU heatmap is as visible over light green and yellow cells as
    // over red cells and the dark plot behind a gap. Method: composite the cursor's edge over
    // the scale's colours and the plot colours, then require the edge or the core to hold 3:1
    // against each of them, and the core to hold it against the edge.
    let backgrounds = [0.0, 25.0, 50.0, 75.0, 100.0]
        .map(|percent| hex(utilization_color(percent)))
        .into_iter()
        .chain([PANEL, GRID]);

    for background in backgrounds {
        let edge = over(CURSOR_EDGE, CURSOR_EDGE_OPACITY, background);
        let seen = contrast(edge, background).max(contrast(CURSOR_CORE, background));
        assert!(seen >= GRAPHIC_RATIO, "cursor over {background:06x}");
        assert!(
            contrast(CURSOR_CORE, edge) >= GRAPHIC_RATIO,
            "core over its edge on {background:06x}"
        );
    }
}

/// The colour a `0x` literal writes, where it is 24 bits wide, with or without a zero alpha
/// byte in front. Anything else, such as an `rgba` value, is not a plain fill.
fn hex_literal(after_prefix: &str) -> Option<u32> {
    let digits: String = after_prefix
        .chars()
        .take_while(|c| c.is_ascii_hexdigit() || *c == '_')
        .filter(|c| *c != '_')
        .collect();
    let digits = match digits.len() {
        8 => digits.strip_prefix("00")?,
        _ => digits.as_str(),
    };
    if digits.len() != 6 {
        return None;
    }

    u32::from_str_radix(digits, 16).ok()
}

/// Every plain colour literal on a line that sets a background, as the views paint them.
fn painted_fills() -> Vec<(String, u32)> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let Ok(entries) = std::fs::read_dir(&root) else {
        panic!("src is readable");
    };
    let mut fills = Vec::new();
    for entry in entries {
        let Ok(entry) = entry else {
            panic!("a src entry is readable");
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_rust = entry.path().extension().is_some_and(|ext| ext == "rs");
        if !is_rust || name.contains("test") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(entry.path()) else {
            panic!("{name} is readable");
        };
        for (number, line) in text.lines().enumerate() {
            if !line.contains(".bg(") {
                continue;
            }
            for literal in line.split("0x").skip(1) {
                if let Some(color) = hex_literal(literal) {
                    fills.push((format!("{name}:{}", number + 1), color));
                }
            }
        }
    }
    fills
}

#[test]
fn text_holds_four_and_a_half_to_one_on_every_fill_a_view_paints() {
    // Goal: no label, value or note the views draw in the two text colours falls below WCAG
    // 1.4.3 on a card, a button, a hovered row or a selected row. Method: read every plain
    // colour literal on a background line of the sources and compare both colours with it.
    let fills = painted_fills();
    assert!(fills.len() > 10, "found {} fills", fills.len());

    for (place, fill) in fills
        .into_iter()
        .chain([("card".into(), PANEL), ("page".into(), PAGE)])
    {
        for (name, text) in [("TEXT", TEXT), ("MUTED", MUTED)] {
            let ratio = contrast(text, fill);
            assert!(
                ratio >= TEXT_RATIO,
                "{name} on {fill:06x} at {place} is {ratio:.2}:1"
            );
        }
    }
}

#[test]
fn series_colours_hold_their_ratios_on_the_card_and_the_page() {
    // Goal: a series' line, swatch and legend text stay readable on the dark card. Method:
    // compare each series colour with the card and the page at the text ratio, which also
    // covers the 3:1 a line or swatch needs, and the focus ring at 3:1.
    for (name, colour) in [
        ("BLUE", BLUE),
        ("GREEN", GREEN),
        ("ORANGE", ORANGE),
        ("PURPLE", PURPLE),
        ("RED", RED),
        ("YELLOW", YELLOW),
    ] {
        for background in [PANEL, PAGE] {
            let ratio = contrast(colour, background);
            assert!(
                ratio >= TEXT_RATIO,
                "{name} on {background:06x} is {ratio:.2}:1"
            );
        }
    }
    for background in [PANEL, PAGE] {
        assert!(contrast(FOCUS_RING, background) >= GRAPHIC_RATIO);
    }
}
