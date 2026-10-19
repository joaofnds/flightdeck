use gpui::{App, Hsla, IntoElement, Pixels, TextRun, Window, canvas, div, font, point};
use gpui::{prelude::*, px, rgb};

use crate::charts::{PLOT_INSETS, Scale, TEXT, number};

/// The wall-clock seconds a live chart spans, from `first` for `span` seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeWindow {
    pub first: u64,
    pub span: u64,
}

/// Labelled ticks of one value axis, with the gutter width their longest label needs.
pub struct AxisLabels {
    pub width: Pixels,
    entries: Vec<(f32, String)>,
}

const LABEL_SIZE: f32 = 11.0;
const TIME_LABEL_WIDTH: f32 = 60.0;
const TIME_INTERVALS: [u64; 8] = [60, 120, 300, 600, 900, 1800, 3600, 7200];

#[must_use]
pub fn axis_labels(scale: Scale, unit: &str) -> AxisLabels {
    labels(
        scale
            .ticks()
            .map(|value| (scale.fraction(value), axis_label(value, unit)))
            .collect(),
    )
}

/// Value labels at the given fractions of the plot's height, the lowest first.
#[must_use]
pub fn labels(entries: Vec<(f32, String)>) -> AxisLabels {
    let characters = entries
        .iter()
        .map(|(_, label)| label.chars().count())
        .max()
        .unwrap_or(1);
    AxisLabels {
        width: px((number(characters.min(18)) * LABEL_SIZE * 0.6 + 10.0).max(28.0)),
        entries,
    }
}

/// The side of the plot an axis column stands on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AxisSide {
    Leading,
    Trailing,
}

/// Value labels beside a plot of `height`, each centred on its gridline and aligned toward
/// the plot.
#[must_use]
pub fn axis_column(labels: AxisLabels, height: Pixels, side: AxisSide) -> impl IntoElement {
    let plot = height - PLOT_INSETS.top - PLOT_INSETS.bottom;
    let mut column = div()
        .relative()
        .flex_shrink_0()
        .w(labels.width)
        .h(height)
        .text_size(px(LABEL_SIZE))
        .text_color(rgb(TEXT));
    for (fraction, label) in labels.entries {
        let entry = div()
            .absolute()
            .top(PLOT_INSETS.top + plot * (1.0 - fraction) - px(8.0))
            .child(label);
        column = column.child(match side {
            AxisSide::Leading => entry.right(px(2.0)),
            AxisSide::Trailing => entry.left(px(4.0)),
        });
    }
    column
}

/// Local wall-clock labels at the same ticks the plot's time grid uses.
#[must_use]
pub fn time_labels(time: Option<TimeWindow>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, cx| {
            let Some(time) = time else {
                return;
            };

            let plot_width = bounds.size.width - PLOT_INSETS.left - PLOT_INSETS.right;
            for (fraction, second) in time_ticks(time, f32::from(plot_width)) {
                let x = bounds.origin.x + PLOT_INSETS.left + plot_width * fraction;
                let label = clock(second, false);
                paint_label(&label, point(x, bounds.origin.y), plot_width, window, cx);
            }
        },
    )
    .size_full()
}

fn paint_label(
    label: &str,
    center: gpui::Point<Pixels>,
    plot_width: Pixels,
    window: &mut Window,
    cx: &mut App,
) {
    let color: Hsla = rgb(TEXT).into();
    let line = window.text_system().shape_line(
        label.to_owned().into(),
        px(LABEL_SIZE),
        &[TextRun {
            len: label.len(),
            font: font(".SystemUIFont"),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        }],
        None,
    );
    let half = line.width / 2.0;
    let left = (center.x - half).min(center.x + plot_width - line.width);
    if let Err(error) = line.paint(
        point(left.max(center.x - line.width), center.y + px(2.0)),
        px(LABEL_SIZE + 5.0),
        gpui::TextAlign::Left,
        None,
        window,
        cx,
    ) {
        eprintln!("chart time label: {error}");
    }
}

/// Picks the prototype's tick interval for about one label per 60 pixels, aligned to local
/// minutes, and returns each tick's fraction of the window with its second.
#[must_use]
pub fn time_ticks(time: TimeWindow, width: f32) -> Vec<(f32, u64)> {
    let mut ticks = Vec::with_capacity(16);
    if time.span == 0 {
        return ticks;
    }
    let Some(offset) = local_parts(time.first).map(|parts| parts.tm_gmtoff) else {
        return ticks;
    };

    let mut slots = 1_u64;
    for _ in 0..64 {
        if number_u64(slots + 1) * TIME_LABEL_WIDTH > width {
            break;
        }
        slots += 1;
    }
    let desired = time.span.div_ceil(slots);
    let interval = TIME_INTERVALS
        .into_iter()
        .find(|interval| *interval >= desired)
        .unwrap_or(7200);
    let local = time.first.saturating_add_signed(offset);
    let mut second = local.div_ceil(interval) * interval;
    for _ in 0..16 {
        if second > local + time.span {
            break;
        }
        let elapsed = second - local;
        ticks.push((
            number_u64(elapsed) / number_u64(time.span),
            second.saturating_add_signed(-offset),
        ));
        second += interval;
    }
    ticks
}

/// Converts the small second counts of one chart window; larger values saturate.
fn number_u64(value: u64) -> f32 {
    u16::try_from(value).map_or(f32::from(u16::MAX), f32::from)
}

/// Local wall-clock time of `second`, as HH:MM or HH:MM:SS.
#[must_use]
pub fn clock(second: u64, seconds: bool) -> String {
    let Some(parts) = local_parts(second) else {
        return "Unavailable".into();
    };
    if seconds {
        format!(
            "{:02}:{:02}:{:02}",
            parts.tm_hour, parts.tm_min, parts.tm_sec
        )
    } else {
        format!("{:02}:{:02}", parts.tm_hour, parts.tm_min)
    }
}

pub(crate) fn local_parts(second: u64) -> Option<libc::tm> {
    let Ok(timestamp) = libc::time_t::try_from(second) else {
        return None;
    };
    let mut output = std::mem::MaybeUninit::<libc::tm>::uninit();
    // Both pointers are valid for the call; only a successful conversion initializes output.
    let result = unsafe { libc::localtime_r(&raw const timestamp, output.as_mut_ptr()) };
    if result.is_null() {
        return None;
    }
    // localtime_r returned the supplied initialized output pointer.
    Some(unsafe { output.assume_init() })
}

/// Formats an axis value as the approved prototype does: unit-aware scaling and the fewest
/// digits that separate neighbouring ticks.
#[must_use]
pub fn axis_label(value: f32, unit: &str) -> String {
    let magnitude = value.abs();
    let (scaled, suffix) = match unit {
        "GiB" if magnitude < 1.0 => {
            if magnitude == 0.0 {
                (value, "B")
            } else {
                (value * 1024.0, "MiB")
            }
        }
        "MB/s" if magnitude < 0.001 && magnitude > 0.0 => (value * 1_000_000.0, "B/s"),
        "MB/s" if magnitude < 1.0 => {
            if magnitude == 0.0 {
                (value, "B/s")
            } else {
                (value * 1000.0, "kB/s")
            }
        }
        "W" if magnitude > 0.0 && magnitude < 1.0 => (value * 1000.0, "mW"),
        "GHz" if magnitude < 1.0 => (value * 1000.0, "MHz"),
        _ => (value, unit),
    };
    let (scaled, prefix) = thousands(scaled, suffix);
    let number = digits(scaled);
    let suffix = format!("{prefix}{suffix}");
    if suffix.is_empty() {
        return number;
    }
    if suffix.starts_with('%') {
        return format!("{number}{suffix}");
    }
    format!("{number} {suffix}")
}

fn thousands(value: f32, unit: &str) -> (f32, &'static str) {
    let counted = [
        "",
        "rpm",
        "ops/s",
        "work items",
        "pps",
        "/s",
        "files",
        "threads",
        "sockets",
        "pages/s",
        "errors/s",
        "packets/s",
    ];
    if !counted.contains(&unit) {
        return (value, "");
    }
    let magnitude = value.abs();
    if magnitude >= 1_000_000.0 {
        return (
            value / 1_000_000.0,
            if unit.is_empty() { "M" } else { "M " },
        );
    }
    if magnitude >= 1000.0 {
        return (value / 1000.0, if unit.is_empty() { "k" } else { "k " });
    }
    (value, "")
}

fn digits(value: f32) -> String {
    let magnitude = value.abs();
    let mut precision = 0_usize;
    if magnitude > 0.0 && magnitude < 100.0 {
        precision = 1;
        if magnitude < 1.0 {
            let mut scaled = magnitude;
            precision = 1;
            for _ in 0..6 {
                if scaled >= 1.0 {
                    break;
                }
                scaled *= 10.0;
                precision += 1;
            }
            precision = precision.clamp(2, 6);
        }
    }
    let text = format!("{value:.precision$}");
    let trimmed = if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.')
    } else {
        &text
    };
    if trimmed == "-0" {
        return "0".into();
    }
    group(trimmed)
}

fn group(text: &str) -> String {
    let (sign, digits) = text
        .strip_prefix('-')
        .map_or(("", text), |rest| ("-", rest));
    let (whole, fraction) = digits
        .split_once('.')
        .map_or((digits, None), |(a, b)| (a, Some(b)));
    if whole.len() <= 3 {
        return text.to_owned();
    }
    let mut grouped = String::with_capacity(whole.len() + whole.len() / 3);
    for (index, character) in whole.chars().enumerate() {
        if index > 0 && (whole.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(character);
    }
    match fraction {
        Some(fraction) => format!("{sign}{grouped}.{fraction}"),
        None => format!("{sign}{grouped}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn axis_labels_read_like_the_prototype() {
        // Goal: axis labels carry units and the prototype's scaling.
        // Method: format representative ticks of each unit family.
        assert_eq!(axis_label(100.0, "%"), "100%");
        assert_eq!(axis_label(0.0, "%"), "0%");
        assert_eq!(axis_label(48.0, "GiB"), "48 GiB");
        assert_eq!(axis_label(0.0, "GiB"), "0 B");
        assert_eq!(axis_label(0.5, "GiB"), "512 MiB");
        assert_eq!(axis_label(20.0, "MB/s"), "20 MB/s");
        assert_eq!(axis_label(-10.0, "MB/s"), "-10 MB/s");
        assert_eq!(axis_label(0.0, "MB/s"), "0 B/s");
        assert_eq!(axis_label(0.25, "MB/s"), "250 kB/s");
        assert_eq!(axis_label(0.0, "W"), "0 W");
        assert_eq!(axis_label(0.4, "W"), "400 mW");
        assert_eq!(axis_label(2.5, "W"), "2.5 W");
        assert_eq!(axis_label(1500.0, "rpm"), "1.5 k rpm");
        assert_eq!(axis_label(0.6, "ms"), "0.6 ms");
        assert_eq!(axis_label(1024.0, "MiB"), "1,024 MiB");
        assert_eq!(axis_label(0.01, ""), "0.01");
        assert_eq!(axis_label(-0.05, ""), "-0.05");
    }

    #[test]
    fn time_ticks_land_on_local_minutes_inside_the_window() {
        // Goal: time labels sit on whole local minutes chosen by the prototype's intervals.
        // Method: tick a 15-minute window at two widths and check spacing and bounds.
        let time = TimeWindow {
            first: 1_792_410_000,
            span: 900,
        };

        let narrow = time_ticks(time, 300.0);
        let wide = time_ticks(time, 1200.0);

        assert!(narrow.len() >= 3);
        assert!(wide.len() > narrow.len());
        for (fraction, second) in narrow.iter().chain(&wide) {
            assert!((0.0..=1.0).contains(fraction));
            assert_eq!(second % 60, 0);
        }
        assert_eq!(narrow[1].1 - narrow[0].1, 300);
    }
}
