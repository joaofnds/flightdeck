use num_traits::ToPrimitive;
use serde_json::Value;

#[derive(Debug, PartialEq)]
pub struct MemoryQuantity {
    pub name: &'static str,
    pub reading: String,
}

pub fn quantities(selection: &Value) -> Vec<MemoryQuantity> {
    let summary = crate::live_views::matching_summary(selection);
    let bytes = |key: &str| summary[key].as_u64();
    let cached = bytes("memory_file_backed_bytes")
        .zip(bytes("memory_purgeable_bytes"))
        .and_then(|(file_backed, purgeable)| file_backed.checked_add(purgeable));
    let readings = [
        (
            "App memory",
            crate::live_projection::app_memory(summary).as_u64(),
        ),
        ("Wired", bytes("memory_wired_bytes")),
        ("Compressed", bytes("memory_compressed_bytes")),
        ("Cached", cached),
        ("Free", bytes("memory_free_bytes")),
    ];

    let mut rows: Vec<MemoryQuantity> = readings
        .into_iter()
        .map(|(name, bytes)| MemoryQuantity {
            name,
            reading: gibibytes(bytes),
        })
        .collect();
    rows.push(MemoryQuantity {
        name: "Memory pressure",
        reading: pressure(&summary["pressure"]).to_owned(),
    });

    rows
}

/// A byte count in GiB to one decimal, as the prototype writes its memory quantities.
fn gibibytes(bytes: Option<u64>) -> String {
    const GIBIBYTE: f32 = 1_073_741_824.0;
    bytes.and_then(|bytes| bytes.to_f32()).map_or_else(
        || "Unavailable".to_owned(),
        |bytes| {
            format!(
                "{} GiB",
                crate::live_projection::fixed_text(bytes / GIBIBYTE, 1)
            )
        },
    )
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
