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

        fn bits(&self) -> Value {
            serde_json::from_slice(&fs::read(self.directory.join("bits.json")).unwrap()).unwrap()
        }

        fn rejects(&self) {
            let result = self.run();

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
}
