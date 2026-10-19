use gpui::{
    Bounds, IntoElement, Pixels, Window, canvas, div, fill, point, prelude::*, px, rgb, size,
};
use serde::Deserialize;
use serde_json::Value;

use crate::charts::{GREEN, MUTED, ORANGE, PANEL, RED};
use crate::live_dashboard::LiveDashboard;
use crate::live_source::{identity_signature, identity_validate};

#[derive(Debug, Deserialize)]
pub struct CPUActivity {
    pub observation: Value,
    pub previous_observation: Option<Value>,
    pub interval_available: bool,
    pub unavailable_reason: Option<String>,
    pub cpus: Vec<LogicalCPU>,
}

#[derive(Debug, Deserialize)]
pub struct LogicalCPU {
    pub cpu: u32,
    pub busy_percent: Option<f32>,
}

const CPUS_MAX: usize = 1024;
const COLUMNS_MAX: usize = 18;
const SEGMENTS: u8 = 20;
const _: () = {
    assert!(CPUS_MAX >= COLUMNS_MAX);
    assert!(SEGMENTS == 20);
};

impl CPUActivity {
    fn rows(&self) -> std::slice::Chunks<'_, LogicalCPU> {
        self.cpus.chunks(COLUMNS_MAX)
    }

    pub fn decode(value: Value, selected: &Value) -> Result<Self, String> {
        let cpus = value["cpus"]
            .as_array()
            .ok_or("Missing logical CPU readings")?;
        if cpus.len() > CPUS_MAX {
            return Err("Logical CPU count exceeds 1024".into());
        }
        for cpu in cpus {
            if !cpu["busy_percent"].is_null() {
                let value = cpu["busy_percent"]
                    .as_f64()
                    .ok_or("Invalid CPU percentage")?;
                if !value.is_finite() {
                    return Err("Non-finite logical CPU percentage".into());
                }
                if !(0.0..=100.0).contains(&value) {
                    return Err("Logical CPU percentage is outside 0 to 100".into());
                }
            }
        }
        identity_validate(&value["observation"], selected)?;
        let mut activity: Self =
            serde_json::from_value(value).map_err(|error| error.to_string())?;
        activity.validate(selected)?;
        Ok(activity)
    }

    fn validate(&mut self, selected: &Value) -> Result<(), String> {
        for field in ["run_tick", "wall_ns"] {
            if observation_number(&self.observation, field)? != observation_number(selected, field)?
            {
                return Err(format!("Logical CPU selection has a different {field}"));
            }
        }
        if self.cpus.len() > CPUS_MAX {
            return Err("Logical CPU count exceeds 1024".into());
        }
        self.cpus.sort_by_key(|reading| reading.cpu);
        for (index, reading) in self.cpus.iter().enumerate() {
            if usize::try_from(reading.cpu).map_err(|error| error.to_string())? != index {
                return Err("Logical CPU indices do not form a contiguous domain".into());
            }
            if let Some(value) = reading.busy_percent {
                if !value.is_finite() {
                    return Err("Non-finite logical CPU percentage".into());
                }
                if !(0.0..=100.0).contains(&value) {
                    return Err("Logical CPU percentage is outside 0 to 100".into());
                }
            }
        }
        self.validate_interval()
    }

    fn validate_interval(&self) -> Result<(), String> {
        if let Some(reason) = &self.unavailable_reason {
            if reason.len() > 96 {
                return Err("Logical CPU reason exceeds 96 bytes".into());
            }
        }
        if let Some(previous) = &self.previous_observation {
            let before = identity_signature(previous)?;
            let after = identity_signature(&self.observation)?;
            if before.0 != after.0 {
                return Err("Logical CPU interval crosses runs".into());
            }
            if before.1 != after.1 {
                return Err("Logical CPU interval crosses boots".into());
            }
        }
        if !self.interval_available {
            if self
                .cpus
                .iter()
                .any(|reading| reading.busy_percent.is_some())
            {
                return Err("Unavailable CPU interval contains a measured percentage".into());
            }
            return Ok(());
        }
        if self.cpus.is_empty() {
            return Err("Available CPU interval has no logical CPUs".into());
        }
        if self.unavailable_reason.is_some() {
            return Err("Available CPU interval contains an unavailable reason".into());
        }
        let previous = self
            .previous_observation
            .as_ref()
            .ok_or("Missing previous CPU reading")?;
        for field in ["publication", "run_tick"] {
            let previous = observation_number(previous, field)?;
            if previous.checked_add(1) != Some(observation_number(&self.observation, field)?) {
                return Err("Logical CPU interval is not consecutive".into());
            }
        }
        let before = observation_number(previous, "wall_ns")?;
        let after = observation_number(&self.observation, "wall_ns")?;
        if before >= after {
            return Err("Logical CPU interval clock does not advance".into());
        }
        if after / 1_000_000_000 > (before / 1_000_000_000).saturating_add(1) {
            return Err("Logical CPU interval crosses a missing second".into());
        }
        Ok(())
    }
}

impl LiveDashboard {
    pub fn cpu_activity(&self) -> impl IntoElement {
        let mut panel = div()
            .debug_selector(|| "cpu-selected-panel".into())
            .flex_1()
            .min_w_0()
            .px(px(12.0))
            .pt(px(8.0))
            .pb(px(10.0))
            .rounded(px(12.0))
            .border_1()
            .border_color(rgb(crate::charts::LINE))
            .bg(rgb(PANEL))
            .child(crate::live_chart_view::timeline::timeline_heading_text(
                "Cores at the selected time",
                "Busy scheduler ticks over total ticks of each logical CPU at the selected \
                second. 100% is one fully busy logical CPU."
                    .into(),
                "ⓘ",
            ));
        let unavailable = || {
            div()
                .debug_selector(|| "Logical CPU readings unavailable.".into())
                .child("Logical CPU readings unavailable.")
        };
        if self.reading_target_source().map(gpui::SharedString::as_ref) == Some("cpu-history-plot")
        {
            if self.cpu_inspection == Some(crate::live_cpu_history::CPUInspection::Gap) {
                return panel.child(unavailable());
            }
        }
        let Some(activity) = self
            .reading_snapshot()
            .and_then(|snapshot| snapshot.cpu_detail.as_ref())
        else {
            return panel.child(unavailable());
        };
        let second = activity.observation["wall_ns"]
            .as_u64()
            .map(|wall| wall / 1_000_000_000);
        panel = panel.child(
            div()
                .mt(px(6.0))
                .text_size(px(11.0))
                .text_color(rgb(MUTED))
                .child(match second {
                    Some(second) => format!(
                        "Activity at {} · 100% is one fully busy logical CPU",
                        crate::live_presentation::clock(second, true)
                    ),
                    None => "Selected CPU observation time unavailable".into(),
                }),
        );
        if activity.cpus.is_empty() {
            return panel.child(unavailable());
        }
        let columns = activity.cpus.len().min(COLUMNS_MAX);
        let compact = self.viewport_size.width >= px(1100.0);
        for cpus in activity.rows() {
            panel = panel.child(cpu_activity_row(cpus, columns, compact));
        }
        panel.children(activity.unavailable_reason.clone().map(|reason| {
            div()
                .text_size(px(11.0))
                .text_color(rgb(MUTED))
                .child(reason)
        }))
    }
}

#[derive(Clone, Copy)]
enum Labels {
    Percent,
    Index,
}

fn cpu_activity_row(cpus: &[LogicalCPU], columns: usize, compact: bool) -> impl IntoElement {
    assert!(columns <= COLUMNS_MAX);
    assert!(cpus.len() <= columns);
    let columns = match u8::try_from(columns) {
        Ok(columns) => columns,
        Err(error) => return div().child(format!("Invalid CPU column count: {error}")),
    };
    let values = cpus.iter().map(|cpu| cpu.busy_percent).collect::<Vec<_>>();
    div()
        .h(px(if compact { 248.0 } else { 268.0 }))
        .flex()
        .flex_col()
        .when(!compact, |row| {
            row.child(cpu_activity_labels(cpus, columns, Labels::Percent))
        })
        .child(div().h(px(10.0)))
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, (), window, _| {
                    cpu_activity_paint(&values, columns, bounds, window);
                },
            )
            .w_full()
            .h(px(210.0)),
        )
        .child(cpu_activity_labels(cpus, columns, Labels::Index))
}

fn cpu_activity_labels(cpus: &[LogicalCPU], columns: u8, kind: Labels) -> impl IntoElement {
    let height = match kind {
        Labels::Percent => 20.0,
        Labels::Index => 28.0,
    };
    let mut row = div()
        .h(px(height))
        .flex()
        .items_center()
        .text_size(px(10.0))
        .whitespace_nowrap();
    for cpu in cpus {
        let text = match kind {
            Labels::Percent => cpu
                .busy_percent
                .map_or_else(|| "N/A".into(), |value| format!("{value:.0}%")),
            Labels::Index => cpu.cpu.to_string(),
        };
        row = row.child(
            div()
                .id(gpui::SharedString::from(format!(
                    "cpu-selected-label-{}-{text}",
                    cpu.cpu
                )))
                .when(matches!(kind, Labels::Index), |label| {
                    label
                        .role(gpui::Role::Label)
                        .aria_label(format!("Selected logical CPU {}", cpu.cpu))
                        .aria_value(
                            cpu.busy_percent
                                .map_or_else(|| "Unavailable".into(), |value| format!("{value}%")),
                        )
                })
                .flex_1()
                .min_w_0()
                .text_center()
                .debug_selector(|| text.clone())
                .child(
                    div()
                        .debug_selector(|| format!("cpu-selected-value:{}:{text}", cpu.cpu))
                        .child(text),
                ),
        );
    }
    for _column in cpus.len()..usize::from(columns) {
        row = row.child(div().flex_1().min_w_0());
    }
    row
}

fn cpu_activity_paint(
    cpus: &[Option<f32>],
    columns: u8,
    bounds: Bounds<Pixels>,
    window: &mut Window,
) {
    assert!(columns > 0);
    assert!(usize::from(columns) <= COLUMNS_MAX);
    assert!(cpus.len() <= usize::from(columns));
    let width = bounds.size.width / f32::from(columns);
    for (index, value) in (0_u8..18).zip(cpus) {
        for segment in 0..SEGMENTS {
            let opacity = if segment_lit(*value, segment) {
                0.9
            } else {
                0.12
            };
            let rectangle = Bounds::new(
                point(
                    bounds.origin.x + width * f32::from(index) + px(1.0),
                    bounds.origin.y + px(f32::from(segment) * 10.5),
                ),
                size((width - px(2.0)).max(px(1.0)), px(8.8)),
            );
            window.paint_quad(
                fill(
                    rectangle,
                    crate::charts::alpha(segment_color(segment), opacity),
                )
                .corner_radii(px(1.5)),
            );
        }
    }
}

fn segment_lit(value: Option<f32>, segment: u8) -> bool {
    assert!(segment < SEGMENTS);
    let Some(value) = value else {
        return false;
    };
    let threshold = f32::from(SEGMENTS - segment) * 5.0;
    if value >= threshold {
        return true;
    }
    if segment == SEGMENTS - 1 {
        return value > 0.0;
    }
    false
}

fn segment_color(segment: u8) -> u32 {
    assert!(segment < SEGMENTS);
    let level = (SEGMENTS - segment) * 5 - 1;
    if level >= 90 {
        return RED;
    }
    if level >= 80 {
        return ORANGE;
    }
    if level >= 60 {
        return 0xf2_cc_0d;
    }
    GREEN
}

fn observation_number(observation: &Value, field: &str) -> Result<u64, String> {
    observation[field]
        .as_u64()
        .ok_or_else(|| format!("Missing CPU observation {field}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn activity() -> Value {
        json!({"observation":{
            "run":"00000000000000010000000000000002",
            "boot":"00000000000000030000000000000004", "publication":2,
            "run_tick":2,"wall_ns":2_000_000_000},
            "previous_observation":{
                "run":"00000000000000010000000000000002",
                "boot":"00000000000000030000000000000004", "publication":1,
                "run_tick":1,"wall_ns":1_000_000_000},
            "interval_available":true,"unavailable_reason":null,
            "cpus":[{"cpu":0,"busy_percent":25.0}]})
    }

    #[test]
    fn retains_missing_interval_reason_without_inventing_activity() {
        // Goal: a gap stays unavailable and carries the backend's explanation.
        // Method: decode one unavailable logical CPU with no previous observation.
        let mut payload = activity();
        payload["previous_observation"] = Value::Null;
        payload["interval_available"] = json!(false);
        payload["unavailable_reason"] = json!("Previous CPU reading unavailable");
        payload["cpus"][0]["busy_percent"] = Value::Null;
        let selected = payload["observation"].clone();

        let result = CPUActivity::decode(payload, &selected).unwrap();

        assert_eq!(result.cpus[0].busy_percent, None);
        assert_eq!(
            result.unavailable_reason.as_deref(),
            Some("Previous CPU reading unavailable")
        );
    }

    #[test]
    fn rejects_a_payload_from_another_selected_publication() {
        // Goal: activity cannot silently use a sibling observation.
        // Method: compare a valid payload with a selected identity at another publication.
        let payload = activity();
        let mut selected = payload["observation"].clone();
        selected["publication"] = json!(3);

        assert!(CPUActivity::decode(payload, &selected).is_err());
    }

    #[test]
    fn rejects_non_finite_logical_cpu_percentages() {
        // Goal: non-finite percentages cannot reach segment thresholds.
        // Method: inject NaN into the typed validation boundary after decoding.
        let payload = activity();
        let selected = payload["observation"].clone();
        let mut result = CPUActivity::decode(payload, &selected).unwrap();
        result.cpus[0].busy_percent = Some(f32::NAN);

        assert!(result.validate(&selected).is_err());
    }

    #[test]
    fn rejects_percentages_outside_the_scheduler_domain() {
        // Goal: values above fully busy cannot light a plausible CPU gauge.
        // Method: decode an otherwise valid interval containing 100.1 percent.
        let mut payload = activity();
        payload["cpus"][0]["busy_percent"] = json!(100.1);
        let selected = payload["observation"].clone();

        assert!(CPUActivity::decode(payload, &selected).is_err());
    }

    #[test]
    fn rejects_out_of_domain_percentages_before_narrowing_to_f32() {
        // Goal: rounding to f32 cannot admit a payload just above fully busy.
        // Method: decode a value above 100 that would round to exactly 100 in f32.
        let mut payload = activity();
        payload["cpus"][0]["busy_percent"] = json!(100.000_000_1);
        let selected = payload["observation"].clone();

        assert!(CPUActivity::decode(payload, &selected).is_err());
    }

    #[test]
    fn rejects_non_contiguous_logical_cpu_indices() {
        // Goal: a missing scheduler index cannot shift the columns of another CPU.
        // Method: provide CPU index one as the only reading.
        let mut payload = activity();
        payload["cpus"][0]["cpu"] = json!(1);
        let selected = payload["observation"].clone();

        assert!(CPUActivity::decode(payload, &selected).is_err());
    }

    #[test]
    fn rejects_measurements_on_an_unavailable_interval() {
        // Goal: a missing interval cannot retain a percentage from another interval.
        // Method: mark a measured interval unavailable without clearing its reading.
        let mut payload = activity();
        payload["interval_available"] = json!(false);
        let selected = payload["observation"].clone();

        assert!(CPUActivity::decode(payload, &selected).is_err());
    }

    #[test]
    fn rejects_a_counter_interval_crossing_a_missing_second() {
        // Goal: consecutive publication numbers alone cannot bridge a missing wall second.
        // Method: move the current observation two seconds past its predecessor.
        let mut payload = activity();
        payload["observation"]["wall_ns"] = json!(3_000_000_000_u64);
        let selected = payload["observation"].clone();

        assert!(CPUActivity::decode(payload, &selected).is_err());
    }

    #[test]
    fn lights_segment_thresholds_and_keeps_missing_readings_dark() {
        // Goal: the first five-percent segment also exposes small positive CPU activity.
        // Method: compare values at the bottom and top thresholds, plus an absent reading.
        assert!(segment_lit(Some(0.1), 19));
        assert!(!segment_lit(Some(0.0), 19));
        assert!(!segment_lit(Some(4.9), 18));
        assert!(segment_lit(Some(10.0), 18));
        assert!(!segment_lit(Some(99.0), 0));
        assert!(segment_lit(Some(100.0), 0));
        assert!(!segment_lit(None, 19));
    }

    #[test]
    fn groups_scheduler_domains_into_rows_of_eighteen() {
        // Goal: larger scheduler domains keep eighteen fixed columns with a partial last row.
        // Method: decode nineteen CPUs and inspect its bounded row grouping.
        let mut payload = activity();
        payload["cpus"] = json!(
            (0..19)
                .map(|cpu| json!({"cpu":cpu,"busy_percent":null}))
                .collect::<Vec<_>>()
        );
        let selected = payload["observation"].clone();

        let result = CPUActivity::decode(payload, &selected).unwrap();
        let rows = result.rows().map(<[LogicalCPU]>::len).collect::<Vec<_>>();

        assert_eq!(rows, vec![18, 1]);
    }
}
