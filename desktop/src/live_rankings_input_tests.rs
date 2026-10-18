use std::sync::Arc;
use std::time::Instant;

use gpui::{Modifiers, MouseButton, TestAppContext, px};
use serde_json::json;

use super::{InputDriver, InputHarness, PlotExit, accepted_snapshot};
use crate::live_source::{Snapshot, Update};

#[gpui::test]
fn overview_rankings_show_current_measurements_and_explicit_source_limits(cx: &mut TestAppContext) {
    // Read real production rank nodes after providing one exact shared-response page.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();

    assert!(
        harness
            .driver
            .reading_is_visible("ranking:CPU:Current:80.0 %")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Resident memory:Current:1.0 GiB")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:Process power:Current:500.0 mW")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("App network, disk and GPU readings are unavailable.")
    );
    harness.teardown();
}

#[gpui::test]
fn partial_ranked_totals_identify_their_known_subtotal(cx: &mut TestAppContext) {
    // A measured subtotal remains available while missing members stay visible beside it.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();

    harness.driver.make_ranked_cpu_partial();

    assert!(
        harness
            .driver
            .reading_is_visible("ranking:CPU:Current:80.0 % known")
    );
    assert!(
        harness
            .driver
            .reading_is_visible("1 measured · 2 unavailable")
    );
    harness.teardown();
}

#[gpui::test]
fn ranking_cells_compare_unequal_amounts_with_proportional_fills(cx: &mut TestAppContext) {
    // Measure production GPUI tracks and fills for exact CPU amounts in a four-to-one ratio.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();
    harness.driver.set_ranked_cpu_amounts(80, 20);

    harness
        .driver
        .assert_ranked_fill("ranking-fill:CPU:41", "ranking-track:CPU:41", 1.0);
    harness
        .driver
        .assert_ranked_fill("ranking-fill:CPU:42", "ranking-track:CPU:42", 0.25);
    harness.teardown();
}

#[gpui::test]
fn ranking_cells_keep_measured_zero_fills_empty(cx: &mut TestAppContext) {
    // An all-zero population has measured values without a fabricated positive comparison bar.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();
    harness.driver.set_ranked_cpu_amounts(0, 0);

    harness
        .driver
        .assert_ranked_fill("ranking-fill:CPU:41", "ranking-track:CPU:41", 0.0);
    harness
        .driver
        .assert_ranked_fill("ranking-fill:CPU:42", "ranking-track:CPU:42", 0.0);
    harness.teardown();
}

#[gpui::test]
fn ranked_metric_help_explains_the_measured_quantity(cx: &mut TestAppContext) {
    // Hover the installed About control and advance GPUI's virtual tooltip clock.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();

    harness.driver.hover_ranked_help();

    assert!(harness.driver.reading_is_visible(
        "One-core CPU time divided by the captured continuous interval. Known subtotals \
         exclude unavailable members; zero is a measured reading."
    ));
    harness.teardown();
}

#[gpui::test]
fn a_ranking_only_failure_preserves_the_exact_historical_chart(cx: &mut TestAppContext) {
    // A successful historical snapshot can refuse App ranks while its scalar chart stays usable.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();
    harness.driver.hover_previous_second_on("live-cpu-plot");

    harness.driver.complete_unavailable_ranked_preview();

    assert!(harness.driver.reading_is_visible("13.0 %"));
    assert!(
        harness
            .driver
            .reading_is_visible("App totals unavailable: App response bound")
    );
    assert!(!harness.driver.reading_is_visible("87.0 %"));
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:CPU:Current:80.0 %")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:CPU:Historical:20.0 %")
    );
    harness.teardown();
}

#[gpui::test]
fn historical_hover_hides_current_ranks_until_its_exact_payload_arrives(cx: &mut TestAppContext) {
    // Keep charts resident while the App payload loads, then restore current ranks on plot exit.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();

    harness.driver.hover_previous_second_on("live-cpu-plot");

    assert!(
        harness
            .driver
            .reading_is_visible("Loading exact App totals")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:CPU:Current:80.0 %")
    );
    harness.driver.complete_ranked_preview();
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:CPU:Historical:20.0 %")
    );
    assert_eq!(harness.driver.requested_publication(), Some(2));
    harness
        .driver
        .leave_plot("live-cpu-plot", PlotExit::Sidebar);
    assert!(
        harness
            .driver
            .reading_is_visible("ranking:CPU:Current:80.0 %")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:CPU:Historical:20.0 %")
    );
    harness.teardown();
}

#[gpui::test]
fn failed_historical_rankings_cannot_restore_current_app_links(cx: &mut TestAppContext) {
    // Deliver a worker error for the exact hover generation and inspect the composed failure.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();
    harness.driver.hover_previous_second_on("live-cpu-plot");

    harness.driver.fail_ranked_preview();

    assert!(
        harness
            .driver
            .reading_is_visible("App totals unavailable: Ranking read failed")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:CPU:Current:80.0 %")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:CPU:Historical:20.0 %")
    );
    harness.teardown();
}

#[gpui::test]
fn a_missing_historical_observation_has_no_ranked_app_links(cx: &mut TestAppContext) {
    // A true gap has no identity and cannot borrow App totals from the committed selection.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();
    let position = harness
        .driver
        .second_position("live-cpu-plot", 897.5 / 900.0);

    harness.driver.hover_position(position);
    harness.driver.finish_missing_ranked_preview();

    assert!(
        harness
            .driver
            .reading_is_visible("App totals unavailable at this observation")
    );
    assert!(
        !harness
            .driver
            .reading_is_visible("ranking:CPU:Current:80.0 %")
    );
    harness.teardown();
}

#[gpui::test]
fn a_paused_same_second_rank_link_keeps_the_earlier_observation(cx: &mut TestAppContext) {
    // A later resident sibling must not replace the exact identity captured by the row.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();
    harness.driver.add_later_ranked_sibling();

    harness
        .driver
        .click_ranked_app("ranking:CPU:Current:80.0 %");

    harness.driver.assert_ranked_app_request(1000, 2, "41");
    harness.teardown();
}

#[gpui::test]
fn a_keyboard_inspection_rank_link_commits_its_displayed_historical_identity(
    cx: &mut TestAppContext,
) {
    // Dispatch the chart's actual arrow key before opening its selected historical App detail.
    let mut harness = InputHarness::setup_route(cx, "overview");
    harness.driver.seed_ranked_overview();
    harness.driver.inspect_ranked_history_with_keyboard();
    harness.driver.complete_ranked_selection();

    harness
        .driver
        .click_ranked_app("ranking:CPU:Historical:20.0 %");

    harness.driver.assert_ranked_app_request(999, 1, "42");
    harness.teardown();
}

impl InputDriver {
    fn set_ranked_cpu_amounts(&mut self, first: u64, second: u64) {
        use num_traits::ToPrimitive;
        self.view.update(&mut self.context, |dashboard, cx| {
            let snapshot = dashboard.snapshot.as_mut().unwrap();
            let rankings = Arc::get_mut(snapshot.rankings.as_mut().unwrap()).unwrap();
            let crate::live_rankings::Rankings::Ready(rankings) = rankings else {
                panic!()
            };
            let entries = &mut rankings.metrics[0].entries;
            entries[0].amount = first;
            entries[0].rate = Some(first.to_f64().unwrap());
            entries.push(crate::live_rankings::Entry {
                app: Arc::new(crate::live_rankings::App {
                    key: crate::live_source::AppKey {
                        kind: "bundle".into(),
                        bytes_hex: "42".into(),
                    },
                    name: "Lower".into(),
                }),
                amount: second,
                rate: Some(second.to_f64().unwrap()),
                measured_members: 1,
                unavailable_members: 0,
            });
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn assert_ranked_fill(&mut self, fill: &'static str, track: &'static str, fraction: f32) {
        let fill = self.context.debug_bounds(fill).expect(fill);
        let track = self.context.debug_bounds(track).expect(track);
        assert!((f32::from(fill.size.width - track.size.width * fraction)).abs() < 0.5);
    }

    fn hover_ranked_help(&mut self) {
        let bounds = self.context.debug_bounds("ranking-help:CPU").unwrap();
        self.context
            .simulate_mouse_move(bounds.center(), None, Modifiers::none());
        self.context.run_until_parked();
        self.context
            .executor()
            .advance_clock(std::time::Duration::from_secs(1));
        self.context.run_until_parked();
    }

    fn make_ranked_cpu_partial(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let snapshot = dashboard.snapshot.as_mut().unwrap();
            let rankings = Arc::get_mut(snapshot.rankings.as_mut().unwrap()).unwrap();
            let crate::live_rankings::Rankings::Ready(rankings) = rankings else {
                panic!()
            };
            rankings.metrics[0].partial = 1;
            rankings.metrics[0].entries[0].unavailable_members = 2;
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn seed_ranked_overview(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            snapshot.selection["trends"][0]["cpu_percent"] = json!(13);
            snapshot.selection["trends"][1]["cpu_percent"] = json!(87);
            snapshot.selection["summary"] = snapshot.selection["trends"][1].clone();
            snapshot.rankings = Some(ranked_payload(&snapshot, "Current", "41", 80));
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context
            .simulate_resize(gpui::size(px(1320.0), px(3500.0)));
        self.context.run_until_parked();
    }

    fn complete_ranked_preview(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick_at(Instant::now(), cx);
            let preview = dashboard.preview.as_ref().unwrap();
            let mut snapshot = accepted_snapshot(preview.generation.unwrap());
            snapshot.selection["second"] = json!(999);
            snapshot.selection["selected"] = preview.target.identity.clone().unwrap();
            snapshot.rankings = Some(ranked_payload(&snapshot, "Historical", "42", 20));
            assert!(dashboard.preview_receive(Update::Ready(snapshot)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn complete_unavailable_ranked_preview(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick_at(Instant::now(), cx);
            let preview = dashboard.preview.as_ref().unwrap();
            let mut snapshot = accepted_snapshot(preview.generation.unwrap());
            snapshot.selection["second"] = json!(999);
            snapshot.selection["selected"] = preview.target.identity.clone().unwrap();
            snapshot.selection["summary"] = snapshot.selection["trends"][0].clone();
            snapshot.selection["summary"]["cpu_percent"] = json!(13);
            snapshot.rankings = Some(Arc::new(crate::live_rankings::Rankings::Unavailable(
                "App response bound".into(),
            )));
            assert!(dashboard.preview_receive(Update::Ready(snapshot)));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn fail_ranked_preview(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick_at(Instant::now(), cx);
            assert!(dashboard.preview_receive(Update::Failed {
                generation: dashboard.preview.as_ref().unwrap().generation.unwrap(),
                message: "Ranking read failed".into(),
            }));
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn finish_missing_ranked_preview(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.preview_tick_at(Instant::now(), cx);
        });
        self.context.run_until_parked();
    }

    fn add_later_ranked_sibling(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let snapshot = dashboard.snapshot.as_mut().unwrap();
            let mut sibling = snapshot.selection["trends"][1].clone();
            sibling["publication"] = json!(3);
            snapshot.selection["trends"]
                .as_array_mut()
                .unwrap()
                .push(sibling);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn inspect_ranked_history_with_keyboard(&mut self) {
        let bounds = self.chart_bounds("live-cpu-plot");
        self.context
            .simulate_mouse_down(bounds.center(), MouseButton::Right, Modifiers::none());
        self.context
            .simulate_mouse_up(bounds.center(), MouseButton::Right, Modifiers::none());
        self.press("left");
        assert_eq!(self.requested_second(), Some(999));
        assert_eq!(self.requested_publication(), Some(1));
    }

    fn complete_ranked_selection(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let mut snapshot = accepted_snapshot(dashboard.generation);
            snapshot.selection["second"] = json!(999);
            snapshot.selection["selected"] = dashboard.request.identity.clone().unwrap();
            snapshot.selection["trends"]
                .as_array_mut()
                .unwrap()
                .truncate(1);
            snapshot.selection["summary"] = snapshot.selection["trends"][0].clone();
            snapshot.rankings = Some(ranked_payload(&snapshot, "Historical", "42", 20));
            dashboard.accept(snapshot);
            assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
            cx.notify();
        });
        self.context.run_until_parked();
    }

    fn click_ranked_app(&mut self, selector: &'static str) {
        let bounds = self.context.debug_bounds(selector).expect(selector);
        self.context
            .simulate_click(bounds.center(), Modifiers::none());
        self.context.run_until_parked();
    }

    fn assert_ranked_app_request(&self, second: u64, publication: u64, key: &str) {
        let mut identity = accepted_snapshot(0).selection["selected"].clone();
        identity["publication"] = json!(publication);
        let expected = crate::live_hover::Target::exact(second, &identity).unwrap();
        self.view.read_with(&self.context, |dashboard, _| {
            assert_eq!(dashboard.request.route, "apps");
            assert_eq!(dashboard.request.second, Some(expected.second));
            assert_eq!(dashboard.request.identity, expected.identity);
            assert_eq!(
                dashboard.request.app,
                Some(crate::live_source::AppKey {
                    kind: "bundle".into(),
                    bytes_hex: key.into(),
                })
            );
            assert!(dashboard.preview.is_none());
        });
    }
}

fn ranked_payload(
    snapshot: &Snapshot,
    name: &str,
    key: &str,
    cpu: u32,
) -> Arc<crate::live_rankings::Rankings> {
    let measure = json!({"known":cpu.to_string(),"measured_members":1,
        "unavailable_members":0,"overflowed":false});
    let app = json!({"display_name":name,"key":{"kind":"bundle","bytes_hex":key},
        "cpu_ns":measure,"energy_nj":measure,
        "resident_bytes":{"known":"1073741824","measured_members":1,
            "unavailable_members":0,"overflowed":false},
        "cpu_percent_one_core":cpu,"energy_watts":0.5});
    let page = json!({"selected":snapshot.selection["selected"],"apps":[app],
        "app_count":1,"membership_complete":true,"ungrouped_members":0,
        "page":{"offset":0,"limit":512,"total":1,"next_offset":null}});
    Arc::new(crate::live_rankings::read(
        &snapshot.selection["selected"],
        |_| Ok(page.clone()),
    ))
}
