use std::rc::Rc;

use gpui::{IntoElement, div, prelude::*, px, rgb};
use num_traits::ToPrimitive;
use serde_json::Value;

use crate::charts::{GREEN, LINE, MUTED, ORANGE, PANEL, RED, TEXT, YELLOW};
use crate::hardware::{Readings, hardware_stage};
use crate::live_dashboard::LiveDashboard;
use crate::live_source::Snapshot;

impl LiveDashboard {
    pub fn live_hardware(&self) -> impl IntoElement {
        let (readings, failures) = self.live_hardware_readings();
        let thermal = readings.thermal.clone();
        div()
            .p(px(16.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(PANEL))
            .child(hardware_state(&thermal))
            .child(div().mt(px(16.0)).child(hardware_stage(
                readings,
                Rc::clone(&self.hardware_state),
                self.animate,
            )))
            .children(failures.into_iter().flatten().map(|error| {
                div()
                    .debug_selector(|| "schematic-error".into())
                    .text_color(rgb(ORANGE))
                    .child(error)
            }))
    }

    /// The figure's readings and what kept any of them from it: the newest second of the store's
    /// View where the page draws from it, and the worker's reading of the second it shows
    /// otherwise.
    pub(crate) fn live_hardware_readings(&self) -> (Readings, [Option<String>; 2]) {
        if let Some(readings) = self.store_view_hardware() {
            return (readings, [None, None]);
        }

        let (speeds, failure) = match self.reading_fan_speeds() {
            Ok(speeds) => (speeds, None),
            Err(error) => ([None, None], Some(error)),
        };
        let (readings, summary_failure) = self.live_hardware_projection(speeds);

        (readings, [failure, summary_failure])
    }

    pub(crate) fn live_hardware_projection(
        &self,
        speeds: [Option<f32>; 2],
    ) -> (Readings, Option<String>) {
        let summary = self
            .reading_snapshot()
            .map_or(&Value::Null, |snapshot| &snapshot.selection["summary"]);
        let mut readings = match live_readings(summary, speeds) {
            Ok(readings) => readings,
            Err(error) => return (Readings::default(), Some(error)),
        };

        match live_battery(summary) {
            Ok(battery) => {
                readings.battery = battery;

                (readings, None)
            }
            Err(error) => (readings, Some(error)),
        }
    }

    fn reading_fan_speeds(&self) -> Result<[Option<f32>; 2], String> {
        if self.preview.is_none() {
            return fan_speeds(self.snapshot.as_deref());
        }

        let Some(snapshot) = self.reading_snapshot() else {
            return Ok([None, None]);
        };

        let value = &snapshot.selection["summary"]["fan_rpm"];
        if value.is_null() {
            return Ok([None, None]);
        }

        let channels = value.as_array().ok_or("Invalid hovered fan channels")?;
        if channels.len() > 10 {
            return Err("Hovered fan channels exceed ten".into());
        }

        let mut speeds = [None, None];
        for (speed, channel) in speeds.iter_mut().zip(channels) {
            *speed = fan_speed(channel)?;
        }

        Ok(speeds)
    }
}

fn live_readings(summary: &Value, speeds: [Option<f32>; 2]) -> Result<Readings, String> {
    Ok(Readings {
        speeds,
        // A recording made before the collector read each die has only the chip's hottest
        // sensor, which belongs to neither callout, so those callouts show no temperature.
        temperatures: [
            live_number(&summary["cpu_celsius"], 1.0)?,
            live_number(&summary["gpu_celsius"], 1.0)?,
            None,
        ],
        utilization: [
            live_number(&summary["cpu_percent"], 1.0)?,
            live_number(&summary["gpu_active_percent"], 1.0)?,
        ],
        memory: [
            live_memory_in_use(summary),
            live_number(&summary["memory_total_bytes"], 1_073_741_824.0)?,
        ],
        thermal: crate::live_thermal_pressure::thermal_pressure_text(summary).into(),
        ..Readings::default()
    })
}

/// Memory in use in GiB, counted as the Memory page counts it.
fn live_memory_in_use(summary: &Value) -> Option<f32> {
    let bytes = crate::live_views::summary::memory_in_use_bytes(summary)?;
    Some(bytes.to_f32()? / 1_073_741_824.0)
}

fn live_battery(summary: &Value) -> Result<[Option<f32>; 2], String> {
    Ok([
        live_number(&summary["battery"]["charge_percent"], 1.0)?,
        live_number_signed(&summary["battery"]["power_watts"])?,
    ])
}

fn live_number(value: &Value, divisor: f32) -> Result<Option<f32>, String> {
    let Some(value) = live_number_signed(value)? else {
        return Ok(None);
    };

    if value < 0.0 {
        return Err("Schematic value is negative".into());
    }

    Ok(Some(value / divisor))
}

fn live_number_signed(value: &Value) -> Result<Option<f32>, String> {
    if value.is_null() {
        return Ok(None);
    }

    let text = crate::live_views::text(value);
    if text.len() > 64 {
        return Err("Schematic value exceeds its byte bound".into());
    }

    let value = text.parse::<f32>().map_err(|error| error.to_string())?;
    if !value.is_finite() {
        return Err("Schematic value is not finite".into());
    }

    Ok(Some(value))
}

fn fan_speeds(snapshot: Option<&Snapshot>) -> Result<[Option<f32>; 2], String> {
    let Some(snapshot) = snapshot else {
        return Ok([None, None]);
    };
    let Some(page) = snapshot.pages.get("fans") else {
        return Ok([None, None]);
    };
    let selected = &snapshot.selection["selected"];
    if selected.is_null() {
        return Err("Fan data has no selected observation".into());
    }
    for key in ["run", "boot", "publication"] {
        if page["observation"][key].is_null() {
            return Err(format!("Fan data has no observation {key}"));
        }
        if page["observation"][key] != selected[key] {
            return Err("Fan data does not match the selected observation".into());
        }
    }
    let columns = page["columns"]
        .as_array()
        .ok_or("Fan columns are unavailable")?;
    if columns.len() != 3 {
        return Err("Invalid fan column count".into());
    }
    for (column, expected) in columns.iter().zip(["publication", "fan", "rpm"]) {
        if column.as_str() != Some(expected) {
            return Err("Invalid fan table schema".into());
        }
    }
    let rows = page["rows"].as_array().ok_or("Fan rows are unavailable")?;
    if rows.len() > 10 {
        return Err("Fan rows exceed the ten-channel bound".into());
    }
    let publication = integer(&selected["publication"])?;
    fan_speeds_rows(rows, publication)
}

fn fan_speeds_rows(rows: &[Value], publication: u64) -> Result<[Option<f32>; 2], String> {
    assert!(rows.len() <= 10);
    let mut speeds = [None, None];
    let mut seen = [false; 10];
    for row in rows {
        let fields = row.as_array().ok_or("Invalid fan row")?;
        if fields.len() != 3 {
            return Err("Invalid fan row field count".into());
        }
        if integer(&fields[0])? != publication {
            return Err("Fan row does not match the selected publication".into());
        }
        let channel = usize::try_from(integer(&fields[1])?).map_err(|error| error.to_string())?;
        if channel >= seen.len() {
            return Err("Fan channel exceeds its bound".into());
        }
        if seen[channel] {
            return Err("Fan channel is repeated".into());
        }
        seen[channel] = true;
        let speed = fan_speed(&fields[2])?;
        if channel < speeds.len() {
            speeds[channel] = speed;
        }
    }
    Ok(speeds)
}

fn fan_speed(value: &Value) -> Result<Option<f32>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let text = crate::live_views::text(value);
    if text.len() > 64 {
        return Err("Fan speed exceeds its byte bound".into());
    }
    let speed = text.parse::<f32>().map_err(|error| error.to_string())?;
    if !speed.is_finite() {
        return Err("Fan speed is not finite".into());
    }
    if speed < 0.0 {
        return Err("Fan speed is negative".into());
    }
    Ok(Some(speed))
}

fn integer(value: &Value) -> Result<u64, String> {
    match value {
        Value::String(text) => {
            if text.len() > 20 {
                return Err("Fan integer exceeds its byte bound".into());
            }
            text.parse::<u64>().map_err(|error| error.to_string())
        }
        value => value.as_u64().ok_or_else(|| "Invalid fan integer".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::collections::BTreeMap;

    fn snapshot() -> Snapshot {
        let identity = json!({"run":"00000000000000000000000000000001",
            "boot":"00000000000000000000000000000002", "publication":42});
        Snapshot {
            generation: 1,
            catalog: Value::Null,
            selection: json!({"selected":identity}),
            pages: BTreeMap::from([(
                "fans".into(),
                json!({"observation":identity,
                "columns":["publication","fan","rpm"],
                "rows":[["42","0","1530"],["42","1",null]]}),
            )]),
            apps: None,
            detail: None,
            history: None,
            cpu_detail: None,
            cpu_history: None,
            resident_history: None,
            app_attribution_history: None,
            overview_app_histories: None,
            histories_pending: false,
            process_history: None,
            rankings: None,
            app_ranks: None,
            app_parts: None,
            read_ms: 0.0,
        }
    }

    #[test]
    fn fans_preserve_missing_speed() {
        // Decode the real raw-page string representation without replacing null with zero.
        assert_eq!(fan_speeds(Some(&snapshot())).unwrap(), [Some(1530.0), None]);
        assert_eq!(fan_speeds(None).unwrap(), [None, None]);
    }

    #[test]
    fn fans_refuse_other_observation() {
        // A fan payload for another exact publication must not decorate this reading.
        let mut snapshot = snapshot();
        snapshot.pages.get_mut("fans").unwrap()["observation"]["publication"] = json!(43);
        assert!(fan_speeds(Some(&snapshot)).is_err());
    }

    #[test]
    fn fans_refuse_invalid_schema() {
        // A changed raw-table layout must fail visibly rather than reorder meanings silently.
        let mut snapshot = snapshot();
        snapshot.pages.get_mut("fans").unwrap()["columns"][1] = json!("rpm");
        assert!(fan_speeds(Some(&snapshot)).is_err());
    }

    #[test]
    fn fans_refuse_duplicate_channels_and_wrong_publication() {
        // Reject ambiguity within a page even when its outer observation is correct.
        assert!(
            fan_speeds_rows(
                &[json!(["42", "0", "1000"]), json!(["42", "0", "2000"])],
                42
            )
            .is_err()
        );
        assert!(fan_speeds_rows(&[json!(["43", "0", "1000"])], 42).is_err());
    }

    #[test]
    fn fans_refuse_invalid_values_and_channel() {
        // Kernel-facing values remain unavailable when invalid instead of driving animation.
        for speed in [json!("NaN"), json!("inf"), json!("-1")] {
            assert!(fan_speeds_rows(&[json!(["42", "0", speed])], 42).is_err());
        }
        assert!(fan_speeds_rows(&[json!(["42", "10", "1000"])], 42).is_err());
    }
}

#[cfg(test)]
mod battery_hud_tests {
    use super::live_battery;
    use serde_json::json;

    #[test]
    fn battery_hud_preserves_measured_charge_and_charging_power() {
        // Goal: show the selected controller values by projecting a charging observation.
        let summary = json!({"battery":{"charge_percent":76.5,"power_watts":12.25,
            "charging":true}});

        let battery = live_battery(&summary).unwrap();

        assert_eq!(battery, [Some(76.5), Some(12.25)]);
    }

    #[test]
    fn battery_hud_preserves_signed_discharge() {
        // Goal: retain discharge direction by projecting a negative recorded power flow.
        let summary = json!({"battery":{"charge_percent":"65.5","power_watts":"-8.25",
            "charging":false}});

        let battery = live_battery(&summary).unwrap();

        assert_eq!(battery, [Some(65.5), Some(-8.25)]);
    }

    #[test]
    fn battery_hud_preserves_measured_zero() {
        // Goal: distinguish measured zero from missing charge and power in the callout.
        let summary = json!({"battery":{"charge_percent":0,"power_watts":0}});

        let battery = live_battery(&summary).unwrap();

        assert_eq!(battery, [Some(0.0), Some(0.0)]);
    }

    #[test]
    fn battery_hud_retains_charge_when_power_is_null() {
        // Goal: preserve independent availability by withholding only the measured flow.
        let summary = json!({"battery":{"charge_percent":42.5,"power_watts":null}});

        let battery = live_battery(&summary).unwrap();

        assert_eq!(battery, [Some(42.5), None]);
    }

    #[test]
    fn battery_hud_retains_power_when_charge_is_missing() {
        // Goal: preserve independent availability by omitting only the measured charge.
        let summary = json!({"battery":{"power_watts":-3.5}});

        let battery = live_battery(&summary).unwrap();

        assert_eq!(battery, [None, Some(-3.5)]);
    }

    #[test]
    fn battery_hud_keeps_missing_battery_unavailable() {
        // Goal: preserve the existing absence contract for omitted and null battery readings.
        let omitted = live_battery(&json!({})).unwrap();
        let null = live_battery(&json!({"battery":null})).unwrap();

        assert_eq!(omitted, [None, None]);
        assert_eq!(null, [None, None]);
    }

    #[test]
    fn battery_hud_rejects_malformed_charge() {
        // Goal: report malformed source data by supplying a nonnumeric charge string.
        let summary = json!({"battery":{"charge_percent":"broken","power_watts":4}});

        let battery = live_battery(&summary);

        assert!(battery.is_err());
    }

    #[test]
    fn battery_hud_rejects_nonfinite_charge() {
        // Goal: prevent an unpaintable charge value by supplying the string form of NaN.
        let summary = json!({"battery":{"charge_percent":"NaN","power_watts":4}});

        let battery = live_battery(&summary);

        assert!(battery.is_err());
    }

    #[test]
    fn battery_hud_rejects_nonfinite_power() {
        // Goal: prevent an unpaintable power value while retaining a valid charge input.
        let summary = json!({"battery":{"charge_percent":50,"power_watts":"-inf"}});

        let battery = live_battery(&summary);

        assert!(battery.is_err());
    }

    #[test]
    fn battery_hud_rejects_negative_charge() {
        // Goal: reject an impossible charge without rejecting valid signed discharge power.
        let summary = json!({"battery":{"charge_percent":-1,"power_watts":-3.5}});

        let battery = live_battery(&summary);

        assert!(battery.is_err());
    }
}

#[cfg(test)]
mod projection_boundary_tests {
    use super::{live_battery, live_readings};
    use serde_json::json;

    #[test]
    fn battery_hud_accepts_numeric_power_at_the_byte_limit() {
        // Goal: the inclusive 64-byte bound accepts a valid finite numeric power string.
        let summary = json!({"battery":{"charge_percent":50,"power_watts":"0".repeat(64)}});

        let battery = live_battery(&summary).unwrap();

        assert_eq!(battery, [Some(50.0), Some(0.0)]);
    }

    #[test]
    fn battery_hud_rejects_numeric_power_past_the_byte_limit() {
        // Goal: a 65-byte numeric string cannot bypass the bounded parser by being finite.
        let summary = json!({"battery":{"charge_percent":50,"power_watts":"0".repeat(65)}});

        let battery = live_battery(&summary);

        assert!(battery.is_err());
    }

    #[test]
    fn the_cpu_and_gpu_callouts_read_their_own_temperatures() {
        // Goal: each callout leads with its own die temperature and keeps utilization below.
        // Method: project one summary with a chip, a CPU and a GPU temperature and both loads.
        let summary = json!({"soc_celsius":58.1,"cpu_celsius":71.5,"gpu_celsius":63.25,
            "cpu_percent":25,"gpu_active_percent":40});

        let readings = live_readings(&summary, [None, None]).unwrap();

        assert_eq!(readings.temperatures[..2], [Some(71.5), Some(63.25)]);
        assert_eq!(readings.utilization, [Some(25.0), Some(40.0)]);
    }

    #[test]
    fn the_callouts_stay_unavailable_without_a_die_temperature() {
        // Goal: a die with no reading shows none, rather than the chip's hottest sensor or a zero.
        // Method: project a summary that has a chip temperature and one die temperature only.
        let summary = json!({"soc_celsius":58.1,"cpu_celsius":71.5,"gpu_celsius":null,
            "cpu_percent":25});
        let legacy = json!({"soc_celsius":58.1,"cpu_percent":25});

        let readings = live_readings(&summary, [None, None]).unwrap();
        let recorded_before = live_readings(&legacy, [None, None]).unwrap();

        assert_eq!(readings.temperatures, [Some(71.5), None, None]);
        assert_eq!(recorded_before.temperatures, [None, None, None]);
    }

    #[test]
    fn schematic_rejects_negative_cpu_utilization() {
        // Goal: signed Battery power support cannot admit negative CPU utilization.
        let summary = json!({"cpu_percent":-1});

        let readings = live_readings(&summary, [None, None]);

        assert!(readings.is_err());
    }

    #[test]
    fn schematic_rejects_negative_gpu_utilization() {
        // Goal: signed Battery power support cannot admit negative GPU utilization.
        let summary = json!({"gpu_active_percent":-1});

        let readings = live_readings(&summary, [None, None]);

        assert!(readings.is_err());
    }

    #[test]
    fn schematic_rejects_negative_installed_memory() {
        // Goal: signed Battery power support cannot admit negative installed-memory bytes.
        let summary = json!({"memory_total_bytes":-1});

        let readings = live_readings(&summary, [None, None]);

        assert!(readings.is_err());
    }
}

/// The thermal state beside its colored dot.
fn hardware_state(thermal: &str) -> gpui::Div {
    let caption = "Thermal state";
    let value = thermal;
    let color = match thermal {
        "Fair" => YELLOW,
        "Serious" => ORANGE,
        "Critical" => RED,
        "Nominal" => GREEN,
        _ => MUTED,
    };
    div()
        .flex()
        .items_center()
        .gap(px(8.0))
        .pl(px(8.0))
        .text_size(px(12.0))
        .line_height(px(18.0))
        .text_color(rgb(MUTED))
        .child(div().size(px(6.0)).rounded_full().bg(rgb(color)))
        .child(
            div().flex().gap(px(4.0)).child(caption.to_owned()).child(
                div()
                    .font_weight(gpui::FontWeight::MEDIUM)
                    .text_color(rgb(TEXT))
                    .child(value.to_owned()),
            ),
        )
}
