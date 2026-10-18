use std::collections::BTreeMap;

use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, rgb};
use serde_json::{Value, json};

use crate::charts::{GRID, MUTED};
use crate::live_app_views::{app_cells, exact_json, memory, rate};
use crate::live_dashboard::LiveDashboard;

pub struct Executable {
    pub key: String,
    pub name: String,
    pub instances: u32,
    pub cpu_ns: Subtotal,
    pub energy_nj: Subtotal,
    pub resident_bytes: Subtotal,
    pub footprint_bytes: Subtotal,
    elapsed_ns: Option<u64>,
}

#[derive(Debug, Default, PartialEq)]
pub struct Subtotal {
    pub known: u64,
    pub measured: u32,
    pub unavailable: u32,
    pub overflowed: bool,
}

const PROCESS_ROWS_MAX: usize = 512;
const PROGRAM_BYTES_MAX: usize = 4095;
const HEX_BYTES_MAX: usize = PROGRAM_BYTES_MAX * 2;
const DISPLAY_BYTES_MAX: usize = PROGRAM_BYTES_MAX * 3;
const UNAVAILABLE: &str = "unavailable";
const _: () = {
    assert!(PROCESS_ROWS_MAX <= 4_294_967_295);
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
    fn metrics(&self) -> [String; 5] {
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
            subtotal_rate(power, &self.energy_nj, "W"),
            subtotal_rate(
                self.energy_nj.value().map(viewer::apps::energy_joules),
                &self.energy_nj,
                "J",
            ),
        ]
    }

    pub(crate) fn json(&self) -> Value {
        json!({"executable_bytes":self.key, "instances":self.instances,
            "scope":"loaded process page only", "cpu_ns":self.cpu_ns.json(),
            "energy_nj":self.energy_nj.json(), "resident_bytes":self.resident_bytes.json(),
            "footprint_bytes":self.footprint_bytes.json()})
    }
}

impl LiveDashboard {
    pub fn executable_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(group) = self.executables.get(index) else {
            return div()
                .text_color(rgb(MUTED))
                .child("Executable unavailable")
                .into_any_element();
        };
        let selected = self.executable.as_ref() == Some(&group.key);
        let key = group.key.clone();
        let expanded = self
            .expanded
            .as_ref()
            .is_some_and(|(kind, row)| kind == "executables" && *row == index);
        let mut row = div()
            .id(SharedString::from(format!("executable-{index}")))
            .accessibility_id(format!("flightdeck.executable.{}", group.key))
            .role(gpui::Role::Button)
            .aria_label(format!(
                "{}; {} instances; loaded process page subtotal",
                group.name, group.instances
            ))
            .aria_selected(selected)
            .aria_expanded(expanded)
            .border_b_1()
            .border_color(rgb(GRID))
            .cursor_pointer()
            .focusable()
            .tab_stop(true)
            .focus(|style| style.bg(rgb(0x32_46_61)))
            .bg(rgb(if selected { 0x20_30_41 } else { 0x14_19_24 }))
            .hover(|style| style.bg(rgb(0x20_29_36)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.executable = if selected { None } else { Some(key.clone()) };
                this.expanded = if expanded {
                    None
                } else {
                    Some(("executables".into(), index))
                };
                this.rebuild();
                cx.notify();
            }))
            .child(app_cells(
                group.name.clone(),
                group.instances.to_string(),
                group.metrics(),
            ));
        if expanded {
            row = row.child(exact_json(&group.json()));
        }
        row.into_any_element()
    }
}

fn groups_add(group: &mut Executable, process: &Value) -> Result<(), String> {
    group.instances += 1;
    if process["app_attribution"].is_null() {
        group.cpu_ns.add(amount(&process["cpu_ns"])?);
        group.energy_nj.add(amount(&process["energy_nj"])?);
    } else {
        group.cpu_ns.add(None);
        group.energy_nj.add(None);
    }
    match process["state"].as_str() {
        Some("live") => {
            group
                .resident_bytes
                .add(amount(&process["resident_bytes"])?);
            group
                .footprint_bytes
                .add(amount(&process["footprint_bytes"])?);
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
            "footprint_bytes":{"known":"32","unavailable":null}})
    }

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
