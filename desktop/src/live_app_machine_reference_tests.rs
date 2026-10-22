use serde_json::{Value, json};

use crate::live_app_attribution_history::{AppAttributionHistory, Metric, tests};
use crate::live_hover::Target;

pub(crate) fn scalar_selection() -> Value {
    json!({"second":1000,"context_last_second":1000,
        "selected":{"run":[1,2],"boot":[3,4],"publication":2},
        "trends":[
            {"run":[1,2],"boot":[3,4],"publication":1,"wall_ns":999_000_000_000_u64,
                "cpu_percent":12.5,"cpu_percent_one_core":150.0,"gpu_active_percent":30.0,
                "storage":{"state":"complete","read_bps":2_000_000.0,"write_bps":1_000_000.0}},
            {"run":[1,2],"boot":[3,4],"publication":2,"wall_ns":1_000_000_000_000_u64,
                "cpu_percent":25.0,"cpu_percent_one_core":450.0,"gpu_active_percent":45.0,
                "storage":{"state":"complete","read_bps":4_000_000.0,"write_bps":2_000_000.0}}]})
}

#[test]
fn cpu_reference_is_independent_unstacked_and_excluded_from_captured_totals() {
    // Distinct literal machine/App rates make an accidental subtotal or normalized CPU visible.
    let history = AppAttributionHistory::prepare(
        Metric::Cpu,
        &tests::fixture(Metric::Cpu),
        &scalar_selection(),
    );

    let panel = history.panel().unwrap();

    assert_eq!(panel.lines.len(), 3);
    assert_eq!(panel.lines[2].name, "All CPU");
    assert_eq!(panel.lines[2].values, [Some(150.0), Some(450.0)]);
    assert_eq!(panel.lines[2].stack, Some(false));
    assert_eq!(panel.lines[2].fill, Some(0.0));
    assert_eq!(panel.lines[2].dashed, Some(true));
    assert_eq!(
        history.reading(2, Some(0)),
        "150% · all recorded logical CPUs"
    );
    assert_eq!(
        history.reading(2, Some(1)),
        "450% · all recorded logical CPUs"
    );
    assert_eq!(history.headline(Some(1)), "250% captured");
}

#[test]
fn gpu_reference_is_the_gpus_own_busy_share_over_each_apps_gpu_time() {
    // Goal: the GPU page draws each App's GPU time stacked under the GPU's own busy share, which
    // work from several Apps at once can sit under. Method: one App at 20% and 35% of two
    // intervals beside a GPU busy for 30% and 45% of them.
    let history = AppAttributionHistory::prepare(
        Metric::Gpu,
        &tests::fixture(Metric::Gpu),
        &scalar_selection(),
    );

    let panel = history.panel().unwrap();

    assert_eq!(panel.id, "live-gpu-apps");
    assert_eq!(panel.title, "GPU time by app");
    assert_eq!(panel.lines[1].values, [Some(20.0), Some(35.0)]);
    assert_eq!(panel.lines[2].name, "GPU busy");
    assert_eq!(panel.lines[2].values, [Some(30.0), Some(45.0)]);
    assert_eq!(panel.lines[2].dashed, Some(true));
    assert_eq!(history.reading(2, Some(1)), "45% · GPU active time");
}

#[test]
fn disk_reference_keeps_whole_device_scope_and_exact_app_bytes() {
    // Keep the reference outside App byte accounting even above f32 integer precision.
    let mut apps = tests::disk_fixture();
    apps["points"][1]["apps"][0]["disk_io_bytes"]["known"] = json!("9007199254740993");
    apps["points"][1]["apps"][0]["disk_io_bps"] = json!(4_503_599_627_370_496.0);
    let history = AppAttributionHistory::prepare(Metric::Disk, &apps, &scalar_selection());

    let panel = history.panel().unwrap();

    assert_eq!(panel.lines[2].name, "All disk I/O");
    assert_eq!(panel.lines[2].values, [Some(3.0), Some(6.0)]);
    assert_eq!(history.amount(2, Some(1)), Some(6_000_000.0));
    assert_eq!(history.captured_amount(2, Some(1)), None);
    assert_eq!(
        history.captured_amount(1, Some(1)),
        Some(9_007_199_254_740_993)
    );
    assert_eq!(
        history.reading(2, Some(0)),
        "3 MB/s · whole-device read + write"
    );
    assert_eq!(
        history.reading(2, Some(1)),
        "6 MB/s · whole-device read + write"
    );
    assert!(history.reading(1, Some(1)).ends_with(" MB/s"));
}

#[test]
fn references_use_exact_same_second_identity_across_runs_and_boots() {
    // A same-second sibling with a different boot must never supply the reference.
    let mut selected = scalar_selection();
    let mut apps = tests::fixture(Metric::Cpu);
    apps["points"][0]["observation"]["wall_ns"] = json!("1000000000000");
    apps["points"][0]["observation"]["stamp"]["boot"] = json!(["2", "4"]);
    selected["trends"][0]["wall_ns"] = json!(1_000_000_000_000_u64);
    let history = AppAttributionHistory::prepare(Metric::Cpu, &apps, &selected);
    let earlier = Target::exact(1000, &json!({"run":[1,2],"boot":[2,4],"publication":1})).unwrap();

    let index = history.index(Some(&earlier));

    assert_eq!(index, Some(0));
    assert_eq!(history.reading(1, index), "120%");
    assert_eq!(history.amount(2, index), None);
    assert_eq!(history.amount(2, Some(1)), Some(450.0));
}

#[test]
fn a_missing_scalar_observation_never_borrows_its_neighbor() {
    // The App source can remain available while its independent machine source is absent.
    let mut selected = scalar_selection();
    selected["trends"].as_array_mut().unwrap().remove(0);
    let history = AppAttributionHistory::prepare(Metric::Disk, &tests::disk_fixture(), &selected);

    assert_eq!(history.reading(1, Some(0)), "500 kB/s");
    assert_eq!(history.amount(2, Some(0)), None);
    assert_eq!(history.panel().unwrap().lines[2].values, [None, Some(6.0)]);
}

#[test]
fn incomplete_device_scans_remain_explicitly_unavailable() {
    // Per-device known rates cannot become an All disk I/O subtotal.
    let mut selected = scalar_selection();
    selected["trends"][0]["storage"]["state"] = json!("partial");
    let history = AppAttributionHistory::prepare(Metric::Disk, &tests::disk_fixture(), &selected);

    assert_eq!(
        history.reading(2, Some(0)),
        "Unavailable · whole-device scan partial"
    );
    assert_eq!(history.panel().unwrap().lines[2].values, [None, Some(6.0)]);
    assert_eq!(history.headline(Some(0)), "500 kB/s captured");
}

#[test]
fn a_missing_device_direction_never_becomes_a_zero() {
    // Complete device enumeration does not make an unavailable write interval measured.
    let mut selected = scalar_selection();
    selected["trends"][1]["storage"]["write_bps"] = Value::Null;
    let history = AppAttributionHistory::prepare(Metric::Disk, &tests::disk_fixture(), &selected);

    assert_eq!(history.amount(2, Some(1)), None);
    assert_eq!(
        history.reading(2, Some(1)),
        "Unavailable · machine interval unavailable"
    );
}

#[test]
fn process_power_gets_no_fabricated_machine_comparison() {
    // Whole-machine scalar CPU and Disk availability must not create a Process power reference.
    let history = AppAttributionHistory::prepare(
        Metric::Power,
        &tests::fixture(Metric::Power),
        &scalar_selection(),
    );

    assert_eq!(history.panel().unwrap().lines.len(), 2);
    assert_eq!(history.reference_reading(Some(1)), None);
}

#[test]
fn an_app_named_like_the_reference_keeps_an_independent_legend_identity() {
    // Name-keyed legend visibility must still distinguish a real App from the machine line.
    let mut apps = tests::fixture(Metric::Cpu);
    apps["cohort"][0]["key"]["display"] = json!("All CPU");
    let history = AppAttributionHistory::prepare(Metric::Cpu, &apps, &scalar_selection());

    let panel = history.panel().unwrap();

    assert_eq!(panel.lines[1].name, "All CPU");
    assert_eq!(panel.lines[2].name, "All CPU (machine reference 1)");
}

#[test]
fn normalized_cpu_without_recorded_core_rates_remains_unavailable() {
    // A machine average and an arbitrary core count do not establish one-core CPU units.
    let mut selected = scalar_selection();
    selected["trends"][1]["cpu_percent_one_core"] = Value::Null;
    selected["trends"][1]["logical_cpu_count"] = json!(18);
    let history =
        AppAttributionHistory::prepare(Metric::Cpu, &tests::fixture(Metric::Cpu), &selected);

    assert_eq!(history.amount(2, Some(1)), None);
    assert_eq!(
        history.reading(2, Some(1)),
        "Unavailable · machine interval unavailable"
    );
    assert_eq!(history.headline(Some(1)), "250% captured");
}

#[test]
fn duplicate_scalar_identities_make_only_the_machine_context_unavailable() {
    // Conflicting scalar rows cannot silently choose a reference by array order.
    let mut selected = scalar_selection();
    let duplicate = selected["trends"][1].clone();
    selected["trends"].as_array_mut().unwrap().push(duplicate);
    let history =
        AppAttributionHistory::prepare(Metric::Cpu, &tests::fixture(Metric::Cpu), &selected);

    assert_eq!(history.amount(2, Some(1)), None);
    assert_eq!(
        history.reading(2, Some(1)),
        "Unavailable · machine context invalid"
    );
    assert_eq!(history.headline(Some(1)), "250% captured");
}

#[test]
fn a_same_identity_with_different_nanoseconds_cannot_lend_its_reference() {
    // Matching run/boot/publication is insufficient when the recorded instant differs.
    let mut selected = scalar_selection();
    selected["trends"][0]["wall_ns"] = json!(999_000_000_001_u64);
    let history =
        AppAttributionHistory::prepare(Metric::Cpu, &tests::fixture(Metric::Cpu), &selected);

    assert_eq!(history.amount(2, Some(0)), None);
    assert_eq!(history.amount(2, Some(1)), Some(450.0));
    assert_eq!(history.reading(1, Some(0)), "120%");
}

#[test]
fn a_malformed_scalar_identity_refuses_only_the_machine_context() {
    // A scalar source that cannot establish its identity must not erase valid App history.
    let mut selected = scalar_selection();
    selected["trends"][0]["run"] = Value::Null;
    let history =
        AppAttributionHistory::prepare(Metric::Cpu, &tests::fixture(Metric::Cpu), &selected);

    assert_eq!(
        history.reading(2, Some(1)),
        "Unavailable · machine context invalid"
    );
    assert_eq!(history.headline(Some(1)), "250% captured");
}

#[test]
fn an_oversized_scalar_context_cannot_expand_the_reference_index() {
    // Reject the first point beyond the existing context bound before building its index.
    let selected = scalar_context_with_count(crate::data::LIVE_SAMPLE_LIMIT + 1);
    let history =
        AppAttributionHistory::prepare(Metric::Cpu, &tests::fixture(Metric::Cpu), &selected);

    assert_eq!(history.amount(2, Some(0)), None);
    assert_eq!(history.amount(2, Some(1)), None);
    assert_eq!(
        history.reading(2, Some(1)),
        "Unavailable · machine context invalid"
    );
    assert_eq!(history.headline(Some(1)), "250% captured");
}

#[test]
fn an_exact_limit_scalar_context_keeps_both_matching_references() {
    // Unique identities reach the bound without duplicate rejection masking its behavior.
    let selected = scalar_context_with_count(crate::data::LIVE_SAMPLE_LIMIT);
    let history =
        AppAttributionHistory::prepare(Metric::Cpu, &tests::fixture(Metric::Cpu), &selected);

    assert_eq!(
        history.panel().unwrap().lines[2].values,
        [Some(150.0), Some(450.0)]
    );
    assert_eq!(history.amount(2, Some(0)), Some(150.0));
    assert_eq!(history.amount(2, Some(1)), Some(450.0));
    assert_eq!(history.headline(Some(1)), "250% captured");
}

fn scalar_context_with_count(count: usize) -> Value {
    assert!(count >= 2);
    assert!(count <= crate::data::LIVE_SAMPLE_LIMIT + 1);
    let mut selected = scalar_selection();
    let template = selected["trends"][0].clone();
    let points = selected["trends"].as_array_mut().unwrap();
    for publication in 3..=u64::try_from(count).unwrap() {
        let mut point = template.clone();
        point["publication"] = json!(publication);
        point["wall_ns"] = json!(1_000_000_000_000_u64 + publication);
        points.push(point);
    }
    selected
}

macro_rules! reference_identity_case {
    ($name:ident, $field:literal, $wrong:expr) => {
        #[test]
        fn $name() {
            // A same-second scalar with one wrong identity field cannot lend its reference.
            let mut selected = scalar_selection();
            let mut apps = tests::fixture(Metric::Cpu);
            apps["points"][0]["observation"]["wall_ns"] = json!("1000000000000");
            selected["trends"][0]["wall_ns"] = json!(1_000_000_000_000_u64);
            selected["trends"][0][$field] = $wrong;
            let history = AppAttributionHistory::prepare(Metric::Cpu, &apps, &selected);

            assert_eq!(history.amount(2, Some(0)), None);
            assert_eq!(history.amount(2, Some(1)), Some(450.0));
            assert_eq!(history.reading(1, Some(0)), "120%");
        }
    };
}

reference_identity_case!(machine_reference_refuses_another_run, "run", json!([9, 10]));
reference_identity_case!(
    machine_reference_refuses_another_boot,
    "boot",
    json!([9, 10])
);
reference_identity_case!(
    machine_reference_refuses_another_publication,
    "publication",
    json!(3)
);
