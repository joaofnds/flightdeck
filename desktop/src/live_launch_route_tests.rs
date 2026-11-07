//! Every page the app registers, read through the live launch.

use persistence::work::ingestion::{Event, Kind, Producer};
use serde_json::{Value, json};

use super::{Harness, Input, SourceLayout, count, request, source};
use crate::live_dashboard::ROUTES;
use crate::live_source::{Request, Snapshot, Update, Worker};

#[test]
fn no_page_takes_in_a_publication_the_store_took_in() {
    // Goal: whichever page the window reads, each publication has one accepted machine reduction
    // and one App aggregation, the Store's. Method: read every page the app registers through
    // the real constructor, and count each kind of work per producer.
    let mut harness = None;
    Harness::setup(Input::Sealed(SourceLayout::Identity), &mut harness);
    let mut harness = harness.unwrap();

    for (route, _) in ROUTES {
        harness.read(route);
    }
    harness.teardown();

    let events = harness.events();
    assert_worker_took_in_nothing(&events);
    for kind in [Kind::MachineReduction, Kind::AppDerivation, Kind::Accepted] {
        assert_eq!(count(&events, Producer::Store, kind, 2), 1, "{kind:?}");
    }
}

#[test]
fn a_second_the_store_leaves_to_the_saved_minutes_takes_in_no_publication() {
    // Goal: the reader that answers a second the Store does not hold leaves the live Mac's
    // readings to the Store. Method: hold a second before any reading on the page of a CPU chart,
    // which the Store leaves to the saved minutes, and count the work of the reader that answers.
    let mut harness = None;
    Harness::setup(Input::Sealed(SourceLayout::Identity), &mut harness);
    let mut harness = harness.unwrap();
    harness.read_publication("overview", 2);
    let mut held = request("cpu");
    held.second = Some(source::SECOND_FIRST - 3_600);

    let answer = harness.read_request(held);
    harness.teardown();

    assert!(answer.unwrap().selection["selected"].is_null());
    assert_worker_took_in_nothing(&harness.events());
}

#[test]
fn every_page_of_the_newest_second_shows_what_the_saved_route_shows() {
    // Goal: moving a page onto the Store changes nothing the window shows or copies. Method: read
    // every page the app registers through the live launch and through a saved recording's
    // reader over the same recording, and compare what each page draws and copies.
    let mut pages = Pages::new();

    for (route, _) in ROUTES {
        let live = pages.live_newest(route);
        let saved = pages.saved(request(route), Worker::request);

        assert_eq!(
            shown(route, &request(route), live),
            shown(route, &request(route), saved),
            "{route}"
        );
    }
}

#[test]
fn every_page_past_its_first_rows_shows_what_the_saved_route_shows() {
    // Goal: paging reads the same rows, whichever route answers. Method: read every page the app
    // registers past the first row of each of its tables through both readers, and compare what
    // each page draws and copies.
    let mut pages = Pages::new();

    for (route, _) in ROUTES {
        let mut past_first = request(route);
        past_first.apps_offset = 1;
        past_first.members_offset = 1;
        past_first.cpu_offset = 32;
        let live = pages.live_newest_of(past_first.clone());
        let saved = pages.saved(past_first.clone(), Worker::request);

        assert_eq!(
            shown(route, &past_first, live),
            shown(route, &past_first, saved),
            "{route}"
        );
    }
}

#[test]
fn every_page_of_a_held_second_shows_what_the_saved_route_shows() {
    // Goal: a hold reads the same second on every page, whichever route answers it. Method: read
    // the first second, by its identity, on every page through both readers, and compare what
    // each page draws and copies.
    let mut pages = Pages::new();

    for route in routes_of_a_held_second() {
        let held = pages.held_request(route);
        let live = pages.harness.read_request(held.clone()).unwrap();
        let saved = pages.saved(held.clone(), Worker::request);

        assert_eq!(
            shown(route, &held, live),
            shown(route, &held, saved),
            "{route}"
        );
    }
}

#[test]
fn every_page_hovering_a_second_shows_what_the_saved_route_shows() {
    // Goal: a hover reads the same second on every page, whichever route answers it. Method:
    // read every page at its newest second, then the first second as a hover does, keeping the
    // page's context, through both readers, and compare what each page draws and copies.
    let mut pages = Pages::new();

    for route in routes_of_a_held_second() {
        let mut hover = pages.held_request(route);
        pages.live_newest(route);
        pages.saved(request(route), Worker::request);
        hover.load_resident_history = false;
        hover.load_app_attribution_history = false;
        hover.load_process_history = false;
        hover.earlier = crate::live_source::Earlier::Skip;
        let live = pages.harness.inspect_request(hover.clone()).unwrap();
        let saved = pages.saved(hover.clone(), Worker::inspect);

        assert_eq!(
            shown(route, &hover, live),
            shown(route, &hover, saved),
            "{route}"
        );
    }
}

#[test]
fn the_computer_page_copies_the_second_a_hold_names() {
    // Goal: the Computer page copies the held second, as every other page does, where the saved
    // route has only the recording's newest reading and so no copy to give. Method: hold the
    // first second, read the Computer page through the live launch, and read what it copies.
    let mut pages = Pages::new();
    let held = pages.held_request("computer");

    let live = pages.harness.read_request(held.clone()).unwrap();

    assert_eq!(copied_second(&live, &held), source::SECOND_FIRST);
}

#[test]
fn the_computer_page_copies_a_held_second_no_identity_names() {
    // Goal: a second held from a chart, which names no exact reading, reads on the Computer page
    // as on every other. Method: hold the first second without an identity, read the Computer
    // page through the live launch, and read what it copies.
    let mut pages = Pages::new();
    let mut held = pages.held_request("computer");
    held.identity = None;

    let live = pages.harness.read_request(held.clone()).unwrap();

    assert_eq!(copied_second(&live, &held), source::SECOND_FIRST);
}

#[test]
fn the_self_page_waits_for_no_second_a_hold_names() {
    // Goal: a hold on a second with no readings refuses no page that shows none of its own, and
    // Self-monitoring shows only the app's figures. Method: hold a second before any reading,
    // and read the Self-monitoring page through the live launch.
    let mut pages = Pages::new();
    let mut held = pages.held_request("self");
    held.second = Some(source::SECOND_FIRST - 3_600);
    held.identity = None;

    let live = pages.harness.read_request(held);

    assert!(live.is_ok(), "{:?}", live.err());
}

#[test]
fn the_computer_page_copies_a_held_second_older_than_the_store_holds() {
    // Goal: a hold reads on the Computer page as on every other once the Store's span has left it
    // behind. Method: hold the older of two minutes further apart than the Store's span, read the
    // Computer page through a Store's worker, and read what it copies.
    let mut pages = OlderPages::new();
    let held = OlderPages::held("computer");

    let live = finished_page(&mut pages.live, held.clone());

    assert_eq!(copied_second(&live, &held), OlderPages::SECOND);
}

#[test]
fn every_page_of_a_second_older_than_the_store_holds_shows_what_the_saved_route_shows() {
    // Goal: a second the Store leaves to the saved minutes reads as the saved route reads it.
    // Method: hold the older of two minutes further apart than the Store's span, read every page
    // through a Store's worker and through a saved recording's reader, and compare.
    let mut pages = OlderPages::new();

    for route in routes_of_a_held_second() {
        let held = OlderPages::held(route);

        let live = finished_page(&mut pages.live, held.clone());
        let saved = finished_page(&mut pages.saved, held.clone());

        assert_eq!(
            shown(route, &held, live),
            shown(route, &held, saved),
            "{route}"
        );
    }
}

/// Refuses any reading of the ring by the worker, whatever it was made for: one made for a page's
/// older tail still takes in what the Store took in.
fn assert_worker_took_in_nothing(events: &[Event]) {
    for kind in [
        Kind::MachineReduction,
        Kind::AppDerivation,
        Kind::Accepted,
        Kind::RingReadAttempt,
        Kind::RingRead,
        Kind::FrameCaptureAttempt,
        Kind::FrameCapture,
        Kind::RawCaptureAttempt,
        Kind::RawCapture,
        Kind::NativeCollect,
    ] {
        let taken = events
            .iter()
            .filter(|event| event.producer == Producer::Worker)
            .filter(|event| event.kind == kind)
            .count();
        assert_eq!(taken, 0, "{kind:?}");
    }
}

fn copied_second(snapshot: &Snapshot, request: &Request) -> Value {
    let copied = crate::native_workflows::selected_copy(Some(snapshot), request, None).unwrap();
    let copied: Value = serde_json::from_str(&copied).unwrap();

    copied["second"].clone()
}

/// Every page the app registers but Computer, which the test of its own holds apart.
fn routes_of_a_held_second() -> impl Iterator<Item = &'static str> {
    ROUTES
        .iter()
        .map(|(route, _)| *route)
        .filter(|route| *route != "computer")
}

/// A recording read through the live launch and through a saved recording's reader.
struct Pages {
    harness: Harness,
    saved: Worker,
}

impl Pages {
    fn new() -> Self {
        let mut harness = None;
        Harness::setup(Input::Sealed(SourceLayout::Identity), &mut harness);
        let harness = harness.unwrap();
        let saved = Worker::start_recorded(harness.root.clone()).unwrap();

        Self { harness, saved }
    }

    /// The live launch's page of the newest second, which the Store takes in on its own thread.
    fn live_newest(&mut self, route: &str) -> Box<Snapshot> {
        self.harness.read_publication("overview", 2);
        self.harness.read(route)
    }

    fn live_newest_of(&mut self, request: Request) -> Box<Snapshot> {
        self.harness.read_publication("overview", 2);
        self.harness.read_request(request).unwrap()
    }

    fn held_request(&mut self, route: &str) -> Request {
        let latest = self.harness.read_publication("overview", 2);
        let identity =
            crate::live_projection::inspection_identity(&latest.selection, source::SECOND_FIRST)
                .unwrap();
        let mut held = request(route);
        held.second = Some(source::SECOND_FIRST);
        held.identity = Some(identity);

        held
    }

    fn saved(
        &mut self,
        request: Request,
        submit: fn(&mut Worker, Request) -> Result<u64, String>,
    ) -> Box<Snapshot> {
        let generation = submit(&mut self.saved, request).unwrap();
        for _step in 0..2_000 {
            match self.saved.take().unwrap() {
                Some(Update::Ready(snapshot)) => {
                    assert_eq!(snapshot.generation, generation);
                    if self.saved.last_output_complete() {
                        return snapshot;
                    }
                }
                Some(Update::Failed { message, .. }) => panic!("{message}"),
                None => {}
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the saved reader never finished its page");
    }
}

/// Two saved minutes further apart than the Store's span, read through a Store's worker and through
/// a saved recording's reader.
struct OlderPages {
    live: Worker,
    saved: Worker,
    store: Option<viewer::store::Store>,
    /// Kept until the readers have stopped, since dropping it deletes the recording.
    _recording: crate::live_source::tests::SealedRecording,
}

impl OlderPages {
    const SECOND: u64 = 1000;

    fn new() -> Self {
        let recording =
            crate::live_source::tests::SealedRecording::with_a_minute_past_the_store_hour();
        let budget = std::sync::Arc::new(viewer::store::ByteBudget::new(256 * 1024 * 1024));
        let store = viewer::store::Store::start_with_budget(&recording.root, budget).unwrap();
        let live = Worker::start_from_store(recording.root.clone(), store.query_handle().unwrap())
            .unwrap();
        let saved = Worker::start_recorded(recording.root.clone()).unwrap();

        Self {
            live,
            saved,
            store: Some(store),
            _recording: recording,
        }
    }

    fn held(route: &str) -> Request {
        let mut held = request(route);
        held.second = Some(Self::SECOND);

        held
    }
}

impl Drop for OlderPages {
    fn drop(&mut self) {
        self.live.stop();
        self.saved.stop();
        drop(self.store.take());
    }
}

fn finished_page(worker: &mut Worker, request: Request) -> Box<Snapshot> {
    let generation = worker.request(request).unwrap();
    for _step in 0..4_000 {
        match worker.take().unwrap() {
            Some(Update::Ready(snapshot)) => {
                assert_eq!(snapshot.generation, generation);
                if worker.last_output_complete() {
                    return snapshot;
                }
            }
            Some(Update::Failed { message, .. }) => panic!("{message}"),
            None => {}
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    panic!("the reader never finished its page");
}

/// What the window shows of `snapshot`: the panels of its page, its tables, histories and rankings
/// and what it copies. A reading names who counted its losses, the minute that holds it on one
/// route and the ring on the other, so the name is left out of the comparison and the count stays
/// in it. A history's coverage counts each route's own cache bytes, which the window draws nothing
/// from, so of its fields the comparison keeps only those the window reads.
fn shown(route: &str, request: &Request, snapshot: Box<Snapshot>) -> Value {
    let mut effective = request.clone();
    crate::live_acceptance::normalize_offsets(&mut effective, &snapshot).unwrap();
    let ranks = super::output::window_values(&snapshot);
    let process_history = super::output::process_history(&snapshot);
    let cpu_history = snapshot
        .cpu_history
        .as_ref()
        .map(|history| format!("{history:?}"));
    let rankings = snapshot
        .rankings
        .as_ref()
        .map(|rankings| format!("{rankings:?}"));
    let tables = json!({"apps": snapshot.apps, "detail": snapshot.detail,
        "history": snapshot.history, "cpu_detail": snapshot.cpu_detail});
    let presentation = crate::live_acceptance::Presentation::prepare(&effective, snapshot).unwrap();
    let copy = crate::native_workflows::selected_copy(
        Some(&presentation.snapshot),
        &presentation.request,
        None,
    );
    let copied = copy.map(|text| serde_json::from_str::<Value>(&text).unwrap());

    let mut shown = json!({"route": route, "tables": tables, "ranks": ranks, "rankings": rankings,
        "process_history": process_history, "cpu_history": cpu_history,
        "panels": presentation.panels.iter().map(|panel| json!(panel)).collect::<Vec<_>>(),
        "copy": copied});
    shown_comparable(&mut shown);

    shown
}

fn shown_comparable(shown: &mut Value) {
    let mut values = vec![shown];
    while let Some(value) = values.pop() {
        match value {
            Value::Object(fields) => {
                if fields.contains_key("window_losses") {
                    let window = fields.remove("window_losses").unwrap();
                    let reader = fields.remove("reader_losses").unwrap_or(Value::Null);
                    let losses = if window.is_null() { reader } else { window };
                    fields.insert("losses".to_owned(), losses);
                }
                if let Some(Value::Object(coverage)) = fields.get_mut("coverage") {
                    coverage.retain(|name, _| {
                        matches!(name.as_str(), "retention_limited" | "observations_retained")
                    });
                }
                values.extend(fields.values_mut());
            }
            Value::Array(items) => values.extend(items.iter_mut()),
            _ => {}
        }
    }
}
