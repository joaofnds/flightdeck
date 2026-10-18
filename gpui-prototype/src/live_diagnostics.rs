use gpui::{IntoElement, div, prelude::*, px, rgb};
use serde_json::Value;

use crate::charts::{GRID, MUTED, PANEL};
use crate::live_dashboard::LiveDashboard;

pub struct Field {
    pub name: &'static str,
    pub reading: String,
}

pub struct DiagnosticSources<'a> {
    pub catalog: &'a Value,
    pub selected: &'a Value,
    pub reader: &'a str,
}

pub fn fields(sources: &DiagnosticSources<'_>) -> Vec<Field> {
    let latest = &sources.catalog["latest_observation"];
    let collector = match sources.catalog["collector_completed"].as_bool() {
        Some(true) => "Stopped",
        Some(false) => "Producer observed",
        None => "Unavailable",
    };
    let history = if sources.catalog["history_error"].is_null() {
        text(&sources.catalog["windows"])
    } else {
        "Unavailable".into()
    };
    let mut values = vec![
        field("Collector", collector.into()),
        field(
            "Collector PID",
            text(&sources.catalog["latest_collector_pid"]),
        ),
        field("Reader", sources.reader.into()),
        field("Saved windows", history),
        field(
            "Reader losses",
            losses(&sources.catalog["reader_losses"]).into(),
        ),
        field("Recording", text(&sources.catalog["path"])),
        field("Latest publication", text(&latest["publication"])),
        field("Latest observed at", observed_clock(latest)),
        field("Latest run", text(&latest["run"])),
        field("Latest boot", text(&latest["boot"])),
        field(
            "Selected publication",
            text(&sources.selected["publication"]),
        ),
        field("Selected observed at", observed_clock(sources.selected)),
        field("Selected run", text(&sources.selected["run"])),
        field("Selected boot", text(&sources.selected["boot"])),
        field(
            "Unsealed ticks retained",
            text(&sources.catalog["tail_ticks"]),
        ),
        field(
            "Tail cache bytes",
            text(&sources.catalog["tail_cache_bytes"]),
        ),
        field(
            "Tail cache budget bytes",
            text(&sources.catalog["tail_cache_bytes_max"]),
        ),
        field(
            "Ticks without wall time",
            text(&sources.catalog["unknown_clock_ticks"]),
        ),
    ];
    if let Some(error) = sources.catalog["history_error"].as_str() {
        values.push(field("Saved history error", bounded(error)));
    }
    for notice in crate::live_notices::losses(sources.catalog, &Value::Null) {
        values.push(field("Source notice", notice));
    }

    assert!(values.len() <= 26);

    values
}

fn field(name: &'static str, reading: String) -> Field {
    Field { name, reading }
}

fn bounded(value: &str) -> String {
    crate::live_notices::recording_context::bounded(value, 512)
}

fn text(value: &Value) -> String {
    match value {
        Value::String(value) => bounded(value),
        Value::Number(value) => value.to_string(),
        _ => "Unavailable".into(),
    }
}

fn observed_clock(observation: &Value) -> String {
    observation["wall_ns"].as_u64().map_or_else(
        || "Unavailable".into(),
        |wall| {
            crate::live_presentation::selected_clock(&serde_json::json!({
                "second":wall / 1_000_000_000,
            }))
        },
    )
}

fn losses(readers: &Value) -> &'static str {
    let Some(readers) = readers.as_array() else {
        return "Unavailable";
    };

    if readers.is_empty() {
        return "Unavailable";
    }

    if readers.len() > 2 {
        return "Unavailable";
    }

    for reader in readers {
        let losses = &reader["losses"];
        if losses["unknown"].as_bool() != Some(false) {
            return "Additional loss unknown";
        }

        for counter in ["publications", "events"] {
            match losses[counter].as_u64() {
                Some(0) => {}
                Some(_) => return "Loss reported",
                None => return "Additional loss unknown",
            }
        }
    }

    "None reported"
}

impl LiveDashboard {
    pub fn self_monitoring(&self) -> impl IntoElement {
        let catalog = self
            .context_snapshot()
            .map_or(&Value::Null, |snapshot| &snapshot.catalog);
        let readings = fields(&DiagnosticSources {
            catalog,
            selected: &self.native.diagnostic_selected,
            reader: &self.status(),
        });
        let mut content = div().flex().flex_col().gap(px(12.0)).child(
            div()
                .text_size(px(17.0))
                .child("Recording and reader diagnostics"),
        );

        for field in readings {
            let selector = format!("diagnostic:{}:{}", field.name, field.reading);
            content = content.child(
                div()
                    .debug_selector(|| selector.clone())
                    .flex()
                    .flex_wrap()
                    .justify_between()
                    .gap(px(12.0))
                    .child(div().text_color(rgb(MUTED)).child(field.name))
                    .child(field.reading),
            );
        }

        div()
            .p(px(18.0))
            .rounded(px(7.0))
            .border_1()
            .border_color(rgb(GRID))
            .bg(rgb(PANEL))
            .child(content)
            .children(self.error.as_ref().map(|error| {
                div().mt(px(12.0)).text_color(rgb(crate::charts::ORANGE)).child(
                    format!("Reader error: {}", bounded(error)),
                )
            }))
            .child(div().mt(px(16.0)).text_size(px(11.0)).text_color(rgb(MUTED)).child(
                "Connection metadata does not establish complete capture or durable saving. \
                 Latest producer and selected observations are separate. Publication numbers \
                 belong to their run and boot. CPU time, physical footprint, per-source costs \
                 and seal latency are not exposed by this connection.",
            ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn diagnostics_distinguish_the_latest_producer_from_a_paused_selection() {
        // Compare source positions without borrowing the newer publication for the selection.
        let catalog = json!({"latest_collector_pid":42,"tail_ticks":7,
            "windows":12,"collector_completed":false,
            "latest_observation":{"publication":9_007_199_254_740_993_u64,"run":"new-run",
                "boot":"boot","wall_ns":1_000_000_000_000_u64}});
        let selected = json!({"publication":3,"run":"older-run","boot":"boot"});

        let readings = fields(&DiagnosticSources {
            catalog: &catalog,
            selected: &selected,
            reader: "Paused",
        });
        let values: Vec<_> = readings
            .iter()
            .map(|field| (field.name, field.reading.as_str()))
            .collect();

        assert!(values.contains(&("Collector PID", "42")));
        assert!(values.contains(&("Latest publication", "9007199254740993")));
        assert!(values.contains(&("Selected publication", "3")));
        assert!(values.contains(&("Selected run", "older-run")));
        assert!(values.contains(&("Unsealed ticks retained", "7")));
    }

    #[test]
    fn unknown_reader_losses_remain_visible_beside_stopped_history() {
        // Keep independent failures visible without treating unknown loss as a measured zero.
        let catalog = json!({"collector_completed":true,"history_error":"Unreadable minute",
            "reader_losses":[{"run":"run","losses":{"publications":0,
                "events":0,"unknown":true}}]});

        let readings = fields(&DiagnosticSources {
            catalog: &catalog,
            selected: &Value::Null,
            reader: "Reading unavailable",
        });
        let values: Vec<_> = readings
            .iter()
            .map(|field| (field.name, field.reading.as_str()))
            .collect();

        assert!(values.contains(&("Collector", "Stopped")));
        assert!(values.contains(&("Reader", "Reading unavailable")));
        assert!(values.contains(&("Saved windows", "Unavailable")));
        assert!(values.contains(&("Saved history error", "Unreadable minute")));
        assert!(values.contains(&("Reader losses", "Additional loss unknown")));
        assert!(!values.contains(&("Reader losses", "None reported")));
    }

    #[test]
    fn missing_diagnostic_metadata_does_not_imply_a_healthy_pipeline() {
        // An unconnected reader has neither known zero loss nor a known producer.
        let readings = fields(&DiagnosticSources {
            catalog: &Value::Null,
            selected: &Value::Null,
            reader: "Waiting",
        });
        let values: Vec<_> = readings
            .iter()
            .map(|field| (field.name, field.reading.as_str()))
            .collect();

        assert!(values.contains(&("Collector", "Unavailable")));
        assert!(values.contains(&("Reader losses", "Unavailable")));
        assert!(values.contains(&("Selected publication", "Unavailable")));
    }

    #[test]
    fn known_zero_reader_losses_stay_distinct_from_unavailable_metadata() {
        // A valid report of zero loss may say none reported without promising completeness.
        let catalog = json!({"reader_losses":[{"losses":{"publications":0,
            "events":0,"unknown":false}}]});

        let readings = fields(&DiagnosticSources {
            catalog: &catalog,
            selected: &Value::Null,
            reader: "Reader connected",
        });

        assert!(
            readings
                .iter()
                .any(|field| { field.name == "Reader losses" && field.reading == "None reported" })
        );
    }
}
