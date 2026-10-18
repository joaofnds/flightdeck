use duckdb::Connection;
use persistence::Window;
use persistence::schema::Table;
use polars::polars_utils::compression::ZstdLevel;
use polars::prelude::*;

use super::query_support::Directory;

pub fn append(connection: &Connection, table: Table, window: &Window) {
    let directory = Directory::new();
    let path = directory.0.join("native-frame.parquet");
    let mut frame = persistence::frame(table, window).unwrap();
    let file = std::fs::File::create(&path).unwrap();
    let bytes = ParquetWriter::new(file)
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
        .with_data_page_size(Some(1024 * 1024))
        .set_parallel(false)
        .with_key_value_metadata(None)
        .with_context_info(None)
        .finish(&mut frame)
        .unwrap();
    assert!(bytes > 0);
    let escaped = persistence::query::parquet_literal(&path).unwrap();
    connection
        .execute_batch(&format!(
            "INSERT INTO {} SELECT * FROM read_parquet('{escaped}')",
            table.name()
        ))
        .unwrap();
}
