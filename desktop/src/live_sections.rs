use gpui::{Context, FocusHandle, ListOffset, Window, div, prelude::*, px, rgb};

use crate::charts::{GRID, TEXT};
use crate::live_dashboard::{LiveDashboard, Row};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Section {
    OverviewHistories,
    OverviewContext,
    AppTotals,
    AppProcesses,
    MemoryQuantities,
    EnergyContext,
    ComputerReference,
    SystemFiles,
    SelfReports,
    SelfMinutes,
}

impl Section {
    fn index(self) -> usize {
        match self {
            Self::OverviewHistories => 0,
            Self::OverviewContext => 1,
            Self::AppTotals => 2,
            Self::MemoryQuantities => 3,
            Self::EnergyContext => 4,
            Self::ComputerReference => 5,
            Self::SystemFiles => 6,
            Self::AppProcesses => 7,
            Self::SelfReports => 8,
            Self::SelfMinutes => 9,
        }
    }

    pub(crate) fn id(self) -> &'static str {
        match self {
            Self::OverviewHistories => "overview-histories",
            Self::OverviewContext => "overview-context",
            Self::AppTotals => "app-totals",
            Self::MemoryQuantities => "memory-quantities",
            Self::EnergyContext => "energy-context",
            Self::ComputerReference => "computer-reference",
            Self::SystemFiles => "system-files",
            Self::AppProcesses => "app-processes",
            Self::SelfReports => "self-reports",
            Self::SelfMinutes => "self-minutes",
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::OverviewHistories => "App resource histories",
            Self::OverviewContext => "More resource context",
            Self::AppTotals => "Selected app totals",
            Self::MemoryQuantities => "All memory quantities",
            Self::EnergyContext => "Power breakdown and thermal context",
            Self::ComputerReference => "Schematic reference",
            Self::SystemFiles => "System file entries",
            Self::AppProcesses => "Process instances · selected app",
            Self::SelfReports => "Captured report data",
            Self::SelfMinutes => "CPU budget intervals and capture provenance",
        }
    }

    /// The prototype has no table of process instances, so that section starts folded and
    /// keeps the Apps page to the prototype's content until the user opens it.
    fn starts_folded(self) -> bool {
        matches!(self, Self::AppProcesses)
    }
}

const SECTIONS: [Section; 10] = [
    Section::OverviewHistories,
    Section::OverviewContext,
    Section::AppTotals,
    Section::MemoryQuantities,
    Section::EnergyContext,
    Section::ComputerReference,
    Section::SystemFiles,
    Section::AppProcesses,
    Section::SelfReports,
    Section::SelfMinutes,
];

pub(crate) struct Sections {
    collapsed: [bool; 10],
    focus: [Option<FocusHandle>; 10],
}

impl Default for Sections {
    fn default() -> Self {
        Self {
            collapsed: SECTIONS.map(Section::starts_folded),
            focus: Default::default(),
        }
    }
}

impl Sections {
    pub(crate) fn expanded(&self, section: Section) -> bool {
        !self.collapsed[section.index()]
    }

    pub(crate) fn reset(&mut self) {
        self.collapsed = SECTIONS.map(Section::starts_folded);
    }

    fn install_focus(&mut self, cx: &mut Context<LiveDashboard>) -> bool {
        let mut installed = false;
        for focus in &mut self.focus {
            if focus.is_none() {
                *focus = Some(cx.focus_handle());
                installed = true;
            }
        }
        installed
    }
}

impl Row {
    pub(crate) fn disclosure_section(&self) -> Option<Section> {
        match self {
            Self::Disclosure(section) => Some(*section),
            Self::MemoryAttribution(_) => Some(Section::MemoryQuantities),
            Self::Inventory => Some(Section::ComputerReference),
            Self::SelfPart(crate::live_diagnostics::SelfPart::Reports) => {
                Some(Section::SelfReports)
            }
            Self::SelfPart(crate::live_diagnostics::SelfPart::Minutes) => {
                Some(Section::SelfMinutes)
            }
            _ => None,
        }
    }
}

impl LiveDashboard {
    pub(crate) fn install_section_focus(&mut self, cx: &mut Context<Self>) {
        if self.sections.install_focus(cx) {
            self.splice_section_rows(self.list.item_count());
        }
    }

    pub(crate) fn splice_section_rows(&self, previous: usize) {
        self.list.splice_focusable(
            0..previous,
            self.rows.iter().map(|row| {
                row.disclosure_section()
                    .and_then(|section| self.sections.focus[section.index()].clone())
            }),
        );
    }

    pub(crate) fn reset_section_rows(&self) {
        self.list.reset(0);
        self.splice_section_rows(0);
    }

    pub(crate) fn disclosure(
        &self,
        section: Section,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let Some(focus) = &self.sections.focus[section.index()] else {
            unreachable!("Dashboard installs disclosure focus before rendering content");
        };
        let expanded = self.sections.expanded(section);

        div()
            .id(section.id())
            .debug_selector(move || format!("section:{}", section.id()))
            .accessibility_id(format!("flightdeck.section.{}", section.id()))
            .role(gpui::Role::Button)
            .aria_label(section.title())
            .aria_expanded(expanded)
            .track_focus(focus)
            .tab_stop(true)
            .cursor_pointer()
            .focus(|style| style.bg(rgb(0x32_46_61)))
            .hover(|style| style.bg(rgb(0x22_25_2b)))
            .flex()
            .items_center()
            .gap(px(8.0))
            .min_h(px(44.0))
            .px(px(8.0))
            .rounded(px(5.0))
            .border_b_1()
            .border_color(rgb(GRID))
            .text_size(px(16.0))
            .text_color(rgb(TEXT))
            .child(if expanded { "⌄" } else { "›" })
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .debug_selector(move || section.title().into())
                    .child(section.title()),
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.toggle_section(section, window, cx);
            }))
            .on_key_down(
                cx.listener(move |this, event: &gpui::KeyDownEvent, window, cx| {
                    if event.keystroke.modifiers.modified() {
                        return;
                    }

                    if matches!(event.keystroke.key.as_str(), "enter" | "space") {
                        this.toggle_section(section, window, cx);
                        window.prevent_default();
                        cx.stop_propagation();
                    }
                }),
            )
    }

    pub(crate) fn toggle_section(
        &mut self,
        section: Section,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let offset = self.list.logical_scroll_top();
        let anchor = self
            .rows
            .get(offset.item_ix)
            .map(crate::live_dashboard::Row::layout_key);

        self.sections.collapsed[section.index()] = self.sections.expanded(section);
        let mut rows = self.reading_rows();
        if self
            .preview
            .as_ref()
            .is_some_and(|preview| !rows.iter().any(|row| row.owns_plot(&preview.source)))
        {
            self.end_reading_preview(cx);
            rows = self.reading_rows();
        }

        if let Some(preview) = &mut self.preview {
            preview.layout = None;
        }
        self.row_heights.borrow_mut().remove("memory-attribution");
        self.row_heights.borrow_mut().remove("inventory");

        let previous = self.rows.len();
        self.rows = rows;
        self.splice_section_rows(previous);

        let index = anchor.and_then(|anchor| {
            self.rows
                .iter()
                .take(4096)
                .position(|row| row.layout_key() == anchor)
        });
        self.list.scroll_to(ListOffset {
            item_ix: index.unwrap_or_else(|| offset.item_ix.min(self.rows.len().saturating_sub(1))),
            offset_in_item: offset.offset_in_item,
        });

        if let Some(focus) = &self.sections.focus[section.index()] {
            window.focus(focus, cx);
        }

        cx.notify();
    }
}
