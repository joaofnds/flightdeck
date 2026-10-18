use serde_json::Value;

use crate::live_dashboard::{LiveDashboard, Row};

impl LiveDashboard {
    pub(crate) fn reading_rows(&self) -> Vec<Row> {
        if self.request.route == "computer" {
            return vec![Row::Inventory];
        }

        if self.request.route == "apps" {
            return self.reading_rows_apps();
        }

        let mut rows = Vec::with_capacity(4096);
        if self.request.route == "memory" {
            rows.push(Row::Summary);
        }
        if self.request.route == "cpu" {
            rows.push(Row::CPUHistory);
            rows.push(Row::CPUActivity);
        }
        for panels in self.panels.chunks(2) {
            rows.push(Row::Charts(panels.to_vec()));
        }
        if self.request.route == "overview" {
            rows.push(Row::OverviewStatus);
            if self.snapshot.is_some() {
                rows.push(Row::Hardware);
            }
        }
        if let Some(snapshot) = self.reading_snapshot() {
            for (name, page) in &snapshot.pages {
                if name == "fans" {
                    continue;
                }
                rows.push(Row::TableHeading(name.clone()));
                for index in 0..array_len(Some(page), "rows") {
                    rows.push(Row::Table(name.clone(), index));
                }
            }
        }
        assert!(rows.len() <= 4096);

        rows
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
        rows.push(Row::AppSummary);
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
        rows.push(Row::Heading("Process instances · selected app".into()));
        rows.push(Row::AppColumns("processes"));
        for index in 0..array_len(detail, "processes") {
            if let Some(detail) = detail {
                if let Some(key) = &self.executable {
                    if !crate::live_executables::matches(&detail["processes"][index], key) {
                        continue;
                    }
                }
            }
            rows.push(Row::Process(index));
        }
        rows.push(Row::Heading("Selected app · last 15 minutes".into()));
        for panels in self.panels.chunks(2) {
            rows.push(Row::Charts(panels.to_vec()));
        }
        assert!(rows.len() <= 2048);

        rows
    }
}

fn array_len(value: Option<&Value>, key: &str) -> usize {
    value
        .and_then(|value| value[key].as_array())
        .map_or(0, |rows| rows.len().min(512))
}
