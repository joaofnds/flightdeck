use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Instant;

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
            Self::SelfPart(part) => part.key().into(),
            Self::OverviewStatus => "overview-status".into(),
            Self::BatteryState(_) => "battery-state".into(),
            Self::Temperature(name, _, _) => format!("temperature-{name}"),
            Self::OverviewRankings(slots) => format!("overview-rankings-{}", slots[0]),
            Self::OverviewAppHistories(slots) => format!("overview-app-histories-{}", slots[0]),
            Self::OverviewAnalysis(_) => "overview-analysis".into(),
            Self::MemoryAttribution(_) => "memory-attribution".into(),
            Self::MemoryQuantities => "memory-quantities".into(),
            Self::AppAttribution(metric, _) => format!("app-attribution-{}", metric.panel_id()),
            Self::GpuApps(_) => "app-attribution-live-gpu-apps".into(),
            Self::NetworkApps(_) => "app-attribution-live-network-apps".into(),
            Self::AppSelector => "app-selector".into(),
            Self::ProcessGroupBar => "process-group-bar".into(),
            Self::ProcessGroupingAbout => "process-grouping-about".into(),
            Self::AppColumns(kind) => format!("app-columns-{kind}"),
            Self::CPUTop(_) => "cpu-top".into(),
            Self::CPUActivity => "cpu-activity".into(),
            Self::CPUHistory => "cpu-history".into(),
            Self::SystemContext(_) => "system-context".into(),
            Self::Charts { panels, .. } => format!("charts-{}", panels[0].id),
            Self::ContextCharts { panels, .. } => format!("context-charts-{}", panels[0].id),
            Self::EnergyCooling { .. } => "energy-cooling".into(),
            Self::Heading(title) => {
                if title.starts_with("Process group history") {
                    return "process-group-history-heading".into();
                }
                if title.ends_with(" temperature") {
                    return format!("temperature-heading-{title}");
                }
                if title.ends_with(" by app") {
                    return format!("attribution-heading-{title}");
                }
                match title.as_str() {
                    "Swap and paging" => return "memory-swap-paging-heading".into(),
                    "Frequency and scheduling" => return "cpu-scheduling-heading".into(),
                    "Top apps at the selected time"
                    | "App resource histories"
                    | "More resource context"
                    | "MacBook" => {
                        return format!("overview-heading-{title}");
                    }
                    _ => {}
                }
                if title.starts_with("Inside ") {
                    "app-executable-heading".into()
                } else {
                    "app-history-heading".into()
                }
            }
            Self::Disclosure(section) => format!("section-{}", section.id()),
            Self::Hardware => "hardware".into(),
            Self::App(index) => format!("app-{index}"),
            Self::Executable(index) => format!("executable-{index}"),
            Self::Inventory => "inventory".into(),
            Self::Spacer(_) => "hover-spacing".into(),
            Self::Placeholder(key, _) => key.clone(),
        }
    }

    /// The layout keys of everything the row shows, so a row the column count regroups is found
    /// by what it holds. The first is the row's own layout key.
    pub(crate) fn layout_members(&self) -> Vec<String> {
        match self {
            Self::Charts { panels, .. } => panels
                .iter()
                .map(|panel| format!("charts-{}", panel.id))
                .collect(),
            Self::ContextCharts { panels, .. } => panels
                .iter()
                .map(|panel| format!("context-charts-{}", panel.id))
                .collect(),
            Self::OverviewRankings(slots) => slots
                .iter()
                .map(|slot| format!("overview-rankings-{slot}"))
                .collect(),
            Self::OverviewAppHistories(slots) => slots
                .iter()
                .map(|slot| format!("overview-app-histories-{slot}"))
                .collect(),
            _ => vec![self.layout_key()],
        }
    }

    pub(crate) fn owns_plot(&self, source: &str) -> bool {
        match self {
            Self::Charts { panels, .. } | Self::ContextCharts { panels, .. } => {
                panels.iter().take(3).any(|panel| panel.id == source)
            }
            Self::EnergyCooling {
                fans,
                thermal,
                maxima,
            } => [fans, thermal, maxima]
                .into_iter()
                .flatten()
                .any(|panel| panel.id == source),
            Self::OverviewAppHistories(slots) => slots.iter().take(2).any(|slot| {
                crate::live_overview_app_histories::overview_app_history_slot(*slot).0 == source
            }),
            Self::MemoryAttribution(Some(panel))
            | Self::OverviewAnalysis(Some(panel))
            | Self::AppAttribution(_, Some(panel))
            | Self::SystemContext(Some(panel))
            | Self::Temperature(_, _, Some(panel))
            | Self::BatteryState(Some(panel)) => panel.id == source,
            Self::CPUTop(panel) => {
                if source == "cpu-history-plot" {
                    true
                } else {
                    panel.as_ref().is_some_and(|panel| panel.id == source)
                }
            }
            Self::CPUHistory => source == "cpu-history-plot",
            Self::OverviewStatus => {
                source == "live-memory-pressure"
                    || source == crate::live_projection::THERMAL_STATE_ID
            }
            _ => false,
        }
    }

    fn measured_height(&self, heights: &BTreeMap<String, Pixels>) -> Option<Pixels> {
        match self {
            Self::Spacer(height) | Self::Placeholder(_, height) => Some(*height),
            Self::App(_) | Self::Executable(_) => {
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

        let previous_columns = self.reading_chart_columns();
        let previous_wide = self.viewport_size.width >= px(1100.0);
        self.viewport_size = size;
        self.row_heights.borrow_mut().clear();
        if let Some(preview) = &mut self.preview {
            preview.layout = None;
            preview.header_height = None;
            preview.retarget_geometry = preview.origin == Origin::Pointer;
        }
        if previous_columns == self.reading_chart_columns() {
            if matches!(self.request.route.as_str(), "cpu" | "memory") {
                if previous_wide != (self.viewport_size.width >= px(1100.0)) {
                    self.rebuild();
                }
            }
        } else {
            self.rebuild();
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

        let hovered_at = Instant::now();
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
                self.cpu_history_hover(&history, bounds, window.mouse_position(), hovered_at, cx);
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
        self.hover_reading_target(source.clone(), target, hovered_at, cx);
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

    /// The key of each row and the room it took when last drawn. A row never drawn takes none,
    /// which only rows outside the viewport can be.
    pub(crate) fn placeholder_rows(&self) -> Vec<(String, Pixels)> {
        let mut heights = self.row_heights.borrow().clone();
        for (index, row) in self.rows.iter().enumerate().take(4096) {
            if let Some(bounds) = self.list.bounds_for_item(index) {
                heights.insert(row.layout_key(), bounds.size.height);
            }
        }
        self.rows
            .iter()
            .take(4096)
            .map(|row| {
                let height = row.measured_height(&heights).unwrap_or(px(0.0));
                (row.layout_key(), height)
            })
            .collect()
    }

    pub(crate) fn reading_row_height(&self, row: &Row) -> Option<Pixels> {
        let layout = self.preview.as_ref()?.layout.as_ref()?;
        row.measured_height(&layout.heights)
    }

    /// Rebuilds the rows in place, keeping the row the user reads at the top of the viewport, or
    /// with a `layout`, keeping the chart it measured where it was on screen.
    pub(crate) fn rebuild_anchored(&mut self, layout: Option<Layout>) {
        let mut rows = self.reading_rows();
        let anchor = layout.and_then(|layout| self.reading_layout_offset(&layout, &mut rows));
        let top = self.replace_rows(rows);
        self.list.scroll_to(anchor.unwrap_or(top));
    }

    fn reading_layout_offset(&self, layout: &Layout, rows: &mut Vec<Row>) -> Option<ListOffset> {
        let chart = rows
            .iter()
            .take(4096)
            .position(|row| row.owns_plot(&layout.source))?;
        let mut index = chart;
        let mut offset = self.list.viewport_bounds().top() - layout.source_top;
        for _ in 0..4096 {
            if offset >= px(0.0) {
                if self.preview.is_some() {
                    let tail = self.reading_layout_tail(layout, rows, index, offset);
                    if tail > px(0.0) {
                        rows.push(Row::Spacer(tail));
                    }
                }
                return Some(ListOffset {
                    item_ix: index,
                    offset_in_item: offset,
                });
            }
            if index == 0 {
                if self.preview.is_some() {
                    rows.insert(chart, Row::Spacer(-offset));
                    let tail = self.reading_layout_tail(layout, rows, 0, px(0.0));
                    if tail > px(0.0) {
                        rows.push(Row::Spacer(tail));
                    }
                }
                return Some(ListOffset {
                    item_ix: 0,
                    offset_in_item: px(0.0),
                });
            }
            index -= 1;
            offset += rows[index].measured_height(&layout.heights)?;
        }

        None
    }

    /// The space the page needs after its last row so the list can hold the chart in place,
    /// which is the part of the viewport the rows from the anchor down do not fill. A row not
    /// measured yet counts as empty, so the chart never moves, at the cost of some blank space.
    fn reading_layout_tail(
        &self,
        layout: &Layout,
        rows: &[Row],
        index: usize,
        offset: Pixels,
    ) -> Pixels {
        let below: Pixels = rows[index..]
            .iter()
            .take(4096)
            .map(|row| row.measured_height(&layout.heights).unwrap_or(px(0.0)))
            .sum();
        self.list.viewport_bounds().size.height - (below - offset)
    }
}
