use std::collections::BTreeMap;
use std::rc::Rc;

use gpui::{ListOffset, Pixels, SharedString, px};

use super::Origin;
use crate::live_dashboard::{LiveDashboard, Row};

#[derive(Clone)]
pub struct Layout {
    source: SharedString,
    source_top: Pixels,
    heights: Rc<BTreeMap<String, Pixels>>,
}

impl Row {
    pub(crate) fn layout_key(&self) -> String {
        match self {
            Self::Summary => "summary".into(),
            Self::OverviewStatus => "overview-status".into(),
            Self::AppSelector => "app-selector".into(),
            Self::AppSummary => "app-summary".into(),
            Self::AppColumns(kind) => format!("app-columns-{kind}"),
            Self::CPUActivity => "cpu-activity".into(),
            Self::CPUHistory => "cpu-history".into(),
            Self::Charts(panels) => format!("charts-{}", panels[0].id),
            Self::Heading(title) => {
                if title.starts_with("Inside ") {
                    "app-executable-heading".into()
                } else {
                    if title.starts_with("Process instances") {
                        "app-process-heading".into()
                    } else {
                        "app-history-heading".into()
                    }
                }
            }
            Self::Hardware => "hardware".into(),
            Self::TableHeading(name) => format!("table-heading-{name}"),
            Self::Table(name, index) => format!("table-{name}-{index}"),
            Self::App(index) => format!("app-{index}"),
            Self::Process(index) => format!("process-{index}"),
            Self::Executable(index) => format!("executable-{index}"),
            Self::Inventory => "inventory".into(),
            Self::Spacer(_) => "hover-spacing".into(),
        }
    }

    fn owns_plot(&self, source: &str) -> bool {
        match self {
            Self::Charts(panels) => panels.iter().take(2).any(|panel| panel.id == source),
            Self::CPUHistory => source == "cpu-history-plot",
            _ => false,
        }
    }

    fn measured_height(&self, heights: &BTreeMap<String, Pixels>) -> Option<Pixels> {
        match self {
            Self::Spacer(height) => Some(*height),
            Self::Table(_, _) | Self::App(_) | Self::Process(_) | Self::Executable(_) => {
                heights.get(&self.layout_key()).copied().or(Some(px(26.0)))
            }
            _ => heights.get(&self.layout_key()).copied(),
        }
    }
}

impl LiveDashboard {
    pub(crate) fn reading_keyboard_hover_retained(
        &self,
        bounds: gpui::Bounds<Pixels>,
        window: &gpui::Window,
    ) -> bool {
        if !window.last_input_was_keyboard() {
            return false;
        }

        self.list
            .viewport_bounds()
            .intersect(&bounds)
            .contains(&window.mouse_position())
    }

    pub(crate) fn reading_membership_observer(
        &self,
        source: SharedString,
        cx: &mut gpui::Context<Self>,
    ) -> Option<impl gpui::IntoElement> {
        use gpui::prelude::*;
        let preview = self.preview.as_ref()?;
        if preview.origin != Origin::Pointer {
            return None;
        }

        if preview.source != source {
            return None;
        }

        let target = preview.target.clone();
        let view = cx.weak_entity();
        Some(
            gpui::canvas(
                move |bounds, window, cx| {
                    let hitbox = window.insert_hitbox(bounds, gpui::HitboxBehavior::Normal);
                    window.defer(cx, move |window, cx| {
                        match view.update(cx, |this, cx| {
                            if this.reading_target() != Some(&target) {
                                return;
                            }

                            if hitbox.is_hovered_at(window.mouse_position(), window) {
                                this.reading_geometry_ready(&source, bounds, window, cx);
                            } else {
                                this.clear_reading_target(&source, cx);
                            }
                        }) {
                            Ok(()) => {}
                            Err(error) => eprintln!("Hover layout observer: {error}"),
                        }
                    });
                },
                |_, (), _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
    }

    pub(crate) fn reading_viewport(&mut self, size: gpui::Size<Pixels>) {
        if self.viewport_size == size {
            return;
        }

        self.viewport_size = size;
        self.row_heights.borrow_mut().clear();
        if let Some(preview) = &mut self.preview {
            preview.layout = None;
            preview.header_height = None;
            preview.retarget_geometry = preview.origin == Origin::Pointer;
        }
    }

    fn reading_geometry_ready(
        &mut self,
        source: &SharedString,
        bounds: gpui::Bounds<Pixels>,
        window: &gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) {
        if !self
            .preview
            .as_ref()
            .is_some_and(|preview| preview.retarget_geometry)
        {
            return;
        }

        if self.reading_target_source() != Some(source) {
            return;
        }

        let layout = self.reading_layout(source);
        if let Some(preview) = &mut self.preview {
            preview.layout = layout;
            preview.header_height = Some(self.header_height.get());
            preview.retarget_geometry = false;
        }
        if source.as_ref() == "cpu-history-plot" {
            if let Some(history) = self
                .snapshot
                .as_ref()
                .and_then(|snapshot| snapshot.cpu_history.clone())
            {
                self.cpu_history_hover(&history, bounds, window.mouse_position(), cx);
            }
            return;
        }

        let Some(snapshot) = self.snapshot.as_ref() else {
            return;
        };

        let fraction =
            f32::from(window.mouse_position().x - bounds.left()) / f32::from(bounds.size.width);
        let Some(second) = crate::live_projection::sample_second(&snapshot.selection, fraction)
        else {
            return;
        };

        let target = super::Target::regular(snapshot, second);
        self.set_reading_target(source.clone(), target, Origin::Pointer, cx);
    }

    pub(crate) fn reading_layout_scrolled(&mut self) {
        let Some(source) = self.reading_target_source().cloned() else {
            return;
        };

        let layout = self.reading_layout(&source);
        if let Some(preview) = &mut self.preview {
            preview.layout = layout;
        }
    }

    pub(crate) fn reading_layout(&self, source: &SharedString) -> Option<Layout> {
        let index = self
            .rows
            .iter()
            .take(4096)
            .position(|row| row.owns_plot(source))?;
        let bounds = self.list.bounds_for_item(index)?;
        let mut heights = self.row_heights.borrow().clone();
        for (index, row) in self.rows.iter().enumerate().take(4096) {
            if let Some(bounds) = self.list.bounds_for_item(index) {
                heights.insert(row.layout_key(), bounds.size.height);
            }
        }
        Some(Layout {
            source: source.clone(),
            source_top: bounds.top(),
            heights: Rc::new(heights),
        })
    }

    pub(crate) fn reading_row_height(&self, row: &Row) -> Option<Pixels> {
        let layout = self.preview.as_ref()?.layout.as_ref()?;
        row.measured_height(&layout.heights)
    }

    pub(crate) fn rebuild_anchored(&mut self, layout: Option<Layout>) {
        let previous = self.rows.len();
        self.rows = self.reading_rows();
        if let Some(layout) = layout {
            if let Some(offset) = self.reading_layout_offset(&layout) {
                self.list.splice(0..previous, self.rows.len());
                self.list.scroll_to(offset);
                return;
            }
        }
        if previous != self.rows.len() {
            self.list.reset(self.rows.len());
        }
    }

    fn reading_layout_offset(&mut self, layout: &Layout) -> Option<ListOffset> {
        let mut index = self
            .rows
            .iter()
            .take(4096)
            .position(|row| row.owns_plot(&layout.source))?;
        let mut offset = self.list.viewport_bounds().top() - layout.source_top;
        for _ in 0..4096 {
            if offset >= px(0.0) {
                if self.preview.is_some() {
                    self.rows
                        .push(Row::Spacer(self.list.viewport_bounds().size.height));
                }
                return Some(ListOffset {
                    item_ix: index,
                    offset_in_item: offset,
                });
            }
            if index == 0 {
                if self.preview.is_some() {
                    let chart = self
                        .rows
                        .iter()
                        .take(4096)
                        .position(|row| row.owns_plot(&layout.source))?;
                    self.rows.insert(chart, Row::Spacer(-offset));
                    self.rows
                        .push(Row::Spacer(self.list.viewport_bounds().size.height));
                }
                return Some(ListOffset {
                    item_ix: 0,
                    offset_in_item: px(0.0),
                });
            }
            index -= 1;
            offset += self.rows[index].measured_height(&layout.heights)?;
        }

        None
    }
}
