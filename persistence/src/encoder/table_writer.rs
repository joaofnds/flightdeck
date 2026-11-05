//! One production Parquet owner accepts bounded frames without retaining previous rows.
use super::{DATA_PAGE_BYTES, ROW_GROUP_ROWS, WRITE_BUFFER_BYTES};
use crate::{Error, Result};
use polars::prelude::*;
use polars_parquet::write::{
    CompressionOptions, Encoding, FileWriter, Version, WriteOptions, ZstdLevel, get_dtype_encoding,
    row_group_iter,
};
use std::io::{BufWriter, Write};

pub(crate) struct TableWriter<W: Write> {
    writer: FileWriter<BufWriter<W>>,
}

impl<W: Write> TableWriter<W> {
    pub(crate) fn new(frame: &DataFrame, file: W) -> Result<Self> {
        Self::with_coding(frame, file, true, 3)
    }

    /// A writer for one batch of events. The largest window writes 4,096 of them while the 30 s
    /// recovery bound runs, each row holding two paths of up to 8 KiB, and an event table finds its
    /// batch through its index, so it pays for no statistics of those paths and compresses at a
    /// lighter level. Together they save 1.4 of 5.8 s on an M5 Pro.
    pub(crate) fn event_batch(frame: &DataFrame, file: W) -> Result<Self> {
        Self::with_coding(frame, file, false, 1)
    }

    fn with_coding(frame: &DataFrame, file: W, bounds: bool, zstd_level: i32) -> Result<Self> {
        let capacity =
            usize::try_from(WRITE_BUFFER_BYTES).map_err(|_| Error::Bound("write buffer"))?;
        let buffer = BufWriter::with_capacity(capacity, file);
        let schema = frame.schema().to_arrow(CompatLevel::newest());
        let options = WriteOptions {
            statistics: StatisticsOptions {
                min_value: bounds,
                max_value: bounds,
                distinct_count: false,
                null_count: true,
                binary_statistics_truncate_length: None,
            },
            version: Version::V1,
            compression: CompressionOptions::Zstd(Some(ZstdLevel::try_new(zstd_level)?)),
            data_page_size: Some(
                usize::try_from(DATA_PAGE_BYTES).map_err(|_| Error::Bound("data page size"))?,
            ),
        };
        Ok(Self {
            writer: FileWriter::try_new(buffer, schema, options)?,
        })
    }

    pub(crate) fn write(&mut self, frame: &mut DataFrame) -> Result<()> {
        if frame.schema().to_arrow(CompatLevel::newest()) != *self.writer.schema() {
            return Err(Error::Bound("table frame schema changed"));
        }

        frame.rechunk_mut();
        // Plain pages retain the production codec and avoid a per-column dictionary.
        let encodings: Vec<Vec<Encoding>> = self
            .writer
            .schema()
            .iter_values()
            .map(|field| {
                get_dtype_encoding(&field.dtype)
                    .iter()
                    .map(|_leaf| Encoding::Plain)
                    .collect()
            })
            .collect();
        let fields = self.writer.parquet_schema().fields().to_vec();
        let options = self.writer.options();
        for offset in (0..frame.height()).step_by(ROW_GROUP_ROWS) {
            let length = (frame.height() - offset).min(ROW_GROUP_ROWS);
            let offset = i64::try_from(offset).map_err(|_| Error::Bound("parquet row offset"))?;
            for batch in frame
                .slice(offset, length)
                .iter_chunks(CompatLevel::newest(), false)
            {
                let rows = collector::u64_from_usize(batch.len());
                let group =
                    row_group_iter(batch, encodings.clone().into(), fields.clone(), options);
                self.writer.write(rows, group)?;
            }
        }
        Ok(())
    }

    pub(crate) fn finish(mut self) -> Result<()> {
        let bytes = self.writer.end(None)?;
        assert!(bytes > 0);
        // Flush explicitly so a final buffered write failure remains an operating error.
        self.writer
            .into_inner()
            .into_inner()
            .map_err(std::io::IntoInnerError::into_error)?;
        Ok(())
    }
}
