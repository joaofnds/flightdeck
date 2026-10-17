use gpui::{Context, IntoElement, div, prelude::*, px, rgb};
use serde_json::Value;

use crate::charts::{BLUE, GREEN, GRID, MUTED, ORANGE, PANEL, PURPLE};
use crate::dashboard::control;
use crate::data::reading;
use crate::live_dashboard::LiveDashboard;
use crate::live_projection::quantity;
use crate::live_source::Snapshot;

impl LiveDashboard {
    pub fn live_hardware(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let (speeds, failure) = match fan_speeds(self.snapshot.as_deref()) {
            Ok(speeds) => (speeds, None),
            Err(error) => ([None, None], Some(error)),
        };
        let summary = self
            .snapshot
            .as_ref()
            .map_or(&Value::Null, |snapshot| &snapshot.selection["summary"]);
        div()
            .p(px(12.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(rgb(GRID))
            .bg(rgb(PANEL))
            .child(
                div()
                    .flex()
                    .justify_between()
                    .items_center()
                    .child("MacBook · live recorded readings")
                    .child(
                        control(if self.animate {
                            "Pause animation"
                        } else {
                            "Animate fans"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.animate = !this.animate;
                            cx.notify();
                        })),
                    ),
            )
            .child(Self::live_hardware_values(summary, speeds))
            .child(
                div()
                    .flex()
                    .justify_center()
                    .child(crate::hardware::hardware_stage(
                        speeds,
                        self.started.elapsed().as_secs_f32(),
                        self.animate,
                    )),
            )
            .child(
                div()
                    .text_color(rgb(MUTED))
                    .text_size(px(11.0))
                    .child(concat!(
                        "16-inch assembly illustration; fan channel sides unverified. ",
                        "Thermal: Unavailable.",
                    )),
            )
            .children(failure.map(|error| div().text_color(rgb(ORANGE)).child(error)))
    }

    fn live_hardware_values(summary: &Value, speeds: [Option<f32>; 2]) -> impl IntoElement {
        let mut values = div().flex().flex_wrap().gap(px(12.0));
        for (label, primary, secondary, tint) in [
            (
                "CPU",
                quantity(summary, "cpu_percent", "%", 1.0),
                "Temperature unavailable",
                BLUE,
            ),
            (
                "GPU",
                quantity(summary, "gpu_active_percent", "%", 1.0),
                "Temperature unavailable",
                PURPLE,
            ),
            (
                "Installed memory",
                quantity(summary, "memory_total_bytes", "GiB", 1_073_741_824.0),
                "Used memory / temperature unavailable",
                GREEN,
            ),
            (
                "Fan 0",
                reading(speeds[0], "rpm"),
                fan_state(speeds[0]),
                BLUE,
            ),
            (
                "Fan 1",
                reading(speeds[1], "rpm"),
                fan_state(speeds[1]),
                BLUE,
            ),
            ("Battery", "Unavailable".into(), "Power unavailable", ORANGE),
        ] {
            values = values.child(
                div()
                    .flex_1()
                    .min_w(px(115.0))
                    .p(px(10.0))
                    .child(label)
                    .child(
                        div()
                            .text_size(px(20.0))
                            .text_color(rgb(tint))
                            .child(primary),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(rgb(MUTED))
                            .child(secondary),
                    ),
            );
        }
        values
    }
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

fn fan_state(speed: Option<f32>) -> &'static str {
    match speed {
        None => "Unavailable",
        Some(0.0) => "Stopped",
        Some(_) => "Running",
    }
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
        assert_eq!(fan_state(Some(0.0)), "Stopped");
        assert_eq!(fan_state(None), "Unavailable");
    }
}
