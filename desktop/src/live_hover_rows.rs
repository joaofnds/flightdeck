use std::rc::Rc;

use serde_json::Value;

use crate::data::Panel;

use crate::live_dashboard::{LiveDashboard, Row};
use crate::live_sections::Section;

impl LiveDashboard {
    pub(crate) fn reading_chart_columns(&self) -> usize {
        if self.viewport_size.width < gpui::px(640.0) {
            return 1;
        }
        if matches!(self.request.route.as_str(), "overview" | "apps") {
            if self.viewport_size.width >= gpui::px(1100.0) {
                return 3;
            }
        }

        2
    }

    pub(crate) fn reading_rows(&self) -> Vec<Row> {
        if self.request.route == "self" {
            return vec![Row::Diagnostics];
        }

        if self.request.route == "computer" {
            return vec![Row::Inventory];
        }

        if self.request.route == "apps" {
            return self.reading_rows_apps();
        }

        let mut rows = Vec::with_capacity(4096);
        if matches!(
            self.request.route.as_str(),
            "overview" | "cpu" | "gpu" | "memory" | "energy" | "network" | "storage" | "battery"
        ) {
            rows.push(Row::Summary);
        }
        if self.request.route == "cpu" {
            self.reading_rows_cpu(&mut rows);
        }
        self.reading_rows_charts(&mut rows);
        match self.request.route.as_str() {
            "cpu" => {
                let metric = crate::live_app_attribution_history::Metric::Cpu;
                self.reading_rows_attribution(metric, &mut rows);
                self.reading_rows_system_files(&mut rows);
            }
            "memory" => {
                rows.push(Row::Heading(crate::live_resident_history::TITLE.into()));
                rows.push(Row::MemoryAttribution(
                    self.panels
                        .iter()
                        .find(|panel| panel.id == crate::live_resident_history::PANEL_ID)
                        .cloned(),
                ));
            }
            _ => {}
        }
        if self.request.route == "storage" {
            let metric = crate::live_app_attribution_history::Metric::Disk;
            self.reading_rows_attribution(metric, &mut rows);
            for index in 0..self.resource_device_count() {
                rows.push(Row::StorageDevice(
                    index,
                    self.resource_device_layout_key(index),
                ));
            }
        }
        if self.request.route == "overview" {
            self.reading_rows_overview(&mut rows);
        }
        self.reading_rows_tables(&mut rows);
        assert!(rows.len() <= 4096);

        rows
    }

    /// The heading and chart of one resource's attribution to apps.
    fn reading_rows_attribution(
        &self,
        metric: crate::live_app_attribution_history::Metric,
        rows: &mut Vec<Row>,
    ) {
        let panel = self
            .panels
            .iter()
            .find(|panel| panel.id == metric.panel_id())
            .cloned();
        rows.push(Row::Heading(metric.title().into()));
        rows.push(Row::AppAttribution(metric, panel));
    }

    fn reading_rows_charts(&self, rows: &mut Vec<Row>) {
        if self.request.route == "energy" {
            self.reading_rows_charts_energy(rows);
            return;
        }

        if self.request.route == "memory" {
            self.reading_rows_charts_memory(rows);
            return;
        }

        if self.request.route == "battery" {
            self.reading_rows_charts_battery(rows);
            return;
        }

        if self.request.route == "gpu" {
            self.reading_rows_charts_gpu(rows);
            return;
        }

        if self.request.route == "cpu" {
            rows.push(Row::Heading("Frequency and scheduling".into()));
            let panels: Vec<_> = [
                crate::live_projection::FREQUENCY_ID,
                crate::live_projection::LOAD_AVERAGE_ID,
            ]
            .iter()
            .filter_map(|id| self.panels.iter().take(16).find(|panel| panel.id == *id))
            .cloned()
            .collect();
            let columns = self.reading_chart_columns().min(panels.len().max(1));
            for panels in panels.chunks(columns) {
                rows.push(Row::Charts {
                    panels: panels.to_vec(),
                    columns,
                });
            }
            return;
        }

        let machine_panel_count = self
            .panels
            .iter()
            .take(16)
            .take_while(|panel| {
                if panel.id == crate::live_resident_history::PANEL_ID {
                    return false;
                }
                crate::live_app_attribution_history::Metric::from_panel(&panel.id).is_none()
            })
            .count();
        let primary_panel_count = if self.request.route == "overview" {
            machine_panel_count.min(crate::live_projection::OVERVIEW_PRIMARY_PANELS)
        } else {
            machine_panel_count
        };
        let first = usize::from(self.request.route == "cpu");
        // A short last row keeps the grid's column width, as the prototype's grid does.
        let columns = self.reading_chart_columns();
        for panels in
            self.panels[first.min(primary_panel_count)..primary_panel_count].chunks(columns)
        {
            rows.push(Row::Charts {
                panels: panels.to_vec(),
                columns,
            });
        }
    }

    /// The GPU's activity and power in the grid, then its memory across the page under its
    /// own heading, as the prototype lays them out.
    fn reading_rows_charts_gpu(&self, rows: &mut Vec<Row>) {
        let columns = self.reading_chart_columns();
        let primary = self.named_panels(&["live-gpu-activity", "live-gpu-power"]);
        for panels in primary.chunks(columns) {
            rows.push(Row::Charts {
                panels: panels.to_vec(),
                columns,
            });
        }

        let memory = self.named_panels(&[crate::live_projection::GPU_MEMORY_ID]);
        if !memory.is_empty() {
            rows.push(Row::Heading("Unified-memory allocations".into()));
            rows.push(Row::Charts {
                panels: memory,
                columns: 1,
            });
        }

        rows.push(Row::Heading("GPU time by app".into()));
        rows.push(Row::GpuApps(
            self.named_panels(&[crate::live_projection::GPU_APPS_ID])
                .into_iter()
                .next(),
        ));
    }

    fn reading_rows_charts_memory(&self, rows: &mut Vec<Row>) {
        let primary = self.named_panels(&["live-memory", "live-memory-pressure"]);

        for panels in primary.chunks(self.reading_chart_columns()) {
            rows.push(Row::Charts {
                panels: panels.to_vec(),
                columns: panels.len(),
            });
        }

        rows.push(Row::Heading("Swap and paging".into()));
        let swap = self.named_panels(&["live-swap", "live-swap-flow", "live-paging"]);
        let columns = if self.viewport_size.width >= gpui::px(1100.0) {
            3
        } else {
            self.reading_chart_columns()
        };

        for panels in swap.chunks(columns) {
            rows.push(Row::Charts {
                panels: panels.to_vec(),
                columns,
            });
        }
    }

    /// Charge beside power flow, then the charging timeline beside the battery details.
    fn reading_rows_charts_battery(&self, rows: &mut Vec<Row>) {
        let flows = self.named_panels(&["live-battery-charge", "live-battery-flow"]);
        for panels in flows.chunks(self.reading_chart_columns()) {
            rows.push(Row::Charts {
                panels: panels.to_vec(),
                columns: panels.len(),
            });
        }

        let charging = self.named_panels(&["live-battery-charging"]);
        rows.push(Row::BatteryState(charging.into_iter().next()));
    }

    fn named_panels(&self, ids: &[&str]) -> Vec<Rc<Panel>> {
        assert!(ids.len() <= 3);
        ids.iter()
            .filter_map(|id| {
                self.panels
                    .iter()
                    .take(16)
                    .find(|panel| panel.id == *id)
                    .cloned()
            })
            .collect()
    }

    fn reading_rows_charts_energy(&self, rows: &mut Vec<Row>) {
        let power: Vec<_> = self
            .panels
            .iter()
            .take(16)
            .filter(|panel| matches!(panel.id.as_str(), "live-energy" | "live-gpu-power"))
            .cloned()
            .collect();

        for panels in power.chunks(self.reading_chart_columns()) {
            rows.push(Row::Charts {
                panels: panels.to_vec(),
                columns: panels.len(),
            });
        }

        let metric = crate::live_app_attribution_history::Metric::Power;
        let attribution = self
            .panels
            .iter()
            .take(16)
            .find(|panel| panel.id == metric.panel_id())
            .cloned();

        rows.push(Row::Heading(metric.title().into()));
        rows.push(Row::AppAttribution(metric, attribution));

        rows.push(Row::Disclosure(Section::EnergyContext));
        if self.sections.expanded(Section::EnergyContext) {
            let context: Vec<_> = self
                .panels
                .iter()
                .take(16)
                .filter(|panel| {
                    matches!(
                        panel.id.as_str(),
                        "live-power-breakdown" | "live-soc-temperature"
                    )
                })
                .cloned()
                .collect();
            for panels in context.chunks(self.reading_chart_columns()) {
                rows.push(Row::Charts {
                    panels: panels.to_vec(),
                    columns: panels.len(),
                });
            }
        }
        rows.push(Row::Heading("Cooling and temperatures".into()));
        let cooling: Vec<_> = self
            .panels
            .iter()
            .take(16)
            .filter(|panel| panel.id == "live-fans")
            .cloned()
            .collect();
        for panels in cooling.chunks(self.reading_chart_columns()) {
            rows.push(Row::Charts {
                panels: panels.to_vec(),
                columns: panels.len(),
            });
        }
    }

    /// The CPU page's disclosure of uptime and the file entries open across the system.
    fn reading_rows_system_files(&self, rows: &mut Vec<Row>) {
        rows.push(Row::Disclosure(Section::SystemFiles));
        if self.sections.expanded(Section::SystemFiles) {
            rows.push(Row::SystemContext);
            self.reading_rows_panel(crate::live_projection::OPEN_FILES_ID, rows);
        }
    }

    /// One chart across the row, where the page has a panel of that id.
    fn reading_rows_panel(&self, id: &str, rows: &mut Vec<Row>) {
        if let Some(panel) = self.panels.iter().take(16).find(|panel| panel.id == id) {
            rows.push(Row::Charts {
                panels: vec![Rc::clone(panel)],
                columns: 1,
            });
        }
    }

    fn reading_rows_cpu(&self, rows: &mut Vec<Row>) {
        let panel = self
            .panels
            .iter()
            .find(|panel| panel.id == "live-cpu")
            .cloned();
        if self.viewport_size.width >= gpui::px(1100.0) {
            rows.push(Row::CPUTop(panel));
        } else {
            if let Some(panel) = panel {
                rows.push(Row::Charts {
                    panels: vec![panel],
                    columns: 1,
                });
            }
            rows.push(Row::CPUActivity);
            rows.push(Row::CPUHistory);
        }
    }

    fn reading_rows_overview(&self, rows: &mut Vec<Row>) {
        rows.push(Row::OverviewStatus);

        rows.push(Row::Heading("Top apps at the selected time".into()));
        let slots: Vec<usize> = (0..crate::live_rankings::view::OVERVIEW_SLOT_COUNT).collect();
        for row in slots.chunks(self.reading_chart_columns()) {
            rows.push(Row::OverviewRankings(row.to_vec()));
        }
        rows.push(Row::Disclosure(Section::OverviewHistories));
        if self.sections.expanded(Section::OverviewHistories) {
            for slots in [0, 1, 2, 3].chunks(self.reading_chart_columns().min(2)) {
                rows.push(Row::OverviewAppHistories(slots.to_vec()));
            }
        }
        rows.push(Row::Disclosure(Section::OverviewContext));
        if self.sections.expanded(Section::OverviewContext) {
            self.reading_rows_overview_context(rows);
        }
        if self.snapshot.is_some() {
            rows.push(Row::Heading("MacBook".into()));
            rows.push(Row::Hardware);
        }
    }

    fn reading_rows_overview_context(&self, rows: &mut Vec<Row>) {
        let primary_panel_count = self
            .panels
            .len()
            .min(crate::live_projection::OVERVIEW_PRIMARY_PANELS);
        let correlation = self.panels[primary_panel_count..]
            .iter()
            .take(16)
            .find(|panel| panel.id == crate::live_projection::CORRELATION_ID)
            .cloned();
        rows.push(Row::OverviewAnalysis(correlation));
    }

    fn reading_rows_tables(&self, rows: &mut Vec<Row>) {
        if self.request.route != "raw" {
            return;
        }

        if let Some(snapshot) = self.reading_snapshot() {
            for (name, page) in &snapshot.pages {
                rows.push(Row::TableHeading(name.clone()));
                for index in 0..array_len(Some(page), "rows") {
                    rows.push(Row::Table(name.clone(), index));
                }
            }
        }
    }

    fn reading_rows_apps(&self) -> Vec<Row> {
        let mut rows = Vec::with_capacity(2048);
        rows.push(Row::Summary);
        rows.push(Row::AppSelector);
        let Some(snapshot) = self.reading_snapshot() else {
            return rows;
        };

        if self.picker == Some(crate::live_dashboard::Picker::App) {
            rows.push(Row::AppColumns("apps"));
            for index in self.app_choice_indices() {
                rows.push(Row::App(index));
            }
        }
        let detail = crate::live_app_views::selected_detail(
            snapshot.detail.as_ref(),
            self.request.app.as_ref(),
        );
        let name = detail.map_or_else(
            || "selected App".into(),
            |detail| crate::live_views::text(&detail["app"]["display_name"]),
        );
        rows.push(Row::Heading(format!("Inside {name}")));
        rows.push(Row::AppColumns("executables"));
        for index in 0..self.reading_executables().len().min(512) {
            rows.push(Row::Executable(index));
        }
        rows.push(Row::ProcessGroupingAbout);
        self.reading_rows_process_history(&mut rows);
        rows.push(Row::Disclosure(Section::AppTotals));
        if self.sections.expanded(Section::AppTotals) {
            self.reading_rows_app_totals(&mut rows);
        }
        rows.push(Row::Heading("Process instances · selected app".into()));
        rows.push(Row::AppColumns("processes"));
        for index in 0..array_len(detail, "processes") {
            if let Some(detail) = detail {
                if let Some(key) = &self.request.executable {
                    if !crate::live_executables::matches(&detail["processes"][index], key) {
                        continue;
                    }
                }
            }
            rows.push(Row::Process(index));
        }
        assert!(rows.len() <= 2048);

        rows
    }

    fn reading_rows_app_totals(&self, rows: &mut Vec<Row>) {
        rows.push(Row::AppTotalsNote);
        let totals: Vec<_> = self
            .panels
            .iter()
            .take(16)
            .filter(|panel| panel.id.starts_with("live-app-"))
            .cloned()
            .collect();
        let columns = self.reading_chart_columns().min(2);
        for panels in totals.chunks(columns) {
            rows.push(Row::Charts {
                panels: panels.to_vec(),
                columns,
            });
        }
    }

    fn reading_rows_process_history(&self, rows: &mut Vec<Row>) {
        use crate::live_process_history::PANEL_PREFIX;
        rows.push(Row::ProcessGroupBar);
        let panels: Vec<_> = self
            .panels
            .iter()
            .take(16)
            .filter(|panel| {
                if panel.id.starts_with(PANEL_PREFIX) {
                    return true;
                }
                panel.id.starts_with("live-process-groups-")
            })
            .cloned()
            .collect();
        for panels in panels.chunks(self.reading_chart_columns()) {
            rows.push(Row::Charts {
                panels: panels.to_vec(),
                columns: self.reading_chart_columns(),
            });
        }
    }
}

fn array_len(value: Option<&Value>, key: &str) -> usize {
    value
        .and_then(|value| value[key].as_array())
        .map_or(0, |rows| rows.len().min(512))
}
