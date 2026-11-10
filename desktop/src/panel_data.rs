use std::sync::Arc;

use serde::Deserialize;
pub use viewer::store::MachinePlace;

pub const LIVE_SAMPLE_LIMIT: usize = 4096;

fn tick_seconds() -> u64 {
    crate::charts::TICK_SECONDS
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[cfg_attr(test, derive(serde::Serialize))]
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
    /// The wall-clock seconds the positions span, which puts each reading at its own second. A
    /// panel with none draws each reading on its own.
    #[serde(skip)]
    pub window: Option<crate::chart_axes::TimeWindow>,
    /// The reading at each position, which holding a maximum of a reading on its own holds. A panel
    /// with none holds a maximum by its second alone.
    #[serde(skip)]
    pub places: Option<Arc<[MachinePlace]>>,
    /// The first second of full-detail readings, before which each reading stands for
    /// `viewer::EARLIER_STRIDE_SECONDS`.
    #[serde(skip)]
    pub detail_first: Option<u64>,
    /// The seconds between two of the panel's readings where none is missed: a tick for the
    /// readings of each tick, longer for reports the collector writes every few ticks, each of
    /// which names the second this app read it, the end of the seconds it covers. Readings
    /// before `detail_first` stand further apart.
    #[serde(skip, default = "tick_seconds")]
    pub reading_seconds: u64,
}

/// The highest reading each value of a line stands for, where it names it.
pub type LineExtremes = Vec<Option<LineReach>>;

/// What the readings one point of a line stands for reached.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineReach {
    pub high: f32,
    /// The reading that read `high`.
    pub peak: MachinePlace,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[cfg_attr(test, derive(serde::Serialize))]
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
    /// The lowest and highest reading each of `values` stands for, on the chart's own scale, where
    /// a value stands for more than itself. A stacked line's reach is the whole stack beneath it at
    /// one reading, and `None` leaves each value standing for itself alone.
    #[serde(skip)]
    pub extremes: Option<LineExtremes>,
}
