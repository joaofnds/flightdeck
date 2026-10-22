//! The machine's facts as the collector read them, which every reading's summary carries.

use serde_json::Value;

/// The labels of the Computer page's model card, in its order.
const LABELS: [&str; 6] = [
    "Model identifier",
    "Chip",
    "Architecture",
    "CPU cores",
    "GPU cores",
    "Memory",
];

/// The Mac's identity and the sizes the Computer page's model card shows, from the summary
/// of the newest reading. A fact the collector did not read is unavailable, and every fact
/// is loading before the first reading.
#[must_use]
pub fn rows(summary: Option<&Value>) -> Vec<(String, String)> {
    let Some(summary) = summary else {
        return LABELS
            .iter()
            .map(|label| ((*label).to_owned(), "Loading".to_owned()))
            .collect();
    };

    let readings = [
        summary["machine_model"].as_str().map(str::to_owned),
        summary["machine_chip"].as_str().map(str::to_owned),
        summary["machine_architecture"].as_str().map(str::to_owned),
        summary["machine_cpu_cores"]
            .as_u64()
            .map(|cores| cores.to_string()),
        summary["machine_gpu_cores"]
            .as_u64()
            .map(|cores| cores.to_string()),
        summary["machine_memory_bytes"].as_u64().map(memory_format),
    ];
    LABELS
        .iter()
        .zip(readings)
        .map(|(label, reading)| ((*label).to_owned(), rows_reading(reading)))
        .collect()
}

fn rows_reading(reading: Option<String>) -> String {
    match reading {
        Some(reading) => reading,
        None => "Unavailable".to_owned(),
    }
}

/// One performance level of the Mac's CPUs, named as macOS names it.
pub struct PerformanceLevel {
    pub name: String,
    pub cores: String,
    /// The same, in lower case, as a chart's legend names a cluster.
    pub cluster_cores: String,
    pub logical_cpus: u32,
}

/// The Mac's two performance levels as `point` recorded them, fastest first, or none where
/// the collector read no two levels or `point` holds no reading.
#[must_use]
pub fn performance_levels(point: &Value) -> Option<[PerformanceLevel; 2]> {
    let Some([fastest, slower]) = point["performance_levels"].as_array().map(Vec::as_slice) else {
        return None;
    };

    Some([
        performance_levels_level(fastest)?,
        performance_levels_level(slower)?,
    ])
}

fn performance_levels_level(level: &Value) -> Option<PerformanceLevel> {
    let name = level["name"].as_str()?;
    let Ok(logical_cpus) = u32::try_from(level["logical_cpus"].as_u64()?) else {
        return None;
    };

    Some(PerformanceLevel {
        name: name.to_owned(),
        cores: format!("{name} cores"),
        cluster_cores: format!("{} cores", name.to_lowercase()),
        logical_cpus,
    })
}

/// A logical CPU named by its performance level as the prototype names it, such as
/// "super 12". macOS numbers the slower level's CPUs first. A CPU past both levels, or a
/// reading without levels, keeps its plain index.
#[must_use]
pub fn logical_cpu_name(levels: Option<&[PerformanceLevel; 2]>, cpu: u32) -> String {
    let Some([fastest, slower]) = levels else {
        return format!("CPU {cpu}");
    };
    if cpu < slower.logical_cpus {
        return format!("{} {cpu}", slower.name.to_lowercase());
    }
    if cpu - slower.logical_cpus < fastest.logical_cpus {
        return format!("{} {cpu}", fastest.name.to_lowercase());
    }
    format!("CPU {cpu}")
}

/// Whole gibibytes as the prototype writes them, such as "48 GiB", and any other size to
/// the hundredth below it.
fn memory_format(bytes: u64) -> String {
    let gibibytes = bytes / 1_073_741_824;
    let remainder = bytes % 1_073_741_824;
    if remainder == 0 {
        return format!("{gibibytes} GiB");
    }
    // The hundredths floor, so a size never reads larger than it is.
    let hundredths = remainder * 100 / 1_073_741_824;
    format!("{gibibytes}.{hundredths:02} GiB")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn rows_read_each_fact_the_reading_carries() {
        // Goal: the model card shows the machine as the collector recorded it.
        // Method: read the rows of a summary that carries every fact.
        let summary = json!({
            "machine_model": "Mac17,8",
            "machine_chip": "Apple M5 Pro",
            "machine_architecture": "arm64",
            "machine_cpu_cores": 18,
            "machine_gpu_cores": 20,
            "machine_memory_bytes": 51_539_607_552_u64,
        });

        let rows = rows(Some(&summary));

        assert_eq!(
            rows,
            [
                ("Model identifier", "Mac17,8"),
                ("Chip", "Apple M5 Pro"),
                ("Architecture", "arm64"),
                ("CPU cores", "18"),
                ("GPU cores", "20"),
                ("Memory", "48 GiB"),
            ]
            .map(|(label, value)| (label.to_owned(), value.to_owned()))
        );
    }

    #[test]
    fn rows_read_a_fact_the_collector_did_not_read_as_unavailable() {
        // Goal: a fact missing from the reading is said to be missing, not left blank.
        // Method: read the rows of a summary whose GPU core count is null.
        let summary = json!({"machine_model": "Mac17,8", "machine_gpu_cores": null});

        let rows = rows(Some(&summary));

        assert_eq!(rows[0].1, "Mac17,8");
        assert_eq!(rows[4].1, "Unavailable");
    }

    #[test]
    fn rows_read_every_fact_as_loading_before_the_first_reading() {
        // Goal: the model card keeps its rows before a reading arrives.
        // Method: read the rows with no summary.
        let rows = rows(None);

        assert_eq!(rows.len(), LABELS.len());
        assert!(rows.iter().all(|(_, value)| value == "Loading"));
    }

    #[test]
    fn performance_levels_read_the_two_levels_a_reading_recorded() {
        // Goal: the levels come from the reading, fastest first, named as macOS names them.
        // Method: read a point carrying six super and twelve performance CPUs.
        let point = json!({"performance_levels": [
            {"name": "Super", "logical_cpus": 6},
            {"name": "Performance", "logical_cpus": 12},
        ]});

        let levels = performance_levels(&point).unwrap();

        assert_eq!(levels[0].cores, "Super cores");
        assert_eq!(levels[0].logical_cpus, 6);
        assert_eq!(levels[1].cluster_cores, "performance cores");
        assert_eq!(levels[1].logical_cpus, 12);
    }

    #[test]
    fn performance_levels_read_none_without_two_levels() {
        // Goal: a reading without both levels splits nothing by level.
        // Method: read a point with null levels, one with a single level, and none at all.
        let single = json!({"performance_levels": [{"name": "Super", "logical_cpus": 6}]});

        let points = [json!({"performance_levels": null}), single, Value::Null];

        for point in points {
            assert!(performance_levels(&point).is_none(), "{point}");
        }
    }

    #[test]
    fn memory_format_writes_whole_gibibytes_without_decimals() {
        // Goal: memory reads as the prototype writes it, with decimals only where they count.
        // Method: format 48 GiB exactly, and 47.5 GiB less one byte, which floors.
        let gibibyte = 1_073_741_824_u64;

        let sizes = [48 * gibibyte, 47 * gibibyte + gibibyte / 2 - 1].map(memory_format);

        assert_eq!(sizes, ["48 GiB", "47.49 GiB"]);
    }

    #[test]
    fn logical_cpu_name_numbers_the_slower_level_first() {
        // Goal: CPUs read as the prototype labels them, slower level first, then the fastest.
        // Method: twelve performance and six super CPUs, with an index past both and no levels.
        let level = |name: &str, logical_cpus| PerformanceLevel {
            name: name.into(),
            cores: String::new(),
            cluster_cores: String::new(),
            logical_cpus,
        };
        let levels = [level("Super", 6), level("Performance", 12)];

        let names = [0, 11, 12, 17, 18].map(|cpu| logical_cpu_name(Some(&levels), cpu));

        assert_eq!(
            names,
            [
                "performance 0",
                "performance 11",
                "super 12",
                "super 17",
                "CPU 18"
            ]
        );
        assert_eq!(logical_cpu_name(None, 3), "CPU 3");
    }
}
