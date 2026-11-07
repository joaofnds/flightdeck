use std::rc::Rc;

use gpui::{IntoElement, div, prelude::*, px, rgb};
use num_traits::ToPrimitive;
use serde_json::Value;

use crate::charts::{GREEN, LINE, MUTED, ORANGE, PANEL, RED, TEXT, YELLOW};
use crate::hardware::{Readings, hardware_stage};
use crate::live_dashboard::LiveDashboard;

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
            .child(
                div()
                    .mt(px(16.0))
                    .child(hardware_stage(readings, Rc::clone(&self.hardware_state))),
            )
            .children(failures.into_iter().flatten().map(|error| {
                div()
                    .debug_selector(|| "schematic-error".into())
                    .text_color(rgb(ORANGE))
                    .child(error)
            }))
            .children(self.hardware_state.animation_failure().map(|error| {
                div()
                    .debug_selector(|| "hardware-animation-error".into())
                    .text_color(rgb(ORANGE))
                    .child(format!(
                        "Fan animation unavailable; showing still fans. {error}"
                    ))
            }))
    }

    /// The figure's readings and what kept any of them from it: the second the store's View
    /// shows where the page draws from it, and the worker's reading of the second it shows
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
        let Some(snapshot) = self.reading_snapshot() else {
            return Ok([None, None]);
        };

        fan_speeds(&snapshot.selection["summary"]["fan_rpm"])
    }
}

fn fan_speeds(channels: &Value) -> Result<[Option<f32>; 2], String> {
    if channels.is_null() {
        return Ok([None, None]);
    }

    let channels = channels.as_array().ok_or("Invalid fan channels")?;
    if channels.len() > 10 {
        return Err("Fan channels exceed ten".into());
    }

    let mut speeds = [None, None];
    for (speed, channel) in speeds.iter_mut().zip(channels) {
        *speed = fan_speed(channel)?;
    }

    Ok(speeds)
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
    crate::live_projection::finite_number(value)
        .map(Some)
        .ok_or_else(|| "Schematic value is not a finite number".into())
}

fn fan_speed(value: &Value) -> Result<Option<f32>, String> {
    if value.is_null() {
        return Ok(None);
    }
    let speed =
        crate::live_projection::finite_number(value).ok_or("Fan speed is not a finite number")?;
    if speed < 0.0 {
        return Err("Fan speed is negative".into());
    }
    Ok(Some(speed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_reading_keeps_a_fan_it_could_not_read_missing() {
        // Goal: an unread channel is no fan speed, rather than a stopped fan.
        // Method: read two channels, the second of them unread.

        let speeds = fan_speeds(&json!([1530.0, null]));

        assert_eq!(speeds.unwrap(), [Some(1530.0), None]);
    }

    #[test]
    fn a_reading_without_fans_has_no_fan_speeds() {
        // Goal: a reading that measured no fans draws none.
        // Method: read the field a second with no fan readings leaves empty.

        let speeds = fan_speeds(&Value::Null);

        assert_eq!(speeds.unwrap(), [None, None]);
    }

    #[test]
    fn fan_speeds_refuse_more_channels_than_a_mac_has() {
        // Goal: a reading past the bound fails visibly rather than dropping channels silently.
        // Method: read eleven channels, one past the bound.

        let speeds = fan_speeds(&json!(vec![1.0; 11]));

        assert!(speeds.is_err());
    }

    #[test]
    fn fan_speeds_refuse_a_negative_speed() {
        // Goal: a speed no fan can turn at never drives the animation.
        // Method: read a channel below zero.

        let speeds = fan_speeds(&json!([-1.0]));

        assert!(speeds.is_err());
    }

    #[test]
    fn fan_speeds_refuse_a_speed_past_what_a_chart_can_hold() {
        // Goal: a speed that overflows the drawn number never reads as infinite.
        // Method: read a channel far past the largest `f32`.

        let speeds = fan_speeds(&json!([1e300]));

        assert!(speeds.is_err());
    }

    #[test]
    fn fan_speeds_refuse_text() {
        // Goal: only a number reads as a fan speed.
        // Method: read a channel holding digits as text.

        let speeds = fan_speeds(&json!(["1530"]));

        assert!(speeds.is_err());
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
        let summary = json!({"battery":{"charge_percent":65.5,"power_watts":-8.25,
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
    fn battery_hud_refuses_power_held_as_text() {
        // Goal: only a number reads as a battery's power.
        // Method: project a summary whose power holds digits as text.
        let summary = json!({"battery":{"charge_percent":50,"power_watts":"8.25"}});

        let battery = live_battery(&summary);

        assert!(battery.is_err());
    }

    #[test]
    fn battery_hud_refuses_power_past_what_a_chart_can_hold() {
        // Goal: a power that overflows the drawn number never reads as infinite.
        // Method: project a summary whose power lies far past the largest `f32`.
        let summary = json!({"battery":{"charge_percent":50,"power_watts":1e300}});

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
