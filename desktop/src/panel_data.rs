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
    /// the clock. A panel with none draws each reading on its own.
    #[serde(skip)]
    pub window: Option<crate::chart_axes::TimeWindow>,
    /// The first second of full-detail readings, before which each reading stands for several
    /// seconds, so no average mixes the two.
    #[serde(skip)]
    pub detail_first: Option<u64>,
    /// Whether readings still leave at the window's start and arrive at its end, as they do while
    /// the view follows the newest second. The chart holds back the curve those readings would
    /// still reshape.
    #[serde(skip)]
    pub sliding: bool,
    /// How many pixels wide the chart's card was inside its border when last laid out, which
    /// sets how long its buckets are, or `None` before the card is first laid out. The page writes
    /// it as it draws the chart rather than giving the chart a copy, since hover knows a chart by
    /// the panel it holds and not by its contents.
    #[serde(skip)]
    pub chart_width: std::cell::Cell<Option<f32>>,
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
