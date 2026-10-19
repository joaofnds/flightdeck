#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::{Command, Output};
    use std::sync::atomic::{AtomicU64, Ordering};

    use serde_json::{Value, json};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    struct Benchmark {
        directory: PathBuf,
    }

    impl Benchmark {
        fn new(selection: &Value) -> Self {
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            assert!(sequence < 1000);
            let directory = std::env::temp_dir().join(format!(
                "flightdeck-projection-cli-{}-{sequence}",
                std::process::id()
            ));
            fs::create_dir(&directory).unwrap();
            fs::write(directory.join("input.json"), selection.to_string()).unwrap();

            Self { directory }
        }

        fn run(&self) -> Output {
            Command::new(env!("CARGO_BIN_EXE_projection_bench"))
                .arg("--input")
                .arg(self.directory.join("input.json"))
                .args(["--route", "cpu", "--iterations", "1", "--output"])
                .arg(self.directory.join("bits.json"))
                .output()
                .unwrap()
        }

        fn run_app(&self, iterations: u32, extra: &[&str]) -> Output {
            assert!((1..=100).contains(&iterations));
            assert!(extra.len() <= 1);
            Command::new(env!("CARGO_BIN_EXE_projection_bench"))
                .arg("--app-input")
                .arg(self.directory.join("input.json"))
                .arg("--iterations")
                .arg(iterations.to_string())
                .arg("--output")
                .arg(self.directory.join("bits.json"))
                .args(extra)
                .output()
                .unwrap()
        }

        fn bits(&self) -> Value {
            serde_json::from_slice(&fs::read(self.directory.join("bits.json")).unwrap()).unwrap()
        }

        fn rejects(&self) {
            let result = self.run();

            assert_eq!(result.status.code(), Some(1));
            assert!(result.stdout.is_empty());
            assert!(!self.directory.join("bits.json").exists());
        }

        fn rejects_app(&self, extra: &[&str]) {
            let result = self.run_app(1, extra);

            assert_eq!(result.status.code(), Some(1));
            assert!(result.stdout.is_empty());
            assert!(!self.directory.join("bits.json").exists());
        }
    }

    impl Drop for Benchmark {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.directory).unwrap();
        }
    }

    fn selection() -> Value {
        json!({"second": 1000, "selected": {"run": [1, 2], "boot": [3, 4],
        "publication": 1}, "trends": [{"run": [1, 2], "boot": [3, 4],
        "publication": 1, "segment": 0, "wall_ns": 1_000_000_000_000_u64,
        "cpu_percent": 25.0}]})
    }

    #[test]
    fn projects_valid_selection_with_exact_sample_bits() {
        // Goal: the CLI accepts valid input without changing selected samples.
        // Method: inspect its saved bit representation and structured point count.
        let benchmark = Benchmark::new(&selection());

        let result = benchmark.run();

        assert!(result.status.success(), "{:?}", result.stderr);
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["scalar_points"], 1);
        let bits = benchmark.bits();
        assert_eq!(bits[0]["selected_index"], 0);
        assert_eq!(bits[0]["segments"], json!([0]));
        assert_eq!(bits[0]["lines"][0]["values"], json!([25.0_f32.to_bits()]));
    }

    #[test]
    fn accepts_explicitly_unselected_history() {
        // Goal: valid history remains measurable when its selected second is a gap.
        // Method: explicitly supply null selection metadata and retain the sample.
        let mut input = selection();
        input["selected"] = Value::Null;
        let benchmark = Benchmark::new(&input);

        let result = benchmark.run();

        assert!(result.status.success(), "{:?}", result.stderr);
        assert_eq!(benchmark.bits()[0]["selected_index"], Value::Null);
    }

    #[test]
    fn rejects_missing_trends_before_writing_measurement() {
        // Goal: missing history cannot masquerade as an empty successful measurement.
        // Method: send an empty object through the actual CLI.
        Benchmark::new(&json!({})).rejects();
    }

    #[test]
    fn rejects_mistyped_trends_before_writing_measurement() {
        // Goal: corrupted history cannot use the production UI's no-data fallback.
        // Method: supply a string where the selection requires an array.
        Benchmark::new(&json!({"trends": "corrupt"})).rejects();
    }

    #[test]
    fn rejects_empty_history_before_writing_measurement() {
        // Goal: this measurement tool requires an explicit nonempty workload.
        // Method: provide a structurally present but empty history.
        let mut input = selection();
        input["trends"] = json!([]);

        Benchmark::new(&input).rejects();
    }

    #[test]
    fn rejects_non_object_points_before_writing_measurement() {
        // Goal: malformed points fail at the benchmark boundary.
        // Method: replace a real point with a scalar.
        let mut input = selection();
        input["trends"][0] = json!(7);

        Benchmark::new(&input).rejects();
    }

    #[test]
    fn rejects_invalid_point_identity_before_writing_measurement() {
        // Goal: structurally present points still require valid observation identities.
        // Method: corrupt a run identity and exercise the uncounted projection validation.
        let mut input = selection();
        input["trends"][0]["run"] = json!("corrupt");

        Benchmark::new(&input).rejects();
    }

    #[test]
    fn rejects_history_exceeding_the_sample_bound() {
        // Goal: excessive workloads fail instead of silently truncating measurements.
        // Method: send one more point than the projection's 4096-observation limit.
        let mut input = selection();
        input["trends"] = json!(vec![input["trends"][0].clone(); 4097]);

        Benchmark::new(&input).rejects();
    }

    #[test]
    fn rejects_missing_publication_even_without_selected_point() {
        // Goal: every measured point remains a complete observation identity.
        // Method: remove publication while selection matching is disabled.
        let mut input = selection();
        input["selected"] = Value::Null;
        input["trends"][0]
            .as_object_mut()
            .unwrap()
            .remove("publication");

        Benchmark::new(&input).rejects();
    }

    #[test]
    fn rejects_invalid_publication_even_without_selected_point() {
        // Goal: identity integer bounds apply to unselected observations too.
        // Method: provide an overflowing publication without selection matching.
        let mut input = selection();
        input["selected"] = Value::Null;
        input["trends"][0]["publication"] = json!("18446744073709551616");

        Benchmark::new(&input).rejects();
    }

    #[test]
    fn rejects_missing_selection_identity_field() {
        // Goal: missing selection metadata cannot silently mean no selected point.
        // Method: omit selected rather than explicitly supplying null.
        let mut input = selection();
        input.as_object_mut().unwrap().remove("selected");

        Benchmark::new(&input).rejects();
    }

    #[test]
    fn projects_selected_app_history_through_the_public_command() {
        // Goal: the spawned App command retains five exact panel outputs and selected identity.
        // Method: submit a synthetic coherent envelope through the existing CLI driver.
        let input = app_input();
        let benchmark = Benchmark::new(&input);

        let result = benchmark.run_app(1, &[]);

        assert!(result.status.success(), "{:?}", result.stderr);
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(report["mode"], "app-projection");
        assert_eq!(report["history_points"], 2);
        assert_eq!(report["warmup_projections"], 1);
        assert_eq!(report["measurements"].as_array().unwrap().len(), 1);
        assert_eq!(report["measurements"][0]["iteration"], 0);
        assert_app_measurement(&report["measurements"][0]);
        let output = benchmark.bits();
        assert_eq!(output["app"], input["app"]);
        assert_eq!(output["selected"], input["history"]["selected"]);
        assert_eq!(output["selection_selected"], input["selection"]["selected"]);
        assert_eq!(
            app_panel_bits(&output["panels"]),
            json!([
                ["live-app-cpu", 1, [0, 1], [[null, 25.0_f32.to_bits()]]],
                ["live-app-resident", 1, [0, 1], [[null, 1.0_f32.to_bits()]]],
                ["live-app-disk", 1, [0, 1], [[null, 0.002_56_f32.to_bits()]]],
                ["live-app-gpu", 1, [0, 1], [[null, null]]],
                ["live-app-power", 1, [0, 1], [[null, 3.0_f32.to_bits()]]]
            ])
        );
    }

    #[test]
    fn rejects_mismatched_app_identity_through_the_public_command() {
        // Goal: an App measurement cannot combine different selected observations.
        // Method: change only the history publication in a synthetic coherent CLI envelope.
        let mut input = app_input();
        input["history"]["selected"]["stamp"]["publication"] = json!("3");

        Benchmark::new(&input).rejects_app(&[]);
    }

    #[test]
    fn rejects_an_extra_app_command_argument() {
        // Goal: the public command refuses trailing arguments rather than ignoring them.
        // Method: append one sentinel argument through the same App driver as the valid case.
        let benchmark = Benchmark::new(&app_input());

        benchmark.rejects_app(&["extra"]);
    }

    #[test]
    fn rejects_paired_invalid_app_recording_through_the_public_command() {
        // Goal: identical malformed metadata is refused before it becomes benchmark evidence.
        // Method: corrupt recording in both selected metadata copies and use the actual CLI.
        let mut input = app_input();
        input["history"]["selected"]["recording"] = json!("corrupt");
        input["history"]["points"][1]["observation"]["recording"] = json!("corrupt");

        Benchmark::new(&input).rejects_app(&[]);
    }

    #[test]
    fn rejects_non_object_selection_trends_through_the_app_command() {
        // Goal: the retained public scalar response remains valid in an App envelope.
        // Method: replace its trends with a scalar point while leaving App history intact.
        let mut input = app_input();
        input["selection"]["trends"] = json!([7]);

        Benchmark::new(&input).rejects_app(&[]);
    }

    #[test]
    fn measures_each_requested_app_projection_iteration() {
        // Goal: the public measurement command honors the requested operation count.
        // Method: request two iterations and inspect both structured result rows.
        let input = app_input();
        let benchmark = Benchmark::new(&input);

        let result = benchmark.run_app(2, &[]);

        assert!(result.status.success(), "{:?}", result.stderr);
        let report: Value = serde_json::from_slice(&result.stdout).unwrap();
        let rows = report["measurements"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["iteration"], 0);
        assert_eq!(rows[1]["iteration"], 1);
        assert_app_measurement(&rows[0]);
        assert_app_measurement(&rows[1]);
        assert_eq!(benchmark.bits()["selected"], input["history"]["selected"]);
    }

    fn assert_app_measurement(measurement: &Value) {
        assert!(measurement["elapsed_ns"].as_u64().is_some());
        assert!(measurement["process_cpu_ns"].as_u64().is_some());
        assert!(measurement["allocation_calls"].as_u64().is_some());
        assert!(measurement["requested_bytes"].as_u64().is_some());
        assert!(measurement["allocation_calls"].as_u64().unwrap() > 0);
        assert!(measurement["requested_bytes"].as_u64().unwrap() > 0);
    }

    fn app_panel_bits(panels: &Value) -> Value {
        json!(
            panels
                .as_array()
                .unwrap()
                .iter()
                .map(|panel| {
                    let lines: Vec<_> = panel["lines"]
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|line| line["values"].clone())
                        .collect();
                    json!([
                        panel["id"],
                        panel["selected_index"],
                        panel["segments"],
                        lines
                    ])
                })
                .collect::<Vec<_>>()
        )
    }

    fn app_input() -> Value {
        let mut context = selection();
        context["selected"]["publication"] = json!(2);
        context["selected"]["run_tick"] = json!(2);
        context["selected"]["wall_ns"] = json!(1_000_000_000_000_u64);
        context["trends"][0]["publication"] = json!(2);
        context["trends"][0]["run_tick"] = json!(2);
        context["trends"][0]["continuous_ns"] = json!(3_000_000_000_u64);
        let selected = json!({"recording":["5","6"],"wall_ns":"1000000000000",
            "run_tick":"2","stamp":{"run":["1","2"],"boot":["3","4"],
                "publication":"2","continuous_ns":"3000000000","awake_ns":"700000000"}});
        let measure = |known: u64| {
            json!({"known":known.to_string(),"measured_members":1,
                "unavailable_members":0,"overflowed":false})
        };
        let gap = json!({"observation":{"recording":["5","6"],"wall_ns":"998000000000",
            "run_tick":"1","stamp":{"run":["1","2"],"boot":["3","4"],
                "publication":"1","continuous_ns":"1000000000","awake_ns":"200000000"}},
            "segment":"0","source_available":false,"elapsed_ns":null,
            "members":null,"membership_complete":false,
            "cpu_ns":null,"energy_nj":null,"resident_bytes":null,"footprint_bytes":null,
            "cpu_percent_one_core":null,"energy_joules":null,"energy_watts":null,
            "disk_read_bytes":null,"disk_written_bytes":null,"disk_io_bytes":null,
            "disk_read_bps":null,"disk_written_bps":null,"disk_io_bps":null});
        let measured = json!({"observation":selected,"segment":"1","source_available":true,
            "elapsed_ns":"2000000000","members":1,"membership_complete":true,
            "cpu_ns":measure(500_000_000),"energy_nj":measure(6_000_000_000),
            "resident_bytes":measure(1_073_741_824),"footprint_bytes":measure(2_147_483_648),
            "cpu_percent_one_core":25.0,"energy_joules":6.0,"energy_watts":3.0,
            "disk_read_bytes":measure(4096),"disk_written_bytes":measure(1024),
            "disk_io_bytes":measure(5120),"disk_read_bps":2048.0,
            "disk_written_bps":512.0,"disk_io_bps":2560.0});
        json!({"schema_version":1,"route":"apps","app":{"kind":"bundle","bytes_hex":"41"},
            "selection":context,"history":{"selected":selected,"context_seconds":900,
                "points":[gap,measured],"coverage":{"retention_limited":false}}})
    }
}
