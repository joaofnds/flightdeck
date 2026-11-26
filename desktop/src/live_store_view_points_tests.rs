//! A page drawn from the store's View leaves the points of its charts to the View once its
//! reader rests, since the View holds every point the page draws.

use std::sync::Arc;
use std::time::Duration;

use gpui::{AppContext, TestAppContext};
use serde_json::Value;
use viewer::store::{AppsSecond, AppsStretch, MachineSecond};

use super::super::super::{LiveDashboard, Presence};
use super::{followed_page_settled, memory_seconds, opening_on, rested_on, worker_reading};
use crate::live_hover::{Origin, Target};
use crate::live_source::tests::rankings::kept_window_ranks;
use crate::live_source::{PointsKeeping, Snapshot, Update, identity_signature};
use crate::live_store_app_fixtures::{HOUR_NEWEST, apps_at, hour};
use crate::live_store_fixtures::{reading_at, trend_point, view};

/// The first second of the older tail of `hour()`, whose stretch the View draws one point for.
const OLDER: u64 = HOUR_NEWEST - 1_000;
/// A second of the live span of `hour()`, whose Apps the View holds.
const LIVE: u64 = HOUR_NEWEST - 500;
/// A second later than the worker's reading of 9,999 and 10,000 by more than the 960 seconds it
/// draws a second apart.
const LATER: u64 = 12_000;

/// Runs the test `$name`, which takes the route of the page, on Overview, Memory, Network,
/// Storage, GPU and Battery, six of the pages `store_panels_machine` names, as a test of its own
/// each.
macro_rules! on_each_drawn_page {
    ($name:ident) => {
        mod $name {
            #[test]
            fn overview() {
                super::$name("overview");
            }

            #[test]
            fn memory() {
                super::$name("memory");
            }

            #[test]
            fn network() {
                super::$name("network");
            }

            #[test]
            fn storage() {
                super::$name("storage");
            }

            #[test]
            fn gpu() {
                super::$name("gpu");
            }

            #[test]
            fn battery() {
                super::$name("battery");
            }
        }
    };
    ($name:ident, gpui) => {
        mod $name {
            #[gpui::test]
            fn overview(cx: &mut gpui::TestAppContext) {
                super::$name(cx, "overview");
            }

            #[gpui::test]
            fn memory(cx: &mut gpui::TestAppContext) {
                super::$name(cx, "memory");
            }

            #[gpui::test]
            fn network(cx: &mut gpui::TestAppContext) {
                super::$name(cx, "network");
            }

            #[gpui::test]
            fn storage(cx: &mut gpui::TestAppContext) {
                super::$name(cx, "storage");
            }

            #[gpui::test]
            fn gpu(cx: &mut gpui::TestAppContext) {
                super::$name(cx, "gpu");
            }

            #[gpui::test]
            fn battery(cx: &mut gpui::TestAppContext) {
                super::$name(cx, "battery");
            }
        }
    };
}

/// Runs the test `$name`, which takes the route of the page, on each page listed, as a test of
/// its own each.
macro_rules! on_pages {
    ($name:ident, plain on $($page:ident)+) => {
        mod $name {
            $(
                #[test]
                fn $page() {
                    super::$name(stringify!($page));
                }
            )+
        }
    };
    ($name:ident on $($page:ident)+) => {
        mod $name {
            $(
                #[gpui::test]
                fn $page(cx: &mut gpui::TestAppContext) {
                    super::$name(cx, stringify!($page));
                }
            )+
        }
    };
}

/// Runs the test `$name`, which takes the route of the page, the direction of an arrow step
/// from inside the older tail's second stretch and the second it should show, on each page
/// listed and in each direction, as a test of its own each.
macro_rules! each_step_way {
    ($name:ident on $($page:ident)+) => {
        mod $name {
            $(
                mod $page {
                    #[gpui::test]
                    fn to_the_right(cx: &mut gpui::TestAppContext) {
                        let expected = super::super::OLDER + 2 * viewer::EARLIER_STRIDE_SECONDS;
                        super::super::$name(cx, stringify!($page), 1, expected);
                    }

                    #[gpui::test]
                    fn to_the_left(cx: &mut gpui::TestAppContext) {
                        super::super::$name(cx, stringify!($page), -1, super::super::OLDER);
                    }
                }
            )+
        }
    };
}

on_each_drawn_page!(a_followed_page_that_rests_its_reader_keeps_none_of_its_chart_points);
fn a_followed_page_that_rests_its_reader_keeps_none_of_its_chart_points(route: &str) {
    // Goal: a page drawn from the store's View holds none of the points of its charts once its
    // reader rests, since the View draws every one of them. Method: the worker's reading of two
    // seconds, then the View of the same seconds that rests the page's reader, then the points
    // the page's reading still holds.
    let seconds = memory_seconds(&[9_999, 10_000]);

    let dashboard = rested_on(route, &seconds, worker_reading(&seconds));

    let selection = &dashboard.snapshot.as_ref().unwrap().selection;
    assert!(selection.trends().is_none());
    assert_eq!(selection["earlier_trends"], Value::Null);
}

on_each_drawn_page!(the_stats_line_of_a_rested_page_counts_the_points_its_charts_draw);
fn the_stats_line_of_a_rested_page_counts_the_points_its_charts_draw(route: &str) {
    // Goal: the `--seconds` stats line of a page whose reader rests still counts the points a
    // second apart its charts draw, which are the View's once the reading holds none. Method:
    // the worker's reading of two seconds, then the View of the same seconds that rests the
    // page's reader, then the count the stats line reports.
    let seconds = memory_seconds(&[9_999, 10_000]);

    let dashboard = rested_on(route, &seconds, worker_reading(&seconds));

    assert_eq!(dashboard.tick_deadline_scalar_points(), Some(2));
}

on_each_drawn_page!(the_stats_line_of_a_rested_page_counts_only_the_seconds_drawn_a_second_apart);
fn the_stats_line_of_a_rested_page_counts_only_the_seconds_drawn_a_second_apart(route: &str) {
    // Goal: the `--seconds` stats line of a rested page counts only the View's seconds its
    // charts draw a second apart, the newest 960, and not the older seconds of the View's live
    // span. Method: a page whose reader rests over the View of the worker's reading of two
    // seconds, then a View of 1,000 seconds in a row, then the count the stats line reports.
    let seconds = memory_seconds(&[9_999, 10_000]);
    let mut dashboard = rested_on(route, &seconds, worker_reading(&seconds));
    let longer: Vec<u64> = (9_001..=10_000).collect();
    assert!(dashboard.take_view(Arc::new(view(2, &[], &memory_seconds(&longer)))));

    let counted = dashboard.tick_deadline_scalar_points();

    assert_eq!(counted, Some(960));
}

on_each_drawn_page!(a_window_closed_on_a_rested_page_keeps_none_of_its_chart_points);
fn a_window_closed_on_a_rested_page_keeps_none_of_its_chart_points(route: &str) {
    // Goal: what a window closed on a page drawn from the store's View keeps holds none of the
    // points of its charts. Method: a page whose reader rests over the View of the worker's
    // reading closes, then the points of the reading it keeps.
    let seconds = memory_seconds(&[9_999, 10_000]);
    let mut dashboard = rested_on(route, &seconds, worker_reading(&seconds));

    let state = dashboard.window_state();

    let selection = &state.snapshot.as_ref().unwrap().selection;
    assert!(selection.trends().is_none());
    assert_eq!(selection["earlier_trends"], Value::Null);
}

on_each_drawn_page!(
    a_window_reopened_on_a_rested_page_draws_its_charts_at_once_and_reads_nothing,
    gpui
);
fn a_window_reopened_on_a_rested_page_draws_its_charts_at_once_and_reads_nothing(
    cx: &mut TestAppContext,
    route: &str,
) {
    // Goal: a window closed on a page whose reader rests and opened again draws the charts it
    // drew at once and resumes from the store's View, asking its reader for nothing, although
    // its reading kept none of their points. Method: a page over a real store and reader rests
    // on the View, the window closes as the app closes it and opens again from what it kept,
    // then the charts it draws, and whether it reads once it opens and after its first tick.
    let (recording, page) = followed_page_settled(cx, route);
    tick_until_rested(cx, &page);
    let drawn = cx.update(|cx| drawn_panels(page.read(cx)));
    let (worker, state) = page.update(cx, |page, _| {
        let worker = page.worker.hand_over().unwrap().unwrap();
        (worker, page.window_state())
    });
    cx.update(|cx| crate::live_dashboard::set_store_active(false, cx));
    let kept = state.snapshot.as_ref().unwrap().points_keeping;
    assert_eq!(kept, PointsKeeping::LeftToView);

    let reopened = cx.update(|cx| cx.new(|cx| LiveDashboard::restore(worker, state, cx)));

    cx.update(|cx| {
        let reopened = reopened.read(cx);
        assert_eq!(drawn_panels(reopened), drawn);
        assert_eq!(reopened.presence, Presence::Showing);
        assert!(!reopened.reply_pending);
    });
    reopened.update(cx, LiveDashboard::tick);
    cx.update(|cx| assert!(!reopened.read(cx).reply_pending));
    drop(recording);
    cx.update(|cx| drop(cx.remove_global::<super::super::LiveStore>()));
}

on_each_drawn_page!(
    a_window_closed_before_its_reader_rested_keeps_no_chart_points_and_draws_them_again,
    gpui
);
fn a_window_closed_before_its_reader_rested_keeps_no_chart_points_and_draws_them_again(
    cx: &mut TestAppContext,
    route: &str,
) {
    // Goal: a window closed on a page that follows the store's View before its reader rests
    // keeps none of the points of its charts, as one closed after the rest keeps none, and
    // opened again draws the charts it drew. Method: the worker's reading of two seconds, then
    // the View of the same seconds with the reader not yet rested, the window closes as the app
    // closes it, then the points of the reading it keeps and the charts it draws once it opens
    // again.
    let seconds = memory_seconds(&[9_999, 10_000]);
    let mut dashboard = opening_on(route);
    let mut reading = worker_reading(&seconds);
    reading.generation = dashboard.generation;
    assert!(dashboard.receive(Update::Ready(reading)));
    assert!(dashboard.take_view(Arc::new(view(1, &[], &seconds))));
    assert!(!dashboard.reader_rested());
    let drawn = drawn_panels(&dashboard);

    let worker = dashboard.worker.hand_over().unwrap().unwrap();
    let state = dashboard.window_state();

    {
        let selection = &state.snapshot.as_ref().unwrap().selection;
        assert!(selection.trends().is_none());
        assert_eq!(selection["earlier_trends"], Value::Null);
    }
    let reopened = cx.update(|cx| cx.new(|cx| LiveDashboard::restore(worker, state, cx)));
    cx.update(|cx| assert_eq!(drawn_panels(reopened.read(cx)), drawn));
}

each_step_way!(
    a_step_across_the_older_tail_of_a_rested_page_moves_one_stretch
    on overview memory network storage gpu
);
fn a_step_across_the_older_tail_of_a_rested_page_moves_one_stretch(
    cx: &mut TestAppContext,
    route: &str,
    delta: i64,
    expected: u64,
) {
    // Goal: an arrow step on the older tail of a page whose reader rests moves to the next
    // stretch the charts draw in either direction, as it does while the page holds the points
    // of its charts, rather than back to the stretch it shows. Method: a page over the hour
    // rests its reader and steps from the newest second to a second inside the older tail's
    // second stretch, then one step.
    let page = cx.new(|_| rested_over_the_hour(route));

    let stepped = step_twice_from_inside_the_second_stretch(cx, &page, delta);

    assert_eq!(
        stepped,
        [Some(OLDER + viewer::EARLIER_STRIDE_SECONDS), Some(expected)]
    );
}

each_step_way!(
    a_step_across_the_older_tail_a_reading_left_to_the_view_moves_one_stretch
    on overview memory network storage gpu cpu
);
fn a_step_across_the_older_tail_a_reading_left_to_the_view_moves_one_stretch(
    cx: &mut TestAppContext,
    route: &str,
    delta: i64,
    expected: u64,
) {
    // Goal: an arrow step on the older tail of a page whose reading left that tail to the store's
    // View, since the View draws the page whole, moves to the next stretch the charts draw in
    // either direction, as it does over a reading that read the tail. Method: a page over the
    // hour takes a reading with no older tail, steps from the newest second to a second inside
    // the older tail's second stretch, then one step.
    let page = cx.new(|_| over_the_hour_with_its_tail_left_to_the_view(route));

    let stepped = step_twice_from_inside_the_second_stretch(cx, &page, delta);

    assert_eq!(
        stepped,
        [Some(OLDER + viewer::EARLIER_STRIDE_SECONDS), Some(expected)]
    );
}

on_pages!(
    a_click_on_the_older_tail_a_reading_left_to_the_view_holds_what_a_reading_of_it_holds
    on overview memory network storage gpu cpu
);
fn a_click_on_the_older_tail_a_reading_left_to_the_view_holds_what_a_reading_of_it_holds(
    cx: &mut TestAppContext,
    route: &str,
) {
    // Goal: a click with no hover on the older tail of a page whose reading left that tail to the
    // store's View holds the reading a page whose reading read the tail holds. Method: a page
    // over the hour that took a reading with no older tail and one whose reading holds the tail,
    // then a click with no hover shown on a second inside an older stretch other than its first,
    // then the readings the two holds name.
    let second = OLDER + 3;
    let left = cx.new(|_| over_the_hour_with_its_tail_left_to_the_view(route));
    let read = cx.new(|_| over_the_hour(route));

    left.update(cx, |page, cx| page.pin_second(second, cx));
    read.update(cx, |page, cx| page.pin_second(second, cx));

    cx.update(|cx| assert_eq!(held_target(left.read(cx)), held_target(read.read(cx))));
}

on_pages!(
    a_window_shown_again_over_a_reading_left_to_the_view_shows_the_points_it_showed
    on overview memory network storage gpu cpu
);
fn a_window_shown_again_over_a_reading_left_to_the_view_shows_the_points_it_showed(
    cx: &mut TestAppContext,
    route: &str,
) {
    // Goal: a window that hid and let go of the View, then took a reading that left its older
    // tail to the View, still draws the View's charts, and reads each at the point of the
    // hovered second rather than at that point shifted by the older tail the reading lacks.
    // Method: a page over the hour whose reading left its tail to the View, the point each
    // chart shows while a second of the live span is hovered, then the window hides with no
    // hover, takes the next such reading and shows the same hover.
    let page = cx.new(|_| over_the_hour_with_its_tail_left_to_the_view(route));
    let target = cx.update(|cx| page.read(cx).store_view_target(LIVE).unwrap());
    let source: gpui::SharedString = "live-cpu-plot".into();
    let hover = |cx: &mut TestAppContext| {
        page.update(cx, |page, cx| {
            page.set_reading_target(source.clone(), target.clone(), Origin::Pointer, cx);
        });
    };
    hover(cx);
    let shown = cx.update(|cx| shown_points(page.read(cx)));
    page.update(cx, |page, cx| page.clear_reading_target(&source, cx));
    page.update(cx, |page, _| page.tick_hidden());
    page.update(cx, |page, _| {
        let reading = reading_left_to_the_view(page);
        assert!(page.receive(Update::Ready(reading)));
    });

    hover(cx);

    cx.update(|cx| assert_eq!(shown_points(page.read(cx)), shown));
}

on_pages!(
    a_clock_time_typed_over_a_reading_left_to_the_view_shows_the_points_of_its_second
    on overview memory network storage gpu cpu
);
fn a_clock_time_typed_over_a_reading_left_to_the_view_shows_the_points_of_its_second(
    cx: &mut TestAppContext,
    route: &str,
) {
    // Goal: a clock time typed on a page whose reading left its older tail to the View reads the
    // View's charts, which stay drawn until the reply, at the typed second's point, as a page
    // whose reading read the tail does. Method: a page over the hour whose reading left its tail
    // to the View and one whose reading holds the tail each take a typed second of the live
    // span and a tick, then the point each machine chart shows, since the second page's
    // reading brings no chart by App.
    let left = cx.new(|_| over_the_hour_with_its_tail_left_to_the_view(route));
    let read = cx.new(|_| over_the_hour(route));

    left.update(cx, |page, cx| page.inspect(LIVE, cx));
    read.update(cx, |page, cx| page.inspect(LIVE, cx));
    left.update(cx, LiveDashboard::tick);
    read.update(cx, LiveDashboard::tick);

    let machine = |page: &gpui::Entity<LiveDashboard>| {
        let shown = cx.update(|cx| shown_points(page.read(cx)));
        shown
            .into_iter()
            .filter(|(id, _)| !super::super::store_view_draws_app_chart(route, id))
            .collect::<Vec<_>>()
    };
    assert_eq!(machine(&left), machine(&read));
}

on_pages!(
    a_step_across_the_older_tail_after_choosing_the_page_shown_moves_one_stretch
    on overview memory network storage gpu cpu
);
fn a_step_across_the_older_tail_after_choosing_the_page_shown_moves_one_stretch(
    cx: &mut TestAppContext,
    route: &str,
) {
    // Goal: choosing the page already shown keeps the record that its reading left the older
    // tail to the store's View, so an arrow step across that tail still moves one stretch.
    // Method: a page over the hour whose reading left its tail to the View is chosen again
    // from the sidebar, which keeps the page and shows the kept reading at once, then a step
    // from the newest second to a second inside the older tail's second stretch and one step
    // to the right.
    let stride = viewer::EARLIER_STRIDE_SECONDS;
    let page = cx.new(|_| over_the_hour_with_its_tail_left_to_the_view(route));
    page.update(cx, |page, _| page.switch_route(route));

    let stepped = step_twice_from_inside_the_second_stretch(cx, &page, 1);

    assert_eq!(stepped, [Some(OLDER + stride), Some(OLDER + 2 * stride)]);
}

on_pages!(
    a_click_on_the_older_tail_after_choosing_the_page_shown_holds_what_a_reading_of_it_holds
    on overview memory network storage gpu cpu
);
fn a_click_on_the_older_tail_after_choosing_the_page_shown_holds_what_a_reading_of_it_holds(
    cx: &mut TestAppContext,
    route: &str,
) {
    // Goal: choosing the page already shown keeps the record that its reading left the older
    // tail to the store's View, so a click with no hover on that tail holds what a page whose
    // reading read the tail holds. Method: a page over the hour whose reading left its tail to
    // the View is chosen again from the sidebar, and a page whose reading holds the tail is not,
    // then a click with no hover on a second inside an older stretch other than its first.
    let second = OLDER + 3;
    let left = cx.new(|_| over_the_hour_with_its_tail_left_to_the_view(route));
    let read = cx.new(|_| over_the_hour(route));
    left.update(cx, |page, _| page.switch_route(route));

    left.update(cx, |page, cx| page.pin_second(second, cx));
    read.update(cx, |page, cx| page.pin_second(second, cx));

    cx.update(|cx| assert_eq!(held_target(left.read(cx)), held_target(read.read(cx))));
}

on_each_drawn_page!(
    a_click_without_a_hover_on_a_rested_page_holds_what_a_page_never_rested_holds,
    gpui
);
fn a_click_without_a_hover_on_a_rested_page_holds_what_a_page_never_rested_holds(
    cx: &mut TestAppContext,
    route: &str,
) {
    // Goal: a click on a page whose reader rests, where no hover names the reading under it,
    // holds the reading a page that still holds the points of its charts holds. Method: a page
    // over the hour that rests its reader and one that never rests it, then a click with no
    // hover shown on a second of the live span, and on new pages one on a second inside an
    // older stretch other than its first, then the readings the two holds name.
    for second in [LIVE, OLDER + 3] {
        let rested = cx.new(|_| rested_over_the_hour(route));
        let kept = cx.new(|_| over_the_hour(route));

        rested.update(cx, |page, cx| page.pin_second(second, cx));
        kept.update(cx, |page, cx| page.pin_second(second, cx));

        cx.update(|cx| assert_eq!(held_target(rested.read(cx)), held_target(kept.read(cx))));
    }
}

on_each_drawn_page!(
    a_range_picked_on_a_rested_page_holds_what_a_page_never_rested_holds,
    gpui
);
fn a_range_picked_on_a_rested_page_holds_what_a_page_never_rested_holds(
    cx: &mut TestAppContext,
    route: &str,
) {
    // Goal: a range picked on a chart of a page whose reader rests holds the second and the
    // range a page that still holds the points of its charts holds. Method: a page over the
    // hour that rests its reader and one that never rests it, then the same range of the live
    // span is picked on each, then the reading each hold names and the range each shows.
    let picked = crate::time_range::Range::Between {
        first: LIVE,
        last: LIVE + 60,
    };
    let rested = cx.new(|_| rested_over_the_hour(route));
    let kept = cx.new(|_| over_the_hour(route));

    rested.update(cx, |page, cx| page.select_range(picked, cx));
    kept.update(cx, |page, cx| page.select_range(picked, cx));

    cx.update(|cx| {
        let (rested, kept) = (rested.read(cx), kept.read(cx));
        assert!(rested.hold.is_some());
        assert_eq!(held_target(rested), held_target(kept));
        assert_eq!(rested.range, kept.range);
    });
}

on_each_drawn_page!(a_clock_time_typed_on_a_rested_page_finds_its_second, gpui);
fn a_clock_time_typed_on_a_rested_page_finds_its_second(cx: &mut TestAppContext, route: &str) {
    // Goal: a local clock time typed on a page whose reader rests finds the second the charts
    // draw at that time, as it does while the page holds the points of its charts. Method: a
    // page over the hour rests its reader, then the clock time of a second of the live span is
    // typed, then the second the page asks for and the notice it shows.
    let page = cx.new(|_| rested_over_the_hour(route));
    let typed = crate::live_presentation::clock(LIVE, true);

    page.update(cx, |page, cx| page.inspect_text(&typed, cx));

    cx.update(|cx| {
        let page = page.read(cx);
        assert_eq!(page.native.notice, None);
        assert_eq!(page.request.second, Some(LIVE));
    });
}

on_each_drawn_page!(
    a_clock_time_typed_long_after_a_page_rested_finds_a_second_drawn_since,
    gpui
);
fn a_clock_time_typed_long_after_a_page_rested_finds_a_second_drawn_since(
    cx: &mut TestAppContext,
    route: &str,
) {
    // Goal: a local clock time typed on a page whose reader has rested longer than its reading
    // reaches finds a second the charts drew since the rest. Method: a page whose reader rests
    // over the View of the worker's reading of two seconds, then a View of a second later than
    // the reading by more than its span, then the clock time of that second is typed, then the
    // notice the page shows and the second it asks for.
    let seconds = memory_seconds(&[9_999, 10_000]);
    let mut dashboard = rested_on(route, &seconds, worker_reading(&seconds));
    assert!(dashboard.take_view(Arc::new(view(2, &[], &memory_seconds(&[LATER])))));
    let page = cx.new(|_| dashboard);
    let typed = crate::live_presentation::clock(LATER, true);

    page.update(cx, |page, cx| page.inspect_text(&typed, cx));

    cx.update(|cx| {
        let page = page.read(cx);
        assert_eq!(page.native.notice, None);
        assert_eq!(page.request.second, Some(LATER));
    });
}

on_each_drawn_page!(
    a_hover_of_an_older_stretch_whose_ranks_the_reading_holds_reads_nothing_on_a_rested_page,
    gpui
);
fn a_hover_of_an_older_stretch_whose_ranks_the_reading_holds_reads_nothing_on_a_rested_page(
    cx: &mut TestAppContext,
    route: &str,
) {
    // Goal: a hover of an older stretch on a page whose reader rests reads nothing where the
    // View holds the stretch's readings and the page's reading holds its top Apps, although
    // the View sampled no tick of it. Method: the View of the hour whose oldest stretch starts
    // at a tick the window's ranks rank, and the worker's reading of it with those ranks, rest
    // the page's reader, then the stretch's first second is hovered, then whether the hover
    // holds every reading it shows.
    let dashboard = rested_over_the_ranked_hour(route);
    let target = dashboard.store_view_target(OLDER).unwrap();
    let page = cx.new(|_| dashboard);

    page.update(cx, |page, cx| {
        page.set_reading_target("live-cpu-plot".into(), target, Origin::Pointer, cx)
    });

    cx.update(|cx| assert!(page.read(cx).preview.as_ref().unwrap().complete));
}

on_each_drawn_page!(
    a_hover_of_a_ranked_second_the_view_let_go_of_asks_the_reader_of_a_rested_page_for_the_page,
    gpui
);
fn a_hover_of_a_ranked_second_the_view_let_go_of_asks_the_reader_of_a_rested_page_for_the_page(
    cx: &mut TestAppContext,
    route: &str,
) {
    // Goal: a hover of a second whose top Apps the page's reading holds, on a page whose
    // reading left its points to the View, asks the rested reader for the page once the View
    // holds the second no longer, since the second's readings went with the points and the top
    // Apps alone do not complete it. Method: a page whose reader rests over the View of the
    // hour, whose oldest stretch the reading ranks, then a View of one later second alone, then
    // that stretch's first second is hovered and the hover's step runs, then whether the page
    // waits on a reply.
    let mut dashboard = rested_over_the_ranked_hour(route);
    let target = dashboard.store_view_target(OLDER).unwrap();
    assert!(dashboard.take_view(Arc::new(view(2, &[], &memory_seconds(&[LATER])))));
    let page = cx.new(|_| dashboard);

    page.update(cx, |page, cx| {
        page.set_reading_target("live-cpu-plot".into(), target, Origin::Pointer, cx);
        page.preview_tick(cx);
    });

    cx.update(|cx| assert!(page.read(cx).reply_pending));
}

on_each_drawn_page!(a_rested_page_whose_view_cannot_be_drawn_asks_its_reader_for_the_page);
fn a_rested_page_whose_view_cannot_be_drawn_asks_its_reader_for_the_page(route: &str) {
    // Goal: a page whose reader rests and whose reading left its points to the View asks its
    // reader for the page once it lets go of a View it could not draw, since it has nothing
    // else to draw its charts from. Method: a page whose reader rests over the View of the
    // worker's reading, then the page lets go of the View as a failed draw does, then whether
    // it waits on a reply.
    let seconds = memory_seconds(&[9_999, 10_000]);
    let mut dashboard = rested_on(route, &seconds, worker_reading(&seconds));

    dashboard.draw_store_view_failed("The View could not be drawn".to_owned());

    assert!(dashboard.reply_pending);
}

on_pages!(
    a_page_whose_reading_left_its_tail_to_a_view_it_cannot_draw_asks_its_reader_for_the_page, plain
    on overview memory network storage gpu cpu
);
fn a_page_whose_reading_left_its_tail_to_a_view_it_cannot_draw_asks_its_reader_for_the_page(
    route: &str,
) {
    // Goal: a page whose reading left its older tail to the View asks its reader for the page
    // once it lets go of a View it could not draw, since its reading holds none of the older
    // seconds the View drew. Method: a page over the hour whose reading left its tail to the
    // View and whose reader is not rested, then the page lets go of the View as a failed draw
    // does, then whether it waits on a reply.
    let mut dashboard = over_the_hour_with_its_tail_left_to_the_view(route);
    assert!(!dashboard.reply_pending);

    dashboard.draw_store_view_failed("The View could not be drawn".to_owned());

    assert!(dashboard.reply_pending);
}

on_each_drawn_page!(a_page_left_after_its_reader_rested_awaits_a_reading_on_return);
fn a_page_left_after_its_reader_rested_awaits_a_reading_on_return(route: &str) {
    // Goal: a page left after its reader rested, and opened again before its next reading,
    // waits for the reading that brings its points, as any page left does. Method: a page whose
    // reader rests over the View of the worker's reading, the CPU page, the page again, then
    // whether its kept reading waits for another.
    let seconds = memory_seconds(&[9_999, 10_000]);
    let mut dashboard = rested_on(route, &seconds, worker_reading(&seconds));

    dashboard.switch_route("cpu");
    dashboard.switch_route(route);

    assert!(dashboard.snapshot.as_ref().unwrap().awaits_reading());
}

on_each_drawn_page!(a_rested_page_whose_view_cannot_be_drawn_asks_its_reader_once_while_it_waits);
fn a_rested_page_whose_view_cannot_be_drawn_asks_its_reader_once_while_it_waits(route: &str) {
    // Goal: a page whose reading left its points to the View asks its reader for the page once
    // while it waits on the reply, and not again each time the View fails to draw meanwhile.
    // Method: a page whose reader rests over the View of the worker's reading lets go of the
    // View as a failed draw does, twice, then the read it waits on after the second failure
    // against the one it asked for at the first.
    let seconds = memory_seconds(&[9_999, 10_000]);
    let mut dashboard = rested_on(route, &seconds, worker_reading(&seconds));
    dashboard.draw_store_view_failed("The View could not be drawn".to_owned());
    let asked = dashboard.generation;

    dashboard.draw_store_view_failed("The View could not be drawn".to_owned());

    assert_eq!(dashboard.generation, asked);
}

on_each_drawn_page!(a_reading_left_without_its_points_awaits_a_reading_though_left_to_the_view);
fn a_reading_left_without_its_points_awaits_a_reading_though_left_to_the_view(route: &str) {
    // Goal: a reading that dropped its points as its page was left goes on waiting for the
    // reading that brings them when the page then leaves its points to the View, since the
    // charts it kept were drawn before the page was left and the View did not draw them.
    // Method: a page whose reader rests over the View of the worker's reading, the CPU page,
    // the page again, then its kept reading leaves its points to the View, then whether it
    // waits for another.
    let seconds = memory_seconds(&[9_999, 10_000]);
    let mut dashboard = rested_on(route, &seconds, worker_reading(&seconds));
    dashboard.switch_route("cpu");
    dashboard.switch_route(route);
    let kept = dashboard.snapshot.as_mut().unwrap();

    kept.leave_points_to_view();

    assert!(kept.awaits_reading());
}

on_each_drawn_page!(
    a_hover_of_a_second_the_view_lacks_on_a_rested_page_asks_its_reader_for_the_page,
    gpui
);
fn a_hover_of_a_second_the_view_lacks_on_a_rested_page_asks_its_reader_for_the_page(
    cx: &mut TestAppContext,
    route: &str,
) {
    // Goal: a hover of a second the View holds nothing of, on a page whose reading left its
    // points to the View, asks the rested reader for the page, since such a page cannot tell a
    // gap from a second the View lacks and holds no reading of the second to show. Method: a
    // page whose reader rests over the View of the worker's reading of two seconds, then a
    // hover of a second a thousand seconds before them, which names no reading, and the hover's
    // step, then whether the page waits on a reply.
    let seconds = memory_seconds(&[9_999, 10_000]);
    let page = cx.new(|_| rested_on(route, &seconds, worker_reading(&seconds)));
    let lacked = Target {
        second: 9_000,
        identity: None,
    };

    page.update(cx, |page, cx| {
        page.set_reading_target("live-cpu-plot".into(), lacked, Origin::Pointer, cx);
        page.preview_tick(cx);
    });

    cx.update(|cx| assert!(page.read(cx).reply_pending));
}

on_each_drawn_page!(
    a_clock_time_typed_on_a_rested_page_shows_no_hover_of_its_second_before_the_reply,
    gpui
);
fn a_clock_time_typed_on_a_rested_page_shows_no_hover_of_its_second_before_the_reply(
    cx: &mut TestAppContext,
    route: &str,
) {
    // Goal: a clock time typed on a page whose reader rests shows no hover of the second before
    // the read of it arrives, since the reading left its points to the View and a hover drawn
    // from that reading would show the second as holding nothing. Method: a page over the hour
    // rests its reader, then the clock time of a second of the live span is typed, then the
    // hover the page shows.
    let page = cx.new(|_| rested_over_the_hour(route));
    let typed = crate::live_presentation::clock(LIVE, true);

    page.update(cx, |page, cx| page.inspect_text(&typed, cx));

    cx.update(|cx| assert!(page.read(cx).preview.is_none()));
}

on_each_drawn_page!(
    a_window_closed_over_a_hold_made_before_its_reader_rested_keeps_no_chart_points,
    gpui
);
fn a_window_closed_over_a_hold_made_before_its_reader_rested_keeps_no_chart_points(
    cx: &mut TestAppContext,
    route: &str,
) {
    // Goal: a window closed while a second is held on a page that follows the store's View,
    // before its reader rests, keeps none of the points of its charts, as one closed with no
    // hold keeps none, and opened again draws the charts it drew. Method: a page over the hour
    // with its reader not yet rested, a click on a second of the live span, the window closes
    // as the app closes it, then the points of the reading it keeps and the charts it draws
    // once it opens again.
    let page = cx.new(|_| over_the_hour(route));
    page.update(cx, |page, cx| page.pin_second(LIVE, cx));
    let (drawn, worker, state) = page.update(cx, |page, _| {
        assert!(page.hold.is_some());
        let drawn = drawn_panels(page);
        let worker = page.worker.hand_over().unwrap().unwrap();
        (drawn, worker, page.window_state())
    });

    {
        let selection = &state.snapshot.as_ref().unwrap().selection;
        assert!(selection.trends().is_none());
        assert_eq!(selection["earlier_trends"], Value::Null);
    }
    let reopened = cx.update(|cx| cx.new(|cx| LiveDashboard::restore(worker, state, cx)));
    cx.update(|cx| assert_eq!(drawn_panels(reopened.read(cx)), drawn));
}

/// A page on `route` whose reader rests over the View of `hour()`, whose oldest stretch starts
/// at a tick the page's reading ranks the top Apps of, although the View sampled no tick of it.
fn rested_over_the_ranked_hour(route: &str) -> LiveDashboard {
    let (mut view, _) = hour();
    let mut ranked = reading_at(OLDER, 0);
    ranked.publication = 1;
    let older: Vec<Arc<AppsSecond>> = (OLDER..OLDER + 10)
        .map(|second| match second {
            OLDER => Arc::new(apps_at(&ranked, &[("A", 500_000_000)])),
            _ => Arc::new(apps_at(&reading_at(second, 0), &[("A", 500_000_000)])),
        })
        .collect();
    view.earlier[0] = Arc::new(ranked);
    view.stretches[0] = Arc::new(AppsStretch::of_seconds(OLDER / 10, &older));
    let live: Vec<MachineSecond> = view
        .seconds
        .iter()
        .map(|second| (**second).clone())
        .collect();
    let mut reading = worker_reading(&live);
    reading.selection.legacy_mut().unwrap()["earlier_trends"] = view
        .earlier
        .iter()
        .map(|second| trend_point(second))
        .collect();
    reading.app_ranks = Some(kept_window_ranks());
    let mut dashboard = opening_on(route);
    reading.generation = dashboard.generation;
    assert!(dashboard.receive(Update::Ready(reading)));
    assert!(dashboard.take_view(Arc::new(view)));
    dashboard.tick_rest();
    assert!(dashboard.reader_rested());

    dashboard
}

/// A page on `route` that took the worker's reading of `hour()`, its live span and its older
/// tail, and rests its reader over the View of the hour.
fn rested_over_the_hour(route: &str) -> LiveDashboard {
    let mut dashboard = over_the_hour(route);

    dashboard.tick_rest();

    assert!(dashboard.reader_rested());
    assert_eq!(
        dashboard.snapshot.as_ref().unwrap().points_keeping,
        PointsKeeping::LeftToView
    );
    dashboard
}

/// A page on `route` that took the worker's reading of `hour()`, its live span and its older
/// tail, and draws from the View of the hour, with its reader not yet rested, so it holds the
/// points of its charts.
fn over_the_hour(route: &str) -> LiveDashboard {
    let (view, _) = hour();
    let live: Vec<MachineSecond> = view
        .seconds
        .iter()
        .map(|second| (**second).clone())
        .collect();
    let mut reading = worker_reading(&live);
    reading.selection.legacy_mut().unwrap()["earlier_trends"] = view
        .earlier
        .iter()
        .map(|second| trend_point(second))
        .collect();
    let mut dashboard = opening_on(route);
    reading.generation = dashboard.generation;
    assert!(dashboard.receive(Update::Ready(reading)));
    assert!(dashboard.take_view(Arc::new(view)));
    assert_eq!(
        dashboard.snapshot.as_ref().unwrap().points_keeping,
        PointsKeeping::Kept
    );

    dashboard
}

/// A page on `route` that draws from the View of `hour()` and then took the reader's next
/// reading, which left the older tail to the View since the View draws the page whole, and so
/// holds the seconds of its full detail alone.
fn over_the_hour_with_its_tail_left_to_the_view(route: &str) -> LiveDashboard {
    let mut dashboard = over_the_hour(route);

    assert!(dashboard.receive(Update::Ready(reading_left_to_the_view(&dashboard))));

    assert!(dashboard.older_seconds_left_to_view());
    assert!(!dashboard.points_left_to_view());
    dashboard
}

/// The reader's next reading of `hour()` for `dashboard`, which left the older tail to the View
/// since the View draws the page whole, and so holds the seconds of its full detail alone.
fn reading_left_to_the_view(dashboard: &LiveDashboard) -> Box<Snapshot> {
    let (view, _) = hour();
    let detail_first = HOUR_NEWEST - (viewer::CHARTED_SECONDS - 1);
    let live: Vec<MachineSecond> = view
        .seconds
        .iter()
        .filter(|second| second.wall_ns / 1_000_000_000 >= detail_first)
        .map(|second| (**second).clone())
        .collect();
    let mut reading = worker_reading(&live);
    crate::live_store_page_fixtures::left_tail_to_view(&mut reading);
    reading.generation = dashboard.generation;
    reading
}

/// The point each chart the page draws shows, by the chart's name.
fn shown_points(page: &LiveDashboard) -> Vec<(String, Option<usize>)> {
    page.panels
        .iter()
        .map(|panel| (panel.id.clone(), page.chart_look(panel).index))
        .collect()
}

/// The second the page's hold names and the run, boot and publication of its reading.
fn held_target(page: &LiveDashboard) -> (u64, (String, String, u64)) {
    let target = &page.hold.as_ref().unwrap().reading.target;
    let identity = target.identity.as_ref().unwrap();

    (target.second, identity_signature(identity).unwrap())
}

/// Steps `page` from the newest second to a second inside the older tail's second stretch, then
/// once more by `delta`, and gives the second each step shows.
fn step_twice_from_inside_the_second_stretch(
    cx: &mut TestAppContext,
    page: &gpui::Entity<LiveDashboard>,
    delta: i64,
) -> [Option<u64>; 2] {
    let inside = OLDER + viewer::EARLIER_STRIDE_SECONDS + 5;
    page.update(cx, |page, cx| {
        page.chart_step(-i64::try_from(HOUR_NEWEST - inside).unwrap(), cx);
    });
    let first = stepped(cx, page);
    page.update(cx, |page, cx| page.chart_step(delta, cx));

    [first, stepped(cx, page)]
}

/// The second the page's arrow step shows.
fn stepped(cx: &mut TestAppContext, page: &gpui::Entity<LiveDashboard>) -> Option<u64> {
    cx.update(|cx| page.read(cx).shown_second())
}

/// The charts the page draws, whole.
fn drawn_panels(page: &LiveDashboard) -> Vec<crate::data::Panel> {
    page.panels.iter().map(|panel| (**panel).clone()).collect()
}

/// Ticks `page` until its reader rests.
fn tick_until_rested(cx: &mut TestAppContext, page: &gpui::Entity<LiveDashboard>) {
    let deadline = std::time::Instant::now() + Duration::from_secs(4);
    for _ in 0..4_000 {
        page.update(cx, LiveDashboard::tick);
        if cx.update(|cx| page.read(cx).reader_rested()) {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the page's reader never rested"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    panic!("the page's reader never rested");
}
