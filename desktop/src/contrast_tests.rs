//! WCAG 2.2 contrast checks for the colours the app paints.

use gpui::Rgba;
use num_traits::ToPrimitive;

use crate::charts::{GRID, PANEL, utilization_color};
use crate::live_cpu_history::view::{CURSOR_CORE, CURSOR_EDGE, CURSOR_EDGE_OPACITY};

/// Text and the parts of a control that carry meaning must reach this ratio (WCAG 1.4.3, 1.4.11
/// at 3:1 for graphics).
pub(crate) const GRAPHIC_RATIO: f64 = 3.0;

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
