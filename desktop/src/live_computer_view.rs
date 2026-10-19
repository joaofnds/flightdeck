use gpui::{Context, IntoElement, div, prelude::*, px, rgb};

use crate::charts::{BLUE, GRID, MUTED, PANEL};

struct Reference {
    name: &'static str,
    url: &'static str,
}

const REFERENCES: [Reference; 5] = [
    Reference {
        name: "Apple logic-board service view",
        url: "https://support.apple.com/en-us/125827",
    },
    Reference {
        name: "Apple fan procedure",
        url: "https://support.apple.com/en-us/125832",
    },
    Reference {
        name: "Apple exploded view",
        url: "https://support.apple.com/en-us/125838",
    },
    Reference {
        name: "Counterpoint M5 Pro chip teardown",
        url: concat!(
            "https://counterpointresearch.com/en/insights/",
            "apple-m5-pro-chip-teardown-analysis"
        ),
    },
    Reference {
        name: "iFixit 2026 logic-board replacement",
        url: concat!(
            "https://www.ifixit.com/Guide/",
            "MacBook+Pro+16-Inch+2026+Logic+Board+Replacement/223848"
        ),
    },
];

pub(super) fn view(
    dashboard: &crate::live_dashboard::LiveDashboard,
    cx: &mut Context<crate::live_dashboard::LiveDashboard>,
) -> impl IntoElement {
    let inventory = &dashboard.inventory;
    assert!(inventory.len() <= 16);
    div()
        .flex()
        .flex_col()
        .gap(px(16.0))
        .child(model_card(inventory))
        .child(system_details(inventory))
        .child(schematic_reference(dashboard, cx))
}

fn model_card(inventory: &[(String, String)]) -> impl IntoElement {
    let (title, generation) = if inventory_value(inventory, "Model identifier") == "Mac17,8" {
        ("MacBook Pro", "16-inch · 2026")
    } else {
        ("This Mac", "Current hardware inventory")
    };
    div()
        .id("computer-specifications")
        .debug_selector(|| "computer-specifications".into())
        .role(gpui::Role::Group)
        .aria_label("Computer model and specifications")
        .max_w(px(760.0))
        .p(px(18.0))
        .rounded(px(7.0))
        .border_1()
        .border_color(rgb(GRID))
        .bg(rgb(PANEL))
        .child(
            div()
                .text_size(px(14.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .debug_selector(|| format!("computer-model:{title}"))
                .child(title),
        )
        .child(
            div()
                .mt(px(5.0))
                .text_size(px(11.0))
                .text_color(rgb(MUTED))
                .debug_selector(|| format!("computer-generation:{generation}"))
                .child(generation),
        )
        .child(model_card_specifications(inventory))
        .child(
            div()
                .mt(px(18.0))
                .text_size(px(11.0))
                .text_color(rgb(MUTED))
                .child(
                    "Current computer · independent of the selected recording. \
                Core counts are the maximum reported this boot.",
                ),
        )
}

fn model_card_specifications(inventory: &[(String, String)]) -> impl IntoElement {
    let mut specifications = div().mt(px(16.0));
    for (label, reading) in [
        (
            "Model identifier",
            inventory_value(inventory, "Model identifier"),
        ),
        ("Chip", inventory_value(inventory, "Chip")),
        (
            "Physical CPU cores",
            inventory_value(inventory, "Physical CPU cores · maximum this boot"),
        ),
        (
            "Installed memory",
            memory_display(inventory_value(inventory, "Installed memory")),
        ),
    ] {
        specifications = specifications.child(
            div()
                .flex()
                .justify_between()
                .items_start()
                .gap(px(12.0))
                .py(px(8.0))
                .border_b_1()
                .border_color(rgb(GRID))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(13.0))
                        .text_color(rgb(MUTED))
                        .debug_selector(|| format!("computer-spec-label:{label}"))
                        .child(label),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_right()
                        .text_size(px(13.0))
                        .font_weight(gpui::FontWeight::MEDIUM)
                        .debug_selector(|| format!("computer-spec:{label}:{reading}"))
                        .child(reading.to_owned()),
                ),
        );
    }

    specifications
}

fn system_details(inventory: &[(String, String)]) -> impl IntoElement {
    let mut rows = div().mt(px(12.0));
    for (label, reading) in inventory {
        if matches!(
            label.as_str(),
            "Model identifier" | "Chip" | "Physical CPU cores · maximum this boot"
        ) {
            continue;
        }
        rows = rows.child(
            div()
                .flex()
                .flex_wrap()
                .gap(px(8.0))
                .py(px(9.0))
                .border_b_1()
                .border_color(rgb(GRID))
                .child(
                    div()
                        .flex_1()
                        .flex_basis(px(210.0))
                        .min_w_0()
                        .text_color(rgb(MUTED))
                        .child(label.clone()),
                )
                .child(
                    div()
                        .flex_1()
                        .flex_basis(px(260.0))
                        .min_w_0()
                        .debug_selector(|| format!("computer-detail:{label}:{reading}"))
                        .child(reading.clone()),
                ),
        );
    }

    div()
        .id("computer-system-details")
        .debug_selector(|| "computer-system-details".into())
        .role(gpui::Role::Group)
        .aria_label("System details")
        .p(px(18.0))
        .rounded(px(7.0))
        .border_1()
        .border_color(rgb(GRID))
        .bg(rgb(PANEL))
        .text_size(px(12.0))
        .child(
            div()
                .text_size(px(16.0))
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .child("System details"),
        )
        .child(rows)
}

fn schematic_reference(
    dashboard: &crate::live_dashboard::LiveDashboard,
    cx: &mut Context<crate::live_dashboard::LiveDashboard>,
) -> impl IntoElement {
    div()
        .id("computer-schematic-reference")
        .debug_selector(|| "computer-schematic-reference".into())
        .role(gpui::Role::Group)
        .aria_label("Schematic reference")
        .p(px(18.0))
        .rounded(px(7.0))
        .border_1()
        .border_color(rgb(GRID))
        .bg(rgb(PANEL))
        .text_size(px(12.0))
        .text_color(rgb(MUTED))
        .child(dashboard.disclosure(crate::live_sections::Section::ComputerReference, cx))
        .when(
            dashboard
                .sections
                .expanded(crate::live_sections::Section::ComputerReference),
            |panel| panel.child(schematic_reference_content()),
        )
}

fn schematic_reference_content() -> impl IntoElement {
    div()
        .child(div().mt(px(12.0)).child(
            "Reference assembly: MacBook Pro 16-inch · 2026 · Mac17,8. \
             Bottom cover removed, hinge at the top.",
        ))
        .child(div().mt(px(10.0)).child(
            "Apple's 2026 logic-board service view, fan procedure and exploded view \
             establish the major assembly layout. The drawing simplifies the installed assembly.",
        ))
        .child(div().mt(px(10.0)).child(
            "CPU and GPU share the cooling assembly. Airflow is illustrative; fan-channel sides, \
             physical sensor coordinates and hidden chip placement remain unverified. \
             No complete electrical schematic or sensor-coordinate map was verified.",
        ))
        .child(div().mt(px(10.0)).child(
            "Counterpoint identifies separate CPU and GPU dies in one package using a 14-inch \
             machine; it does not establish their orientation in this 16-inch chassis.",
        ))
        .child(div().mt(px(10.0)).child(
            "Apple's manual reuses 2024 detached-board and fan photos; iFixit also discloses \
             earlier-model photos. Those photos support assembly context, not exact hidden \
             M5 Pro chip placement.",
        ))
        .child(
            div()
                .flex()
                .flex_col()
                .items_start()
                .gap(px(6.0))
                .mt(px(12.0))
                .children(REFERENCES.iter().map(schematic_reference_link)),
        )
}

fn schematic_reference_link(reference: &'static Reference) -> impl IntoElement {
    div()
        .id(gpui::SharedString::from(reference.url))
        .debug_selector(|| format!("computer-reference:{}", reference.name))
        .role(gpui::Role::Link)
        .aria_label(reference.name)
        .aria_description(reference.url)
        .text_color(rgb(BLUE))
        .underline()
        .cursor_pointer()
        .focusable()
        .tab_stop(true)
        .focus(|style| style.bg(rgb(0x32_46_61)))
        .py(px(4.0))
        .child(reference.name)
        .on_click(move |_, _, cx| cx.open_url(reference.url))
}

fn inventory_value<'inventory>(
    inventory: &'inventory [(String, String)],
    label: &str,
) -> &'inventory str {
    inventory
        .iter()
        .find(|(name, _)| name == label)
        .map_or("Unavailable", |(_, reading)| reading.as_str())
}

fn memory_display(reading: &str) -> &str {
    match reading.split_once(" GiB · ") {
        Some((gibibytes, _)) => {
            let end = gibibytes.len() + " GiB".len();
            &reading[..end]
        }
        None => reading,
    }
}
