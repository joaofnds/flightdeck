//! The stretch of time the charts show: the whole loaded history, the last few minutes, or a
//! stretch the reader selected on a chart.

use crate::chart_axes::TimeWindow;
use crate::data::Panel;

/// The seconds of full detail the viewer loads, which the default range shows.
pub const HISTORY_SECONDS: u64 = 900;
/// The seconds of history the viewer loads in all, which bounds every range.
pub const LOADED_SECONDS: u64 = viewer::LOADED_SECONDS;
const _: () = assert!(HISTORY_SECONDS < LOADED_SECONDS);
/// How far beyond the plot a reading may sit and still be painted, as a share of its width.
const EDGE: f32 = 0.001;
/// The shortest stretch a selection can make, so a stray drag cannot zoom into nothing.
pub const SHORTEST_SECONDS: u64 = 5;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Range {
    /// The 900 seconds of full detail around the selected second.
    #[default]
    History,
    /// This many seconds around the selected second, which move on as it does.
    Last(u64),
    /// A stretch of seconds that stays where it is while the selected second moves on.
    Between { first: u64, last: u64 },
}

/// The seconds a range is laid against: the selected second, and the last second the page
/// holds readings of, which runs past the selected second where the recording holds later
/// seconds.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Anchor {
    pub selected: u64,
    pub context_last: u64,
}

impl Range {
    /// Whether the range moves on with the selected second.
    #[must_use]
    pub fn slides(self) -> bool {
        !matches!(self, Self::Between { .. })
    }

    /// The selection a drag from one second to another makes, or none when it is too short.
    #[must_use]
    pub fn selection(from: u64, to: u64) -> Option<Self> {
        let first = from.min(to);
        let last = from.max(to);
        if last - first + 1 < SHORTEST_SECONDS {
            return None;
        }

        Some(Self::Between { first, last })
    }

    /// The last `minutes` minutes, held to what the viewer loads. The default history is the
    /// range of its own length.
    #[must_use]
    pub fn minutes(minutes: u64) -> Self {
        let seconds = minutes.saturating_mul(60);
        if seconds == HISTORY_SECONDS {
            return Self::History;
        }

        Self::Last(seconds.min(LOADED_SECONDS))
    }

    /// How the picker names the range.
    #[must_use]
    pub fn label(self, anchor: Anchor) -> String {
        match self {
            Self::History => "Last 15 minutes".into(),
            Self::Last(60) => "Last 1 minute".into(),
            Self::Last(seconds) if seconds % 60 == 0 => format!("Last {} minutes", seconds / 60),
            Self::Last(seconds) => format!("Last {seconds} seconds"),
            Self::Between { .. } => {
                let window = self.window(anchor);
                let last = window.first + window.span - 1;
                format!(
                    "{} to {}",
                    crate::chart_axes::clock(window.first, true),
                    crate::chart_axes::clock(last, true)
                )
            }
        }
    }

    /// The minutes the range covers, counting a part minute as a whole one.
    #[must_use]
    pub fn whole_minutes(self, anchor: Anchor) -> u64 {
        self.window(anchor).span.div_ceil(60)
    }

    /// The seconds the range covers, clipped to what the page holds.
    #[must_use]
    pub fn window(self, anchor: Anchor) -> TimeWindow {
        assert!(anchor.selected <= anchor.context_last);
        let history = window_around(anchor, HISTORY_SECONDS);
        let loaded_first = anchor.context_last.saturating_sub(LOADED_SECONDS - 1);
        let (first, last) = match self {
            Self::History => return history,
            Self::Last(seconds) => {
                let around = window_around(anchor, seconds);
                (around.first, around.first + around.span - 1)
            }
            Self::Between { first, last } => (first, last.min(anchor.context_last)),
        };
        let first = first.max(loaded_first);
        if last < first {
            return history;
        }

        TimeWindow {
            first,
            span: last - first + 1,
        }
    }
}

impl Range {
    /// The seconds the range covers while the page holds the selected second over charts drawn
    /// up to `anchor.context_last`: those that end there where they hold the selected second, so
    /// the charts stay as they were drawn, and otherwise those around it, moved later where they
    /// would start before the loaded hour, so the range keeps its length.
    #[must_use]
    pub fn held_window(self, anchor: Anchor) -> TimeWindow {
        assert!(anchor.selected <= anchor.context_last);
        let drawn = self.window(Anchor {
            selected: anchor.context_last,
            context_last: anchor.context_last,
        });
        if drawn.first <= anchor.selected {
            return drawn;
        }
        let seconds = match self {
            Self::History => HISTORY_SECONDS,
            Self::Last(seconds) => seconds,
            Self::Between { .. } => return self.window(anchor),
        };

        let loaded_first = anchor.context_last.saturating_sub(LOADED_SECONDS - 1);
        let first = window_around(anchor, seconds).first.max(loaded_first);
        let last = first
            .saturating_add(seconds.saturating_sub(1))
            .min(anchor.context_last);

        TimeWindow {
            first,
            span: last - first + 1,
        }
    }
}

/// The `seconds` around the selected second, which end at it while it is the newest the page
/// holds, and hold it at their middle once the page holds half as many after it, so a step past
/// the selected second stays in view.
fn window_around(anchor: Anchor, seconds: u64) -> TimeWindow {
    // The half floors, so an odd count holds one more second before the selection than after.
    let after = anchor.selected.saturating_add(seconds / 2);
    let last = after.min(anchor.context_last);
    let first = last.saturating_sub(seconds.saturating_sub(1));

    TimeWindow {
        first,
        span: last - first + 1,
    }
}

/// Re-expresses where `panel` places its readings so `view` spans its whole width. Readings
/// outside the view stay in the panel, beyond the 0 to 1 the plot draws, so every index keeps
/// naming the observation it named. A panel with no clock keeps its own axis.
pub fn zoom(panel: &mut Panel, view: TimeWindow) {
    assert!(view.span > 0);
    let Some(from) = panel.window else {
        return;
    };
    let Some(positions) = panel.positions.as_mut() else {
        return;
    };
    // A panel more than 18 hours from the view is a page left long ago, which a fresh reading
    // replaces at once.
    let (Some(shift), Ok(from_span), Ok(view_span)) = (
        seconds_between(from.first, view.first),
        u16::try_from(from.span),
        u16::try_from(view.span),
    ) else {
        return;
    };
    for position in positions.iter_mut() {
        *position = (*position * f32::from(from_span) + shift) / f32::from(view_span);
    }
    panel.window = Some(view);
}

/// The first and last index of `positions` that fall inside the plot, which are the readings a
/// zoomed chart paints. A reading a hair beyond an edge counts, since rounding puts the reading
/// at an edge a hair either side of it. When none are inside, one reading stays so the plot
/// paints empty.
#[must_use]
pub fn visible(positions: &[f32]) -> (usize, usize) {
    let lower = positions.iter().position(|position| *position >= -EDGE);
    let upper = positions
        .iter()
        .rposition(|position| *position <= 1.0 + EDGE);
    match (lower, upper) {
        (Some(lower), Some(upper)) if lower <= upper => (lower, upper),
        (_, Some(index)) | (Some(index), None) => (index, index),
        (None, None) => (0, 0),
    }
}

/// Maps fractions of a window onto fractions of a view of it, and back.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Lens {
    shift: f32,
    full_span: f32,
    view_span: f32,
}

impl Lens {
    /// The lens that leaves every fraction where it is.
    #[must_use]
    pub fn identity() -> Self {
        Self {
            shift: 0.0,
            full_span: 1.0,
            view_span: 1.0,
        }
    }

    /// The lens from `full` to `view`, when both are non-empty and under 18 hours apart.
    #[must_use]
    pub fn new(full: TimeWindow, view: TimeWindow) -> Option<Self> {
        if full.span == 0 || view.span == 0 {
            return None;
        }
        let (Some(shift), Ok(full_span), Ok(view_span)) = (
            seconds_between(full.first, view.first),
            u16::try_from(full.span),
            u16::try_from(view.span),
        ) else {
            return None;
        };

        Some(Self {
            shift,
            full_span: f32::from(full_span),
            view_span: f32::from(view_span),
        })
    }

    /// Where a point at `full` of the window sits across the view, outside 0 to 1 when the view
    /// leaves it out.
    #[must_use]
    pub fn to_view(self, full: f32) -> f32 {
        (full * self.full_span + self.shift) / self.view_span
    }

    /// Where a point at `view` across the view sits across the window.
    #[must_use]
    pub fn to_full(self, view: f32) -> f32 {
        (view * self.view_span - self.shift) / self.full_span
    }
}

/// Seconds from `to` to `from`, negative when `from` is earlier, if they are under 18 hours apart.
fn seconds_between(from: u64, to: u64) -> Option<f32> {
    let apart = u16::try_from(from.abs_diff(to)).ok()?;

    Some(if from < to {
        -f32::from(apart)
    } else {
        f32::from(apart)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lays a range against `second` as the newest second the page holds and the selected one.
    fn newest(second: u64) -> Anchor {
        Anchor {
            selected: second,
            context_last: second,
        }
    }

    #[test]
    fn a_lens_maps_between_a_window_and_a_view_of_it() {
        // Goal: a chart that keeps its own clock can be zoomed and still answer the pointer.
        // Method: view the middle third of a window and map fractions both ways.
        let full = TimeWindow {
            first: 100,
            span: 900,
        };
        let view = TimeWindow {
            first: 400,
            span: 300,
        };

        let lens = Lens::new(full, view).unwrap();

        assert!(lens.to_view(1.0 / 3.0).abs() < 1e-6);
        assert!((lens.to_view(2.0 / 3.0) - 1.0).abs() < 1e-6);
        assert!((lens.to_full(0.5) - 0.5).abs() < 1e-6);
        assert!((lens.to_full(lens.to_view(0.4)) - 0.4).abs() < 1e-6);
        assert_eq!(
            Lens::new(
                full,
                TimeWindow {
                    first: 400,
                    span: 0
                }
            ),
            None
        );
    }

    #[test]
    fn the_whole_history_ends_at_the_selected_second() {
        // Goal: the default range shows the 900 seconds the viewer loaded.
        // Method: resolve it for a long recording and for one younger than 900 seconds.
        assert_eq!(
            Range::History.window(newest(2000)),
            TimeWindow {
                first: 1101,
                span: 900
            }
        );
        assert_eq!(
            Range::History.window(newest(100)),
            TimeWindow {
                first: 0,
                span: 101
            }
        );
    }

    #[test]
    fn a_last_range_follows_the_selected_second() {
        // Goal: "Last 5 minutes" keeps its end on the newest second as the recording grows.
        // Method: resolve it for two selected seconds one apart.
        assert_eq!(
            Range::Last(300).window(newest(2000)),
            TimeWindow {
                first: 1701,
                span: 300
            }
        );
        assert_eq!(
            Range::Last(300).window(newest(2001)),
            TimeWindow {
                first: 1702,
                span: 300
            }
        );
    }

    #[test]
    fn a_range_never_reaches_beyond_the_loaded_history() {
        // Goal: a range longer than the loaded hour, or one that slid out of it, shows what is
        // loaded. Method: ask for more than 3600 seconds and for a stretch that ended before it.
        assert_eq!(
            Range::Last(7200).window(newest(2000)),
            TimeWindow {
                first: 0,
                span: 2001
            }
        );
        assert_eq!(
            Range::Last(7200).window(newest(5000)),
            TimeWindow {
                first: 1401,
                span: 3600
            }
        );
        assert_eq!(
            Range::Between {
                first: 500,
                last: 600
            }
            .window(newest(5000)),
            Range::History.window(newest(5000))
        );
        assert_eq!(
            Range::Between {
                first: 4000,
                last: 4500
            }
            .window(newest(5000)),
            TimeWindow {
                first: 4000,
                span: 501
            }
        );
    }

    #[test]
    fn a_last_range_longer_than_the_default_history_reaches_back_that_far() {
        // Goal: "Last 30 minutes" shows 1800 seconds, which is beyond the 900 of the default view.
        // Method: resolve 30 and 60 minutes for a long recording and for a short one.
        assert_eq!(
            Range::Last(1800).window(newest(5000)),
            TimeWindow {
                first: 3201,
                span: 1800
            }
        );
        assert_eq!(
            Range::Last(3600).window(newest(5000)),
            TimeWindow {
                first: 1401,
                span: 3600
            }
        );
        assert_eq!(
            Range::Last(1800).window(newest(100)),
            TimeWindow {
                first: 0,
                span: 101
            }
        );
    }

    #[test]
    fn a_selection_can_start_before_the_default_history() {
        // Goal: a drag across a 60-minute view may select seconds older than 15 minutes.
        // Method: resolve a stretch that starts 40 minutes back.
        assert_eq!(
            Range::Between {
                first: 2600,
                last: 2700
            }
            .window(newest(5000)),
            TimeWindow {
                first: 2600,
                span: 101
            }
        );
    }

    #[test]
    fn a_fixed_range_keeps_its_seconds() {
        // Goal: a selection made on a chart stays where it was drawn while the reader looks.
        // Method: resolve the same stretch against a later selected second.
        let range = Range::Between {
            first: 1800,
            last: 1859,
        };
        let expected = TimeWindow {
            first: 1800,
            span: 60,
        };

        assert_eq!(range.window(newest(1900)), expected);
        assert_eq!(range.window(newest(1950)), expected);
    }

    #[test]
    fn the_whole_history_holds_a_selected_second_at_its_middle() {
        // Goal: the default range shows the 900 seconds the page holds around a selected
        // second that has 450 seconds after it.
        // Method: resolve it for second 1000 with seconds held to 1450.
        assert_eq!(
            Range::History.window(Anchor {
                selected: 1000,
                context_last: 1450
            }),
            TimeWindow {
                first: 551,
                span: 900
            }
        );
    }

    #[test]
    fn a_short_range_keeps_a_selected_second_in_view() {
        // Goal: a range shorter than the seconds held after a selected second still shows that
        // second, at its middle, with the seconds a step past it reaches.
        // Method: resolve the last 60 seconds for second 1000 with seconds held to 1450.
        assert_eq!(
            Range::Last(60).window(Anchor {
                selected: 1000,
                context_last: 1450
            }),
            TimeWindow {
                first: 971,
                span: 60
            }
        );
    }

    #[test]
    fn a_range_ends_where_the_held_seconds_end() {
        // Goal: a range never reaches past the last second the page holds, so a selected second
        // with fewer seconds after it than half the range sits right of the middle.
        // Method: resolve the last 60 seconds for second 1000 with seconds held to 1010.
        assert_eq!(
            Range::Last(60).window(Anchor {
                selected: 1000,
                context_last: 1010
            }),
            TimeWindow {
                first: 951,
                span: 60
            }
        );
    }

    #[test]
    fn a_range_picked_while_held_ends_where_the_charts_were_drawn() {
        // Goal: while a second is held, a range that still holds it ends at the last second the
        // charts were drawn to, so the charts keep their place instead of centring the second.
        // Method: hold second 4701, the oldest of the last 5 minutes of charts drawn to 5000, and
        // resolve the last 5 minutes.
        assert_eq!(
            Range::Last(300).held_window(Anchor {
                selected: 4701,
                context_last: 5000
            }),
            TimeWindow {
                first: 4701,
                span: 300
            }
        );
    }

    #[test]
    fn a_range_picked_while_held_centres_a_second_it_would_leave_out() {
        // Goal: while a second is held, a range too short to hold it where the charts end puts
        // it at its middle. Method: hold second 3000 over charts drawn to 5000 and resolve the
        // last 5 minutes and the default 15.
        assert_eq!(
            Range::Last(300).held_window(Anchor {
                selected: 3000,
                context_last: 5000
            }),
            TimeWindow {
                first: 2851,
                span: 300
            }
        );
        assert_eq!(
            Range::History.held_window(Anchor {
                selected: 3000,
                context_last: 5000
            }),
            TimeWindow {
                first: 2551,
                span: 900
            }
        );
    }

    #[test]
    fn a_range_picked_while_held_keeps_its_length_near_the_oldest_loaded_second() {
        // Goal: a range centred on a held second near the oldest second of the loaded hour moves
        // later so it stays whole, rather than losing the seconds before the hour. Method: hold
        // second 1500 over charts drawn to 5000, whose hour starts at 1401, and resolve the last
        // 30 minutes.
        assert_eq!(
            Range::Last(1800).held_window(Anchor {
                selected: 1500,
                context_last: 5000
            }),
            TimeWindow {
                first: 1401,
                span: 1800
            }
        );
    }

    #[test]
    fn a_dragged_range_keeps_its_seconds_while_held() {
        // Goal: a stretch dragged out on a chart stays where it was drawn while a second outside
        // it is held. Method: hold second 3000 over charts drawn to 5000 and resolve a stretch
        // from 4000 to 4100.
        assert_eq!(
            Range::Between {
                first: 4000,
                last: 4100
            }
            .held_window(Anchor {
                selected: 3000,
                context_last: 5000
            }),
            TimeWindow {
                first: 4000,
                span: 101
            }
        );
    }

    #[test]
    fn a_selection_is_ordered_and_never_shorter_than_the_shortest_range() {
        // Goal: dragging right to left selects the same stretch, and a stray click selects none.
        // Method: build selections from both drag directions and from two nearby seconds.
        let forward = Range::selection(1800, 1860);
        let backward = Range::selection(1860, 1800);

        assert_eq!(forward, backward);
        assert_eq!(
            forward,
            Some(Range::Between {
                first: 1800,
                last: 1860
            })
        );
        assert_eq!(Range::selection(1800, 1802), None);
    }

    #[test]
    fn a_fraction_of_the_window_names_a_second() {
        // Goal: pointing at a zoomed chart names the second under the pointer.
        // Method: read the edges and the middle of a 60-second window, and fractions outside it.
        let window = TimeWindow {
            first: 1800,
            span: 60,
        };

        assert_eq!(window.second_at(0.0), Some(1800));
        assert_eq!(window.second_at(0.5), Some(1830));
        assert_eq!(window.second_at(1.0), Some(1859));
        assert_eq!(window.second_at(-0.1), None);
        assert_eq!(window.second_at(1.1), None);
        assert_eq!(window.second_at(f32::NAN), None);
    }

    #[test]
    fn a_second_names_the_middle_of_its_place_in_the_window() {
        // Goal: the cursor of a second sits mid-way through that second, as a hover does.
        // Method: place seconds inside and outside a 60-second window.
        let window = TimeWindow {
            first: 1800,
            span: 60,
        };

        assert_eq!(window.fraction_of(1800), Some(0.5 / 60.0));
        assert_eq!(window.fraction_of(1859), Some(59.5 / 60.0));
        assert_eq!(window.fraction_of(1799), None);
        assert_eq!(window.fraction_of(1860), None);
    }

    fn history_panel() -> crate::data::Panel {
        // Three readings at 0:00, 7:30 and 14:59 of a 900-second window starting at second 1101.
        let panel = serde_json::json!({
            "selectedIndex": 2, "positions": [0.0, 0.5, 0.998], "segments": [0, 0, 0],
            "id": "p", "title": "P", "unit": "%", "kind": "line",
            "lines": [{"name": "a", "color": "#fff", "values": [1.0, 2.0, 3.0]}],
        });
        let mut panel: crate::data::Panel = serde_json::from_value(panel).unwrap();
        panel.window = Some(Range::History.window(newest(2000)));
        panel
    }

    #[test]
    fn zooming_a_panel_spreads_the_view_across_its_width_and_keeps_every_reading() {
        // Goal: a zoomed chart draws the chosen seconds edge to edge while indices keep naming
        // the same observations, so hover and inspection still find them.
        // Method: zoom to the second half, read the positions, then zoom back out.
        let mut panel = history_panel();
        let second_half = TimeWindow {
            first: 1551,
            span: 450,
        };

        zoom(&mut panel, second_half);

        assert_eq!(panel.window, Some(second_half));
        assert_eq!(panel.lines[0].values.len(), 3);
        assert_eq!(panel.selected_index, Some(2));
        let positions = panel.positions.clone().unwrap();
        assert!((positions[0] + 1.0).abs() < 1e-6);
        assert!(positions[1].abs() < 1e-6);
        assert!((positions[2] - 0.996).abs() < 1e-4);

        zoom(&mut panel, Range::History.window(newest(2000)));

        let restored = panel.positions.unwrap();
        assert!((restored[1] - 0.5).abs() < 1e-6);
        assert!((restored[2] - 0.998).abs() < 1e-4);
    }

    #[test]
    fn zooming_leaves_a_panel_without_a_clock_alone() {
        // Goal: a chart that is not drawn against the clock keeps its own axis.
        // Method: zoom a panel with no window and compare its positions.
        let mut panel = history_panel();
        panel.window = None;

        zoom(
            &mut panel,
            TimeWindow {
                first: 1551,
                span: 450,
            },
        );

        assert_eq!(panel.positions, Some(vec![0.0, 0.5, 0.998]));
    }

    #[test]
    fn the_visible_readings_are_the_ones_inside_the_view() {
        // Goal: a zoomed chart paints only the readings inside its width.
        // Method: place positions around both edges and read the first and last visible index.
        assert_eq!(visible(&[-1.0, -0.2, 0.0, 0.5, 0.99, 1.0, 1.4]), (2, 5));
        assert_eq!(visible(&[0.0, 0.5]), (0, 1));
        assert_eq!(visible(&[]), (0, 0));
        // Nothing inside: one reading stays so the chart paints an empty plot.
        assert_eq!(visible(&[-2.0, -1.0]), (1, 1));
        assert_eq!(visible(&[1.5, 2.0]), (0, 0));
    }

    #[test]
    fn minutes_are_held_to_the_loaded_hour() {
        // Goal: a custom number of minutes never asks for more than the viewer loaded.
        // Method: build ranges of one, fifteen, thirty, sixty and ninety minutes.
        assert_eq!(Range::minutes(1), Range::Last(60));
        assert_eq!(Range::minutes(14), Range::Last(840));
        assert_eq!(Range::minutes(15), Range::History);
        assert_eq!(Range::minutes(30), Range::Last(1800));
        assert_eq!(Range::minutes(60), Range::Last(3600));
        assert_eq!(Range::minutes(90), Range::Last(3600));
    }

    #[test]
    fn a_range_is_named_for_the_reader() {
        // Goal: the picker says what the charts show.
        // Method: name the whole history, whole and part minutes, and a selected stretch.
        assert_eq!(Range::History.label(newest(2000)), "Last 15 minutes");
        assert_eq!(Range::Last(60).label(newest(2000)), "Last 1 minute");
        assert_eq!(Range::Last(300).label(newest(2000)), "Last 5 minutes");
        assert_eq!(Range::Last(90).label(newest(2000)), "Last 90 seconds");
        assert_eq!(Range::Last(1800).label(newest(2000)), "Last 30 minutes");
        assert_eq!(Range::Last(3600).label(newest(5000)), "Last 60 minutes");
        let stretch = Range::Between {
            first: 1800,
            last: 1859,
        };
        assert!(stretch.label(newest(2000)).contains(" to "));
    }

    #[test]
    fn the_minutes_shown_round_up_to_a_whole_minute() {
        // Goal: the custom stepper starts from the minutes the charts now show.
        // Method: read the minutes of each kind of range.
        assert_eq!(Range::History.whole_minutes(newest(2000)), 15);
        assert_eq!(Range::Last(300).whole_minutes(newest(2000)), 5);
        assert_eq!(Range::Last(90).whole_minutes(newest(2000)), 2);
        assert_eq!(Range::Last(3600).whole_minutes(newest(5000)), 60);
        let stretch = Range::Between {
            first: 1800,
            last: 1859,
        };
        assert_eq!(stretch.whole_minutes(newest(2000)), 1);
    }
}
