use std::rc::Rc;

use crate::data::Panel;

use crate::live_dashboard::{LiveDashboard, Row};
use crate::live_sections::Section;

/// What the Apps page calls the selected App before a reading names it.
const UNNAMED_APP: &str = "selected App";

impl LiveDashboard {
    pub(crate) fn reading_chart_columns(&self) -> usize {
        if self.viewport_size.width < gpui::px(640.0) {
            return 1;
        }
        if matches!(self.request.route.as_str(), "overview" | "apps" | "gpu") {
            if self.viewport_size.width >= gpui::px(1100.0) {
                return 3;
            }
        }

        2
    }

    pub(crate) fn reading_rows(&self) -> Vec<Row> {
        if !self.placeholders.is_empty() {
            return self
                .placeholders
                .iter()
                .map(|(key, height)| Row::Placeholder(key.clone(), *height))
                .collect();
        }

        self.reading_rows_page()
    }

    /// The rows of the current route's page.
    fn reading_rows_page(&self) -> Vec<Row> {
        if self.request.route == "self" {
            return crate::live_diagnostics::SelfPart::ALL
                .into_iter()
                .map(Row::SelfPart)
                .collect();
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
            "cpu" => self.reading_rows_cpu_sections(&mut rows),
            "memory" => self.reading_rows_memory_sections(&mut rows),
            "storage" => self.reading_rows_storage(&mut rows),
            "network" => {
                rows.push(Row::Heading("Network by app".into()));
                let panel = self.named_panels(&[crate::live_projection::NETWORK_APPS_ID]);
                rows.push(Row::NetworkApps(panel.into_iter().next()));
            }
            _ => {}
        }
        if self.request.route == "overview" {
            self.reading_rows_overview(&mut rows);
        }
        assert!(rows.len() <= 4096);

        rows
    }

    /// CPU by app, the system files, then the CPU temperature, as the prototype orders them.
    fn reading_rows_cpu_sections(&self, rows: &mut Vec<Row>) {
        let metric = crate::live_app_attribution_history::Metric::Cpu;
        self.reading_rows_attribution(metric, rows);
        self.reading_rows_system_files(rows);
        rows.push(Row::Heading("CPU temperature".into()));
        let panel = self.named_panels(&[crate::live_projection::CPU_TEMPERATURE_ID]);
        rows.push(Row::Temperature(
            "cpu",
            "Hottest CPU-area sensor",
            panel.into_iter().next(),
        ));
    }

    /// Memory by app, the memory quantities, then the memory temperature, as the prototype
    /// orders them.
    fn reading_rows_memory_sections(&self, rows: &mut Vec<Row>) {
        rows.push(Row::Heading(crate::live_resident_history::TITLE.into()));
        rows.push(Row::MemoryAttribution(
            self.panels
                .iter()
                .find(|panel| panel.id == crate::live_resident_history::PANEL_ID)
                .cloned(),
        ));
        self.reading_rows_memory_quantities(rows);
        rows.push(Row::Heading("Memory temperature".into()));
        let panel = self.named_panels(&[crate::live_projection::MEMORY_TEMPERATURE_ID]);
        rows.push(Row::Temperature(
            "memory",
            "Hottest Memory-area sensor",
            panel.into_iter().next(),
        ));
    }

    /// Disk I/O by app, then the storage temperature, as the prototype orders them.
    fn reading_rows_storage(&self, rows: &mut Vec<Row>) {
        let metric = crate::live_app_attribution_history::Metric::Disk;
        self.reading_rows_attribution(metric, rows);
        rows.push(Row::Heading("Storage temperature".into()));
        let panel = self.named_panels(&[crate::live_projection::STORAGE_TEMPERATURE_ID]);
        rows.push(Row::Temperature(
            "storage",
            "Hottest Storage-area sensor",
            panel.into_iter().next(),
        ));
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
            self.reading_rows_charts_cpu(rows);
            return;
        }

        let machine_panel_count = self
            .panels
            .iter()
            .take(16)
            .take_while(|panel| {
                if panel.id == crate::live_resident_history::PANEL_ID
                    || panel.id == crate::live_projection::STORAGE_TEMPERATURE_ID
                    || panel.id == crate::live_projection::NETWORK_APPS_ID
                {
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

    /// The CPU's frequency and load average in the grid under their own heading.
    fn reading_rows_charts_cpu(&self, rows: &mut Vec<Row>) {
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
    }

    /// The GPU's activity and power in the grid, then its memory across the page under its
    /// own heading, as the prototype lays them out.
    fn reading_rows_charts_gpu(&self, rows: &mut Vec<Row>) {
        let columns = self.reading_chart_columns();
        let primary = self.named_panels(&[
            "live-gpu-activity",
            crate::live_projection::GPU_FREQUENCY_ID,
            "live-gpu-power",
        ]);
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

        self.reading_rows_attribution(crate::live_app_attribution_history::Metric::Gpu, rows);
        rows.push(Row::Heading("GPU temperature".into()));
        let temperature = self.named_panels(&[crate::live_projection::GPU_TEMPERATURE_ID]);
        rows.push(Row::Temperature(
            "gpu",
            "Hottest GPU-area sensor",
            temperature.into_iter().next(),
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
                rows.push(Row::ContextCharts {
                    panels: panels.to_vec(),
                    columns: panels.len(),
                });
            }
        }
        rows.push(Row::Heading("Cooling and temperatures".into()));
        let find = |id: &str| {
            self.panels
                .iter()
                .take(16)
                .find(|panel| panel.id == id)
                .cloned()
        };
        rows.push(Row::EnergyCooling {
            fans: find(crate::live_projection::FANS_ID),
            thermal: find(crate::live_projection::THERMAL_STATE_ID),
            maxima: find(crate::live_projection::AREA_MAXIMA_ID),
        });
    }

    /// The Memory page's disclosure of every captured memory quantity.
    fn reading_rows_memory_quantities(&self, rows: &mut Vec<Row>) {
        rows.push(Row::Disclosure(Section::MemoryQuantities));
        if self.sections.expanded(Section::MemoryQuantities) {
            rows.push(Row::MemoryQuantities);
        }
    }

    /// The CPU page's disclosure of uptime and the file entries open across the system.
    fn reading_rows_system_files(&self, rows: &mut Vec<Row>) {
        rows.push(Row::Disclosure(Section::SystemFiles));
        if self.sections.expanded(Section::SystemFiles) {
            let panel = self
                .panels
                .iter()
                .take(16)
                .find(|panel| panel.id == crate::live_projection::OPEN_FILES_ID)
                .cloned();
            rows.push(Row::SystemContext(panel));
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
            let slots: Vec<usize> =
                (0..crate::live_overview_app_histories::OVERVIEW_APP_HISTORY_SLOTS).collect();
            for slots in slots.chunks(self.reading_chart_columns().min(2)) {
                rows.push(Row::OverviewAppHistories(slots.to_vec()));
            }
        }
        rows.push(Row::Disclosure(Section::OverviewContext));
        if self.sections.expanded(Section::OverviewContext) {
            self.reading_rows_overview_context(rows);
        }
        rows.push(Row::Heading("MacBook".into()));
        rows.push(Row::Hardware);
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

    fn reading_rows_apps(&self) -> Vec<Row> {
        let mut rows = Vec::with_capacity(2048);
        rows.push(Row::Summary);
        rows.push(Row::AppSelector);
        let Some(snapshot) = self.reading_snapshot() else {
            // Only the reading names the App's executables, so the page holds the rows above
            // them and the rest arrive below, moving nothing it drew.
            if self.is_opening() {
                rows.push(Row::Heading(format!("Inside {UNNAMED_APP}")));
                rows.push(Row::AppColumns("executables"));
            }
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
            || UNNAMED_APP.into(),
            |detail| crate::live_app_views::app_name(&detail["app"]),
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
        assert!(rows.len() <= 2048);

        rows
    }

    fn reading_rows_app_totals(&self, rows: &mut Vec<Row>) {
        let totals: Vec<_> = self
            .panels
            .iter()
            .take(32)
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
            .take(32)
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
