use serde::Deserialize;

pub const LIVE_SAMPLE_LIMIT: usize = 4096;

#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Panel {
    pub selected_index: Option<usize>,
    pub positions: Option<Vec<f32>>,
    pub segments: Option<Vec<u64>>,
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
    /// The wall-clock seconds the positions span, which pins the chart's display averaging to
    /// the clock. A panel with none averages from its first position.
    #[serde(skip)]
    pub window: Option<crate::chart_axes::TimeWindow>,
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
    /// The name of the line whose values this one restates in another unit, so the two axes
    /// mark the same heights.
    pub follows: Option<String>,
    pub unit: Option<String>,
    pub fill: Option<f32>,
}
