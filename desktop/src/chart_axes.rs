use gpui::{App, Hsla, IntoElement, Pixels, TextRun, Window, canvas, div, font, point};
use gpui::{prelude::*, px, rgb};

use crate::charts::{PLOT_INSETS, Scale, TEXT};

/// The wall-clock seconds a live chart spans, from `first` for `span` seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimeWindow {
    pub first: u64,
    pub span: u64,
}

impl TimeWindow {
    /// The 900 seconds a live chart holds, ending at `last_second`, the last second of the
    /// selection's context.
    #[must_use]
    pub fn live(last_second: u64) -> Self {
        Self {
            first: last_second.saturating_sub(899),
            span: 900,
        }
    }

    /// The second under `fraction` of the window's width, where the right edge belongs to the
    /// last second.
    #[must_use]
    pub fn second_at(self, fraction: f32) -> Option<u64> {
        if !(0.0..=1.0).contains(&fraction) {
            return None;
        }
        let Ok(span) = u16::try_from(self.span) else {
            return None;
        };
        let offset = num_traits::ToPrimitive::to_u64(&(fraction * f32::from(span)).floor())?;

        self.first
            .checked_add(offset.min(self.span.checked_sub(1)?))
    }

    /// Where `second` sits across the window, at the middle of that second.
    #[must_use]
    pub fn fraction_of(self, second: u64) -> Option<f32> {
        let offset = second.checked_sub(self.first)?;
        if offset >= self.span {
            return None;
        }
        let (Ok(offset), Ok(span)) = (u16::try_from(offset), u16::try_from(self.span)) else {
            return None;
        };

        Some((f32::from(offset) + 0.5) / f32::from(span))
    }
}

/// Labelled ticks of one value axis, with the gutter width their longest label needs.
pub struct AxisLabels {
    pub width: Pixels,
    entries: Vec<(f32, String)>,
}

const LABEL_SIZE: f32 = 12.0;
/// A label character's width as a share of the label size, as the prototype estimates it.
const GLYPH_WIDTH: f32 = 0.62;
const GUTTER_PADDING: f32 = 16.0;
const GUTTER_MIN: f32 = 28.0;
const GUTTER_MAX: f32 = 128.0;
/// The gap between a value label and the plot it labels, and the margin beyond the labels.
const LABEL_GAP: f32 = 7.0;
/// Where a time label's line starts below the plot, in a row `TIME_ROW_HEIGHT` tall.
const TIME_LABEL_TOP: f32 = 8.5;
pub const TIME_ROW_HEIGHT: f32 = 28.0;
const LABEL_LINE: f32 = 15.0;
const TIME_LABEL_WIDTH: f32 = 60.0;
const TIME_INTERVALS: [u64; 14] = [
    1, 2, 5, 10, 15, 30, 60, 120, 300, 600, 900, 1800, 3600, 7200,
];

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
    let longest = entries
        .iter()
        .map(|(_, label)| label.chars().count())
        .max()
        .unwrap_or(0);
    AxisLabels {
        width: gutter_width(longest),
        entries,
    }
}

/// The prototype's gutter for a value axis: its longest label's characters at 0.62 of the label
/// size, plus 16 pixels, kept between 28 and 128 pixels.
fn gutter_width(characters: usize) -> Pixels {
    let characters = u16::try_from(characters).map_or(f32::from(u16::MAX), f32::from);

    px((characters * LABEL_SIZE * GLYPH_WIDTH + GUTTER_PADDING).clamp(GUTTER_MIN, GUTTER_MAX))
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
        .line_height(px(LABEL_LINE))
        .text_color(rgb(TEXT));
    for (fraction, label) in labels.entries {
        let entry = div()
            .absolute()
            .top(PLOT_INSETS.top + plot * (1.0 - fraction) - px(LABEL_LINE / 2.0))
            .child(label);
        column = column.child(match side {
            AxisSide::Leading => entry.right(px(LABEL_GAP)),
            AxisSide::Trailing => entry.left(px(LABEL_GAP)),
        });
    }
    column
}

/// Local wall-clock labels at the same ticks the plot's time grid uses.
#[must_use]
pub fn time_labels(time: Option<TimeWindow>) -> impl IntoElement {
    time_labels_between(time, PLOT_INSETS.left, PLOT_INSETS.right)
}

/// Local wall-clock labels for a plot that starts `left` pixels in from the labels' left edge
/// and ends `right` pixels before their right edge.
#[must_use]
pub fn time_labels_between(
    time: Option<TimeWindow>,
    left: Pixels,
    right: Pixels,
) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, cx| {
            let Some(time) = time else {
                return;
            };

            let plot_width = bounds.size.width - left - right;
            let ticks = time_ticks(time, f32::from(plot_width));
            for (fraction, label) in tick_labels(&ticks) {
                let x = bounds.origin.x + left + plot_width * fraction;
                paint_label(
                    &label,
                    point(x, bounds.origin.y),
                    plot_width,
                    None,
                    (window, cx),
                );
            }
        },
    )
    .size_full()
}

/// Elapsed-time labels at five equal steps across a run of `span` seconds, the last one ending
/// at the plot's right edge, for a plot that starts `left` pixels in from the labels' left edge
/// and ends `right` pixels before their right edge.
#[must_use]
pub fn elapsed_labels(span: u64, left: Pixels, right: Pixels) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, (), window, cx| {
            let plot_width = bounds.size.width - left - right;
            for step in 0..=5_u64 {
                let seconds = span * step / 5;
                let x = bounds.origin.x + left + plot_width * (number_u64(step) / 5.0);
                let label = format!("{}:{:02}", seconds / 60, seconds % 60);
                let anchor = (step == 5).then_some(LabelAnchor::End);
                paint_label(
                    &label,
                    point(x, bounds.origin.y),
                    plot_width,
                    anchor,
                    (window, cx),
                );
            }
        },
    )
    .size_full()
}

/// Where a label sits against the point it labels, when not centered on it.
#[derive(Clone, Copy)]
enum LabelAnchor {
    End,
}

fn paint_label(
    label: &str,
    center: gpui::Point<Pixels>,
    plot_width: Pixels,
    anchor: Option<LabelAnchor>,
    (window, cx): (&mut Window, &mut App),
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
    let left = match anchor {
        Some(LabelAnchor::End) => center.x - line.width,
        None => left.max(center.x - line.width),
    };
    if let Err(error) = line.paint(
        point(left, center.y + px(TIME_LABEL_TOP)),
        px(LABEL_LINE),
        gpui::TextAlign::Left,
        None,
        window,
        cx,
    ) {
        eprintln!("chart time label: {error}");
    }
}

/// The clock label of each tick, with seconds when any tick falls between two minutes.
#[must_use]
pub fn tick_labels(ticks: &[(f32, u64)]) -> Vec<(f32, String)> {
    let seconds = ticks.iter().any(|(_, second)| second % 60 != 0);

    ticks
        .iter()
        .map(|(fraction, second)| (*fraction, clock(*second, seconds)))
        .collect()
}

/// Picks the prototype's tick interval for about one label per 60 pixels, aligned to local
/// time, and returns each tick's fraction of the window with its second.
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

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// The local date and zone abbreviation of `second`, as in `15 Oct 2026 CEST`.
#[must_use]
pub fn date_with_zone(second: u64) -> String {
    let Some(parts) = local_parts(second) else {
        return "Unavailable".into();
    };
    let Some(date) = date(&parts) else {
        return "Unavailable".into();
    };

    format!("{date} {}", zone(&parts))
}

fn date(parts: &libc::tm) -> Option<String> {
    let index = usize::try_from(parts.tm_mon).ok()?;
    let month = MONTHS.get(index)?;

    Some(format!(
        "{} {month} {}",
        parts.tm_mday,
        i64::from(parts.tm_year) + 1900
    ))
}

fn zone(parts: &libc::tm) -> String {
    if parts.tm_zone.is_null() {
        return String::new();
    }
    // localtime_r points tm_zone at a NUL-terminated abbreviation that outlives this call.
    let name = unsafe { std::ffi::CStr::from_ptr(parts.tm_zone) };

    name.to_string_lossy().into_owned()
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

    #[test]
    fn time_ticks_of_a_short_window_land_on_whole_seconds() {
        // Goal: a zoomed-in chart still gets a label about every 60 pixels.
        // Method: tick a one-minute window 600 pixels wide and check the spacing and bounds.
        let time = TimeWindow {
            first: 1_792_410_000,
            span: 60,
        };

        let ticks = time_ticks(time, 600.0);

        assert!(ticks.len() >= 6);
        for (fraction, second) in &ticks {
            assert!((0.0..=1.0).contains(fraction));
            assert!((time.first..=time.first + time.span).contains(second));
        }
        assert_eq!(ticks[1].1 - ticks[0].1, 10);
    }

    #[test]
    fn tick_labels_show_seconds_only_when_ticks_fall_between_minutes() {
        // Goal: labels stay HH:MM across minutes and gain seconds when ticks are closer.
        // Method: label the ticks of a 15-minute window and of a one-minute window.
        let long = TimeWindow {
            first: 1_792_410_000,
            span: 900,
        };
        let short = TimeWindow {
            first: 1_792_410_000,
            span: 60,
        };

        let minutes = tick_labels(&time_ticks(long, 600.0));
        let seconds = tick_labels(&time_ticks(short, 600.0));

        assert!(minutes.iter().all(|(_, label)| label.len() == 5));
        assert!(seconds.iter().all(|(_, label)| label.len() == 8));
    }

    #[test]
    fn axis_gutters_take_the_prototypes_width_from_the_longest_label() {
        // Goal: a value axis gutter is as wide as the prototype draws it, so plots and their
        // time labels line up with it.
        // Method: build axes from labels of known length and check the width the prototype's
        // rule gives, which is 7.44 pixels per character plus 16, between 28 and 128.
        let entries = |texts: &[&str]| -> Vec<(f32, String)> {
            texts
                .iter()
                .map(|text| (0.0, (*text).to_string()))
                .collect()
        };

        let files = labels(entries(&["0 files", "20 k files"]));
        let share = labels(entries(&["0%", "5%"]));
        let none = labels(Vec::new());
        let long = labels(entries(&[
            "a label far longer than any axis would carry, surely",
        ]));

        assert!((files.width - px(90.4)).abs() < px(0.01));
        assert!((share.width - px(30.88)).abs() < px(0.01));
        assert_eq!(none.width, px(28.0));
        assert_eq!(long.width, px(128.0));
    }
}
