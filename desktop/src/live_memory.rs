use serde_json::Value;

#[derive(Debug, PartialEq)]
pub struct MemoryQuantity {
    pub name: &'static str,
    pub reading: String,
}

pub fn quantities(selection: &Value) -> Vec<MemoryQuantity> {
    const QUANTITIES: [(&str, &str); 12] = [
        ("Installed", "memory_total_bytes"),
        ("Active", "memory_active_bytes"),
        ("Wired", "memory_wired_bytes"),
        ("Compressed", "memory_compressed_bytes"),
        ("Free", "memory_free_bytes"),
        ("Inactive", "memory_inactive_bytes"),
        ("Speculative", "memory_speculative_bytes"),
        ("Anonymous", "memory_anonymous_bytes"),
        ("File-backed", "memory_file_backed_bytes"),
        ("Purgeable", "memory_purgeable_bytes"),
        ("Swap used", "swap_used_bytes"),
        ("Swap allocated", "swap_total_bytes"),
    ];
    let summary = crate::live_views::matching_summary(selection);
    let mut rows = Vec::with_capacity(13);

    for (name, key) in QUANTITIES {
        rows.push(MemoryQuantity {
            name,
            reading: crate::live_projection::quantity(summary, key, "GiB", 1_073_741_824.0),
        });
    }

    rows.push(MemoryQuantity {
        name: "Memory pressure",
        reading: pressure(&summary["pressure"]).to_owned(),
    });

    rows
}

fn pressure(value: &Value) -> &'static str {
    match value.as_u64() {
        Some(0) => "Normal",
        Some(1) => "Warning",
        Some(2) => "Critical",
        _ => "Unavailable",
    }
}

#[cfg(test)]
#[path = "live_memory_tests.rs"]
mod tests;
