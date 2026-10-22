use serde_json::json;

#[path = "live_cpu_history_source_tests.rs"]
mod cpu_history;

#[path = "live_source_hover_tests.rs"]
mod hover;
#[path = "live_process_history_source_tests.rs"]
pub(crate) mod process_history;
#[path = "live_rankings_source_tests.rs"]
pub(crate) mod rankings;
#[path = "live_source_visibility_tests.rs"]
mod visibility;

#[test]
fn an_empty_recording_reports_absence_without_requiring_a_live_ring() {
    // Goal: a valid recording with no saved ticks opens without a fabricated observation.
    // Method: create the real empty store and read it through the normal worker.
    let recording = SealedRecording::empty();

    let snapshot = recording.read(SealedRecording::request()).unwrap();

    assert!(snapshot.selection["second"].is_null());
    assert!(snapshot.selection["selected"].is_null());
    assert_eq!(snapshot.selection["trends"], json!([]));
    assert!(snapshot.pages.is_empty());
}

#[test]
fn a_chart_request_carries_the_older_tail_and_a_hover_request_does_not() {
    // Goal: charts get the saved readings older than the full-detail context so a range of 30 or
    // 60 minutes has something to show, while a hover's preview stays cheap. Method: read a
    // recording with a minute sealed long before its newest second, with and without the tail.
    let recording = SealedRecording::with_an_older_minute();
    let mut request = SealedRecording::request();
    request.earlier = crate::live_source::Earlier::Load;

    let charts = recording.read(request.clone()).unwrap();
    request.earlier = crate::live_source::Earlier::Skip;
    let hover = recording.read(request).unwrap();

    let tail = charts.selection["earlier_trends"].as_array().unwrap();
    assert_eq!(tail.len(), 1);
    assert_eq!(tail[0]["wall_ns"], 1_000_000_000_000_u64);
    assert_eq!(
        charts.selection["earlier_stride_seconds"],
        viewer::EARLIER_STRIDE_SECONDS
    );
    assert!(hover.selection.get("earlier_trends").is_none());
    let panels = crate::live_projection::panels("cpu", &charts.selection, None).unwrap();
    assert_eq!(
        panels[0].window,
        Some(crate::chart_axes::TimeWindow {
            first: 0,
            span: 2201
        })
    );
}

#[test]
fn reader_diagnostics_returns_catalog_without_reading_a_selected_gap() {
    // Goal: diagnostics remains available without projecting a historical selection or trends.
    // Method: use a real sealed store while the committed inspection points into a gap.
    let recording = SealedRecording::new();
    let mut request = SealedRecording::request();
    request.route = "self".into();
    request.second = Some(1001);

    let snapshot = recording.read(request).unwrap();

    assert_eq!(snapshot.catalog["last_second"], 1002);
    assert!(snapshot.selection.is_null());
    assert!(snapshot.pages.is_empty());
    assert!(snapshot.cpu_detail.is_none());
    assert!(snapshot.cpu_history.is_none());
}

#[test]
fn a_delivered_saved_source_does_not_poll_its_recording_while_idle() {
    // Goal: opening saved readings performs no idle data-source work after delivery.
    // Method: make the real store invalid after a successful request and step again.
    let recording = SealedRecording::new();
    let mut reader = SnapshotReader::new();
    reader.read_complete(&recording);
    std::fs::write(recording.root.join("identity"), b"invalid").unwrap();

    let idle = reader.read(&recording);

    assert!(idle.unwrap().is_none());
}

#[test]
fn a_new_saved_request_revalidates_the_source_after_idle() {
    // Goal: an explicit refresh can report a store that became unreadable after opening.
    // Method: invalidate the real identity and submit the next requested generation.
    let recording = SealedRecording::new();
    let mut reader = SnapshotReader::new();
    reader.read_complete(&recording);
    std::fs::write(recording.root.join("identity"), b"invalid").unwrap();
    reader.command.generation += 1;

    let refreshed = reader.read(&recording);

    assert!(refreshed.is_err());
}

#[test]
fn a_history_route_publishes_its_readings_before_its_app_histories() {
    // Goal: a newly opened page shows its readings without waiting for its slower App histories.
    // Method: read Overview twice through the reader over a real sealed recording.
    let recording = SealedRecording::new();
    let mut reader = SnapshotReader::new();

    let first = reader.read(&recording).unwrap().unwrap();
    let second = reader.read(&recording).unwrap().unwrap();

    assert!(first.histories_pending);
    assert_eq!(first.selection["second"], 1002);
    assert!(first.overview_app_histories.is_none());
    assert!(first.resident_history.is_none());
    assert!(!second.histories_pending);
    assert_eq!(second.selection["second"], 1002);
    assert!(overview_ready(&second));
    assert!(resident_ready(&second));
}

#[test]
fn a_page_opened_over_minutes_that_keep_their_app_parts_publishes_its_app_histories_at_once() {
    // Goal: a newly opened page whose saved minutes keep their Apps' parts fills its App charts
    // with its first readings, since reading them replays no saved minute.
    // Method: read Overview once over a sealed recording whose minute keeps its App parts.
    let recording = SealedRecording::with_kept_parts();
    let mut reader = SnapshotReader::new();

    let opened = reader.read(&recording).unwrap().unwrap();

    assert!(!opened.histories_pending);
    assert_eq!(opened.selection["second"], 1002);
    assert!(overview_ready(&opened));
    assert!(resident_ready(&opened));
}

#[test]
fn a_page_opened_before_any_minute_is_saved_publishes_its_app_histories_at_once() {
    // Goal: a launch over a recording that has saved no minute yet fills its App charts with
    // its first readings, since reading them replays no saved minute.
    // Method: publish two ticks to a live ring with no saved history and open Overview.
    let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    recording.publish();
    recording.publish();
    let mut reader = SnapshotReader::new();
    reader.command.mode = crate::live_source_mode::SourceMode::Live;

    let opened = reader.read_at(&recording.root).unwrap().unwrap();

    assert!(!opened.histories_pending);
    assert!(overview_ready(&opened));
}

#[test]
fn a_memory_page_opened_over_minutes_that_keep_their_app_parts_publishes_its_history_at_once() {
    // Goal: the Memory page fills its resident chart with its first readings where its saved
    // minutes keep their Apps' parts, as Overview does.
    // Method: read the Memory page once over a sealed recording whose minute keeps its parts.
    let recording = SealedRecording::with_kept_parts();
    let mut reader = SnapshotReader::new();
    reader.command.request.route = "memory".into();

    let opened = reader.read(&recording).unwrap().unwrap();

    assert!(!opened.histories_pending);
    assert!(resident_ready(&opened));
}

#[test]
fn a_page_that_reads_its_app_histories_early_ranks_the_rest_of_its_window_next() {
    // Goal: a page whose first readings brought its App histories still ranks the observations
    // its window keeps no ranks for, in a step straight after, even in a recording no new
    // reading reaches.
    // Method: open Overview over a sealed recording whose minute keeps its parts, then step the
    // reader again.
    let recording = SealedRecording::with_kept_parts();
    let mut reader = SnapshotReader::new();
    reader.read(&recording).unwrap().unwrap();
    let pause = reader.pause();

    let ranked = reader.read(&recording).unwrap();

    assert_eq!(pause, Duration::ZERO);
    assert!(ranked.is_some_and(|snapshot| !snapshot.histories_pending));
}

#[test]
fn a_page_whose_saved_minutes_cannot_be_checked_leaves_its_app_histories_to_its_next_read() {
    // Goal: a failed check of the saved minutes never fails a page's first readings, and the
    // page's next step reads the App histories, which shows the failure.
    // Method: corrupt the App parts of a sealed recording whose minute keeps them, then open
    // Overview.
    let recording = SealedRecording::with_kept_parts();
    recording.corrupt_app_parts();
    let mut reader = SnapshotReader::new();

    let opened = reader.read(&recording).unwrap().unwrap();

    assert!(opened.histories_pending);
    assert!(opened.overview_app_histories.is_none());
}

#[test]
fn pages_of_one_metric_leave_their_app_history_to_their_next_read() {
    // Goal: the first readings of the CPU, energy and storage pages never wait on their App
    // history, since it replays every saved minute of its range, parts kept or not.
    // Method: open each page over a sealed recording whose minute keeps its App parts.
    let recording = SealedRecording::with_kept_parts();
    for route in ["cpu", "energy", "storage"] {
        let mut reader = SnapshotReader::new();
        reader.command.request.route = route.into();

        let opened = reader.read(&recording).unwrap().unwrap();

        assert!(opened.histories_pending, "{route}");
        assert!(opened.app_attribution_history.is_none(), "{route}");
    }
}

#[test]
fn a_page_that_reads_its_app_histories_early_reads_an_older_tail_next() {
    // Goal: an opened page whose first readings brought its App histories reads an older tail
    // its session does not keep straight after them.
    // Method: open Overview over a recording whose minutes keep their parts and one of which
    // was sealed long before its newest second, one step at a time.
    let recording = SealedRecording::with_an_older_kept_minute();
    let mut reader = SnapshotReader::new();
    reader.command.request.earlier = crate::live_source::Earlier::Load;

    let first = reader.read(&recording).unwrap().unwrap();
    let pause = reader.pause();
    let second = reader
        .read(&recording)
        .unwrap()
        .expect("The reader left the older tail unread");

    assert!(overview_ready(&first));
    assert!(first.histories_pending);
    assert!(first.selection.get("earlier_trends").is_none());
    assert_eq!(pause, Duration::ZERO);
    assert!(!second.histories_pending);
    let tail = second.selection["earlier_trends"].as_array().unwrap();
    assert_eq!(tail.len(), 1);
}

#[test]
fn a_route_without_app_histories_publishes_one_complete_snapshot() {
    // Goal: a page with no App history is read once, with nothing left pending.
    // Method: read the GPU page twice through the reader over a real sealed recording.
    let recording = SealedRecording::new();
    let mut reader = SnapshotReader::new();
    reader.command.request.route = "gpu".into();

    let first = reader.read(&recording).unwrap().unwrap();
    let second = reader.read(&recording).unwrap();

    assert!(!first.histories_pending);
    assert!(second.is_none());
}

#[test]
fn a_newly_opened_page_reads_its_app_histories_without_a_pause() {
    // Goal: the worker reads a page's App histories straight after publishing its readings,
    // since the page shows them as being read until then.
    // Method: read Overview once through the reader over a real sealed recording.
    let recording = SealedRecording::new();
    let mut reader = SnapshotReader::new();

    reader.read(&recording).unwrap().unwrap();

    assert_eq!(reader.pause(), Duration::ZERO);
}

#[test]
fn a_newly_opened_page_publishes_its_readings_before_an_older_tail_it_has_to_read() {
    // Goal: an opened page shows its readings without waiting for an older tail its session
    // would read from saved minutes, and the worker reads that tail straight after.
    // Method: read the GPU page, which has no App histories, over a recording with a minute
    // sealed long before its newest second, one step at a time.
    let recording = SealedRecording::with_an_older_minute();
    let mut reader = SnapshotReader::new();
    reader.command.request.route = "gpu".into();
    reader.command.request.earlier = crate::live_source::Earlier::Load;

    let first = reader.read(&recording).unwrap().unwrap();
    let pause = reader.pause();
    let second = reader
        .read(&recording)
        .unwrap()
        .expect("The reader left the older tail unread");

    assert!(first.histories_pending);
    assert!(first.selection.get("earlier_trends").is_none());
    assert_eq!(pause, Duration::ZERO);
    assert!(!second.histories_pending);
    let tail = second.selection["earlier_trends"].as_array().unwrap();
    assert_eq!(tail.len(), 1);
}

#[test]
fn a_newly_opened_page_publishes_its_app_histories_before_an_older_tail_it_has_to_read() {
    // Goal: an opened page fills its App charts without waiting for an older tail its charts
    // draw only over a longer range, and the worker reads that tail straight after.
    // Method: read Overview over a recording with a minute sealed long before its newest
    // second, one step at a time.
    let recording = SealedRecording::with_an_older_minute();
    let mut reader = SnapshotReader::new();
    reader.command.request.earlier = crate::live_source::Earlier::Load;
    reader.read(&recording).unwrap().unwrap();

    let histories = reader.read(&recording).unwrap().unwrap();
    let pause = reader.pause();
    let tail = reader
        .read(&recording)
        .unwrap()
        .expect("The reader left the older tail unread");

    assert!(overview_ready(&histories));
    assert!(resident_ready(&histories));
    assert!(histories.selection.get("earlier_trends").is_none());
    assert!(histories.histories_pending);
    assert_eq!(pause, Duration::ZERO);
    assert!(overview_ready(&tail));
    assert_eq!(
        tail.selection["earlier_trends"].as_array().unwrap().len(),
        1
    );
    assert!(!tail.histories_pending);
}

#[test]
fn a_new_request_of_a_page_still_reads_its_app_histories_before_an_older_tail_it_has_to_read() {
    // Goal: a page asked for again before its App histories arrive, as a window that settles
    // its size at launch asks, still fills its App charts before reading the older tail.
    // Method: read Overview once over a recording with a minute sealed long before its newest
    // second, ask for it again under a new generation, and read one step at a time.
    let recording = SealedRecording::with_an_older_minute();
    let mut reader = SnapshotReader::new();
    reader.command.request.earlier = crate::live_source::Earlier::Load;
    reader.read(&recording).unwrap().unwrap();
    reader.command.generation += 1;

    let histories = reader.read(&recording).unwrap().unwrap();
    let tail = reader
        .read(&recording)
        .unwrap()
        .expect("The reader left the older tail unread");

    assert!(overview_ready(&histories));
    assert!(histories.selection.get("earlier_trends").is_none());
    assert!(histories.histories_pending);
    assert_eq!(
        tail.selection["earlier_trends"].as_array().unwrap().len(),
        1
    );
    assert!(!tail.histories_pending);
}

#[test]
fn a_newly_opened_page_completes_with_its_app_histories_where_its_session_keeps_the_tail() {
    // Goal: a page whose older tail its session already keeps takes no step past its App
    // histories.
    // Method: read the GPU page to the end, then open Memory at the same newest second and read
    // two steps.
    let recording = SealedRecording::with_an_older_minute();
    let mut reader = SnapshotReader::new();
    reader.command.request.route = "gpu".into();
    reader.command.request.earlier = crate::live_source::Earlier::Load;
    reader.read_complete(&recording);
    reader.command.generation += 1;
    reader.command.request.route = "memory".into();
    reader.read(&recording).unwrap().unwrap();

    let histories = reader.read(&recording).unwrap().unwrap();
    let pause = reader.pause();

    assert!(resident_ready(&histories));
    assert_eq!(
        histories.selection["earlier_trends"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(!histories.histories_pending);
    assert_eq!(pause, super::POLL_INTERVAL);
}

#[test]
fn a_newly_opened_page_brings_the_older_tail_its_session_keeps_with_its_readings() {
    // Goal: a page opened after another read the same older tail shows it at once.
    // Method: read the GPU page to the end, then open Memory at the same newest second.
    let recording = SealedRecording::with_an_older_minute();
    let mut reader = SnapshotReader::new();
    reader.command.request.route = "gpu".into();
    reader.command.request.earlier = crate::live_source::Earlier::Load;
    reader.read_complete(&recording);
    reader.command.generation += 1;
    reader.command.request.route = "memory".into();

    let opened = reader.read(&recording).unwrap().unwrap();

    let tail = opened.selection["earlier_trends"].as_array().unwrap();
    assert_eq!(tail.len(), 1);
}

#[test]
fn a_handed_over_reader_reads_the_next_windows_page_as_newly_opened() {
    // Goal: a window opened over a kept reader shows its readings before its App histories, as
    // it would over a new reader, whatever page the closed window showed last.
    // Method: read Overview to the end, hand the reader over, request Overview through the
    // kept handle, and step the reader's mailbox and read.
    let recording = SealedRecording::new();
    let mut reader = SnapshotReader::new();
    reader.read_complete(&recording);
    let mut kept = reader.hand_over();
    kept.request(SealedRecording::request()).unwrap();

    let received = reader.receive();
    let reopened = reader.read(&recording).unwrap().unwrap();

    assert_eq!(received, (super::Received::Taken, true));
    assert!(reopened.histories_pending);
}

#[test]
fn a_handed_over_reader_follows_the_ring_while_no_page_reads() {
    // Goal: a window opened over a kept reader reads only the ticks since the reader's last
    // step, rather than every tick published since its window closed.
    // Method: read a live ring, hand the reader over, publish a tick, and step the reader as
    // the worker does.
    let mut recording = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    recording.publish();
    let mut reader = SnapshotReader::new();
    reader.command.mode = crate::live_source_mode::SourceMode::Live;
    reader.read_at(&recording.root).unwrap().unwrap();
    let _kept = reader.hand_over();
    recording.publish();

    let received = reader.receive();
    let pause = reader.step_resting(&recording.root);

    assert_eq!(received, (super::Received::Continued, false));
    assert_eq!(pause, super::POLL_INTERVAL);
    let last_second = reader.session().catalog()["last_second"].as_u64();
    assert_eq!(last_second, Some(recording.last_second()));
}

#[test]
fn a_resting_reader_without_a_session_opens_one() {
    // Goal: a window closed before its reader opened the recording still opens again at once.
    // Method: step a reader that never read a page as the worker steps a resting one.
    let recording = SealedRecording::new();
    let mut reader = SnapshotReader::new();

    let pause = reader.step_resting(&recording.root);

    assert_eq!(pause, super::POLL_INTERVAL);
    assert_eq!(reader.session().catalog()["last_second"], 1002);
}

#[test]
fn a_request_to_a_resting_reader_lets_its_step_finish() {
    // Goal: a launch's first request does not throw away the session the resting reader is
    // opening, since the request reads through that same session.
    // Method: let the reader rest, then send it a request and compare the cancellation epoch.
    let mut reader = SnapshotReader::new();
    let mut kept = reader.hand_over();
    reader.receive();
    let epoch = reader.shared.cancellation.epoch();

    kept.request(SealedRecording::request()).unwrap();

    assert_eq!(reader.shared.cancellation.epoch(), epoch);
}

#[test]
fn a_request_to_a_reading_reader_cancels_its_read() {
    // Goal: a page the window no longer wants stops being read as soon as another is asked for.
    // Method: let the resting reader take a request and step again with no other waiting, as it
    // does while it reads the page, then send another request and compare the epoch.
    let mut reader = SnapshotReader::new();
    let mut kept = reader.hand_over();
    reader.receive();
    kept.request(SealedRecording::request()).unwrap();
    reader.receive();
    reader.receive();
    let epoch = reader.shared.cancellation.epoch();

    kept.request(SealedRecording::request()).unwrap();

    assert!(reader.shared.cancellation.epoch() > epoch);
}

#[test]
fn a_window_that_takes_the_resting_reader_renews_its_step_limit() {
    // Goal: a reader kept across windows gives each window the full step limit, so a window
    // opened days after the app started never finds its reader at the limit.
    // Method: request a page from a resting reader, and run the worker's steps over it.
    let recording = SealedRecording::new();
    let mut reader = SnapshotReader::new();
    let mut kept = reader.hand_over();
    kept.request(SealedRecording::request()).unwrap();

    let lease = reader.lease(&recording.root, None, 1);

    assert_eq!(lease, super::Lease::Renewed);
}

#[test]
fn a_window_that_reads_to_the_step_limit_is_told_so() {
    // Goal: a window whose reader reached its limit reports it rather than going quiet.
    // Method: run no steps over a reader with a page to read, and take the worker's output.
    let recording = SealedRecording::new();
    let mut reader = SnapshotReader::new();
    let command = SnapshotReader::command();

    let lease = reader.lease(&recording.root, Some(command), 0);

    assert_eq!(lease, super::Lease::Ended);
    let output = reader.shared.mailbox.lock().unwrap().output.take();
    let Some(Update::Failed { message, .. }) = output else {
        panic!("The worker published no failure");
    };
    assert_eq!(message, "Live reader reached its seven-day session limit");
}

#[test]
fn a_read_the_closed_window_asked_for_never_reaches_the_next_window() {
    // Goal: the window opened over a kept reader never shows the page the closed window read.
    // Method: hand the reader over, then publish a read under the closed window's generation.
    let recording = SealedRecording::new();
    let mut reader = SnapshotReader::new();
    let mut kept = reader.hand_over();
    let read = reader.step(&recording).unwrap();

    super::worker_publish(&reader.shared, reader.command.generation, read);

    assert!(kept.take().unwrap().is_none());
}

#[test]
fn a_kept_reader_answers_requests_after_the_closed_window_drops_its_handle() {
    // Goal: closing the window that handed its reader on leaves the reader running.
    // Method: read Overview through a worker, hand its reader over, drop the handle left
    // behind, and read Overview through the kept handle.
    let recording = SealedRecording::new();
    let mut worker = Worker::start(recording.root.clone()).unwrap();
    SealedRecording::read_through(&mut worker, SealedRecording::request(), Worker::request)
        .unwrap();
    let mut kept = worker.hand_over().unwrap().unwrap();
    drop(worker);

    let snapshot =
        SealedRecording::read_through(&mut kept, SealedRecording::request(), Worker::request);

    assert_eq!(snapshot.unwrap().selection["second"], 1002);
}

#[test]
fn a_handle_that_handed_its_reader_over_refuses_requests() {
    // Goal: a closed window's handle can never steer the reader the next window reads through.
    // Method: hand a worker's reader over and send a request through the handle left behind.
    let recording = SealedRecording::new();
    let mut worker = Worker::start(recording.root.clone()).unwrap();
    let kept = worker.hand_over().unwrap();

    let refused = worker.request(SealedRecording::request());

    assert!(kept.is_some());
    assert!(refused.is_err());
}

#[test]
fn a_handle_that_handed_its_reader_over_hands_nothing_more() {
    // Goal: the reader the next window reads through is never lost to a second hand-over.
    // Method: hand a worker's reader over twice.
    let recording = SealedRecording::new();
    let mut worker = Worker::start(recording.root.clone()).unwrap();
    let kept = worker.hand_over().unwrap();

    let again = worker.hand_over();

    assert!(kept.is_some());
    assert!(again.is_err());
}

#[test]
fn the_worker_pauses_once_a_page_has_all_it_asked_for() {
    // Goal: the worker polls at its own pace once the page's App histories are read.
    // Method: read Overview until complete through the reader over a real sealed recording.
    let recording = SealedRecording::new();
    let mut reader = SnapshotReader::new();

    reader.read_complete(&recording);

    assert_eq!(reader.pause(), super::POLL_INTERVAL);
}

#[test]
fn a_failed_read_of_a_pages_app_histories_pauses_before_its_retry() {
    // Goal: a failed read of a page's App histories never sets the worker spinning until the
    // read is retried.
    // Method: publish Overview's readings, make the real store invalid, then step the reader.
    let recording = SealedRecording::new();
    let mut reader = SnapshotReader::new();
    reader.read(&recording).unwrap().unwrap();
    std::fs::write(recording.root.join("identity"), b"invalid").unwrap();

    let failed = reader.step(&recording);

    assert!(matches!(failed, Some(Update::Failed { .. })));
    assert_eq!(reader.pause(), super::POLL_INTERVAL);
}

/// Whether the snapshot carries the Overview's App histories, each read rather than failed.
fn overview_ready(snapshot: &super::Snapshot) -> bool {
    use crate::live_app_attribution_history::AppAttributionHistory;
    let Some(histories) = &snapshot.overview_app_histories else {
        return false;
    };
    let ready =
        |history: &AppAttributionHistory| matches!(history, AppAttributionHistory::Ready(_));

    ready(&histories.cpu) && ready(&histories.power) && ready(&histories.disk)
}

/// Whether the snapshot carries the resident history, read rather than failed.
fn resident_ready(snapshot: &super::Snapshot) -> bool {
    snapshot.resident_history.as_deref().is_some_and(|history| {
        matches!(
            history,
            crate::live_resident_history::ResidentHistory::Ready(_)
        )
    })
}

struct SnapshotReader {
    reader: super::Reader,
    command: super::Command,
    shared: Arc<Shared>,
}

impl SnapshotReader {
    fn new() -> Self {
        Self {
            reader: super::Reader {
                session: None,
                system: crate::live_system::SystemSamples::unread(),
                last_catalog: None,
                last_generation: 0,
                last_route: String::new(),
                pending: None,
                retry_after: std::time::Instant::now(),
            },
            command: Self::command(),
            shared: Arc::new(Shared {
                mailbox: Mutex::new(Mailbox {
                    generation: 1,
                    request: None,
                    output: None,
                    rest: false,
                    resting: false,
                }),
                wake: Condvar::new(),
                stopped: AtomicBool::new(false),
                cancellation: Arc::new(Cancellation::default()),
            }),
        }
    }

    fn command() -> super::Command {
        super::Command {
            generation: 1,
            epoch: 0,
            mode: crate::live_source_mode::SourceMode::Recorded,
            request: SealedRecording::request(),
            context: super::Context::Select,
        }
    }

    fn read(
        &mut self,
        recording: &SealedRecording,
    ) -> Result<Option<Box<super::Snapshot>>, String> {
        self.read_at(&recording.root)
    }

    fn read_at(&mut self, root: &std::path::Path) -> Result<Option<Box<super::Snapshot>>, String> {
        super::reader_snapshot(&mut self.reader, root, &mut self.command, &self.shared)
    }

    /// Hands the reader over as a closing window does, through a handle without a thread, so
    /// the test steps the reader itself.
    fn hand_over(&self) -> Worker {
        let mut closing = Worker {
            shared: Arc::clone(&self.shared),
            thread: None,
            mode: crate::live_source_mode::SourceMode::Live,
        };
        closing.hand_over().unwrap().unwrap()
    }

    /// Takes the mailbox as the worker does at the top of a step, and tells whether the reader
    /// is left with a page to read.
    fn receive(&mut self) -> (super::Received, bool) {
        let mut command = Some(std::mem::replace(&mut self.command, Self::command()));
        let received = super::worker_receive(&self.shared, &mut self.reader, &mut command);
        let reading = command.is_some();
        if let Some(command) = command {
            self.command = command;
        }
        (received, reading)
    }

    /// Steps the reader as the worker does while no page reads, and returns its pause.
    fn step_resting(&mut self, root: &std::path::Path) -> Duration {
        super::worker_step(root, &self.shared, &mut self.reader, &mut None)
    }

    fn lease(
        &mut self,
        root: &std::path::Path,
        mut command: Option<super::Command>,
        steps_max: u64,
    ) -> super::Lease {
        super::worker_lease(
            root,
            &self.shared,
            &mut self.reader,
            &mut command,
            steps_max,
        )
    }

    fn session(&self) -> &viewer::LiveSession {
        self.reader.session.as_ref().unwrap()
    }

    fn step(&mut self, recording: &SealedRecording) -> Option<Update> {
        super::reader_read(
            &mut self.reader,
            &recording.root,
            &mut self.command,
            &self.shared,
        )
    }

    fn pause(&self) -> Duration {
        super::worker_pause(&self.reader, &self.command)
    }

    fn read_complete(&mut self, recording: &SealedRecording) -> Box<super::Snapshot> {
        for _attempt in 0..2 {
            let snapshot = self.read(recording).unwrap().unwrap();
            if !snapshot.histories_pending {
                return snapshot;
            }
        }
        panic!("The reader left its App histories pending");
    }
}

#[test]
fn corrupt_saved_history_is_reported_instead_of_becoming_an_empty_recording() {
    // Goal: an unreadable commitment cannot masquerade as valid no-data history.
    // Method: corrupt only this fixture's saved seal and read through the normal worker.
    let recording = SealedRecording::new();
    let seal = recording
        .root
        .join("commits/00000000000000010000000000000002")
        .join("00000000000000000000/00000000000000000000.seal");
    assert!(!std::fs::read(&seal).unwrap().is_empty());
    std::fs::write(seal, b"corrupt").unwrap();

    let result = recording.read(SealedRecording::request());

    assert!(result.is_err());
}

#[test]
fn a_sealed_recording_opens_at_its_latest_saved_observation_without_a_live_ring() {
    // Goal: latest selects the final sibling of the last second without a live collector.
    // Method: request Overview through its worker using a real sealed-only recording.
    let recording = SealedRecording::new();

    let result = recording.read(SealedRecording::request());

    assert!(result.is_ok(), "{}", result.err().unwrap());
    let snapshot = result.unwrap();
    assert_eq!(snapshot.selection["second"], 1002);
    assert_eq!(snapshot.selection["selected"]["publication"], 3);
    assert_eq!(snapshot.selection["summary"]["memory_total_bytes"], 12_288);
    assert_eq!(
        snapshot.pages["fans"]["observation"],
        snapshot.selection["selected"]
    );
}

#[test]
fn a_sealed_recording_keeps_an_explicit_same_second_observation() {
    // Goal: exact inspection can choose an earlier sibling after latest chooses the last.
    // Method: select the earlier literal identity through the real recording worker.
    let recording = SealedRecording::new();
    let mut request = SealedRecording::request();
    request.second = Some(1002);
    request.identity = Some(json!({"run":"00000000000000010000000000000002",
        "boot":"00000000000000030000000000000004", "publication":2}));

    let snapshot = recording.read(request).unwrap();

    assert_eq!(snapshot.selection["selected"]["publication"], 2);
    assert_eq!(snapshot.selection["summary"]["memory_total_bytes"], 8192);
    assert_eq!(
        snapshot.selection["alternatives"].as_array().unwrap().len(),
        2
    );
}

#[test]
fn a_hover_read_brings_its_second_alone_and_keeps_the_context() {
    // Goal: a hover's read brings the hovered observation's readings without the 900 seconds
    // around it or the CPU history over them, which the page's charts already hold.
    // Method: read the second of three CPU observations as a hover's read asks for it.
    let recording = SealedRecording::with_cpu_history();
    let mut request = SealedRecording::request();
    request.route = "cpu".into();
    request.second = Some(1001);
    request.identity = Some(json!({"run":"00000000000000010000000000000002",
        "boot":"00000000000000030000000000000004", "publication":2}));
    request.load_resident_history = false;
    request.load_app_attribution_history = false;
    request.load_process_history = false;

    let snapshot = recording.inspect(request).unwrap();

    assert_eq!(snapshot.selection["selected"]["publication"], 2);
    assert_eq!(snapshot.selection["summary"]["publication"], 2);
    assert!(snapshot.selection.get("trends").is_none());
    assert_eq!(
        snapshot.cpu_detail.as_ref().unwrap().observation["publication"],
        2
    );
    assert!(snapshot.cpu_history.is_none());
}

#[test]
fn a_sealed_recording_leaves_a_missing_second_empty() {
    // Goal: a saved gap never borrows either neighboring measured observation.
    // Method: request the unrecorded second between two real encoded ticks.
    let recording = SealedRecording::new();
    let mut request = SealedRecording::request();
    request.second = Some(1001);

    let snapshot = recording.read(request).unwrap();

    assert_eq!(snapshot.selection["second"], 1001);
    assert!(snapshot.selection["selected"].is_null());
    assert!(snapshot.selection["summary"].is_null());
    assert!(snapshot.pages.is_empty());
}

struct SealedRecording {
    root: std::path::PathBuf,
}

impl SealedRecording {
    /// Overwrites the App parts every sealed window of the recording keeps with bytes no
    /// Parquet reader reads.
    fn corrupt_app_parts(&self) {
        let mut directories = vec![self.root.clone()];
        let mut corrupted = 0;
        while let Some(directory) = directories.pop() {
            for entry in std::fs::read_dir(directory).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    directories.push(path);
                } else {
                    if path.file_name() == Some("app_parts.parquet".as_ref()) {
                        std::fs::write(&path, b"corrupt").unwrap();
                        corrupted += 1;
                    }
                }
            }
        }
        assert!(corrupted > 0);
    }

    fn empty() -> Self {
        let token =
            collector::recording::identity_text(collector::recording::random_identity().unwrap());
        let root = std::env::temp_dir().join(format!("flightdeck-sealed-{token}"));
        let recording = Self { root };
        let store = persistence::Store::open(&recording.root).unwrap();
        drop(store);

        recording
    }

    fn new() -> Self {
        Self::sealed(&Underived)
    }

    /// The recording of `new`, sealed as the persister seals a window now, with the App parts
    /// and ranks of each of its observations.
    fn with_kept_parts() -> Self {
        Self::sealed(&viewer::SealedApps::default())
    }

    /// Readings at seconds 1000 and 1002 in one window sealed with the tables `derive` derives.
    fn sealed(derive: &dyn persistence::worker::Derive) -> Self {
        let recording = Self::empty();
        let store = persistence::Store::open(&recording.root).unwrap();
        let identity = persistence::WindowIdentity {
            run: collector::ring::RunIdentity {
                recording: store.recording(),
                run: collector::ring::Identity([1, 2]),
                boot: collector::ring::Identity([3, 4]),
            },
            window: 0,
        };
        let mut window = Self::window(identity);
        Self::append(&mut window, 1, 1000);
        Self::append(&mut window, 2, 1002);
        Self::append(&mut window, 3, 1002);
        Self::commit_derived(&store, identity, &window, derive);
        drop(store);

        recording
    }

    /// Readings at second 1000 and two at second 2200, which are more than 900 seconds apart.
    fn with_an_older_minute() -> Self {
        Self::older_minute(&Underived)
    }

    /// The recording of `with_an_older_minute`, sealed with the App parts and ranks of each of
    /// its observations.
    fn with_an_older_kept_minute() -> Self {
        Self::older_minute(&viewer::SealedApps::default())
    }

    /// Readings at second 1000 and two at second 2200 in one window sealed with the tables
    /// `derive` derives.
    fn older_minute(derive: &dyn persistence::worker::Derive) -> Self {
        let recording = Self::empty();
        let store = persistence::Store::open(&recording.root).unwrap();
        let identity = persistence::WindowIdentity {
            run: collector::ring::RunIdentity {
                recording: store.recording(),
                run: collector::ring::Identity([1, 2]),
                boot: collector::ring::Identity([3, 4]),
            },
            window: 0,
        };
        let mut window = Self::window(identity);
        Self::append(&mut window, 1, 1000);
        Self::append(&mut window, 2, 2200);
        Self::append(&mut window, 3, 2200);
        Self::commit_derived(&store, identity, &window, derive);
        drop(store);

        recording
    }

    fn window(identity: persistence::WindowIdentity) -> persistence::Window {
        persistence::Window::new(
            identity,
            persistence::Limits::from_shape(&Self::shape()).unwrap(),
        )
    }

    fn shape() -> collector::ring::Shape {
        collector::ring::Shape {
            slot_count: 4,
            cpu_capacity: 1,
            interface_capacity: 1,
            core_capacity: 1,
            state_capacity: 1,
            fan_capacity: 1,
            process_capacity: 1,
            coalition_capacity: 1,
            event_capacity: 1,
        }
    }

    fn append(window: &mut persistence::Window, publication: u64, second: u64) {
        let mut tick = persistence::tick_buffer(&Self::shape()).unwrap();
        tick.run_tick = publication - 1;
        tick.memory.total_bytes = 4096 * publication;
        let observation = persistence::Observation {
            publication,
            wall_ns: Some(second * 1_000_000_000),
            continuous_ns: Some(publication * 1_000_000_000),
            awake_ns: Some(publication * 1_000_000_000),
        };
        window.push_tick(&observation, &tick, &[]).unwrap();
    }

    fn commit(
        store: &persistence::Store,
        identity: persistence::WindowIdentity,
        window: &persistence::Window,
    ) {
        Self::commit_derived(store, identity, window, &Underived);
    }

    /// Commits the run's first window with the tables `derive` derives from it, as the persister
    /// seals a window.
    fn commit_derived(
        store: &persistence::Store,
        identity: persistence::WindowIdentity,
        window: &persistence::Window,
        derive: &dyn persistence::worker::Derive,
    ) {
        let persistence::Begin::Pending(pending) = store.begin(&identity).unwrap() else {
            panic!("Fixture window already committed");
        };
        let mut tables = persistence::Encoder.encode(window, pending.path()).unwrap();
        tables.extend(
            derive
                .derive(&persistence::worker::Derivation {
                    identity,
                    pending: pending.path(),
                    predecessor: None,
                })
                .unwrap(),
        );
        let publications = window.publication_range();
        let events = window.event_range();
        store
            .commit(
                pending,
                persistence::Manifest {
                    identity,
                    publication_first: publications.start,
                    publication_end: publications.end,
                    events_first: events.start,
                    events_end: events.end,
                    wall_ns: None,
                    continuous_ns: None,
                    awake_ns: None,
                    losses: persistence::Losses::default(),
                    data_directory: String::new(),
                    tables,
                },
            )
            .unwrap();
    }

    fn request() -> Request {
        Request {
            route: "overview".into(),
            second: None,
            identity: None,
            table: "ticks".into(),
            offset: 0,
            cpu_offset: 0,
            load_resident_history: true,
            load_app_attribution_history: true,
            load_process_history: true,
            earlier: crate::live_source::Earlier::Skip,
            executable: None,
            app: None,
            apps_offset: 0,
            members_offset: 0,
        }
    }

    fn read(&self, request: Request) -> Result<Box<super::Snapshot>, String> {
        self.read_with(request, Worker::request)
    }

    /// Reads `request` as a hover's read of its second alone.
    fn inspect(&self, request: Request) -> Result<Box<super::Snapshot>, String> {
        self.read_with(request, Worker::inspect)
    }

    fn read_with(
        &self,
        request: Request,
        submit: fn(&mut Worker, Request) -> Result<u64, String>,
    ) -> Result<Box<super::Snapshot>, String> {
        let mut worker = Worker::start(self.root.clone()).unwrap();
        Self::read_through(&mut worker, request, submit)
    }

    /// Reads `request` through `worker` until the snapshot leaves nothing pending.
    fn read_through(
        worker: &mut Worker,
        request: Request,
        submit: fn(&mut Worker, Request) -> Result<u64, String>,
    ) -> Result<Box<super::Snapshot>, String> {
        let generation = submit(worker, request)?;
        for _attempt in 0..1000 {
            match worker.take()? {
                Some(Update::Ready(snapshot)) if snapshot.generation == generation => {
                    if !snapshot.histories_pending {
                        return Ok(snapshot);
                    }
                }
                Some(Update::Failed { message, .. }) => return Err(message),
                _ => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }

        Err("Fixture reader exceeded five seconds".into())
    }
}

impl Drop for SealedRecording {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

/// Derives no table, as a persister from before windows kept their App ranks sealed them.
struct Underived;

impl persistence::worker::Derive for Underived {
    fn derive(
        &self,
        _derivation: &persistence::worker::Derivation<'_>,
    ) -> persistence::Result<Vec<persistence::TableManifest>> {
        Ok(Vec::new())
    }
}

#[test]
fn the_default_app_survives_a_new_first_row_and_an_empty_page() {
    // App history must keep its original target as the loaded list changes.
    let mut request = Request {
        route: "apps".into(),
        second: None,
        identity: None,
        table: "ticks".into(),
        offset: 0,
        cpu_offset: 0,
        load_resident_history: true,
        load_app_attribution_history: true,
        load_process_history: true,
        earlier: crate::live_source::Earlier::Skip,
        executable: None,
        app: None,
        apps_offset: 0,
        members_offset: 0,
    };
    let first = json!({"apps":[{"key":{"kind":"bundle", "bytes_hex":"41"}}]});
    let changed = json!({"apps":[{"key":{"kind":"bundle", "bytes_hex":"42"}}]});

    super::reader_apps_remember(&mut request, &first).unwrap();
    super::reader_apps_remember(&mut request, &changed).unwrap();
    super::reader_apps_remember(&mut request, &json!({"apps":[]})).unwrap();

    assert_eq!(request.app.unwrap().bytes_hex, "41");
}

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use super::{
    Cancellation, Mailbox, Request, Shared, Update, Worker, app_key_bytes, identity_signature,
    identity_validate, selection_alternative, worker_pause_for, worker_publish,
};

#[test]
fn resolves_the_exact_observation_among_same_second_siblings() {
    // Goal: a scrub keeps its run, boot and publication rather than taking the first sibling.
    // Method: resolve the third of three literal observations sharing one second.
    let selection = json!({"alternatives": [
        {"run": "00000000000000010000000000000002",
            "boot": "00000000000000030000000000000004", "publication": 7},
        {"run": "00000000000000050000000000000006",
            "boot": "00000000000000030000000000000004", "publication": 8},
        {"run": "00000000000000050000000000000006",
            "boot": "0000000000000009000000000000000a", "publication": 8}
    ]});
    let requested = selection["alternatives"][2].clone();

    let alternative = selection_alternative(&selection, &requested).unwrap();

    assert_eq!(alternative, 2);
}

#[test]
fn refuses_a_missing_observation_instead_of_selecting_a_sibling() {
    // Goal: an expired publication cannot silently move to another observation.
    // Method: request publication eight from a response retaining only seven.
    let selection = json!({"alternatives": [{
        "run": "00000000000000010000000000000002",
        "boot": "00000000000000030000000000000004", "publication": 7
    }]});
    let requested = json!({
        "run": "00000000000000010000000000000002",
        "boot": "00000000000000030000000000000004", "publication": 8
    });

    let alternative = selection_alternative(&selection, &requested);

    assert!(alternative.is_err());
}

#[test]
fn matches_live_app_stamp_words_with_the_selected_hex_identity() {
    // Goal: App packets refer to the same observation despite their lossless string encoding.
    // Method: compare decimal stamp words and publication with the live selection format.
    let selected = json!({
        "run": "00000000000000010000000000000002",
        "boot": "00000000000000030000000000000004", "publication": 9
    });
    let observed = json!({"stamp": {
        "run": ["1", "2"], "boot": ["3", "4"], "publication": "9"
    }});

    let validation = identity_validate(&observed, &selected);

    assert!(validation.is_ok());
}

#[test]
fn rejects_app_payloads_from_another_publication() {
    // Goal: mixed-generation App details never reach the UI as a coherent selection.
    // Method: compare two actual wire shapes with different publication numbers.
    let selected = json!({
        "run": "00000000000000010000000000000002",
        "boot": "00000000000000030000000000000004", "publication": 9
    });
    let observed = json!({"stamp": {
        "run": ["1", "2"], "boot": ["3", "4"], "publication": "8"
    }});

    let validation = identity_validate(&observed, &selected);

    assert!(validation.is_err());
}

#[test]
fn refuses_missing_observation_identities() {
    // Goal: absent fields cannot compare equal and admit an unvalidated packet.
    // Method: compare two empty JSON objects.
    assert!(identity_validate(&json!({}), &json!({})).is_err());
}

#[test]
fn decodes_non_utf8_app_key_bytes_without_changing_them() {
    // Goal: a selected App preserves its captured byte key rather than its display text.
    // Method: decode a path ending in two bytes that are not UTF-8.
    let bytes = app_key_bytes("2f4170702fffFe").unwrap();

    assert_eq!(bytes, b"/App/\xff\xfe");
}

#[test]
fn rejects_an_incomplete_app_key_byte() {
    // Goal: truncated hexadecimal keys cannot address another App.
    // Method: decode an odd-length wire key.
    assert!(app_key_bytes("2f1").is_err());
}

#[test]
fn rejects_non_hexadecimal_app_key_bytes() {
    // Goal: malformed bytes cannot be silently replaced with a zero byte.
    // Method: decode a wire key containing an invalid nibble.
    assert!(app_key_bytes("2fg1").is_err());
}

#[test]
fn rejects_app_keys_beyond_the_path_bound() {
    // Goal: a request cannot allocate an unbounded decoded key.
    // Method: exceed the recording format's 4095-byte path limit by one byte.
    assert!(app_key_bytes(&"aa".repeat(4096)).is_err());
}

#[test]
fn keeps_the_same_observation_when_loss_metadata_changes() {
    // Goal: a metadata refresh cannot count as an advancing live publication.
    // Method: compare one identity before and after its reader loss count changes.
    let first = json!({
        "run": "00000000000000010000000000000002",
        "boot": "00000000000000030000000000000004", "publication": 9,
        "reader_losses": {"publications": 0, "events": 0, "unknown": 0}
    });
    let mut updated = first.clone();
    updated["reader_losses"]["publications"] = json!(1);

    let first_identity = identity_signature(&first).unwrap();
    let updated_identity = identity_signature(&updated).unwrap();

    assert_eq!(first_identity, updated_identity);
}

#[test]
fn refuses_superseded_results_after_accepting_a_new_request() {
    // Goal: an old worker result cannot replace the latest requested generation.
    // Method: publish both generations through the real mailbox without a background thread.
    let mut worker = Worker {
        shared: Arc::new(Shared::new()),
        thread: None,
        mode: crate::live_source_mode::SourceMode::Live,
    };
    let request = Request {
        route: "overview".to_owned(),
        second: None,
        identity: None,
        table: "ticks".to_owned(),
        offset: 0,
        cpu_offset: 0,
        load_resident_history: true,
        load_app_attribution_history: true,
        load_process_history: true,
        earlier: crate::live_source::Earlier::Skip,
        executable: None,
        app: None,
        apps_offset: 0,
        members_offset: 0,
    };
    let old = worker.request(request.clone()).unwrap();
    let latest = worker.request(request).unwrap();

    worker_publish(
        &worker.shared,
        old,
        Update::Failed {
            generation: old,
            message: "superseded".to_owned(),
        },
    );
    let superseded = worker.take().unwrap();
    worker_publish(
        &worker.shared,
        latest,
        Update::Failed {
            generation: latest,
            message: "latest".to_owned(),
        },
    );
    let accepted = worker.take().unwrap();

    assert!(superseded.is_none());
    assert!(matches!(accepted, Some(Update::Failed { generation, .. }) if generation == latest));
}

#[test]
fn a_new_request_ends_the_pause_between_polls() {
    // Goal: opening a page does not wait out the worker's poll interval, since every page of
    // the app reads local files and should show at once. Method: pause a thread for a minute on
    // the real mailbox, send a request, and wait up to ten seconds for the pause to end, which
    // keeps the verdict independent of how busy the machine is.
    let mut worker = Worker {
        shared: Arc::new(Shared::new()),
        thread: None,
        mode: crate::live_source_mode::SourceMode::Live,
    };
    let (ended, ending) = std::sync::mpsc::channel();
    let shared = Arc::clone(&worker.shared);
    let pausing = std::thread::spawn(move || {
        worker_pause_for(&shared, Duration::from_secs(60));
        ended.send(()).unwrap();
    });
    std::thread::sleep(Duration::from_millis(50));

    worker.request(SealedRecording::request()).unwrap();

    ending.recv_timeout(Duration::from_secs(10)).unwrap();
    pausing.join().unwrap();
}
