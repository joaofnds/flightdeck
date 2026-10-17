use serde_json::Value;

pub fn losses(catalog: &Value, selection: &Value) -> Vec<String> {
    let mut notices = Vec::with_capacity(4);
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
    assert!(notices.len() <= 4);
    notices
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
