use std::collections::BTreeMap;

use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px, rgb};
use serde_json::{Value, json};

use crate::charts::{GRID, MUTED};
use crate::live_app_views::{app_cells, memory, rate};
use crate::live_dashboard::LiveDashboard;

pub struct Executable {
    pub key: String,
    pub name: String,
    pub instances: u32,
    pub cpu_ns: Subtotal,
    pub energy_nj: Subtotal,
    pub resident_bytes: Subtotal,
    pub footprint_bytes: Subtotal,
    pub disk_read_bytes: Subtotal,
    pub disk_written_bytes: Subtotal,
    pub disk_io_bytes: Subtotal,
    pub idle_wakeups: Subtotal,
    pub pageins: Subtotal,
    pub threads: Subtotal,
    elapsed_ns: Option<u64>,
}

#[derive(Debug, Default, PartialEq)]
pub struct Subtotal {
    pub known: u64,
    pub measured: u32,
    pub unavailable: u32,
    pub overflowed: bool,
}

/// The process group table's metric columns, as the prototype orders them.
pub const COLUMNS: [&str; 11] = [
    "CPU",
    "Resident",
    "Footprint",
    "Network",
    "Disk I/O",
    "GPU time",
    "Power",
    "Wakeups/s",
    "Threads",
    "Open files",
    "Sockets",
];
/// The reading of a column no source records per process, which the prototype marks so.
const NOT_CAPTURED: &str = "—";
/// The index of Disk I/O in `COLUMNS`.
const DISK_COLUMN: usize = 4;
const PROCESS_ROWS_MAX: usize = 512;
const PROGRAM_BYTES_MAX: usize = 4095;
const HEX_BYTES_MAX: usize = PROGRAM_BYTES_MAX * 2;
const DISPLAY_BYTES_MAX: usize = PROGRAM_BYTES_MAX * 3;
const UNAVAILABLE: &str = "unavailable";
const _: () = {
    assert!(PROCESS_ROWS_MAX <= 4_294_967_295);
    assert!(DISK_COLUMN < COLUMNS.len());
    assert!(HEX_BYTES_MAX >= PROGRAM_BYTES_MAX);
    assert!(DISPLAY_BYTES_MAX >= PROGRAM_BYTES_MAX);
};

pub fn groups(detail: Option<&Value>) -> Result<Vec<Executable>, String> {
    let Some(detail) = detail.filter(|detail| !detail.is_null()) else {
        return Ok(Vec::new());
    };
    let processes = detail["processes"]
        .as_array()
        .ok_or("Missing executable process rows")?;
    if processes.len() > PROCESS_ROWS_MAX {
        return Err("Executable process page exceeds 512 rows".to_owned());
    }
    let elapsed_ns = counter_optional(&detail["elapsed_ns"])?;
    let mut groups = BTreeMap::<String, Executable>::new();
    for process in processes {
        let (key, name) = groups_identity(process);
        let group = groups.entry(key.clone()).or_insert_with(|| Executable {
            key,
            name,
            instances: 0,
            cpu_ns: Subtotal::default(),
            energy_nj: Subtotal::default(),
            resident_bytes: Subtotal::default(),
            footprint_bytes: Subtotal::default(),
            disk_read_bytes: Subtotal::default(),
            disk_written_bytes: Subtotal::default(),
            disk_io_bytes: Subtotal::default(),
            idle_wakeups: Subtotal::default(),
            pageins: Subtotal::default(),
            threads: Subtotal::default(),
            elapsed_ns,
        });
        groups_add(group, process)?;
    }
    let mut groups: Vec<_> = groups.into_values().collect();
    groups.sort_by(|first, second| {
        first
            .name
            .cmp(&second.name)
            .then(first.key.cmp(&second.key))
    });
    assert!(groups.len() <= PROCESS_ROWS_MAX);
    Ok(groups)
}

pub fn matches(process: &Value, key: &str) -> bool {
    match groups_bytes(process) {
        Some(bytes) => key
            .strip_prefix("hex:")
            .is_some_and(|selected| selected.eq_ignore_ascii_case(bytes)),
        None => key == UNAVAILABLE,
    }
}

impl Subtotal {
    fn add(&mut self, value: Option<u64>) {
        match value {
            Some(value) => {
                if let Some(known) = self.known.checked_add(value) {
                    self.known = known;
                    self.measured += 1;
                } else {
                    self.unavailable += 1;
                    self.overflowed = true;
                }
            }
            None => self.unavailable += 1,
        }
        assert!(self.measured + self.unavailable <= 512);
    }

    pub fn value(&self) -> Option<u64> {
        if self.overflowed {
            return None;
        }
        if self.measured == 0 {
            return None;
        }
        Some(self.known)
    }

    fn json(&self) -> Value {
        json!({"known":self.known.to_string(), "measured_members":self.measured,
            "unavailable_members":self.unavailable, "overflowed":self.overflowed})
    }
}

impl Executable {
    fn metrics(&self) -> [String; COLUMNS.len()] {
        let cpu = self.cpu_ns.value().and_then(|cpu_ns| {
            viewer::apps::cpu_percent_one_core(viewer::apps::CpuRate {
                cpu_ns,
                elapsed_ns: self.elapsed_ns?,
            })
        });
        let power = self.energy_nj.value().and_then(|energy_nj| {
            viewer::apps::energy_watts(viewer::apps::EnergyRate {
                energy_nj,
                elapsed_ns: self.elapsed_ns?,
            })
        });
        [
            subtotal_rate(cpu, &self.cpu_ns, "%"),
            subtotal_memory(&self.resident_bytes),
            subtotal_memory(&self.footprint_bytes),
            NOT_CAPTURED.into(),
            self.disk_rate(),
            NOT_CAPTURED.into(),
            subtotal_rate(power, &self.energy_nj, "W"),
            self.event_rate(&self.idle_wakeups, "/s"),
            self.threads.value().map_or_else(
                || "Unavailable".into(),
                |value| {
                    format!(
                        "{value}{}",
                        if self.threads.unavailable > 0 {
                            " *"
                        } else {
                            ""
                        }
                    )
                },
            ),
            NOT_CAPTURED.into(),
            NOT_CAPTURED.into(),
        ]
    }

    fn event_rate(&self, amount: &Subtotal, unit: &str) -> String {
        let rate = amount.value().and_then(|events| {
            viewer::apps::events_per_second(viewer::apps::EventRate {
                events,
                elapsed_ns: self.elapsed_ns?,
            })
        });
        let suffix = if amount.unavailable > 0 { " *" } else { "" };

        format!("{}{suffix}", crate::live_app_views::event_rate(rate, unit))
    }

    fn disk_rate(&self) -> String {
        let rate = self.disk_io_bytes.value().and_then(|bytes| {
            viewer::apps::bytes_per_second(viewer::apps::BytesRate {
                bytes,
                elapsed_ns: self.elapsed_ns?,
            })
        });
        let reading = crate::live_app_views::disk_rate(rate);
        if self.disk_io_bytes.unavailable > 0 {
            return format!("{reading} *");
        }

        reading
    }

    pub(crate) fn json(&self) -> Value {
        json!({"executable_bytes":self.key, "instances":self.instances,
            "scope":"loaded process page only", "cpu_ns":self.cpu_ns.json(),
            "energy_nj":self.energy_nj.json(), "resident_bytes":self.resident_bytes.json(),
            "footprint_bytes":self.footprint_bytes.json(),
            "disk_read_bytes":self.disk_read_bytes.json(),
            "disk_written_bytes":self.disk_written_bytes.json(),
            "disk_io_bytes":self.disk_io_bytes.json(),
            "idle_wakeups":self.idle_wakeups.json(), "pageins":self.pageins.json(),
            "threads":self.threads.json()})
    }
}

impl LiveDashboard {
    pub fn executable_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(group) = self.reading_executables().get(index) else {
            return div()
                .text_color(rgb(MUTED))
                .child("Executable unavailable")
                .into_any_element();
        };
        let selected = self.request.executable.as_ref() == Some(&group.key);
        let key = group.key.clone();
        let metrics = group.metrics();
        let disk_selector = format!("executable-disk:{}:{}", group.key, metrics[DISK_COLUMN]);
        let selection_help =
            if crate::live_process_history::ProcessHistory::filter_reason(Some(&group.key))
                .is_some()
            {
                "Filter instances; named history unavailable"
            } else {
                "Select captured group history and matching instances"
            };

        div()
            .id(SharedString::from(format!("executable-{index}")))
            .accessibility_id(format!("flightdeck.executable.{}", group.key))
            .role(gpui::Role::Button)
            .aria_label(format!(
                "{}; {} instances; loaded process page subtotal; {selection_help}",
                group.name, group.instances,
            ))
            .aria_selected(selected)
            .py(px(10.0))
            .min_w_0()
            .max_w_full()
            .overflow_x_scroll()
            .restrict_scroll_to_axis()
            .track_scroll(&self.app_table_scroll)
            .border_b_1()
            .border_color(rgb(GRID))
            .cursor_pointer()
            .focusable()
            .tab_stop(true)
            .focus(|style| style.bg(rgb(0x32_46_61)))
            .bg(rgb(if selected { 0x20_30_41 } else { 0x14_19_24 }))
            .hover(|style| style.bg(rgb(0x20_29_36)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select_executable(if selected { None } else { Some(key.clone()) });
                this.rebuild();
                cx.notify();
            }))
            .child(app_cells(
                group.name.clone(),
                group.instances.to_string(),
                metrics,
                crate::live_app_views::reading_selector(DISK_COLUMN, &disk_selector),
            ))
            .into_any_element()
    }
}

impl LiveDashboard {
    pub(crate) fn select_executable(&mut self, executable: Option<String>) {
        self.request.executable = executable;
        if let Some(preview) = &self.preview {
            self.request.second = Some(preview.target.second);
            self.request.identity = preview.target.identity.clone();
            self.submit();
            return;
        }

        if let Some(snapshot) = &mut self.snapshot {
            snapshot.process_history = None;
        }
        self.panels.retain(|panel| {
            !panel
                .id
                .starts_with(crate::live_process_history::PANEL_PREFIX)
        });
        self.chart_inspector = None;
        self.error = None;
        match self.worker.request(self.request.clone()) {
            Ok(generation) => self.generation = generation,
            Err(error) => self.error = Some(error),
        }
        self.rebuild();
    }
}

fn groups_add(group: &mut Executable, process: &Value) -> Result<(), String> {
    group.instances += 1;
    if process["app_attribution"].is_null() {
        group.cpu_ns.add(amount(&process["cpu_ns"])?);
        group.energy_nj.add(amount(&process["energy_nj"])?);
        group
            .disk_read_bytes
            .add(amount(&process["disk_read_bytes"])?);
        group
            .disk_written_bytes
            .add(amount(&process["disk_written_bytes"])?);
        group.disk_io_bytes.add(amount(&process["disk_io_bytes"])?);
        group.idle_wakeups.add(amount(&process["idle_wakeups"])?);
        group.pageins.add(amount(&process["pageins"])?);
    } else {
        group.cpu_ns.add(None);
        group.energy_nj.add(None);
        group.disk_read_bytes.add(None);
        group.disk_written_bytes.add(None);
        group.disk_io_bytes.add(None);
        group.idle_wakeups.add(None);
        group.pageins.add(None);
    }
    match process["state"].as_str() {
        Some("live") => {
            group
                .resident_bytes
                .add(amount(&process["resident_bytes"])?);
            group
                .footprint_bytes
                .add(amount(&process["footprint_bytes"])?);
            group.threads.add(amount(&process["threads"])?);
        }
        Some("exited_verified" | "exited_unverified") => {}
        _ => return Err("Missing or invalid executable process state".to_owned()),
    }
    assert!(group.instances <= 512);
    Ok(())
}

fn amount(value: &Value) -> Result<Option<u64>, String> {
    if !value.is_object() {
        return Err("Missing executable member amount".to_owned());
    }
    let known = counter_optional(&value["known"])?;
    if known.is_some() {
        if !value["unavailable"].is_null() {
            return Err("Executable member amount is both known and unavailable".to_owned());
        }
    } else {
        if value["unavailable"].as_str().is_none() {
            return Err("Executable member amount has no availability reason".to_owned());
        }
    }
    Ok(known)
}

fn counter_optional(value: &Value) -> Result<Option<u64>, String> {
    match value {
        Value::Null => Ok(None),
        Value::String(text) => text
            .parse::<u64>()
            .map(Some)
            .map_err(|error| error.to_string()),
        _ => Err("Executable counter is not an exact decimal string".to_owned()),
    }
}

fn subtotal_rate(value: Option<f64>, subtotal: &Subtotal, unit: &str) -> String {
    let suffix = if subtotal.unavailable > 0 { " *" } else { "" };
    format!("{}{suffix}", rate(value, unit))
}

fn subtotal_memory(subtotal: &Subtotal) -> String {
    let suffix = if subtotal.unavailable > 0 { " *" } else { "" };
    format!("{}{suffix}", memory(subtotal.value()))
}

fn groups_identity(process: &Value) -> (String, String) {
    let Some(bytes) = groups_bytes(process) else {
        return (UNAVAILABLE.into(), "Executable unavailable".into());
    };
    let name = match process["executable_name"]["display"].as_str() {
        Some(name) if name.len() <= DISPLAY_BYTES_MAX => name.to_owned(),
        _ => "Executable · recorded bytes".into(),
    };
    (format!("hex:{}", bytes.to_ascii_lowercase()), name)
}

fn groups_bytes(process: &Value) -> Option<&str> {
    let bytes = process["executable_name"]["bytes_hex"].as_str()?;
    if bytes.len() > HEX_BYTES_MAX {
        return None;
    }
    if !bytes.len().is_multiple_of(2) {
        return None;
    }
    if !bytes.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    Some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn process(bytes: &str) -> Value {
        json!({"executable_name":{"bytes_hex":bytes,"display":"Program"}, "state":"live",
            "app_attribution":null, "cpu_ns":{"known":"4","unavailable":null},
            "energy_nj":{"known":"8","unavailable":null},
            "resident_bytes":{"known":"16","unavailable":null},
            "footprint_bytes":{"known":"32","unavailable":null},
            "disk_read_bytes":{"known":null,"unavailable":"interval"},
            "disk_written_bytes":{"known":null,"unavailable":"interval"},
            "disk_io_bytes":{"known":null,"unavailable":"interval"},
            "idle_wakeups":{"known":"6","unavailable":null},
            "pageins":{"known":"4","unavailable":null},
            "threads":{"known":"8","unavailable":null}})
    }

    macro_rules! missing_captured_amount {
        ($name:ident, $field:literal) => {
            #[test]
            fn $name() {
                // A missing owned field must fail grouping rather than look like refused data.
                let mut row = process("61");
                assert!(row.as_object_mut().unwrap().remove($field).is_some());
                let detail = json!({"processes":[row]});

                assert!(groups(Some(&detail)).is_err());
            }
        };
    }

    macro_rules! null_captured_amount {
        ($name:ident, $field:literal) => {
            #[test]
            fn $name() {
                // A whole-field null is malformed; actual unavailability is an Amount object.
                let mut row = process("61");
                row[$field] = Value::Null;
                let detail = json!({"processes":[row]});

                assert!(groups(Some(&detail)).is_err());
            }
        };
    }

    missing_captured_amount!(rejects_missing_wakeups_amount, "idle_wakeups");
    missing_captured_amount!(rejects_missing_pageins_amount, "pageins");
    missing_captured_amount!(rejects_missing_threads_amount, "threads");
    null_captured_amount!(rejects_null_wakeups_amount, "idle_wakeups");
    null_captured_amount!(rejects_null_pageins_amount, "pageins");
    null_captured_amount!(rejects_null_threads_amount, "threads");

    #[test]
    fn keeps_exact_known_subtotals_beside_unavailable_members() {
        // Goal: partial totals preserve exact integers and report the missing member.
        // Method: group one known interval above f64 precision with one refused interval.
        let mut known = process("61");
        known["cpu_ns"]["known"] = json!("9007199254740993");
        let mut refused = process("61");
        refused["cpu_ns"] = json!({"known":null,"unavailable":"refused"});
        let detail = json!({"processes":[known,refused],"elapsed_ns":"1000000000"});

        let groups = groups(Some(&detail)).unwrap();

        assert_eq!(
            groups[0].cpu_ns,
            Subtotal {
                known: 9_007_199_254_740_993,
                measured: 1,
                unavailable: 1,
                overflowed: false
            }
        );
    }

    #[test]
    fn leaves_fully_unavailable_metrics_absent() {
        // Goal: no known members cannot be presented as a zero usage reading.
        // Method: group a refused CPU amount and inspect its missing value and coverage.
        let mut refused = process("61");
        refused["cpu_ns"] = json!({"known":null,"unavailable":"refused"});
        let detail = json!({"processes":[refused]});

        let groups = groups(Some(&detail)).unwrap();

        assert_eq!(groups[0].cpu_ns.value(), None);
        assert_eq!(groups[0].cpu_ns.unavailable, 1);
    }

    #[test]
    fn suppresses_overflowed_subtotals_without_wrapping() {
        // Goal: overflow retains the valid known subtotal but suppresses derived readings.
        // Method: add one to a maximum u64 CPU interval in the same executable group.
        let mut maximum = process("61");
        maximum["cpu_ns"]["known"] = json!(u64::MAX.to_string());
        let mut extra = process("61");
        extra["cpu_ns"]["known"] = json!("1");
        let detail = json!({"processes":[maximum,extra]});

        let groups = groups(Some(&detail)).unwrap();

        assert_eq!(
            groups[0].cpu_ns,
            Subtotal {
                known: u64::MAX,
                measured: 1,
                unavailable: 1,
                overflowed: true
            }
        );
        assert_eq!(groups[0].cpu_ns.value(), None);
    }

    #[test]
    fn excludes_cpu_and_energy_crossing_an_exec_boundary() {
        // Goal: an exec boundary cannot attribute the whole interval to its final executable.
        // Method: group one ordinary interval and one exec-crossing live process.
        let ordinary = process("61");
        let mut crossing = process("61");
        crossing["app_attribution"] = json!("exec");
        let detail = json!({"processes":[ordinary,crossing]});

        let groups = groups(Some(&detail)).unwrap();

        assert_eq!(
            groups[0].cpu_ns,
            Subtotal {
                known: 4,
                measured: 1,
                unavailable: 1,
                overflowed: false
            }
        );
        assert_eq!(groups[0].energy_nj.known, 8);
        assert_eq!(groups[0].energy_nj.unavailable, 1);
        assert_eq!(groups[0].resident_bytes.known, 32);
    }

    #[test]
    fn excludes_exited_processes_from_memory_subtotals() {
        // Goal: historical process memory cannot inflate current executable memory.
        // Method: group one live and one verified-exited process with identical amounts.
        let mut exited = process("61");
        exited["state"] = json!("exited_verified");
        let detail = json!({"processes":[process("61"),exited]});

        let groups = groups(Some(&detail)).unwrap();

        assert_eq!(groups[0].resident_bytes.known, 16);
        assert_eq!(groups[0].footprint_bytes.known, 32);
        assert_eq!(groups[0].cpu_ns.known, 8);
    }

    #[test]
    fn keeps_disk_directions_separate_from_the_jointly_measured_population() {
        // Group disjoint read/write populations and retain an unavailable combined interval.
        let mut read = process("61");
        read["disk_read_bytes"] = json!({"known":"5","unavailable":null});
        let mut written = process("61");
        written["disk_written_bytes"] = json!({"known":"7","unavailable":null});
        let detail = json!({"processes":[read,written],"elapsed_ns":"1000000000"});

        let groups = groups(Some(&detail)).unwrap();

        assert_eq!(groups[0].disk_read_bytes.value(), Some(5));
        assert_eq!(groups[0].disk_written_bytes.value(), Some(7));
        assert_eq!(groups[0].disk_io_bytes.value(), None);
        assert_eq!(groups[0].disk_io_bytes.unavailable, 2);
        assert_eq!(groups[0].disk_rate(), "Unavailable *");
    }

    #[test]
    fn suppresses_an_overflowed_disk_total_without_losing_valid_directions() {
        // Overflow only the combined subtotal while preserving exact independent directions.
        let mut maximum = process("61");
        maximum["disk_read_bytes"] = json!({"known":u64::MAX.to_string(),"unavailable":null});
        maximum["disk_written_bytes"] = json!({"known":"0","unavailable":null});
        maximum["disk_io_bytes"] = maximum["disk_read_bytes"].clone();
        let mut extra = process("61");
        extra["disk_read_bytes"] = json!({"known":"0","unavailable":null});
        extra["disk_written_bytes"] = json!({"known":"1","unavailable":null});
        extra["disk_io_bytes"] = extra["disk_written_bytes"].clone();
        let detail = json!({"processes":[maximum,extra],"elapsed_ns":"1000000000"});

        let groups = groups(Some(&detail)).unwrap();

        assert_eq!(groups[0].disk_read_bytes.value(), Some(u64::MAX));
        assert_eq!(groups[0].disk_written_bytes.value(), Some(1));
        assert!(groups[0].disk_io_bytes.overflowed);
        assert_eq!(groups[0].disk_io_bytes.value(), None);
        assert_eq!(groups[0].disk_rate(), "Unavailable *");
    }

    #[test]
    fn retains_a_verified_exit_disk_interval_without_adding_its_memory() {
        // A final verified interval remains attributable after its memory gauge expires.
        let mut exited = process("61");
        exited["state"] = json!("exited_verified");
        exited["disk_read_bytes"] = json!({"known":"3000","unavailable":null});
        exited["disk_written_bytes"] = json!({"known":"1000","unavailable":null});
        exited["disk_io_bytes"] = json!({"known":"4000","unavailable":null});
        let detail = json!({"processes":[exited],"elapsed_ns":"2000000000"});

        let groups = groups(Some(&detail)).unwrap();

        assert_eq!(groups[0].resident_bytes.value(), None);
        assert_eq!(groups[0].disk_io_bytes.value(), Some(4000));
        assert_eq!(groups[0].disk_rate(), "2.0 kB/s");
    }

    #[test]
    fn groups_captured_bytes_instead_of_matching_display_names() {
        // Goal: byte-distinct programs stay separate despite identical display names.
        // Method: group two distinct bytes and one case-variant encoding of the first byte.
        let detail = json!({"processes":[process("ff"),process("fe"),process("FF")]});

        let groups = groups(Some(&detail)).unwrap();

        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups
                .iter()
                .find(|group| group.key == "hex:ff")
                .unwrap()
                .instances,
            2
        );
    }

    #[test]
    fn rejects_malformed_exact_counters() {
        // Goal: corrupt decimal text surfaces as an error rather than fabricated zero usage.
        // Method: group a member whose CPU counter contains non-decimal text.
        let mut invalid = process("61");
        invalid["cpu_ns"]["known"] = json!("invalid");
        let detail = json!({"processes":[invalid]});

        assert!(groups(Some(&detail)).is_err());
    }

    #[test]
    fn rejects_pages_larger_than_the_loaded_page_bound() {
        // Goal: oversized payloads cannot create unbounded grouping work.
        // Method: provide one more than the 512 permitted process rows.
        let detail = json!({"processes":vec![process("61"); PROCESS_ROWS_MAX + 1]});

        assert!(groups(Some(&detail)).is_err());
    }

    #[test]
    fn unavailable_names_have_a_distinct_filter() {
        // Goal: unavailable identity stays separate from an observed empty executable name.
        // Method: compare an absent name with an actual empty captured byte sequence.
        let mut unknown = process("61");
        unknown["executable_name"] = Value::Null;
        let empty = process("");
        let detail = json!({"processes":[unknown,empty]});

        let groups = groups(Some(&detail)).unwrap();

        assert_eq!(groups.len(), 2);
        assert!(matches(&detail["processes"][0], UNAVAILABLE));
        assert!(matches(&detail["processes"][1], "hex:"));
    }

    #[test]
    fn incomplete_hex_bytes_are_unavailable() {
        // Goal: truncated bytes cannot become a valid executable identity.
        // Method: filter a captured name containing an incomplete hexadecimal byte.
        assert!(matches(&process("f"), UNAVAILABLE));
    }

    #[test]
    fn non_hex_bytes_are_unavailable() {
        // Goal: malformed names cannot match a captured executable byte key.
        // Method: filter a captured name containing non-hexadecimal text.
        assert!(matches(&process("zz"), UNAVAILABLE));
    }

    #[test]
    fn oversized_executable_bytes_are_unavailable() {
        // Goal: an executable identity cannot exceed the recording's captured byte bound.
        // Method: exceed the maximum program path size by one byte.
        assert!(matches(
            &process(&"00".repeat(PROGRAM_BYTES_MAX + 1)),
            UNAVAILABLE
        ));
    }
}
