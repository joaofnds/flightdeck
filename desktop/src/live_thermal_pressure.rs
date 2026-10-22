//! The thermal pressure macOS publishes, by the names its header gives the levels.

use serde_json::Value;

/// The thermal pressure the collector read at a reading, by macOS's name, or "Unavailable".
#[must_use]
pub fn thermal_pressure_text(point: &Value) -> &'static str {
    point["thermal_pressure"]
        .as_u64()
        .and_then(thermal_pressure_name)
        .unwrap_or("Unavailable")
}

/// macOS's name for a thermal pressure level, as `OSThermalNotification.h` lists them.
#[must_use]
pub fn thermal_pressure_name(level: u64) -> Option<&'static str> {
    match level {
        0 => Some("Nominal"),
        1 => Some("Moderate"),
        2 => Some("Heavy"),
        3 => Some("Trapping"),
        4 => Some("Sleeping"),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thermal_pressure_names_follow_the_header() {
        // Goal: each level macOS defines reads by its name and an unknown one by none.
        // Method: every defined level and the first past them.
        let names = [0, 1, 2, 3, 4, 5].map(thermal_pressure_name);

        assert_eq!(
            names,
            [
                Some("Nominal"),
                Some("Moderate"),
                Some("Heavy"),
                Some("Trapping"),
                Some("Sleeping"),
                None
            ]
        );
    }
}
