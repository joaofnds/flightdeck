use std::collections::BTreeMap;
use std::time::Instant;

use serde_json::{Value, json};

use crate::allocation_tests::measure;
use crate::live_hover::Target;
use crate::live_source::Snapshot;

const RUN: &str = "00000000000000010000000000000002";
const BOOT: &str = "00000000000000030000000000000004";

macro_rules! hover_case {
    ($name:ident, $publication:expr, $expected:expr) => {
        #[test]
        fn $name() {
            // Measure production chart lookup over a full window, outside fixture setup.
            let snapshot = snapshot();
            let target = Target::exact(1000, &identity($publication)).unwrap();
            let panels = crate::live_projection::panels("overview", &snapshot.selection, None)
                .unwrap();
            let panel = &panels[0];
            let started = Instant::now();

            let (indices, counts) = measure(|| {
                std::array::from_fn::<_, 3, _>(|_| super::target_index(panel, &snapshot, &target))
            });

            assert_eq!(indices, [$expected; 3]);
            println!("{}", json!({"case":stringify!($name), "indices":indices,
                "allocation_calls":counts.calls,"requested_bytes":counts.bytes,
                "elapsed_ns":started.elapsed().as_nanos()}));
            assert!(counts.calls <= 12, "{counts:?}");
        }
    };
}

hover_case!(earliest_hover_bounds_identity_allocations, 1, Some(0));
hover_case!(middle_hover_bounds_identity_allocations, 450, Some(449));
hover_case!(latest_hover_bounds_identity_allocations, 900, Some(899));
hover_case!(missing_hover_bounds_identity_allocations, 901, None);

fn identity(publication: u64) -> Value {
    json!({"run":RUN,"boot":BOOT,"publication":publication})
}

fn snapshot() -> Snapshot {
    let points: Vec<_> = (1..=900_u64)
        .map(|publication| {
            json!({"run":RUN,"boot":BOOT,"publication":publication,"segment":0,
                "wall_ns":(1000+publication)*1_000_000_000,"cpu_percent":20.0})
        })
        .collect();

    Snapshot {
        generation: 1,
        catalog: Value::Null,
        selection: json!({"second":1900,"selected":identity(900),"trends":points}),
        pages: BTreeMap::new(),
        apps: None,
        detail: None,
        history: None,
        cpu_detail: None,
        cpu_history: None,
        resident_history: None,
        app_attribution_history: None,
        rankings: None,
        read_ms: 0.0,
    }
}
