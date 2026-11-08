use super::*;

const HISTORY_PROCESSES: u32 = 16_384;
const INITIAL_ROOM_BYTES: usize = 52 * 1024 * 1024;

#[test]
fn a_primary_worker_keeps_serving_while_history_admission_is_denied() {
    // Goal: real history backpressure keeps Primary usable without a Worker checkpoint.
    // Method: deny a larger saved process input, change route, then release its byte lease.
    let (mut harness, budget, lease) = Harness::denied_history();
    let mut selected = request("memory");
    selected.load_resident_history = true;
    let generation = harness.worker.request(selected).unwrap();

    let primary = harness.ready_primary(&budget);

    assert_eq!(primary.generation, generation);
    assert!(primary.histories_pending);
    assert!(!harness.worker.last_output_complete());
    assert_eq!(primary.selection["selected"]["publication"], 124);
    assert_eq!(primary.selection["summary"]["cpu_percent"], 50.0);
    assert!(primary.resident_history.is_none());
    denied_history_check(&harness.recording.as_ref().unwrap().root, &budget);
    assert!(
        harness
            .worker
            .shared
            .measurement_gate
            .lock()
            .unwrap()
            .is_none()
    );
    assert_eq!(harness.ordinary_app_work(), 0);
    let source = primary.catalog["source_generation"].clone();
    let identity = primary.selection["selected"].clone();
    drop(primary);
    let generation = harness.worker.request(request("cpu")).unwrap();
    let pending = harness.ready_primary(&budget);
    assert_eq!(pending.generation, generation);
    assert!(pending.histories_pending);
    assert_eq!(pending.catalog["source_generation"], source);
    assert_eq!(pending.selection["selected"], identity);
    assert_eq!(
        pending.cpu_detail.as_ref().unwrap().cpus,
        [crate::live_cpu::LogicalCPU {
            cpu: 0,
            busy_percent: Some(50.0),
        }]
    );
    assert_eq!(pending.cpu_history.as_ref().unwrap().observation_count(), 9);
    assert_eq!(harness.ordinary_app_work(), 0);
    drop(pending);
    drop(lease);
    let complete = harness.ready();

    assert_eq!(complete.generation, generation);
    assert!(!complete.histories_pending);
    assert!(harness.worker.last_output_complete());
    assert_eq!(complete.catalog["source_generation"], source);
    assert_eq!(complete.selection["selected"], identity);
    assert!(harness.ordinary_app_work() > 0);
    harness.assert_source_free();
    drop((complete, harness));
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_cancelled_kept_primary_refresh_cannot_replace_a_new_selection() {
    // Goal: an inspection awaiting Complete loses its reply when its operation is cancelled.
    // Method: submit a real kept query during history denial, cancel, then complete a selection.
    let (harness, budget, lease) = Harness::denied_history();
    let handle = harness.store.as_ref().unwrap().query_handle();
    let mut adapter = Adapter::new(handle.clone());
    let primary = adapter.ready_for(viewer::store::QueryReadiness::Primary);
    let source = primary.catalog["source_generation"].clone();
    let selected = primary.selection["selected"].clone();
    let mut inspection = request("memory");
    inspection.second = Some(1000);
    inspection.identity = Some(primary.selection["trends"][0].clone());
    adapter.submit(inspection, crate::live_source::Context::Keep);

    assert!(adapter.read().is_none());
    assert!(handle.pending());
    adapter.shared.cancellation.cancel().unwrap();
    assert!(adapter.read().is_none());
    assert!(handle.take().is_none());
    adapter.submit(request("network"), crate::live_source::Context::Select);
    drop(primary);
    drop(lease);
    let complete = adapter.ready_for(viewer::store::QueryReadiness::Complete);

    assert_eq!(complete.generation, 3);
    assert_eq!(complete.catalog["source_generation"], source);
    assert_eq!(complete.selection["selected"], selected);
    assert_eq!(complete.selection["selected"]["publication"], 124);
    assert_eq!(complete.selection["summary"]["cpu_percent"], 50.0);
    harness.assert_source_free();
    drop((complete, adapter, handle, harness));
    assert_eq!(budget.used(), 0);
}

#[test]
fn a_store_missing_its_ring_leaves_the_page_to_the_saved_minutes() {
    // Goal: a page never waits for ever on a Store that cannot prove what it holds.
    // Method: register a ring that does not exist beside a saved minute, and step the reader.
    let mut recording = LiveRecording::new();
    recording.publish_resources();
    recording.publish_resources();
    seal_recording(&recording, 0);
    let mut registry = collector::recording::Registry::read(&recording.root).unwrap();
    registry.runs[0].ring_name = String::from("/fdnoring");
    registry.write(&recording.root).unwrap();
    let store = viewer::store::Store::start_with_budget(
        &recording.root,
        Arc::new(viewer::store::ByteBudget::new(128 * 1024 * 1024)),
    )
    .unwrap();
    let mut adapter = Adapter::on_root(store.query_handle(), &recording.root);

    let update = adapter.first_update();

    let Update::Ready(snapshot) = update else {
        panic!("the saved minutes did not answer");
    };
    assert_eq!(snapshot.selection["selected"]["publication"], 2);
}

#[test]
fn a_recording_that_never_appears_says_so() {
    // Goal: a page never waits for ever on a Store that has no recording to read.
    // Method: start the Store on a root nothing makes, and step the reader.
    let directory =
        std::env::temp_dir().join(format!("flightdeck-no-recording-{}", std::process::id()));
    let store = viewer::store::Store::start_with_budget(
        &directory,
        Arc::new(viewer::store::ByteBudget::new(128 * 1024 * 1024)),
    )
    .unwrap();
    let mut adapter = Adapter::on_root(store.query_handle(), &directory);

    let update = adapter.first_update();

    assert!(matches!(update, Update::Failed { .. }));
}

#[test]
fn a_page_the_saved_minutes_answered_reads_them_again_a_second_later() {
    // Goal: a Store that stays behind costs the page one listing of the recording a second, and the
    // minute saved meanwhile still reaches it.
    // Method: read past the wait with the Store parked behind the ring, save a minute, and time the
    // next update from before the first.
    let (mut recording, _store, handle) = store_parked_behind_the_ring();
    let mut adapter = Adapter::on_root(handle, &recording.root);
    adapter.submit(request("network"), crate::live_source::Context::Select);
    let started = Instant::now();
    let Update::Ready(first) = adapter.first_update() else {
        panic!("the saved minutes did not answer");
    };
    assert_eq!(first.selection["selected"]["publication"], 2);
    for _ in 0..62 {
        recording.publish_resources();
    }
    seal_recording(&recording, 1);

    let next = adapter.update_within(Duration::from_secs(10));

    let Update::Ready(next) = next else {
        panic!("the saved minutes did not answer again");
    };
    assert!(started.elapsed() >= crate::live_source::RETRY_INTERVAL);
    assert_eq!(next.selection["selected"]["publication"], 65);
}

#[test]
fn a_store_that_catches_up_takes_the_page_back_from_the_saved_minutes() {
    // Goal: the saved minutes stand in for a Store that stays behind only until it catches up.
    // Method: read past the wait with the Store parked behind the ring, then let it go on.
    let (recording, store, handle) = store_parked_behind_the_ring();
    let mut adapter = Adapter::on_root(handle, &recording.root);
    adapter.submit(request("network"), crate::live_source::Context::Select);
    let Update::Ready(saved) = adapter.first_update() else {
        panic!("the saved minutes did not answer");
    };
    assert_eq!(saved.selection["selected"]["publication"], 2);

    store.set_active(true);
    let next = adapter.update_within(Duration::from_secs(10));

    let Update::Ready(live) = next else {
        panic!("the Store did not answer");
    };
    assert_eq!(live.selection["selected"]["publication"], 3);
}

#[test]
fn a_page_the_saved_minutes_answered_in_part_asks_again_at_the_workers_pace() {
    // Goal: the wait of a second between saved answers never delays the rest of a page's own read.
    // Method: read a page whose histories come a step later past the wait, with the Store parked
    // behind the ring, and look at when the reader may step again.
    let (recording, _store, handle) = store_parked_behind_the_ring();
    let mut adapter = Adapter::on_root(handle, &recording.root);
    let mut request = request("memory");
    request.load_resident_history = true;
    adapter.submit(request, crate::live_source::Context::Select);

    let Update::Ready(first) = adapter.first_update() else {
        panic!("the saved minutes did not answer");
    };

    assert!(first.histories_pending);
    assert!(adapter.reader.retry_after <= Instant::now());
}

#[test]
fn a_second_whose_apps_are_unread_holds_the_page_until_the_store_reads_them() {
    // Goal: a reading the Store is about to complete shows from the Store, not the saved minutes.
    // Method: ask for an older second of a Primary while history admission is denied, stepping
    // short of the bounded wait on a root with no recording, where a saved read fails, then let
    // the Store read on.
    let (harness, _budget, lease) = Harness::denied_history();
    let mut adapter = Adapter::new(harness.store.as_ref().unwrap().query_handle());
    let primary = adapter.ready_for(viewer::store::QueryReadiness::Primary);
    let mut older = request("memory");
    older.second = Some(1000);
    adapter.submit(older, crate::live_source::Context::Select);

    let steps_short_of_the_wait = super::super::STORE_WAITS_MAX - 10;
    for _ in 0..steps_short_of_the_wait {
        assert!(adapter.read().is_none());
        std::thread::sleep(Duration::from_millis(10));
    }
    drop((primary, lease));
    let complete = adapter.ready_for(viewer::store::QueryReadiness::Complete);

    assert_eq!(complete.selection["selected"]["publication"], 1);
}

#[test]
fn an_incomplete_ring_holds_the_page_for_the_wait() {
    // Goal: a ring the Store is about to note as whole costs a wait, not a saved read.
    // Method: answer the reader directly, since a real Store answers this only in the moment
    // between reading the ring and noting that it did.
    let (_recording, _store, handle) = store_parked_behind_the_ring();
    let mut reader = super::super::StoreReader::new(handle);

    let answer = reader.needs_load(viewer::store::QueryNeedsLoad::IncompleteRing);

    assert!(matches!(answer, super::super::Answer::Waiting));
}

#[test]
fn a_second_whose_detail_the_store_let_go_reads_the_saved_minutes_at_once() {
    // Goal: waiting cannot bring back what the Store let go, so the page does not wait for it.
    // Method: answer the reader directly, which leaves it no reason to step again.
    let (_recording, _store, handle) = store_parked_behind_the_ring();
    let mut reader = super::super::StoreReader::new(handle);

    let answer = reader.needs_load(viewer::store::QueryNeedsLoad::FullDetailEvicted);

    assert!(matches!(answer, super::super::Answer::Saved));
}

#[test]
fn a_page_the_store_published_lists_the_saved_minutes_again_once_it_falls_to_them() {
    // Goal: a later step keeps only a listing the saved minutes made for its page, so a page the
    // Store published finds a minute saved after the session last listed. Method: leave a page
    // the Store published with part of it still to read, and take the listing its next saved
    // step reads.
    let (_recording, _store, handle) = store_parked_behind_the_ring();
    let mut store = super::super::StoreReader::new(handle);
    store.published = 1;
    let reader = crate::live_source::Reader {
        session: None,
        store: Some(store),
        last_catalog: None,
        last_generation: 1,
        last_route: String::from("overview"),
        pending: Some(crate::live_source::Pending {
            generation: 1,
            histories: crate::live_source::Histories::Defer,
        }),
        retry_after: Instant::now(),
    };

    let listing = crate::live_source::reader_snapshot_listing(&reader, 1);

    assert_eq!(listing, crate::live_source::Listing::Fresh);
}

#[test]
fn a_second_the_store_holds_no_reading_of_reads_the_saved_minutes_at_once() {
    // Goal: waiting cannot bring a reading the Store does not hold, so the page does not wait.
    // Method: answer the reader directly, which leaves it no reason to step again.
    let (_recording, _store, handle) = store_parked_behind_the_ring();
    let mut reader = super::super::StoreReader::new(handle);

    let answer = reader.needs_load(viewer::store::QueryNeedsLoad::MissingObservation);

    assert!(matches!(answer, super::super::Answer::Saved));
}

/// A Store parked a reading behind its ring, over a saved minute, and a handle to ask it.
fn store_parked_behind_the_ring() -> (
    LiveRecording,
    viewer::store::Store,
    viewer::store::QueryHandle,
) {
    let (mut recording, store, handle) = store_over_a_sealed_minute();
    Harness::ready_context(&handle);
    park_behind_the_ring(&mut recording, &store);

    (recording, store, handle)
}

/// A Store started over one sealed minute of two readings, which it holds once it answers.
fn store_over_a_sealed_minute() -> (
    LiveRecording,
    viewer::store::Store,
    viewer::store::QueryHandle,
) {
    let mut recording = LiveRecording::new();
    recording.publish_resources();
    recording.publish_resources();
    seal_recording(&recording, 0);
    let store = viewer::store::Store::start_with_budget(
        &recording.root,
        Arc::new(viewer::store::ByteBudget::new(128 * 1024 * 1024)),
    )
    .unwrap();
    let handle = store.query_handle();

    (recording, store, handle)
}

/// Pauses the Store and publishes a reading it does not take, so that it answers not yet.
fn park_behind_the_ring(recording: &mut LiveRecording, store: &viewer::store::Store) {
    store.set_active(false);
    Harness::wait_until_paused(store);
    recording.publish_resources();
}

#[test]
fn the_stores_answer_ends_the_pause_of_a_page_waiting_for_it() {
    // Goal: a page shows as soon as the Store answers it, rather than a whole poll later.
    // Method: pause a thread for a minute on the real mailbox, see it wait, then ask the real
    // Store, and wait up to thirty seconds, which keeps the verdict independent of how busy the
    // machine is.
    let (_recording, _store, handle) = store_answering();
    let shared = Arc::new(crate::live_source::Shared::new());
    crate::live_source::worker_answer_wakes(&shared, &handle);
    let (ended, ending) = std::sync::mpsc::channel();
    let pausing = std::thread::spawn({
        let shared = Arc::clone(&shared);
        let handle = handle.clone();
        move || {
            crate::live_source::worker_pause_for(&shared, Duration::from_secs(60), Some(&handle));
            ended.send(()).unwrap();
        }
    });
    wait_until_the_pause_awaits_an_answer(&shared);

    handle.submit(latest_query()).unwrap();

    ending.recv_timeout(Duration::from_secs(30)).unwrap();
    pausing.join().unwrap();
}

#[test]
fn an_answer_that_landed_before_the_pause_ends_it_at_once() {
    // Goal: an answer landing between the step that asked and its pause costs the page no poll.
    // Method: let the real Store answer first, then pause for a minute, which would outlast the
    // limit if the pause did not look for the answer.
    let (_recording, _store, handle) = store_answering();
    let shared = crate::live_source::Shared::new();
    handle.submit(latest_query()).unwrap();
    wait_until_answered(&handle);
    let started = Instant::now();

    crate::live_source::worker_pause_for(&shared, Duration::from_secs(60), Some(&handle));

    assert!(started.elapsed() < Duration::from_secs(30));
}

#[test]
fn a_page_with_no_reading_yet_awaits_the_stores_answer() {
    // Goal: the wake applies to a page whose first reading is due.
    // Method: step the reader once, which asks the Store.
    let (_recording, _store, handle) = store_answering();
    let mut adapter = Adapter::new(handle);

    assert!(adapter.read().is_none());

    assert!(
        adapter
            .reader
            .store
            .as_ref()
            .unwrap()
            .answer_awaited()
            .is_some()
    );
}

#[test]
fn a_page_showing_its_readings_awaits_no_answer() {
    // Goal: a page that follows the Mac asks at the worker's pace, since an answer that ended the
    // pause would double the queries it costs while the Mac is followed.
    // Method: step the reader past its first reading until it asks the Store again.
    let (recording, _store, handle) = store_answering();
    let mut adapter = Adapter::on_root(handle, &recording.root);
    adapter.command.request = request("network");
    assert!(matches!(
        adapter.update_within(Duration::from_secs(10)),
        Update::Ready(_)
    ));

    assert!(adapter.read().is_none());

    assert!(adapter.reader.store.as_ref().unwrap().pending.is_some());
    assert!(
        adapter
            .reader
            .store
            .as_ref()
            .unwrap()
            .answer_awaited()
            .is_none()
    );
}

#[test]
fn a_store_that_never_answers_holds_the_page_for_every_step_of_the_wait() {
    // Goal: the wait lasts the same whether the Store says nothing or keeps saying not yet, since
    // an answer now ends the pause after the step that asked, and counting that step would halve
    // it.
    // Method: step a reader over a Store with no room to start, which never replies, and count.
    let recording = LiveRecording::new();
    let store = viewer::store::Store::start_with_budget(
        &recording.root,
        Arc::new(viewer::store::ByteBudget::new(1024)),
    )
    .unwrap();
    let mut adapter = Adapter::on_root(store.query_handle(), &recording.root);
    assert!(adapter.read().is_none());

    for _ in 0..super::super::STORE_WAITS_MAX {
        assert!(adapter.read().is_none());
    }

    assert!(adapter.read().is_some());
}

#[test]
fn a_page_showing_its_readings_waits_as_many_steps_for_a_store_that_keeps_saying_not_yet() {
    // Goal: the wait lasts as many steps for a page that shows its readings as for one waiting for
    // its first, although the Store's answer ends no pause of the first, so asking costs it a step.
    // Method: show a page from the real Store, park the Store behind the ring, so that it answers
    // every query with not yet, let it answer each step's query, and count the steps until the
    // saved minutes answer.
    let (mut recording, store, handle) = store_over_a_sealed_minute();
    let mut adapter = Adapter::on_root(handle.clone(), &recording.root);
    adapter.command.request = request("network");
    assert!(matches!(
        adapter.update_within(Duration::from_secs(10)),
        Update::Ready(_)
    ));
    park_behind_the_ring(&mut recording, &store);

    for _ in 0..super::super::STORE_WAITS_MAX {
        assert!(adapter_step_answered(&mut adapter, &handle).is_none());
    }

    let Some(Update::Ready(saved)) = adapter.read() else {
        panic!("the saved minutes did not answer");
    };
    assert_eq!(saved.selection["selected"]["publication"], 2);
}

/// Steps the reader, then lets the Store answer what that step asked, as the worker's pause would.
fn adapter_step_answered(
    adapter: &mut Adapter,
    handle: &viewer::store::QueryHandle,
) -> Option<Update> {
    let update = adapter.read();
    if adapter.reader.store.as_ref().unwrap().pending.is_some() {
        wait_until_answered(handle);
    }

    update
}

#[test]
fn a_page_asking_for_its_first_reading_ends_the_stores_wait_on_the_ring() {
    // Goal: a new page does not wait out the 100 ms the Store waits for the next reading.
    // Method: publish nothing, so that only the page can end that wait, and take the quickest of
    // many, since one quick answer shows the wait ended, however busy the machine is for the rest.
    let (recording, _store, handle) = store_answering();
    let mut adapter = Adapter::on_root(handle.clone(), &recording.root);

    let quickest = (0..20)
        .map(|_| {
            adapter.submit(request("network"), crate::live_source::Context::Select);
            adapter_time_to_answer(&mut adapter, &handle)
        })
        .min()
        .unwrap();

    assert!(quickest < WAIT_ON_THE_RING_HALF);
}

#[test]
fn a_page_following_the_mac_leaves_the_store_to_its_wait_on_the_ring() {
    // Goal: a followed page's refreshes do not wake the Store, since that wake reaches every reader
    // of the ring, the persister too, at the rate the page asks.
    // Method: publish nothing, and take the slowest of a few refreshes, since one slow answer shows
    // the wait ran, however a busy machine moved the rest.
    let (recording, _store, handle) = store_answering();
    let mut adapter = Adapter::on_root(handle.clone(), &recording.root);
    adapter.command.request = request("network");
    assert!(matches!(
        adapter.update_within(Duration::from_secs(10)),
        Update::Ready(_)
    ));

    let slowest = (0..5)
        .map(|_| adapter_time_to_answer(&mut adapter, &handle))
        .max()
        .unwrap();

    assert!(slowest >= WAIT_ON_THE_RING_HALF);
}

const WAIT_ON_THE_RING_HALF: Duration = Duration::from_millis(50);

/// How long the Store takes to answer what the reader's next step asks, from a moment when the
/// Store has gone back to waiting on the ring.
fn adapter_time_to_answer(adapter: &mut Adapter, handle: &viewer::store::QueryHandle) -> Duration {
    std::thread::sleep(Duration::from_millis(10));
    let started = Instant::now();
    assert!(adapter.read().is_none());
    wait_until_answered(handle);
    let answered = started.elapsed();
    adapter.read();

    answered
}

#[test]
fn an_answer_no_page_waits_for_leaves_the_pause_to_run() {
    // Goal: a page that follows the Mac asks at the worker's pace, since an answer that ended the
    // pause would double the queries it costs.
    // Method: pause for half a second with no answer awaited while the real Store answers.
    let (_recording, _store, handle) = store_answering();
    let shared = Arc::new(crate::live_source::Shared::new());
    crate::live_source::worker_answer_wakes(&shared, &handle);
    handle.submit(latest_query()).unwrap();
    let pause = Duration::from_millis(500);
    let started = Instant::now();

    crate::live_source::worker_pause_for(&shared, pause, None);

    assert!(handle.answered());
    assert!(started.elapsed() >= pause);
}

#[test]
fn a_page_opened_before_any_minute_is_saved_shows_its_app_histories_at_once() {
    // Goal: a launch over a recording that has saved no minute yet fills its App charts with its
    // first readings, since reading them replays no saved minute.
    // Method: open Overview on the Store of a ring with no saved history.
    let (recording, _store, handle) = store_answering();
    wait_until_complete(&handle);
    let mut adapter = Adapter::on_root(handle, &recording.root);
    adapter.command.request = request("overview");
    adapter.command.request.load_app_attribution_history = true;

    let Update::Ready(opened) = adapter.update_within(Duration::from_secs(10)) else {
        panic!("the page did not open");
    };

    assert!(!opened.histories_pending);
    assert!(crate::live_source::tests::overview_ready(&opened));
}

#[test]
fn a_resting_reader_selects_a_second_published_since_its_last_read() {
    // Goal: Pause can select a reading already shown by the Store while the page reader rests.
    // Method: rest the reader, publish another second, then ask for it.
    let (mut recording, _store, handle) = store_answering();
    wait_until_complete(&handle);
    let mut adapter = Adapter::on_root(handle, &recording.root);
    adapter.command.request = request("network");
    adapter.update_within(Duration::from_secs(10));
    adapter.reader.store.as_mut().unwrap().rest();
    recording.publish_resources();
    let mut held = request("network");
    held.second = Some(recording.last_second());
    adapter.submit(held, crate::live_source::Context::Select);

    let selected = adapter.finished_within(Duration::from_secs(10));

    assert_eq!(selected.selection["second"], recording.last_second());
    assert_eq!(selected.selection["selected"]["publication"], 3);
    assert!(!selected.selection["summary"].is_null());
}

#[test]
fn restoring_a_pin_finishes_its_histories_on_its_own_second() {
    // Goal: a restored pin stays on its second with its histories read, whatever arrived since.
    // Method: read a page, publish another reading, then restore the first page's second.
    let (mut recording, _store, handle) = store_answering();
    wait_until_complete(&handle);
    let mut adapter = Adapter::on_root(handle, &recording.root);
    adapter.command.request = request("memory");
    let initial = adapter.finished_within(Duration::from_secs(10));
    let second = initial.selection["second"].as_u64().unwrap();
    recording.publish_resources();
    let mut pinned = request("memory");
    pinned.second = Some(second);
    adapter.submit(pinned, crate::live_source::Context::Select);

    let restored = adapter.finished_within(Duration::from_secs(10));

    assert_eq!(restored.selection["second"], second);
    assert_ne!(second, recording.last_second());
}

#[test]
fn live_network_ingestion_runs_the_worker_without_staging_queries_or_string_tables() {
    // Goal: the worker's ordinary read of a followed page has no eager table work.
    // Method: step the reader on this thread over a Store that follows a ring, reading Network.
    let (recording, _store, handle) = store_answering();
    wait_until_complete(&handle);
    let mut adapter = Adapter::on_root(handle, &recording.root);
    adapter.command.request = request("network");
    let measured = persistence::work::observe();

    let snapshot = adapter.finished_within(Duration::from_secs(10));

    assert_eq!(snapshot.selection["selected"]["publication"], 2);
    assert_eq!(snapshot.selection["summary"]["cpu_percent"], 50.0);
    assert_eq!(snapshot.selection["summary"]["network_rx_bps"], 1.0);
    assert_eq!(measured.counts(), persistence::work::Counts::default());
}

#[test]
fn live_overview_formats_no_cell() {
    // Goal: Overview reads its fan speed from the reading, without formatting a raw page.
    // Method: step the reader on this thread over a Store that follows one measured fan row.
    let (recording, _store, handle) = store_answering();
    wait_until_complete(&handle);
    let mut adapter = Adapter::on_root(handle, &recording.root);
    adapter.command.request = request("overview");
    let measured = persistence::work::observe();

    let snapshot = adapter.finished_within(Duration::from_secs(10));

    assert_eq!(snapshot.selection["summary"]["fan_rpm"][0], 1234.0);
    let work = measured.counts();
    assert_eq!(work.staged_ticks, 0);
    assert_eq!(work.frames, 0);
    assert_eq!(work.sql_plans, 0);
    assert_eq!(work.collections, 0);
    assert_eq!(work.string_cells, 0);
}

#[test]
fn a_page_the_saved_minutes_answer_reads_the_rest_at_once() {
    // Goal: a page reads the rest of what the saved minutes answer straight after showing its first
    // part, since it asks the Store nothing it has to wait for.
    // Method: hold a second past the Store's hour, one step at a time, and read the pause after the
    // step that showed the page, then the pause while the Store is asked again.
    let recording = crate::live_source::tests::SealedRecording::with_a_minute_past_the_store_hour();
    let store = viewer::store::Store::start_with_budget(
        &recording.root,
        Arc::new(viewer::store::ByteBudget::new(128 * 1024 * 1024)),
    )
    .unwrap();
    let handle = store.query_handle();
    wait_until_complete(&handle);
    let mut adapter = Adapter::on_root(handle, &recording.root);
    adapter.command.request = request("overview");
    adapter.command.request.second = Some(1000);
    assert!(matches!(
        adapter.update_within(Duration::from_secs(10)),
        Update::Ready(_)
    ));
    assert!(crate::live_source::reader_pending(&adapter.reader, 1));

    let after_the_first_part = crate::live_source::worker_pause(&adapter.reader, &adapter.command);
    let asked = adapter.read();
    let while_asking = crate::live_source::worker_pause(&adapter.reader, &adapter.command);
    let Update::Ready(rest) = adapter.update_within(Duration::from_secs(10)) else {
        panic!("the rest of the page did not read");
    };

    assert_eq!(after_the_first_part, Duration::ZERO);
    assert!(asked.is_none());
    assert_eq!(while_asking, crate::live_source::POLL_INTERVAL);
    assert!(!rest.histories_pending);
}

#[test]
fn a_page_showing_its_readings_asks_the_store_for_the_rest_at_the_workers_pace() {
    // Goal: a page that shows its readings and has the rest of itself to come does not spin the
    // worker on a Store with nothing new to say.
    // Method: open a page on a Store whose history reads are denied, so it gives only its first
    // readings, and read the pause.
    let (harness, _budget, _lease) = Harness::denied_history();
    let handle = harness.store.as_ref().unwrap().query_handle();
    let mut adapter = Adapter::new(handle);
    adapter.ready_for(viewer::store::QueryReadiness::Primary);
    assert!(crate::live_source::reader_pending(&adapter.reader, 1));

    let pause = crate::live_source::worker_pause(&adapter.reader, &adapter.command);

    assert_eq!(pause, crate::live_source::POLL_INTERVAL);
}

#[test]
fn the_apps_page_reads_no_older_tail() {
    // Goal: the Apps page, whose charts draw no older tail, costs no read of one.
    // Method: open it on a Store over minutes one of which lies before its full detail, and read it
    // to its end.
    let recording = crate::live_source::tests::SealedRecording::with_an_older_kept_minute();
    let store = viewer::store::Store::start_with_budget(
        &recording.root,
        Arc::new(viewer::store::ByteBudget::new(128 * 1024 * 1024)),
    )
    .unwrap();
    let handle = store.query_handle();
    wait_until_complete(&handle);
    let mut adapter = Adapter::on_root(handle, &recording.root);
    adapter.command.request = request("apps");
    adapter.command.request.earlier = crate::live_source::Earlier::Load;

    let page = adapter.finished_within(Duration::from_secs(10));

    assert!(page.selection.get("earlier_trends").is_none());
}

#[test]
fn a_failed_read_counts_as_unread_and_rests_the_reader() {
    // Goal: a page whose read failed counts as not read, and the reader takes a second before it
    // tries again, however soon the Store answers.
    // Method: a page shown before the read, a Store that leaves it to the saved minutes, and a root
    // that holds no recording.
    let (_recording, _store, handle) = store_parked_behind_the_ring();
    let mut adapter = Adapter::new(handle);
    adapter.reader.last_generation = adapter.command.generation;

    let failed = adapter.first_update();

    assert!(matches!(failed, Update::Failed { .. }));
    assert!(!crate::live_source::reader_published(
        &adapter.reader,
        adapter.command.generation
    ));
    assert!(adapter.reader.retry_after > Instant::now());
}

fn store_answering() -> (
    LiveRecording,
    viewer::store::Store,
    viewer::store::QueryHandle,
) {
    let mut recording = LiveRecording::new();
    recording.publish_resources();
    recording.publish_resources();
    let store = viewer::store::Store::start_with_budget(
        &recording.root,
        Arc::new(viewer::store::ByteBudget::new(128 * 1024 * 1024)),
    )
    .unwrap();
    let handle = store.query_handle();
    Harness::ready_context(&handle);

    (recording, store, handle)
}

fn latest_query() -> viewer::store::QueryRequest {
    viewer::store::QueryRequest {
        epoch: 0,
        source: None,
        selection: viewer::store::QuerySelection::FollowLatest,
    }
}

fn wait_until_the_pause_awaits_an_answer(shared: &crate::live_source::Shared) {
    for _attempt in 0..6_000 {
        if shared.mailbox.lock().unwrap().pause_end == crate::live_source::PauseEnd::StoreAnswer {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the pause never began");
}

fn wait_until_complete(handle: &viewer::store::QueryHandle) {
    for _attempt in 0..6_000 {
        if Harness::ready_context(handle).readiness() == viewer::store::QueryReadiness::Complete {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the Store never finished reading");
}

fn wait_until_answered(handle: &viewer::store::QueryHandle) {
    for _attempt in 0..6_000 {
        if handle.answered() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("the Store never answered");
}

struct Adapter {
    root: std::path::PathBuf,
    reader: crate::live_source::Reader,
    command: crate::live_source::Command,
    shared: crate::live_source::Shared,
}

impl Adapter {
    fn new(handle: viewer::store::QueryHandle) -> Self {
        Self::on_root(handle, std::path::Path::new(""))
    }

    fn on_root(handle: viewer::store::QueryHandle, root: &std::path::Path) -> Self {
        Self {
            root: root.to_owned(),
            reader: crate::live_source::Reader {
                session: None,
                store: Some(super::super::StoreReader::new(handle)),
                last_catalog: None,
                last_generation: 0,
                last_route: String::new(),
                pending: None,
                retry_after: Instant::now(),
            },
            command: crate::live_source::Command {
                generation: 1,
                epoch: 0,
                request: request("memory"),
                context: crate::live_source::Context::Select,
            },
            shared: crate::live_source::Shared::new(),
        }
    }

    fn read(&mut self) -> Option<Update> {
        super::super::read(
            &mut self.reader,
            &self.root,
            &mut self.command,
            &self.shared,
        )
    }

    /// The first thing the reader publishes, stepping with no pause, so a test of its bounded wait
    /// takes none of the time that wait lasts at the worker's pace.
    fn first_update(&mut self) -> Update {
        for _ in 0..1_000 {
            if let Some(update) = self.read() {
                return update;
            }
        }
        panic!("the reader kept waiting");
    }

    fn update_within(&mut self, limit: Duration) -> Update {
        let deadline = Instant::now() + limit;
        for _ in 0..1_000 {
            if let Some(update) = self.read() {
                return update;
            }
            assert!(Instant::now() < deadline, "the reader kept waiting");
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the reader kept waiting within its attempts");
    }

    /// The page once the reader has read all of it, a step at the worker's quickest.
    fn finished_within(&mut self, limit: Duration) -> Box<Snapshot> {
        for _ in 0..1_000 {
            let Update::Ready(page) = self.update_within(limit) else {
                panic!("the page failed");
            };
            if !page.histories_pending {
                return page;
            }
        }
        panic!("the page never finished within its attempts");
    }

    fn submit(&mut self, request: Request, context: crate::live_source::Context) {
        self.shared.cancellation.cancel().unwrap();
        self.command.generation += 1;
        self.command.epoch = self.shared.cancellation.epoch();
        self.command.request = request;
        self.command.context = context;
    }

    fn ready_for(&mut self, readiness: viewer::store::QueryReadiness) -> Box<Snapshot> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut failure = None;
        for _ in 0..1000 {
            if let Some(update) = self.read() {
                match update {
                    Update::Ready(snapshot) => {
                        if snapshot.histories_pending
                            == (readiness == viewer::store::QueryReadiness::Primary)
                        {
                            return snapshot;
                        }
                    }
                    Update::Failed { message, .. } => failure = Some(message),
                }
            }
            assert!(
                Instant::now() < deadline,
                "Adapter did not reply: {failure:?}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("Adapter exceeded bounded attempts: {failure:?}");
    }
}

impl Harness {
    fn denied_history() -> (
        Self,
        Arc<viewer::store::ByteBudget>,
        viewer::store::ByteReservation,
    ) {
        let mut recording = LiveRecording::with_process_capacity(u64::from(HISTORY_PROCESSES));
        recording.publish_resources_with_processes(&history_processes());
        seal_recording(&recording, 0);
        for _ in 0..63 {
            recording.publish_resources();
        }
        seal_recording(&recording, 1);
        for _ in 0..60 {
            recording.publish_resources();
        }
        seal_recording(&recording, 2);
        let budget = Arc::new(viewer::store::ByteBudget::new(128 * 1024 * 1024));
        let lease = budget
            .reserve(budget.maximum() - INITIAL_ROOM_BYTES)
            .unwrap();
        let trace = persistence::work::ingestion::Trace::new(
            collector::recording::read_identity(&recording.root).unwrap(),
        );
        let scope = trace.enter();
        let store =
            viewer::store::Store::start_with_budget(&recording.root, Arc::clone(&budget)).unwrap();
        let worker =
            Worker::start_from_store(recording.root.clone(), store.query_handle()).unwrap();

        (
            Self {
                worker,
                store: Some(store),
                recording: Some(recording),
                trace,
                scope: Some(scope),
            },
            budget,
            lease,
        )
    }

    fn ordinary_app_work(&self) -> usize {
        use persistence::work::ingestion::{Kind, Producer, Purpose};
        let mut events = Vec::new();
        self.trace.snapshot(&mut events).unwrap();
        events
            .iter()
            .filter(|event| event.producer == Producer::Store)
            .filter(|event| event.purpose == Purpose::Ordinary)
            .filter(|event| event.kind == Kind::AppDerivation)
            .count()
    }

    fn ready_primary(&mut self, budget: &viewer::store::ByteBudget) -> Box<Snapshot> {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut failure = None;
        for _ in 0..1000 {
            if let Some(update) = self.worker.take().unwrap() {
                match update {
                    Update::Ready(snapshot) => {
                        assert!(snapshot.histories_pending);
                        return snapshot;
                    }
                    Update::Failed { message, .. } => failure = Some(message),
                }
            }
            assert!(
                Instant::now() < deadline,
                "Primary did not reply: {failure:?}; budget {}/{}",
                budget.used(),
                budget.maximum(),
            );
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("Primary exceeded bounded attempts: {failure:?}");
    }
}

fn denied_history_check(root: &std::path::Path, budget: &Arc<viewer::store::ByteBudget>) {
    let run = collector::recording::Registry::read(root).unwrap().runs[0].identity;
    let catalog_budget = Arc::new(persistence::resources::ByteBudget::new(1 << 28));
    let manifest = persistence::canonical::catalog::Catalog::open(root, &catalog_budget)
        .unwrap()
        .windows_of(&run, &mut persistence::canonical::ReadWork::default())
        .unwrap()
        .into_iter()
        .find(|manifest| manifest.identity.window == 0)
        .unwrap();
    let processes = root.join(manifest.data_directory).join("processes.parquet");
    assert!(matches!(
        persistence::engine::AdmittedTable::from_file_budgeted(
            std::fs::File::open(processes).unwrap(),
            budget,
        ),
        Err(persistence::engine::AdmissionRefusal::Budget)
    ));
}

fn history_processes() -> Vec<collector::processes::Process> {
    use collector::processes::{Process, Task, Usage};
    let mut seed = 0x6275_6467_6574_6564_u64;
    (1..=HISTORY_PROCESSES)
        .map(|pid| Process {
            pid: i32::try_from(pid).unwrap(),
            usage: Usage {
                awake_ns_at_start: 0,
                user_ns: history_counter(&mut seed),
                system_ns: history_counter(&mut seed),
                energy_nj: history_counter(&mut seed),
                instructions: history_counter(&mut seed),
                super_instructions: history_counter(&mut seed),
                cycles: history_counter(&mut seed),
                idle_wakeups: history_counter(&mut seed),
                interrupt_wakeups: history_counter(&mut seed),
                pageins: history_counter(&mut seed),
                resident_bytes: history_counter(&mut seed),
                footprint_bytes: history_counter(&mut seed),
                disk_read_bytes: history_counter(&mut seed),
                disk_written_bytes: history_counter(&mut seed),
            },
            task: Task {
                threads: history_task_counter(&mut seed),
                faults: history_task_counter(&mut seed),
                context_switches: history_task_counter(&mut seed),
                mach_syscalls: history_task_counter(&mut seed),
                unix_syscalls: history_task_counter(&mut seed),
            },
        })
        .collect()
}

fn history_counter(seed: &mut u64) -> u64 {
    *seed ^= *seed << 13;
    *seed ^= *seed >> 7;
    *seed ^= *seed << 17;
    *seed
}

fn history_task_counter(seed: &mut u64) -> i32 {
    i32::try_from(history_counter(seed) & 0x3fff_ffff).unwrap()
}
