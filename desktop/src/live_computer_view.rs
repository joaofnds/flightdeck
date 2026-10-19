use gpui::{Context, IntoElement, div, prelude::*, px, rgb};

use crate::charts::{BLUE, GRID, MUTED, PANEL, RED, YELLOW};

/// One run of a reference paragraph, linked to its source where it names one.
struct Run {
    text: &'static str,
    url: Option<&'static str>,
}

const fn text(text: &'static str) -> Run {
    Run { text, url: None }
}

const fn link(text: &'static str, url: &'static str) -> Run {
    Run {
        text,
        url: Some(url),
    }
}

/// The reference assembly the hardware drawing follows, as the prototype names it. It is
/// reference data and stays the same whatever Mac the page runs on.
const ASSEMBLY: &str = "MacBook Pro 16″ · 2026 · Apple M5 Pro · Mac17,8 · A3428";

const PARAGRAPHS: [&[Run]; 4] = [
    &[text(
        "Leaders use component anchors; physical sensor coordinates and fan channel sides are \
         unverified. CPU and GPU share the cooling assembly. The separate airflow colors are a \
         display choice, not measured exhaust temperatures. Rotor and particle speed follows \
         each channel’s RPM.",
    )],
    &[
        link(
            "Apple’s 16-inch 2026 logic-board service view",
            "https://support.apple.com/en-us/125827",
        ),
        text(", "),
        link("fan procedure", "https://support.apple.com/en-us/125832"),
        text(" and "),
        link("exploded view", "https://support.apple.com/en-us/125838"),
        text(
            " establish the assembly layout: board and cooling assembly near the hinge, two \
             blowers, six battery cells and side speaker assemblies. These references cover M5 \
             Pro and M5 Max; this drawing simplifies the installed assembly.",
        ),
    ],
    &[
        text("CPU, GPU and memory leaders reference the covered processor assembly. "),
        link(
            "Counterpoint’s M5 Pro chip teardown",
            concat!(
                "https://counterpointresearch.com/en/insights/",
                "apple-m5-pro-chip-teardown-analysis"
            ),
        ),
        text(
            " identifies separate CPU and GPU dies in one package, using a 14-inch machine. It \
             does not establish their orientation in this 16-inch chassis. Individual memory \
             and NAND outlines, sensor coordinates and fan-channel sides remain unverified.",
        ),
    ],
    &[
        text(
            "These Apple references provide service illustrations. The current manual reuses \
             2024 detached-board and fan photos; ",
        ),
        link(
            "iFixit also discloses earlier-model photos",
            concat!(
                "https://www.ifixit.com/Guide/",
                "MacBook+Pro+16-Inch+2026+Logic+Board+Replacement/223848"
            ),
        ),
        text(
            ". Those photos support assembly context, not exact hidden M5 Pro chip placement. \
             The 2026 installed locator anchors the major layout and cooling contour. No \
             complete electrical schematic or sensor-coordinate map was verified in this \
             research. Airflow is illustrative, not a measured path or speed.",
        ),
    ],
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
}

fn model_card_specifications(inventory: &[(String, String)]) -> impl IntoElement {
    let mut specifications = div().mt(px(16.0));
    let memory = inventory_value(inventory, "Memory");
    let memory = if inventory_value(inventory, "Architecture") == "arm64" {
        format!("{memory} unified memory")
    } else {
        memory.to_owned()
    };
    for (label, reading) in [
        (
            "Model identifier",
            inventory_value(inventory, "Model identifier").to_owned(),
        ),
        ("Chip", inventory_value(inventory, "Chip").to_owned()),
        ("CPU", cores(inventory_value(inventory, "CPU cores"))),
        ("GPU", cores(inventory_value(inventory, "GPU cores"))),
        ("Memory", memory),
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
                        .child(reading),
                ),
        );
    }

    specifications
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
        .child(
            div()
                .mt(px(12.0))
                .child(
                    div()
                        .debug_selector(|| format!("computer-reference-assembly:{ASSEMBLY}"))
                        .child(ASSEMBLY),
                )
                .child("Bottom view with the cover removed. Hinge at the top."),
        )
        .child(airflow_guide())
        .children(
            PARAGRAPHS
                .iter()
                .enumerate()
                .map(|(index, runs)| schematic_reference_paragraph(index, runs)),
        )
}

fn airflow_guide() -> impl IntoElement {
    let mut key = div().flex().flex_wrap().gap(px(14.0)).mt(px(6.0));
    for (color, stop) in [(BLUE, "50 °C"), (YELLOW, "70 °C"), (RED, "90 °C")] {
        key = key.child(
            div()
                .flex()
                .items_center()
                .gap(px(6.0))
                .debug_selector(|| format!("computer-airflow-stop:{stop}"))
                .child(div().w(px(12.0)).h(px(7.0)).rounded(px(1.0)).bg(rgb(color)))
                .child(stop),
        );
    }
    div()
        .mt(px(12.0))
        .child("Airflow tint · left CPU-area temperature · right GPU-area temperature")
        .child(key)
}

/// A paragraph whose linked runs open their source when clicked, as the prototype's
/// inline links do.
fn schematic_reference_paragraph(index: usize, runs: &'static [Run]) -> impl IntoElement {
    let mut paragraph = String::with_capacity(512);
    let mut ranges = Vec::with_capacity(runs.len());
    let mut urls = Vec::with_capacity(runs.len());
    for run in runs {
        let start = paragraph.len();
        paragraph.push_str(run.text);
        if let Some(url) = run.url {
            ranges.push(start..paragraph.len());
            urls.push(url);
        }
    }
    let link_style = gpui::HighlightStyle {
        color: Some(rgb(BLUE).into()),
        underline: Some(gpui::UnderlineStyle {
            thickness: px(1.0),
            color: Some(rgb(BLUE).into()),
            wavy: false,
        }),
        ..gpui::HighlightStyle::default()
    };
    let styled = gpui::StyledText::new(paragraph)
        .with_highlights(ranges.iter().map(|range| (range.clone(), link_style)));
    div()
        .mt(px(10.0))
        .debug_selector(move || format!("computer-reference-paragraph:{index}"))
        .child(
            gpui::InteractiveText::new(("computer-reference-paragraph", index), styled)
                .on_click(ranges, move |link, _, cx| cx.open_url(urls[link])),
        )
}

/// A core count as the prototype writes it, such as "18 cores", or the reading unchanged
/// where it holds no count.
fn cores(reading: &str) -> String {
    match reading.parse::<u64>() {
        Ok(count) => format!("{count} cores"),
        Err(_) => reading.to_owned(),
    }
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
