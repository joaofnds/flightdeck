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
        let capacity =
            usize::try_from(WRITE_BUFFER_BYTES).map_err(|_| Error::Bound("write buffer"))?;
        let buffer = BufWriter::with_capacity(capacity, file);
        let schema = frame.schema().to_arrow(CompatLevel::newest());
        let options = WriteOptions {
            statistics: StatisticsOptions {
                min_value: true,
                max_value: true,
                distinct_count: false,
                null_count: true,
                binary_statistics_truncate_length: None,
            },
            version: Version::V1,
            compression: CompressionOptions::Zstd(Some(ZstdLevel::try_new(3)?)),
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
