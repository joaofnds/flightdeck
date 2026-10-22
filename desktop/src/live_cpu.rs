use gpui::{
    Bounds, IntoElement, Pixels, Window, canvas, div, fill, point, prelude::*, px, rgb, size,
};
use serde::Deserialize;
use serde_json::Value;

use crate::charts::{PANEL, TEXT};
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

#[derive(Debug, Deserialize, PartialEq)]
pub struct LogicalCPU {
    pub cpu: u32,
    pub busy_percent: Option<f32>,
}

/// A run of adjacent logical CPUs that share a performance level.
#[derive(Debug, PartialEq, Eq)]
struct CoreGroup {
    title: String,
    cpus: u8,
}

/// The performance levels of `cpu_count` logical CPUs in index order, which is the slower level
/// first. One group of all the CPUs when the levels are unknown or do not cover the CPUs
/// exactly, so the headings keep their row whatever the reading recorded.
fn core_groups(
    cpu_count: usize,
    levels: Option<&[crate::live_inventory::PerformanceLevel; 2]>,
) -> Vec<CoreGroup> {
    let Ok(all_cpus) = u8::try_from(cpu_count.min(COLUMNS_MAX)) else {
        unreachable!("COLUMNS_MAX fits a u8");
    };
    let all = vec![CoreGroup {
        title: format!("Logical CPUs · {cpu_count}"),
        cpus: all_cpus,
    }];
    let Some([fastest, slower]) = levels else {
        return all;
    };
    let (Ok(fastest_cpus), Ok(slower_cpus)) = (
        u8::try_from(fastest.logical_cpus),
        u8::try_from(slower.logical_cpus),
    ) else {
        return all;
    };
    let covered = usize::from(fastest_cpus) + usize::from(slower_cpus);
    if covered != cpu_count {
        return all;
    }

    vec![
        CoreGroup {
            title: format!("{} · {slower_cpus}", slower.cores),
            cpus: slower_cpus,
        },
        CoreGroup {
            title: format!("{} · {fastest_cpus}", fastest.cores),
            cpus: fastest_cpus,
        },
    ]
}

/// The names of the performance levels above the bars, each as wide as its bars.
fn core_group_headings(groups: &[CoreGroup]) -> impl IntoElement {
    let total: u8 = groups.iter().map(|group| group.cpus).sum();
    assert!(total > 0);
    let mut row = div()
        .flex()
        .mx(px(PLOT_INSET))
        .mb(px(6.0))
        .text_size(px(11.0))
        .text_color(rgb(crate::charts::MUTED));
    for (index, group) in groups.iter().enumerate() {
        let share = f32::from(group.cpus) / f32::from(total);
        row = row.child(
            div()
                .w(gpui::relative(share))
                .when(index > 0, |heading| {
                    heading.border_l_1().border_color(rgb(crate::charts::LINE))
                })
                .pb(px(3.0))
                .border_b_1()
                .border_color(rgb(crate::charts::LINE))
                .px(px(4.0))
                .truncate()
                .child(group.title.clone()),
        );
    }
    row
}

const CPUS_MAX: usize = 1024;
const COLUMNS_MAX: usize = 18;
const ACTIVITY_UNAVAILABLE: &str = "Logical CPU readings unavailable.";
const SEGMENTS: u8 = 20;
const _: () = {
    assert!(CPUS_MAX >= COLUMNS_MAX);
    assert!(SEGMENTS == 20);
};

/// The rows the bars of `cpus` take, each of at most `COLUMNS_MAX`.
fn cpu_rows(cpus: &[LogicalCPU]) -> std::slice::Chunks<'_, LogicalCPU> {
    cpus.chunks(COLUMNS_MAX)
}

impl CPUActivity {
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
        let panel = div()
            .debug_selector(|| "cpu-selected-panel".into())
            .flex_1()
            .min_w_0()
            .px(px(12.0))
            .pt(px(8.0))
            .pb(px(8.0))
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
        let levels = self.performance_levels();
        if self.reading_target_source().map(gpui::SharedString::as_ref) == Some("cpu-history-plot")
        {
            if self.cpu_inspection == Some(crate::live_cpu_history::CPUInspection::Gap) {
                return cpu_activity_absent(panel, ACTIVITY_UNAVAILABLE, levels.as_ref());
            }
        }
        let Some(activity) = self
            .reading_snapshot()
            .and_then(|snapshot| snapshot.cpu_detail.as_ref())
        else {
            let notice = self.absent_reading(ACTIVITY_UNAVAILABLE);
            return cpu_activity_absent(panel, notice, levels.as_ref());
        };
        if activity.cpus.is_empty() {
            return cpu_activity_absent(panel, ACTIVITY_UNAVAILABLE, levels.as_ref());
        }
        let groups = core_groups(activity.cpus.len(), levels.as_ref());
        cpu_activity_bars(panel, &activity.cpus, &groups)
    }

    /// The performance levels the shown reading recorded, or none before a reading.
    pub(crate) fn performance_levels(
        &self,
    ) -> Option<[crate::live_inventory::PerformanceLevel; 2]> {
        let snapshot = self.reading_snapshot()?;
        crate::live_inventory::performance_levels(&snapshot.selection["summary"])
    }
}

/// The bars of `cpus` under `panel`'s heading, in rows of at most `COLUMNS_MAX`, with the
/// performance levels of `groups` named above them.
fn cpu_activity_bars(mut panel: gpui::Div, cpus: &[LogicalCPU], groups: &[CoreGroup]) -> gpui::Div {
    assert!(!cpus.is_empty());
    assert!(!groups.is_empty());
    let columns = cpus.len().min(COLUMNS_MAX);
    panel = panel.child(core_group_headings(groups));
    for row in cpu_rows(cpus) {
        panel = panel.child(cpu_activity_row(row, columns));
    }
    panel
}

/// The panel with no reading of its CPUs: empty bars with `notice` over them, so the panel
/// keeps the size its readings give it. The bars are the CPUs of `levels`, or before a reading
/// one full row of them, which is the size the readings of a Mac of up to `COLUMNS_MAX` logical
/// CPUs give the panel. The window reads no CPU count of its own.
fn cpu_activity_absent(
    panel: gpui::Div,
    notice: &'static str,
    levels: Option<&[crate::live_inventory::PerformanceLevel; 2]>,
) -> gpui::Div {
    let cpus = cpu_activity_absent_cpus(levels);
    let groups = match levels {
        Some(_) => core_groups(cpus.len(), levels),
        None => vec![CoreGroup {
            title: "Loading".into(),
            cpus: 1,
        }],
    };

    cpu_activity_bars(panel.relative(), &cpus, &groups).child(
        crate::live_chart_view::plot_notice(notice.into())
            .absolute()
            .inset_0(),
    )
}

/// Empty readings of the logical CPUs of `levels`, or of one full row where the levels are
/// unknown or count no CPUs or more than `CPUS_MAX`.
fn cpu_activity_absent_cpus(
    levels: Option<&[crate::live_inventory::PerformanceLevel; 2]>,
) -> Vec<LogicalCPU> {
    let Ok(row) = u32::try_from(COLUMNS_MAX) else {
        unreachable!("COLUMNS_MAX fits a u32");
    };
    let Ok(cpus_max) = u32::try_from(CPUS_MAX) else {
        unreachable!("CPUS_MAX fits a u32");
    };
    let mut count = row;
    if let Some([fastest, slower]) = levels {
        if let Some(recorded) = fastest.logical_cpus.checked_add(slower.logical_cpus) {
            if 0 < recorded {
                if recorded <= cpus_max {
                    count = recorded;
                }
            }
        }
    }

    (0..count)
        .map(|cpu| LogicalCPU {
            cpu,
            busy_percent: None,
        })
        .collect()
}

/// The prototype's bars: a 244 pixel plot of twenty segments per core, with each core's index
/// under its bar.
const PLOT_HEIGHT: f32 = 244.0;
const PLOT_INSET: f32 = 4.0;
const SEGMENTS_TOP: f32 = 12.0;
const INDEX_TOP: f32 = 226.5;

fn cpu_activity_row(cpus: &[LogicalCPU], columns: usize) -> impl IntoElement {
    assert!(columns <= COLUMNS_MAX);
    assert!(cpus.len() <= columns);
    let columns = match u8::try_from(columns) {
        Ok(columns) => columns,
        Err(error) => return div().child(format!("Invalid CPU column count: {error}")),
    };
    let values = cpus.iter().map(|cpu| cpu.busy_percent).collect::<Vec<_>>();
    div()
        .relative()
        .h(px(PLOT_HEIGHT))
        .child(
            canvas(
                |_, _, _| (),
                move |bounds, (), window, _| {
                    cpu_activity_paint(&values, columns, bounds, window);
                },
            )
            .absolute()
            .size_full(),
        )
        .child(cpu_activity_labels(cpus, columns))
}

fn cpu_activity_labels(cpus: &[LogicalCPU], columns: u8) -> impl IntoElement {
    let mut row = div()
        .absolute()
        .left(px(PLOT_INSET))
        .right(px(PLOT_INSET))
        .top(px(INDEX_TOP))
        .h(px(14.0))
        .flex()
        .items_center()
        .text_size(px(10.0))
        .text_color(rgb(TEXT))
        .whitespace_nowrap();
    for cpu in cpus {
        let text = cpu.cpu.to_string();
        row = row.child(
            div()
                .id(gpui::SharedString::from(format!(
                    "cpu-selected-label-{}-{text}",
                    cpu.cpu
                )))
                .role(gpui::Role::Label)
                .aria_label(format!("Selected logical CPU {}", cpu.cpu))
                .aria_value(
                    cpu.busy_percent
                        .map_or_else(|| "Unavailable".into(), |value| format!("{value}%")),
                )
                .flex_1()
                .min_w_0()
                .text_center()
                .debug_selector(|| text.clone())
                .child(text),
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
    let width = (bounds.size.width - px(2.0 * PLOT_INSET)) / f32::from(columns);
    for (index, value) in (0_u8..18).zip(cpus) {
        for segment in 0..SEGMENTS {
            let rectangle = Bounds::new(
                point(
                    bounds.origin.x + px(PLOT_INSET) + width * f32::from(index),
                    bounds.origin.y + px(SEGMENTS_TOP + f32::from(segment) * 10.5),
                ),
                size((width - px(2.0)).max(px(1.0)), px(8.8)),
            );
            window.paint_quad(fill(rectangle, segment_fill(*value, segment)).corner_radii(px(1.5)));
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

/// A lit segment takes the colour the heatmap gives its core's reading, so a bar and the cell
/// for the same reading agree. A dark segment keeps a faint tint of the level it stands for.
fn segment_fill(value: Option<f32>, segment: u8) -> gpui::Rgba {
    assert!(segment < SEGMENTS);
    if let Some(value) = value {
        if segment_lit(Some(value), segment) {
            return crate::charts::utilization_color(f64::from(value));
        }
    }
    let mut tint = crate::charts::utilization_color(f64::from(SEGMENTS - segment) * 5.0 - 2.5);
    tint.a = 0.12;
    tint
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

    fn level(name: &str, logical_cpus: u32) -> crate::live_inventory::PerformanceLevel {
        crate::live_inventory::PerformanceLevel {
            name: name.into(),
            cores: format!("{name} cores"),
            cluster_cores: format!("{} cores", name.to_lowercase()),
            logical_cpus,
        }
    }

    #[test]
    fn cores_are_grouped_by_performance_level_in_index_order() {
        // Goal: the bars name the slower cores first, since macOS numbers them first.
        // Method: group 18 logical CPUs of a Mac with 6 super and 12 performance cores.
        let levels = [level("Super", 6), level("Performance", 12)];

        let groups = core_groups(18, Some(&levels));

        assert_eq!(
            groups,
            [
                CoreGroup {
                    title: "Performance cores · 12".into(),
                    cpus: 12
                },
                CoreGroup {
                    title: "Super cores · 6".into(),
                    cpus: 6
                },
            ]
        );
    }

    #[test]
    fn cores_share_one_heading_when_the_levels_do_not_cover_them() {
        // Goal: a count that disagrees with the levels never labels a core with the wrong type,
        // and the heading row stays, so a reading without levels moves nothing.
        // Method: group 17 CPUs against 18 cores of levels, then 18 CPUs with no levels.
        let levels = [level("Super", 6), level("Performance", 12)];

        let groups = [core_groups(17, Some(&levels)), core_groups(18, None)];

        assert_eq!(
            groups,
            [17, 18].map(|count| vec![CoreGroup {
                title: format!("Logical CPUs · {count}"),
                cpus: count,
            }])
        );
    }

    #[test]
    fn an_absent_reading_draws_one_full_row_before_the_levels_are_known() {
        // Goal: before a reading the panel takes the size of one full row, from no fact about
        // the Mac, and with a reading it takes the size of the CPUs the reading recorded.
        // Method: count the empty bars with no levels, with 18, and with levels counting none.
        let levels = [level("Super", 6), level("Performance", 12)];
        let large = [level("Super", 16), level("Performance", 16)];
        let empty = [level("Super", 0), level("Performance", 0)];

        let counts = [None, Some(&levels), Some(&large), Some(&empty)]
            .map(|levels| cpu_activity_absent_cpus(levels).len());

        assert_eq!(counts, [COLUMNS_MAX, 18, 32, COLUMNS_MAX]);
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
    fn colors_a_lit_segment_as_the_heatmap_colors_the_same_reading() {
        // Goal: a core's bar and its heatmap cell never disagree about the reading's colour.
        // Method: compare every lit segment with the shared scale, and a dark one with it.
        for value in [3.0_f32, 42.0, 61.0, 75.0, 88.0, 100.0] {
            let expected = crate::charts::utilization_color(f64::from(value));
            for segment in 0..SEGMENTS {
                if segment_lit(Some(value), segment) {
                    assert_eq!(segment_fill(Some(value), segment), expected);
                } else {
                    assert!(segment_fill(Some(value), segment).a < 0.2);
                }
            }
        }
        assert!(segment_fill(None, 0).a < 0.2);
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
        let rows = cpu_rows(&result.cpus)
            .map(<[LogicalCPU]>::len)
            .collect::<Vec<_>>();

        assert_eq!(rows, vec![18, 1]);
    }
}
