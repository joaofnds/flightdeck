//! The saved recording and the lookups the tests of a window's values share.
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::ObservationKey;
use crate::live::identity_fixture::Fixture;

/// Two sealed seconds of a recording whose registry lists no live run, so a session reads them
/// only from their saved minute.
pub fn saved_only() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    fixture.seal();
    forget_live_runs(&fixture.root);

    fixture
}

/// The two sealed seconds of `saved_only`, sealed with what the persister keeps of their Apps.
pub fn saved_with_apps() -> Fixture {
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    fixture.seal_derived(&[], &crate::SealedApps::default());
    forget_live_runs(&fixture.root);

    fixture
}

/// The key a window holds the observation a session response names as selected under.
pub fn observation_key(selected: &Value) -> ObservationKey {
    ObservationKey {
        run: collector::recording::parse_identity(selected["run"].as_str().unwrap())
            .unwrap()
            .0,
        boot: collector::recording::parse_identity(selected["boot"].as_str().unwrap())
            .unwrap()
            .0,
        publication: selected["publication"].as_u64().unwrap(),
    }
}

/// Four sealed seconds in two saved minutes of a recording whose registry lists no live run,
/// and the saved tables of the earlier minute, which holds 1000 and 1001 where the later one
/// holds 1060 and 1061.
pub fn saved_in_two_minutes() -> (Fixture, Vec<PathBuf>) {
    let mut fixture = Fixture::new();
    fixture.publish_with_system_rows(1000);
    fixture.publish_with_system_rows(1001);
    fixture.seal_minute(0);
    let earlier = saved_tables(&fixture.root);
    fixture.skip_run_ticks(58);
    fixture.publish_with_system_rows(1060);
    fixture.publish_with_system_rows(1061);
    fixture.seal_minute(1);
    forget_live_runs(&fixture.root);

    (fixture, earlier)
}

/// Overwrites these saved tables in place, so a later read of their minute fails.
pub fn damage(tables: &[PathBuf]) {
    assert!(!tables.is_empty());
    for table in tables {
        std::fs::write(table, b"damaged").unwrap();
    }
}

/// Rewrites the recording's registry with no live run, so a session reads its seconds only
/// from their saved minutes.
fn forget_live_runs(root: &Path) {
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(root)
    .unwrap();
}

fn saved_tables(root: &Path) -> Vec<PathBuf> {
    let mut tables = Vec::new();
    let mut directories = vec![root.to_path_buf()];
    while let Some(directory) = directories.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                directories.push(path);
            } else {
                if path
                    .extension()
                    .is_some_and(|extension| extension == "parquet")
                {
                    tables.push(path);
                }
            }
        }
    }

    tables
}
