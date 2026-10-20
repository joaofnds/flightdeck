use std::time::{Duration, Instant};

use gpui::{Entity, TestAppContext, VisualTestContext, WindowOptions};
use serde_json::json;

use super::SealedRecording;
use crate::live_dashboard::LiveDashboard;
use crate::live_hover::{Origin, Target};
use crate::live_source::{Worker, identity_signature};

#[gpui::test]
fn detail_preview_reads_the_exact_saved_same_second_observation(cx: &mut TestAppContext) {
    // Read the real saved sibling through the production controller and its sole worker.
    let mut harness = ReaderHoverHarness::setup(cx);

    harness.driver.preview_publication(2);
    harness.driver.await_preview();

    assert_eq!(harness.driver.reading(), Reading::publication(2));
    assert_eq!(harness.driver.committed_publication(), Some(3));
    assert!(harness.driver.follows_latest());
    harness.teardown();
}

#[gpui::test]
fn leaving_preview_reads_the_nonadvancing_saved_source_again(cx: &mut TestAppContext) {
    // Require a fresh worker response after exit, even though the final saved identity
    // is unchanged.
    let mut harness = ReaderHoverHarness::setup(cx);
    harness.driver.preview_publication(2);
    harness.driver.await_preview();

    harness.driver.leave_preview_and_await_follow();

    assert_eq!(harness.driver.reading(), Reading::publication(3));
    assert!(harness.driver.follows_latest());
    harness.teardown();
}

#[gpui::test]
fn a_superseded_preview_remains_readable_through_the_same_worker(cx: &mut TestAppContext) {
    // Supersede the first real request before waiting for the second exact publication.
    let mut harness = ReaderHoverHarness::setup(cx);
    harness.driver.preview_publication(1);
    harness.driver.poll_once();

    harness.driver.preview_publication(2);
    harness.driver.await_preview();

    assert_eq!(harness.driver.reading(), Reading::publication(2));
    assert_eq!(harness.driver.committed_publication(), Some(3));
    harness.teardown();
}

#[gpui::test]
fn a_saved_gap_cannot_retain_the_previous_exact_cpu_details(cx: &mut TestAppContext) {
    // Move from an acquired historical CPU payload into a true gap in the same recording.
    let mut harness = ReaderHoverHarness::setup(cx);
    harness.driver.preview_publication(2);
    harness.driver.await_preview();

    harness.driver.preview_gap();
    harness.driver.poll_once();

    assert_eq!(
        harness.driver.reading(),
        Reading {
            second: 1002,
            publication: None,
            cpu_publication: None,
            cpu_count: 0,
            page_publications: Vec::new(),
        }
    );
    assert_eq!(harness.driver.committed_publication(), Some(3));
    harness.teardown();
}

#[gpui::test]
fn a_hover_holds_the_page_awaiting_its_read_until_the_reply(cx: &mut TestAppContext) {
    // Goal: the app polls the worker quickly from the moment a hover needs a read until its
    // reply is taken, so the read goes out and the hovered second shows as soon as each can,
    // rather than at the next tick a quarter second later.
    // Method: hover a second only a read gives through the production controller, and ask
    // whether the page awaits a reply before the read is sent, while it is out, and once it
    // has arrived.
    let mut harness = ReaderHoverHarness::setup(cx);
    harness.driver.await_state(Await::Settled);

    harness.driver.preview_publication(2);

    assert!(harness.driver.awaiting_reply());
    harness.driver.submit_preview();
    assert!(harness.driver.awaiting_reply());
    harness.driver.await_preview();
    assert!(!harness.driver.awaiting_reply());
    harness.teardown();
}

struct ReaderHoverHarness {
    driver: ReaderHoverDriver,
    recording: SealedRecording,
}

impl ReaderHoverHarness {
    fn setup(cx: &mut TestAppContext) -> Self {
        let recording = SealedRecording::with_cpu_history();
        let worker = Worker::start(recording.root.clone()).unwrap();
        let window = cx.update(|cx| {
            cx.open_window(WindowOptions::default(), |_, cx| {
                crate::native_workflows::create_dashboard(worker, "cpu".into(), None, cx)
            })
            .unwrap()
        });
        let view = window.root(cx).unwrap();
        let context = VisualTestContext::from_window(window.into(), cx);
        let mut harness = Self {
            driver: ReaderHoverDriver { view, context },
            recording,
        };
        assert!(harness.recording.root.exists());
        harness.driver.await_state(Await::Initial);

        harness
    }

    fn teardown(self) {
        drop(self);
    }
}

impl Drop for ReaderHoverHarness {
    fn drop(&mut self) {
        self.driver.context.update(|window, cx| {
            self.driver
                .view
                .update(cx, |dashboard, _| dashboard.worker.stop());
            window.remove_window();
        });
        self.driver.context.run_until_parked();
    }
}

struct ReaderHoverDriver {
    view: Entity<LiveDashboard>,
    context: VisualTestContext,
}

#[derive(Clone, Copy)]
enum Await {
    Initial,
    /// The page holds the answer to its last request, its App histories included.
    Settled,
    /// The hover's read is out and its reply untaken, since a tick takes the worker's reply
    /// before it submits a hover's read.
    Submitted,
    Preview,
    FollowAfter(u64),
}

impl ReaderHoverDriver {
    fn preview_publication(&mut self, publication: u64) {
        let second = if publication == 1 { 1000 } else { 1001 };
        let identity = json!({"run":"00000000000000010000000000000002",
            "boot":"00000000000000030000000000000004","publication":publication});
        let target = Target::exact(second, &identity).unwrap();
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.set_reading_target("reader-contract".into(), target, Origin::Keyboard, cx);
        });
        self.context.run_until_parked();
    }

    fn preview_gap(&mut self) {
        self.view.update(&mut self.context, |dashboard, cx| {
            let target = Target::regular(dashboard.snapshot.as_deref().unwrap(), 1002);
            dashboard.set_reading_target("reader-contract".into(), target, Origin::Keyboard, cx);
        });
        self.context.run_until_parked();
    }

    fn submit_preview(&mut self) {
        self.await_state(Await::Submitted);
    }

    fn awaiting_reply(&self) -> bool {
        self.view
            .read_with(&self.context, |dashboard, _| dashboard.awaiting_reply())
    }

    fn await_preview(&mut self) {
        self.await_state(Await::Preview);
    }

    fn leave_preview_and_await_follow(&mut self) {
        let generation = self.view.read_with(&self.context, |dashboard, _| {
            dashboard.snapshot.as_ref().unwrap().generation
        });
        self.view.update(&mut self.context, |dashboard, cx| {
            dashboard.end_reading_preview(cx);
        });
        self.await_state(Await::FollowAfter(generation));
    }

    fn poll_once(&mut self) {
        self.view.update(&mut self.context, LiveDashboard::tick);
        self.context.run_until_parked();
    }

    fn await_state(&mut self, target: Await) {
        let deadline = Instant::now() + Duration::from_secs(10);
        for _attempt in 0..2000 {
            self.poll_once();
            let ready = self.view.read_with(&self.context, |dashboard, _| {
                assert!(dashboard.error.is_none(), "{:?}", dashboard.error);
                if let Some(preview) = &dashboard.preview {
                    assert!(preview.failure.is_none(), "{:?}", preview.failure);
                }
                match target {
                    Await::Initial => dashboard.snapshot.is_some(),
                    Await::Settled => settled(dashboard),
                    Await::Submitted => submitted(dashboard),
                    Await::Preview => dashboard
                        .preview
                        .as_ref()
                        .is_some_and(|preview| preview.complete),
                    Await::FollowAfter(generation) => dashboard
                        .snapshot
                        .as_ref()
                        .is_some_and(|snapshot| snapshot.generation > generation),
                }
            });
            if ready {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "Real hover reader exceeded ten seconds"
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        panic!("Real hover reader exceeded its bounded polls");
    }

    fn reading(&self) -> Reading {
        self.view.read_with(&self.context, |dashboard, _| {
            let snapshot = dashboard.reading_snapshot().unwrap();
            Reading {
                second: snapshot.selection["second"].as_u64().unwrap(),
                publication: snapshot.selection["selected"]["publication"].as_u64(),
                cpu_publication: snapshot
                    .cpu_detail
                    .as_ref()
                    .map(|detail| identity_signature(&detail.observation).unwrap().2),
                cpu_count: snapshot
                    .cpu_detail
                    .as_ref()
                    .map_or(0, |detail| detail.cpus.len()),
                page_publications: snapshot
                    .pages
                    .iter()
                    .map(|(name, page)| (name.clone(), page["observation"]["publication"].as_u64()))
                    .collect(),
            }
        })
    }

    fn committed_publication(&self) -> Option<u64> {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.snapshot.as_ref().unwrap().selection["selected"]["publication"].as_u64()
        })
    }

    fn follows_latest(&self) -> bool {
        self.view.read_with(&self.context, |dashboard, _| {
            dashboard.request.second.is_none()
        })
    }
}

fn settled(dashboard: &LiveDashboard) -> bool {
    let Some(snapshot) = &dashboard.snapshot else {
        return false;
    };

    if snapshot.generation == dashboard.generation {
        !snapshot.histories_pending
    } else {
        false
    }
}

fn submitted(dashboard: &LiveDashboard) -> bool {
    let preview = dashboard.preview.as_ref().unwrap();
    assert!(!preview.complete);

    preview.generation.is_some()
}

#[derive(Debug, PartialEq)]
struct Reading {
    second: u64,
    publication: Option<u64>,
    cpu_publication: Option<u64>,
    cpu_count: usize,
    page_publications: Vec<(String, Option<u64>)>,
}

impl Reading {
    fn publication(publication: u64) -> Self {
        Self {
            second: if publication == 1 { 1000 } else { 1001 },
            publication: Some(publication),
            cpu_publication: Some(publication),
            cpu_count: 35,
            page_publications: Vec::new(),
        }
    }
}
