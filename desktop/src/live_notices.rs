use serde_json::Value;

#[path = "live_recording_context.rs"]
pub mod recording_context;

pub fn losses(catalog: &Value, selection: &Value) -> Vec<String> {
    let mut notices = Vec::with_capacity(7);
    if let Some(readers) = catalog["reader_losses"].as_array() {
        for reader in readers.iter().take(2) {
            if let Some(message) = notice("Live reader", &reader["losses"]) {
                notices.push(format!("{message} · run {}", reader["run"]));
            }
        }
    }
    let selected = &selection["selected"];
    if let Some(message) = notice("Selected observation reader", &selected["reader_losses"]) {
        let already_listed = catalog["reader_losses"].as_array().is_some_and(|readers| {
            readers.iter().take(2).any(|reader| {
                reader["run"] == selected["run"] && reader["boot"] == selected["boot"]
            })
        });
        if !already_listed {
            notices.push(message);
        }
    }
    if let Some(message) = notice("Selected recording window", &selected["window_losses"]) {
        notices.push(message);
    }
    diagnostics(catalog, selection, &mut notices);
    assert!(notices.len() <= 7);

    notices
}

fn diagnostics(catalog: &Value, selection: &Value, notices: &mut Vec<String>) {
    assert!(notices.len() <= 4);
    for (label, error) in [
        ("Recording history unavailable", &catalog["history_error"]),
        (
            "Selected history context unavailable",
            &selection["context_error"],
        ),
    ] {
        if let Some(error) = error.as_str() {
            notices.push(format!(
                "{label}: {}",
                recording_context::bounded(error, 512)
            ));
        }
    }
    // A tick with no wall time falls in no time window the pages draw, so it matters only
    // when no tick has one and the recording would otherwise look empty.
    if catalog["last_second"].is_null() {
        if let Some(count) = catalog["unknown_clock_ticks"].as_u64() {
            if count > 0 {
                notices.push(format!("{count} recorded ticks have no wall time"));
            }
        }
    }
}

fn notice(label: &str, losses: &Value) -> Option<String> {
    if losses.is_null() {
        return None;
    }
    let publications = losses["publications"].as_u64();
    let events = losses["events"].as_u64();
    let unknown = losses["unknown"].as_bool();
    if publications == Some(0) {
        if events == Some(0) {
            if unknown == Some(false) {
                return None;
            }
        }
    }
    let additional = if unknown == Some(false) {
        ""
    } else {
        "; additional loss is unknown"
    };
    Some(format!(
        "{label}: {} missed publications, {} missed events{additional}",
        publications.map_or_else(|| "Unavailable".into(), |count| count.to_string()),
        events.map_or_else(|| "Unavailable".into(), |count| count.to_string())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn degraded_history_remains_distinct_from_missing_readings() {
        // Goal: unreadable history remains visible while live readings are healthy.
        // Method: supply independent catalog and selection context failures.
        let catalog = json!({"history_error": "Saved window unavailable"});
        let selection = json!({"context_error": "Earlier context unavailable"});

        let messages = losses(&catalog, &selection);

        assert!(messages.iter().any(|message| message ==
            "Recording history unavailable: Saved window unavailable"));
        assert!(messages.iter().any(|message| message
            == "Selected history context unavailable: Earlier context unavailable"));
    }

    #[test]
    fn clockless_ticks_are_reported_without_inventing_an_extent() {
        // Goal: clockless recorded ticks must not look like a healthy empty recording.
        // Method: retain exact unknown-clock counts above f64's precise integer range.
        let catalog = json!({"unknown_clock_ticks": 9_007_199_254_740_993_u64});

        let messages = losses(&catalog, &Value::Null);

        assert!(
            messages
                .iter()
                .any(|message| message == "9007199254740993 recorded ticks have no wall time")
        );
    }

    #[test]
    fn clockless_ticks_beside_timed_ones_raise_no_notice() {
        // Goal: ticks no time window can show do not crowd the pages of a timed recording.
        // Method: report clockless ticks in a recording whose other ticks have wall time.
        let catalog = json!({"unknown_clock_ticks": 9, "last_second": 1_792_436_468_u64});

        let messages = losses(&catalog, &Value::Null);

        assert!(messages.is_empty());
    }

    #[test]
    fn history_error_display_is_bounded_without_splitting_utf8() {
        // Goal: a source error cannot create an unbounded header message.
        // Method: format a long multibyte history error through the displayed notice API.
        let catalog = json!({"history_error": "é".repeat(600)});

        let messages = losses(&catalog, &Value::Null);

        assert_eq!(messages.len(), 1);
        assert!(messages[0].ends_with('…'));
        assert!(messages[0].chars().count() <= 550);
    }

    #[test]
    fn collection_losses_keep_exact_counts_and_unknown_state() {
        // Real loss metadata must stay visible, including counts above f64's exact range.
        let catalog = json!({"reader_losses":[{"run":"run", "losses": {
            "publications":9_007_199_254_740_993_u64,"events":2,"unknown":true}}]});
        let selection = json!({"selected":{"window_losses":{
            "publications":0,"events":4,"unknown":false}}});
        let messages = losses(&catalog, &selection);
        assert_eq!(messages.len(), 2);
        assert!(messages[0].contains("9007199254740993 missed publications"));
        assert!(messages[0].contains("additional loss is unknown"));
        assert!(messages[1].contains("4 missed events"));
    }

    #[test]
    fn zero_known_losses_do_not_raise_a_notice() {
        // A healthy reader and absent uncommitted-window metadata need no warning.
        let catalog = json!({"reader_losses":[{"losses":{
            "publications":0,"events":0,"unknown":false}}]});
        assert!(losses(&catalog, &Value::Null).is_empty());
    }

    #[test]
    fn an_inspected_retired_reader_keeps_its_own_loss_notice() {
        // Another run's notice must not suppress losses on the selected older observation.
        let catalog = json!({"reader_losses":[{"run":"A","boot":"boot","losses":{
            "publications":1,"events":0,"unknown":false}}]});
        let selection = json!({"selected":{"run":"B","boot":"boot","reader_losses":{
            "publications":2,"events":0,"unknown":true}}});
        let notices = losses(&catalog, &selection);
        assert_eq!(notices.len(), 2);
        assert!(notices[1].contains("2 missed publications"));
        assert!(notices[1].contains("additional loss is unknown"));
    }
}
