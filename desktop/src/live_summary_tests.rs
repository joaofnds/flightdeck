use serde_json::{Value, json};

use super::cards;

#[test]
fn battery_cards_preserve_signed_flow_false_and_exact_cycles() {
    // Goal: the Battery cards keep a discharge's sign, a measured false and an exact count.
    // Method: build the cards from an identity-matched selection with a huge cycle count.
    let selection = selection(&json!({"battery": {
        "charge_percent": 70.0, "power_watts": -2.5, "charging": false,
        "health_percent": 105.0, "cycle_count": 9_007_199_254_740_993_u64
    }}));

    let cards = cards("battery", &selection);

    assert_eq!(
        readings(&cards),
        vec![
            ("Charge", "70%".into()),
            ("Charging", "No".into()),
            ("Battery flow", "-2.5 W".into()),
            ("Health", "105%".into()),
        ]
    );
    assert_eq!(cards[3].note, "9007199254740993 cycles");
}

#[test]
fn memory_in_use_adds_app_wired_and_compressed_and_refuses_a_partial_total() {
    // Goal: memory in use is app memory, wired and compressed, and never a partial sum.
    // Method: build the Memory cards once with every quantity and once without wired.
    let mut quantities = json!({"memory_anonymous_bytes": 6_442_450_944_u64,
        "memory_purgeable_bytes": 1_073_741_824_u64, "memory_wired_bytes": 2_147_483_648_u64,
        "memory_compressed_bytes": 1_073_741_824_u64, "memory_total_bytes": 25_769_803_776_u64});

    let whole = cards("memory", &selection(&quantities));
    quantities["memory_wired_bytes"] = Value::Null;
    let partial = cards("memory", &selection(&quantities));

    assert_eq!(whole[1].reading, "8 GiB");
    assert_eq!(whole[1].note, "Of 24 GiB installed");
    assert_eq!(partial[1].reading, "Unavailable");
}

#[test]
fn a_sibling_or_retimed_observation_cannot_lend_its_cards() {
    // Goal: neither another publication nor a changed wall clock relabels a summary.
    // Method: change the summary's publication, then its wall time, and read the cards.
    let mut sibling = selection(&json!({"battery": {"charge_percent": 70}}));
    sibling["summary"]["publication"] = json!(10);
    let mut retimed = selection(&json!({"battery": {"charge_percent": 70}}));
    retimed["summary"]["wall_ns"] = json!(1_000_000_000_001_u64);

    for selection in [sibling, retimed] {
        let cards = cards("battery", &selection);
        assert!(cards.iter().all(|card| card.reading == "Unavailable"));
        assert!(cards.iter().all(|card| card.color == crate::charts::MUTED));
    }
}

#[test]
fn the_overview_cpu_and_gpu_cards_lead_with_temperature_and_keep_utilization_below() {
    // Goal: each die's card reads its temperature, with the load beneath it, on the Overview.
    // Method: build the Overview and the two resource pages from one selection that has both.
    let selection = selection(&json!({"cpu_celsius": 71.5, "gpu_celsius": 63.25,
        "cpu_percent": 25.0, "gpu_active_percent": 40.0}));

    let overview = cards("overview", &selection);
    let cpu_page = cards("cpu", &selection);
    let gpu_page = cards("gpu", &selection);

    assert_eq!(
        readings(&overview[..2]),
        vec![("CPU", "71.5 °C".into()), ("GPU", "63.2 °C".into())]
    );
    assert_eq!(overview[0].note, "Utilization 25%");
    assert_eq!(overview[1].note, "Utilization 40%");
    assert_eq!(cpu_page[0].reading, "25%");
    assert_eq!(gpu_page[0].reading, "40%");
}

#[test]
fn an_overview_card_without_a_die_temperature_stays_unavailable_and_keeps_its_load() {
    // Goal: a die with no reading shows none rather than the chip's sensor or its load.
    // Method: build the Overview from a selection with a chip temperature and loads only.
    let selection = selection(&json!({"soc_celsius": 58.1, "cpu_celsius": null,
        "cpu_percent": 25.0, "gpu_active_percent": 40.0}));

    let overview = cards("overview", &selection);

    assert_eq!(overview[0].reading, "Unavailable");
    assert_eq!(overview[0].color, crate::charts::MUTED);
    assert_eq!(overview[0].note, "Utilization 25%");
    assert_eq!(overview[1].reading, "Unavailable");
    assert_eq!(overview[1].note, "Utilization 40%");
}

fn readings(cards: &[super::Card]) -> Vec<(&'static str, String)> {
    cards
        .iter()
        .map(|card| (card.label, card.reading.clone()))
        .collect()
}

fn selection(resources: &Value) -> Value {
    let selected = json!({"run": "run", "boot": "boot", "publication": 9,
        "wall_ns": 1_000_000_000_000_u64});
    let mut summary = selected.clone();
    for (key, value) in resources.as_object().unwrap() {
        summary[key] = value.clone();
    }
    json!({"selected": selected, "summary": summary})
}
