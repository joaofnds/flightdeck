//! The saved recording and the lookups the tests of a window's values share.
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
    collector::recording::Registry {
        runs: Vec::new(),
        evictions: 0,
    }
    .write(&fixture.root)
    .unwrap();

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
