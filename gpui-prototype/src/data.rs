use std::collections::BTreeMap;
use std::rc::Rc;

use serde::Deserialize;

pub const SAMPLE_COUNT: usize = 121;
pub const PANEL_LIMIT: usize = 128;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capture {
    pub start: u64,
    pub step: u32,
    pub series: BTreeMap<String, Vec<Series>>,
    pub process_apps: Vec<String>,
}

#[derive(Deserialize)]
pub struct Series {
    pub app: Option<String>,
    pub labels: BTreeMap<String, String>,
    pub values: Vec<Option<f32>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Catalog {
    pub navigation: Vec<[String; 3]>,
    pub pages: BTreeMap<String, Vec<Rc<Panel>>>,
    pub hour_pages: BTreeMap<String, Vec<Rc<Panel>>>,
    pub app_pages: BTreeMap<String, Vec<Rc<Panel>>>,
    pub self_panels: Vec<Rc<Panel>>,
    pub computer: Vec<Specification>,
    pub help: BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize)]
pub struct Specification {
    pub name: String,
    pub value: String,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Panel {
    pub id: String,
    pub title: String,
    pub unit: String,
    pub kind: String,
    pub lines: Vec<Line>,
    pub minimum: Option<f32>,
    pub maximum: Option<f32>,
    pub stack: Option<bool>,
    pub fill: Option<f32>,
    pub caption: Option<String>,
    pub help: Option<String>,
    pub source: Option<String>,
    pub quantities: Option<Vec<Vec<String>>>,
    pub states: Option<Vec<String>>,
    pub table_headings: Option<Vec<String>>,
    pub table_rows: Option<Vec<Vec<String>>>,
}

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Line {
    pub name: String,
    pub color: String,
    pub values: Vec<Option<f32>>,
    pub stack: Option<bool>,
    pub negative: Option<bool>,
    pub dashed: Option<bool>,
    pub axis: Option<String>,
    pub unit: Option<String>,
    pub fill: Option<f32>,
}

impl Capture {
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let capture: Self =
            serde_json::from_slice(include_bytes!("../resources/reference_snapshot.json"))?;
        if capture.series.len() > 128 {
            return Err("too many captured metric families".into());
        }
        for rows in capture.series.values() {
            if rows.len() > 2048 {
                return Err("too many captured series".into());
            }
            for row in rows {
                validate_values(&row.values)?;
                if row.values.len() != SAMPLE_COUNT {
                    return Err("machine capture needs 121 samples per series".into());
                }
            }
        }
        Ok(capture)
    }

    pub fn value(&self, key: &str, selected: usize) -> Option<f32> {
        assert!(selected < SAMPLE_COUNT);
        self.series
            .get(key)?
            .first()?
            .values
            .get(selected)
            .copied()
            .flatten()
    }

    pub fn quantity(&self, key: &str, selected: usize) -> String {
        let (metric, scale, unit) = match key {
            "tempSourceSoc" => ("temperature0", 1.0, "°C"),
            "batteryCharge" => ("battery0", 1.0, "%"),
            "memApp" | "memWired" | "memCompressed" | "memCached" | "memFree" => {
                ("memory", 1.0 / 1_073_741_824.0, "GiB")
            }
            "fanChannel0" | "fanChannel1" => {
                let channel = if key == "fanChannel0" { "0" } else { "1" };
                return reading(self.fan(channel, selected), "rpm");
            }
            "pressure" => return state(self.value("pressure0", selected), false).into(),
            "thermal" => return state(self.value("thermal0", selected), true).into(),
            _ => return "Unavailable in this prototype".into(),
        };
        if metric == "memory" {
            let category = match key {
                "memApp" => "App",
                "memWired" => "Wired",
                "memCompressed" => "Compressed",
                "memCached" => "Cached",
                _ => "Free",
            };
            let value = self
                .series
                .get(metric)
                .and_then(|rows| {
                    rows.iter().find(|row| {
                        row.labels
                            .values()
                            .any(|value| value.eq_ignore_ascii_case(category))
                    })
                })
                .and_then(|row| row.values.get(selected).copied().flatten());
            return reading(value.map(|value| value * scale), unit);
        }
        reading(
            self.value(metric, selected).map(|value| value * scale),
            unit,
        )
    }

    pub fn fan(&self, channel: &str, selected: usize) -> Option<f32> {
        self.series
            .get("fans0")?
            .iter()
            .find(|row| row.labels.get("fan").is_some_and(|value| value == channel))?
            .values
            .get(selected)
            .copied()
            .flatten()
    }

    pub fn time(&self, selected: usize) -> String {
        assert!(selected < SAMPLE_COUNT);
        let selected = match u64::try_from(selected) {
            Ok(selected) => selected,
            Err(error) => panic!("bounded sample index exceeds u64: {error}"),
        };
        let seconds = self.start + selected * u64::from(self.step);
        format!(
            "{:02}:{:02}:{:02} UTC",
            seconds / 3600 % 24,
            seconds / 60 % 60,
            seconds % 60
        )
    }
}

impl Catalog {
    pub fn load() -> Result<Self, Box<dyn std::error::Error>> {
        let catalog: Self =
            serde_json::from_slice(include_bytes!("../resources/reference_catalog.json"))?;
        if catalog.navigation.len() != 11 {
            return Err("unexpected captured navigation".into());
        }
        for [route, _, _] in &catalog.navigation {
            if !catalog.pages.contains_key(route) {
                return Err(format!("missing captured page {route}").into());
            }
            if !catalog.hour_pages.contains_key(route) {
                return Err(format!("missing captured hour page {route}").into());
            }
        }
        for panels in catalog
            .pages
            .values()
            .chain(catalog.hour_pages.values())
            .chain(catalog.app_pages.values())
            .chain(std::iter::once(&catalog.self_panels))
        {
            if panels.len() > PANEL_LIMIT {
                return Err("too many captured panels".into());
            }
            for panel in panels {
                if panel.lines.len() > 128 {
                    return Err("too many captured chart lines".into());
                }
                for line in &panel.lines {
                    validate_values(&line.values)?;
                }
            }
        }
        Ok(catalog)
    }

    pub fn app_panels(&self, capture: &Capture, app: &str) -> Vec<Rc<Panel>> {
        if let Some(panels) = self.app_pages.get(app) {
            return panels.clone();
        }
        let mut result = Vec::with_capacity(6);
        if let Some(templates) = self.pages.get("apps") {
            for template in templates.iter().take(PANEL_LIMIT) {
                if let Some(key) = template.id.strip_prefix("selected") {
                    if let Some(row) = capture
                        .series
                        .get(key)
                        .and_then(|rows| rows.iter().find(|row| row.app.as_deref() == Some(app)))
                    {
                        result.push(Self::app_panel(template, row, key, app));
                    }
                }
            }
        }
        result
    }

    fn app_panel(template: &Panel, row: &Series, key: &str, app: &str) -> Rc<Panel> {
        let mut panel = template.clone();
        let title = template.title.split('·').next().unwrap_or("App").trim();
        panel.title = format!("{title} · {app}");
        let scale = match key {
            "appMemory" => 1.0 / 1_073_741_824.0,
            "appNetwork" | "appDisk" => 1e-6,
            _ => 1.0,
        };
        if let Some(line) = panel.lines.first_mut() {
            app.clone_into(&mut line.name);
            line.values.clone_from(&row.values);
            for value in line.values.iter_mut().flatten() {
                *value *= scale;
            }
        }
        Rc::new(panel)
    }
}

pub fn reading(value: Option<f32>, unit: &str) -> String {
    match value {
        Some(value) => format!("{value:.1} {unit}"),
        None => "Unavailable".into(),
    }
}

pub fn state(value: Option<f32>, thermal: bool) -> &'static str {
    match value {
        Some(0.0) => {
            if thermal {
                "Nominal"
            } else {
                "Normal"
            }
        }
        Some(1.0) => {
            if thermal {
                "Fair"
            } else {
                "Warning"
            }
        }
        Some(2.0) => {
            if thermal {
                "Serious"
            } else {
                "Critical"
            }
        }
        Some(3.0) => "Critical",
        _ => "Unavailable",
    }
}

fn validate_values(values: &[Option<f32>]) -> Result<(), &'static str> {
    if values.len() > SAMPLE_COUNT {
        return Err("too many captured samples");
    }
    if values.iter().flatten().any(|value| !value.is_finite()) {
        return Err("non-finite captured value");
    }
    Ok(())
}
