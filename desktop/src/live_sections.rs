use gpui::{Context, FocusHandle, ListOffset, Window, div, prelude::*, px, rgb};

use crate::charts::{MUTED, TEXT};
use crate::live_dashboard::{LiveDashboard, Row};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Section {
    OverviewHistories,
    OverviewContext,
    AppTotals,
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
            Self::SelfReports => 7,
            Self::SelfMinutes => 8,
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
            Self::SelfReports => "Captured report data",
            Self::SelfMinutes => "CPU budget intervals and capture provenance",
        }
    }
}

#[derive(Default)]
pub(crate) struct Sections {
    collapsed: [bool; 9],
    focus: [Option<FocusHandle>; 9],
}

impl Sections {
    pub(crate) fn expanded(&self, section: Section) -> bool {
        !self.collapsed[section.index()]
    }

    pub(crate) fn reset(&mut self) {
        self.collapsed = Default::default();
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
    /// Whether the row belongs to a details panel, which the prototype draws as a bordered
    /// panel around its summary and the content it folds.
    pub(crate) fn in_details(&self) -> bool {
        match self {
            Self::Disclosure(section) => matches!(
                section,
                Section::OverviewHistories
                    | Section::OverviewContext
                    | Section::SystemFiles
                    | Section::MemoryQuantities
                    | Section::EnergyContext
            ),
            Self::OverviewAppHistories(_)
            | Self::OverviewAnalysis(_)
            | Self::SystemContext(_)
            | Self::MemoryQuantities
            | Self::ContextCharts { .. } => true,
            _ => false,
        }
    }

    pub(crate) fn disclosure_section(&self) -> Option<Section> {
        match self {
            Self::Disclosure(section) => Some(*section),
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

    /// Replaces the rows with `rows`, splicing only the run that changed so the list keeps every
    /// other row's measured height, and returns where the row at the top of the viewport is now.
    pub(crate) fn replace_rows(&mut self, rows: Vec<Row>) -> ListOffset {
        let splice =
            crate::live_row_splice::row_splice(&self.rows, &rows, self.list.logical_scroll_top());
        self.rows = rows;
        self.list.splice_focusable(
            splice.source,
            self.rows[splice.target].iter().map(|row| {
                row.disclosure_section()
                    .and_then(|section| self.sections.focus[section.index()].clone())
            }),
        );

        splice.top
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
        // The prototype's plain details read in the page's own text, and the ones in panels
        // read quietly.
        let plain = matches!(section, Section::AppTotals | Section::ComputerReference);

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
            .gap(px(5.0))
            .min_h(px(44.0))
            .rounded(px(5.0))
            .text_size(px(if plain { 13.0 } else { 12.0 }))
            .line_height(px(if plain { 19.5 } else { 18.0 }))
            .font_weight(gpui::FontWeight::MEDIUM)
            .text_color(rgb(if plain { TEXT } else { MUTED }))
            .child(
                div()
                    .text_size(px(16.0))
                    .child(if expanded { "⌄" } else { "›" }),
            )
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

        let top = self.replace_rows(rows);
        self.list.scroll_to(top);

        if let Some(focus) = &self.sections.focus[section.index()] {
            window.focus(focus, cx);
        }

        cx.notify();
    }
}
