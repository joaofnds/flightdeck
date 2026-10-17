use std::collections::BTreeMap;

use gpui::{AnyElement, Context, IntoElement, SharedString, div, prelude::*, px, rgb};
use serde_json::Value;

use crate::charts::{BLUE, GRID, MUTED, PANEL};
use crate::live_dashboard::LiveDashboard;

pub struct Executable {
    pub key: String,
    pub name: String,
    pub instances: u32,
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

pub fn groups(detail: Option<&Value>) -> Vec<Executable> {
    let Some(processes) = detail.and_then(|detail| detail["processes"].as_array()) else {
        return Vec::new();
    };
    let mut groups = BTreeMap::<String, Executable>::new();
    for process in processes.iter().take(PROCESS_ROWS_MAX) {
        let (key, name) = groups_identity(process);
        let group = groups.entry(key.clone()).or_insert(Executable {
            key,
            name,
            instances: 0,
        });
        group.instances += 1;
    }
    let mut groups: Vec<_> = groups.into_values().collect();
    groups.sort_by(|first, second| {
        first
            .name
            .cmp(&second.name)
            .then(first.key.cmp(&second.key))
    });
    assert!(groups.len() <= PROCESS_ROWS_MAX);
    groups
}

pub fn matches(process: &Value, key: &str) -> bool {
    match groups_bytes(process) {
        Some(bytes) => key
            .strip_prefix("hex:")
            .is_some_and(|selected| selected.eq_ignore_ascii_case(bytes)),
        None => key == UNAVAILABLE,
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
        div()
            .id(SharedString::from(format!("executable-{index}")))
            .p(px(12.0))
            .rounded(px(6.0))
            .border_1()
            .border_color(rgb(GRID))
            .bg(rgb(if selected { 0x1c_2b_40 } else { PANEL }))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.executable = if selected { None } else { Some(key.clone()) };
                this.expanded = None;
                this.rebuild();
                cx.notify();
            }))
            .child(
                div()
                    .text_color(rgb(if selected { BLUE } else { MUTED }))
                    .child(group.name.clone()),
            )
            .child(
                div()
                    .mt(px(4.0))
                    .text_size(px(11.0))
                    .text_color(rgb(MUTED))
                    .child(format!(
                        "{} instances in loaded process page · app charts remain app totals",
                        group.instances,
                    )),
            )
            .into_any_element()
    }
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
    use serde_json::json;

    use super::*;

    #[test]
    fn groups_exact_bytes_instead_of_display_names() {
        // Distinct bytes with the same display stay separate; identical bytes share a count.
        let detail = json!({"processes":[
            {"executable_name":{"bytes_hex":"ff", "display":"�"}},
            {"executable_name":{"bytes_hex":"fe", "display":"�"}},
            {"executable_name":{"bytes_hex":"FF", "display":"Different label"}}
        ]});
        let groups = groups(Some(&detail));
        assert_eq!(groups.len(), 2);
        assert_eq!(
            groups
                .iter()
                .find(|group| group.key == "hex:ff")
                .unwrap()
                .instances,
            2
        );
        assert_eq!(
            groups
                .iter()
                .find(|group| group.key == "hex:fe")
                .unwrap()
                .instances,
            1
        );
    }

    #[test]
    fn unavailable_names_have_a_distinct_filter() {
        // An unknown executable must not merge into an observed empty executable byte string.
        let unknown = json!({"executable_name":null});
        let empty = json!({"executable_name":{"bytes_hex":"", "display":""}});
        let detail = json!({"processes":[unknown, empty]});
        let groups = groups(Some(&detail));
        assert_eq!(groups.len(), 2);
        assert!(matches(&unknown, UNAVAILABLE));
        assert!(!matches(&empty, UNAVAILABLE));
        assert!(matches(&empty, "hex:"));
    }

    #[test]
    fn groups_count_only_the_bounded_loaded_page() {
        // A malformed oversized page cannot increase counts beyond the 512 loaded-row bound.
        let process = json!({"executable_name":{"bytes_hex":"61", "display":"a"}});
        let detail = json!({"processes":vec![process; PROCESS_ROWS_MAX + 1]});
        let executables = groups(Some(&detail));
        assert_eq!(executables.len(), 1);
        assert_eq!(executables[0].instances, 512);
        assert!(groups(None).is_empty());
    }

    #[test]
    fn incomplete_hex_bytes_are_unavailable() {
        // A truncated byte must not become an executable identity.
        let process = json!({"executable_name":{"bytes_hex":"f", "display":"known"}});

        assert!(matches(&process, UNAVAILABLE));
    }

    #[test]
    fn non_hex_bytes_are_unavailable() {
        // Non-hexadecimal text must not become an executable identity.
        let process = json!({"executable_name":{"bytes_hex":"zz", "display":"known"}});

        assert!(matches(&process, UNAVAILABLE));
    }

    #[test]
    fn oversized_executable_bytes_are_unavailable() {
        // An overlong executable name must not cross the recording's byte bound.
        let process = json!({"executable_name":{"bytes_hex":
            "00".repeat(PROGRAM_BYTES_MAX + 1), "display":"known"}});

        assert!(matches(&process, UNAVAILABLE));
    }
}
