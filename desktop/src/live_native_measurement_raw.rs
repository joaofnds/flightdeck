use std::path::Path;

use serde_json::{Value, json};

use super::{SnapshotReader, emit, payloads, read_complete};

const TABLES: [&str; 11] = [
    "ticks",
    "cpus",
    "memory",
    "interfaces",
    "gpu_states",
    "core_states",
    "fans",
    "processes",
    "coalitions",
    "events",
    "core_level_states",
];
const PAGES_MAX: u64 = 128;

pub(super) fn run(root: &Path, route: &str, expected: &Value) {
    let mut reader = SnapshotReader::new();
    reader.command.mode = crate::live_source_mode::SourceMode::Live;
    reader.command.request.route = route.to_owned();
    let (snapshot, _) = read_complete(&mut reader, root);
    payloads::validate(&snapshot, route, expected).unwrap();
    let session = reader.reader.session.as_ref().unwrap();
    let pages = pages(session);
    emit(
        &json!({"route": route, "phase": "raw", "selected": snapshot.selection["selected"],
        "summary": snapshot.selection["summary"], "pages": pages}),
    );
}

fn pages(session: &viewer::LiveSession) -> Value {
    let mut tables = serde_json::Map::new();
    for table in TABLES {
        let mut pages = Vec::new();
        for page in 0..PAGES_MAX {
            let value = session.page(table, page * 512).unwrap();
            let rows = value["rows"].as_array().unwrap();
            assert!(rows.len() <= 512);
            let total = value["total"].as_u64().unwrap();
            pages.push(value);
            if (page + 1) * 512 >= total {
                break;
            }
        }
        assert_eq!(
            pages
                .iter()
                .map(|page| page["rows"].as_array().unwrap().len())
                .sum::<usize>(),
            usize::try_from(pages[0]["total"].as_u64().unwrap()).unwrap()
        );
        tables.insert(table.to_owned(), json!(pages));
    }
    json!(tables)
}

#[test]
fn exports_every_raw_cell_and_page_from_the_existing_native_fixture() {
    // Goal: the comparison artifact includes all eleven on-demand raw table surfaces.
    // Method: request completed Network and enumerate raw pages from that same real session.
    let mut fixture = crate::live_dashboard::input_tests::source_mode::LiveRecording::new();
    fixture.publish_resources();
    fixture.publish_resources();
    let mut reader = SnapshotReader::new();
    reader.command.mode = crate::live_source_mode::SourceMode::Live;
    reader.command.request.route = "network".into();
    let (snapshot, _) = read_complete(&mut reader, &fixture.root);

    let output = pages(reader.reader.session.as_ref().unwrap());

    assert_eq!(output.as_object().unwrap().len(), 11);
    assert_eq!(output["fans"][0]["rows"][0][2], "1234");
    assert_eq!(
        output["memory"][0]["observation"],
        snapshot.selection["selected"]
    );
}
