//! Saved recording files written as the encoder writes them.
use std::path::Path;

use polars::polars_utils::compression::ZstdLevel;
use polars::prelude::{DataFrame, ParquetCompression, ParquetWriter, StatisticsOptions};

/// Writes a frame with the encoder's file options.
pub fn write(frame: &mut DataFrame, path: &Path) {
    let bytes = ParquetWriter::new(std::fs::File::create_new(path).unwrap())
        .with_compression(ParquetCompression::Zstd(Some(
            ZstdLevel::try_new(3).unwrap(),
        )))
        .with_statistics(StatisticsOptions {
            min_value: true,
            max_value: true,
            distinct_count: false,
            null_count: true,
            binary_statistics_truncate_length: None,
        })
        .with_row_group_size(Some(16_384))
        .with_data_page_size(None)
        .set_parallel(false)
        .with_key_value_metadata(None)
        .with_context_info(None)
        .finish(frame)
        .unwrap();
    assert!(bytes > 0);
}
