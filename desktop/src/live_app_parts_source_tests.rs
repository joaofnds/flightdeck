use serde_json::Value;

use super::{SealedRecording, ranked_identity, ranked_request_at};
use crate::live_hover::{Preview, Target};
use crate::live_source::{AppKey, Request, Snapshot, app_key_bytes_hex};

#[test]
fn hover_preview_on_the_apps_page_shows_a_saved_sibling_as_the_worker_reads_it() {
    // Goal: a hover on the Apps page shows another observation's App at once, as the worker
    // reads it.
    // Method: preview publication two over a committed second Apps page at publication three
    // of 600 saved Apps, and read publication two through the worker.
    let recording = SealedRecording::with_ranked_apps();
    let request = apps_request_at(3, "App00600", 512);
    let committed = recording.read(request.clone()).unwrap();
    let worker = recording.read(apps_request_at(2, "App00600", 512)).unwrap();
    assert!(worker.detail.as_ref().unwrap()["app"].is_object());
    let target = Target::exact(1001, &ranked_identity(2)).unwrap();

    let preview = Preview::resident(&committed, &request, target);

    assert!(preview.complete);
    assert_eq!(preview.reading.detail, worker.detail);
    assert_eq!(preview.reading.apps.as_deref(), Some(&apps_paging(&worker)));
    assert_eq!(
        preview.executables,
        crate::live_executables::groups(worker.detail.as_deref()).unwrap()
    );
}

#[test]
fn hover_preview_from_a_kept_app_part_bounds_its_allocations() {
    // Goal: a hover on the Apps page shows the selected App from its kept part in a bounded
    // number of allocations, so a change that makes the frame of a pointer move allocate more
    // fails here.
    // Method: select App00600 on a committed Apps page at publication three of 600 saved Apps,
    // preview publication two, and count the allocations of the preview alone.
    let recording = SealedRecording::with_ranked_apps();
    let request = apps_request_at(3, "App00600", 512);
    let committed = recording.read(request.clone()).unwrap();
    let target = Target::exact(1001, &ranked_identity(2)).unwrap();
    let started = std::time::Instant::now();

    let (preview, counts) =
        crate::allocation_tests::measure(|| Preview::resident(&committed, &request, target));

    println!(
        "{}",
        serde_json::json!({"case":"apps_kept_part_preview", "allocation_calls":counts.calls,
        "requested_bytes":counts.bytes, "elapsed_ns":started.elapsed().as_nanos()})
    );
    assert!(preview.complete);
    // About a quarter above the 1037 calls measured when this test was written.
    assert!(counts.calls <= 1_300, "{counts:?}");
}

#[test]
fn hover_preview_on_the_apps_page_moves_past_the_end_pages_to_the_first_as_the_worker_does() {
    // Goal: a hover pages a second with fewer Apps or processes as a read of it does.
    // Method: preview a saved sibling with offsets past the end of its Apps and its App's
    // processes, and read it through the worker with the same offsets.
    let recording = SealedRecording::with_ranked_apps();
    let mut request = apps_request_at(3, "App00600", 1024);
    request.members_offset = 512;
    let committed = recording.read(request.clone()).unwrap();
    let mut exact = apps_request_at(2, "App00600", 1024);
    exact.members_offset = 512;
    let worker = recording.read(exact).unwrap();
    let target = Target::exact(1001, &ranked_identity(2)).unwrap();

    let preview = Preview::resident(&committed, &request, target);

    assert_eq!(preview.reading.detail, worker.detail);
    assert_eq!(preview.reading.apps.as_deref(), Some(&apps_paging(&worker)));
    assert_eq!(preview.request.apps_offset, 0);
    assert_eq!(preview.request.members_offset, 0);
}

#[test]
fn hover_preview_on_the_apps_page_shows_an_app_absent_from_its_second_as_the_worker_does() {
    // Goal: a hover on a second the selected App did not run in shows it absent at once, as a
    // read of that second does.
    // Method: keep the parts of an App no saved second holds, past the end of its processes,
    // then preview a sibling and read it through the worker.
    let recording = SealedRecording::with_ranked_apps();
    let mut request = apps_request_at(3, "Absent", 0);
    request.members_offset = 512;
    let committed = recording.read(request.clone()).unwrap();
    let mut exact = apps_request_at(2, "Absent", 0);
    exact.members_offset = 512;
    let worker = recording.read(exact).unwrap();
    let target = Target::exact(1001, &ranked_identity(2)).unwrap();

    let preview = Preview::resident(&committed, &request, target);

    assert_eq!(worker.detail.as_deref(), Some(&Value::Null));
    assert!(preview.complete);
    assert_eq!(preview.reading.detail, worker.detail);
}

#[test]
fn hover_preview_on_the_apps_page_reads_a_second_for_an_app_its_parts_are_not_of() {
    // Goal: a hover never shows the parts of one App as another App's.
    // Method: keep one App's parts, then preview a sibling with another App selected.
    let recording = SealedRecording::with_ranked_apps();
    let committed = recording.read(apps_request_at(3, "App00600", 0)).unwrap();
    let request = apps_request_at(3, "App00599", 0);
    let target = Target::exact(1001, &ranked_identity(2)).unwrap();

    let preview = Preview::resident(&committed, &request, target);

    assert!(preview.reading.detail.is_none());
    assert!(!preview.complete);
}

#[test]
fn hover_preview_with_the_app_picker_open_reads_its_second() {
    // Goal: the open App picker lists the hovered second's Apps, which only a read gives.
    // Method: preview a saved sibling whose App parts are kept, with the App picker open.
    let recording = SealedRecording::with_ranked_apps();
    let request = apps_request_at(3, "App00600", 0);
    let committed = recording.read(request.clone()).unwrap();
    let target = Target::exact(1001, &ranked_identity(2)).unwrap();
    let mut preview = Preview::resident(&committed, &request, target);
    assert!(preview.complete);

    preview.list_apps(&committed);

    assert!(!preview.complete);
}

#[test]
fn hover_reads_on_the_apps_page_keep_no_app_parts() {
    // Goal: a read for a hovered observation never waits on keeping its App's whole window.
    // Method: read a saved observation of the Apps page the way a hover does.
    let recording = SealedRecording::with_ranked_apps();
    let mut request = apps_request_at(2, "App00600", 0);
    request.load_resident_history = false;
    request.load_app_attribution_history = false;
    request.load_process_history = false;

    let snapshot = recording.read(request).unwrap();

    assert!(snapshot.detail.is_some());
    assert!(snapshot.app_parts.is_none());
}

/// The parts a page keeps of the App `app` names in the window of a sealed recording whose
/// observations share the run and boot of the input tests' snapshots.
pub(crate) fn kept_window_parts(app: &str) -> std::sync::Arc<crate::live_source::PartsWindow> {
    let recording = SealedRecording::with_ranked_apps();

    let snapshot = recording.read(apps_request_at(3, app, 0)).unwrap();

    snapshot.app_parts.unwrap()
}

fn apps_request_at(publication: u64, app: &str, apps_offset: u32) -> Request {
    let mut request = ranked_request_at("apps", publication);
    request.app = Some(AppKey {
        kind: "executable".into(),
        bytes_hex: app_key_bytes_hex(app.as_bytes()),
    });
    request.apps_offset = apps_offset;

    request
}

/// The worker's Apps page without the Apps it lists, which a hover's Apps page leaves out.
fn apps_paging(worker: &Snapshot) -> Value {
    let mut apps = worker.apps.as_deref().unwrap().clone();
    apps.as_object_mut().unwrap().remove("apps").unwrap();

    apps
}
