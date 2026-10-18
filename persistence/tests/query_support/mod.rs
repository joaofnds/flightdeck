//! Independent `DuckDB` fixtures pass through the native Polars validation boundary.
use std::os::unix::fs::DirBuilderExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use duckdb::Connection;
use persistence::engine::{Engine, scan};
use persistence::query::TableSource;
use persistence::schema::Table;
use polars::polars_utils::compression::ZstdLevel;
use polars::prelude::{DataFrame, ParquetCompression, ParquetWriter, StatisticsOptions};

pub fn table_source(
    connection: &Connection,
    table: Table,
    source: TableSource<'_>,
) -> persistence::Result<String> {
    let engine = Engine::new();
    let directory = Directory::new();
    let normalized = match source {
        TableSource::Parquet(path) => {
            persistence::query::table_source(&engine, table, TableSource::Parquet(path))?
        }
        TableSource::Staged => {
            let path = directory.0.join("fixture.parquet");
            let escaped = persistence::query::parquet_literal(&path)?;
            connection
                .execute_batch(&format!(
                    "COPY {} TO '{escaped}' (FORMAT PARQUET)",
                    table.name()
                ))
                .unwrap();
            engine.register_lazy(table.name(), scan(&path)?)?;
            persistence::query::table_source(&engine, table, TableSource::Staged)?
        }
    };
    let mut frame = engine.query(&format!("SELECT * FROM {normalized}"))?;
    table_source_materialize(connection, &mut frame, &directory)
}

fn table_source_materialize(
    connection: &Connection,
    frame: &mut DataFrame,
    directory: &Directory,
) -> persistence::Result<String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = directory.0.join("normalized.parquet");
    ParquetWriter::new(std::fs::File::create_new(&path)?)
        .with_compression(ParquetCompression::Zstd(Some(ZstdLevel::try_new(3)?)))
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
        .finish(frame)?;
    let relation = format!("native_normalized_{}", NEXT.fetch_add(1, Ordering::Relaxed));
    let escaped = persistence::query::parquet_literal(&path)?;
    connection
        .execute_batch(&format!(
            "CREATE TEMP TABLE {relation} AS SELECT * FROM read_parquet('{escaped}')"
        ))
        .unwrap();
    Ok(relation)
}

pub(super) struct Directory(pub PathBuf);

impl Directory {
    pub(super) fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let number = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "polars-query-fixture-{}-{number}",
            std::process::id()
        ));
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .unwrap();

        Self(path)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
