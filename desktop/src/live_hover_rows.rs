use serde_json::Value;

use crate::live_dashboard::{LiveDashboard, Row};

impl LiveDashboard {
    pub(crate) fn reading_chart_columns(&self) -> usize {
        if self.viewport_size.width < gpui::px(640.0) {
            return 1;
        }
        if self.request.route == "overview" {
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
            "overview" | "cpu" | "gpu" | "memory" | "energy" | "network"
        ) {
            rows.push(Row::Summary);
        }
        if matches!(self.request.route.as_str(), "storage" | "battery") {
            rows.push(Row::ResourceSummary);
        }
        if self.request.route == "cpu" {
            self.reading_rows_cpu(&mut rows);
        }
        self.reading_rows_charts(&mut rows);
        match self.request.route.as_str() {
            "cpu" => {
                let metric = crate::live_app_attribution_history::Metric::Cpu;
                let panel = self
                    .panels
                    .iter()
                    .find(|panel| panel.id == metric.panel_id())
                    .cloned();
                rows.push(Row::AppAttribution(metric, panel));
            }
            "memory" => rows.push(Row::MemoryAttribution(
                self.panels
                    .iter()
                    .find(|panel| panel.id == crate::live_resident_history::PANEL_ID)
                    .cloned(),
            )),
            _ => {}
        }
        if self.request.route == "storage" {
            let metric = crate::live_app_attribution_history::Metric::Disk;
            let panel = self
                .panels
                .iter()
                .take(16)
                .find(|panel| panel.id == metric.panel_id())
                .cloned();
            rows.push(Row::AppAttribution(metric, panel));
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

    fn reading_rows_charts(&self, rows: &mut Vec<Row>) {
        if self.request.route == "energy" {
            self.reading_rows_charts_energy(rows);
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
        for panels in self.panels[first.min(primary_panel_count)..primary_panel_count]
            .chunks(self.reading_chart_columns())
        {
            rows.push(Row::Charts(panels.to_vec()));
        }
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
            rows.push(Row::Charts(panels.to_vec()));
        }

        let metric = crate::live_app_attribution_history::Metric::Power;
        let attribution = self
            .panels
            .iter()
            .take(16)
            .find(|panel| panel.id == metric.panel_id())
            .cloned();

        rows.push(Row::AppAttribution(metric, attribution));

        let context: Vec<_> = self
            .panels
            .iter()
            .take(16)
            .filter(|panel| matches!(panel.id.as_str(), "live-soc-temperature" | "live-fans"))
            .cloned()
            .collect();

        for panels in context.chunks(self.reading_chart_columns()) {
            rows.push(Row::Charts(panels.to_vec()));
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
                rows.push(Row::Charts(vec![panel]));
            }
            rows.push(Row::CPUActivity);
            rows.push(Row::CPUHistory);
        }
    }

    fn reading_rows_overview(&self, rows: &mut Vec<Row>) {
        rows.push(Row::Heading("Top apps at the selected time".into()));
        for metrics in [0, 1, 2, 3].chunks(self.reading_chart_columns().min(2)) {
            rows.push(Row::Rankings(metrics.to_vec()));
        }
        rows.push(Row::Heading("More resource context".into()));
        rows.push(Row::OverviewStatus);
        let primary_panel_count = self
            .panels
            .len()
            .min(crate::live_projection::OVERVIEW_PRIMARY_PANELS);
        for panels in self.panels[primary_panel_count..].chunks(self.reading_chart_columns()) {
            rows.push(Row::Charts(panels.to_vec()));
        }
        if self.snapshot.is_some() {
            rows.push(Row::Heading("MacBook".into()));
            rows.push(Row::Hardware);
        }
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
        rows.push(Row::AppSelector);
        let Some(snapshot) = self.reading_snapshot() else {
            return rows;
        };

        if self.app_picker_open {
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
        rows.push(Row::Heading(format!("Inside {name} · executable groups")));
        rows.push(Row::AppColumns("executables"));
        for index in 0..self.reading_executables().len().min(512) {
            rows.push(Row::Executable(index));
        }
        self.reading_rows_process_history(&mut rows, snapshot);
        rows.push(Row::Heading("Selected App totals · last 15 minutes".into()));
        rows.push(Row::AppSummary);
        let totals: Vec<_> = self
            .panels
            .iter()
            .take(16)
            .filter(|panel| panel.id.starts_with("live-app-"))
            .cloned()
            .collect();
        for panels in totals.chunks(self.reading_chart_columns()) {
            rows.push(Row::Charts(panels.to_vec()));
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

    fn reading_rows_process_history(
        &self,
        rows: &mut Vec<Row>,
        snapshot: &crate::live_source::Snapshot,
    ) {
        use crate::live_process_history::{PANEL_PREFIX, ProcessHistory};
        let Some(filter) = self.request.executable.as_deref() else {
            rows.push(Row::Heading(
                "Process group history · Select an executable group to inspect captured history"
                    .into(),
            ));
            return;
        };

        let status = match ProcessHistory::filter_reason(Some(filter)) {
            Some(reason) => reason.to_owned(),
            None => match snapshot.process_history.as_deref() {
                Some(ProcessHistory::Ready { payload, .. }) => format!(
                    "{} · last 15 minutes",
                    crate::live_views::text(&payload["executable_name"]["display"])
                ),
                Some(ProcessHistory::Unavailable(reason)) => format!("Unavailable: {reason}"),
                None => "Loading captured history".into(),
            },
        };
        rows.push(Row::Heading(format!("Process group history · {status}")));
        let panels: Vec<_> = self
            .panels
            .iter()
            .take(16)
            .filter(|panel| panel.id.starts_with(PANEL_PREFIX))
            .cloned()
            .collect();
        for panels in panels.chunks(self.reading_chart_columns()) {
            rows.push(Row::Charts(panels.to_vec()));
        }
    }
}

fn array_len(value: Option<&Value>, key: &str) -> usize {
    value
        .and_then(|value| value[key].as_array())
        .map_or(0, |rows| rows.len().min(512))
}
