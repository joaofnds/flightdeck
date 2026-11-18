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
    let identity = primary.selection["trends"]
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
    let older = shown.selection["trends"]
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
    assert_eq!(snapshot.selection.get("trends"), None);
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
                    crate::live_rankings::observation_key(&snapshot.selection["trends"][0]);

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
