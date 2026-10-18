use std::ffi::OsStr;

use serde_json::{Value, json};

use super::{decode, iterations};

#[path = "command_tests.rs"]
mod command_tests;

#[test]
fn preserves_all_five_app_panels_and_recorded_gaps() {
    // Goal: benchmark populated App histories without replacing their gaps or identity.
    // Method: decode an envelope and project its real production route into all five panels.
    let envelope = fixture();
    let decoded = decode(&serde_json::to_vec(&envelope).unwrap())
        .unwrap()
        .value;

    let panels =
        crate::live_projection::panels("apps", &decoded["selection"], Some(&decoded["history"]))
            .unwrap();
    let bits = crate::projection_bench::bits(&panels);

    assert_eq!(decoded, envelope);
    assert_eq!(bits.as_array().unwrap().len(), 5);
    assert_eq!(bits[0]["id"], "live-app-cpu");
    assert_eq!(bits[1]["id"], "live-app-resident");
    assert_eq!(bits[2]["id"], "live-app-footprint");
    assert_eq!(bits[3]["id"], "live-app-power");
    assert_eq!(bits[4]["id"], "live-app-disk");
    assert_eq!(bits[4]["selected_index"], 1);
    assert_eq!(bits[4]["segments"], json!([0, 1]));
    assert_eq!(bits[4]["lines"][0]["values"][0], Value::Null);
    assert_eq!(bits[4]["lines"][0]["values"][1], 0.002_048_f32.to_bits());
    assert_eq!(bits[4]["lines"][1]["values"][1], 0.000_512_f32.to_bits());
}

macro_rules! rejects_envelope {
    ($name:ident, $path:expr, $value:expr) => {
        #[test]
        fn $name() {
            // Goal: refuse a benchmark input that violates its declared workload contract.
            // Method: alter one field of a coherent envelope before decoding it.
            let mut envelope = fixture();
            *envelope.pointer_mut($path).unwrap() = $value;

            let result = decode(&serde_json::to_vec(&envelope).unwrap());

            assert!(result.is_err());
        }
    };
}

rejects_envelope!(
    rejects_history_for_a_different_selected_observation,
    "/history/selected/stamp/publication",
    json!("3")
);
rejects_envelope!(
    rejects_history_for_a_different_boot,
    "/history/selected/stamp/boot",
    json!(["3", "5"])
);
rejects_envelope!(
    rejects_history_for_a_different_wall_time,
    "/history/selected/wall_ns",
    json!("1000000000001")
);
rejects_envelope!(
    rejects_history_without_its_selected_point,
    "/history/points/1/observation/stamp/publication",
    json!("3")
);
rejects_envelope!(rejects_a_scalar_route, "/route", json!("overview"));
rejects_envelope!(
    rejects_an_unknown_envelope_version,
    "/schema_version",
    json!(2)
);
rejects_envelope!(
    rejects_missing_history_points,
    "/history/points",
    Value::Null
);
rejects_envelope!(rejects_an_empty_history, "/history/points", json!([]));
rejects_envelope!(rejects_an_unknown_app_kind, "/app/kind", json!("coalition"));
rejects_envelope!(rejects_an_incomplete_app_byte, "/app/bytes_hex", json!("a"));
rejects_envelope!(
    rejects_a_nonhexadecimal_app_byte,
    "/app/bytes_hex",
    json!("zz")
);

macro_rules! rejects_paired_selected_metadata {
    ($name:ident, $path:expr, $value:expr) => {
        #[test]
        fn $name() {
            // Goal: matching malformed metadata cannot impersonate a valid public observation.
            // Method: corrupt selected metadata and keep its matching point exactly identical.
            let mut envelope = fixture();
            *envelope["history"]["selected"].pointer_mut($path).unwrap() = $value;
            envelope["history"]["points"][1]["observation"] =
                envelope["history"]["selected"].clone();

            let result = decode(&serde_json::to_vec(&envelope).unwrap());

            assert!(result.is_err());
        }
    };
}

rejects_paired_selected_metadata!(
    rejects_paired_invalid_recording_metadata,
    "/recording",
    json!("corrupt")
);
rejects_paired_selected_metadata!(
    rejects_paired_invalid_continuous_clock_metadata,
    "/stamp/continuous_ns",
    json!("corrupt")
);
rejects_paired_selected_metadata!(
    rejects_paired_invalid_awake_clock_metadata,
    "/stamp/awake_ns",
    json!("corrupt")
);

macro_rules! rejects_paired_missing_clock {
    ($name:ident, $field:literal) => {
        #[test]
        fn $name() {
            // Goal: an absent optional clock field stays distinct from an explicit null clock.
            // Method: remove the same clock from selected metadata and its matching point.
            let mut envelope = fixture();
            assert!(
                envelope["history"]["selected"]["stamp"]
                    .as_object_mut()
                    .unwrap()
                    .remove($field)
                    .is_some()
            );
            envelope["history"]["points"][1]["observation"] =
                envelope["history"]["selected"].clone();

            let result = decode(&serde_json::to_vec(&envelope).unwrap());

            assert!(result.is_err());
        }
    };
}

rejects_paired_missing_clock!(rejects_paired_missing_continuous_clock, "continuous_ns");
rejects_paired_missing_clock!(rejects_paired_missing_awake_clock, "awake_ns");
rejects_envelope!(
    rejects_invalid_recording_on_a_nonselected_history_observation,
    "/history/points/0/observation/recording",
    json!("corrupt")
);
rejects_envelope!(
    rejects_invalid_run_tick_on_a_nonselected_history_observation,
    "/history/points/0/observation/run_tick",
    json!("invalid")
);

#[test]
fn rejects_paired_missing_recording_metadata() {
    // Goal: identical omissions cannot impersonate a complete public observation.
    // Method: remove recording from selected metadata and keep its matching point identical.
    let mut envelope = fixture();
    assert!(
        envelope["history"]["selected"]
            .as_object_mut()
            .unwrap()
            .remove("recording")
            .is_some()
    );
    envelope["history"]["points"][1]["observation"] = envelope["history"]["selected"].clone();

    let result = decode(&serde_json::to_vec(&envelope).unwrap());

    assert!(result.is_err());
}

rejects_envelope!(
    rejects_selected_point_clock_disagreement_without_identity_change,
    "/history/points/1/observation/stamp/awake_ns",
    json!("800000000")
);

#[test]
fn accepts_explicit_null_optional_clocks_on_every_history_observation() {
    // Goal: valid missing-clock readings remain admissible without inventing timestamp values.
    // Method: retain explicit nulls on all observations and identical selected-point metadata.
    let mut envelope = fixture();
    envelope["history"]["selected"]["stamp"]["continuous_ns"] = Value::Null;
    envelope["history"]["selected"]["stamp"]["awake_ns"] = Value::Null;
    envelope["selection"]["trends"][0]["continuous_ns"] = Value::Null;
    envelope["history"]["points"][0]["observation"]["stamp"]["continuous_ns"] = Value::Null;
    envelope["history"]["points"][0]["observation"]["stamp"]["awake_ns"] = Value::Null;
    envelope["history"]["points"][1]["observation"] = envelope["history"]["selected"].clone();
    unavailable_interval(&mut envelope["history"]["points"][1]);

    let decoded = decode(&serde_json::to_vec(&envelope).unwrap())
        .unwrap()
        .value;

    assert_eq!(decoded, envelope);
}

fn unavailable_interval(point: &mut Value) {
    for field in [
        "cpu_ns",
        "energy_nj",
        "disk_read_bytes",
        "disk_written_bytes",
        "disk_io_bytes",
    ] {
        point[field] = json!({"known":"0","measured_members":0,
            "unavailable_members":1,"overflowed":false});
    }
    for field in [
        "elapsed_ns",
        "cpu_percent_one_core",
        "energy_joules",
        "energy_watts",
        "disk_read_bps",
        "disk_written_bps",
        "disk_io_bps",
    ] {
        point[field] = Value::Null;
    }
}

#[test]
fn accepts_full_unsigned_decimal_recording_words() {
    // Goal: recording identity remains two exact unsigned words, including values above 2^53.
    // Method: use full-width decimal words on every retained full observation.
    let mut envelope = fixture();
    let recording = json!(["18446744073709551615", "9007199254740993"]);
    envelope["history"]["selected"]["recording"] = recording.clone();
    envelope["history"]["points"][0]["observation"]["recording"] = recording.clone();
    envelope["history"]["points"][1]["observation"]["recording"] = recording;

    let decoded = decode(&serde_json::to_vec(&envelope).unwrap())
        .unwrap()
        .value;

    assert_eq!(decoded, envelope);
}

#[test]
fn accepts_an_app_key_at_the_4095_byte_bound() {
    // Goal: the inclusive key limit accepts the largest supported captured key.
    // Method: replace only the App key with 4095 complete hexadecimal bytes.
    let mut envelope = fixture();
    envelope["app"]["bytes_hex"] = json!("41".repeat(4095));

    let decoded = decode(&serde_json::to_vec(&envelope).unwrap())
        .unwrap()
        .value;

    assert_eq!(decoded, envelope);
}

#[test]
fn rejects_an_app_key_above_the_4095_byte_bound() {
    // Goal: key storage stays bounded independently from history and file size.
    // Method: replace only the App key with one byte beyond its supported maximum.
    let mut envelope = fixture();
    envelope["app"]["bytes_hex"] = json!("41".repeat(4096));

    let result = decode(&serde_json::to_vec(&envelope).unwrap());

    assert!(result.is_err());
}

#[test]
fn accepts_4096_selection_trends_with_two_app_history_points() {
    // Goal: scalar context has its own inclusive observation bound within the App envelope.
    // Method: generate 4096 distinct public-shaped trends while retaining two history points.
    let mut envelope = fixture();
    envelope["selection"]["trends"] = json!(selection_trends(4096));

    let decoded = decode(&serde_json::to_vec(&envelope).unwrap())
        .unwrap()
        .value;

    assert_eq!(
        decoded["selection"]["trends"].as_array().unwrap().len(),
        4096
    );
    assert_eq!(decoded["history"]["points"].as_array().unwrap().len(), 2);
}

#[test]
fn rejects_4097_selection_trends_with_two_app_history_points() {
    // Goal: scalar context cannot evade its bound because App projection reads another array.
    // Method: add one scalar trend beyond the limit while retaining two history points.
    let mut envelope = fixture();
    envelope["selection"]["trends"] = json!(selection_trends(4097));

    let result = decode(&serde_json::to_vec(&envelope).unwrap());

    assert!(result.is_err());
}

#[test]
fn refuses_duplicate_selected_history_observations() {
    // Goal: a benchmark cannot hide ambiguous selected-point identity behind repeated projections.
    // Method: duplicate the selected real-shaped observation in an otherwise coherent envelope.
    let mut envelope = fixture();
    let selected = envelope["history"]["points"][1].clone();
    envelope["history"]["points"]
        .as_array_mut()
        .unwrap()
        .push(selected);

    let result = decode(&serde_json::to_vec(&envelope).unwrap());

    assert!(result.is_err());
}

#[test]
fn accepts_the_full_observation_bound() {
    // Goal: the benchmark accepts the viewer's maximum actual history envelope.
    // Method: generate 4096 observations with one selected identity and decode them unchanged.
    let envelope = populated(4096);

    let decoded = decode(&serde_json::to_vec(&envelope).unwrap())
        .unwrap()
        .value;

    assert_eq!(decoded["history"]["points"].as_array().unwrap().len(), 4096);
}

#[test]
fn refuses_history_above_the_observation_bound() {
    // Goal: input validation bounds work before production projection begins.
    // Method: decode one observation beyond the viewer's 4096-point history limit.
    let envelope = populated(4097);

    let result = decode(&serde_json::to_vec(&envelope).unwrap());

    assert!(result.is_err());
}

#[test]
fn accepts_input_at_the_eight_mibibyte_bound() {
    // Goal: the complete capture envelope can fill its documented byte budget.
    // Method: append valid JSON whitespace up to exactly 8 MiB before decoding.
    let mut bytes = serde_json::to_vec(&fixture()).unwrap();
    bytes.resize(8 * 1024 * 1024, b' ');

    let result = decode(&bytes);

    assert!(result.is_ok());
}

#[test]
fn refuses_input_above_the_eight_mibibyte_bound() {
    // Goal: a capture cannot escape its byte budget through otherwise valid JSON.
    // Method: append valid JSON whitespace to one byte beyond 8 MiB.
    let mut bytes = serde_json::to_vec(&fixture()).unwrap();
    bytes.resize(8 * 1024 * 1024 + 1, b' ');

    let result = decode(&bytes);

    assert!(result.is_err());
}

#[test]
fn accepts_the_first_and_last_iteration_counts() {
    // Goal: the opt-in projection workload accepts both inclusive iteration bounds.
    // Method: parse the same command-line text that the benchmark receives.
    assert_eq!(iterations(OsStr::new("1")).unwrap(), 1);
    assert_eq!(iterations(OsStr::new("100")).unwrap(), 100);
}

#[test]
fn refuses_zero_iterations() {
    // Goal: benchmark output must represent an actual projection.
    // Method: parse zero iterations through the command-line parser.
    assert!(iterations(OsStr::new("0")).is_err());
}

#[test]
fn refuses_more_than_one_hundred_iterations() {
    // Goal: one benchmark invocation has a fixed operation bound.
    // Method: parse one iteration beyond the supported maximum.
    assert!(iterations(OsStr::new("101")).is_err());
}

fn populated(count: u32) -> Value {
    let mut envelope = fixture();
    let measured = envelope["history"]["points"][1].clone();
    envelope["history"]["points"] = json!(
        (1..=count)
            .map(|publication| {
                let mut point = measured.clone();
                point["observation"]["stamp"]["publication"] = json!(publication.to_string());
                point
            })
            .collect::<Vec<_>>()
    );
    envelope
}

fn selection_trends(count: u32) -> Vec<Value> {
    assert!(count <= 4097);
    let template = fixture()["selection"]["trends"][0].clone();
    (1..=count)
        .map(|publication| {
            let mut point = template.clone();
            point["publication"] = json!(publication);
            point["run_tick"] = json!(publication);
            let (wall_ns, continuous_ns) = if publication == 1 {
                (998_000_000_000_u64, 1_000_000_000_u64)
            } else {
                let offset = u64::from(publication - 2);
                (1_000_000_000_000 + offset, 3_000_000_000 + offset)
            };
            point["wall_ns"] = json!(wall_ns);
            point["continuous_ns"] = json!(continuous_ns);
            point
        })
        .collect()
}

fn fixture() -> Value {
    let run = "00000000000000010000000000000002";
    let boot = "00000000000000030000000000000004";
    let selected = json!({"wall_ns":"1000000000000","recording":["5","6"],
        "run_tick":"2","stamp":{"run":["1","2"],"boot":["3","4"],
            "publication":"2","continuous_ns":"3000000000","awake_ns":"700000000"}});
    let selection = json!({"second":1000,"context_seconds":900,
        "selected":{"run":run,"boot":boot,"publication":2,"run_tick":2,
            "wall_ns":1_000_000_000_000_u64},
        "trends":[{"run":run,"boot":boot,"publication":2,
            "run_tick":2,"continuous_ns":3_000_000_000_u64,
            "wall_ns":1_000_000_000_000_u64,"segment":0}]});
    let measure = |known: u64| {
        json!({"known":known.to_string(),"measured_members":1,
            "unavailable_members":0,"overflowed":false})
    };
    let unavailable = json!({"observation":{"wall_ns":"998000000000",
        "recording":["5","6"],"run_tick":"1",
        "stamp":{"run":["1","2"],"boot":["3","4"],"publication":"1",
            "continuous_ns":"1000000000","awake_ns":"200000000"}},
        "segment":"0","source_available":false,"elapsed_ns":null,
        "members":null,"membership_complete":false,
        "cpu_ns":null,"energy_nj":null,"resident_bytes":null,"footprint_bytes":null,
        "cpu_percent_one_core":null,"energy_joules":null,"energy_watts":null,
        "disk_read_bytes":null,"disk_written_bytes":null,"disk_io_bytes":null,
        "disk_read_bps":null,"disk_written_bps":null,"disk_io_bps":null});
    let measured = json!({"observation":selected,"segment":"1","source_available":true,
        "elapsed_ns":"2000000000","members":1,"membership_complete":true,
        "cpu_ns":measure(0),"energy_nj":measure(0),"resident_bytes":measure(1024),
        "footprint_bytes":measure(2048),"cpu_percent_one_core":0.0,
        "energy_joules":0.0,"energy_watts":0.0,
        "disk_read_bytes":measure(4096),"disk_written_bytes":measure(1024),
        "disk_io_bytes":measure(5120),"disk_read_bps":2048.0,
        "disk_written_bps":512.0,"disk_io_bps":2560.0});
    json!({"schema_version":1,"route":"apps","app":{"kind":"bundle","bytes_hex":"41"},
        "selection":selection,"history":{"selected":selected,"context_seconds":900,
            "points":[unavailable,measured],"coverage":{"retention_limited":false}}})
}
