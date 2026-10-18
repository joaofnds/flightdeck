use std::path::Path;

use persistence::engine::{Engine, Row};
use persistence::query::TableSource;
use persistence::schema::Table;
use persistence::{Manifest, Result};
use polars::prelude::DataFrame;

use super::{ComparisonTable, Reference, bound, check_deadline};
use crate::identity_reference;

/// Publication-first ordering and disjoint windows make concatenation globally ordered.
/// The previous frame is released before the next window is admitted or decoded.
pub(super) struct Cursor<'source> {
    connection: &'source Engine,
    root: &'source Path,
    manifests: &'source [Manifest],
    reference: &'source Reference,
    table: Table,
    label: &'source str,
    columns: String,
    order: &'source str,
    window_index: usize,
    row_index: usize,
    frame: Option<DataFrame>,
}

impl<'source> Cursor<'source> {
    pub(super) fn new(
        connection: &'source Engine,
        root: &'source Path,
        manifests: &'source [Manifest],
        reference: &'source Reference,
        comparison: &ComparisonTable<'source>,
    ) -> Result<Self> {
        assert!(manifests.len() <= 128);
        assert_eq!(comparison.order.split(',').next(), Some("publication"));
        let name = match comparison.label {
            "process_identity" => "processes",
            "event_identity" => "events",
            _ => comparison.label,
        };
        let (table, _, _) = persistence::schema::TABLES
            .iter()
            .find(|entry| entry.0.name() == name)
            .ok_or(bound("reference table"))?;
        let columns = match comparison.label {
            "process_identity" => identity_reference::PROCESS_COLUMNS.to_owned(),
            "event_identity" => identity_reference::EVENT_COLUMNS.to_owned(),
            _ => table.columns().collect::<Vec<_>>().join(","),
        };

        Ok(Self {
            connection,
            root,
            manifests,
            reference,
            table: *table,
            label: comparison.label,
            columns,
            order: comparison.order,
            window_index: 0,
            row_index: 0,
            frame: None,
        })
    }

    pub(super) fn next(&mut self) -> Result<Option<(Row<'_>, usize)>> {
        self.next_frame()?;
        let Some(frame) = &self.frame else {
            return Ok(None);
        };
        assert!(self.row_index < frame.height());
        let row = Row::new(frame, self.row_index)?;
        self.row_index += 1;

        Ok(Some((row, frame.width())))
    }

    fn next_frame(&mut self) -> Result<()> {
        if let Some(frame) = &self.frame {
            if self.row_index < frame.height() {
                return Ok(());
            }
        }
        self.frame = None;
        self.row_index = 0;
        for _window in 0..self.manifests.len() {
            let Some(manifest) = self.manifests.get(self.window_index) else {
                return Ok(());
            };
            self.window_index += 1;
            check_deadline(self.reference.deadline)?;
            let frame = self.next_frame_load(manifest)?;
            check_deadline(self.reference.deadline)?;
            if frame.height() > 0 {
                self.frame = Some(frame);
                return Ok(());
            }
        }

        Ok(())
    }

    fn next_frame_load(&self, manifest: &Manifest) -> Result<DataFrame> {
        let path = self
            .root
            .join(&manifest.data_directory)
            .join(format!("{}.parquet", self.table.name()));
        let source = persistence::query::table_source(
            self.connection,
            self.table,
            TableSource::Parquet(&path),
        )?;
        let measured = if self.label == "processes" {
            "usage_availability=0 AND "
        } else {
            ""
        };
        let frame = self.connection.query(&format!(
            "SELECT {} FROM {source} WHERE {measured}publication>={} \
            AND publication<={} ORDER BY {} LIMIT 100000001",
            self.columns, self.reference.first, self.reference.newest, self.order
        ))?;
        Self::next_frame_validate(manifest, &frame)?;

        Ok(frame)
    }

    fn next_frame_validate(manifest: &Manifest, frame: &DataFrame) -> Result<()> {
        if frame.height() == 0 {
            return Ok(());
        }
        let first: u64 = Row::new(frame, 0)?.get(0)?;
        let newest: u64 = Row::new(frame, frame.height() - 1)?.get(0)?;
        if first < manifest.publication_first {
            return Err(bound("reference table publication range"));
        }
        if newest >= manifest.publication_end {
            return Err(bound("reference table publication range"));
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "cursor/tests.rs"]
mod tests;
