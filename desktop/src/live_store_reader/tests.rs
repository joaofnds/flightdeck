use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::live_dashboard::input_tests::source_mode::LiveRecording;
use crate::live_source::{Earlier, Request, Snapshot, Update, Worker};

#[path = "lifecycle_tests.rs"]
mod lifecycle;

#[test]
fn a_store_worker_presents_primary_then_complete_from_a_queued_sealed_request() {
    // Goal: real budget backpressure preserves a request until Primary, then fills its history.
    // Method: reserve every admitted byte before construction and use the existing Pending hold.
    let (mut harness, lease, handle) = Harness::sealed_blocked();
    let hold = harness
        .worker
        .hold_measurement_stage(crate::live_source::measurement::Stage::Pending);
    let mut request = request("memory");
    request.second = Some(1063);
    request.load_resident_history = true;
    let generation = harness.worker.request(request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    for _ in 0..1000 {
        if handle.pending() {
            break;
        }
        assert!(Instant::now() < deadline, "Store request never queued");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(handle.pending());
    drop(lease);

    let primary = harness.ready_strict();

    assert_eq!(primary.generation, generation);
    assert_eq!(primary.selection["selected"]["publication"], 64);
    assert_eq!(
        primary.selection["summary"]["memory_total_bytes"],
        9_007_199_254_740_993_u64
    );
    assert!(primary.histories_pending);
    assert!(!harness.worker.last_output_complete());
    hold.release();
    let complete = harness.ready_strict();
    assert_eq!(complete.generation, generation);
    assert_eq!(
        complete.selection["selected"],
        primary.selection["selected"]
    );
    assert!(!complete.histories_pending);
    assert!(complete.resident_history.is_some());
    assert!(harness.worker.last_output_complete());
    harness.assert_source_free();
}

#[test]
fn a_kept_selected_inspection_refreshes_primary_without_moving_its_endpoint() {
    // Goal: a kept selected read cannot retain an immutable Primary forever.
    // Method: supersede the held Primary with inspection, append native data, then restore.
    let (mut harness, primary, hold) = primary_sealed(Some(1063));
    let identity = primary.selection["selected"].clone();
    inspect_primary(&mut harness, &primary, hold, 1063, identity);
}

#[test]
fn a_kept_unread_sibling_waits_for_complete_before_formatting_its_apps() {
    // Goal: an unread sibling is a continuation rather than a generic missing-ranks failure.
    // Method: inspect publication one from the immutable Primary after its request is superseded.
    let (mut harness, primary, hold) = primary_sealed(Some(1063));
    let identity = primary
        .selection
        .trends_json()
        .as_array()
        .unwrap()
        .iter()
        .find(|point| point["publication"] == 1)
        .unwrap()
        .clone();
    inspect_primary(&mut harness, &primary, hold, 1000, identity);
}

fn primary_sealed(
    selected_second: Option<u64>,
) -> (
    Harness,
    Box<Snapshot>,
    crate::live_source::measurement::Hold,
) {
    primary_sealed_under(selected_second, crate::live_source::AppChartSource::Reading)
}

/// The Primary reading `primary_sealed` gives, built while the window said `source` draws the
/// page's chart by App.
fn primary_sealed_under(
    selected_second: Option<u64>,
    source: crate::live_source::AppChartSource,
) -> (
    Harness,
    Box<Snapshot>,
    crate::live_source::measurement::Hold,
) {
    let (mut harness, lease, handle) = Harness::sealed_blocked();
    harness.worker.set_app_chart_source(source);
    let hold = harness
        .worker
        .hold_measurement_stage(crate::live_source::measurement::Stage::Pending);
    let mut request = request("memory");
    request.second = selected_second;
    request.load_resident_history = true;
    harness.worker.request(request).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    for _ in 0..1000 {
        if handle.pending() {
            break;
        }
        assert!(Instant::now() < deadline, "Store request never queued");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(handle.pending());
    drop(lease);
    let primary = harness.ready_strict();
    assert!(primary.histories_pending);
    (harness, primary, hold)
}

#[test]
fn a_following_primary_keeps_following_native_progress_through_complete() {
    // Goal: holding a Primary context cannot silently turn FollowLatest into an exact pin.
    // Method: append a native successor while the actual Primary output is held.
    let (mut harness, primary, hold) = primary_sealed(None);
    harness.recording.as_mut().unwrap().publish_resources();
    hold.release();

    for _ in 0..8 {
        let snapshot = harness.ready_strict();
        if snapshot.selection["selected"]["publication"] == 65 {
            assert_eq!(snapshot.generation, primary.generation);
            assert_eq!(snapshot.selection["context_last_second"], 1064);
            assert!(!snapshot.histories_pending);
            assert!(harness.worker.last_output_complete());
            harness.assert_source_free();
            return;
        }
    }
    panic!("following context did not advance through Complete");
}

#[test]
fn a_followed_complete_context_reacquires_the_source_after_a_restart() {
    // Goal: following a new checked source needs no new Worker request.
    // Method: replace the completed real ring and wait for its run under the original request.
    let mut harness = Harness::new();
    let generation = harness.worker.request(request("memory")).unwrap();
    let previous = harness.ready();
    let source = previous.catalog["source_generation"].as_u64().unwrap();
    drop(previous);

    let run = harness.restart_source();
    let current = harness.ready_for_run(run);

    assert_eq!(current.generation, generation);
    assert!(current.catalog["source_generation"].as_u64().unwrap() > source);
    assert_eq!(current.selection["selected"]["publication"], 2);
    assert_eq!(current.selection["selected"]["run_tick"], 1);
    assert_eq!(
        current.selection["selected"]["wall_ns"],
        1_003_000_000_000_u64
    );
    assert_eq!(current.selection["summary"]["cpu_percent"], 50.0);
    harness.assert_source_free();
}

#[test]
fn a_followed_primary_context_reacquires_the_source_after_a_restart() {
    // Goal: a held Primary context keeps logical following through source replacement.
    // Method: restart the real source while the existing Worker Pending checkpoint is held.
    let (mut harness, primary, hold) = primary_sealed(None);
    let generation = primary.generation;
    let source = primary.catalog["source_generation"].as_u64().unwrap();
    drop(primary);

    let run = harness.restart_source();
    hold.release();
    let current = harness.ready_for_run(run);

    assert_eq!(current.generation, generation);
    assert!(current.catalog["source_generation"].as_u64().unwrap() > source);
    assert_eq!(current.selection["selected"]["publication"], 2);
    assert_eq!(current.selection["selected"]["run_tick"], 1);
    assert_eq!(
        current.selection["selected"]["wall_ns"],
        1_065_000_000_000_u64
    );
    assert_eq!(current.selection["summary"]["cpu_percent"], 50.0);
    harness.assert_source_free();
}

#[test]
fn a_held_second_is_asked_for_again_when_its_source_changes() {
    // Goal: a reader whose context names a source that ended asks anew, rather than name that
    // source until its wait ends and the saved minutes answer.
    // Method: restart the real source while the Primary of a held second waits at the Worker's
    // Pending checkpoint, let the Store take in the restart, and trace what the Worker read.
    let (mut harness, primary, hold) = primary_sealed(Some(1063));
    let source = primary.catalog["source_generation"].as_u64().unwrap();
    let selected = primary.selection["selected"].clone();
    drop(primary);
    harness.restart_source();
    harness.wait_until_the_store_reads_past(source);

    hold.release();
    let current = harness.ready_strict();

    assert_eq!(current.selection["selected"], selected);
    assert!(current.catalog["source_generation"].as_u64().unwrap() > source);
    harness.assert_source_free();
}

fn inspect_primary(
    harness: &mut Harness,
    primary: &Snapshot,
    hold: crate::live_source::measurement::Hold,
    second: u64,
    identity: serde_json::Value,
) {
    harness.recording.as_mut().unwrap().publish_resources();
    let mut inspection = request("memory");
    inspection.second = Some(second);
    let publication = identity["publication"].clone();
    inspection.identity = Some(identity);
    let generation = harness.worker.inspect(inspection).unwrap();
    hold.release();
    drop(hold);
    let inspected = harness.ready_strict();
    assert_eq!(inspected.generation, generation);
    assert_eq!(inspected.selection["selected"]["publication"], publication);
    assert!(!inspected.histories_pending);
    assert!(harness.worker.last_output_complete());
    let mut restore = request("memory");
    restore.second = Some(1063);
    restore.identity = Some(primary.selection["selected"].clone());
    harness.worker.request(restore).unwrap();
    let restored = harness.ready_strict();
    assert_eq!(restored.selection["context_last_second"], 1063);
    assert_eq!(
        restored.selection["selected"],
        primary.selection["selected"]
    );
    harness.assert_source_free();
}

#[test]
fn a_new_read_waits_for_a_store_behind_the_newest_reading() {
    // Goal: a read that reaches the Store just before it reads a new reading is not a failure.
    // Method: keep the Store a reading behind while the Worker asks, then let it catch up.
    let mut harness = Harness::new();
    harness.worker.request(request("memory")).unwrap();
    harness.ready();
    harness.store_behind_the_newest_reading();

    let generation = harness.worker.request(request("memory")).unwrap();

    harness.assert_no_update_over_several_polls();
    harness.store_active();
    assert_eq!(harness.ready_strict().generation, generation);
}

#[test]
fn a_kept_refresh_waits_for_a_store_behind_the_newest_reading() {
    // Goal: a held read that reaches the Store just before it reads a new reading stays held.
    // Method: refresh a held Primary while the Store stays a reading behind, then let it catch up.
    let (mut harness, primary, hold) = primary_sealed(Some(1063));
    harness.store_behind_the_newest_reading();
    let mut inspection = request("memory");
    inspection.second = Some(1063);
    inspection.identity = Some(primary.selection["selected"].clone());

    let generation = harness.worker.inspect(inspection).unwrap();
    hold.release();
    drop(hold);

    harness.assert_no_update_over_several_polls();
    harness.store_active();
    assert_eq!(harness.ready_strict().generation, generation);
}

#[test]
fn a_held_second_read_on_another_page_answers_from_its_fixed_context() {
    // Goal: a read of the second a complete fixed context holds answers from that context while
    // the Store's span still holds the second, with no new Store query.
    // Method: hold the newest second until its context is complete, keep the Store a reading
    // behind, which a new query waits out, then read the same second on another page.
    let mut harness = Harness::new();
    harness.worker.request(request("memory")).unwrap();
    let shown = harness.ready();
    let second = shown.selection["selected"]["wall_ns"].as_u64().unwrap() / 1_000_000_000;
    let mut held = request("memory");
    held.second = Some(second);
    harness.worker.request(held).unwrap();
    let held = harness.ready();
    assert!(harness.worker.last_output_complete());
    harness.store_behind_the_newest_reading();
    let mut other = request("network");
    other.second = Some(second);

    let generation = harness.worker.request(other).unwrap();

    let answered = harness.ready_strict();
    assert_eq!(answered.generation, generation);
    assert_eq!(answered.selection["selected"], held.selection["selected"]);
    assert_eq!(answered.selection["summary"]["network_rx_bps"], 1.0);
    harness.store_active();
}

#[test]
fn a_context_limit_met_for_a_moment_holds_the_page_without_a_failure() {
    // Goal: the window holds the context it shows while the next one arrives, which fills the
    // Store's two for a moment and is no failure of the page.
    // Method: keep both contexts while the Worker asks, then let one go.
    let mut harness = Harness::new();
    let (first, second) = harness.both_contexts_held();

    let generation = harness.worker.request(request("memory")).unwrap();

    harness.assert_no_update_over_several_polls();
    drop(first);
    assert_eq!(harness.ready_strict().generation, generation);
    drop(second);
}

#[test]
fn a_context_limit_that_lasts_says_so() {
    // Goal: a page never waits for ever on contexts something else keeps.
    // Method: keep both contexts while the Worker asks, past its bounded wait.
    let mut harness = Harness::new();
    let (first, second) = harness.both_contexts_held();

    harness.worker.request(request("memory")).unwrap();

    assert_eq!(
        harness.failure(),
        "Store readings unavailable: ContextLimit"
    );
    drop((first, second));
}

#[test]
fn an_inspected_overview_reading_needs_no_store_caught_up() {
    // Goal: a picked reading of the Overview shows at once from what the Store already answered.
    // Method: pick the older of two readings while the Store stays behind the newest one.
    let mut harness = Harness::new();
    harness.worker.request(request("overview")).unwrap();
    let shown = harness.ready();
    let older = shown
        .selection
        .trends_json()
        .as_array()
        .unwrap()
        .iter()
        .find(|point| point["publication"] == 1)
        .unwrap()
        .clone();
    harness.store_behind_the_newest_reading();
    let mut inspection = request("overview");
    inspection.second = Some(older["wall_ns"].as_u64().unwrap() / 1_000_000_000);
    inspection.identity = Some(older);

    harness.worker.inspect(inspection).unwrap();

    let inspected = harness.ready_strict();
    assert_eq!(inspected.selection["selected"]["publication"], 1);
}

#[test]
fn a_store_continuation_with_no_reply_uses_the_nonzero_polling_cadence() {
    // Goal: asynchronous Store work cannot consume the Worker's local zero-delay lease.
    // Method: submit through the actual no-reply adapter while all source admission is denied.
    use crate::live_source::{Command, Context, Histories, Pending, Reader, Shared};
    let (harness, lease, handle) = Harness::sealed_blocked();
    let mut reader = Reader {
        session: None,
        store: Some(super::StoreReader::new(handle.clone())),
        last_catalog: None,
        last_generation: 1,
        last_route: String::from("memory"),
        pending: Some(Pending {
            generation: 1,
            histories: Histories::Defer,
        }),
        deferred_listing_last_second: None,
        page_caches_unread_since: None,
        earlier_tail_unread_since: None,
        retry_after: Instant::now(),
    };
    let mut command = Command {
        generation: 1,
        epoch: 0,
        request: request("memory"),
        context: Context::Select,
    };

    assert!(
        super::read(
            &mut reader,
            std::path::Path::new(""),
            &mut command,
            &Shared::new()
        )
        .is_none()
    );

    assert!(handle.pending());
    assert_eq!(
        crate::live_source::worker_pause(&reader, &command),
        crate::live_source::POLL_INTERVAL
    );
    assert!(crate::live_source::POLL_INTERVAL > Duration::ZERO);
    drop(reader);
    drop(lease);
    drop(harness);
}

#[test]
fn a_store_worker_delivers_network_without_reading_or_deriving_the_source() {
    // Goal: the staged Worker presents complete Network facts from the single Store owner.
    // Method: publish real native readings, run both owners, and trace Worker source work.
    let mut harness = Harness::new();
    harness.worker.request(request("network")).unwrap();

    let snapshot = harness.ready();

    assert_eq!(snapshot.selection["selected"]["publication"], 2);
    assert_eq!(snapshot.selection["summary"]["network_rx_bps"], 1.0);
    assert_eq!(snapshot.selection["summary"]["network_tx_bps"], 1.0);
    harness.assert_source_free();
}

#[test]
fn a_store_worker_delivers_the_computer_summary_without_chart_points() {
    // Goal: Computer, which draws no chart, gets the selected second's summary and none of the
    // chart points the other pages draw, which each publication would otherwise rebuild.
    // Method: present native readings to Computer through the staged Worker.
    let mut harness = Harness::new();
    harness.worker.request(request("computer")).unwrap();

    let snapshot = harness.ready();

    assert_eq!(snapshot.selection["selected"]["publication"], 2);
    assert_eq!(
        snapshot.selection["summary"]["memory_total_bytes"].as_u64(),
        Some(9_007_199_254_740_993)
    );
    assert_eq!(snapshot.selection.trends(), None);
    harness.assert_source_free();
}

#[test]
fn a_store_worker_delivers_memory_and_the_requested_resident_history() {
    // Goal: Memory carries its full captured integer and complete requested App history.
    // Method: present native memory readings with resident history through the staged Worker.
    let mut harness = Harness::new();
    let mut request = request("memory");
    request.load_resident_history = true;
    harness.worker.request(request).unwrap();

    let snapshot = harness.ready();

    assert_eq!(
        snapshot.selection["summary"]["memory_total_bytes"].as_u64(),
        Some(9_007_199_254_740_993)
    );
    let history = snapshot.resident_history.as_ref().unwrap();
    assert!(history.panel().is_some(), "{}", history.coverage(None));
    assert!(snapshot.rankings.is_some());
    assert!(!snapshot.histories_pending);
    harness.assert_source_free();
}

mod when_the_view_draws_the_app_chart {
    use super::*;
    use crate::live_source::AppChartSource;

    #[test]
    fn a_cpu_reading_builds_no_app_history_and_still_ranks_the_apps() {
        // Goal: the CPU page whose chart by App the View draws gets a reading with no App
        // history, and the top Apps of each second a hover reads. Method: say the View draws
        // the chart, then ask for the CPU page with its App histories as the page asks.
        let mut harness = Harness::new();
        harness.worker.set_app_chart_source(AppChartSource::View);

        let snapshot = harness.read_with_histories("cpu");

        assert!(snapshot.app_attribution_history.is_none());
        assert!(snapshot.app_ranks.is_some());
        harness.assert_source_free();
    }

    #[test]
    fn a_reading_says_it_left_the_chart_to_the_view() {
        // Goal: a reading built while the View draws the chart says so, so the page reads its
        // missing App history as the chart the View drew. Method: say the View draws the
        // chart, then ask for the CPU page.
        let mut harness = Harness::new();
        harness.worker.set_app_chart_source(AppChartSource::View);

        let snapshot = harness.read_with_histories("cpu");

        assert_eq!(snapshot.app_chart_source, AppChartSource::View);
        harness.assert_source_free();
    }

    #[test]
    fn a_memory_reading_builds_no_resident_history_and_still_ranks_the_apps() {
        // Goal: as for the CPU page, the Memory page whose chart the View draws gets no
        // resident history and the top Apps of each second. Method: as for the CPU page.
        let mut harness = Harness::new();
        harness.worker.set_app_chart_source(AppChartSource::View);

        let snapshot = harness.read_with_histories("memory");

        assert!(snapshot.resident_history.is_none());
        assert!(snapshot.app_ranks.is_some());
        harness.assert_source_free();
    }

    #[test]
    fn an_overview_reading_builds_its_histories_all_the_same() {
        // Goal: Overview's readings always bring its charts by App, so a request for Overview
        // sent with nothing said since a page the View drew still carries them, and never says
        // it left them to the View. Method: say the View draws the chart, then ask for Overview.
        let mut harness = Harness::new();
        harness.worker.set_app_chart_source(AppChartSource::View);

        let snapshot = harness.read_with_histories("overview");

        assert!(snapshot.resident_history.is_some());
        assert_eq!(snapshot.app_chart_source, AppChartSource::Reading);
        harness.assert_source_free();
    }
}

mod when_the_reading_draws_the_app_chart {
    use super::*;
    use crate::live_source::AppChartSource;

    #[test]
    fn a_cpu_reading_builds_the_app_history() {
        // Goal: a page that draws its chart by App from the worker's reading gets the history.
        // Method: say the reading draws it after the View did, then ask for the CPU page.
        let mut harness = Harness::new();
        harness.worker.set_app_chart_source(AppChartSource::View);
        harness.worker.set_app_chart_source(AppChartSource::Reading);

        let snapshot = harness.read_with_histories("cpu");

        assert!(snapshot.app_attribution_history.is_some());
        assert!(snapshot.app_ranks.is_some());
        harness.assert_source_free();
    }

    #[test]
    fn a_reading_says_it_brought_the_chart() {
        // Goal: a reading built while the reading draws the chart says so, so the page reads
        // its App history as the chart. Method: say the reading draws it after the View did,
        // then ask for the CPU page.
        let mut harness = Harness::new();
        harness.worker.set_app_chart_source(AppChartSource::View);
        harness.worker.set_app_chart_source(AppChartSource::Reading);

        let snapshot = harness.read_with_histories("cpu");

        assert_eq!(snapshot.app_chart_source, AppChartSource::Reading);
        harness.assert_source_free();
    }
}

mod when_a_reading_reads_no_app_history {
    use super::*;
    use crate::live_source::AppChartSource;

    #[test]
    fn a_hover_s_reading_says_the_reading_draws_the_chart() {
        // Goal: a hover's reading, which reads no App history whatever the page draws, left
        // nothing to the View, so the page treats it as it did before readings said what drew
        // their chart. Method: say the View draws the chart, read the Memory page, then
        // inspect its second as a hover does.
        let mut harness = Harness::new();
        harness.worker.set_app_chart_source(AppChartSource::View);
        let page = harness.read_with_histories("memory");
        let mut inspection = request("memory");
        inspection.second = page.selection["second"].as_u64();
        inspection.identity = Some(page.selection["selected"].clone());
        harness.worker.inspect(inspection).unwrap();

        let inspected = harness.ready();

        assert_eq!(inspected.app_chart_source, AppChartSource::Reading);
        harness.assert_source_free();
    }

    #[test]
    fn a_reading_whose_histories_are_pending_says_the_reading_draws_the_chart() {
        // Goal: a reading that leaves its App histories to a later one left nothing to the
        // View, so the page treats it as it did before readings said what drew their chart.
        // Method: say the View draws the chart, then hold the Store's answer back until the
        // worker presents its first reading of the Memory page.
        let (harness, primary, hold) = primary_sealed_under(None, AppChartSource::View);

        assert_eq!(primary.app_chart_source, AppChartSource::Reading);
        hold.release();
        harness.assert_source_free();
    }
}

mod the_top_apps_of_each_second {
    use super::*;
    use crate::live_source::store_reader::ownership::RanksWindow;

    /// The wall seconds the store holds in one block at most.
    const HALF_MINUTE_SECONDS: u64 = 30;
    /// A wall second the recording, which starts at second 1000, never held.
    const SECOND_BEFORE_THE_RECORDING: u64 = 500;

    /// The top Apps of each second of the reading whose selected second is the tick of
    /// `publication`, which the worker reaches once the store has taken that tick, and the key of
    /// the oldest second of its window. The reading itself is let go of, since the store admits
    /// two contexts at once and a reading holds one.
    fn ranked_at(
        harness: &mut Harness,
        publication: u64,
    ) -> (Arc<RanksWindow>, viewer::ObservationKey) {
        for _ in 0..32 {
            let snapshot = harness.ready();
            if snapshot.selection["selected"]["publication"] == publication {
                let oldest =
                    crate::live_rankings::observation_key(&snapshot.selection.trends_json()[0]);

                return (
                    Arc::clone(snapshot.app_ranks.as_ref().unwrap()),
                    oldest.unwrap(),
                );
            }
        }
        panic!("the Worker read no reading of publication {publication}");
    }

    #[test]
    fn a_followed_page_shares_the_ranks_of_a_second_the_reading_before_ranked() {
        // Goal: a page that follows the newest second ranks the Apps of a second once, and not
        // again at each reading.
        // Method: read the CPU page three times, a second apart, once a tick in the next half
        // minute has closed the block of its oldest second, and compare where the readings hold
        // that second's ranks. The store packs a closed block once, which has one reading rank
        // the block's seconds again, so the two readings before it or the two after it share
        // the ranks.
        let mut harness = Harness::new();
        let recording = harness.recording.as_mut().unwrap();
        recording.skip_seconds(HALF_MINUTE_SECONDS);
        recording.publish_resources();
        let mut request = request("cpu");
        request.load_app_attribution_history = true;
        harness.worker.request(request).unwrap();
        let (first, oldest) = ranked_at(&mut harness, 3);
        harness.recording.as_mut().unwrap().publish_resources();
        let (second, _) = ranked_at(&mut harness, 4);
        harness.recording.as_mut().unwrap().publish_resources();

        let (third, _) = ranked_at(&mut harness, 5);

        let shared = [
            std::ptr::eq(first.get(&oldest).unwrap(), second.get(&oldest).unwrap()),
            std::ptr::eq(second.get(&oldest).unwrap(), third.get(&oldest).unwrap()),
        ];
        assert!(shared.contains(&true));
    }

    /// The top Apps of each second of the CPU page, asked for anew over the two seconds the
    /// recording starts with, and the key of the older one. Both lie in the block the store
    /// still adds to, which it never packs, and no tick joins it, so only the reader decides
    /// whether two such readings share a second's ranks.
    fn cpu_ranks(harness: &mut Harness) -> (Arc<RanksWindow>, viewer::ObservationKey) {
        let mut request = request("cpu");
        request.load_app_attribution_history = true;
        harness.worker.request(request).unwrap();

        ranked_at(harness, 2)
    }

    #[test]
    fn a_page_asked_for_again_shares_the_ranks_of_the_reading_before() {
        // Goal: a new request for a page, as the end of a hover makes, costs no second ranking
        // of the seconds the page already ranked.
        // Method: ask for the CPU page twice over the same seconds and compare where the two
        // readings hold a second's ranks.
        let mut harness = Harness::new();
        let (first, oldest) = cpu_ranks(&mut harness);

        let (second, _) = cpu_ranks(&mut harness);

        assert!(std::ptr::eq(
            first.get(&oldest).unwrap(),
            second.get(&oldest).unwrap()
        ));
    }

    #[test]
    fn a_page_that_shows_no_top_apps_lets_go_of_the_ranks() {
        // Goal: the reader holds the ranks only while a page shows them, so a page that shows
        // none leaves no window of them behind.
        // Method: read the Network page between two readings of the CPU page over the same
        // seconds, and compare where the two hold a second's ranks.
        let mut harness = Harness::new();
        let (first, oldest) = cpu_ranks(&mut harness);
        drop(harness.read_with_histories("network"));

        let (second, _) = cpu_ranks(&mut harness);

        assert!(!std::ptr::eq(
            first.get(&oldest).unwrap(),
            second.get(&oldest).unwrap()
        ));
    }

    #[test]
    fn a_rest_lets_go_of_the_ranks() {
        // Goal: a reader whose window closed holds no ranks while no page reads it.
        // Method: hand the worker over to a next window between two readings of the CPU page
        // over the same seconds, and compare where the two hold a second's ranks.
        let mut harness = Harness::new();
        let (first, oldest) = cpu_ranks(&mut harness);
        harness.worker = harness.worker.hand_over().unwrap().unwrap();

        let (second, _) = cpu_ranks(&mut harness);

        assert!(!std::ptr::eq(
            first.get(&oldest).unwrap(),
            second.get(&oldest).unwrap()
        ));
    }

    #[test]
    fn a_page_the_saved_minutes_answer_lets_go_of_the_ranks() {
        // Goal: once the saved minutes answer a page, nothing shows the ranks of the Store's
        // seconds, so the reader holds none.
        // Method: between two readings of the CPU page over the same seconds, ask for a second
        // from before the recording, which the Store leaves to the saved minutes, and compare
        // where the two readings hold a second's ranks.
        let mut harness = Harness::new();
        let (first, oldest) = cpu_ranks(&mut harness);
        let mut before = request("cpu");
        before.second = Some(SECOND_BEFORE_THE_RECORDING);
        before.load_app_attribution_history = true;
        harness.worker.request(before).unwrap();
        answered(&mut harness);

        let (second, _) = cpu_ranks(&mut harness);

        assert!(!std::ptr::eq(
            first.get(&oldest).unwrap(),
            second.get(&oldest).unwrap()
        ));
    }

    #[test]
    fn a_hover_the_saved_minutes_answer_keeps_the_ranks() {
        // Goal: a hover's reply goes to the hover and leaves the page showing the Store's ranks,
        // so a hover the saved minutes answer costs the page's next reading no second ranking.
        // Method: between two readings of the CPU page over the same seconds, hover a second
        // from before the recording, which the Store leaves to the saved minutes, and compare
        // where the two readings hold a second's ranks.
        let mut harness = Harness::new();
        let (first, oldest) = cpu_ranks(&mut harness);
        let mut hover = request("cpu");
        hover.second = Some(SECOND_BEFORE_THE_RECORDING);
        hover.identity = Some(serde_json::json!({
            "run": "00000000000000000000000000000001",
            "boot": "00000000000000000000000000000001",
            "publication": 1,
        }));
        harness.worker.inspect(hover).unwrap();
        answered(&mut harness);

        let (second, _) = cpu_ranks(&mut harness);

        assert!(std::ptr::eq(
            first.get(&oldest).unwrap(),
            second.get(&oldest).unwrap()
        ));
    }

    /// Waits for the worker to answer its request, with a reading or with a failure.
    fn answered(harness: &mut Harness) {
        let deadline = Instant::now() + Duration::from_secs(10);
        for _ in 0..1000 {
            if harness.worker.take().unwrap().is_some() {
                return;
            }
            assert!(Instant::now() < deadline, "the Worker did not answer");
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the Worker did not answer within its attempts");
    }
}

#[test]
fn a_store_worker_delivers_cpu_detail_and_history_with_matching_identity() {
    // Goal: CPU uses complete Store counters for both selected detail and its history.
    // Method: request CPU after two native publications, then validate existing decoders' output.
    let mut harness = Harness::new();
    harness.worker.request(request("cpu")).unwrap();

    let snapshot = harness.ready();

    let detail = snapshot.cpu_detail.as_ref().unwrap();
    assert_eq!(detail.cpus.len(), 1);
    assert_eq!(detail.cpus[0].busy_percent, Some(50.0));
    assert!(snapshot.cpu_history.is_some());
    harness.assert_source_free();
}

#[test]
fn cached_rankings_do_not_prevent_visiting_another_resource_page() {
    // Goal: cached resource pages cannot exhaust the Store's replaceable query contexts.
    // Method: retain Memory and CPU snapshots while requesting Energy through the real Worker.
    let mut harness = Harness::new();
    harness.worker.request(request("memory")).unwrap();
    let memory = harness.ready();
    harness.worker.request(request("cpu")).unwrap();
    let cpu = harness.ready();

    harness.worker.request(request("energy")).unwrap();
    let energy = harness.ready();

    assert!(memory.rankings.is_some());
    assert!(cpu.rankings.is_some());
    assert!(energy.rankings.is_some());
    assert_eq!(memory.selection["selected"], energy.selection["selected"]);
    assert_eq!(cpu.selection["selected"], energy.selection["selected"]);
    harness.assert_source_free();
}

#[test]
fn excessive_saved_core_states_are_refused_within_the_machine_scratch_allowance() {
    // Goal: a shape-valid saved state domain cannot allocate beyond admitted reducer scratch.
    // Method: supply a million distinct states, prepared outside the calling-thread measure.
    let before = core_reading(1, 1_000_000, 1);
    let current = core_reading(2, 1_000_000, 1);
    let budget = Arc::new(viewer::store::ByteBudget::new(16 * 1024 * 1024));

    let (result, allocated) = crate::allocation_tests::measure(|| {
        viewer::store::MachineSecond::complete_of(
            &viewer::store::readings(&current).next().unwrap(),
            Some(&viewer::store::readings(&before).next().unwrap()),
            &budget,
        )
    });

    let second = result.unwrap().unwrap();
    assert!(matches!(
        second.complete,
        viewer::store::CompleteMachineState::Refused("core level states")
    ));
    assert!(allocated.bytes <= 8 * 1024 * 1024, "{allocated:?}");
}

#[test]
fn a_full_core_state_domain_still_combines_other_cores_without_another_key() {
    // Goal: reaching the key limit preserves valid accumulation across cores.
    // Method: two cores fill 512 keys, gaining one unit at 1 GHz and three at 3 GHz.
    let before = core_reading_weighted(1);
    let current = core_reading_weighted(2);
    let budget = Arc::new(viewer::store::ByteBudget::new(16 * 1024 * 1024));

    let second = viewer::store::MachineSecond::complete_of(
        &viewer::store::readings(&current).next().unwrap(),
        Some(&viewer::store::readings(&before).next().unwrap()),
        &budget,
    )
    .unwrap()
    .unwrap();

    assert_eq!(
        second.complete().unwrap().summary(&second)["cpu_level_ghz"],
        serde_json::json!([2.5, null])
    );
}

fn core_reading_weighted(publication: u64) -> viewer::recording::MachineMinute {
    let mut reading = core_reading(publication, 512, 2);
    for row in &mut reading.core_states {
        let gained = match (row.core, row.state) {
            (12, 0) => 1,
            (13, 1) => 3,
            _ => 0,
        };
        row.active_ns = (publication - 1) * gained;
        row.frequency_khz = Some(match row.state {
            0 => 1_000_000,
            1 => 3_000_000,
            _ => 2_000_000,
        });
    }

    reading
}

fn core_reading(publication: u64, states: u32, cores: u32) -> viewer::recording::MachineMinute {
    use viewer::recording::{CoreStateRow, MachineMinute, MemoryRow};
    let core_states = (12..12 + cores)
        .flat_map(|core| {
            (0..states).map(move |state| CoreStateRow {
                publication,
                core,
                state,
                active_ns: publication,
                frequency_khz: Some(2_000_000),
            })
        })
        .collect();

    MachineMinute {
        ticks: vec![core_tick(publication)],
        cpus: (0..18)
            .map(|cpu| viewer::recording::CpuRow {
                publication,
                cpu,
                ticks: collector::machine::CPUTicks {
                    user: u32::try_from(publication).unwrap(),
                    idle: u32::try_from(publication).unwrap(),
                    ..collector::machine::CPUTicks::default()
                },
            })
            .collect(),
        memory: vec![MemoryRow {
            publication,
            memory: collector::machine::Memory::default(),
        }],
        core_states,
        ..MachineMinute::default()
    }
}

fn core_tick(publication: u64) -> viewer::recording::TickRow {
    viewer::recording::TickRow {
        gpu_state_count: 0,
        publication,
        run_tick: publication,
        wall_ns: Some(publication * 1_000_000_000),
        continuous_ns: Some(publication * 1_000_000_000),
        awake_ns: Some(publication * 1_000_000_000),
        ioreport_read: true,
        smc_read: false,
        gpu_energy_nj: Some(0),
        power_watts: None,
        soc_celsius: None,
        cpu_celsius: None,
        gpu_celsius: None,
        thermal_pressure: None,
        performance_levels: Some(crate::live_store_fixtures::store_levels_named(
            "Fast", "Slow",
        )),
        system: Some(collector::ring::SystemSnapshot {
            machine: collector::system::MachineFacts {
                levels: Some(crate::live_store_fixtures::store_levels_named(
                    "Fast", "Slow",
                )),
                ..collector::system::MachineFacts::default()
            },
            ..collector::ring::SystemSnapshot::default()
        }),
        saved_computer: None,
        saved_load_average: None,
        battery: None,
        disks: None,
    }
}

#[test]
fn a_page_opens_without_waiting_out_the_workers_poll() {
    // Goal: the real worker over the real Store shows a page sooner than its poll interval.
    // Method: open pages over and over and take the quickest. A pause the Store's answer does not
    // end costs every read the poll interval, however fast the machine, and one quick read among
    // these shows it ended, however busy the machine is for the others.
    let mut harness = Harness::new();
    harness.worker.request(request("network")).unwrap();
    harness.ready();
    let routes = ["battery", "network"];

    let quickest = (0..20)
        .map(|index| {
            let started = Instant::now();
            harness.worker.request(request(routes[index % 2])).unwrap();
            harness.ready();
            started.elapsed()
        })
        .min()
        .unwrap();

    assert!(quickest < crate::live_source::POLL_INTERVAL);
}

struct Harness {
    worker: Worker,
    store: Option<viewer::store::Store>,
    recording: Option<LiveRecording>,
    trace: persistence::work::ingestion::Trace,
    scope: Option<persistence::work::ingestion::Scope>,
}

impl Harness {
    fn restart_source(&mut self) -> collector::ring::Identity {
        let recording = self.recording.as_mut().unwrap();
        let identity = recording.restart();
        recording.publish_resources();
        recording.publish_resources();

        identity.run
    }

    fn ready_for_run(&mut self, run: collector::ring::Identity) -> Box<Snapshot> {
        let run = collector::recording::identity_text(run).to_string();
        for _ in 0..8 {
            let snapshot = self.ready();
            if snapshot.selection["selected"]["run"] == run {
                return snapshot;
            }
        }

        panic!("Store Worker did not follow the replacement run");
    }

    fn sealed_blocked() -> (
        Self,
        viewer::store::ByteReservation,
        viewer::store::QueryHandle,
    ) {
        let mut recording = LiveRecording::new();
        recording.publish_resources();
        recording.publish_resources();
        seal_recording(&recording, 0);
        for _ in 0..62 {
            recording.publish_resources();
        }
        seal_recording(&recording, 1);
        let budget = Arc::new(viewer::store::ByteBudget::new(128 * 1024 * 1024));
        let lease = budget.reserve(budget.maximum()).unwrap();
        let trace = persistence::work::ingestion::Trace::new(
            collector::recording::read_identity(&recording.root).unwrap(),
        );
        let scope = trace.enter();
        let store = viewer::store::Store::start_with_budget(&recording.root, budget).unwrap();
        let handle = store.query_handle();
        let worker = Worker::start_from_store(recording.root.clone(), handle.clone()).unwrap();
        (
            Self {
                worker,
                store: Some(store),
                recording: Some(recording),
                trace,
                scope: Some(scope),
            },
            lease,
            handle,
        )
    }

    fn wait_until_the_store_reads_past(&self, source: u64) {
        let handle = self.store.as_ref().unwrap().query_handle();
        let deadline = Instant::now() + Duration::from_secs(10);
        for _ in 0..1000 {
            if Self::ready_context(&handle).source.generation > source {
                return;
            }
            assert!(Instant::now() < deadline, "the Store kept its source");
        }
        panic!("the Store kept its source within its attempts");
    }

    fn store_behind_the_newest_reading(&mut self) {
        let store = self.store.as_ref().unwrap();
        store.set_active(false);
        Self::wait_until_paused(store);
        self.recording.as_mut().unwrap().publish_resources();
    }

    fn wait_until_paused(store: &viewer::store::Store) {
        let deadline = Instant::now() + Duration::from_secs(10);
        for _ in 0..1000 {
            if store.is_paused() {
                break;
            }
            assert!(Instant::now() < deadline, "Store never paused");
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(store.is_paused());
    }

    fn store_active(&self) {
        self.store.as_ref().unwrap().set_active(true);
    }

    fn assert_no_update_over_several_polls(&mut self) {
        for _ in 0..3 {
            std::thread::sleep(crate::live_source::POLL_INTERVAL);
            match self.worker.take().unwrap() {
                None => {}
                Some(Update::Failed { message, .. }) => panic!("Store Worker failed: {message}"),
                Some(Update::Ready(_)) => panic!("Store Worker answered from a Store behind"),
            }
        }
    }

    fn both_contexts_held(
        &self,
    ) -> (
        Arc<viewer::store::QueryContext>,
        Arc<viewer::store::QueryContext>,
    ) {
        let handle = self.store.as_ref().unwrap().query_handle();
        let first = Self::ready_context(&handle);
        let second = Self::ready_context(&handle);
        assert_eq!(handle.contexts_held(), 2);

        (first, second)
    }

    fn ready_context(handle: &viewer::store::QueryHandle) -> Arc<viewer::store::QueryContext> {
        let deadline = Instant::now() + Duration::from_secs(10);
        for _attempt in 0..1_000 {
            handle
                .submit(viewer::store::QueryRequest {
                    epoch: 0,
                    source: None,
                    selection: viewer::store::QuerySelection::FollowLatest,
                })
                .unwrap();
            std::thread::sleep(Duration::from_millis(10));
            if let Some(reply) = handle.take() {
                if let viewer::store::QueryOutcome::Ready(context) = reply.outcome {
                    return context;
                }
            }
            assert!(Instant::now() < deadline, "the Store gave no context");
        }
        panic!("the Store gave no context within its attempts");
    }

    fn failure(&mut self) -> String {
        let deadline = Instant::now() + Duration::from_secs(20);
        for _attempt in 0..2_000 {
            match self.worker.take().unwrap() {
                None => {}
                Some(Update::Failed { message, .. }) => return message,
                Some(Update::Ready(_)) => panic!("the Worker read past the limit"),
            }
            assert!(Instant::now() < deadline, "the Worker did not fail");
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the Worker did not fail within its attempts");
    }

    fn ready_strict(&mut self) -> Box<Snapshot> {
        let deadline = Instant::now() + Duration::from_secs(10);
        for _ in 0..1000 {
            if let Some(update) = self.worker.take().unwrap() {
                match update {
                    Update::Ready(snapshot) => return snapshot,
                    Update::Failed { message, .. } => panic!("Store Worker failed: {message}"),
                }
            }
            assert!(Instant::now() < deadline, "Store Worker did not reply");
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("Store Worker exceeded bounded attempts");
    }
    fn new() -> Self {
        let mut recording = LiveRecording::new();
        recording.publish_resources();
        recording.publish_resources();
        let identity = collector::recording::read_identity(&recording.root).unwrap();
        let trace = persistence::work::ingestion::Trace::new(identity);
        let scope = trace.enter();
        let store = viewer::store::Store::start_with_budget(
            &recording.root,
            Arc::new(viewer::store::ByteBudget::new(128 * 1024 * 1024)),
        )
        .unwrap();
        let worker =
            Worker::start_from_store(recording.root.clone(), store.query_handle()).unwrap();

        Self {
            worker,
            store: Some(store),
            recording: Some(recording),
            trace,
            scope: Some(scope),
        }
    }

    fn ready(&mut self) -> Box<Snapshot> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut failure = None;
        for _attempt in 0..1_000 {
            if let Some(update) = self.worker.take().unwrap() {
                match update {
                    Update::Ready(snapshot) => {
                        if !snapshot.histories_pending {
                            return snapshot;
                        }
                    }
                    Update::Failed { message, .. } => failure = Some(message),
                }
            }
            assert!(
                Instant::now() < deadline,
                "Store worker did not become ready: {failure:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("Store worker exceeded its bounded attempts: {failure:?}");
    }

    /// The worker's whole reading of the page `route`, asked for with its App histories as the
    /// window asks for a page.
    fn read_with_histories(&mut self, route: &str) -> Box<Snapshot> {
        let mut request = request(route);
        request.load_resident_history = true;
        request.load_app_attribution_history = true;
        self.worker.request(request).unwrap();

        self.ready()
    }

    fn assert_source_free(&self) {
        use persistence::work::ingestion::Producer;
        let mut events = Vec::new();
        self.trace.snapshot(&mut events).unwrap();
        let reads = events
            .iter()
            .filter(|event| event.producer == Producer::Worker)
            .count();

        assert_eq!(reads, 0);
    }
}

/// Derives no table, as a window saved before derived tables holds none.
struct SealUnderived;

impl persistence::worker::Derive for SealUnderived {
    fn derive(
        &self,
        _derivation: &persistence::worker::Derivation<'_>,
    ) -> persistence::Result<Vec<persistence::TableManifest>> {
        Ok(Vec::new())
    }
}

/// Saves what the ring holds of the run through `minute`, with the persister's own worker.
pub(in crate::live_source) fn seal_recording(recording: &LiveRecording, minute: u64) {
    let registry = collector::recording::Registry::read(&recording.root).unwrap();
    let store = persistence::Store::open(&recording.root).unwrap();
    persistence::worker::seal_held_windows(
        &store,
        registry.runs.last().unwrap(),
        Some(minute),
        &mut persistence::worker::HeldWindows::default(),
        &SealUnderived,
    )
    .unwrap();
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.worker.stop();
        drop(self.store.take());
        drop(self.recording.take());
        drop(self.scope.take());
    }
}

fn request(route: &str) -> Request {
    Request {
        route: route.into(),
        second: None,
        identity: None,
        app: None,
        apps_offset: 0,
        members_offset: 0,
        cpu_offset: 0,
        load_resident_history: false,
        load_app_attribution_history: false,
        load_process_history: false,
        earlier: Earlier::Skip,
        executable: None,
    }
}

/// A root the saved minutes refuse before they read anything, since a glob in a path is refused
/// as it is checked, so a reading the saved minutes answer fails at once.
const NO_RECORDING: &str = "no-recording-[here]";

/// A worker's reader whose Store reader asks `store`, which answers only when the test serves it,
/// and whose page of generation 1 the Store has not yet answered.
fn reader(store: &viewer::store::chart_fixture::Charted) -> crate::live_source::Reader {
    let mut store_reader = crate::live_source::store_reader::StoreReader::new(store.handle());
    store_reader.generation = 1;
    crate::live_source::Reader {
        session: None,
        store: Some(store_reader),
        last_catalog: None,
        last_generation: 1,
        last_route: String::from("network"),
        pending: Some(crate::live_source::Pending {
            generation: 1,
            histories: crate::live_source::Histories::Defer,
        }),
        deferred_listing_last_second: None,
        page_caches_unread_since: None,
        earlier_tail_unread_since: None,
        retry_after: Instant::now(),
    }
}

/// The worker's step that reads `command` through `reader`.
fn reader_step(
    reader: &mut crate::live_source::Reader,
    command: &mut crate::live_source::Command,
) -> Option<Update> {
    crate::live_source::store_reader::read(
        reader,
        std::path::Path::new(NO_RECORDING),
        command,
        &crate::live_source::Shared::new(),
    )
}

/// Whether `update` is the failure of a reading the saved minutes answered.
fn reader_step_failed_saved(update: Option<Update>) -> bool {
    matches!(
        update,
        Some(Update::Failed { message, .. }) if message.contains("query path glob")
    )
}

mod the_chart_points_of_each_second {
    use super::*;
    use crate::live_source::store_reader::{Reused, STORE_WAITS_MAX, StoreReader, payload};
    use crate::live_source::{Command, Context, Reader, Shared};
    use viewer::CHARTED_SECONDS;
    use viewer::store::chart_fixture::{
        Charted, FIRST_SECOND, charted, charted_point_count, shared_count,
    };

    /// The reading of `route` that follows the newest second `store` holds, sharing what
    /// `reused` holds.
    fn read(store: &Charted, route: &str, reused: &mut Reused) -> Box<Snapshot> {
        let mut command = Command {
            generation: 1,
            epoch: 0,
            request: request(route),
            context: Context::Select,
        };

        payload::snapshot(
            &store.followed(),
            &mut command,
            &Shared::new(),
            None,
            reused,
        )
        .unwrap()
    }

    /// What `reader`'s Store reader reuses from the reading before.
    fn reused(reader: &mut Reader) -> &mut Reused {
        &mut reader.store.as_mut().unwrap().reused
    }

    #[test]
    fn a_page_that_ranks_no_apps_shares_every_point_but_the_newest() {
        // Goal: a page that draws a chart and shows no top Apps still builds each second's
        // chart point once, since letting go of the ranks lets go of no chart point.
        // Method: read the Network page, take in one more second, read it again with what the
        // first reading left, and count the points the two readings share.
        let mut store = charted();
        let mut reused = Reused::default();
        let first = read(&store, "network", &mut reused);
        store.take(FIRST_SECOND + CHARTED_SECONDS);

        let second = read(&store, "network", &mut reused);

        assert_eq!(
            first.selection.trends().unwrap().len(),
            charted_point_count()
        );
        assert_eq!(
            second.selection.trends().unwrap().len(),
            charted_point_count()
        );
        assert_eq!(
            shared_count(&first.selection, &second.selection),
            charted_point_count() - 1
        );
    }

    #[test]
    fn a_page_that_draws_no_chart_lets_go_of_the_points() {
        // Goal: the reader holds chart points only while a page draws them.
        // Method: read the Network page, then the Computer page, which draws no chart, and see
        // what the reader keeps.
        let store = charted();
        let mut reused = Reused::default();
        drop(read(&store, "network", &mut reused));
        assert_eq!(reused.chart_points.point_count(), charted_point_count());

        drop(read(&store, "computer", &mut reused));

        assert!(reused.chart_points.is_empty());
    }

    #[test]
    fn a_page_that_shows_no_reading_lets_go_of_the_points() {
        // Goal: Self-monitoring, which shows no reading of the recording and so draws no chart
        // of it, lets go of the points the page before it drew.
        // Method: read the Network page, then Self-monitoring, and see what the reader keeps.
        let store = charted();
        let mut reused = Reused::default();
        drop(read(&store, "network", &mut reused));
        assert_eq!(reused.chart_points.point_count(), charted_point_count());

        drop(read(&store, "self", &mut reused));

        assert!(reused.chart_points.is_empty());
    }

    #[test]
    fn a_hover_between_two_readings_leaves_the_points_of_the_first() {
        // Goal: a hover of a page that draws a chart builds no chart and keeps the reader's
        // points, so the page's next reading still builds only the newest second's point.
        // Method: read the Network page, hover an older second of its context, take in one
        // more second, read the page again, and count the points the two readings share.
        let mut store = charted();
        let mut reused = Reused::default();
        let first = read(&store, "network", &mut reused);
        let context = store.followed();
        let hovered = context.seconds()[100].observation;
        let mut hover = Command {
            generation: 2,
            epoch: 0,
            request: request("network"),
            context: Context::Keep,
        };
        drop(
            payload::snapshot(
                &context,
                &mut hover,
                &Shared::new(),
                Some(&hovered),
                &mut reused,
            )
            .unwrap(),
        );
        drop(context);
        store.take(FIRST_SECOND + CHARTED_SECONDS);

        let second = read(&store, "network", &mut reused);

        assert_eq!(
            shared_count(&first.selection, &second.selection),
            charted_point_count() - 1
        );
    }

    #[test]
    fn a_page_the_saved_minutes_select_lets_go_of_the_points() {
        // Goal: a selected page the saved minutes answer draws its charts from them, so the
        // reader keeps none of the Store's points for it.
        // Method: fill a reader's points from the Network page, have its Store reader wait out
        // its bound on a store that answers nothing, so the saved minutes answer the next
        // step, and see what it keeps.
        let store = charted();
        let mut reader = reader(&store);
        drop(read(&store, "network", reused(&mut reader)));
        let mut command = Command {
            generation: 1,
            epoch: 0,
            request: request("network"),
            context: Context::Select,
        };
        assert!(reader_step(&mut reader, &mut command).is_none());
        reader.store.as_mut().unwrap().store_waits = STORE_WAITS_MAX;

        let update = reader_step(&mut reader, &mut command);

        assert!(reader_step_failed_saved(update));
        assert!(reused(&mut reader).chart_points.is_empty());
    }

    #[test]
    fn a_hover_the_saved_minutes_answer_keeps_the_points() {
        // Goal: a hover's reply goes to the hover and leaves the page's charts the Store's, so
        // a hover of a second the context no longer holds, which the saved minutes answer,
        // keeps the points the page's next reading shares.
        // Method: fill a reader's points from the Network page, give its Store reader the
        // context, hover a second before it, and see what it keeps.
        let store = charted();
        let mut reader = reader(&store);
        drop(read(&store, "network", reused(&mut reader)));
        reader.store.as_mut().unwrap().context = Some(store.followed());
        let mut hover = request("network");
        hover.second = Some(FIRST_SECOND - 1);
        hover.identity = Some(serde_json::json!({}));
        let mut command = Command {
            generation: 1,
            epoch: 0,
            request: hover,
            context: Context::Keep,
        };

        let update = reader_step(&mut reader, &mut command);

        assert!(reader_step_failed_saved(update));
        assert_eq!(
            reused(&mut reader).chart_points.point_count(),
            charted_point_count()
        );
    }

    #[test]
    fn a_rest_lets_go_of_the_points() {
        // Goal: a reader whose window closed holds no chart points while no page reads it.
        // Method: have a reader read the Network page, rest it, and see what it keeps.
        let store = charted();
        let mut store_reader = StoreReader::new(store.handle());
        drop(read(&store, "network", &mut store_reader.reused));
        assert_eq!(
            store_reader.reused.chart_points.point_count(),
            charted_point_count()
        );

        store_reader.rest();

        assert!(store_reader.reused.chart_points.is_empty());
    }
}

mod when_a_page_follows_the_newest_second {
    use std::sync::atomic::{AtomicU32, Ordering};

    use super::*;
    use crate::live_source::store_reader::{STORE_WAITS_KEYED_MAX, STORE_WAITS_MAX};
    use crate::live_source::{Command, Context, Reader, Shared};
    use viewer::CHARTED_SECONDS;
    use viewer::store::chart_fixture::{Charted, FIRST_SECOND, charted};

    #[test]
    fn a_second_the_store_takes_is_published_without_waiting_out_the_workers_poll() {
        // Goal: on a page in the steady state, the answer to the follow question ends the
        // worker's pause, so the reading of a second follows the Store's turn that took it.
        // Method: run the real worker over a store the test serves. Each round, take a second in,
        // and once the worker's question is out, serve and time the reading that carries it, so
        // nothing but the serve runs between the question and the clock. A pause the answer does
        // not end costs every round nearly the whole poll interval, and one quick round among
        // these shows it ended, however busy the machine is for the others.
        let mut store = charted();
        let handle = store.handle();
        let mut worker =
            Worker::start_from_store(std::path::PathBuf::from(NO_RECORDING), store.handle())
                .unwrap();
        worker.request(request("network")).unwrap();
        worker_round(&mut store, &handle, &mut worker, None);

        let quickest = (1..=5)
            .map(|round| {
                let second = FIRST_SECOND + CHARTED_SECONDS - 1 + round;
                worker_round(&mut store, &handle, &mut worker, Some(second))
            })
            .min()
            .unwrap();

        assert!(quickest < crate::live_source::POLL_INTERVAL / 2);
    }

    #[test]
    fn a_second_the_store_takes_wakes_the_page_once() {
        // Goal: in the steady state, the reading of one second the Store takes wakes the page
        // once, so the page takes it as it lands and is woken for nothing else.
        // Method: run the real worker over a store the test serves to its first reading, have
        // the worker wake a page that counts, take one second in and serve, and count the wakes
        // once the worker has put its next question out, which it does after the wake.
        let mut store = charted();
        let handle = store.handle();
        let mut worker =
            Worker::start_from_store(std::path::PathBuf::from(NO_RECORDING), store.handle())
                .unwrap();
        worker.request(request("network")).unwrap();
        worker_round(&mut store, &handle, &mut worker, None);
        let woken = Arc::new(AtomicU32::new(0));
        let counted = Arc::clone(&woken);
        worker.wake_page_with(Box::new(move || {
            counted.fetch_add(1, Ordering::AcqRel);
        }));

        worker_round(
            &mut store,
            &handle,
            &mut worker,
            Some(FIRST_SECOND + CHARTED_SECONDS),
        );
        worker_round_asked(&handle, Instant::now() + Duration::from_secs(10));

        assert_eq!(woken.load(Ordering::Acquire), 1);
    }

    /// Takes `second` in where there is one, waits for the worker's question, serves, and returns
    /// how long the worker then took to publish a reading of the newest second the store holds.
    fn worker_round(
        store: &mut Charted,
        handle: &viewer::store::QueryHandle,
        worker: &mut Worker,
        second: Option<u64>,
    ) -> Duration {
        let deadline = Instant::now() + Duration::from_secs(10);
        if let Some(second) = second {
            store.take(second);
        }
        let newest = store.followed().selected().observation.stamp.publication;
        worker_round_asked(handle, deadline);
        let started = Instant::now();
        store.serve();
        for _attempt in 0..10_000 {
            if let Some(Update::Ready(snapshot)) = worker.take().unwrap() {
                if snapshot.selection["selected"]["publication"] == newest {
                    return started.elapsed();
                }
            }
            assert!(Instant::now() < deadline, "the worker published no reading");
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("the worker published no reading within its attempts");
    }

    /// Waits, until `deadline`, for the worker to put a question to the store of `handle`.
    fn worker_round_asked(handle: &viewer::store::QueryHandle, deadline: Instant) {
        for _attempt in 0..10_000 {
            if handle.pending() {
                return;
            }
            assert!(Instant::now() < deadline, "the worker asked nothing");
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("the worker asked nothing within its attempts");
    }

    #[test]
    fn a_store_that_takes_nothing_gives_no_answer_for_half_the_bound() {
        // Goal: a page that shows what the Store holds costs the Store no answer while it holds
        // nothing new, where today it costs about two answers a second.
        // Method: bring a reader to the steady state, then step it and serve one step short of
        // half the bound with no second taken, and count the Store's answers.
        let store = charted();
        let (mut reader, mut command, answers) = steady(&store);

        for _ in 1..STORE_WAITS_KEYED_MAX {
            steady_step(&store, &mut reader, &mut command);
        }

        assert_eq!(answers.load(Ordering::Acquire), 0);
    }

    #[test]
    fn a_store_that_takes_nothing_answers_one_plain_question_at_half_the_bound() {
        // Goal: half the bound into a wait, the reader asks a plain question, which a Store that
        // serves answers at once, so the page learns the Store still serves.
        // Method: bring a reader to the steady state, then step it and serve half the bound with
        // no second taken, and count the Store's answers.
        let store = charted();
        let (mut reader, mut command, answers) = steady(&store);

        for _ in 0..STORE_WAITS_KEYED_MAX {
            steady_step(&store, &mut reader, &mut command);
        }

        assert_eq!(answers.load(Ordering::Acquire), 1);
    }

    #[test]
    fn a_store_that_takes_nothing_keeps_the_page_on_its_reading() {
        // Goal: a Store that serves and holds nothing new never sends the page to the saved
        // minutes, since the plain question's answer resets the bound.
        // Method: bring a reader to the steady state, step it and serve twice the bound with no
        // second taken, and count the updates the steps publish.
        let store = charted();
        let (mut reader, mut command, _) = steady(&store);

        let updates = (0..2 * STORE_WAITS_MAX)
            .filter_map(|_| steady_step(&store, &mut reader, &mut command))
            .count();

        assert_eq!(updates, 0);
    }

    #[test]
    fn a_repeat_answer_brings_no_further_answer_until_the_plain_question() {
        // Goal: a View with no new second answers the follow question once, with what the page
        // already shows, and the next question names that answer, so the Store keeps it.
        // Method: bring a reader to the steady state, put its question out, publish a View and
        // serve, then step it and serve until just before the plain question, and count the
        // Store's answers.
        let mut store = charted();
        let (mut reader, mut command, answers) = steady(&store);
        steady_step(&store, &mut reader, &mut command);
        store.publish();
        store.serve();

        for _ in 1..STORE_WAITS_KEYED_MAX {
            steady_step(&store, &mut reader, &mut command);
        }

        assert_eq!(answers.load(Ordering::Acquire), 1);
    }

    #[test]
    fn a_store_that_stops_serving_sends_the_page_to_the_saved_minutes_at_the_bound() {
        // Goal: the keyed question leaves the bound as it was, so a Store that answers nothing
        // sends the page to the saved minutes after ten seconds and no sooner.
        // Method: bring a reader to the steady state, step it the bound with no serve, then step
        // it once more.
        let store = charted();
        let (mut reader, mut command, _) = steady(&store);
        let waited = (0..STORE_WAITS_MAX)
            .filter_map(|_| reader_step(&mut reader, &mut command))
            .count();
        assert_eq!(waited, 0);

        let update = reader_step(&mut reader, &mut command);

        assert!(reader_step_failed_saved(update));
    }

    #[test]
    fn a_page_opened_next_is_answered_at_the_stores_next_serve() {
        // Goal: a page that has not yet shown a reading of the Store asks with no key, so it opens
        // at the Store's next serve, also where the Store takes no second.
        // Method: bring a reader to the steady state, have the window ask for another page, step
        // and serve once with no second taken, and count the Store's answers.
        let store = charted();
        let (mut reader, mut command, answers) = steady(&store);
        command.generation = 2;
        command.request = request("memory");

        steady_step(&store, &mut reader, &mut command);

        assert_eq!(answers.load(Ordering::Acquire), 1);
    }

    #[test]
    fn a_page_told_the_store_needs_a_load_asks_its_next_question_with_no_key() {
        // Goal: after an answer that is not a reading, the page asks with no key, so the Store
        // answers at its next serve rather than holding the question on the last reading's key.
        // Method: bring a reader to the steady state, give it the Store's answer that its ring is
        // incomplete, then step it and serve once with no second taken, and count the answers.
        let store = charted();
        let (mut reader, mut command, answers) = steady(&store);
        reader
            .store
            .as_mut()
            .unwrap()
            .needs_load(viewer::store::QueryNeedsLoad::IncompleteRing);

        steady_step(&store, &mut reader, &mut command);

        assert_eq!(answers.load(Ordering::Acquire), 1);
    }

    #[test]
    fn a_page_told_the_store_holds_too_many_contexts_asks_its_next_question_with_no_key() {
        // Goal: after the Store's limit on contexts refuses an answer, the page asks with no key,
        // so the Store answers at its next serve rather than holding the question.
        // Method: bring a reader to the steady state, give it the Store's refusal at its limit on
        // contexts, then step it and serve once with no second taken, and count the answers.
        let store = charted();
        let (mut reader, mut command, answers) = steady(&store);
        reader
            .store
            .as_mut()
            .unwrap()
            .unavailable(viewer::store::QueryUnavailable::ContextLimit)
            .unwrap();

        steady_step(&store, &mut reader, &mut command);

        assert_eq!(answers.load(Ordering::Acquire), 1);
    }

    #[test]
    fn a_plain_question_at_half_the_bound_leaves_the_pause_to_run() {
        // Goal: the plain question a page asks half the bound into a wait is looked for at the
        // worker's pace, so a Store that answers it at once costs the page no extra step.
        // Method: bring a reader to the steady state, step it half the bound with no serve, which
        // puts the plain question out, and ask whether an answer would end the worker's pause.
        let store = charted();
        let (mut reader, mut command, _) = steady(&store);

        for _ in 0..STORE_WAITS_KEYED_MAX {
            reader_step(&mut reader, &mut command);
        }

        assert!(reader.store.as_ref().unwrap().answer_awaited().is_none());
    }

    #[test]
    fn a_repeat_answer_puts_the_next_keyed_question_out_in_its_step() {
        // Goal: the step that takes a repeat answer leaves its next question with the Store, so
        // the next second the Store takes is answered in that Store's turn, with no step between.
        // Method: bring a reader to the steady state, have the Store publish a View and answer
        // with a repeat, take that answer in one step, then take a second in and serve with no
        // further step, and count the Store's answers.
        let mut store = charted();
        let (mut reader, mut command, answers) = steady(&store);
        steady_step(&store, &mut reader, &mut command);
        store.publish();
        store.serve();
        assert!(reader_step(&mut reader, &mut command).is_none());

        store.take(FIRST_SECOND + CHARTED_SECONDS);
        store.serve();

        assert_eq!(answers.load(Ordering::Acquire), 2);
    }

    #[test]
    fn a_page_whose_reading_failed_asks_again_at_the_stores_next_serve() {
        // Goal: an answer whose reading the page could not build leaves the page on the key of
        // the reading it shows, so its next question is answered at the Store's next serve rather
        // than held until the plain question.
        // Method: bring a reader to the steady state, take a second in, and have the step that
        // takes its answer fail to build the reading, by asking it for a page that ranks Apps,
        // which the store's seconds lack. Then step the page again and serve with no second
        // taken, and count the Store's answers.
        let mut store = charted();
        let (mut reader, mut command, answers) = steady(&store);
        store.take(FIRST_SECOND + CHARTED_SECONDS);
        steady_step(&store, &mut reader, &mut command);
        command.request = request("memory");
        let failed = reader_step(&mut reader, &mut command);
        assert!(matches!(failed, Some(Update::Failed { .. })));
        command.request = request("network");
        reader.retry_after = Instant::now();

        steady_step(&store, &mut reader, &mut command);

        assert_eq!(answers.load(Ordering::Acquire), 2);
    }

    /// A worker's reader that shows the Network page's reading of the newest second `store`
    /// holds, which it took from the Store, the command of that page, and the count of the
    /// answers the Store gives from then on.
    fn steady(store: &Charted) -> (Reader, Command, Arc<AtomicU32>) {
        let mut reader = reader(store);
        let mut command = Command {
            generation: 1,
            epoch: 0,
            request: request("network"),
            context: Context::Select,
        };
        assert!(steady_step(store, &mut reader, &mut command).is_none());
        let shown = reader_step(&mut reader, &mut command);
        assert!(matches!(shown, Some(Update::Ready(_))));
        let answers = Arc::new(AtomicU32::new(0));
        store.handle().on_reply({
            let answers = Arc::clone(&answers);
            move || {
                answers.fetch_add(1, Ordering::AcqRel);
            }
        });

        (reader, command, answers)
    }

    /// One step of the worker, then one turn of the Store, which serves what the step asked.
    fn steady_step(store: &Charted, reader: &mut Reader, command: &mut Command) -> Option<Update> {
        let update = reader_step(reader, command);
        store.serve();

        update
    }

    /// The real worker over a store the test serves, run to its first reading of `route`. It
    /// waits for any reading rather than through `worker_round`, which waits for a reading of
    /// the selected second that the Self page's reading does not name.
    fn worker_showing(route: &str) -> (Charted, Worker) {
        let store = charted();
        let handle = store.handle();
        let mut worker =
            Worker::start_from_store(std::path::PathBuf::from(NO_RECORDING), store.handle())
                .unwrap();
        worker.request(request(route)).unwrap();
        worker_round_asked(&handle, Instant::now() + Duration::from_secs(10));
        store.serve();
        for _attempt in 0..10_000 {
            if let Some(Update::Ready(_)) = worker.take().unwrap() {
                return (store, worker);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
        panic!("the worker published no reading");
    }

    /// Whether the worker comes, within ten seconds, to say `on_its_way` of whether a reading is
    /// on its way.
    fn worker_comes_to_say(worker: &Worker, on_its_way: bool) -> bool {
        for _attempt in 0..10_000 {
            if worker.reading_on_its_way() == on_its_way {
                return true;
            }
            std::thread::sleep(Duration::from_millis(1));
        }

        false
    }

    /// The count of the worker's wakes of the page from now on.
    fn worker_wakes_counted(worker: &Worker) -> Arc<AtomicU32> {
        let wakes = Arc::new(AtomicU32::new(0));
        worker.wake_page_with({
            let wakes = Arc::clone(&wakes);
            Box::new(move || {
                wakes.fetch_add(1, Ordering::AcqRel);
            })
        });

        wakes
    }

    mod when_the_reader_says_whether_a_reading_is_on_its_way {
        use super::*;
        use crate::live_source::measurement::Stage;

        #[test]
        fn a_keyed_question_out_says_a_reading_is_on_its_way() {
            // Goal: the worker says a reading is on its way while its keyed question is out for a
            // page that shows a reading, so the store's wake leaves the View to that reading.
            // Method: run the real worker to its first reading over a store the test serves, let
            // it put its keyed question out, and read what it says.
            let (_store, worker) = worker_showing("network");

            assert!(worker_comes_to_say(&worker, true));
        }

        #[test]
        fn the_self_page_has_no_reading_on_its_way() {
            // Goal: the Self page shows no reading, so the store's wake takes its View at once,
            // although the page asks keyed questions. Method: run the real worker to the Self
            // page's first reading, hold it at the start of a step once its next question is out,
            // and read what it said after the step before.
            let (store, worker) = worker_showing("self");
            worker_round_asked(&store.handle(), Instant::now() + Duration::from_secs(10));

            assert!(!worker_said(&worker));
        }

        #[test]
        fn a_rested_worker_has_no_reading_on_its_way() {
            // Goal: a worker no window reads sends the page no reading. Method: bring the real
            // worker to its keyed question, hand it over as a closed window does, and read what
            // it says.
            let (_store, mut worker) = worker_showing("network");
            assert!(worker_comes_to_say(&worker, true));

            let rested = worker.hand_over().unwrap().unwrap();

            assert!(worker_comes_to_say(&rested, false));
        }

        #[test]
        fn a_request_that_cancels_the_keyed_question_leaves_no_reading_on_its_way() {
            // Goal: a new request cancels the keyed question of the page before it, and the new
            // page shows nothing until its first reading. Method: bring the real worker to its
            // keyed question, ask it for another page, and read what it says while the store
            // answers nothing.
            let (_store, mut worker) = worker_showing("network");
            assert!(worker_comes_to_say(&worker, true));

            worker.request(request("memory")).unwrap();

            assert!(worker_comes_to_say(&worker, false));
        }

        #[test]
        fn a_plain_question_at_half_the_bound_has_no_reading_on_its_way() {
            // Goal: the plain question half the bound into a wait is answered at the Store's next
            // serve whatever it holds, so no reading of a new second is on its way. Method: bring
            // a reader to the steady state, step it half the bound with no serve, and ask it. The
            // reader is stepped by hand, since the real worker takes five seconds to get there.
            let store = charted();
            let (mut reader, mut command, _) = steady(&store);

            for _ in 0..STORE_WAITS_KEYED_MAX {
                reader_step(&mut reader, &mut command);
            }

            let store_reader = reader.store.as_ref().unwrap();
            assert!(!store_reader.reading_on_its_way(&command));
        }

        #[test]
        fn a_stopped_worker_has_no_reading_on_its_way() {
            // Goal: a worker that stopped sends nothing, so the store's wake takes its View at
            // once. Method: bring the real worker to its keyed question, stop it, and read what it
            // says.
            let (_store, mut worker) = worker_showing("network");
            assert!(worker_comes_to_say(&worker, true));

            worker.stop();

            assert!(!worker.reading_on_its_way());
        }

        /// What the worker said after its last step, read while it waits at the start of its next.
        fn worker_said(worker: &Worker) -> bool {
            let hold = worker.hold_measurement_stage(Stage::Reading);
            hold.entered();

            worker.reading_on_its_way()
        }
    }

    mod when_a_step_publishes_no_reading {
        use super::*;
        use crate::live_source::measurement::Stage;

        #[test]
        fn a_keyed_answer_dropped_as_a_repeat_wakes_the_page_once() {
            // Goal: the View that comes with a keyed answer the reader drops as a repeat is drawn
            // within the worker's step, not at the timer, since the store's wake left it to the
            // reading that never comes. Method: bring the real worker to its keyed question and
            // hold it at the start of a step, count its wakes of the page, have the store publish
            // a View with no new second and answer, then let that step run, and count the wakes
            // and look for an update once the worker reaches the start of the next.
            let (mut store, mut worker) = worker_showing("network");
            assert!(worker_comes_to_say(&worker, true));
            let held = worker.hold_measurement_stage(Stage::Reading);
            held.entered();
            let wakes = worker_wakes_counted(&worker);
            store.publish();
            store.serve();

            held.release();
            drop(held);
            let next = worker.hold_measurement_stage(Stage::Reading);
            next.entered();

            assert_eq!(wakes.load(Ordering::Acquire), 1);
            assert!(matches!(worker.take(), Ok(None)));
        }

        #[test]
        fn a_keyed_answer_the_store_refuses_wakes_the_page_once() {
            // Goal: a keyed answer that brings no reading, as the Store's refusal at its limit on
            // contexts, still wakes the page, so the View the store's wake left to the reading is
            // drawn within the step. Method: bring a reader to the steady state and keep its
            // context, so the page holds two once it takes the next second's. Put its keyed
            // question out, take a second in and serve, which refuses a third context. Then run
            // the worker's step by hand on a page told a reading is on its way, and count the
            // wakes and look for an update. The reader is stepped by hand, since the store the
            // test serves gives no answer that the Store needs a load.
            let mut store = charted();
            let (mut reader, mut command, _) = steady(&store);
            let kept = reader.store.as_ref().unwrap().context.clone();
            assert!(kept.is_some());
            reader_step(&mut reader, &mut command);
            store.take(FIRST_SECOND + CHARTED_SECONDS);
            store.serve();
            assert!(matches!(
                reader_step(&mut reader, &mut command),
                Some(Update::Ready(_))
            ));
            reader_step(&mut reader, &mut command);
            store.take(FIRST_SECOND + CHARTED_SECONDS + 1);
            store.serve();
            let (shared, wakes) = worker_shared_told(&command);
            let mut command = Some(command);

            worker_step_by_hand(&shared, &mut reader, &mut command);

            assert_eq!(wakes.load(Ordering::Acquire), 1);
            assert!(shared.mailbox.lock().unwrap().output.is_none());
        }

        #[test]
        fn a_plain_questions_repeat_wakes_nothing() {
            // Goal: the page leaves a View to a reading only while a keyed question is out, so an
            // answer to a plain question that repeats what the page shows wakes nothing. Method:
            // bring a reader to the steady state and step it half the bound with no serve, so its
            // plain question is out, then have the store publish a View with no new second and
            // answer, run the worker's step by hand, and count the wakes. The reader is stepped
            // by hand, since the real worker takes five seconds to ask the plain question.
            let mut store = charted();
            let (mut reader, command, _) = steady(&store);
            let (shared, wakes) = worker_shared_told(&command);
            let mut command = Some(command);
            for _ in 0..STORE_WAITS_KEYED_MAX {
                worker_step_by_hand(&shared, &mut reader, &mut command);
            }
            assert!(reader.store.as_ref().unwrap().keyed_question().is_none());
            wakes.store(0, Ordering::Release);
            store.publish();
            store.serve();

            worker_step_by_hand(&shared, &mut reader, &mut command);

            assert_eq!(wakes.load(Ordering::Acquire), 0);
            assert!(shared.mailbox.lock().unwrap().output.is_none());
        }

        #[test]
        fn a_request_that_cancels_the_keyed_question_wakes_the_page_once() {
            // Goal: a new request cancels the keyed question the page left its View to, so the
            // step that cancels it wakes the page, which takes that View at once. Method: bring
            // the real worker to its keyed question, count its wakes of the page, ask it for
            // another page while the store answers nothing, and count the wakes and look for an
            // update once the worker says no reading is on its way and reaches its next step.
            let (_store, mut worker) = worker_showing("network");
            assert!(worker_comes_to_say(&worker, true));
            let wakes = worker_wakes_counted(&worker);

            worker.request(request("memory")).unwrap();

            assert!(worker_comes_to_say(&worker, false));
            let next = worker.hold_measurement_stage(Stage::Reading);
            next.entered();
            assert_eq!(wakes.load(Ordering::Acquire), 1);
            assert!(matches!(worker.take(), Ok(None)));
        }

        #[test]
        fn a_step_that_still_waits_for_its_keyed_answer_wakes_nothing() {
            // Goal: the page is woken only once its keyed question ends, so the worker's steps
            // that wait for a second cost the page no tick. Method: bring a reader to the steady
            // state, run the worker's step by hand to put its keyed question out, then run more
            // steps with no serve, short of the plain question half the bound brings, and count
            // the wakes. The reader is stepped by hand, so that the test controls when the store
            // answers.
            let store = charted();
            let (mut reader, command, _) = steady(&store);
            let (shared, wakes) = worker_shared_told(&command);
            let mut command = Some(command);
            worker_step_by_hand(&shared, &mut reader, &mut command);
            assert!(shared.reading_on_its_way.load(Ordering::Acquire));
            let asked = reader.store.as_ref().unwrap().keyed_question();
            wakes.store(0, Ordering::Release);

            for _ in 2..STORE_WAITS_KEYED_MAX {
                worker_step_by_hand(&shared, &mut reader, &mut command);
            }

            assert_eq!(reader.store.as_ref().unwrap().keyed_question(), asked);
            assert_eq!(wakes.load(Ordering::Acquire), 0);
        }

        /// The worker's shared state for the page of `command`, told a reading is on its way, and
        /// the count of its wakes of the page.
        fn worker_shared_told(command: &Command) -> (Shared, Arc<AtomicU32>) {
            let shared = Shared::new();
            shared.mailbox.lock().unwrap().generation = command.generation;
            shared.reading_on_its_way.store(true, Ordering::Release);
            let wakes = Arc::new(AtomicU32::new(0));
            shared.wake_page_with({
                let wakes = Arc::clone(&wakes);
                Box::new(move || {
                    wakes.fetch_add(1, Ordering::AcqRel);
                })
            });

            (shared, wakes)
        }

        /// One step of the worker over `reader`, after which it says, as the worker does, whether
        /// a reading is on its way.
        fn worker_step_by_hand(
            shared: &Shared,
            reader: &mut Reader,
            command: &mut Option<Command>,
        ) {
            crate::live_source::worker_step_read(
                std::path::Path::new(NO_RECORDING),
                shared,
                reader,
                command.as_mut().unwrap(),
            );
            let on_its_way =
                crate::live_source::worker_lease_reading_on_its_way(reader, command.as_ref());
            shared
                .reading_on_its_way
                .store(on_its_way, Ordering::Release);
        }
    }
}
